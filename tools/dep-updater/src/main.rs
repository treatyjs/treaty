//! `dep-updater` CLI.
//!
//! This binary is the entry point for the deterministic dependency-update loop.
//! The data model and engine live in the library crate (`dep_updater`); this
//! file wires up argument handling over them.
//!
//! Commands:
//! * `detect`  — parse the repo's manifests and list the discovered direct
//!   dependencies and their pinned versions (read-only; no network).
//! * `rules`   — print the seeded codemod rule set (the OXC 0.29 -> 0.133 crib).
//! * `dry-run` — for a synthetic bump, print the action plan the execute path
//!   would take (no manifest edits, no processes).
//!
//! Every step is deterministic and idempotent. No AI, GPU, or model is involved.

use std::process::ExitCode;

use dep_updater::detect::{default_repo_root, discover_dependencies};
use dep_updater::model::{DepKind, UpdatePlan};
use dep_updater::orchestrate::{Mode, OrchestrationConfig, Orchestrator, Outcome};
use dep_updater::codemod::oxc_29_to_133_rules;

const USAGE: &str = "\
dep-updater — deterministic, AI-free dependency updater for the treaty repo

USAGE:
    dep-updater <COMMAND>

COMMANDS:
    detect      Parse workspace manifests and list discovered dependencies (read-only)
    rules       Print the seeded codemod rule set (oxc 0.29 -> 0.133 crib)
    dry-run     Print the bump->verify->codemod action plan for a synthetic bump
    version     Print version and exit
    help        Print this help and exit

All steps are deterministic and idempotent. No AI, GPU, or model is involved.";

fn cmd_detect() -> ExitCode {
    let root = default_repo_root();
    match discover_dependencies(&root) {
        Ok(deps) => {
            println!("discovered {} direct dependencies under {}", deps.len(), root.display());
            for d in &deps {
                println!(
                    "  [{}] {} = {} ({}) <- {}",
                    d.kind, d.name, d.current, d.requirement, d.manifest
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("dep-updater detect: {e}");
            ExitCode::from(1)
        }
    }
}

fn cmd_rules() -> ExitCode {
    let set = oxc_29_to_133_rules();
    println!("{} seeded codemod rules (oxc 0.29 -> 0.133):", set.rules.len());
    for r in &set.rules {
        println!("  {} [{}>={}]: {}", r.id, r.dep, r.from, r.description);
    }
    ExitCode::SUCCESS
}

fn cmd_dry_run() -> ExitCode {
    // A representative oxc bump so the plan exercises codemod selection without
    // touching the network or the working tree.
    let plan = UpdatePlan::new(
        "oxc_ast",
        DepKind::Crate,
        semver::Version::new(0, 29, 0),
        semver::Version::new(0, 133, 0),
        "Cargo.toml",
    );
    let rules = oxc_29_to_133_rules();
    let orch = Orchestrator::new(OrchestrationConfig { mode: Mode::DryRun, ..Default::default() }, &rules);
    // A dry run never reads or writes the repo, so no Repo work happens; we
    // print the planned action sequence directly.
    match orch.run_plan_dry(&plan) {
        Outcome::Planned { plan, actions } => {
            println!("dry-run plan for {} ({} actions):", plan.id(), actions.len());
            for (i, a) in actions.iter().enumerate() {
                println!("  {}. {a:?}", i + 1);
            }
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("dep-updater dry-run: unexpected outcome {other:?}");
            ExitCode::from(1)
        }
    }
}

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
        "detect" => cmd_detect(),
        "rules" => cmd_rules(),
        "dry-run" => cmd_dry_run(),
        other => {
            eprintln!("dep-updater: unknown command '{other}'\n");
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
