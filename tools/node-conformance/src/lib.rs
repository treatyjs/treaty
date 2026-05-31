//! Node.js compatibility conformance harness for the Treaty runtime.
//!
//! This crate runs Node `test/parallel`-style `.js` tests through
//! [`treaty_runtime::JsRuntime::with_node_compat`] and produces a deterministic, reproducible
//! pass-rate report — the same kind of Node-compat scoreboard Bun and Deno publish, but with no
//! AI/heuristics in the loop: a test either runs clean (Pass), throws (Fail), or is explicitly
//! tagged unsupported (Skip).
//!
//! # What "conformance" means here
//!
//! The Treaty runtime ([`treaty_runtime`]) currently implements a subset of Node: `require` plus
//! the `node:` builtins `fs`, `fs/promises`, `path`, `process`, `buffer`, `os`, `util`, `events`,
//! `console`, `timers`, `url`, text encoding, and `fetch` — all on top of an event loop. The corpus
//! in `corpus/` therefore seeds tests that exercise exactly those surfaces. Tests targeting
//! still-unimplemented APIs are kept in the corpus but tagged SKIP so the report tracks intent
//! ("we know about this, it is not done yet") rather than silently omitting them.
//!
//! # Report API
//!
//! - [`CaseResult`] — one test file's outcome: its `name`, a [`CaseStatus`] (`Pass` / `Fail` /
//!   `Skip`), an optional `reason` (the failure message, or the skip justification), and the wall
//!   clock `duration`.
//! - [`ConformanceReport`] — the aggregate over a corpus run: `total`, `passed`, `failed`,
//!   `skipped`, the computed `pass_rate`, and the per-case [`CaseResult`]s.
//!
//! # Runner
//!
//! The executor lives in [`mod@runner`] (and is re-exported here). [`run_corpus`] walks a directory
//! of `.js` test files (sorted for determinism), evaluates each in a *fresh*
//! [`treaty_runtime::JsRuntime::with_node_compat`] (so no cross-test global leakage), and folds the
//! [`CaseResult`]s into a [`ConformanceReport`]. [`run_source`] runs a single in-memory test and is
//! the unit-testable core of the walker; its `*_with_manifest` siblings additionally consult a
//! known-unsupported [`Manifest`].
//!
//! # How a corpus test is tagged SKIP
//!
//! A test opts out of execution either with a single-line, machine-readable directive anywhere in
//! the file — by convention the first line —
//!
//! ```js
//! // CONFORMANCE: skip — child_process is not implemented in the Treaty runtime yet
//! ```
//!
//! or by being listed (by case name) in a [`Manifest`], which lets a vendored test be marked
//! unsupported without editing the file. The marker is the literal token [`SKIP_MARKER`]
//! (`CONFORMANCE: skip`); everything after the first `—`/`-`/`:` separator is captured as the
//! human-readable reason. A skipped test is never evaluated, so an unsupported API can sit in the
//! corpus without throwing. See [`detect_skip`] for the exact parse.

use std::time::Duration;

use serde::{Deserialize, Serialize};

mod runner;
pub mod manifest;

/// The in-test harness shim (the `assert.*` + `test`/`describe` prelude every curated corpus file
/// embeds). Exposed so the corpus-integrity test can assert the embedded copies never drift from the
/// canonical source, and so downstream tooling can read the canonical block.
pub mod harness_shim;

pub use runner::{
    detect_skip, run_corpus, run_corpus_with_manifest, run_source, run_source_with_manifest,
};

pub use manifest::{
    ManifestError, ModuleStats, UnsupportedEntry, UnsupportedManifest, module_of, module_stats,
    render_table, write_report_json,
};

/// The literal directive token that tags a corpus test as unsupported. A line containing this
/// substring causes the runner to record a [`CaseStatus::Skip`] without evaluating the file.
pub const SKIP_MARKER: &str = "CONFORMANCE: skip";

/// The classification of a single conformance test case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaseStatus {
    /// The test evaluated to completion without throwing.
    Pass,
    /// The test threw (or failed to parse / convert) while executing.
    Fail,
    /// The test was tagged unsupported and deliberately not run.
    Skip,
}

impl CaseStatus {
    /// The lowercase wire/display name of this status (`"pass"`, `"fail"`, `"skip"`).
    pub fn as_str(self) -> &'static str {
        match self {
            CaseStatus::Pass => "pass",
            CaseStatus::Fail => "fail",
            CaseStatus::Skip => "skip",
        }
    }
}

