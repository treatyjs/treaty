//! Core data types of the harness: the serializable artifacts produced by the three
//! deterministic pillars (drift detection, conformance gate, mechanical codegen).
//!
//! These are intentionally plain, `serde`-serializable structs: the harness emits them as JSON
//! so a human (or a future deterministic step) acts on them. No AI is involved.

use serde::{Deserialize, Serialize};

use crate::symbol_map::PortKind;

/// What kind of change a diff observed for a given exported symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// A new export appeared in the Angular source.
    Added,
    /// An export was removed from the Angular source.
    Removed,
    /// An export's definition changed (signature, body, enum value, identifier string, ...).
    Modified,
}

/// A single exported TS symbol that drifted, attributed to the Rust module(s) that port it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedSymbol {
    /// Exported symbol name as it appears in the Angular TS (e.g. `compileComponentFromMetadata`).
    pub symbol: String,
    /// TS source file, relative to `tools/angular-ref/packages/compiler/src/`.
    pub ts_file: String,
    /// How the symbol changed.
    pub change: ChangeKind,
    /// Rust port file(s), relative to `libs/render3/src/`, resolved via the symbol->module map.
    /// Empty when the harness cannot attribute the symbol to any ported module (itself a
    /// reportable condition — an unmapped Angular export).
    pub rust_files: Vec<String>,
    /// Whether the owning module is a mechanical table (codegen candidate) or hand-ported logic.
    pub kind: Option<PortKind>,
}

/// A `git diff` of one TS source file between the old and new Angular ref.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFile {
    /// TS source file, relative to `tools/angular-ref/packages/compiler/src/`.
    pub ts_file: String,
    /// Whether the file is mapped in the symbol->module map at all.
    pub mapped: bool,
    /// The symbols within the file that changed.
    pub symbols: Vec<ChangedSymbol>,
}

/// Pillar 1 output: the structured "what diverged + which Rust file to touch" report produced
/// by diffing the vendored Angular `packages/compiler` between two refs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DriftReport {
    /// The old Angular git ref (e.g. a tag/sha) the port is currently 1:1 with.
    pub old_ref: String,
    /// The new Angular git ref being compared against.
    pub new_ref: String,
    /// Per-file structured changes, restricted to the compiler subtrees the port covers.
    pub files: Vec<ChangedFile>,
    /// The derived, deduplicated hand-port task list (pillar 1's deliverable).
    pub tasks: Vec<PortTask>,
}

impl DriftReport {
    /// True when no mapped symbol changed — i.e. the port is still 1:1 (the expected pillar-3
    /// verification result against the current pinned Angular).
    pub fn is_clean(&self) -> bool {
        self.files
            .iter()
            .all(|f| f.symbols.iter().all(|s| s.rust_files.is_empty() && !f.mapped))
            || (self.files.is_empty() && self.tasks.is_empty())
    }
}

/// How a [`PortTask`] should be resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskAction {
    /// Mechanical table drift: an oxc codegen can regenerate the Rust; diff it and auto-PR if it
    /// only changed values/names.
    Codegen,
    /// Hand-ported logic drift: a human must re-port; the task pins the exact file + symbols.
    ManualPort,
    /// An Angular export with no mapping — the map itself needs a new row.
    UpdateMap,
}

/// One actionable unit of re-port work derived from the drift report. This is the deterministic
/// artifact a human (or a gated codegen step) consumes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortTask {
    /// Rust port file to touch, relative to `libs/render3/src/` (empty for `UpdateMap` tasks).
    pub rust_file: String,
    /// Originating TS source file(s), relative to `tools/angular-ref/packages/compiler/src/`.
    pub ts_files: Vec<String>,
    /// The symbols driving this task.
    pub symbols: Vec<String>,
    /// What to do.
    pub action: TaskAction,
    /// Per-module port spec under `migration/render3-specs/`, if known.
    pub spec: Option<String>,
    /// Human-readable one-line summary.
    pub summary: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json() {
        let report = DriftReport {
            old_ref: "v22.1.0".into(),
            new_ref: "v22.2.0".into(),
            files: vec![ChangedFile {
                ts_file: "render3/r3_identifiers.ts".into(),
                mapped: true,
                symbols: vec![ChangedSymbol {
                    symbol: "Identifiers".into(),
                    ts_file: "render3/r3_identifiers.ts".into(),
                    change: ChangeKind::Modified,
                    rust_files: vec!["identifiers.rs".into()],
                    kind: Some(PortKind::Mechanical),
                }],
            }],
            tasks: vec![PortTask {
                rust_file: "identifiers.rs".into(),
                ts_files: vec!["render3/r3_identifiers.ts".into()],
                symbols: vec!["Identifiers".into()],
                action: TaskAction::Codegen,
                spec: Some("migration/render3-specs/14-identifiers.md".into()),
                summary: "regenerate identifier table".into(),
            }],
        };

        let json = serde_json::to_string_pretty(&report).expect("serialize");
        let back: DriftReport = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(report, back);
    }

    #[test]
    fn empty_report_is_clean() {
        assert!(DriftReport::default().is_clean());
    }
}
