//! Pillar 2 — the **conformance gate**.
//!
//! This module is a deterministic (NO-AI) wrapper around the two EXISTING JS harnesses that live
//! under `libs/render3` and which are the source of truth for "is the Rust/oxc port still 1:1 with
//! Angular":
//!
//!   * `libs/render3/compliance/run-compliance.mjs` — runs Treaty's compiler against Angular's own
//!     `compiler-cli` compliance corpus and prints headline `PASS`/`DIFF`/`SKIPPED` counts plus, in
//!     `--verbose` mode, a `DIFF <id>` line per diverging case.
//!   * `libs/render3/parity/parity.mjs` — the *oracle* diff: lowers a fixed fixture corpus through
//!     both `@angular/compiler` and the Rust addon and prints a per-fixture `RESULT: PASS|DIFF`.
//!
//! The wrapper:
//!   1. invokes each harness as a `node` child process (READ-ONLY — it never runs `cargo` for
//!      render3 and never edits `libs/render3`),
//!   2. parses their stdout into a structured [`ConformanceReport`] (compliance pass/total, oracle
//!      pass/diff, plus the per-case PASS/DIFF identifiers each harness exposes),
//!   3. and via [`compare_to_baseline`] computes the **drift surface**: the set of cases that pass
//!      in a previous baseline report but FAIL (or vanish) in the current one. That newly-failing
//!      set is the deterministic "we are no longer 1:1 here" signal pillar 2 contributes.
//!
//! The output PARSER (the part that turns harness stdout into structured numbers + case lists) is
//! the unit-tested core; running `node` is a thin shell around it.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

/// Which harness produced a result (used to namespace case IDs in the drift surface so a
/// compliance case and an oracle fixture that happen to share a name never collide).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Harness {
    /// `libs/render3/compliance/run-compliance.mjs`.
    Compliance,
    /// `libs/render3/parity/parity.mjs`.
    Oracle,
}

impl Harness {
    /// Stable short prefix used to namespace case IDs in the drift surface.
    pub fn prefix(self) -> &'static str {
        match self {
            Harness::Compliance => "compliance",
            Harness::Oracle => "oracle",
        }
    }
}

/// The parsed result of the compliance harness (`run-compliance.mjs`).
///
/// Counts are taken verbatim from the harness's headline block. `pass_ids` / `diff_ids` are the
/// per-case identifiers the harness exposes (pass IDs from the report listing, diff IDs from the
/// `--verbose` `DIFF <id>` lines); either may be empty if the harness was not asked to print them,
/// in which case drift detection falls back to the aggregate counts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComplianceResult {
    /// Total compliance cases enumerated from Angular's corpus.
    pub total: u32,
    /// Cases the Rust source front-end could actually compile + match (the runnable subset).
    pub runnable: u32,
    /// Runnable cases whose emit matched the Angular golden.
    pub pass: u32,
    /// Runnable cases whose emit diverged from the golden.
    pub diff: u32,
    /// Cases skipped as un-runnable (front-end declined the shape, no golden, multi-file, ...).
    pub skipped: u32,
    /// Identifiers of passing cases (`<category>/<description>`), when the harness listed them.
    pub pass_ids: Vec<String>,
    /// Identifiers of diverging cases (`<category>/<description>`), from `--verbose` output.
    pub diff_ids: Vec<String>,
}

/// The parsed result of the oracle parity harness (`parity.mjs`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OracleResult {
    /// Fixtures whose normalized Rust emit matched the `@angular/compiler` oracle emit.
    pub pass: u32,
    /// Fixtures that diverged (includes oracle/Rust errors, which the harness counts as DIFF).
    pub diff: u32,
    /// Fixtures the harness could only run on the oracle side (Rust addon unavailable).
    pub oracle_only: u32,
    /// Total fixtures in the parity corpus.
    pub total: u32,
    /// Identifiers of passing fixtures.
    pub pass_ids: Vec<String>,
    /// Identifiers of diverging fixtures.
    pub diff_ids: Vec<String>,
}

/// The combined, serializable conformance artifact — the structured form of one run of both
/// harnesses. This is what [`compare_to_baseline`] diffs across Angular bumps.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConformanceReport {
    /// Parsed compliance numbers, if the compliance harness ran.
    pub compliance: Option<ComplianceResult>,
    /// Parsed oracle numbers, if the parity harness ran.
    pub oracle: Option<OracleResult>,
}

