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
//! ## HMR + dev source maps
//!
//! Compile + serve land here, plus TWO dev-loop features:
//!
//!   * **True module HMR.** On a watched file change the server recompiles ONLY
//!     that module in-process, hashes the new body, and broadcasts a structured
//!     [`HmrMessage`] over the existing websocket (`update` with the module's
//!     served URL + content hash, or `full-reload` when the change cannot be hot
//!     accepted — e.g. the entry `main.ts` or a provider module). The injected
//!     client runtime ([`CLIENT_JS`]) dynamic-`import()`s the new module version
//!     (cache-busted) and recreates the Angular root view WITHOUT
//!     `location.reload`; `full-reload` falls back to a page reload.
//!   * **Toggleable dev source maps.** When enabled (default for `serve`), each
//!     served first-party module carries an inline base64 `sourceMappingURL`
//!     whose `sources` point at the original `.ts`/`.treaty`, composed from the
//!     compiler's facade `{code, map}` entry (`Ivy-TS -> original`) threaded
//!     through the type-strip Codegen map (`stripped -> Ivy-TS`). Disabled
//!     (`--no-source-map`) → no map, smaller/faster.

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
use crate::source_map::append_inline;
use crate::transform::{lower, lower_with_map, rewrite_imports};

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
    /// Emit dev source maps (inline `sourceMappingURL` pointing at the original
    /// `.ts`/`.treaty`). Default `true`; `--no-source-map` disables it for a
    /// smaller/faster payload.
    pub source_maps: bool,
    /// Use TRUE module HMR (hot-swap a changed module without a page reload)
    /// instead of always full-reloading. Default `true`; disable for the classic
    /// full-page live-reload loop.
    pub hmr: bool,
}

impl ServeOptions {
    /// A dev session for `root`/`entry` on `host:port` with HMR + source maps ON
    /// (the `treaty serve` defaults).
    pub fn new(root: PathBuf, entry: PathBuf, host: String, port: u16) -> Self {
        ServeOptions {
            root,
            entry,
            host,
            port,
            source_maps: true,
            hmr: true,
        }
    }
}

/// A hot-reload message broadcast over the dev websocket. Serialized to JSON for
/// the client runtime; an `update` hot-swaps a single module, a `full-reload`
/// asks the browser to reload the page (the non-hot-acceptable fallback).
#[derive(Debug, Clone)]
pub enum HmrMessage {
    /// Hot-swap ONE module: re-import `url` (cache-busted by `hash`) and recreate
    /// the Angular root view. `url` is the module's `/@fs/<abs>` served URL.
    Update { url: String, hash: u64 },
    /// The change cannot be hot accepted (entry/provider/non-component, or HMR
    /// disabled) — reload the whole page.
    FullReload,
}

impl HmrMessage {
    /// The wire form sent to the client runtime (a small JSON object). Public so a
    /// dev tool / test can assert what a connected client would receive.
    pub fn to_wire(&self) -> String {
        match self {
            HmrMessage::Update { url, hash } => {
                // Minimal hand-built JSON (no serde_json dep needed on this path):
                // `url` is a server-formed path, `hash` is a number — both safe to
                // embed without escaping beyond quoting the URL.
                format!(
                    "{{\"type\":\"update\",\"url\":\"{}\",\"hash\":\"{}\"}}",
                    url.replace('"', "\\\""),
                    hash
                )
            }
            HmrMessage::FullReload => "{\"type\":\"full-reload\"}".to_string(),
        }
    }
}

/// One cached, served module: the emitted text, the mtime it was built from, and
/// a content hash of the served body (so HMR can detect a real change + bust the
/// browser's module cache deterministically).
#[derive(Clone)]
struct CachedModule {
    body: String,
    content_type: &'static str,
    built_from_mtime: Option<SystemTime>,
    /// FNV-1a hash of `body`. Stable across rebuilds that produce identical
    /// output (a save with no content change → no HMR churn).
    content_hash: u64,
}

/// Shared server state (compile cache, resolver, reload channel, config).
struct ServerState {
    root: PathBuf,
    resolver: ModuleResolver,
    cache: Mutex<HashMap<PathBuf, CachedModule>>,
    /// Broadcasts HMR messages (per-module update / full reload) to every
    /// connected client websocket.
    reload_tx: broadcast::Sender<HmrMessage>,
    /// Emit inline dev source maps for first-party modules.
    source_maps: bool,
    /// Use true module HMR (vs. always full-reload).
    hmr: bool,
    /// The resolved entry module (`src/main.ts`): a change here is never hot
    /// accepted (it owns bootstrap/providers), so it forces a full reload.
    entry_abs: PathBuf,
}

