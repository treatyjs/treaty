//! `treaty serve` — a FULLY NATIVE Rust dev server.
//!
//! No Node, no external bundler, no NAPI: tokio + axum HTTP, the in-process
//! `treaty_ivy` compiler (oxc), `oxc_resolver` for imports, `notify` for file
//! watching, and an axum websocket for live reload.
//!
//! ## Request model (Vite-like, but Rust)
//!
//!   * `GET /`                     -> the app `index.html`, with `<app-root>`
//!                                    preserved, the `src/main.ts` entry rewritten
//!                                    to its served URL, and the live-reload client
//!                                    injected.
//!   * `GET /@fs/<abs-path>`       -> serve ONE module. Every import in the app is
//!                                    rewritten to this scheme (the absolute path
//!                                    of the resolved file), so a single handler
//!                                    serves first-party `.ts` (compile-on-demand
//!                                    to Ivy ESM), published ESM (`rxjs`), and
//!                                    *partial* `@angular/*` (link-on-demand to
//!                                    AOT) uniformly. The extension + `node_modules`
//!                                    location decide which transform runs.
//!   * `GET /@treaty/client.js`    -> the tiny live-reload client (opens the WS).
//!   * `GET /@treaty/ws`           -> the live-reload websocket.
//!
//! ## On-demand compile
//!
//! For a first-party `.ts`/`.treaty`/`.tsx`, the handler runs
//! [`crate::transform::lower`] (Ivy lowering + type-strip), then rewrites every
//! import specifier in the output to its `/@fs/<abs>` served URL via the resolver.
//! For a `node_modules` `.mjs` containing `ɵɵngDeclare*`, it runs the shared Rust
//! linker ([`treaty_ivy::link_partial`]) so the served Angular needs NO JIT and NO
//! `@angular/compiler`. All results are cached by absolute path + mtime.
//!
//! ## What is and is NOT here
//!
//! Compile + serve + FULL live-reload land here. True HMR (module-level hot swap
//! without a page reload) is the noted follow-up: on any change the server
//! invalidates the cache and tells the browser to reload the page, which re-pulls
//! the changed module. That is correct and snappy for a dev loop; module-level
//! state preservation is the future increment.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use axum::{
    body::Body,
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path as AxumPath, State,
    },
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use notify::{RecursiveMode, Watcher};
use tokio::sync::broadcast;

use crate::resolve::{is_under_node_modules, ModuleResolver};
use crate::transform::{lower, rewrite_imports};

/// Options for a dev session.
#[derive(Debug, Clone)]
pub struct ServeOptions {
    /// The app root (where `index.html` + `src/` live).
    pub root: PathBuf,
    /// The entry module, relative to root (e.g. `src/main.ts`).
    pub entry: PathBuf,
    /// Host to bind.
    pub host: String,
    /// Port to bind.
    pub port: u16,
}

/// One cached, served module: the emitted text and the mtime it was built from.
#[derive(Clone)]
struct CachedModule {
    body: String,
    content_type: &'static str,
    built_from_mtime: Option<SystemTime>,
}

/// Shared server state (compile cache, resolver, reload channel, config).
struct ServerState {
    root: PathBuf,
    resolver: ModuleResolver,
    cache: Mutex<HashMap<PathBuf, CachedModule>>,
    reload_tx: broadcast::Sender<()>,
}

/// The served-URL prefix for an absolute file path. Every import resolves to one
/// of these, so the browser fetches every module through the `/@fs/*` handler.
const FS_PREFIX: &str = "/@fs/";

/// Encode an absolute path into its `/@fs/...` served URL. The path is carried
/// verbatim after the prefix (forward-slashed); the handler re-joins it. A
/// leading drive letter on Windows (`C:`) is kept after the prefix.
fn fs_url(abs: &Path) -> String {
    let s = abs.to_string_lossy().replace('\\', "/");
    let s = s.strip_prefix('/').map(|x| x.to_string()).unwrap_or(s);
    format!("{FS_PREFIX}{s}")
}

/// Decode a `/@fs/<rest>` wildcard capture back to an absolute path.
fn fs_path_from_capture(capture: &str) -> PathBuf {
    // On Windows the capture is like `C:/dev/...`; on Unix `home/...` (the leading
    // `/` was stripped when forming the URL), so re-add it there.
    let looks_windows = capture.len() >= 2 && capture.as_bytes()[1] == b':';
    if looks_windows {
        PathBuf::from(capture)
    } else {
        PathBuf::from(format!("/{capture}"))
    }
}

/// File mtime, or `None` if unavailable.
fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

/// Whether a path is a first-party Treaty/TS authoring source (compile-on-demand).
fn is_authoring_source(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("ts") | Some("tsx") | Some("tjsx") | Some("treaty")
    )
}