impl ConformanceReport {
    /// The full set of namespaced case IDs that PASS in this report, across both harnesses.
    /// IDs are prefixed with the harness name (`compliance/...`, `oracle/...`) so the two case
    /// spaces never collide in the drift surface.
    pub fn passing_cases(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(c) = &self.compliance {
            for id in &c.pass_ids {
                out.push(namespaced(Harness::Compliance, id));
            }
        }
        if let Some(o) = &self.oracle {
            for id in &o.pass_ids {
                out.push(namespaced(Harness::Oracle, id));
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// The full set of namespaced case IDs that FAIL (DIFF) in this report, across both harnesses.
    pub fn failing_cases(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(c) = &self.compliance {
            for id in &c.diff_ids {
                out.push(namespaced(Harness::Compliance, id));
            }
        }
        if let Some(o) = &self.oracle {
            for id in &o.diff_ids {
                out.push(namespaced(Harness::Oracle, id));
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// True when both harnesses (whichever ran) report zero DIFF — i.e. fully 1:1 on the case
    /// granularity each exposes. A harness that did not run does not count against cleanliness.
    pub fn is_clean(&self) -> bool {
        let compliance_clean = self.compliance.as_ref().map(|c| c.diff == 0).unwrap_or(true);
        let oracle_clean = self.oracle.as_ref().map(|o| o.diff == 0).unwrap_or(true);
        compliance_clean && oracle_clean
    }
}

/// Namespace a per-harness case ID with its harness prefix.
fn namespaced(h: Harness, id: &str) -> String {
    format!("{}/{}", h.prefix(), id)
}

/// The deterministic **drift surface**: the difference between a previous (baseline) conformance
/// run and a current one. Newly-failing cases are the load-bearing signal — they bound exactly the
/// re-porting that an Angular bump made necessary.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineComparison {
    /// Namespaced IDs that PASSED in the baseline but now FAIL (DIFF) or have vanished while a
    /// DIFF count grew — the drift surface. Sorted, deduplicated.
    pub newly_failing: Vec<String>,
    /// Namespaced IDs that FAILED in the baseline but now PASS — progress, reported for context.
    pub newly_passing: Vec<String>,
    /// Cases present in the baseline's PASS set that are no longer present at all in the current
    /// report (neither pass nor diff) — e.g. a corpus case Angular removed. Reported separately so
    /// a vanished case is not silently conflated with a regression.
    pub disappeared: Vec<String>,
    /// Net change in the compliance DIFF count (current − baseline); positive = regressed.
    pub compliance_diff_delta: i64,
    /// Net change in the oracle DIFF count (current − baseline); positive = regressed.
    pub oracle_diff_delta: i64,
}

impl BaselineComparison {
    /// True when nothing newly failed and no DIFF count grew — the expected pillar-3 result
    /// against the current pinned Angular.
    pub fn is_clean(&self) -> bool {
        self.newly_failing.is_empty()
            && self.compliance_diff_delta <= 0
            && self.oracle_diff_delta <= 0
    }
}

/// Compute the drift surface between a baseline (`prev`) and current (`curr`) conformance report.
///
/// A case is **newly failing** when it passed in `prev` and now fails — established three ways, in
/// priority order, so the signal is robust whether or not the harness emitted per-case IDs:
///   1. it is in `prev`'s PASS set and in `curr`'s FAIL set (the precise case-level signal), or
///   2. it is in `prev`'s PASS set and `curr` reports it neither pass nor fail (vanished while the
///      relevant DIFF count rose — treated as a regression, not a clean removal), distinguished
///      from a genuine corpus removal (see [`BaselineComparison::disappeared`]).
///
/// When neither harness exposed per-case IDs, the per-case sets are empty and only the aggregate
/// DIFF deltas carry the signal — still deterministic, just coarser.
pub fn compare_to_baseline(prev: &ConformanceReport, curr: &ConformanceReport) -> BaselineComparison {
    use std::collections::BTreeSet;

    let prev_pass: BTreeSet<String> = prev.passing_cases().into_iter().collect();
    let prev_fail: BTreeSet<String> = prev.failing_cases().into_iter().collect();
    let curr_pass: BTreeSet<String> = curr.passing_cases().into_iter().collect();
    let curr_fail: BTreeSet<String> = curr.failing_cases().into_iter().collect();

    // A case "exists" in curr if it appears in either the pass or fail set.
    let curr_present: BTreeSet<&String> = curr_pass.iter().chain(curr_fail.iter()).collect();

    let mut newly_failing = Vec::new();
    let mut disappeared = Vec::new();
    for id in &prev_pass {
        if curr_fail.contains(id) {
            // Passed before, explicitly fails now.
            newly_failing.push(id.clone());
        } else if !curr_present.contains(id) {
            // Passed before, no longer reported at all. Whether that is a regression or a clean
            // corpus removal cannot be decided from IDs alone; classify by the aggregate DIFF
            // delta for the owning harness — if that harness's DIFF rose, treat as drift.
            if harness_diff_rose(id, prev, curr) {
                newly_failing.push(id.clone());
            } else {
                disappeared.push(id.clone());
            }
        }
    }

    let mut newly_passing = Vec::new();
    for id in &curr_pass {
        if prev_fail.contains(id) {
            newly_passing.push(id.clone());
        }
    }

    newly_failing.sort();
    newly_failing.dedup();
    newly_passing.sort();
    newly_passing.dedup();
    disappeared.sort();
    disappeared.dedup();

    BaselineComparison {
        newly_failing,
        newly_passing,
        disappeared,
        compliance_diff_delta: diff_delta(prev.compliance.as_ref().map(|c| c.diff), curr.compliance.as_ref().map(|c| c.diff)),
        oracle_diff_delta: diff_delta(prev.oracle.as_ref().map(|o| o.diff), curr.oracle.as_ref().map(|o| o.diff)),
    }
}

/// Did the harness owning `namespaced_id` see its DIFF count rise from `prev` to `curr`?
fn harness_diff_rose(namespaced_id: &str, prev: &ConformanceReport, curr: &ConformanceReport) -> bool {
    if namespaced_id.starts_with("compliance/") {
        diff_delta(prev.compliance.as_ref().map(|c| c.diff), curr.compliance.as_ref().map(|c| c.diff)) > 0
    } else if namespaced_id.starts_with("oracle/") {
        diff_delta(prev.oracle.as_ref().map(|o| o.diff), curr.oracle.as_ref().map(|o| o.diff)) > 0
    } else {
        false
    }
}

/// `curr − prev` for an optional count, treating a missing side as 0.
fn diff_delta(prev: Option<u32>, curr: Option<u32>) -> i64 {
    i64::from(curr.unwrap_or(0)) - i64::from(prev.unwrap_or(0))
}

// ---------------------------------------------------------------------------
// Output parsers. These are the unit-tested core — pure `&str -> struct` functions with no I/O.
// ---------------------------------------------------------------------------

/// Parse the stdout of `run-compliance.mjs` into a [`ComplianceResult`].
///
/// The harness prints a fixed headline block:
/// ```text
/// Total compliance cases     : 642
/// Compiled (runnable)        : 98
///   PASS  : 50
///   DIFF  : 48
/// Skipped (un-runnable)      : 544
/// ```
/// and, in `--verbose` mode, a `DIFF <category>/<description>` line per diverging case. The
/// markdown report (when `--report` is passed) additionally lists passing cases as `- <id>`
/// bullets under a "Passing cases" heading; this parser also harvests those when present so a
/// captured report file feeds the same drift detection.
pub fn parse_compliance(stdout: &str) -> ComplianceResult {
    let mut r = ComplianceResult::default();
    let mut in_passing_section = false;

    for raw in stdout.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim();

        // Headline counts. Match on the stable label prefix; take the integer after the colon.
        if let Some(n) = label_count(trimmed, "Total compliance cases") {
            r.total = n;
        } else if let Some(n) = label_count(trimmed, "Compiled (runnable)") {
            r.runnable = n;
        } else if let Some(n) = label_count(trimmed, "Skipped (un-runnable)") {
            r.skipped = n;
        } else if let Some(n) = label_count(trimmed, "PASS") {
            // The headline `  PASS  : 50` line. Guard against the markdown table row `| PASS | 50 |`
            // by also accepting the table form below; here we only take the colon form.
            r.pass = n;
        } else if let Some(n) = label_count(trimmed, "DIFF") {
            r.diff = n;
        }

        // Markdown-report table rows: `| PASS | 50 |`, `| DIFF | 48 |`, etc. These let a captured
        // COMPLIANCE-REPORT.md feed the parser too.
        if let Some((label, n)) = table_row_count(trimmed) {
            match label.as_str() {
                "Total compliance cases" => r.total = n,
                "Compiled (runnable)" => r.runnable = n,
                "PASS" => r.pass = n,
                "DIFF" => r.diff = n,
                "Skipped (un-runnable)" => r.skipped = n,
                _ => {}
            }
        }

        // `--verbose` per-case diff lines: `DIFF <category>/<description>` (the next line is the
        // category detail). Distinguished from the headline `DIFF  : 48` count line — which begins
        // with a colon after the label — by requiring the remainder to be a real `cat/desc` ID.
        if let Some(rest) = trimmed.strip_prefix("DIFF ") {
            let id = rest.trim();
            if id.contains('/') && is_case_id(id) {
                push_unique(&mut r.diff_ids, id.to_string());
            }
        }

        // Markdown "Passing cases" bullet list harvesting.
        if trimmed.starts_with("## Passing cases") {
            in_passing_section = true;
            continue;
        }
        if in_passing_section {
            if trimmed.starts_with("## ") {
                in_passing_section = false;
            } else if let Some(id) = trimmed.strip_prefix("- ") {
                let id = id.trim();
                if is_case_id(id) {
                    push_unique(&mut r.pass_ids, id.to_string());
                }
            }
        }

        // Markdown "Sample diverging cases" bullets are nested under `- **\`cat\`** (n):` headers;
        // the case IDs are the deeper `  - <id>` bullets. Harvest those that look like case IDs.
        if line.starts_with("  - ")
            && let Some(id) = trimmed.strip_prefix("- ").map(str::trim).filter(|id| is_case_id(id))
        {
            push_unique(&mut r.diff_ids, id.to_string());
        }
    }

    r
}

/// Parse the stdout of `parity.mjs` into an [`OracleResult`].
///
/// The harness prints, per fixture:
/// ```text
/// Fixture: interpolation
///   template: <div>{{name}}</div>
///   RESULT: PASS
/// ```
/// (or `RESULT: DIFF`, `RESULT: ORACLE-ONLY (...)`, or an `ORACLE ERROR` / `RUST ERROR` line which
/// the harness counts as a DIFF). A trailing summary line `Summary: N PASS, M DIFF, K ORACLE-ONLY
/// (of T)` carries the authoritative totals; this parser uses it when present and otherwise derives
/// the totals from the per-fixture results.
pub fn parse_oracle(stdout: &str) -> OracleResult {
    let mut r = OracleResult::default();
    let mut current: Option<String> = None;
    let mut summary_seen = false;

    for raw in stdout.lines() {
        let line = raw.trim();

        if let Some(rest) = line.strip_prefix("Fixture:") {
            current = Some(rest.trim().to_string());
            continue;
        }

        // Per-fixture outcome. The harness emits exactly one of these per fixture.
        if let Some(rest) = line.strip_prefix("RESULT:") {
            let outcome = rest.trim();
            if let Some(id) = current.take() {
                if outcome.starts_with("PASS") {
                    push_unique(&mut r.pass_ids, id);
                } else if outcome.starts_with("DIFF") {
                    push_unique(&mut r.diff_ids, id);
                }
                // ORACLE-ONLY: not a pass and not a divergence — left out of both ID sets.
            }
            continue;
        }
        // An ORACLE/RUST error aborts the fixture before a RESULT line; the harness counts it as a
        // DIFF, so mirror that here.
        if line.starts_with("ORACLE ERROR") || line.starts_with("RUST ERROR") {
            if let Some(id) = current.take() {
                push_unique(&mut r.diff_ids, id);
            }
            continue;
        }

        // Authoritative summary line.
        if let Some(rest) = line.strip_prefix("Summary:")
            && let Some(s) = parse_oracle_summary(rest)
        {
            r.pass = s.0;
            r.diff = s.1;
            r.oracle_only = s.2;
            r.total = s.3;
            summary_seen = true;
        }
    }

    // Fall back to per-fixture-derived counts if the summary line was absent.
    if !summary_seen {
        r.pass = r.pass_ids.len() as u32;
        r.diff = r.diff_ids.len() as u32;
        // oracle_only and total cannot be derived without the summary; leave oracle_only at 0 and
        // set total to what we observed.
        r.total = r.pass + r.diff + r.oracle_only;
    }

    r
}

/// Parse the tail of a `Summary:` line into `(pass, diff, oracle_only, total)`.
/// Expected form: `N PASS, M DIFF, K ORACLE-ONLY (of T)`.
fn parse_oracle_summary(rest: &str) -> Option<(u32, u32, u32, u32)> {
    let pass = leading_uint_before(rest, "PASS")?;
    let diff = leading_uint_before(rest, "DIFF")?;
    let oracle_only = leading_uint_before(rest, "ORACLE-ONLY").unwrap_or(0);
    // `(of T)` total.
    let total = rest
        .split("(of")
        .nth(1)
        .and_then(first_uint)
        .unwrap_or(pass + diff + oracle_only);
    Some((pass, diff, oracle_only, total))
}

/// Find the integer immediately preceding the first occurrence of `keyword` in `s`.
/// e.g. `leading_uint_before("10 PASS, 5 DIFF", "DIFF") == Some(5)`.
fn leading_uint_before(s: &str, keyword: &str) -> Option<u32> {
    let at = s.find(keyword)?;
    let before = s[..at].trim_end();
    // Take the trailing run of ASCII digits.
    let digits: String = before
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    digits.parse().ok()
}

/// First unsigned integer appearing anywhere in `s`.
fn first_uint(s: &str) -> Option<u32> {
    let mut digits = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
        } else if !digits.is_empty() {
            break;
        }
    }
    digits.parse().ok()
}

/// If `line` is `<label> : <int>` (the harness headline form, allowing arbitrary spaces around the
/// colon and trailing text), return the integer. The match is anchored on the label being a prefix
/// of the pre-colon text so `PASS` does not match `Pass-rate (of runnable)`.
fn label_count(line: &str, label: &str) -> Option<u32> {
    let (head, tail) = line.split_once(':')?;
    if head.trim() != label {
        return None;
    }
    first_uint(tail)
}

/// If `line` is a markdown table row `| <label> | <int> |`, return `(label, int)`.
fn table_row_count(line: &str) -> Option<(String, u32)> {
    if !line.starts_with('|') {
        return None;
    }
    let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
    if cells.len() < 2 {
        return None;
    }
    // Strip markdown bold/backticks the report uses on some rows.
    let label = cells[0].trim_matches(|c| c == '*' || c == '`' || c == ' ').to_string();
    let value = first_uint(cells[1])?;
    Some((label, value))
}

/// A case ID looks like `<category>/<description>` (compliance) or a bare fixture id (oracle). We
/// only require it be non-empty and not contain whitespace-only or markdown noise.
fn is_case_id(s: &str) -> bool {
    !s.is_empty() && !s.contains('|') && !s.starts_with('#') && !s.starts_with("**")
}

/// Push `id` into `v` only if not already present (preserves insertion order, small N).
fn push_unique(v: &mut Vec<String>, id: String) {
    if !v.contains(&id) {
        v.push(id);
    }
}

// ---------------------------------------------------------------------------
// Child-process invocation. Thin shell around the parsers; never runs cargo, never edits render3.
// ---------------------------------------------------------------------------

/// Where the harnesses live, relative to the Treaty repo root.
const COMPLIANCE_SCRIPT: &str = "libs/render3/compliance/run-compliance.mjs";
const PARITY_SCRIPT: &str = "libs/render3/parity/parity.mjs";

/// Errors invoking a harness child process.
#[derive(Debug)]
pub enum RunError {
    /// The harness script does not exist at the resolved path.
    ScriptMissing(PathBuf),
    /// `node` could not be spawned (not installed / not on PATH).
    Spawn(std::io::Error),
    /// The harness exited with a status indicating it could not run at all (e.g. the compliance
    /// harness exits 2 when the Rust addon is unavailable). Carries the captured stderr.
    HarnessUnavailable { code: Option<i32>, stderr: String },
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::ScriptMissing(p) => write!(f, "harness script not found: {}", p.display()),
            RunError::Spawn(e) => write!(f, "failed to spawn node: {e}"),
            RunError::HarnessUnavailable { code, stderr } => {
                write!(f, "harness unavailable (exit {code:?}): {}", stderr.trim())
            }
        }
    }
}

