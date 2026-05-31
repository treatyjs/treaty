//! `render3-sync` CLI — the entry point for the deterministic (NO-AI) render3 sync harness.
//!
//! Scaffold subcommands:
//!   * `map`        — print the symbol->module map as JSON (the maintained pillar-1 table).
//!   * `exports <ts-file>` — parse a TypeScript file under the vendored Angular ref and print its
//!     exported symbol names + the Rust modules each maps to.
//!
//! Drift diffing, conformance regen, and mechanical codegen are layered on top of these
//! primitives (see `migration/RENDER3-SYNC-PLAN.md`).

use std::process::ExitCode;

use render3_sync::symbol_map::{self, MODULE_MAP};
use render3_sync::ts;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("map") => {
            print_map();
            ExitCode::SUCCESS
        }
        Some("exports") => match args.get(1) {
            Some(path) => match std::fs::read_to_string(path) {
                Ok(src) => {
                    print_exports(path, &src);
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("render3-sync: cannot read {path}: {e}");
                    ExitCode::FAILURE
                }
            },
            None => {
                eprintln!("render3-sync: `exports` requires a path to a .ts file");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!(
                "render3-sync — keep the Rust render3 port 1:1 with Angular (NO AI)\n\
                 usage:\n  \
                 render3-sync map                 print the symbol->module map as JSON\n  \
                 render3-sync exports <ts-file>   list exported symbols of a TS file + Rust owners"
            );
            ExitCode::FAILURE
        }
    }
}

/// Serialize the static symbol->module map to JSON on stdout.
fn print_map() {
    // ModuleMapping is Serialize; emit the whole table.
    match serde_json::to_string_pretty(MODULE_MAP) {
        Ok(json) => println!("{json}"),
        Err(e) => eprintln!("render3-sync: failed to serialize map: {e}"),
    }
}

/// Parse a TS file and print, per exported symbol, the Rust modules that port it.
fn print_exports(path: &str, src: &str) {
    let exports = ts::exported_symbols(src);
    println!("{path}: {} exported symbols", exports.len());
    for sym in &exports {
        let owners = symbol_map::rust_modules_for_symbol(&sym.name);
        let owners: Vec<&str> = owners.iter().map(|m| m.rust_file).collect();
        let owned = if owners.is_empty() {
            "<unmapped>".to_string()
        } else {
            owners.join(", ")
        };
        println!("  {:<40} {:?}  ->  {owned}", sym.name, sym.kind);
    }
}
