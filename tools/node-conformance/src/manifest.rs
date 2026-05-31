//! The known-unsupported manifest, its matcher, and the conformance reporter.
//!
//! Two independent-but-related concerns live here:
//!
//! 1. **The known-unsupported manifest** ([`UnsupportedManifest`]). A JSON-backed list of test
//!    names/globs the Treaty runtime *cannot yet pass*, each paired with a human-readable reason
//!    (`"crypto not implemented"`, `"worker_threads pending"`, `"http server pending"`, ...). The
//!    runner loads it once and consults it for every corpus file: a file whose name matches an
//!    entry is recorded as [`crate::CaseStatus::Skip`] *without being evaluated*, exactly like an
//!    in-file `CONFORMANCE: skip` directive — but tracked centrally so the gap is visible and
//!    diffable in one place. This is the same bookkeeping Bun and Deno keep for the Node tests they
//!    knowingly do not pass yet.
//!
//! 2. **The reporter** ([`render_table`], [`write_report_json`]). Turns a
//!    [`crate::ConformanceReport`] into (a) a per-module + overall pass-rate table for humans and
//!    (b) a stable machine-readable JSON file for CI / a scoreboard to diff over time.
//!
//! # Why a manifest *and* an in-file directive?
//!
//! The in-file `CONFORMANCE: skip` directive ([`crate::detect_skip`]) is for a test whose *body*
//! references something unsupported (so it must never run). The manifest is for marking whole
//! tests — by exact name or by glob — without editing their source, which is what you want when
//! seeding upstream Node `test/parallel` files verbatim: you do not touch the vendored `.js`, you
//! add one manifest line. Either mechanism yields the same SKIP outcome and reason.
//!
//! # Manifest file format
//!
//! `known-unsupported.json` is an object with a single `"unsupported"` array; each element is an
//! object with a `"pattern"` (an exact case name or a `*`/`?` glob over case names) and a
//! `"reason"`:
//!
//! ```json
//! {
//!   "unsupported": [
//!     { "pattern": "crypto-*",        "reason": "node:crypto not implemented" },
//!     { "pattern": "worker-threads",  "reason": "worker_threads pending" }
//!   ]
//! }
//! ```
//!
//! Matching is performed against a case's **name** (the corpus file stem, e.g. `fs-roundtrip`).

use std::fmt::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{CaseStatus, ConformanceReport};

/// The canonical on-disk name of the manifest shipped with the crate.
pub const MANIFEST_FILE_NAME: &str = "known-unsupported.json";

/// One known-unsupported entry: a name/glob `pattern` over case names and the `reason` the runtime
/// cannot pass tests matching it yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsupportedEntry {
    /// An exact case name (e.g. `worker-threads`) or a glob over case names. Supported glob
    /// metacharacters: `*` (any run of characters, including none) and `?` (exactly one
    /// character). All other characters match literally. See [`glob_matches`].
    pub pattern: String,
    /// The human-readable justification recorded on the resulting [`CaseStatus::Skip`], e.g.
    /// `"node:crypto not implemented"`.
    pub reason: String,
}

/// The deserialized `known-unsupported.json`: the list of patterns the runner skips.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsupportedManifest {
    /// The known-unsupported entries, in file order. The first entry whose `pattern` matches a
    /// case name wins (see [`UnsupportedManifest::match_reason`]).
    #[serde(default)]
    pub unsupported: Vec<UnsupportedEntry>,
}

/// An error loading or parsing the manifest from disk.
#[derive(Debug)]
pub enum ManifestError {
    /// The manifest file could not be read.
    Read(std::io::Error),
    /// The manifest file was read but is not valid JSON in the expected shape.
    Parse(serde_json::Error),
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestError::Read(error) => write!(f, "could not read manifest: {error}"),
            ManifestError::Parse(error) => write!(f, "could not parse manifest JSON: {error}"),
        }
    }
}

impl std::error::Error for ManifestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ManifestError::Read(error) => Some(error),
            ManifestError::Parse(error) => Some(error),
        }
    }
}

