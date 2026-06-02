//! Serializable artifacts produced by the deterministic backend-parity operations.
//!
//! Plain `serde` structs, emitted as JSON so a human (or a future deterministic CI step) acts on
//! them. No AI. Mirrors the spirit of `tools/render3-sync`'s `report.rs`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

// ------------------------------------------------------------------------------------------------
// `parity` — pairwise byte-equality across the enabled backends.
// ------------------------------------------------------------------------------------------------

/// The outcome of running every enabled backend on one fixture and comparing their outputs pairwise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "result")]
pub enum FixtureParity {
    /// Every enabled backend produced byte-identical output (or only one backend is enabled, which
    /// is trivially in parity with itself — the Phase-1 oxc-only state).
    Ok {
        /// The backends that agreed, in enabled order.
        backends: Vec<String>,
    },
    /// At least two backends produced differing output, or a backend errored.
    Diff {
        /// Per-backend detail: name -> emitted output (or an `ERROR: …` marker if it failed).
        outputs: BTreeMap<String, String>,
        /// A short human-readable description of the first divergence found.
        detail: String,
    },
}

impl FixtureParity {
    /// True when this fixture is in parity across all enabled backends.
    pub fn is_ok(&self) -> bool {
        matches!(self, FixtureParity::Ok { .. })
    }
}

/// One fixture's parity outcome, keyed by fixture id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixtureReport {
    /// The fixture id (matches `parity.mjs`).
    pub id: String,
    /// The pairwise comparison result.
    pub parity: FixtureParity,
}

/// The full parity report over the corpus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ParityReport {
    /// The backends that were enabled for this run, in order.
    pub backends: Vec<String>,
    /// Per-fixture outcome, in corpus order.
    pub fixtures: Vec<FixtureReport>,
}

impl ParityReport {
    /// True when every fixture is in parity (the green gate state).
    pub fn is_all_ok(&self) -> bool {
        self.fixtures.iter().all(|f| f.parity.is_ok())
    }

    /// The fixtures that diverged (the actionable surface).
    pub fn diffs(&self) -> Vec<&FixtureReport> {
        self.fixtures.iter().filter(|f| !f.parity.is_ok()).collect()
    }
}

// ------------------------------------------------------------------------------------------------
// `baseline` / `drift` — the oxc-output tripwire (a regression / future-change detector).
// ------------------------------------------------------------------------------------------------

/// The recorded oxc-emitted Ivy for the whole corpus: `fixture_id -> emitted_ivy_string`, BTreeMap
/// for a stable, diffable, committable artifact (the tripwire, like `render3-sync/baseline.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Baseline {
    /// The backend whose output was recorded (always the reference backend, `"oxc"`).
    pub backend: String,
    /// `fixture_id -> emitted Ivy text`, sorted.
    pub outputs: BTreeMap<String, String>,
}

/// How one fixture's current oxc output compares to the recorded baseline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "change")]
pub enum DriftKind {
    /// A fixture present now but absent in the baseline (corpus grew — re-record the baseline).
    Added,
    /// A fixture present in the baseline but absent now (corpus shrank — re-record the baseline).
    Removed,
    /// A fixture whose current output differs from the baseline (a real output regression/change).
    Modified {
        /// A short description of the first divergence (index + surrounding context).
        detail: String,
    },
}

/// One fixture that drifted from the baseline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriftEntry {
    /// The fixture id.
    pub id: String,
    /// What changed.
    pub change: DriftKind,
}

/// The result of diffing current oxc output against the recorded baseline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DriftReport {
    /// The backend the baseline recorded (echoed for context).
    pub backend: String,
    /// The drifted fixtures (empty == clean).
    pub entries: Vec<DriftEntry>,
}

impl DriftReport {
    /// True when nothing drifted from the baseline.
    pub fn is_clean(&self) -> bool {
        self.entries.is_empty()
    }
}

// ------------------------------------------------------------------------------------------------
// `migrate-plan` — structured port tasks seeding the SWC backend.
// ------------------------------------------------------------------------------------------------

/// Difficulty of one oxc→swc port row, mirroring SWC-BACKEND-PLAN.md §2's column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Difficulty {
    /// Mechanical rename.
    Trivial,
    /// Real code, but contained.
    Moderate,
    /// An architectural gap needing a deliberate adapter.
    Hard,
}

/// One row of the embedded oxc→swc API mapping: what the SWC backend must implement to match oxc on
/// a given concern. Seeded from SWC-BACKEND-PLAN.md §2's mapping table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortTask {
    /// The compiler concern being ported (e.g. `"Parse"`, `"AST builder"`, `"Codegen"`).
    pub concern: String,
    /// What oxc does today.
    pub oxc: String,
    /// The SWC equivalent the backend must implement.
    pub swc: String,
    /// Estimated difficulty.
    pub difficulty: Difficulty,
    /// Where it bites / notes (the plan's "Notes" column, trimmed).
    pub notes: String,
}

/// The structured migration plan emitted by `migrate-plan`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MigratePlan {
    /// Reference to the authoritative plan document.
    pub source: String,
    /// The seed port tasks (a small embedded subset of the §2 table).
    pub tasks: Vec<PortTask>,
}
