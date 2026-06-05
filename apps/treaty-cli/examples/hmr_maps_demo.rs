//! Live demo of HMR + dev source maps against an on-disk Angular app.
//!
//! Run: `cargo run -p treaty_cli --example hmr_maps_demo -- <app-dir> [entry-rel]`
//! e.g. `cargo run -p treaty_cli --example hmr_maps_demo -- examples/ng-bench-app/src src/main.ts`
//!
//! It (1) stands the native dev server up on an ephemeral port WITH the real file
//! watcher, (2) GETs a real component module and verifies it carries an inline
//! source map whose decoded `sources[]` include the ORIGINAL `.ts`, then (3)
//! touches that component on disk and prints the HMR websocket message the server
//! broadcasts (an `update`, not a full reload). It also times the cold first-byte.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use treaty_cli::serve::{build_app_with_watch, HmrMessage, ServeOptions};

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let maps_on = !raw.iter().any(|a| a == "--no-source-map");
    let mut positional = raw.iter().filter(|a| !a.starts_with("--"));
    let app_dir = PathBuf::from(
        positional.next().cloned().unwrap_or_else(|| "examples/ng-bench-app/src".to_string()),
    );
    let entry_rel = positional.next().cloned().unwrap_or_else(|| "main.ts".to_string());

    let root = strip_unc(std::fs::canonicalize(&app_dir).unwrap_or(app_dir.clone()));
    let entry = root.join(&entry_rel);
    println!("[demo] app root = {}", root.display());
    println!("[demo] entry    = {}", entry.display());

    let mut opts = ServeOptions::new(root.clone(), entry, "127.0.0.1".into(), 0);
    opts.source_maps = maps_on;
    println!("[demo] source maps = {}", if maps_on { "ON" } else { "OFF" });
    let (app, reload_tx, _entry, _watch) = build_app_with_watch(&opts);
    let mut rx = reload_tx.subscribe();

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let (tx, prx) = std::sync::mpsc::channel();
    rt.spawn(async move {
        let l = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = l.local_addr().unwrap().port();
        tx.send(port).unwrap();
        axum::serve(l, app).await.unwrap();
    });
    let port = prx.recv().unwrap();
    println!("[demo] serving on http://127.0.0.1:{port}/");

    // (A) Cold first byte for the root component module.
    let comp = pick_component(&root).expect("a component .ts under the app");
    let comp_url = fs_url(&comp);
    let t0 = Instant::now();
    let body = http_get(port, &comp_url);
    let cold_ms = t0.elapsed().as_secs_f64() * 1000.0;
    println!("[demo] GET {comp_url}");
    println!("[demo] cold compile+serve first byte: {cold_ms:.1} ms ({} bytes)", body.len());
    assert!(body.contains("ɵɵdefineComponent") || body.contains("ɵɵdefineDirective"),
        "served module is not Ivy: first 200 = {:?}", &body[..body.len().min(200)]);

    // (B) Source map present + points at the original .ts.
    if let Some(map_json) = decode_inline_map(&body) {
        let orig = map_json.replace('\\', "/");
        let names: Vec<&str> = comp.file_name().and_then(|s| s.to_str()).into_iter().collect();
        let has_orig = names.iter().all(|n| orig.contains(n)) && orig.contains("\"version\":3");
        println!("[demo] inline source map: present (v3={})", orig.contains("\"version\":3"));
        println!("[demo] map sources include original {:?}: {}", names, has_orig);
    } else if maps_on {
        println!("[demo] NO inline source map found (unexpected with maps ON)");
    } else {
        println!("[demo] no inline source map (maps OFF, as requested) — smaller payload");
    }

    // (C) Warm second byte (cache hit).
    let t1 = Instant::now();
    let _ = http_get(port, &comp_url);
    println!("[demo] warm cache-hit serve: {:.2} ms", t1.elapsed().as_secs_f64() * 1000.0);

    // (D) Touch the component -> observe the HMR broadcast.
    println!("[demo] touching {} ...", comp.display());
    let original = std::fs::read_to_string(&comp).unwrap();
    // Append a harmless whitespace-only change won't alter output (no-op); instead
    // flip a comment so the emitted code changes.
    let edited = format!("{original}\n// treaty-hmr-demo edit {}\n", now_tag());
    std::fs::write(&comp, &edited).unwrap();

    match recv_within(&mut rx, Duration::from_secs(10)) {
        Some(HmrMessage::Update { url, hash }) => {
            println!("[demo] HMR broadcast: UPDATE  url={url}  hash={hash}");
            println!("[demo] wire = {}", HmrMessage::Update { url, hash }.to_wire());
            println!("[demo] -> client hot-swaps the module, NO page reload");
        }
        Some(HmrMessage::FullReload) => {
            println!("[demo] HMR broadcast: FULL-RELOAD (expected for entry/provider changes)");
        }
        None => println!("[demo] (no broadcast within timeout — a comment-only edit may be a no-op)"),
    }

    // Restore the file.
    std::fs::write(&comp, original).unwrap();
    println!("[demo] done.");
}

/// Strip the Windows `\\?\` extended-length prefix `canonicalize` adds, so the
/// `/@fs/<C:/...>` URL scheme round-trips.
fn strip_unc(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        p
    }
}

fn now_tag() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

/// Find a first component-ish `.ts` (not main.ts / *.config / *.routes) under the app.
fn pick_component(root: &Path) -> Option<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    if p.file_name().and_then(|s| s.to_str()) != Some("node_modules") {
                        walk(&p, out);
                    }
                } else if p.extension().and_then(|s| s.to_str()) == Some("ts") {
                    out.push(p);
                }
            }
        }
    }
    let mut all = Vec::new();
    walk(root, &mut all);
    // Prefer a file whose source declares an @Component.
    all.iter()
        .find(|p| {
            let n = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            n != "main.ts"
                && std::fs::read_to_string(p).map(|s| s.contains("@Component")).unwrap_or(false)
        })
        .cloned()
        .or_else(|| all.into_iter().next())
}

fn fs_url(abs: &Path) -> String {
    format!(
        "/@fs/{}",
        abs.to_string_lossy().replace('\\', "/").trim_start_matches('/')
    )
}

fn decode_inline_map(body: &str) -> Option<String> {
    let idx = body.rfind("//# sourceMappingURL=")?;
    let url = body[idx..].lines().next()?.trim_start_matches("//# sourceMappingURL=").trim();
    let b64 = url.rsplit("base64,").next()?;
    String::from_utf8(b64_decode(b64)).ok()
}

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

fn http_get(port: u16, path: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    let req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw);
    text.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_else(|| text.to_string())
}

fn recv_within(
    rx: &mut tokio::sync::broadcast::Receiver<HmrMessage>,
    timeout: Duration,
) -> Option<HmrMessage> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    rt.block_on(async move {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut last = None;
        loop {
            let rem = deadline.saturating_duration_since(tokio::time::Instant::now());
            if rem.is_zero() { return last; }
            match tokio::time::timeout(rem, rx.recv()).await {
                Ok(Ok(m)) => {
                    if matches!(m, HmrMessage::Update { .. }) { return Some(m); }
                    last = Some(m);
                }
                _ => return last,
            }
        }
    })
}
