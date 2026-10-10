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
    /// Rust port file(s), relative to `libs/treaty-ivy/`, resolved via the symbol->module map.
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
    /// The old Angular git ref (a commit SHA) the port is currently 1:1 with.
    pub old_ref: String,
    /// The new Angular git ref being compared against.
    pub new_ref: String,
    /// Per-file structured changes, restricted to the compiler subtrees the port covers.
    pub files: Vec<ChangedFile>,
    /// The derived, deduplicated hand-port task list (pillar 1's deliverable).
    pub tasks: Vec<PortTask>,
    /// TS files under the Angular compiler src root that are neither mapped in the symbol map nor
    /// listed as out of scope. Any entry makes the report unclean: classify the file to proceed.
    #[serde(default)]
    pub unclassified_files: Vec<String>,
}

impl DriftReport {
    /// True when nothing drifted: no changed mapped export, no unclassified compiler file, and no
    /// derived task. This is the expected result against the pinned Angular ref.
    pub fn is_clean(&self) -> bool {
        self.files.is_empty() && self.tasks.is_empty() && self.unclassified_files.is_empty()
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
    /// The symbol map needs a row: an Angular export or file that no mapping covers.
    UpdateMap,
}

/// One actionable unit of re-port work derived from the drift report. This is the deterministic
/// artifact a human (or a gated codegen step) consumes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortTask {
    /// Rust port file to touch, relative to `libs/treaty-ivy/` (empty for `UpdateMap` tasks).
    pub rust_file: String,
    /// Originating TS source file(s), relative to `tools/angular-ref/packages/compiler/src/`.
    pub ts_files: Vec<String>,
    /// The symbols driving this task (empty for an unclassified whole file).
    pub symbols: Vec<String>,
    /// What to do.
    pub action: TaskAction,
    /// Per-module port spec, if known.
    pub spec: Option<String>,
    /// Human-readable one-line summary.
    pub summary: String,
}

/// The `UpdateMap` task for a compiler file that is neither mapped nor out of scope.
pub fn unclassified_task(ts_file: &str) -> PortTask {
    PortTask {
        rust_file: String::new(),
        ts_files: vec![ts_file.to_string()],
        symbols: Vec::new(),
        action: TaskAction::UpdateMap,
        spec: None,
        summary: format!(
            "unclassified Angular file {ts_file}: add a symbol_map::MODULE_MAP row for it, or an \
             OUT_OF_SCOPE_FILES entry with a reason"
        ),
    }
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
            unclassified_files: vec!["brand/new.ts".into()],
        };

        let json = serde_json::to_string_pretty(&report).expect("serialize");
        let back: DriftReport = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(report, back);
    }

    #[test]
    fn report_without_unclassified_field_still_parses() {
        // Reports written before the field existed must still deserialize (serde default).
        let json = r#"{"old_ref":"a","new_ref":"b","files":[],"tasks":[]}"#;
        let report: DriftReport = serde_json::from_str(json).expect("deserialize old report");
        assert!(report.unclassified_files.is_empty());
        assert!(report.is_clean());
    }

    #[test]
    fn empty_report_is_clean() {
        assert!(DriftReport::default().is_clean());
    }

    #[test]
    fn unclassified_file_is_not_clean() {
        let report = DriftReport {
            unclassified_files: vec!["brand/new.ts".into()],
            tasks: vec![unclassified_task("brand/new.ts")],
            ..DriftReport::default()
        };
        assert!(
            !report.is_clean(),
            "an unclassified file must not read as clean"
        );
        assert_eq!(report.tasks[0].action, TaskAction::UpdateMap);
    }
}