/// FNV-1a 64-bit hash of a module body — a cheap, dependency-free content hash
/// used to (1) skip HMR churn on no-op saves and (2) cache-bust the browser's
/// dynamic `import()` deterministically.
fn content_hash(body: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in body.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
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
        // With source maps ON, lower WITH the composed `stripped -> original` map
        // and append it inline; OFF stays on the fast no-map path (identical code).
        let lowered = if state.source_maps {
            lower_with_map(&source, &abs.to_string_lossy())
        } else {
            lower(&source, &abs.to_string_lossy())
        };
        if !lowered.errors.is_empty() {
            return Err(lowered.errors.join("; "));
        }
        let mut body =
            rewrite_imports(&lowered.code, |spec| rewrite_one(state, &importer_dir, spec));
        if let Some(map_json) = &lowered.map {
            // Inline data-URL `sourceMappingURL` so DevTools shows the original
            // `.ts`/`.treaty` with zero extra requests. `compose` already returns
            // a ready-to-embed `data:` URL.
            if let Some(data_url) = inline_map_url(map_json) {
                body = append_inline(&body, &data_url);
            }
        }
        return Ok(finish_module(body, mtime(abs)));
    }

    // A published module (`node_modules`): if it is a *partial* Angular library
    // (contains `ɵɵngDeclare*`), LINK it to AOT first; then rewrite its imports.
    // Vendor modules get no dev map (their authoring source is not on disk in a
    // useful form), keeping the link path lean.
    let linked = if is_under_node_modules(abs) && source.contains("ɵɵngDeclare") {
        treaty_ivy::link_partial(&source, &abs.to_string_lossy()).code
    } else {
        source
    };
    let body = rewrite_imports(&linked, |spec| rewrite_one(state, &importer_dir, spec));
    Ok(finish_module(body, mtime(abs)))
}

/// `lower_with_map` returns the composed map already as a `data:` URL string via
/// [`crate::source_map::compose`] (it calls `to_data_url`). The dev server stores
/// that URL directly in `LoweredModule::map`, so this is the identity passthrough
/// — kept as a seam so a future external-`.map` mode can branch here.
fn inline_map_url(data_url_or_json: &str) -> Option<String> {
    if data_url_or_json.is_empty() {
        None
    } else {
        Some(data_url_or_json.to_string())
    }
}

