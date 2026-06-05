//! End-to-end tests for the native `treaty serve` + `treaty build` paths.
//!
//! These drive the REAL pipelines (Ivy compile + oxc type-strip + import rewrite,
//! and the module-graph crawl) against a small on-disk fixture — no Node, no
//! external bundler. The serve test stands the axum app up on an ephemeral
//! listener and speaks raw HTTP/1.1 to it (so no HTTP-client dependency is
//! needed); the build test crawls a two-module graph and asserts the dist is a
//! self-contained, import-rewritten ESM tree with an entry-booting `index.html`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;

use treaty_cli::native_build::{build as native_build, NativeBuildOptions};
use treaty_cli::serve::{build_app, build_app_with_watch, HmrMessage, ServeOptions};

/// Make a fresh temp app dir with an index.html, a component entry, and a sibling.
fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("treaty-e2e-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("index.html"),
        "<!doctype html><html><body><app-root></app-root></body></html>",
    )
    .unwrap();
    // A sibling module the entry imports.
    std::fs::write(
        dir.join("src").join("greeting.ts"),
        "export const greeting: string = 'hello';",
    )
    .unwrap();
    // A bare @Component entry importing the sibling (exercises Ivy + TS strip +
    // relative-import rewrite end to end).
    std::fs::write(
        dir.join("src").join("main.ts"),
        "import { Component } from '@angular/core';\n\
         import { greeting } from './greeting';\n\
         @Component({ selector: 'app-root', template: '<h1>{{msg}}</h1>' })\n\
         export class App { msg: string = greeting; }\n",
    )
    .unwrap();
    dir
}

/// Minimal blocking HTTP/1.1 GET against `127.0.0.1:port`. Returns the body.
fn http_get(port: u16, path: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    let req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).expect("write");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read");
    let text = String::from_utf8_lossy(&raw);
    // Split off headers; return the body.
    match text.split_once("\r\n\r\n") {
        Some((_, body)) => body.to_string(),
        None => text.to_string(),
    }
}

#[test]
fn serve_compiles_and_serves_ivy_esm_and_bootable_index() {
    let dir = fixture("serve");
    let opts = ServeOptions::new(
        dir.clone(),
        dir.join("src").join("main.ts"),
        "127.0.0.1".into(),
        0,
    );
    let (app, _reload_tx, _entry) = build_app(&opts);

    // Stand the app up on an ephemeral port in a background runtime thread.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let (port_tx, port_rx) = std::sync::mpsc::channel();
    let handle = rt.spawn(async move {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        port_tx.send(port).unwrap();
        axum::serve(listener, app).await.unwrap();
    });
    let port = port_rx.recv().unwrap();

    // (1) GET / returns the app shell with the entry rewritten + client injected.
    let index = http_get(port, "/");
    assert!(index.contains("<app-root></app-root>"), "shell lost: {index}");
    assert!(index.contains("/@treaty/client.js"), "client not injected: {index}");
    assert!(index.contains("/@fs/"), "entry not rewritten: {index}");

    // (2) GET the entry module returns Ivy ESM with no TS and rewritten imports.
    let main_abs = dir.join("src").join("main.ts");
    let main_url = format!(
        "/@fs/{}",
        main_abs
            .to_string_lossy()
            .replace('\\', "/")
            .trim_start_matches('/')
    );
    let module = http_get(port, &main_url);
    assert!(module.contains("ɵɵdefineComponent"), "no Ivy def in served module: {module}");
    assert!(!module.contains(": string"), "TS type survived in served module: {module}");
    assert!(
        module.contains("/@fs/") && module.contains("greeting"),
        "relative import not rewritten in served module: {module}"
    );

    // (3) The live-reload client is served as JS.
    let client = http_get(port, "/@treaty/client.js");
    assert!(client.contains("WebSocket"), "client.js missing WS: {client}");

    handle.abort();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Standard-base64 decode (test-only) for inspecting an inline data-URL map.
fn b64_decode(s: &str) -> Vec<u8> {
    const TBL: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let val = |c: u8| TBL.iter().position(|&t| t == c).map(|p| p as u32);
    let mut out = Vec::new();
    let (mut buf, mut bits) = (0u32, 0u32);
    for &c in s.as_bytes() {
        if c == b'=' {
            break;
        }
        if let Some(v) = val(c) {
            buf = (buf << 6) | v;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((buf >> bits) as u8);
            }
        }
    }
    out
}

