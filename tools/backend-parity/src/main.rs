//! `backend-parity` CLI — the entry point for the deterministic (NO-AI) backend-parity harness.
//!
//! Subcommands (each exits non-zero on failure/drift, mirroring `render3-sync`):
//!   * `parity`        — compile every corpus fixture through every ENABLED backend and assert
//!                       pairwise byte-equality. Prints a per-fixture `PARITY <id> OK|DIFF` report
//!                       (JSON with `--out`). Exits 1 on any diff. With only oxc enabled this is
//!                       trivially all-OK (Phase 1, SWC-BACKEND-PLAN.md §5).
//!   * `baseline`      — record the reference (oxc) output per fixture to `baseline.json` (default
//!                       `tools/backend-parity/baseline.json`) — the committed tripwire.
//!   * `drift`         — re-run oxc over the corpus and diff against the recorded baseline. Prints a
//!                       `DriftReport` (JSON). Exits 1 on any drift (a future-change detector).
//!   * `migrate-plan`  — emit the structured oxc→swc port-task seed (JSON) describing what the swc
//!                       backend must implement to match. Always exits 0 (it is a plan, not a gate).
//!
//! See `migration/SWC-BACKEND-PLAN.md` §4.

use std::path::PathBuf;
use std::process::ExitCode;

use backend_parity::{
    build_migrate_plan, diff_baseline, record_baseline, run_parity, Baseline,
};

/// The default baseline artifact path, relative to the located crate dir.
const DEFAULT_BASELINE: &str = "tools/backend-parity/baseline.json";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("parity") => cmd_parity(&args[1..]),
        Some("baseline") => cmd_baseline(&args[1..]),
        Some("drift") => cmd_drift(&args[1..]),
        Some("migrate-plan") => cmd_migrate_plan(&args[1..]),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

const USAGE: &str = "backend-parity — assert byte-identical Ivy across Treaty's oxc/swc backends (NO AI)\n\
     usage:\n  \
     backend-parity parity        [--out F]               compile the corpus through every enabled backend; assert byte-equal (exit 1 on diff)\n  \
     backend-parity baseline      [--out F]               record the reference (oxc) output per fixture -> baseline.json\n  \
     backend-parity drift         [--baseline F] [--out F] diff current oxc output vs the baseline (exit 1 on drift)\n  \
     backend-parity migrate-plan  [--out F]               emit the oxc->swc port-task seed (JSON)\n\
     \n  \
     Backends: oxc is the default + reference. Build with `--features swc` once the SWC backend lands\n  \
     to diff oxc vs swc (see migration/SWC-BACKEND-PLAN.md).";

// ------------------------------------------------------------------------------------------------
// Shared flag parsing.
// ------------------------------------------------------------------------------------------------

#[derive(Debug, Default, PartialEq, Eq)]
struct Opts {
    baseline: Option<PathBuf>,
    out: Option<PathBuf>,
}

/// Parse `--baseline` / `--out`. Unknown flags error so a typo never silently no-ops.
fn parse_opts(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts::default();
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        let val = || args.get(i + 1).cloned().ok_or_else(|| format!("{flag} requires a value"));
        match flag {
            "--baseline" => o.baseline = Some(PathBuf::from(val()?)),
            "--out" => o.out = Some(PathBuf::from(val()?)),
            other => return Err(format!("unknown flag {other}")),
        }
        i += 2;
    }
    Ok(o)
}

