//! `conformance` — pillar 2 CLI.
//!
//! Runs the existing `libs/render3` compliance + oracle JS harnesses as child processes (READ-ONLY:
//! it never runs `cargo` for render3 and never edits `libs/render3`), parses their output into a
//! [`ConformanceReport`], and either emits it as JSON or diffs it against a previously captured
//! baseline JSON to print the **newly-failing drift surface**.
//!
//! Usage:
//!   conformance run [--repo-root <dir>] [--out <file.json>]
//!       Run both harnesses; print the structured report as JSON (and optionally write it).
//!   conformance compare <baseline.json> <current.json>
//!       Diff two captured reports; print the newly-failing cases (the drift surface). Exits 1 if
//!       anything newly failed.
//!
//! `--repo-root` defaults to the current directory's nearest ancestor containing `libs/render3`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use render3_sync::conformance::{
    self, BaselineComparison, ConformanceReport,
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("run") => cmd_run(&args[1..]),
        Some("compare") => cmd_compare(&args[1..]),
        _ => {
            eprintln!(
                "conformance — render3 conformance gate (NO AI)\n\
                 usage:\n  \
                 conformance run [--repo-root <dir>] [--out <file.json>]   run harnesses, print report JSON\n  \
                 conformance compare <baseline.json> <current.json>        print the newly-failing drift surface"
            );
            ExitCode::FAILURE
        }
    }
}

fn cmd_run(args: &[String]) -> ExitCode {
    let mut repo_root: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--repo-root" => {
                let Some(v) = args.get(i + 1) else {
                    eprintln!("conformance: --repo-root requires a path");
                    return ExitCode::FAILURE;
                };
                repo_root = Some(PathBuf::from(v));
                i += 2;
            }
            "--out" => {
                let Some(v) = args.get(i + 1) else {
                    eprintln!("conformance: --out requires a path");
                    return ExitCode::FAILURE;
                };
                out = Some(PathBuf::from(v));
                i += 2;
            }
            other => {
                eprintln!("conformance: unknown flag {other}");
                return ExitCode::FAILURE;
            }
        }
    }

    let root = match repo_root.or_else(find_repo_root) {
        Some(r) => r,
        None => {
            eprintln!("conformance: could not locate repo root (no ancestor contains libs/render3); pass --repo-root");
            return ExitCode::FAILURE;
        }
    };

    let (report, errors) = conformance::run_conformance(&root);
    for e in &errors {
        eprintln!("conformance: {e}");
    }

    let json = match serde_json::to_string_pretty(&report) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("conformance: failed to serialize report: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("{json}");
    if let Some(path) = out
        && let Err(e) = std::fs::write(&path, &json)
    {
        eprintln!("conformance: failed to write {}: {e}", path.display());
        return ExitCode::FAILURE;
    }

    // A run that produced neither harness's numbers is a hard failure; otherwise success.
    if report.compliance.is_none() && report.oracle.is_none() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn cmd_compare(args: &[String]) -> ExitCode {
    let (Some(prev_path), Some(curr_path)) = (args.first(), args.get(1)) else {
        eprintln!("conformance: compare requires <baseline.json> <current.json>");
        return ExitCode::FAILURE;
    };

    let prev = match load_report(Path::new(prev_path)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("conformance: {e}");
            return ExitCode::FAILURE;
        }
    };
    let curr = match load_report(Path::new(curr_path)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("conformance: {e}");
            return ExitCode::FAILURE;
        }
    };

    let cmp = conformance::compare_to_baseline(&prev, &curr);
    print_comparison(&cmp);

    if cmp.is_clean() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn load_report(path: &Path) -> Result<ConformanceReport, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("cannot parse {} as a ConformanceReport: {e}", path.display()))
}

fn print_comparison(cmp: &BaselineComparison) {
    println!("Drift surface (newly-failing cases): {}", cmp.newly_failing.len());
    for id in &cmp.newly_failing {
        println!("  FAIL  {id}");
    }
    if !cmp.newly_passing.is_empty() {
        println!("Newly passing: {}", cmp.newly_passing.len());
        for id in &cmp.newly_passing {
            println!("  PASS  {id}");
        }
    }
    if !cmp.disappeared.is_empty() {
        println!("Disappeared (clean corpus removals): {}", cmp.disappeared.len());
        for id in &cmp.disappeared {
            println!("  GONE  {id}");
        }
    }
    println!("DIFF count delta  compliance: {:+}  oracle: {:+}", cmp.compliance_diff_delta, cmp.oracle_diff_delta);
    println!("{}", if cmp.is_clean() { "CLEAN: still 1:1" } else { "DRIFT: re-port the newly-failing cases" });
}

/// Walk up from the current directory to find the nearest ancestor containing `libs/render3`.
fn find_repo_root() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join("libs").join("render3").is_dir() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}
