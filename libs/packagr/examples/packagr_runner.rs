//! Thin CLI runner over [`treaty_packagr`] for the packagr benchmark.
//!
//! Usage: `cargo run -p treaty_packagr --release --example packagr_runner -- <lib_dir>`
//!
//! Packages the library rooted at `<lib_dir>` (a directory carrying a
//! `treaty-package.json` / `ng-package.json` descriptor) to disk via
//! [`treaty_packagr::build_to_disk`], timing ONLY the packaging call (not process
//! startup), and prints a single JSON object to stdout:
//!
//! ```json
//! { "ok": true, "ms": 1.23, "name": "...", "version": "...",
//!   "dest": "<abs dist dir>", "entries": [ { "dir": ".", "subPath": "" }, ... ] }
//! ```
//!
//! This exists so the `.mjs` benchmark can invoke the Rust packagr (which has no
//! NAPI binding) as a subprocess and read back a precise in-process timing — the
//! same `build_to_disk` the crate's own end-to-end tests drive.

use std::path::Path;
use std::time::Instant;

use treaty_packagr::{build_to_disk, PackageConfig};

fn main() {
    let dir = match std::env::args().nth(1) {
        Some(d) => d,
        None => {
            eprintln!("usage: packagr_runner <lib_dir>");
            std::process::exit(2);
        }
    };
    let lib = Path::new(&dir);

    // Parse the descriptor the same way `package_library_at` does, but keep the
    // parsed config so we can both build and report the resolved dest.
    let descriptor = match ["treaty-package.json", "ng-package.json"]
        .iter()
        .map(|n| lib.join(n))
        .find(|p| p.is_file())
    {
        Some(p) => p,
        None => {
            println!(
                "{{\"ok\":false,\"error\":\"no treaty-package.json / ng-package.json in {}\"}}",
                json_escape(&dir)
            );
            std::process::exit(1);
        }
    };
    let text = match std::fs::read_to_string(&descriptor) {
        Ok(t) => t,
        Err(e) => {
            println!("{{\"ok\":false,\"error\":\"read descriptor: {}\"}}", json_escape(&e.to_string()));
            std::process::exit(1);
        }
    };
    let cfg = match PackageConfig::from_json(&text) {
        Ok(c) => c,
        Err(e) => {
            println!("{{\"ok\":false,\"error\":\"parse descriptor: {}\"}}", json_escape(&e.to_string()));
            std::process::exit(1);
        }
    };

    // Time ONLY the packaging + disk emit (compile every entry to Ivy + .d.ts,
    // flatten FESM, write the APF dist tree). Best of N runs is the caller's job;
    // this runner reports one clean build.
    let dest_abs = lib.join(cfg.dest());
    let start = Instant::now();
    match build_to_disk(lib, &cfg) {
        Ok(dist) => {
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            let entries: Vec<String> = dist
                .entries
                .iter()
                .map(|e| {
                    format!(
                        "{{\"dir\":\"{}\",\"subPath\":\"{}\"}}",
                        json_escape(&e.dir),
                        json_escape(&e.sub_path)
                    )
                })
                .collect();
            println!(
                "{{\"ok\":true,\"ms\":{:.4},\"name\":\"{}\",\"version\":\"{}\",\"dest\":\"{}\",\"entries\":[{}]}}",
                ms,
                json_escape(&dist.name),
                json_escape(&dist.version),
                json_escape(&dest_abs.to_string_lossy()),
                entries.join(",")
            );
        }
        Err(e) => {
            println!("{{\"ok\":false,\"error\":\"{}\"}}", json_escape(&e.to_string()));
            std::process::exit(1);
        }
    }
}

/// Minimal JSON string escaping (quotes, backslashes, control chars).
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}
