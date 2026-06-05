//! `backend-parity` — a deterministic (NO-AI) harness asserting Treaty's Ivy compiler emits
//! **byte-identical** Ivy across its parser/codegen backends (oxc vs swc), per
//! `migration/SWC-BACKEND-PLAN.md` §4.
//!
//! Mirrors `tools/render3-sync` in shape: a standalone Rust CLI crate (its own `[workspace]`) that
//! exits non-zero on drift/failure. The difference is WHAT it diffs — two backends of one compiler,
//! not Treaty vs Angular.
//!
//! Modules:
//!   * [`corpus`]   — the shared fixture corpus, ported from `parity.mjs`'s `FIXTURES`.
//!   * [`backend`]  — the [`Backend`](backend::Backend) trait + the real [`OxcBackend`](backend::oxc::OxcBackend)
//!                    and the feature-gated swc placeholder; [`enabled_backends`](backend::enabled_backends)
//!                    decides which run today.
//!   * [`report`]   — the serializable artifacts ([`ParityReport`](report::ParityReport),
//!                    [`Baseline`](report::Baseline), [`DriftReport`](report::DriftReport),
//!                    [`MigratePlan`](report::MigratePlan)).
//!   * [`parity`]   — the byte-equality gate, the baseline/drift tripwire, and the migrate-plan seed.

pub mod backend;
pub mod corpus;
pub mod parity;
pub mod report;

pub use backend::{enabled_backends, Backend};
pub use corpus::{Fixture, CORPUS};
pub use parity::{
    build_migrate_plan, describe_first_diff, diff_baseline, record_baseline, run_parity,
    REFERENCE_BACKEND,
};
pub use report::{
    Baseline, Difficulty, DriftEntry, DriftKind, DriftReport, FixtureParity, FixtureReport,
    MigratePlan, ParityReport, PortTask,
};