/// Resolve the repo root: the nearest ancestor of CWD that contains `tools/backend-parity` and
/// `libs/treaty-ivy`. Used to anchor the default baseline path.
fn resolve_repo_root() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join("tools/backend-parity").is_dir() && dir.join("libs/treaty-ivy").is_dir() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Write `json` to `out` (with a trailing newline), reporting on failure.
fn write_json(out: &PathBuf, json: &str) -> Result<(), ()> {
    if let Err(e) = std::fs::write(out, format!("{json}\n")) {
        eprintln!("backend-parity: writing {}: {e}", out.display());
        return Err(());
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------------
// `parity`
// ------------------------------------------------------------------------------------------------

fn cmd_parity(args: &[String]) -> ExitCode {
    let opts = match parse_opts(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("backend-parity parity: {e}");
            return ExitCode::FAILURE;
        }
    };

    let report = run_parity();

    eprintln!("backend-parity: backends = [{}]", report.backends.join(", "));
    for fx in &report.fixtures {
        if fx.parity.is_ok() {
            println!("PARITY {} OK", fx.id);
        } else if let backend_parity::FixtureParity::Diff { detail, .. } = &fx.parity {
            println!("PARITY {} DIFF — {detail}", fx.id);
        }
    }

    let json = match serde_json::to_string_pretty(&report) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("backend-parity parity: serialize failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(out) = &opts.out {
        if write_json(out, &json).is_err() {
            return ExitCode::FAILURE;
        }
    }

    let diffs = report.diffs();
    if diffs.is_empty() {
        eprintln!(
            "backend-parity: PARITY — all {} fixture(s) byte-identical across [{}]",
            report.fixtures.len(),
            report.backends.join(", ")
        );
        ExitCode::SUCCESS
    } else {
        eprintln!("backend-parity: DIFF — {} fixture(s) diverged:", diffs.len());
        for d in diffs {
            eprintln!("  {}", d.id);
        }
        ExitCode::FAILURE
    }
}

// ------------------------------------------------------------------------------------------------
// `baseline`
// ------------------------------------------------------------------------------------------------

fn cmd_baseline(args: &[String]) -> ExitCode {
    let opts = match parse_opts(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("backend-parity baseline: {e}");
            return ExitCode::FAILURE;
        }
    };

    let baseline = record_baseline();
    let json = match serde_json::to_string_pretty(&baseline) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("backend-parity baseline: serialize failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    let out = opts.out.unwrap_or_else(|| {
        resolve_repo_root()
            .map(|r| r.join(DEFAULT_BASELINE))
            .unwrap_or_else(|| PathBuf::from(DEFAULT_BASELINE))
    });
    if write_json(&out, &json).is_err() {
        return ExitCode::FAILURE;
    }
    eprintln!(
        "backend-parity: recorded baseline ({} fixtures, backend `{}`) -> {}",
        baseline.outputs.len(),
        baseline.backend,
        out.display()
    );
    ExitCode::SUCCESS
}

// ------------------------------------------------------------------------------------------------
// `drift`
// ------------------------------------------------------------------------------------------------

fn cmd_drift(args: &[String]) -> ExitCode {
    let opts = match parse_opts(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("backend-parity drift: {e}");
            return ExitCode::FAILURE;
        }
    };

    let baseline_path = opts.baseline.unwrap_or_else(|| {
        resolve_repo_root()
            .map(|r| r.join(DEFAULT_BASELINE))
            .unwrap_or_else(|| PathBuf::from(DEFAULT_BASELINE))
    });
    let baseline: Baseline = match std::fs::read_to_string(&baseline_path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("backend-parity drift: parsing {}: {e}", baseline_path.display());
                return ExitCode::FAILURE;
            }
        },
        Err(e) => {
            eprintln!(
                "backend-parity drift: cannot read baseline {} ({e}); run `baseline` first",
                baseline_path.display()
            );
            return ExitCode::FAILURE;
        }
    };

    let report = diff_baseline(&baseline);
    let json = match serde_json::to_string_pretty(&report) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("backend-parity drift: serialize failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("{json}");
    if let Some(out) = &opts.out {
        if write_json(out, &json).is_err() {
            return ExitCode::FAILURE;
        }
    }

    if report.is_clean() {
        eprintln!("backend-parity: CLEAN — oxc output still matches the baseline");
        ExitCode::SUCCESS
    } else {
        eprintln!("backend-parity: DRIFT — {} fixture(s) changed:", report.entries.len());
        for e in &report.entries {
            eprintln!("  {}", e.id);
        }
        ExitCode::FAILURE
    }
}

// ------------------------------------------------------------------------------------------------
// `migrate-plan`
// ------------------------------------------------------------------------------------------------

fn cmd_migrate_plan(args: &[String]) -> ExitCode {
    let opts = match parse_opts(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("backend-parity migrate-plan: {e}");
            return ExitCode::FAILURE;
        }
    };

    let plan = build_migrate_plan();
    let json = match serde_json::to_string_pretty(&plan) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("backend-parity migrate-plan: serialize failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("{json}");
    if let Some(out) = &opts.out {
        if write_json(out, &json).is_err() {
            return ExitCode::FAILURE;
        }
    }
    eprintln!("backend-parity: emitted {} port-task(s) (see SWC-BACKEND-PLAN.md §2)", plan.tasks.len());
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_opts_reads_flags() {
        let args: Vec<String> = ["--baseline", "b.json", "--out", "o.json"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let o = parse_opts(&args).unwrap();
        assert_eq!(o.baseline, Some(PathBuf::from("b.json")));
        assert_eq!(o.out, Some(PathBuf::from("o.json")));
    }

    #[test]
    fn parse_opts_rejects_unknown_flag() {
        let err = parse_opts(&["--nope".to_string()]).unwrap_err();
        assert!(err.contains("unknown flag --nope"), "{err}");
    }

    #[test]
    fn parse_opts_rejects_missing_value() {
        let err = parse_opts(&["--out".to_string()]).unwrap_err();
        assert!(err.contains("requires a value"), "{err}");
    }
}
