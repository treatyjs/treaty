//! The byte-equality **gate** + the baseline/drift tripwire + the migrate-plan seed.
//!
//! All deterministic, NO-AI. The gate (SWC-BACKEND-PLAN.md §4) compiles each corpus fixture through
//! every enabled backend and asserts pairwise byte-equality. Baseline/drift mirrors
//! `render3-sync`'s `baseline`/`drift`: record the reference (oxc) output and diff later runs
//! against it to catch a future change.

use std::collections::BTreeMap;

use crate::backend::{enabled_backends, Backend};
use crate::corpus::{Fixture, CORPUS};
use crate::report::{
    Baseline, Difficulty, DriftEntry, DriftKind, DriftReport, FixtureParity, FixtureReport,
    MigratePlan, ParityReport, PortTask,
};

/// The reference backend name (the baseline is always recorded from this one).
pub const REFERENCE_BACKEND: &str = "oxc";

// ------------------------------------------------------------------------------------------------
// Parity gate.
// ------------------------------------------------------------------------------------------------

/// Run every enabled backend over the whole corpus and compare pairwise, producing a [`ParityReport`].
pub fn run_parity() -> ParityReport {
    let backends = enabled_backends();
    let names: Vec<String> = backends.iter().map(|b| b.name().to_string()).collect();

    let fixtures = CORPUS
        .iter()
        .map(|fx| FixtureReport { id: fx.id.to_string(), parity: compare_backends(fx, &backends) })
        .collect();

    ParityReport { backends: names, fixtures }
}

/// Compile one fixture through every backend and compare. With a single backend this is trivially
/// `Ok` (parity with itself); with two or more, every pair must be byte-identical.
fn compare_backends(fixture: &Fixture, backends: &[Box<dyn Backend>]) -> FixtureParity {
    // Collect each backend's output (or its error, recorded verbatim as an `ERROR:` marker).
    let mut outputs: BTreeMap<String, String> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut had_error = false;
    for backend in backends {
        let name = backend.name().to_string();
        order.push(name.clone());
        match backend.compile(fixture) {
            Ok(code) => {
                outputs.insert(name, code);
            }
            Err(e) => {
                had_error = true;
                outputs.insert(name, format!("ERROR: {e}"));
            }
        }
    }

    if had_error {
        let detail = first_error_detail(&outputs);
        return FixtureParity::Diff { outputs, detail };
    }

    // Pairwise byte-equality against the FIRST backend (the reference). Equality is transitive, so
    // "all equal to the first" == "all pairwise equal".
    let reference = &order[0];
    let reference_out = &outputs[reference];
    for name in order.iter().skip(1) {
        let other = &outputs[name];
        if other != reference_out {
            let detail = format!(
                "backend `{name}` diverges from `{reference}`: {}",
                describe_first_diff(reference_out, other)
            );
            return FixtureParity::Diff { outputs, detail };
        }
    }

    FixtureParity::Ok { backends: order }
}

/// Pick a short detail line when some backend errored.
fn first_error_detail(outputs: &BTreeMap<String, String>) -> String {
    for (name, out) in outputs {
        if out.starts_with("ERROR: ") {
            return format!("backend `{name}` failed: {}", &out["ERROR: ".len()..]);
        }
    }
    "a backend failed to compile".to_string()
}

// ------------------------------------------------------------------------------------------------
// Baseline / drift.
// ------------------------------------------------------------------------------------------------

/// Record the reference-backend output for every corpus fixture into a [`Baseline`].
///
/// Always uses the reference (oxc) backend regardless of feature flags — the baseline is the
/// committed tripwire for oxc's output, the stable side. A fixture the backend cannot compile is
/// recorded with its `ERROR: …` marker so a future fix that makes it compile reads as drift.
pub fn record_baseline() -> Baseline {
    let backend = reference_backend();
    let mut outputs: BTreeMap<String, String> = BTreeMap::new();
    for fx in CORPUS {
        let value = backend.compile(fx).unwrap_or_else(|e| format!("ERROR: {e}"));
        outputs.insert(fx.id.to_string(), value);
    }
    Baseline { backend: backend.name().to_string(), outputs }
}

/// Diff the CURRENT reference-backend output against a recorded `baseline`, producing a [`DriftReport`].
///
/// A fixture present now but not in the baseline is `Added`; present in the baseline but not now is
/// `Removed`; present in both with differing output is `Modified`. Equal == unchanged (omitted).
pub fn diff_baseline(baseline: &Baseline) -> DriftReport {
    let current = record_baseline();

    let mut entries: Vec<DriftEntry> = Vec::new();

    // Union of ids, deterministic order (BTreeMap keys are sorted).
    let mut ids: Vec<&String> = baseline.outputs.keys().chain(current.outputs.keys()).collect();
    ids.sort();
    ids.dedup();

    for id in ids {
        let change = match (baseline.outputs.get(id), current.outputs.get(id)) {
            (None, Some(_)) => Some(DriftKind::Added),
            (Some(_), None) => Some(DriftKind::Removed),
            (Some(old), Some(new)) if old != new => Some(DriftKind::Modified {
                detail: describe_first_diff(old, new),
            }),
            _ => None,
        };
        if let Some(change) = change {
            entries.push(DriftEntry { id: id.clone(), change });
        }
    }

    DriftReport { backend: current.backend, entries }
}