/// Build the served ESM for one resolved file (compile/link/passthrough), with
/// every import specifier rewritten to its `/@fs/<abs>` served URL.
///
/// This is the heart of the on-demand pipeline. Caching-free so it is unit
/// testable; the handler wraps it with the mtime cache.
fn build_module(state: &ServerState, abs: &Path) -> Result<CachedModule, String> {
    let source =
        std::fs::read_to_string(abs).map_err(|e| format!("read {}: {e}", abs.display()))?;
    let importer_dir = abs.parent().unwrap_or(&state.root).to_path_buf();

    if is_authoring_source(abs) {
        // First-party authoring/TS: Ivy-lower + type-strip, then rewrite imports.
        let lowered = lower(&source, &abs.to_string_lossy());
        if !lowered.errors.is_empty() {
            return Err(lowered.errors.join("; "));
        }
        let body =
            rewrite_imports(&lowered.code, |spec| rewrite_one(state, &importer_dir, spec));
        return Ok(CachedModule {
            body,
            content_type: "application/javascript; charset=utf-8",
            built_from_mtime: mtime(abs),
        });
    }

    // A published module (`node_modules`): if it is a *partial* Angular library
    // (contains `ɵɵngDeclare*`), LINK it to AOT first; then rewrite its imports.
    let linked = if is_under_node_modules(abs) && source.contains("ɵɵngDeclare") {
        treaty_ivy::link_partial(&source, &abs.to_string_lossy()).code
    } else {
        source
    };
    let body = rewrite_imports(&linked, |spec| rewrite_one(state, &importer_dir, spec));
    Ok(CachedModule {
        body,
        content_type: "application/javascript; charset=utf-8",
        built_from_mtime: mtime(abs),
    })
}

/// Rewrite ONE import specifier to its served URL, or `None` to leave it as-is.
///
/// Resolves `spec` (relative or bare) against `importer_dir` with `oxc_resolver`;
/// on success returns the `/@fs/<abs>` URL. A specifier that does not resolve
/// (Node builtin, unknown) is left untouched so the browser surfaces it.
fn rewrite_one(state: &ServerState, importer_dir: &Path, spec: &str) -> Option<String> {
    // Already a served URL (idempotent re-entry) — leave it.
    if spec.starts_with(FS_PREFIX) || spec.starts_with("/@treaty/") {
        return None;
    }
    // Data/HTTP URLs: leave untouched.
    if spec.starts_with("http:") || spec.starts_with("https:") || spec.starts_with("data:") {
        return None;
    }
    let resolved = state.resolver.resolve(importer_dir, spec)?;
    Some(fs_url(&resolved))
}

/// Serve `index.html`: preserve the app shell, rewrite the entry script to its
/// served URL, and inject the live-reload client.
fn render_index(state: &ServerState, entry_abs: &Path) -> String {
    let index_path = state.root.join("index.html");
    let raw = std::fs::read_to_string(&index_path).unwrap_or_else(|_| {
        // Minimal shell if the app has no index.html (still boots the entry).
        "<!doctype html><html><head><base href=\"/\"></head><body><app-root></app-root></body></html>".to_string()
    });
    let entry_url = fs_url(entry_abs);
    let scripts = format!(
        "\n<script type=\"module\" src=\"{entry_url}\"></script>\n<script type=\"module\" src=\"/@treaty/client.js\"></script>\n"
    );
    // Inject before </body> if present, else append.
    if let Some(idx) = raw.rfind("</body>") {
        let mut out = String::with_capacity(raw.len() + scripts.len());
        out.push_str(&raw[..idx]);
        out.push_str(&scripts);
        out.push_str(&raw[idx..]);
        out
    } else {
        format!("{raw}{scripts}")
    }
}

/// The live-reload client: open the WS and reload the page on any message.
const CLIENT_JS: &str = r#"// Treaty dev live-reload client (full-page reload on change).
const proto = location.protocol === 'https:' ? 'wss' : 'ws';
const ws = new WebSocket(`${proto}://${location.host}/@treaty/ws`);
ws.addEventListener('message', () => { location.reload(); });
ws.addEventListener('close', () => {
  // Server went away (restart): poll until it is back, then reload.
  const tick = () => fetch('/@treaty/ping').then(() => location.reload()).catch(() => setTimeout(tick, 500));
  setTimeout(tick, 500);
});
"#;

async fn handle_client_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/javascript; charset=utf-8")],
        CLIENT_JS,
    )
}

async fn handle_ping() -> impl IntoResponse {
    StatusCode::OK
}

