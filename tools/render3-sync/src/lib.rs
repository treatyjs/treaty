//! `render3-sync` — a deterministic (NO-AI) harness that keeps Treaty's Rust/oxc Ivy port (the
//! crates under `libs/treaty-ivy/*`, formerly the single `libs/render3`) 1:1 with Angular's
//! `packages/compiler`.
//!
//! See `tools/render3-sync/README.md` for the workflow. The modules:
//!   * [`symbol_map`] — the maintained TS-file/export -> Rust-module table, the out-of-scope list,
//!     and the file classification that makes an unmapped Angular file impossible to miss.
//!   * [`report`] — the serializable artifacts ([`DriftReport`], [`ChangedFile`], [`PortTask`]).
//!   * [`ts`] — a thin oxc wrapper that parses Angular TS and extracts its exported symbol names.
//!   * [`drift`] — the snapshot-based drift differ (name + body hash per export, attributed to Rust).
//!   * [`sync`] — the live-tree glue: `record_baseline`, `diff_baseline`, `verify_codegen`.
//!   * [`ts2rust`] — the mechanical emitter (enums, the `R3` identifier table, lookup tables).
//!   * [`conformance`] — pillar 2's conformance gate over the libs/treaty-ivy JS harnesses.
//!
//! The harness produces REPORTS and verification results; it never modifies the `libs/treaty-ivy/*`
//! ports.

pub mod conformance;
pub mod drift;
pub mod report;
pub mod symbol_map;
pub mod sync;
pub mod ts;
pub mod ts2rust;

pub use conformance::{
    BaselineComparison, ComplianceResult, ConformanceReport, Harness, OracleResult, RunError,
    compare_to_baseline, parse_compliance, parse_oracle, run_compliance, run_conformance,
    run_oracle,
};
pub use drift::{build_report, diff_file, fingerprint_exports, FileSnapshot, SymbolFingerprint};
pub use report::{
    unclassified_task, ChangeKind, ChangedFile, ChangedSymbol, DriftReport, PortTask, TaskAction,
};
pub use symbol_map::{
    FileClass, ModuleMapping, OutOfScopeFile, OutOfScopeSymbol, PortKind, ANGULAR_COMPILER_SRC_ROOT,
    MODULE_MAP, OUT_OF_SCOPE_FILES, OUT_OF_SCOPE_SYMBOLS, TREATY_IVY_SRC_ROOT,
};
pub use sync::{
    diff_baseline, list_ts_files, record_baseline, reference_ts_files, require_reference_root,
    verify_codegen, Baseline, FileBaseline, FsReader, SourceReader, VerifyEntry, VerifyReport,
    VerifyStatus,
};
pub use ts2rust::{diff_against, emit_rust, CodegenReport, Emitted, EmittedKind, Unsupported};

/// The Angular commit this port is pinned to, read from `tools/render3-sync/ANGULAR_REF` at build
/// time. `populate-angular-ref.sh` checks out this commit, `baseline` records it, and `drift` /
/// `codegen-verify` default to it, so the three cannot disagree (a test checks `baseline.json`).
pub fn pinned_angular_sha() -> &'static str {
    include_str!("../ANGULAR_REF").trim()
}

/// The harness crate's root (`tools/render3-sync`'s parent's parent: the Treaty repo) for tests that
/// read the vendored reference and the committed artifacts.
#[cfg(test)]
pub(crate) fn repo_root_for_tests() -> std::path::PathBuf {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent() // tools/
        .and_then(|p| p.parent()) // <repo>/
        .expect("crate is nested at <repo>/tools/render3-sync")
        .to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_sha_is_a_full_lowercase_commit_sha() {
        let sha = pinned_angular_sha();
        assert_eq!(sha.len(), 40, "{sha}");
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()), "{sha}");
    }

    #[test]
    fn committed_baseline_is_recorded_at_the_pinned_sha() {
        // The script, the baseline and the default refs must agree on one commit.
        let path = repo_root_for_tests().join("tools/render3-sync/baseline.json");
        let text = std::fs::read_to_string(&path).expect("read baseline.json");
        let baseline: Baseline = serde_json::from_str(&text).expect("parse baseline.json");
        assert_eq!(baseline.angular_ref, pinned_angular_sha());
        assert!(baseline.files.values().all(|f| f.exists), "a committed baseline has no missing files");
    }
}