fn fs_url_for(abs: &std::path::Path) -> String {
    format!(
        "/@fs/{}",
        abs.to_string_lossy().replace('\\', "/").trim_start_matches('/')
    )
}

/// Stand the app up on an ephemeral port in a background runtime; returns the
/// runtime, the join handle, and the bound port.
fn spawn_server(
    app: axum::Router,
) -> (tokio::runtime::Runtime, tokio::task::JoinHandle<()>, u16) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let (port_tx, port_rx) = std::sync::mpsc::channel();
    let handle = rt.spawn(async move {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        port_tx.send(port).unwrap();
        axum::serve(listener, app).await.unwrap();
    });
    let port = port_rx.recv().unwrap();
    (rt, handle, port)
}

#[test]
fn serve_with_source_maps_on_attaches_inline_map_pointing_at_original() {
    let dir = fixture("serve-maps-on");
    // Maps ON (the serve default).
    let opts = ServeOptions::new(
        dir.clone(),
        dir.join("src").join("main.ts"),
        "127.0.0.1".into(),
        0,
    );
    let (app, _tx, _entry) = build_app(&opts);
    let (_rt, handle, port) = spawn_server(app);

    let main_abs = dir.join("src").join("main.ts");
    let module = http_get(port, &fs_url_for(&main_abs));
    assert!(module.contains("ɵɵdefineComponent"), "no Ivy def: {module}");
    // The served module carries an inline sourceMappingURL whose decoded sources
    // include the ORIGINAL main.ts.
    let marker = "//# sourceMappingURL=";
    let idx = module.rfind(marker).unwrap_or_else(|| panic!("no inline map: {module}"));
    let url = module[idx + marker.len()..].trim();
    assert!(url.starts_with("data:application/json"), "not an inline data url: {url}");
    let b64 = url.rsplit("base64,").next().unwrap();
    let json = String::from_utf8(b64_decode(b64)).unwrap();
    assert!(json.contains("\"version\":3"), "not v3: {json}");
    assert!(
        json.replace('\\', "/").contains("main.ts"),
        "decoded map sources do not include original main.ts: {json}"
    );

    handle.abort();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn serve_with_source_maps_off_omits_the_map() {
    let dir = fixture("serve-maps-off");
    let mut opts = ServeOptions::new(
        dir.clone(),
        dir.join("src").join("main.ts"),
        "127.0.0.1".into(),
        0,
    );
    opts.source_maps = false;
    let (app, _tx, _entry) = build_app(&opts);
    let (_rt, handle, port) = spawn_server(app);

    let main_abs = dir.join("src").join("main.ts");
    let module = http_get(port, &fs_url_for(&main_abs));
    assert!(module.contains("ɵɵdefineComponent"), "no Ivy def: {module}");
    assert!(
        !module.contains("sourceMappingURL"),
        "maps-off served module still has a sourceMappingURL: {module}"
    );

    handle.abort();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn serve_hmr_broadcasts_module_update_not_full_reload_on_a_component_change() {
    let dir = fixture("serve-hmr");
    // A non-entry component the entry imports — this is the hot-swappable module.
    std::fs::write(
        dir.join("src").join("widget.ts"),
        "import { Component } from '@angular/core';\n\
         @Component({ selector: 'app-widget', template: '<p>{{n}}</p>' })\n\
         export class Widget { n: number = 1; }\n",
    )
    .unwrap();

    let opts = ServeOptions::new(
        dir.clone(),
        dir.join("src").join("main.ts"),
        "127.0.0.1".into(),
        0,
    );
    // Spawn the REAL watcher so a genuine on-disk edit flows through the real
    // recompile-and-broadcast path.
    let (app, reload_tx, _entry, _watch) = build_app_with_watch(&opts);
    let mut rx = reload_tx.subscribe();
    let (_rt, handle, port) = spawn_server(app);

    // Prime the server's cache for widget.ts (so the watcher compares against a
    // known prior build and detects the change).
    let widget_abs = dir.join("src").join("widget.ts");
    let _ = http_get(port, &fs_url_for(&widget_abs));

    // Edit the component (changes the emitted output -> a real HMR update).
    std::fs::write(
        &widget_abs,
        "import { Component } from '@angular/core';\n\
         @Component({ selector: 'app-widget', template: '<p>{{n}}!!</p>' })\n\
         export class Widget { n: number = 42; }\n",
    )
    .unwrap();

    // Wait (bounded) for the watcher to broadcast a message.
    let msg = recv_within(&mut rx, std::time::Duration::from_secs(10))
        .expect("an HMR broadcast arrived");
    match &msg {
        HmrMessage::Update { url, .. } => {
            assert!(
                url.replace('\\', "/").ends_with("src/widget.ts"),
                "update targeted the wrong module: {url}"
            );
            let wire = msg.to_wire();
            assert!(wire.contains("\"type\":\"update\""), "not an update wire: {wire}");
            assert!(!wire.contains("full-reload"), "update leaked a full reload: {wire}");
        }
        HmrMessage::FullReload => panic!("component change should hot-update, got full-reload"),
    }

    handle.abort();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Block (off a tiny dedicated runtime) until an HMR message arrives or the
/// deadline passes. Drains intermediate messages, returning the first `Update`
/// if present, else the last message seen.
fn recv_within(
    rx: &mut tokio::sync::broadcast::Receiver<HmrMessage>,
    timeout: std::time::Duration,
) -> Option<HmrMessage> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    rt.block_on(async move {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut last: Option<HmrMessage> = None;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return last;
            }
            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Ok(m)) => {
                    if matches!(m, HmrMessage::Update { .. }) {
                        return Some(m);
                    }
                    last = Some(m);
                }
                Ok(Err(_)) => return last, // channel closed
                Err(_) => return last,     // timed out
            }
        }
    })
}