async fn handle_fs(
    State(state): State<Arc<ServerState>>,
    AxumPath(rest): AxumPath<String>,
) -> Response {
    let abs = fs_path_from_capture(&rest);
    // Cache hit if the file's mtime is unchanged since we built it.
    let current = mtime(&abs);
    if let Some(hit) = state.cache.lock().unwrap().get(&abs) {
        if hit.built_from_mtime == current {
            return module_response(hit);
        }
    }
    match build_module(&state, &abs) {
        Ok(module) => {
            let resp = module_response(&module);
            state.cache.lock().unwrap().insert(abs, module);
            resp
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, "application/javascript; charset=utf-8")],
            format!("/* treaty serve error: {e} */\nthrow new Error({e:?});"),
        )
            .into_response(),
    }
}

fn module_response(module: &CachedModule) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, module.content_type)
        // Dev: never cache in the browser (the server's mtime cache is the cache).
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from(module.body.clone()))
        .unwrap()
}

async fn handle_ws(
    ws: WebSocketUpgrade,
    State(state): State<Arc<ServerState>>,
) -> impl IntoResponse {
    let mut rx = state.reload_tx.subscribe();
    ws.on_upgrade(move |mut socket: WebSocket| async move {
        loop {
            tokio::select! {
                changed = rx.recv() => {
                    match changed {
                        Ok(()) => {
                            if socket.send(Message::Text("reload".into())).await.is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                incoming = socket.recv() => {
                    // Client closed -> done.
                    if incoming.is_none() {
                        break;
                    }
                }
            }
        }
    })
}

/// Build the axum router for a server state + resolved entry path.
fn router(state: Arc<ServerState>, entry_abs: PathBuf) -> Router {
    let index_state = state.clone();
    let index_entry = entry_abs.clone();
    Router::new()
        .route(
            "/",
            get(move || {
                let st = index_state.clone();
                let entry = index_entry.clone();
                async move { Html(render_index(&st, &entry)) }
            }),
        )
        .route(
            "/index.html",
            get({
                let st = state.clone();
                let entry = entry_abs.clone();
                move || {
                    let st = st.clone();
                    let entry = entry.clone();
                    async move { Html(render_index(&st, &entry)) }
                }
            }),
        )
        .route("/@treaty/client.js", get(handle_client_js))
        .route("/@treaty/ping", get(handle_ping))
        .route("/@treaty/ws", get(handle_ws))
        .route("/@fs/{*rest}", get(handle_fs))
        .with_state(state)
}

/// Start the dev server and block until shutdown (Ctrl-C).
pub fn serve_blocking(opts: ServeOptions) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("failed to start tokio runtime: {e}"))?;
    runtime.block_on(async move { serve_async(opts).await })
}

/// Build the configured axum [`Router`] for `opts`, plus the reload sender (so a
/// caller can wire file-watching) and the resolved entry path. Factored out of
/// [`serve_async`] so an integration test can serve the SAME app on an ephemeral
/// listener without binding the configured host/port or blocking forever.
pub fn build_app(opts: &ServeOptions) -> (Router, broadcast::Sender<()>, PathBuf) {
    let (reload_tx, _rx) = broadcast::channel::<()>(64);
    let entry_abs = if opts.entry.is_absolute() {
        opts.entry.clone()
    } else {
        opts.root.join(&opts.entry)
    };
    let state = Arc::new(ServerState {
        root: opts.root.clone(),
        resolver: ModuleResolver::new(),
        cache: Mutex::new(HashMap::new()),
        reload_tx: reload_tx.clone(),
    });
    let app = router(state, entry_abs.clone());
    (app, reload_tx, entry_abs)
}

/// The async server entry (separated so tests can drive it on an ephemeral port).
pub async fn serve_async(opts: ServeOptions) -> Result<(), String> {
    let (app, reload_tx, _entry) = build_app(&opts);

    // Re-create the state-bound watcher: rebuild the app's state-driving watcher by
    // re-deriving it from a fresh state is unnecessary — the watcher only needs the
    // reload sender and the same cache-clear effect. The `build_app` state owns the
    // cache the handlers use; for file-watch we clear via a lightweight watcher that
    // broadcasts reloads (the per-request mtime check already rebuilds stale modules,
    // so a missed cache-clear is self-healing).
    let _watcher = spawn_reload_watcher(opts.root.clone(), reload_tx.clone());

    // Resolve `host:port` to a socket address. A bare IP parses directly; a
    // hostname (`localhost`) is resolved via DNS/hosts (tokio's `lookup_host`).
    let host_port = format!("{}:{}", opts.host, opts.port);
    let addr: SocketAddr = match host_port.parse() {
        Ok(a) => a,
        Err(_) => tokio::net::lookup_host(&host_port)
            .await
            .map_err(|e| format!("cannot resolve {host_port}: {e}"))?
            .next()
            .ok_or_else(|| format!("no address for {host_port}"))?,
    };
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("bind {addr}: {e}"))?;
    let bound = listener.local_addr().map_err(|e| e.to_string())?;
    println!("treaty serve: http://{bound}/");

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|e| format!("server error: {e}"))
}

