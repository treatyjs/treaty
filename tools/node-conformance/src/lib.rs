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
//!   `skipped`, the computed `pass_rate`, the per-case [`CaseResult`]s, and a per-module
//!   `scoreboard` of [`ModuleScore`]s (one row per top-level corpus subdirectory).
//!
//! # Runner
//!
//! The executor lives in [`mod@runner`] (and is re-exported here). [`run_corpus`] walks a directory
//! of `.js` test files *recursively* (sorted by path for determinism, so the corpus may be
//! organized into per-module subdirectories), evaluates each in a *fresh*
//! [`treaty_runtime::JsRuntime::with_node_compat`] (so no cross-test global leakage), and folds the
//! [`CaseResult`]s into a [`ConformanceReport`] — including a per-module `scoreboard` grouped by
//! top-level corpus subdirectory. [`run_source`] runs a single in-memory test and is the
//! unit-testable core of the walker; its `*_with_manifest` siblings additionally consult a
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

/// One module's slice of a corpus run: the per-module scoreboard row.
///
/// A *module* is the grouping the corpus is organized into. When the corpus is laid out into
/// per-module subdirectories (`corpus/fs/…`, `corpus/path/…`), the module is the top-level
/// subdirectory name; a file living directly in the corpus root is grouped by the `<module>` prefix
/// of its name (see [`crate::module_of`]), so a flat corpus keeps grouping by Node surface.
///
/// The counts and `pass_rate` mirror the whole-report convention: the rate is over *executed*
/// (non-skipped) cases, so a module that is entirely skipped reports `0.0` (it attempted nothing)
/// rather than a misleading `100%`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModuleScore {
    /// The module label (top-level corpus subdirectory, or the name prefix for a root-level file).
    pub module: String,
    /// Cases in this module that passed.
    pub passed: usize,
    /// Cases in this module that failed.
    pub failed: usize,
    /// Cases in this module that were skipped (tagged unsupported / in-file directive).
    pub skipped: usize,
    /// Fraction in `[0.0, 1.0]` of *executed* (non-skipped) cases in this module that passed; `0.0`
    /// when the module executed nothing.
    pub pass_rate: f64,
}

impl ModuleScore {
    /// Cases in this module that were actually executed (`passed + failed`).
    pub fn executed(&self) -> usize {
        self.passed + self.failed
    }

    /// Total cases in this module across all statuses (`passed + failed + skipped`).
    pub fn total(&self) -> usize {
        self.passed + self.failed + self.skipped
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
    /// The per-module scoreboard, one [`ModuleScore`] per module, in deterministic ascending module
    /// order. Built by the corpus walker, which knows each case's corpus subdirectory; a report
    /// built from bare cases via [`ConformanceReport::from_cases`] derives the module from each
    /// case name with [`crate::module_of`].
    pub scoreboard: Vec<ModuleScore>,
}

impl ConformanceReport {
    /// Tally a set of case results into a report, computing the counts and pass-rate.
    ///
    /// Each case is assigned to a module by its name via [`crate::module_of`] (the `<module>`
    /// prefix). To group by an explicit corpus subdirectory instead — what the recursive walker
    /// does — use [`ConformanceReport::from_cases_with_modules`].
    pub fn from_cases(cases: Vec<CaseResult>) -> Self {
        let modules: Vec<String> = cases
            .iter()
            .map(|case| crate::module_of(&case.name).to_owned())
            .collect();
        Self::from_cases_with_modules(cases, modules)
    }

    /// Tally case results into a report, grouping each case under the module label at the same index
    /// in `modules` for the per-module [`scoreboard`](Self::scoreboard).
    ///
    /// The two slices are zipped positionally; `modules` must be the same length as `cases` (the
    /// walker guarantees this). The overall counts/pass-rate are independent of the module labels.
    /// The scoreboard is emitted in ascending module order for a deterministic, diffable report.
    pub fn from_cases_with_modules(cases: Vec<CaseResult>, modules: Vec<String>) -> Self {
        debug_assert_eq!(
            cases.len(),
            modules.len(),
            "every case must carry exactly one module label"
        );

        let mut passed = 0;
        let mut failed = 0;
        let mut skipped = 0;
        // Per-module tallies in ascending module order for determinism.
        let mut buckets: std::collections::BTreeMap<String, (usize, usize, usize)> =
            std::collections::BTreeMap::new();

        for (case, module) in cases.iter().zip(modules.iter()) {
            let bucket = buckets.entry(module.clone()).or_insert((0, 0, 0));
            match case.status {
                CaseStatus::Pass => {
                    passed += 1;
                    bucket.0 += 1;
                }
                CaseStatus::Fail => {
                    failed += 1;
                    bucket.1 += 1;
                }
                CaseStatus::Skip => {
                    skipped += 1;
                    bucket.2 += 1;
                }
            }
        }

        let scoreboard = buckets
            .into_iter()
            .map(|(module, (passed, failed, skipped))| {
                let executed = passed + failed;
                let pass_rate = if executed == 0 {
                    0.0
                } else {
                    passed as f64 / executed as f64
                };
                ModuleScore {
                    module,
                    passed,
                    failed,
                    skipped,
                    pass_rate,
                }
            })
            .collect();

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
            scoreboard,
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

    /// A multi-line, human-readable per-module scoreboard, one row per module plus a `TOTAL` row.
    ///
    /// Each row reports `pass / fail / skip` counts and the executed-only pass-rate for that module,
    /// in ascending module order; the trailing `TOTAL` row carries the whole-run counts and headline
    /// pass-rate. The output is deterministic for a given report — suitable for a terminal or CI log.
    pub fn scoreboard(&self) -> String {
        use std::fmt::Write as _;

        let module_header = "MODULE";
        let total_label = "TOTAL";
        let module_width = self
            .scoreboard
            .iter()
            .map(|s| s.module.len())
            .chain([module_header.len(), total_label.len()])
            .max()
            .unwrap_or(module_header.len());

        let rule_width = module_width + 2 + 5 + 2 + 5 + 2 + 5 + 2 + 7;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "{:<mw$}  {:>5}  {:>5}  {:>5}  {:>7}",
            module_header,
            "PASS",
            "FAIL",
            "SKIP",
            "RATE",
            mw = module_width,
        );
        let _ = writeln!(out, "{}", "-".repeat(rule_width));

        for s in &self.scoreboard {
            let _ = writeln!(
                out,
                "{:<mw$}  {:>5}  {:>5}  {:>5}  {:>6.1}%",
                s.module,
                s.passed,
                s.failed,
                s.skipped,
                s.pass_rate * 100.0,
                mw = module_width,
            );
        }

        let _ = writeln!(out, "{}", "-".repeat(rule_width));
        let _ = writeln!(
            out,
            "{:<mw$}  {:>5}  {:>5}  {:>5}  {:>6.1}%",
            total_label,
            self.passed,
            self.failed,
            self.skipped,
            self.pass_rate * 100.0,
            mw = module_width,
        );

        out
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
        // The scoreboard survives the round-trip too.
        assert_eq!(report.scoreboard, back.scoreboard);
    }