#[test]
fn native_build_emits_a_self_contained_bootable_esm_dist() {
    let dir = fixture("build");
    let out_dir = dir.join("dist");
    let out = native_build(&NativeBuildOptions {
        root: dir.clone(),
        entry: dir.join("src").join("main.ts"),
        out_dir: out_dir.clone(),
    })
    .expect("native build ok");

    // index.html + at least the entry module emitted.
    assert!(out.written.len() >= 2, "too few outputs: {:?}", out.written);

    // index.html boots the entry's dist module.
    let index = std::fs::read_to_string(out_dir.join("index.html")).unwrap();
    assert!(index.contains("./_treaty/main-"), "index does not boot entry: {index}");
    assert!(index.contains("type=\"module\""), "entry script not a module: {index}");

    // The entry module: Ivy lowered, types stripped, sibling import rewritten to a
    // dist-relative file (the graph crawl pulled greeting.ts in and rewrote the
    // edge), and the @Component decorator gone.
    let modules_dir = out_dir.join("_treaty");
    let entry_file = std::fs::read_dir(&modules_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.file_name().to_string_lossy().starts_with("main-"))
        .map(|e| e.path())
        .expect("entry module emitted");
    let entry_code = std::fs::read_to_string(&entry_file).unwrap();
    assert!(entry_code.contains("ɵɵdefineComponent"), "no Ivy: {entry_code}");
    assert!(!entry_code.contains("@Component"), "decorator survived: {entry_code}");
    assert!(!entry_code.contains(": string"), "TS type survived: {entry_code}");
    assert!(entry_code.contains("./greeting-"), "sibling edge not rewritten: {entry_code}");

    // The sibling module was emitted too (the crawl followed the import).
    let has_greeting = std::fs::read_dir(&modules_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| e.file_name().to_string_lossy().starts_with("greeting-"));
    assert!(has_greeting, "sibling module not emitted by the graph crawl");

    let _ = std::fs::remove_dir_all(&dir);
}