/// The outcome of running one test file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaseResult {
    /// The test's identifying name — the file stem for a corpus file (e.g. `fs-roundtrip`), or a
    /// caller-supplied label for an in-memory run.
    pub name: String,
    /// How the test was classified.
    pub status: CaseStatus,
    /// For a [`CaseStatus::Fail`], the thrown/parse message. For a [`CaseStatus::Skip`], the
    /// human-readable justification parsed from the skip directive. `None` for a clean pass.
    pub reason: Option<String>,
    /// Wall-clock time spent on this case. A skipped case is `Duration::ZERO` (it never ran).
    pub duration: Duration,
}

impl CaseResult {
    /// A passing case: no reason, with the measured duration.
    pub fn pass(name: impl Into<String>, duration: Duration) -> Self {
        Self {
            name: name.into(),
            status: CaseStatus::Pass,
            reason: None,
            duration,
        }
    }

    /// A failing case carrying the failure message and the measured duration.
    pub fn fail(name: impl Into<String>, reason: impl Into<String>, duration: Duration) -> Self {
        Self {
            name: name.into(),
            status: CaseStatus::Fail,
            reason: Some(reason.into()),
            duration,
        }
    }

    /// A skipped case carrying the unsupported-reason. It is recorded as never having run, so its
    /// duration is zero.
    pub fn skip(name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: CaseStatus::Skip,
            reason: Some(reason.into()),
            duration: Duration::ZERO,
        }
    }
}

/// The aggregate result of a corpus run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConformanceReport {
    /// The number of test cases considered (`passed + failed + skipped`).
    pub total: usize,
    /// How many cases passed.
    pub passed: usize,
    /// How many cases failed.
    pub failed: usize,
    /// How many cases were skipped (tagged unsupported).
    pub skipped: usize,
    /// The fraction in `[0.0, 1.0]` of *executed* (non-skipped) cases that passed. Skipped cases
    /// are excluded from the denominator, since a skip is a known gap rather than a regression; a
    /// run with no executed cases reports `0.0`. This mirrors how Bun/Deno headline their
    /// Node-compat percentage against the tests they actually attempt.
    pub pass_rate: f64,
    /// The per-case outcomes, in the deterministic order they were run.
    pub cases: Vec<CaseResult>,
}

impl ConformanceReport {
    /// Tally a set of case results into a report, computing the counts and pass-rate.
    pub fn from_cases(cases: Vec<CaseResult>) -> Self {
        let mut passed = 0;
        let mut failed = 0;
        let mut skipped = 0;
        for case in &cases {
            match case.status {
                CaseStatus::Pass => passed += 1,
                CaseStatus::Fail => failed += 1,
                CaseStatus::Skip => skipped += 1,
            }
        }
        let executed = passed + failed;
        let pass_rate = if executed == 0 {
            0.0
        } else {
            passed as f64 / executed as f64
        };
        Self {
            total: cases.len(),
            passed,
            failed,
            skipped,
            pass_rate,
            cases,
        }
    }

    /// A one-line, human-readable summary, e.g. `7 passed, 1 failed, 2 skipped (87.5% of 8 run)`.
    pub fn summary(&self) -> String {
        let executed = self.passed + self.failed;
        format!(
            "{} passed, {} failed, {} skipped ({:.1}% of {} run)",
            self.passed,
            self.failed,
            self.skipped,
            self.pass_rate * 100.0,
            executed,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_tallies_and_computes_pass_rate_over_executed_cases() {
        let cases = vec![
            CaseResult::pass("a", Duration::from_millis(1)),
            CaseResult::pass("b", Duration::from_millis(1)),
            CaseResult::pass("c", Duration::from_millis(1)),
            CaseResult::fail("d", "boom", Duration::from_millis(1)),
            CaseResult::skip("e", "unsupported"),
        ];
        let report = ConformanceReport::from_cases(cases);
        assert_eq!(report.total, 5);
        assert_eq!(report.passed, 3);
        assert_eq!(report.failed, 1);
        assert_eq!(report.skipped, 1);
        // 3 of 4 *executed* (skip excluded from the denominator).
        assert!((report.pass_rate - 0.75).abs() < f64::EPSILON);
    }

    #[test]
    fn report_with_no_executed_cases_is_zero_rate() {
        let report = ConformanceReport::from_cases(vec![CaseResult::skip("only", "unsupported")]);
        assert_eq!(report.pass_rate, 0.0);
        assert_eq!(report.total, 1);
    }

    #[test]
    fn report_round_trips_through_json() {
        let report = ConformanceReport::from_cases(vec![
            CaseResult::pass("a", Duration::from_millis(2)),
            CaseResult::fail("b", "runtime: boom", Duration::from_millis(3)),
            CaseResult::skip("c", "net unimplemented"),
        ]);
        let json = serde_json::to_string(&report).unwrap();
        let back: ConformanceReport = serde_json::from_str(&json).unwrap();
        assert_eq!(report, back);
    }
}