    #[test]
    fn from_cases_groups_scoreboard_by_name_prefix() {
        // Without explicit module labels, the scoreboard buckets by the `<module>` name prefix.
        let report = ConformanceReport::from_cases(vec![
            CaseResult::pass("fs-roundtrip", Duration::from_millis(1)),
            CaseResult::fail("fs-watch", "boom", Duration::from_millis(1)),
            CaseResult::pass("path-basic", Duration::from_millis(1)),
            CaseResult::skip("crypto-hash", "node:crypto not implemented"),
        ]);
        let modules: Vec<&str> = report.scoreboard.iter().map(|s| s.module.as_str()).collect();
        // Ascending module order.
        assert_eq!(modules, vec!["crypto", "fs", "path"]);

        let fs = report.scoreboard.iter().find(|s| s.module == "fs").unwrap();
        assert_eq!((fs.passed, fs.failed, fs.skipped), (1, 1, 0));
        assert_eq!(fs.executed(), 2);
        assert_eq!(fs.total(), 2);
        assert!((fs.pass_rate - 0.5).abs() < f64::EPSILON);

        // An all-skipped module reports 0.0 (nothing executed), never a misleading 100%.
        let crypto = report.scoreboard.iter().find(|s| s.module == "crypto").unwrap();
        assert_eq!(crypto.executed(), 0);
        assert_eq!(crypto.pass_rate, 0.0);
    }

    #[test]
    fn from_cases_with_modules_groups_by_explicit_label() {
        // Explicit labels override the name-prefix grouping: two differently-named cases land in the
        // same module when given the same label (this is what the subdirectory walker does).
        let cases = vec![
            CaseResult::pass("roundtrip", Duration::from_millis(1)),
            CaseResult::fail("stat", "boom", Duration::from_millis(1)),
            CaseResult::pass("join", Duration::from_millis(1)),
        ];
        let modules = vec!["fs".to_owned(), "fs".to_owned(), "path".to_owned()];
        let report = ConformanceReport::from_cases_with_modules(cases, modules);

        let names: Vec<&str> = report.scoreboard.iter().map(|s| s.module.as_str()).collect();
        assert_eq!(names, vec!["fs", "path"]);
        let fs = report.scoreboard.iter().find(|s| s.module == "fs").unwrap();
        assert_eq!((fs.passed, fs.failed, fs.skipped), (1, 1, 0));
        // Whole-report totals are independent of labels.
        assert_eq!((report.passed, report.failed, report.skipped), (2, 1, 0));
    }

    #[test]
    fn scoreboard_string_has_module_rows_and_total() {
        let report = ConformanceReport::from_cases(vec![
            CaseResult::pass("fs-roundtrip", Duration::from_millis(1)),
            CaseResult::pass("fs-stat", Duration::from_millis(1)),
            CaseResult::fail("fs-watch", "boom", Duration::from_millis(1)),
            CaseResult::pass("path-basic", Duration::from_millis(1)),
            CaseResult::skip("crypto-hash", "node:crypto not implemented"),
        ]);
        let board = report.scoreboard();
        assert!(board.contains("MODULE"));
        assert!(board.contains("PASS"));
        assert!(board.contains("RATE"));
        assert!(board.contains("crypto"));
        assert!(board.contains("fs"));
        assert!(board.contains("path"));
        let total_line = board
            .lines()
            .find(|l| l.starts_with("TOTAL"))
            .expect("a TOTAL row");
        // 3 passed of 4 executed = 75.0% on the TOTAL row.
        assert!(total_line.contains("75.0%"), "TOTAL line was: {total_line}");
    }

    #[test]
    fn scoreboard_string_is_well_formed_for_empty_report() {
        // No cases: still a header + a TOTAL row, with a 0.0% rate (nothing executed).
        let report = ConformanceReport::from_cases(vec![]);
        let board = report.scoreboard();
        assert!(board.contains("MODULE"));
        let total_line = board.lines().find(|l| l.starts_with("TOTAL")).unwrap();
        assert!(total_line.contains("0.0%"));
    }
}