impl UnsupportedManifest {
    /// Parse a manifest from a JSON string.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Load and parse the manifest from `path`.
    ///
    /// Returns [`ManifestError::Read`] if the file is missing/unreadable and
    /// [`ManifestError::Parse`] if its contents are not the expected JSON shape.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ManifestError> {
        let text = std::fs::read_to_string(path).map_err(ManifestError::Read)?;
        Self::from_json(&text).map_err(ManifestError::Parse)
    }

    /// Load the manifest that ships alongside this crate (`<crate>/known-unsupported.json`).
    ///
    /// A *missing* file is treated as an empty manifest (no known-unsupported entries) rather than
    /// an error, so the harness still runs in a checkout that has not authored one yet. A file that
    /// exists but is malformed is still surfaced as [`ManifestError::Parse`], because that is a
    /// corpus-integrity problem the operator should fix rather than silently ignore.
    pub fn load_default() -> Result<Self, ManifestError> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(MANIFEST_FILE_NAME);
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::from_json(&text).map_err(ManifestError::Parse),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(ManifestError::Read(error)),
        }
    }

    /// Return the reason for the first entry whose `pattern` matches `case_name`, or `None` when no
    /// entry matches (the test should be executed normally).
    ///
    /// First match wins, so a more specific entry can precede a broad glob in the file to override
    /// its reason.
    pub fn match_reason(&self, case_name: &str) -> Option<&str> {
        self.unsupported
            .iter()
            .find(|entry| glob_matches(&entry.pattern, case_name))
            .map(|entry| entry.reason.as_str())
    }

    /// Run one test, consulting this manifest before evaluating.
    ///
    /// If `name` matches an entry the case is recorded [`CaseStatus::Skip`] with the manifest reason
    /// and the source is **never evaluated** (so a test exercising an unimplemented API cannot
    /// throw). Otherwise the source is handed to [`crate::run_source`], which still honours an
    /// in-file `// CONFORMANCE: skip` directive and then evaluates in a fresh Node-compat runtime.
    pub fn run_source(&self, name: &str, source: &str) -> crate::CaseResult {
        match self.match_reason(name) {
            Some(reason) => crate::CaseResult::skip(name, reason),
            None => crate::run_source(name, source),
        }
    }

    /// Run every `.js` file under `dir` (non-recursively, sorted for determinism), consulting this
    /// manifest for each, and fold the outcomes into a [`ConformanceReport`].
    ///
    /// This is the glob-manifest counterpart to [`crate::run_corpus`]: a file whose case name (its
    /// stem) matches a manifest pattern is skipped with the manifest reason without being evaluated;
    /// every other file runs normally. A file that cannot be read is recorded as a
    /// [`CaseStatus::Fail`] so a corpus-integrity problem is never silently omitted and `total`
    /// always equals the number of `.js` files discovered.
    ///
    /// Returns an [`std::io::Error`] only when `dir` itself cannot be enumerated.
    pub fn run_corpus(&self, dir: impl AsRef<Path>) -> std::io::Result<ConformanceReport> {
        let dir = dir.as_ref();
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(dir)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "js"))
            .collect();
        files.sort();

        let cases = files
            .into_iter()
            .map(|path| {
                let name = path
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string_lossy().into_owned());
                match std::fs::read_to_string(&path) {
                    Ok(source) => self.run_source(&name, &source),
                    Err(error) => crate::CaseResult::fail(
                        name,
                        format!("could not read test file: {error}"),
                        std::time::Duration::ZERO,
                    ),
                }
            })
            .collect();

        Ok(ConformanceReport::from_cases(cases))
    }
}

/// Match a case `name` against a `pattern` that may contain the glob metacharacters `*` (zero or
/// more characters) and `?` (exactly one character). Every other character is matched literally.
///
/// The match is anchored at both ends (the whole name must be consumed), implemented with linear
/// backtracking — patterns are short and the corpus is small, so this is more than fast enough and
/// avoids pulling in a regex/glob dependency for the harness.
pub fn glob_matches(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    glob_at(&pattern, &name)
}

/// Recursive anchored glob match over char slices. `*` consumes greedily with backtracking.
fn glob_at(pattern: &[char], name: &[char]) -> bool {
    match pattern.first() {
        None => name.is_empty(),
        Some('*') => {
            // `*` matches zero chars (skip it) or one-or-more (consume a name char, retry the `*`).
            glob_at(&pattern[1..], name)
                || (!name.is_empty() && glob_at(pattern, &name[1..]))
        }
        Some('?') => !name.is_empty() && glob_at(&pattern[1..], &name[1..]),
        Some(&literal) => {
            matches!(name.first(), Some(&first) if first == literal)
                && glob_at(&pattern[1..], &name[1..])
        }
    }
}

