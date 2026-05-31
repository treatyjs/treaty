//! `dep-updater` CLI.
//!
//! This binary is the entry point for the deterministic dependency-update loop.
//! The data model and engine live in the library crate (`dep_updater`); this
//! file wires up argument handling and a default `--help`/`version` surface.
//!
//! Subcommands (detector, codemod engine, and the apply/verify/PR orchestration)
//! are implemented in parallel workstreams against the library types defined in
//! [`dep_updater::model`]. Until those land, the CLI reports its version and the
//! contract it operates under so it is runnable and self-describing.

use std::process::ExitCode;

const USAGE: &str = "\
dep-updater — deterministic, AI-free dependency updater for the treaty repo

USAGE:
    dep-updater <COMMAND>

COMMANDS:
    detect      Parse workspace manifests + query registries -> emit UpdatePlans
    apply       Apply one UpdatePlan on a branch (bump the manifest)
    verify      Run the repo gates (build/test/oracle/oxlint) -> VerifyResult
    codemod     Run the codemod rules matching a bump, then re-verify
    version     Print version and exit
    help        Print this help and exit

All steps are deterministic and idempotent. No AI, GPU, or model is involved.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("help");

    match command {
        "version" | "--version" | "-V" => {
            println!("dep-updater {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        "detect" | "apply" | "verify" | "codemod" => {
            // Subcommand bodies are delivered by the parallel detector / engine
            // / orchestration workstreams against the library types. The CLI
            // contract and routing are fixed here so those can plug in.
            eprintln!(
                "dep-updater: '{command}' is not wired up in this build; \
                 run via the library API (dep_updater::model)."
            );
            ExitCode::from(2)
        }
        other => {
            eprintln!("dep-updater: unknown command '{other}'\n");
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
