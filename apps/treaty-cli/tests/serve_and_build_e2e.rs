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
use treaty_cli::serve::{build_app, ServeOptions};

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
    let opts = ServeOptions {
        root: dir.clone(),
        entry: dir.join("src").join("main.ts"),
        host: "127.0.0.1".into(),
        port: 0,
    };
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