/// Derive a stable "module" label for a case from its name: the segment before the first `-`.
///
/// Corpus files follow a `<module>-<topic>` convention (`fs-roundtrip`, `path-basic`,
/// `crypto-hash`), so this groups every case by the Node surface it exercises. A name with no `-`
/// is its own module. Used only by the reporter to bucket the per-module table.
pub fn module_of(case_name: &str) -> &str {
    match case_name.split_once('-') {
        Some((module, _)) => module,
        None => case_name,
    }
}

/// One row of the per-module pass-rate table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModuleStats {
    /// The module label (see [`module_of`]).
    pub module: String,
    /// Cases in this module that passed.
    pub passed: usize,
    /// Cases in this module that failed.
    pub failed: usize,
    /// Cases in this module that were skipped (known-unsupported or in-file directive).
    pub skipped: usize,
    /// Fraction in `[0.0, 1.0]` of *executed* (non-skipped) cases in this module that passed;
    /// `0.0` when the module has no executed cases. Mirrors the overall
    /// [`ConformanceReport::pass_rate`] convention.
    pub pass_rate: f64,
}

impl ModuleStats {
    /// Cases actually executed in this module (`passed + failed`).
    pub fn executed(&self) -> usize {
        self.passed + self.failed
    }

    /// Total cases in this module across all statuses.
    pub fn total(&self) -> usize {
        self.passed + self.failed + self.skipped
    }
}

/// Aggregate a [`ConformanceReport`] into per-module [`ModuleStats`], sorted by module name for a
/// deterministic, diffable table.
///
/// Modules are derived with [`module_of`]. Within each module the pass-rate uses the same
/// executed-only denominator as the overall report, so a module that is entirely skipped reports a
/// `0.0` rate (it has attempted nothing) rather than a misleading `100%`.
pub fn module_stats(report: &ConformanceReport) -> Vec<ModuleStats> {
    // Accumulate (passed, failed, skipped) per module in a stable, sorted map.
    use std::collections::BTreeMap;
    let mut buckets: BTreeMap<&str, (usize, usize, usize)> = BTreeMap::new();
    for case in &report.cases {
        let entry = buckets.entry(module_of(&case.name)).or_insert((0, 0, 0));
        match case.status {
            CaseStatus::Pass => entry.0 += 1,
            CaseStatus::Fail => entry.1 += 1,
            CaseStatus::Skip => entry.2 += 1,
        }
    }
    buckets
        .into_iter()
        .map(|(module, (passed, failed, skipped))| {
            let executed = passed + failed;
            let pass_rate = if executed == 0 {
                0.0
            } else {
                passed as f64 / executed as f64
            };
            ModuleStats {
                module: module.to_owned(),
                passed,
                failed,
                skipped,
                pass_rate,
            }
        })
        .collect()
}