impl std::error::Error for RunError {}

/// Run the compliance harness (`node run-compliance.mjs --verbose`) under `repo_root` and parse its
/// stdout. `--verbose` is passed so the per-case `DIFF <id>` lines are emitted for drift detection.
///
/// READ-ONLY: this only runs `node` on the existing JS harness; it never invokes `cargo` for
/// render3 and never writes into `libs/render3`.
pub fn run_compliance(repo_root: &Path) -> Result<ComplianceResult, RunError> {
    let script = repo_root.join(COMPLIANCE_SCRIPT);
    if !script.exists() {
        return Err(RunError::ScriptMissing(script));
    }
    let output = Command::new("node")
        .arg(&script)
        .arg("--verbose")
        .current_dir(repo_root)
        .output()
        .map_err(RunError::Spawn)?;

    // The compliance harness exits 2 when the Rust addon is unavailable (nothing to measure).
    if output.status.code() == Some(2) {
        return Err(RunError::HarnessUnavailable {
            code: Some(2),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_compliance(&stdout))
}

/// Run the oracle parity harness (`node parity.mjs`) under `repo_root` and parse its stdout.
///
/// READ-ONLY: same guarantees as [`run_compliance`]. The parity harness exits non-zero when a DIFF
/// occurred, which is a normal (parseable) outcome, not a failure to run — so a non-zero exit is
/// NOT treated as [`RunError`]; only a spawn failure or missing script is.
pub fn run_oracle(repo_root: &Path) -> Result<OracleResult, RunError> {
    let script = repo_root.join(PARITY_SCRIPT);
    if !script.exists() {
        return Err(RunError::ScriptMissing(script));
    }
    let output = Command::new("node")
        .arg(&script)
        .current_dir(repo_root)
        .output()
        .map_err(RunError::Spawn)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_oracle(&stdout))
}

/// Run BOTH harnesses under `repo_root` and assemble a [`ConformanceReport`]. A harness that fails
/// to run (missing script / node not installed / addon unavailable) is left `None` rather than
/// aborting the whole report, so a partial run is still actionable; the per-harness error is
/// returned alongside for the caller to surface.
pub fn run_conformance(repo_root: &Path) -> (ConformanceReport, Vec<RunError>) {
    let mut report = ConformanceReport::default();
    let mut errors = Vec::new();

    match run_compliance(repo_root) {
        Ok(c) => report.compliance = Some(c),
        Err(e) => errors.push(e),
    }
    match run_oracle(repo_root) {
        Ok(o) => report.oracle = Some(o),
        Err(e) => errors.push(e),
    }

    (report, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Captured sample of `run-compliance.mjs` stdout in `--verbose` mode (headline block + a couple
    // of `DIFF <id>` lines, matching printReport() in run-compliance.mjs).
    const COMPLIANCE_STDOUT: &str = "\
======================================================================
Treaty render3 vs Angular compliance suite (SOURCE front-end)
======================================================================
Rust addon: libs/authoring/node/authoring_node.win32-x64-msvc.node

Total compliance cases     : 642
Compiled (runnable)        : 98
  PASS  : 50
  DIFF  : 48
Skipped (un-runnable)      : 544

Pass-rate (of runnable)    : 51.0%  (50/98)
Pass-rate (of total)       : 7.8%  (50/642)

DIFF basic/some_case
   ɵɵtext: ...near detail...
DIFF events/host_listener
   ɵɵlistener: ...near detail...

Top DIFF/gap categories (runnable cases that diverge):
    20  ɵɵtext
    18  ɵɵlistener

Top skip categories (case not runnable through source front-end):
   200  fe:providers
";

    // Captured sample of `parity.mjs` stdout (per-fixture blocks + summary, matching main()).
    const ORACLE_STDOUT: &str = "\
Treaty render3 <-> @angular/compiler parity harness
================================================================
Rust addon: LOADED from libs/authoring/node/authoring_node.win32-x64-msvc.node

----------------------------------------------------------------
Fixture: static-element
  template: <button>Hi</button>
  RESULT: PASS
----------------------------------------------------------------
Fixture: interpolation
  template: <div>{{name}}</div>
  RESULT: PASS
----------------------------------------------------------------
Fixture: control-flow-if-elseif-else
  template: <div>@if (a) { <span>x</span> }</div>
  RESULT: DIFF
  first divergence at normalized index 120
    oracle: ...ɵɵconditionalCreate...
    rust:   ...ɵɵconditionalBranchCreate...
----------------------------------------------------------------
Fixture: i18n-static
  template: <div i18n>Hello</div>
  ORACLE ERROR: only important for i18n
================================================================
Summary: 2 PASS, 2 DIFF, 0 ORACLE-ONLY (of 4)
";

    #[test]
    fn parses_compliance_headline_counts() {
        let r = parse_compliance(COMPLIANCE_STDOUT);
        assert_eq!(r.total, 642);
        assert_eq!(r.runnable, 98);
        assert_eq!(r.pass, 50);
        assert_eq!(r.diff, 48);
        assert_eq!(r.skipped, 544);
    }

    #[test]
    fn pass_label_does_not_match_pass_rate_line() {
        // `Pass-rate (of runnable)    : 51.0%` must NOT be parsed as the PASS count.
        let r = parse_compliance(COMPLIANCE_STDOUT);
        assert_eq!(r.pass, 50, "PASS came from the headline, not the pass-rate line");
    }

    #[test]
    fn parses_compliance_verbose_diff_ids() {
        let r = parse_compliance(COMPLIANCE_STDOUT);
        assert!(r.diff_ids.contains(&"basic/some_case".to_string()));
        assert!(r.diff_ids.contains(&"events/host_listener".to_string()));
        assert_eq!(r.diff_ids.len(), 2, "unexpected diff_ids: {:?}", r.diff_ids);
    }

    #[test]
    fn parses_oracle_summary_counts() {
        let r = parse_oracle(ORACLE_STDOUT);
        assert_eq!(r.pass, 2);
        assert_eq!(r.diff, 2);
        assert_eq!(r.oracle_only, 0);
        assert_eq!(r.total, 4);
    }

    #[test]
    fn parses_oracle_per_fixture_results() {
        let r = parse_oracle(ORACLE_STDOUT);
        assert!(r.pass_ids.contains(&"static-element".to_string()));
        assert!(r.pass_ids.contains(&"interpolation".to_string()));
        // A DIFF fixture and an ORACLE-ERROR fixture both land in diff_ids.
        assert!(r.diff_ids.contains(&"control-flow-if-elseif-else".to_string()));
        assert!(r.diff_ids.contains(&"i18n-static".to_string()));
        assert_eq!(r.pass_ids.len(), 2);
        assert_eq!(r.diff_ids.len(), 2);
    }

    #[test]
    fn oracle_falls_back_to_derived_counts_without_summary() {
        let no_summary = "\
Fixture: a
  RESULT: PASS
Fixture: b
  RESULT: DIFF
";
        let r = parse_oracle(no_summary);
        assert_eq!(r.pass, 1);
        assert_eq!(r.diff, 1);
        assert_eq!(r.pass_ids, vec!["a".to_string()]);
        assert_eq!(r.diff_ids, vec!["b".to_string()]);
    }

    #[test]
    fn parses_compliance_markdown_report_form() {
        // A captured COMPLIANCE-REPORT.md feeds the same parser via the table rows + passing list.
        let md = "\
## Headline numbers

| Metric | Value |
| --- | --- |
| Total compliance cases | 642 |
| Compiled (runnable) | 98 |
| PASS | 50 |
| DIFF | 48 |
| Skipped (un-runnable) | 544 |

## Passing cases

50 runnable compliance cases match Angular's golden:

- basic/simple_component
- basic/another_component
";
        let r = parse_compliance(md);
        assert_eq!(r.total, 642);
        assert_eq!(r.pass, 50);
        assert_eq!(r.diff, 48);
        assert!(r.pass_ids.contains(&"basic/simple_component".to_string()));
        assert!(r.pass_ids.contains(&"basic/another_component".to_string()));
    }

    // ---- compareToBaseline: the newly-failing drift surface ----

    fn report(compliance_pass: &[&str], compliance_diff: &[&str], oracle_pass: &[&str], oracle_diff: &[&str]) -> ConformanceReport {
        ConformanceReport {
            compliance: Some(ComplianceResult {
                total: 100,
                runnable: (compliance_pass.len() + compliance_diff.len()) as u32,
                pass: compliance_pass.len() as u32,
                diff: compliance_diff.len() as u32,
                skipped: 0,
                pass_ids: compliance_pass.iter().map(|s| s.to_string()).collect(),
                diff_ids: compliance_diff.iter().map(|s| s.to_string()).collect(),
            }),
            oracle: Some(OracleResult {
                pass: oracle_pass.len() as u32,
                diff: oracle_diff.len() as u32,
                oracle_only: 0,
                total: (oracle_pass.len() + oracle_diff.len()) as u32,
                pass_ids: oracle_pass.iter().map(|s| s.to_string()).collect(),
                diff_ids: oracle_diff.iter().map(|s| s.to_string()).collect(),
            }),
        }
    }

    #[test]
    fn compare_lists_newly_failing_cases() {
        let prev = report(&["a", "b", "c"], &[], &["x", "y"], &[]);
        // `b` regressed to a DIFF; `y` oracle fixture regressed too.
        let curr = report(&["a", "c"], &["b"], &["x"], &["y"]);
        let cmp = compare_to_baseline(&prev, &curr);
        assert_eq!(
            cmp.newly_failing,
            vec!["compliance/b".to_string(), "oracle/y".to_string()]
        );
        assert!(cmp.newly_passing.is_empty());
        assert!(cmp.disappeared.is_empty());
        assert_eq!(cmp.compliance_diff_delta, 1);
        assert_eq!(cmp.oracle_diff_delta, 1);
        assert!(!cmp.is_clean());
    }

    #[test]
    fn compare_clean_when_nothing_regresses() {
        let prev = report(&["a", "b"], &["c"], &["x"], &[]);
        let curr = report(&["a", "b"], &["c"], &["x"], &[]);
        let cmp = compare_to_baseline(&prev, &curr);
        assert!(cmp.newly_failing.is_empty());
        assert!(cmp.is_clean());
    }

    #[test]
    fn compare_reports_newly_passing_progress() {
        let prev = report(&["a"], &["b"], &[], &[]);
        let curr = report(&["a", "b"], &[], &[], &[]);
        let cmp = compare_to_baseline(&prev, &curr);
        assert_eq!(cmp.newly_passing, vec!["compliance/b".to_string()]);
        assert!(cmp.newly_failing.is_empty());
        // A shrinking DIFF count is progress, so the comparison is clean.
        assert!(cmp.is_clean());
    }

    #[test]
    fn vanished_pass_with_rising_diff_is_drift_not_disappearance() {
        // `b` passed before; now it is reported neither pass nor fail, AND the compliance DIFF count
        // rose. That is a regression (the case stopped matching and got reclassified), not a clean
        // corpus removal.
        let prev = report(&["a", "b"], &[], &[], &[]);
        let mut curr = report(&["a"], &[], &[], &[]);
        // Force the DIFF count up without naming the case (simulates aggregate-only signal).
        curr.compliance.as_mut().unwrap().diff = 1;
        let cmp = compare_to_baseline(&prev, &curr);
        assert_eq!(cmp.newly_failing, vec!["compliance/b".to_string()]);
        assert!(cmp.disappeared.is_empty());
    }

    #[test]
    fn vanished_pass_without_rising_diff_is_a_clean_removal() {
        // `b` passed before and is simply gone now, with DIFF unchanged — a corpus removal.
        let prev = report(&["a", "b"], &[], &[], &[]);
        let curr = report(&["a"], &[], &[], &[]);
        let cmp = compare_to_baseline(&prev, &curr);
        assert!(cmp.newly_failing.is_empty());
        assert_eq!(cmp.disappeared, vec!["compliance/b".to_string()]);
        assert!(cmp.is_clean());
    }

    #[test]
    fn report_passing_and_failing_cases_are_namespaced() {
        let r = report(&["a"], &["b"], &["a"], &["b"]);
        // Same bare names in both harnesses must NOT collide.
        assert_eq!(
            r.passing_cases(),
            vec!["compliance/a".to_string(), "oracle/a".to_string()]
        );
        assert_eq!(
            r.failing_cases(),
            vec!["compliance/b".to_string(), "oracle/b".to_string()]
        );
    }

    #[test]
    fn report_clean_iff_no_diffs() {
        assert!(report(&["a"], &[], &["x"], &[]).is_clean());
        assert!(!report(&["a"], &["b"], &["x"], &[]).is_clean());
        assert!(!report(&["a"], &[], &["x"], &["y"]).is_clean());
        // A harness that didn't run doesn't count against cleanliness.
        let only_oracle = ConformanceReport { compliance: None, oracle: Some(OracleResult::default()) };
        assert!(only_oracle.is_clean());
    }

    #[test]
    fn report_round_trips_through_json() {
        let r = report(&["a"], &["b"], &["x"], &["y"]);
        let json = serde_json::to_string_pretty(&r).expect("serialize");
        let back: ConformanceReport = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(r, back);
    }
}