/// The reference backend instance (oxc) used to record/refresh the baseline.
fn reference_backend() -> Box<dyn Backend> {
    // The oxc backend is always compiled (it is in the default feature set and the reference). We
    // construct it directly so the baseline never depends on whether other features are on.
    Box::new(crate::backend::oxc::OxcBackend)
}

// ------------------------------------------------------------------------------------------------
// migrate-plan seed.
// ------------------------------------------------------------------------------------------------

/// Emit the structured port-task seed describing what the SWC backend must implement to match oxc.
///
/// Seeded from a small embedded subset of SWC-BACKEND-PLAN.md §2's oxc→swc API mapping table. This
/// is the "migrate future changes" seed: when the SWC backend is built, each task is a checklist
/// item; the parity gate (`parity`) then proves they were done right (byte-equal output).
pub fn build_migrate_plan() -> MigratePlan {
    MigratePlan {
        source: "migration/SWC-BACKEND-PLAN.md §2 (oxc → swc API mapping)".to_string(),
        tasks: vec![
            PortTask {
                concern: "Arena / lifetimes".to_string(),
                oxc: "oxc_allocator::Allocator passed explicitly; 'a-borrowed AST".to_string(),
                swc: "swc_common::GLOBALS thread-local SourceMap; owned Box/Vec".to_string(),
                difficulty: Difficulty::Hard,
                notes: "Every SWC op runs inside GLOBALS.set(...); the backend owns this context so callers never see it.".to_string(),
            },
            PortTask {
                concern: "Parse".to_string(),
                oxc: "Parser::new(&alloc, src, SourceType).parse() -> borrowed Program<'a>".to_string(),
                swc: "swc_ecma_parser::parse_file_as_program(&fm, syntax, target, comments, &mut errs) -> owned Program".to_string(),
                difficulty: Difficulty::Moderate,
                notes: "SWC needs a SourceFile from the SourceMap (inside GLOBALS); returns owned, no 'a.".to_string(),
            },
            PortTask {
                concern: "Source type / syntax".to_string(),
                oxc: "SourceType::ts() / ::tsx()".to_string(),
                swc: "Syntax::Typescript(TsSyntax { tsx, decorators, .. })".to_string(),
                difficulty: Difficulty::Trivial,
                notes: "Direct enum swap; tsx flag maps 1:1.".to_string(),
            },
            PortTask {
                concern: "AST builder".to_string(),
                oxc: "oxc_ast::AstBuilder (ast.<node>(...) into arena)".to_string(),
                swc: "direct swc_ecma_ast struct literals / Box::new(...)".to_string(),
                difficulty: Difficulty::Moderate,
                notes: "Bulk of the emit port: every self.ast.<node>(...) in emitter.rs becomes an owned struct (one Lowerer per backend).".to_string(),
            },
            PortTask {
                concern: "Codegen".to_string(),
                oxc: "oxc_codegen::Codegen::default().build(&program).code -> CodegenReturn".to_string(),
                swc: "swc_ecma_codegen::Emitter { cfg, cm, comments, wr } writing into a Vec<u8>".to_string(),
                difficulty: Difficulty::Moderate,
                notes: "OXC is stateless + returns a string; SWC is a writer-based Emitter. Precedence-based parenthesization (emitter.rs §7.1/§7.2) must be matched — the parity gate proves it.".to_string(),
            },
            PortTask {
                concern: "Source map".to_string(),
                oxc: "CodegenReturn.map (oxc sourcemap)".to_string(),
                swc: "Emitter source-map output via cm + SourceMapBuilder".to_string(),
                difficulty: Difficulty::Moderate,
                notes: "output/source_map.rs already owns v3 shaping (byte offsets, UTF-16 columns); the adapter only feeds raw mappings.".to_string(),
            },
            PortTask {
                concern: "Semantic".to_string(),
                oxc: "oxc_semantic::{Semantic, Scope, SymbolId, Reference}".to_string(),
                swc: "no equivalent — kept OXC-only by design".to_string(),
                difficulty: Difficulty::Hard,
                notes: "NOT on the Ivy hot path today; never required of the SWC backend (plan §0 non-goal).".to_string(),
            },
        ],
    }
}

// ------------------------------------------------------------------------------------------------
// Shared byte-diff description (NO regex; pure byte scan).
// ------------------------------------------------------------------------------------------------