/// Render a human-readable per-module + overall pass-rate table for a [`ConformanceReport`].
///
/// The table has one row per Node module (alphabetical), columns `PASS / FAIL / SKIP / RATE`, and a
/// trailing `TOTAL` row carrying the report's overall counts and headline pass-rate. The output is
/// plain monospace-aligned text suitable for a terminal or a CI log; it is deterministic for a
/// given report.
pub fn render_table(report: &ConformanceReport) -> String {
    let stats = module_stats(report);

    // Width the module column to the widest label (or the headers), so columns line up.
    let module_header = "MODULE";
    let total_label = "TOTAL";
    let module_width = stats
        .iter()
        .map(|s| s.module.len())
        .chain([module_header.len(), total_label.len()])
        .max()
        .unwrap_or(module_header.len());

    let mut out = String::new();
    // Header.
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
    let _ = writeln!(out, "{}", "-".repeat(module_width + 2 + 5 + 2 + 5 + 2 + 5 + 2 + 7));

    // Per-module rows.
    for s in &stats {
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

    // Overall row.
    let _ = writeln!(out, "{}", "-".repeat(module_width + 2 + 5 + 2 + 5 + 2 + 5 + 2 + 7));
    let _ = writeln!(
        out,
        "{:<mw$}  {:>5}  {:>5}  {:>5}  {:>6.1}%",
        total_label,
        report.passed,
        report.failed,
        report.skipped,
        report.pass_rate * 100.0,
        mw = module_width,
    );

    out
}

/// Serialize the report as pretty JSON and write it to `path`, creating/truncating the file.
///
/// This is the machine-readable artifact a scoreboard / CI diff consumes; it is exactly the
/// [`ConformanceReport`] serialization (so it round-trips back via serde).
pub fn write_report_json(report: &ConformanceReport, path: impl AsRef<Path>) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(report)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    std::fs::write(path, json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CaseResult;
    use std::time::Duration;

    #[test]
    fn glob_exact_name_matches_only_itself() {
        assert!(glob_matches("worker-threads", "worker-threads"));
        assert!(!glob_matches("worker-threads", "worker-pool"));
        assert!(!glob_matches("worker-threads", "worker-threadsX"));
        assert!(!glob_matches("worker-threads", "Xworker-threads"));
    }

    #[test]
    fn glob_star_matches_prefix_family() {
        assert!(glob_matches("crypto-*", "crypto-hash"));
        assert!(glob_matches("crypto-*", "crypto-")); // `*` matches zero chars
        assert!(glob_matches("crypto-*", "crypto-randombytes"));
        assert!(!glob_matches("crypto-*", "cryptid")); // literal prefix must match
        assert!(!glob_matches("crypto-*", "http-server"));
    }

    #[test]
    fn glob_star_matches_anywhere_and_multiple() {
        assert!(glob_matches("*-server", "http-server"));
        assert!(glob_matches("*-server", "https-server"));
        assert!(glob_matches("*", "anything-at-all"));
        assert!(glob_matches("*", "")); // `*` matches the empty string
        assert!(glob_matches("a*b*c", "axxbyyc"));
        assert!(!glob_matches("a*b*c", "axxbyy")); // trailing `c` required
    }

    #[test]
    fn glob_question_matches_exactly_one_char() {
        assert!(glob_matches("fs?promises", "fs-promises"));
        assert!(glob_matches("fs?promises", "fs/promises"));
        assert!(!glob_matches("fs?promises", "fspromises")); // `?` needs a char
        assert!(!glob_matches("fs?promises", "fs--promises")); // exactly one
    }

    #[test]
    fn manifest_parses_and_matches_first_entry_wins() {
        let json = r#"
        {
          "unsupported": [
            { "pattern": "crypto-md5", "reason": "specific: md5 disabled" },
            { "pattern": "crypto-*",   "reason": "node:crypto not implemented" },
            { "pattern": "worker-threads", "reason": "worker_threads pending" }
          ]
        }"#;
        let manifest = UnsupportedManifest::from_json(json).expect("manifest parses");
        assert_eq!(manifest.unsupported.len(), 3);
        // First match wins: the specific md5 entry precedes the broad glob.
        assert_eq!(manifest.match_reason("crypto-md5"), Some("specific: md5 disabled"));
        assert_eq!(
            manifest.match_reason("crypto-sha256"),
            Some("node:crypto not implemented")
        );
        assert_eq!(manifest.match_reason("worker-threads"), Some("worker_threads pending"));
        // Unmatched names run normally.
        assert_eq!(manifest.match_reason("fs-roundtrip"), None);
    }

    #[test]
    fn manifest_default_when_unsupported_field_absent() {
        let manifest = UnsupportedManifest::from_json("{}").expect("empty object parses");
        assert!(manifest.unsupported.is_empty());
        assert_eq!(manifest.match_reason("anything"), None);
    }

    #[test]
    fn manifest_load_missing_file_via_load_is_read_error() {
        let err = UnsupportedManifest::load("d:/does/not/exist/known-unsupported.json")
            .expect_err("missing file is an error for explicit load()");
        assert!(matches!(err, ManifestError::Read(_)));
    }

    #[test]
    fn manifest_run_source_skips_matched_without_evaluating() {
        // A name matching a glob is skipped; the body (which would throw) must never run.
        let manifest =
            UnsupportedManifest::from_json(r#"{ "unsupported": [{ "pattern": "crypto-*", "reason": "node:crypto not implemented" }] }"#)
                .unwrap();
        let result = manifest.run_source("crypto-hash", "throw new Error('should never run');");
        assert_eq!(result.status, CaseStatus::Skip);
        assert_eq!(result.reason.as_deref(), Some("node:crypto not implemented"));
        assert_eq!(result.duration, Duration::ZERO);
    }

    #[test]
    fn manifest_run_source_executes_unmatched() {
        let manifest = UnsupportedManifest::default();
        let result = manifest.run_source("plain", "1 + 1;");
        assert_eq!(result.status, CaseStatus::Pass, "{:?}", result.reason);
    }

    #[test]
    fn manifest_run_corpus_skips_matched_seed_file() {
        // The shipped manifest's `child-process-*` glob must flip the seed `child-process-skip`
        // corpus file to Skip via the manifest path (independent of its in-file directive), proving
        // the glob manifest is wired through a real corpus walk.
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus");
        let manifest = UnsupportedManifest::load_default().expect("shipped manifest parses");
        let report = manifest.run_corpus(&dir).expect("corpus readable");
        assert!(report.total >= 1, "seed corpus must contain tests");
        // Every case named like an unsupported module must be Skip with a manifest/inline reason.
        for case in &report.cases {
            if manifest.match_reason(&case.name).is_some() {
                assert_eq!(
                    case.status,
                    CaseStatus::Skip,
                    "manifest-matched case {} must be skipped",
                    case.name
                );
            }
        }
        // Determinism: sorted by name.
        let names: Vec<&str> = report.cases.iter().map(|c| c.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[test]
    fn shipped_manifest_loads_and_is_nonempty() {
        // The crate ships a known-unsupported.json; it must parse and list at least one gap.
        let manifest = UnsupportedManifest::load_default().expect("shipped manifest must parse");
        assert!(
            !manifest.unsupported.is_empty(),
            "shipped manifest should enumerate known gaps (crypto, http, worker_threads, ...)"
        );
        // Sanity: a representative unimplemented surface is covered by some entry.
        assert!(
            manifest.match_reason("crypto-hash").is_some(),
            "a crypto-* test should be marked unsupported by the shipped manifest"
        );
    }

    #[test]
    fn module_of_splits_on_first_hyphen() {
        assert_eq!(module_of("fs-roundtrip"), "fs");
        assert_eq!(module_of("fs-promises-stat"), "fs"); // first hyphen only
        assert_eq!(module_of("buffer"), "buffer"); // no hyphen -> whole name
    }

    fn sample_report() -> ConformanceReport {
        ConformanceReport::from_cases(vec![
            CaseResult::pass("fs-roundtrip", Duration::from_millis(1)),
            CaseResult::pass("fs-stat", Duration::from_millis(1)),
            CaseResult::fail("fs-watch", "runtime: not impl", Duration::from_millis(1)),
            CaseResult::pass("path-basic", Duration::from_millis(1)),
            CaseResult::skip("crypto-hash", "node:crypto not implemented"),
        ])
    }

    #[test]
    fn module_stats_buckets_and_rates_per_module() {
        let report = sample_report();
        let stats = module_stats(&report);
        // Sorted by module name: crypto, fs, path.
        let names: Vec<&str> = stats.iter().map(|s| s.module.as_str()).collect();
        assert_eq!(names, vec!["crypto", "fs", "path"]);

        let fs = stats.iter().find(|s| s.module == "fs").unwrap();
        assert_eq!((fs.passed, fs.failed, fs.skipped), (2, 1, 0));
        assert_eq!(fs.total(), 3);
        assert_eq!(fs.executed(), 3);
        assert!((fs.pass_rate - 2.0 / 3.0).abs() < 1e-9);

        let path = stats.iter().find(|s| s.module == "path").unwrap();
        assert!((path.pass_rate - 1.0).abs() < f64::EPSILON);

        // An all-skipped module reports a 0.0 rate (nothing executed), not 100%.
        let crypto = stats.iter().find(|s| s.module == "crypto").unwrap();
        assert_eq!((crypto.passed, crypto.failed, crypto.skipped), (0, 0, 1));
        assert_eq!(crypto.executed(), 0);
        assert_eq!(crypto.pass_rate, 0.0);
    }

    #[test]
    fn render_table_has_header_module_rows_and_total() {
        let report = sample_report();
        let table = render_table(&report);
        assert!(table.contains("MODULE"));
        assert!(table.contains("PASS"));
        assert!(table.contains("RATE"));
        // One row per module plus a TOTAL row.
        assert!(table.contains("crypto"));
        assert!(table.contains("fs"));
        assert!(table.contains("path"));
        assert!(table.contains("TOTAL"));
        // The overall pass-rate (3 passed of 4 executed = 75.0%) appears on the TOTAL row.
        let total_line = table
            .lines()
            .find(|l| l.starts_with("TOTAL"))
            .expect("a TOTAL row");
        assert!(total_line.contains("75.0%"), "TOTAL line was: {total_line}");
    }

    #[test]
    fn write_report_json_round_trips_from_disk() {
        let report = sample_report();
        let dir = std::env::temp_dir();
        let path = dir.join(format!("node-conformance-test-report-{}.json", std::process::id()));
        write_report_json(&report, &path).expect("write report json");
        let text = std::fs::read_to_string(&path).expect("read back");
        let back: ConformanceReport = serde_json::from_str(&text).expect("parse back");
        assert_eq!(report, back);
        let _ = std::fs::remove_file(&path);
    }
}
