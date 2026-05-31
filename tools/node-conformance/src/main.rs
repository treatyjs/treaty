//! Node conformance harness CLI.
//!
//! Runs the seed corpus (or a directory passed as the first argument) through the Treaty runtime,
//! prints a per-case line and a one-line summary to stderr, and writes the full
//! [`node_conformance::ConformanceReport`] as JSON to stdout so it can be captured / diffed in CI.
//!
//! Usage:
//!   node_conformance [CORPUS_DIR]
//!
//! Exit code is `0` when there are no failures (skips are fine), `1` when any case failed.

use std::path::PathBuf;
use std::process::ExitCode;

use node_conformance::{CaseStatus, run_corpus};

fn main() -> ExitCode {
    // Default to the crate's bundled seed corpus when no directory is given.
    let dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("corpus"));

    let report = match run_corpus(&dir) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("error: could not read corpus at {}: {error}", dir.display());
            return ExitCode::FAILURE;
        }
    };

    for case in &report.cases {
        match (&case.status, &case.reason) {
            (CaseStatus::Pass, _) => {
                eprintln!("PASS {} ({} ms)", case.name, case.duration.as_millis())
            }
            (status, Some(reason)) => {
                eprintln!("{} {} — {reason}", status.as_str().to_uppercase(), case.name)
            }
            (status, None) => eprintln!("{} {}", status.as_str().to_uppercase(), case.name),
        }
    }
    eprintln!("{}", report.summary());

    match serde_json::to_string_pretty(&report) {
        Ok(json) => println!("{json}"),
        Err(error) => {
            eprintln!("error: could not serialize report: {error}");
            return ExitCode::FAILURE;
        }
    }

    if report.failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