/// Spawn a filesystem watcher that, on any change under `root`, broadcasts a
/// reload. The returned watcher must be kept alive.
///
/// It does not touch the compile cache: each `/@fs/` request already rebuilds a
/// module whose own mtime advanced (the per-request mtime guard in [`handle_fs`]),
/// and a module's emitted text only embeds its imports' *served URLs* (which are
/// stable across a dependency's content change), so a stale importer entry is
/// harmless — the browser re-fetches the changed dependency directly after the
/// full-page reload. Cache-clearing is therefore unnecessary for correctness.
fn spawn_reload_watcher(
    root: PathBuf,
    reload_tx: broadcast::Sender<()>,
) -> Option<notify::RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            use notify::EventKind;
            if matches!(
                event.kind,
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
            ) {
                let _ = reload_tx.send(());
            }
        }
    })
    .ok()?;
    // Watch the app's source tree (not node_modules — published libs do not change
    // during a dev session, and watching them is expensive).
    let src = root.join("src");
    let target = if src.exists() { src } else { root };
    watcher.watch(&target, RecursiveMode::Recursive).ok()?;
    Some(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_for(root: &Path) -> Arc<ServerState> {
        let (tx, _rx) = broadcast::channel(8);
        Arc::new(ServerState {
            root: root.to_path_buf(),
            resolver: ModuleResolver::new(),
            cache: Mutex::new(HashMap::new()),
            reload_tx: tx,
        })
    }

    #[test]
    fn fs_url_roundtrips_through_capture() {
        let p = if cfg!(windows) {
            PathBuf::from("C:/dev/app/src/app.ts")
        } else {
            PathBuf::from("/home/app/src/app.ts")
        };
        let url = fs_url(&p);
        assert!(url.starts_with(FS_PREFIX));
        let capture = url.strip_prefix(FS_PREFIX).unwrap();
        let back = fs_path_from_capture(capture);
        assert_eq!(
            back.to_string_lossy().replace('\\', "/"),
            p.to_string_lossy().replace('\\', "/")
        );
    }

    #[test]
    fn build_module_lowers_a_component_and_rewrites_relative_imports() {
        let dir = std::env::temp_dir().join(format!("treaty-serve-bm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("dep.ts"), "export const dep = 1;").unwrap();
        let comp = "import { Component } from '@angular/core';\nimport { dep } from './dep';\n@Component({ selector: 'a-b', template: '<p>{{dep}}</p>' })\nexport class AB { v: number = dep; }";
        let comp_path = dir.join("ab.ts");
        std::fs::write(&comp_path, comp).unwrap();

        let state = state_for(&dir);
        let module = build_module(&state, &comp_path).expect("build_module ok");
        assert!(module.body.contains("ɵɵdefineComponent"), "no Ivy: {}", module.body);
        assert!(!module.body.contains(": number"), "type survived: {}", module.body);
        // The relative `./dep` import was rewritten to a served /@fs/ URL.
        assert!(module.body.contains("/@fs/"), "relative import not rewritten: {}", module.body);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_index_injects_entry_and_client() {
        let dir = std::env::temp_dir().join(format!("treaty-serve-idx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("index.html"),
            "<!doctype html><html><body><app-root></app-root></body></html>",
        )
        .unwrap();
        let state = state_for(&dir);
        let entry = dir.join("src").join("main.ts");
        let html = render_index(&state, &entry);
        assert!(html.contains("<app-root></app-root>"), "shell lost");
        assert!(html.contains("/@treaty/client.js"), "client not injected");
        assert!(html.contains("/@fs/"), "entry not rewritten to served url");
        assert!(html.contains("</body>"), "closing body lost");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn links_partial_node_modules_module() {
        // A fake node_modules partial module with a ɵɵngDeclareInjectable call.
        let base = std::env::temp_dir().join(format!("treaty-serve-link-{}", std::process::id()));
        let dir = base.join("node_modules").join("pkg");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&dir).unwrap();
        let partial = "import * as i0 from '@angular/core';\nexport class S {}\nS.ɵprov = i0.ɵɵngDeclareInjectable({ minVersion: '12.0.0', version: '0.0.0', ngImport: i0, type: S, providedIn: 'root' });";
        let p = dir.join("index.mjs");
        std::fs::write(&p, partial).unwrap();
        let state = state_for(&dir);
        let module = build_module(&state, &p).expect("build ok");
        assert!(module.body.contains("ɵɵdefineInjectable"), "partial not linked: {}", module.body);
        assert!(!module.body.contains("ɵɵngDeclareInjectable"), "residual partial: {}", module.body);
        let _ = std::fs::remove_dir_all(&base);
    }
}