/// Describe the first byte at which `a` and `b` differ, with a small surrounding window for context.
/// Pure byte comparison (no normalization, no regex) — strict equality is the policy
/// (SWC-BACKEND-PLAN.md §4.3).
pub fn describe_first_diff(a: &str, b: &str) -> String {
    let ab = a.as_bytes();
    let bb = b.as_bytes();
    let n = ab.len().min(bb.len());
    let mut i = 0;
    while i < n && ab[i] == bb[i] {
        i += 1;
    }
    if i == n && ab.len() == bb.len() {
        return "identical".to_string();
    }
    let start = i.saturating_sub(20);
    let a_win = window(a, start, i + 20);
    let b_win = window(b, start, i + 20);
    format!("first diff at byte {i}: left=...{a_win}... right=...{b_win}...")
}

/// A char-boundary-safe slice of `s` over the byte range `[start, end)`, for diff context windows.
fn window(s: &str, start: usize, end: usize) -> String {
    let bytes = s.as_bytes();
    let end = end.min(bytes.len());
    // Walk forward to a char boundary for both ends so we never slice mid-UTF-8 (ɵ is multibyte).
    let mut lo = start.min(bytes.len());
    while lo < bytes.len() && !s.is_char_boundary(lo) {
        lo += 1;
    }
    let mut hi = end;
    while hi < bytes.len() && !s.is_char_boundary(hi) {
        hi += 1;
    }
    s[lo..hi].replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(feature = "swc"))]
    fn parity_is_all_ok_with_only_oxc_enabled() {
        // With the default (oxc-only) feature set, every fixture is in parity with itself.
        let report = run_parity();
        assert_eq!(report.backends, vec!["oxc".to_string()]);
        assert!(report.is_all_ok(), "oxc-only parity must be all-ok; diffs: {:?}", report.diffs());
        assert_eq!(report.fixtures.len(), CORPUS.len());
    }

    #[test]
    #[cfg(feature = "swc")]
    fn parity_oxc_vs_swc_is_byte_identical_across_corpus() {
        // With `--features swc` the gate does REAL work: every corpus fixture must emit
        // byte-identical Ivy through the oxc emitter and the neutral (swc-side) printer.
        let report = run_parity();
        assert_eq!(report.backends, vec!["oxc".to_string(), "swc".to_string()]);
        assert!(
            report.is_all_ok(),
            "oxc vs swc must be byte-identical across the corpus; diffs: {:?}",
            report.diffs()
        );
        assert_eq!(report.fixtures.len(), CORPUS.len());
    }

    #[test]
    fn baseline_round_trips_and_is_clean_against_itself() {
        let baseline = record_baseline();
        assert_eq!(baseline.backend, "oxc");
        assert_eq!(baseline.outputs.len(), CORPUS.len());
        // A fresh diff against the just-recorded baseline must be clean (deterministic compiler).
        let drift = diff_baseline(&baseline);
        assert!(drift.is_clean(), "self-diff must be clean; entries: {:?}", drift.entries);
        // JSON round-trip (the artifact is committed).
        let json = serde_json::to_string_pretty(&baseline).unwrap();
        let back: Baseline = serde_json::from_str(&json).unwrap();
        assert_eq!(baseline, back);
    }

    #[test]
    fn drift_detects_a_corrupted_baseline_entry() {
        let mut baseline = record_baseline();
        // Corrupt one fixture's recorded output -> Modified drift for exactly that id.
        let id = CORPUS[0].id.to_string();
        baseline.outputs.insert(id.clone(), "definitely not the real output".to_string());
        let drift = diff_baseline(&baseline);
        assert!(!drift.is_clean());
        let entry = drift.entries.iter().find(|e| e.id == id).expect("corrupted id must drift");
        assert!(matches!(entry.change, DriftKind::Modified { .. }), "{:?}", entry.change);
    }

    #[test]
    fn migrate_plan_has_seed_rows() {
        let plan = build_migrate_plan();
        assert!(plan.source.contains("SWC-BACKEND-PLAN.md"));
        assert!(!plan.tasks.is_empty());
        // The three genuinely hard architectural rows are present (plan §2 / §3).
        assert!(plan.tasks.iter().any(|t| t.concern == "Arena / lifetimes" && t.difficulty == Difficulty::Hard));
        assert!(plan.tasks.iter().any(|t| t.concern == "Semantic" && t.difficulty == Difficulty::Hard));
    }

    #[test]
    fn describe_first_diff_is_utf8_safe_on_multibyte_marker() {
        // ɵ is 2 bytes; ensure the windowing never panics slicing mid-codepoint.
        let a = "\u{0275}\u{0275}defineComponent(A)";
        let b = "\u{0275}\u{0275}defineComponent(B)";
        let d = describe_first_diff(a, b);
        assert!(d.starts_with("first diff at byte"), "{d}");
    }
}