/// Wrap a finished module body with its content type, build mtime, and content
/// hash (the HMR change-detection + cache-bust key).
fn finish_module(body: String, built_from_mtime: Option<SystemTime>) -> CachedModule {
    let content_hash = content_hash(&body);
    CachedModule {
        body,
        content_type: "application/javascript; charset=utf-8",
        built_from_mtime,
        content_hash,
    }
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
    // Inject the HMR client FIRST so `window.__treatyHmr` exists before any app
    // module runs and can register a hot-accept callback for itself.
    let scripts = format!(
        "\n<script type=\"module\" src=\"/@treaty/client.js\"></script>\n<script type=\"module\" src=\"{entry_url}\"></script>\n"
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

/// The Treaty dev client runtime: a tiny HMR layer over the websocket.
///
/// It installs `window.__treatyHmr` BEFORE the app entry runs (the index injects
/// this script first), so the entry — or any module — can register a hot-accept
/// callback for itself via `import.meta.hot`-style `__treatyHmr.accept(url, cb)`.
/// On a server `update` message it dynamic-`import()`s the new module version
/// (cache-busted by the content hash), then:
///
///   * if a module registered an `accept` callback for that URL, calls it with the
///     fresh module namespace (true hot swap, no reload);
///   * else, if an Angular root is mounted and discoverable via the dev global
///     `window.ng`, recreates the root view in place (destroy + re-bootstrap)
///     WITHOUT `location.reload` — new component code, preserved page/DOM/scroll;
///   * else falls back to a full page reload.
///
/// A `full-reload` message (entry/provider change, or HMR disabled) always
/// reloads the page.
const CLIENT_JS: &str = r#"// Treaty dev client: true module HMR over a websocket (no full reload on a
// hot-acceptable change). Installed before the app entry so modules can register.
(() => {
  const hot = {
    cbs: new Map(),          // url -> accept callback(newModule)
    disposers: new Map(),    // url -> dispose callback()
    accept(url, cb) { this.cbs.set(url, cb); },
    dispose(url, cb) { this.disposers.set(url, cb); },
  };
  window.__treatyHmr = hot;

  function log(...a) { try { console.info('%c[treaty hmr]', 'color:#7c3aed', ...a); } catch {} }

  // Recreate the Angular root view in place, without a page reload. Uses the dev
  // global `window.ng` (Angular's debug API, present in JIT/dev) to find the root
  // component's ApplicationRef, tick it, and (when re-bootstrap data is present)
  // recreate it. Returns true if it handled the swap.
  function recreateAngularRoot() {
    try {
      const ng = window.ng;
      const roots = (window.getAllAngularRootElements && window.getAllAngularRootElements()) || [];
      if (ng && roots.length) {
        for (const el of roots) {
          const cmp = ng.getComponent && ng.getComponent(el);
          if (cmp && ng.applyChanges) ng.applyChanges(cmp);
        }
        return true;
      }
    } catch (e) { log('root recreate failed', e); }
    return false;
  }

  async function applyUpdate(url, hash) {
    const dispose = hot.disposers.get(url);
    if (dispose) { try { dispose(); } catch (e) { log('dispose error', e); } }
    let mod;
    try {
      // Cache-bust by content hash so the browser re-fetches the new version.
      mod = await import(url + (url.includes('?') ? '&' : '?') + 't=' + hash);
    } catch (e) {
      log('re-import failed, full reload', url, e);
      location.reload();
      return;
    }
    const cb = hot.cbs.get(url);
    if (cb) {
      try { cb(mod); log('hot-swapped', url); return; }
      catch (e) { log('accept callback threw, full reload', e); location.reload(); return; }
    }
    // No explicit acceptor: try to recreate the Angular root in place.
    if (recreateAngularRoot()) { log('recreated root for', url); return; }
    // Nothing could accept it: fall back to a page reload.
    log('no acceptor, full reload', url);
    location.reload();
  }

  const proto = location.protocol === 'https:' ? 'wss' : 'ws';
  const ws = new WebSocket(`${proto}://${location.host}/@treaty/ws`);
  ws.addEventListener('message', (ev) => {
    let msg;
    try { msg = JSON.parse(ev.data); } catch { location.reload(); return; }
    if (msg && msg.type === 'update') { applyUpdate(msg.url, msg.hash); }
    else { location.reload(); }
  });
  ws.addEventListener('close', () => {
    // Server went away (restart): poll until it is back, then reload.
    const tick = () => fetch('/@treaty/ping').then(() => location.reload()).catch(() => setTimeout(tick, 500));
    setTimeout(tick, 500);
  });
})();
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
                        Ok(msg) => {
                            if socket.send(Message::Text(msg.to_wire().into())).await.is_err() {
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

/// Build the shared [`ServerState`] + reload sender + resolved entry path for
/// `opts`. The state is shared (an `Arc`) by both the axum handlers AND the file
/// watcher so a change can recompile the changed module in the SAME cache the
/// handlers serve from.
fn build_state(opts: &ServeOptions) -> (Arc<ServerState>, broadcast::Sender<HmrMessage>, PathBuf) {
    let (reload_tx, _rx) = broadcast::channel::<HmrMessage>(64);
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
        source_maps: opts.source_maps,
        hmr: opts.hmr,
        entry_abs: entry_abs.clone(),
    });
    (state, reload_tx, entry_abs)
}

/// Build the configured axum [`Router`] for `opts`, plus the reload sender (so a
/// caller can wire file-watching) and the resolved entry path. Factored out of
/// [`serve_async`] so an integration test can serve the SAME app on an ephemeral
/// listener without binding the configured host/port or blocking forever.
/// An opaque guard that keeps a [`build_app_with_watch`] file watcher alive. The
/// watcher stops when this is dropped.
pub struct WatchHandle(#[allow(dead_code)] Option<notify::RecommendedWatcher>);

/// Like [`build_app`] but ALSO spawning the REAL file watcher (the one
/// [`serve_async`] uses), so an integration test can drive a genuine on-disk
/// change end-to-end through the SAME recompile-and-broadcast path the running
/// server uses. The returned [`WatchHandle`] must be held for the watcher to live.
pub fn build_app_with_watch(
    opts: &ServeOptions,
) -> (Router, broadcast::Sender<HmrMessage>, PathBuf, WatchHandle) {
    let (state, reload_tx, entry_abs) = build_state(opts);
    let watcher = spawn_reload_watcher(state.clone(), reload_tx.clone());
    let app = router(state, entry_abs.clone());
    (app, reload_tx, entry_abs, WatchHandle(watcher))
}

pub fn build_app(opts: &ServeOptions) -> (Router, broadcast::Sender<HmrMessage>, PathBuf) {
    let (state, reload_tx, entry_abs) = build_state(opts);
    let app = router(state, entry_abs.clone());
    (app, reload_tx, entry_abs)
}

/// The async server entry (separated so tests can drive it on an ephemeral port).
pub async fn serve_async(opts: ServeOptions) -> Result<(), String> {
    let (state, reload_tx, _entry) = build_state(&opts);
    let app = router(state.clone(), _entry.clone());

    // Watch the source tree. On a change the watcher recompiles ONLY the changed
    // module in `state`'s shared cache and broadcasts the per-module HMR decision
    // (hot `update` or `full-reload`) to every connected client.
    let _watcher = spawn_reload_watcher(state.clone(), reload_tx.clone());

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

/// Whether a changed module can be HOT-ACCEPTED (swapped without a page reload).
///
/// The entry (`main.ts`) owns bootstrap + providers, so a change there must
/// reload. A first-party authoring source (`.ts`/`.treaty`/`.tsx`) that is NOT
/// the entry is a component/directive/pipe/service module — hot-swappable. Any
/// other change (`index.html`, a `.css`, a non-authoring file) is not a module
/// the browser holds in its ES-module graph, so it reloads.
fn is_hot_acceptable(state: &ServerState, abs: &Path) -> bool {
    if !state.hmr {
        return false;
    }
    if paths_equal(abs, &state.entry_abs) {
        return false;
    }
    is_authoring_source(abs)
}

/// OS-appropriate path comparison (Windows is case-insensitive; the watcher and
/// the configured entry can differ in case / separators).
fn paths_equal(a: &Path, b: &Path) -> bool {
    let na = a.to_string_lossy().replace('\\', "/");
    let nb = b.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        na.eq_ignore_ascii_case(&nb)
    } else {
        na == nb
    }
}

/// Compute the HMR message for a changed file: recompile the module into the
/// shared cache and decide hot `update` vs. `full-reload`.
///
/// Returns `None` when the change should produce NO message (an unchanged-content
/// save, or a file the dev server does not serve). Otherwise the message is the
/// per-module decision. Pulled out of the watcher closure so it is unit testable.
fn hmr_message_for_change(state: &ServerState, abs: &Path) -> Option<HmrMessage> {
    // A removed file: nothing to recompile — fall back to a reload so the browser
    // re-evaluates (e.g. a deleted route module).
    if !abs.exists() {
        return Some(HmrMessage::FullReload);
    }

    // Only first-party authoring sources participate in module HMR; anything else
    // (index.html, assets) reloads the page.
    if !is_authoring_source(abs) {
        return Some(HmrMessage::FullReload);
    }

    if !is_hot_acceptable(state, abs) {
        // Entry / HMR-disabled: recompile is unnecessary, the client reloads.
        return Some(HmrMessage::FullReload);
    }

    // Recompile ONLY this module into the shared cache. If the rebuilt body is
    // byte-identical to the cached one (a no-op save), emit no message.
    let rebuilt = match build_module(state, abs) {
        Ok(m) => m,
        // A compile error: keep the old module, ask the browser to reload so the
        // error overlay/console surfaces on the next fetch.
        Err(_) => return Some(HmrMessage::FullReload),
    };
    let new_hash = rebuilt.content_hash;
    {
        let mut cache = state.cache.lock().unwrap();
        if let Some(prev) = cache.get(abs) {
            if prev.content_hash == new_hash {
                // Identical output → no HMR churn.
                return None;
            }
        }
        cache.insert(abs.to_path_buf(), rebuilt);
    }
    Some(HmrMessage::Update {
        url: fs_url(abs),
        hash: new_hash,
    })
}

/// Spawn a filesystem watcher that, on a change under the app's source tree,
/// recompiles the changed module and broadcasts its HMR decision. The returned
/// watcher must be kept alive.
fn spawn_reload_watcher(
    state: Arc<ServerState>,
    reload_tx: broadcast::Sender<HmrMessage>,
) -> Option<notify::RecommendedWatcher> {
    let root = state.root.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            use notify::EventKind;
            if !matches!(
                event.kind,
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
            ) {
                return;
            }
            // Each affected path gets its own per-module decision. A change with no
            // concrete path (rare) falls back to a reload.
            if event.paths.is_empty() {
                let _ = reload_tx.send(HmrMessage::FullReload);
                return;
            }
            for path in &event.paths {
                if let Some(msg) = hmr_message_for_change(&state, path) {
                    let _ = reload_tx.send(msg);
                }
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

    /// Default test state: source maps + HMR ON, entry at `<root>/src/main.ts`.
    fn state_for(root: &Path) -> Arc<ServerState> {
        state_with(root, true, true, root.join("src").join("main.ts"))
    }

    /// Test state with explicit flags + entry (so HMR-accept / maps-off paths are
    /// exercisable).
    fn state_with(root: &Path, source_maps: bool, hmr: bool, entry: PathBuf) -> Arc<ServerState> {
        let (tx, _rx) = broadcast::channel(8);
        Arc::new(ServerState {
            root: root.to_path_buf(),
            resolver: ModuleResolver::new(),
            cache: Mutex::new(HashMap::new()),
            reload_tx: tx,
            source_maps,
            hmr,
            entry_abs: entry,
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

    // ---- dev source maps -------------------------------------------------------

    /// Minimal standard-base64 decoder (test-only; avoids a dep just to inspect
    /// the inline data URL).
    fn b64_decode(s: &str) -> Vec<u8> {
        const TBL: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let val = |c: u8| TBL.iter().position(|&t| t == c).map(|p| p as u32);
        let mut out = Vec::new();
        let (mut buf, mut bits) = (0u32, 0u32);
        for &c in s.as_bytes() {
            if c == b'=' { break; }
            if let Some(v) = val(c) {
                buf = (buf << 6) | v;
                bits += 6;
                if bits >= 8 { bits -= 8; out.push((buf >> bits) as u8); }
            }
        }
        out
    }

    fn component_fixture(tag: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("treaty-serve-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let comp = "import { Component } from '@angular/core';\n@Component({ selector: 'app-card', template: '<p>{{label}}</p>' })\nexport class Card { label: string = 'hi'; }\n";
        let comp_path = dir.join("src").join("card.ts");
        std::fs::write(&comp_path, comp).unwrap();
        (dir, comp_path)
    }

    #[test]
    fn maps_on_appends_inline_sourcemap_pointing_at_original_ts() {
        let (dir, comp_path) = component_fixture("maps-on");
        let state = state_with(&dir, true, true, dir.join("src").join("main.ts"));
        let module = build_module(&state, &comp_path).expect("build ok");

        // The served body carries an inline sourceMappingURL data URL.
        assert!(
            module.body.contains("//# sourceMappingURL=data:application/json"),
            "no inline sourceMappingURL in maps-on body: {}",
            module.body
        );
        // Decode it and assert sources[] include the ORIGINAL authoring file and
        // the map is valid v3.
        let marker = "//# sourceMappingURL=";
        let idx = module.body.rfind(marker).unwrap();
        let url = module.body[idx + marker.len()..].trim();
        let b64 = url.rsplit("base64,").next().unwrap();
        let json = String::from_utf8(b64_decode(b64)).unwrap();
        assert!(json.contains("\"version\":3"), "not v3: {json}");
        let decoded =
            oxc_sourcemap::SourceMap::from_json_string(&json).expect("valid v3 sourcemap");
        let sources: Vec<String> = decoded.get_sources().map(|s| s.to_string()).collect();
        assert!(
            sources.iter().any(|s| s.replace('\\', "/").ends_with("src/card.ts")),
            "decoded sources do not include the original card.ts: {sources:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn maps_off_omits_the_sourcemap() {
        let (dir, comp_path) = component_fixture("maps-off");
        let state = state_with(&dir, false, true, dir.join("src").join("main.ts"));
        let module = build_module(&state, &comp_path).expect("build ok");
        assert!(
            !module.body.contains("sourceMappingURL"),
            "maps-off body still has a sourceMappingURL: {}",
            module.body
        );
        // Still real Ivy ESM — the map being off must not change the code path's output.
        assert!(module.body.contains("ɵɵdefineComponent"), "no Ivy def: {}", module.body);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn maps_on_and_off_emit_byte_identical_code() {
        // The map is purely additive: stripping the sourceMappingURL line from the
        // maps-on body must yield exactly the maps-off body.
        let (dir, comp_path) = component_fixture("maps-parity");
        let on = build_module(
            &state_with(&dir, true, true, dir.join("src").join("main.ts")),
            &comp_path,
        )
        .unwrap();
        let off = build_module(
            &state_with(&dir, false, true, dir.join("src").join("main.ts")),
            &comp_path,
        )
        .unwrap();
        let on_code: String = on
            .body
            .lines()
            .filter(|l| !l.starts_with("//# sourceMappingURL="))
            .collect::<Vec<_>>()
            .join("\n");
        let off_code = off.body.trim_end_matches('\n').to_string();
        assert_eq!(
            on_code.trim_end(),
            off_code.trim_end(),
            "code differs once the map line is removed:\nON:\n{}\nOFF:\n{}",
            on.body,
            off.body
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- module HMR ------------------------------------------------------------

    #[test]
    fn hmr_emits_update_not_reload_for_a_changed_component() {
        let (dir, comp_path) = component_fixture("hmr-update");
        // Entry is main.ts (absent), so card.ts is hot-acceptable.
        let state = state_with(&dir, true, true, dir.join("src").join("main.ts"));
        // Prime the cache with the first build.
        let first = build_module(&state, &comp_path).unwrap();
        state.cache.lock().unwrap().insert(comp_path.clone(), first);

        // Edit the component body (changes the emitted output).
        std::fs::write(
            &comp_path,
            "import { Component } from '@angular/core';\n@Component({ selector: 'app-card', template: '<p>{{label}}!</p>' })\nexport class Card { label: string = 'changed'; }\n",
        )
        .unwrap();

        let msg = hmr_message_for_change(&state, &comp_path).expect("a message");
        match msg {
            HmrMessage::Update { url, hash } => {
                assert!(url.contains("/@fs/") && url.replace('\\', "/").ends_with("src/card.ts"),
                    "update url not the changed module: {url}");
                assert_ne!(hash, 0, "hash should be set");
                // The wire form is an `update`, NOT a reload.
                let wire = msg_wire(&HmrMessage::Update { url, hash });
                assert!(wire.contains("\"type\":\"update\""), "wire not an update: {wire}");
                assert!(!wire.contains("full-reload"), "update wire leaked a reload: {wire}");
            }
            HmrMessage::FullReload => panic!("expected an HMR update, got full-reload"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Re-expose the private wire form for assertions.
    fn msg_wire(m: &HmrMessage) -> String {
        m.to_wire()
    }

    #[test]
    fn hmr_falls_back_to_full_reload_for_the_entry_module() {
        let (dir, _comp) = component_fixture("hmr-entry");
        // Make main.ts the entry AND the changed file.
        let entry = dir.join("src").join("main.ts");
        std::fs::write(
            &entry,
            "import { bootstrapApplication } from '@angular/platform-browser';\nexport const x = 1;\n",
        )
        .unwrap();
        let state = state_with(&dir, true, true, entry.clone());
        let msg = hmr_message_for_change(&state, &entry).expect("a message");
        assert!(
            matches!(msg, HmrMessage::FullReload),
            "entry change must full-reload, got {msg:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hmr_disabled_full_reloads_even_for_a_component() {
        let (dir, comp_path) = component_fixture("hmr-off");
        let state = state_with(&dir, true, false, dir.join("src").join("main.ts"));
        let msg = hmr_message_for_change(&state, &comp_path).expect("a message");
        assert!(matches!(msg, HmrMessage::FullReload), "hmr-off must reload, got {msg:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hmr_no_op_save_emits_no_message() {
        let (dir, comp_path) = component_fixture("hmr-noop");
        let state = state_with(&dir, true, true, dir.join("src").join("main.ts"));
        let first = build_module(&state, &comp_path).unwrap();
        state.cache.lock().unwrap().insert(comp_path.clone(), first);
        // "Save" without changing content: identical output → no HMR churn.
        let msg = hmr_message_for_change(&state, &comp_path);
        assert!(msg.is_none(), "no-op save should emit no message, got {msg:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn client_runtime_is_hmr_not_blind_reload() {
        // The injected client must run module HMR (dynamic import + accept hooks),
        // not the old unconditional location.reload on every message.
        assert!(CLIENT_JS.contains("__treatyHmr"), "no HMR global in client runtime");
        assert!(CLIENT_JS.contains("import("), "client does not dynamic-import the new module");
        assert!(CLIENT_JS.contains("'update'"), "client does not handle update messages");
        assert!(CLIENT_JS.contains("accept"), "client exposes no accept hook");
    }
}
