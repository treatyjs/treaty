//! `render3-sync` — a deterministic (NO-AI) harness that keeps Treaty's Rust/oxc `render3`
//! port 1:1 with Angular's `packages/compiler`.
//!
//! See `migration/RENDER3-SYNC-PLAN.md` for the three pillars. This crate is the scaffold:
//!   * [`symbol_map`] — the maintained TS-file/export -> Rust-module table (pillar 1 input).
//!   * [`report`] — the serializable artifacts ([`DriftReport`], [`ChangedSymbol`], [`PortTask`]).
//!   * [`ts`] — a thin oxc wrapper that parses Angular TS and extracts its exported symbol names
//!     (the deterministic primitive every pillar builds on).
//!   * [`drift`] — pillar 1's drift differ: diff two TS snapshots' exports (name + body hash) and
//!     map each change to its Rust owner, producing a [`DriftReport`] with derived [`PortTask`]s.
//!   * [`conformance`] — pillar 2's conformance gate: run the existing `libs/render3` compliance +
//!     oracle JS harnesses as child processes, parse their output into a
//!     [`conformance::ConformanceReport`], and compute the newly-failing drift surface via
//!     [`conformance::compare_to_baseline`].
//!
//! The harness produces REPORTS + staged codegen; it never modifies `libs/render3/src`.

pub mod conformance;
pub mod drift;
pub mod report;
pub mod symbol_map;
pub mod ts;
pub mod ts2rust;

pub use conformance::{
    BaselineComparison, ComplianceResult, ConformanceReport, Harness, OracleResult, RunError,
    compare_to_baseline, parse_compliance, parse_oracle, run_compliance, run_conformance,
    run_oracle,
};
pub use drift::{build_report, diff_file, fingerprint_exports, FileSnapshot, SymbolFingerprint};
pub use report::{
    ChangeKind, ChangedFile, ChangedSymbol, DriftReport, PortTask, TaskAction,
};
pub use symbol_map::{
    ModuleMapping, PortKind, ANGULAR_COMPILER_SRC_ROOT, MODULE_MAP, RENDER3_SRC_ROOT,
};
pub use ts2rust::{
    diff_against, emit_rust, CodegenReport, Emitted, EmittedKind, Unsupported,
};
