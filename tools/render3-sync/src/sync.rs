//! Pillar 1 + pillar 3 **glue against the live tree**: the deterministic logic the CLI wires to
//! the real `tools/angular-ref` reference sources and the committed `libs/render3/src` Rust port.
//!
//! Three operations, all NO-AI and reproducible:
//!   * [`record_baseline`] — fingerprint every symbol-map reference TS file at the current
//!     `angular-ref` (a [`Baseline`] artifact, serialized to `baseline.json`).
//!   * [`diff_baseline`] — re-fingerprint the same files now and diff against a recorded
//!     [`Baseline`], producing the standard [`DriftReport`] (Added/Removed/Modified exports +
//!     derived [`PortTask`](crate::report::PortTask)s).
//!   * [`verify_codegen`] — for every `Mechanical` symbol-map row, run [`emit_rust`] over the
//!     vendored TS and [`diff_against`] the committed Rust, reporting in-sync vs drifted with a
//!     unified diff.
//!
//! File IO is abstracted behind [`SourceReader`] so the logic is unit-testable with an in-memory
//! map; the CLI injects [`FsReader`], a read-only real-filesystem reader rooted at the repo.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::drift::fingerprint_exports;
use crate::report::{ChangeKind, ChangedFile, ChangedSymbol, DriftReport, PortTask, TaskAction};
use crate::symbol_map::{self, PortKind, ANGULAR_COMPILER_SRC_ROOT, MODULE_MAP, RENDER3_SRC_ROOT};
use crate::ts2rust::{diff_against, emit_rust};

/// A read-only source provider. `ts(rel)` reads a vendored Angular source RELATIVE to
/// [`ANGULAR_COMPILER_SRC_ROOT`]; `rust(rel)` reads a committed Rust port RELATIVE to
/// [`RENDER3_SRC_ROOT`]. A missing file is `Ok(None)` (NOT an error) — a deleted reference is a
/// legitimate, reportable drift, not a harness failure.
pub trait SourceReader {
    /// Read a TS reference file relative to [`ANGULAR_COMPILER_SRC_ROOT`].
    fn ts(&self, rel: &str) -> std::io::Result<Option<String>>;
    /// Read a committed Rust port file relative to [`RENDER3_SRC_ROOT`].
    fn rust(&self, rel: &str) -> std::io::Result<Option<String>>;
}

/// A read-only [`SourceReader`] over the real filesystem, rooted at the Treaty repo. Resolves
/// `<root>/tools/angular-ref/packages/compiler/src/<rel>` and `<root>/libs/render3/src/<rel>`.
pub struct FsReader {
    root: std::path::PathBuf,
}

impl FsReader {
    /// Root at `repo_root` (the directory that contains `tools/` and `libs/`).
    pub fn new(repo_root: impl Into<std::path::PathBuf>) -> Self {
        Self { root: repo_root.into() }
    }

    fn read_opt(path: &std::path::Path) -> std::io::Result<Option<String>> {
        match std::fs::read_to_string(path) {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
}

impl SourceReader for FsReader {
    fn ts(&self, rel: &str) -> std::io::Result<Option<String>> {
        let path = self.root.join(ANGULAR_COMPILER_SRC_ROOT).join(rel);
        Self::read_opt(&path)
    }
    fn rust(&self, rel: &str) -> std::io::Result<Option<String>> {
        let path = self.root.join(RENDER3_SRC_ROOT).join(rel);
        Self::read_opt(&path)
    }
}

// ------------------------------------------------------------------------------------------------
// Baseline artifact (pillar 1).
// ------------------------------------------------------------------------------------------------

/// The recorded fingerprint of one reference TS file at a baseline ref: the set of its exported
/// symbols keyed by name, each carrying a deterministic body hash. (`exists: false` records that
/// the file was absent at baseline, so a later appearance reads as drift rather than noise.)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileBaseline {
    /// Whether the file existed at the baseline ref.
    pub exists: bool,
    /// Exported symbol name -> body hash, sorted (BTreeMap) for a stable, diffable artifact.
    pub symbols: BTreeMap<String, u64>,
}

/// The full baseline artifact written to `baseline.json`: a ref label plus a per-file fingerprint
/// for every symbol-map reference source. Recording and diffing are both keyed on the symbol map,
/// so adding a map row automatically extends the baseline surface on the next `baseline` run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Baseline {
    /// The Angular ref this baseline was recorded at (caller-chosen label: tag/sha/freeform).
    pub angular_ref: String,
    /// Per reference file (path relative to [`ANGULAR_COMPILER_SRC_ROOT`]) -> its fingerprint.
    pub files: BTreeMap<String, FileBaseline>,
}

/// The distinct set of reference TS files the symbol map covers, sorted & deduplicated. This is the
/// exact surface both `record_baseline` and `diff_baseline` operate over.
pub fn reference_ts_files() -> Vec<&'static str> {
    let mut files: Vec<&'static str> = MODULE_MAP.iter().map(|m| m.ts_file).collect();
    files.sort_unstable();
    files.dedup();
    files
}

/// Fingerprint every reference TS file at the current ref into a [`Baseline`].
///
/// `angular_ref` is recorded verbatim. Each file is read via `reader`; a missing file is recorded
/// as `exists: false` (not an error). Fully deterministic given the same sources.
pub fn record_baseline(
    reader: &dyn SourceReader,
    angular_ref: &str,
) -> std::io::Result<Baseline> {
    let mut files = BTreeMap::new();
    for ts_file in reference_ts_files() {
        let fb = match reader.ts(ts_file)? {
            Some(src) => {
                let symbols = fingerprint_exports(&src)
                    .into_iter()
                    .map(|(name, fp)| (name, fp.body_hash))
                    .collect();
                FileBaseline { exists: true, symbols }
            }
            None => FileBaseline { exists: false, symbols: BTreeMap::new() },
        };
        files.insert(ts_file.to_string(), fb);
    }
    Ok(Baseline { angular_ref: angular_ref.to_string(), files })
}

/// Diff the CURRENT reference sources (read via `reader`) against a recorded `baseline`, producing
/// a standard [`DriftReport`] (Added/Removed/Modified exports per file + derived port tasks).
///
/// `current_ref` labels the now-side. Every reference file is considered: a file present in the
/// baseline but the symbol map no longer references is simply not visited (the map is the surface).
/// A symbol present only now is `Added`, only at baseline is `Removed`, in both with a differing
/// hash is `Modified`; equal hashes are unchanged and omitted. Files with no change are dropped.
pub fn diff_baseline(
    reader: &dyn SourceReader,
    baseline: &Baseline,
    current_ref: &str,
) -> std::io::Result<DriftReport> {
    let mut changed_files: Vec<ChangedFile> = Vec::new();

    for ts_file in reference_ts_files() {
        let old = baseline
            .files
            .get(ts_file)
            .map(|fb| fb.symbols.clone())
            .unwrap_or_default();
        let new: BTreeMap<String, u64> = match reader.ts(ts_file)? {
            Some(src) => fingerprint_exports(&src)
                .into_iter()
                .map(|(name, fp)| (name, fp.body_hash))
                .collect(),
            None => BTreeMap::new(),
        };

        let symbols = diff_symbol_hashes(ts_file, &old, &new);
        if !symbols.is_empty() {
            let mapped = !symbol_map::rust_modules_for_ts(ts_file).is_empty();
            changed_files.push(ChangedFile { ts_file: ts_file.to_string(), mapped, symbols });
        }
    }

    changed_files.sort_by(|a, b| a.ts_file.cmp(&b.ts_file));
    let tasks = derive_tasks(&changed_files);
    Ok(DriftReport {
        old_ref: baseline.angular_ref.clone(),
        new_ref: current_ref.to_string(),
        files: changed_files,
        tasks,
    })
}

/// Diff one file's baseline vs current symbol-hash maps into attributed [`ChangedSymbol`]s.
fn diff_symbol_hashes(
    ts_file: &str,
    old: &BTreeMap<String, u64>,
    new: &BTreeMap<String, u64>,
) -> Vec<ChangedSymbol> {
    let mut names: Vec<&String> = old.keys().chain(new.keys()).collect();
    names.sort();
    names.dedup();

    let mut out = Vec::new();
    for name in names {
        let change = match (old.get(name), new.get(name)) {
            (None, Some(_)) => ChangeKind::Added,
            (Some(_), None) => ChangeKind::Removed,
            (Some(o), Some(n)) if o != n => ChangeKind::Modified,
            _ => continue,
        };
        out.push(attribute_symbol(name, ts_file, change));
    }
    out
}

/// Resolve the Rust owner(s) of a changed symbol — anchor mapping first, then the file-level
/// mapping, then unmapped (drives an `UpdateMap`). Mirrors [`crate::drift`]'s attribution so the
/// baseline-driven report is identical in shape to the snapshot-driven one.
fn attribute_symbol(symbol: &str, ts_file: &str, change: ChangeKind) -> ChangedSymbol {
    let mut owners = symbol_map::rust_modules_for_symbol(symbol);
    if owners.is_empty() {
        owners = symbol_map::rust_modules_for_ts(ts_file);
    }

    let mut rust_files: Vec<String> = owners.iter().map(|m| m.rust_file.to_string()).collect();
    rust_files.sort();
    rust_files.dedup();

    let kind = if owners.is_empty() {
        None
    } else if owners.iter().any(|m| m.kind == PortKind::Mechanical) {
        Some(PortKind::Mechanical)
    } else {
        Some(PortKind::Logic)
    };

    ChangedSymbol { symbol: symbol.to_string(), ts_file: ts_file.to_string(), change, rust_files, kind }
}

/// Group per-symbol changes into the deduplicated, actionable [`PortTask`] list: a `Codegen` task
/// per mechanical Rust owner (any contributing logic symbol downgrades it to `ManualPort`), and an
/// `UpdateMap` task per unmapped TS file. Sorted for determinism.
fn derive_tasks(files: &[ChangedFile]) -> Vec<PortTask> {
    struct Group {
        rust_file: String,
        ts_files: Vec<String>,
        symbols: Vec<String>,
        action: TaskAction,
        spec: Option<String>,
    }
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    let mut unmapped: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for file in files {
        for sym in &file.symbols {
            if sym.rust_files.is_empty() {
                let entry = unmapped.entry(sym.ts_file.clone()).or_default();
                push_unique(entry, sym.symbol.clone());
                continue;
            }
            for rust_file in &sym.rust_files {
                let mechanical = sym.kind == Some(PortKind::Mechanical);
                let g = groups.entry(rust_file.clone()).or_insert_with(|| Group {
                    rust_file: rust_file.clone(),
                    ts_files: Vec::new(),
                    symbols: Vec::new(),
                    action: if mechanical { TaskAction::Codegen } else { TaskAction::ManualPort },
                    spec: spec_for_rust_file(rust_file),
                });
                if !mechanical {
                    g.action = TaskAction::ManualPort;
                }
                push_unique(&mut g.ts_files, sym.ts_file.clone());
                push_unique(&mut g.symbols, sym.symbol.clone());
            }
        }
    }

    let mut tasks: Vec<PortTask> = Vec::new();
    for (_, mut g) in groups {
        g.ts_files.sort();
        g.symbols.sort();
        let summary = task_summary(&g.action, &g.rust_file, &g.symbols);
        tasks.push(PortTask {
            rust_file: g.rust_file,
            ts_files: g.ts_files,
            symbols: g.symbols,
            action: g.action,
            spec: g.spec,
            summary,
        });
    }
    for (ts_file, mut symbols) in unmapped {
        symbols.sort();
        let summary = format!(
            "unmapped Angular export(s) in {ts_file}: {} — add a symbol->module map row",
            symbols.join(", ")
        );
        tasks.push(PortTask {
            rust_file: String::new(),
            ts_files: vec![ts_file],
            symbols,
            action: TaskAction::UpdateMap,
            spec: None,
            summary,
        });
    }

    tasks.sort_by(|a, b| a.rust_file.cmp(&b.rust_file).then_with(|| a.ts_files.cmp(&b.ts_files)));
    tasks
}

fn spec_for_rust_file(rust_file: &str) -> Option<String> {
    MODULE_MAP
        .iter()
        .find(|m| m.rust_file == rust_file && m.spec.is_some())
        .and_then(|m| m.spec)
        .map(str::to_string)
}

fn task_summary(action: &TaskAction, rust_file: &str, symbols: &[String]) -> String {
    let syms = symbols.join(", ");
    match action {
        TaskAction::Codegen => {
            format!("regenerate mechanical table {rust_file} (oxc codegen + byte-diff): {syms}")
        }
        TaskAction::ManualPort => format!("re-port {rust_file} by hand — drifted symbols: {syms}"),
        TaskAction::UpdateMap => format!("add symbol->module map row(s) for: {syms}"),
    }
}

fn push_unique(vec: &mut Vec<String>, value: String) {
    if !vec.contains(&value) {
        vec.push(value);
    }
}

// ------------------------------------------------------------------------------------------------
// Codegen verification (pillar 3 against the live tree).
// ------------------------------------------------------------------------------------------------

/// Whether a verified mechanical row is byte-for-byte in sync with its committed Rust, or drifted
/// (with a unified diff), or could not be checked (a missing source — recorded, never guessed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum VerifyStatus {
    /// Emitted Rust matches the committed Rust for the verified symbol (still 1:1).
    InSync,
    /// Emitted Rust differs from the committed Rust; carries the unified diff.
    Drifted {
        /// Unified `--- emitted / +++ committed` diff from [`diff_against`].
        diff: String,
    },
    /// A required source (TS reference or committed Rust port) was absent.
    Missing {
        /// Which source was missing and where it was looked for.
        reason: String,
    },
    /// The emitter refused the TS as non-mechanical (e.g. the row's kind is stale).
    Unsupported {
        /// The refusal reason(s) recorded by the emitter.
        reason: String,
    },
}

/// One verified mechanical symbol-map row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyEntry {
    /// TS reference file, relative to [`ANGULAR_COMPILER_SRC_ROOT`].
    pub ts_file: String,
    /// Committed Rust port file, relative to [`RENDER3_SRC_ROOT`].
    pub rust_file: String,
    /// The outcome.
    pub status: VerifyStatus,
}

impl VerifyEntry {
    /// True when this row is byte-for-byte in sync.
    pub fn is_in_sync(&self) -> bool {
        matches!(self.status, VerifyStatus::InSync)
    }
}

/// The result of verifying every mechanical row against the committed Rust.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct VerifyReport {
    /// One entry per `Mechanical` symbol-map row, in symbol-map order.
    pub entries: Vec<VerifyEntry>,
}

impl VerifyReport {
    /// True when every verified row is in sync (no drift / missing / unsupported).
    pub fn is_all_in_sync(&self) -> bool {
        self.entries.iter().all(VerifyEntry::is_in_sync)
    }

    /// The rows that are NOT in sync (the actionable surface).
    pub fn out_of_sync(&self) -> Vec<&VerifyEntry> {
        self.entries.iter().filter(|e| !e.is_in_sync()).collect()
    }
}

/// For every [`PortKind::Mechanical`] symbol-map row, emit Rust from the vendored TS and diff it
/// against the committed Rust port, returning a [`VerifyReport`].
///
/// The committed Rust modules contain hand-written prose + non-mechanical items, so a raw
/// whole-file byte diff would always "drift". Instead, each emitted mechanical item is matched to
/// the committed file by extracting the corresponding committed item (the `pub enum NAME {…}` /
/// `pub const NAME: … = &[…];` block) and diffing only that block. A row whose committed file does
/// not contain the emitted item is reported as drifted (the table is missing/renamed), with the
/// emitted Rust shown as the expected text.
pub fn verify_codegen(reader: &dyn SourceReader) -> std::io::Result<VerifyReport> {
    let mut entries = Vec::new();

    for m in MODULE_MAP {
        if m.kind != PortKind::Mechanical {
            continue;
        }

        let ts_src = match reader.ts(m.ts_file)? {
            Some(s) => s,
            None => {
                entries.push(VerifyEntry {
                    ts_file: m.ts_file.to_string(),
                    rust_file: m.rust_file.to_string(),
                    status: VerifyStatus::Missing {
                        reason: format!("vendored TS reference {} not found", m.ts_file),
                    },
                });
                continue;
            }
        };
        let rust_src = match reader.rust(m.rust_file)? {
            Some(s) => s,
            None => {
                entries.push(VerifyEntry {
                    ts_file: m.ts_file.to_string(),
                    rust_file: m.rust_file.to_string(),
                    status: VerifyStatus::Missing {
                        reason: format!("committed Rust port {} not found", m.rust_file),
                    },
                });
                continue;
            }
        };

        let codegen = emit_rust(&ts_src);
        if codegen.emitted.is_empty() {
            let reason = codegen
                .unsupported
                .iter()
                .map(|u| format!("{}: {}", u.name, u.reason))
                .collect::<Vec<_>>()
                .join("; ");
            entries.push(VerifyEntry {
                ts_file: m.ts_file.to_string(),
                rust_file: m.rust_file.to_string(),
                status: VerifyStatus::Unsupported {
                    reason: if reason.is_empty() {
                        "emitter produced no mechanical items".to_string()
                    } else {
                        reason
                    },
                },
            });
            continue;
        }

        entries.push(verify_one(m.ts_file, m.rust_file, &codegen, &rust_src));
    }

    Ok(VerifyReport { entries })
}

/// Verify a single row's emitted items against the committed Rust. Each emitted item is located in
/// the committed source by its item header and the matched block is byte-diffed; the per-item
/// diffs are concatenated. In sync iff EVERY emitted item matched.
fn verify_one(
    ts_file: &str,
    rust_file: &str,
    codegen: &crate::ts2rust::CodegenReport,
    committed: &str,
) -> VerifyEntry {
    let mut diffs = String::new();
    for item in &codegen.emitted {
        let header = item_header(&item.rust);
        // Compare the DECLARATION block (header line onward) on both sides: the emitter prepends a
        // doc-comment the committed file need not reproduce verbatim, so anchoring both on the
        // `pub enum`/`pub const` line is what makes the byte-diff meaningful.
        let emitted_block = declaration_block(&item.rust, &header);
        match extract_item(committed, &header) {
            Some(block) => {
                if let Err(diff) = diff_against(&emitted_block, &block) {
                    diffs.push_str(&format!("# item `{}` ({:?})\n", item.name, item.kind));
                    diffs.push_str(&diff);
                    diffs.push('\n');
                }
            }
            None => {
                diffs.push_str(&format!(
                    "# item `{}` ({:?}) not found in committed {rust_file} (header: `{header}`)\n",
                    item.name, item.kind
                ));
                diffs.push_str("--- emitted\n+++ committed (absent)\n");
                for line in emitted_block.lines() {
                    diffs.push_str(&format!("-{line}\n"));
                }
                diffs.push('\n');
            }
        }
    }

    let status = if diffs.is_empty() {
        VerifyStatus::InSync
    } else {
        VerifyStatus::Drifted { diff: diffs.trim_end().to_string() }
    };
    VerifyEntry { ts_file: ts_file.to_string(), rust_file: rust_file.to_string(), status }
}

/// The first line of an emitted item that uniquely anchors it in the committed file: the `pub enum
/// X {` / `pub const X: … = &[` declaration line (skipping any leading doc-comment lines the
/// emitter prepends).
fn item_header(emitted: &str) -> String {
    emitted
        .lines()
        .find(|l| {
            let t = l.trim_start();
            t.starts_with("pub enum ") || t.starts_with("pub const ")
        })
        .map(|l| l.trim_start().to_string())
        .unwrap_or_default()
}

/// Slice an emitted item from its `header` line (or `#[repr...]`) onward, dropping any leading
/// doc-comment the emitter prepends. This is the half that is compared against the committed
/// declaration block, so the comparison is over the declaration itself, not the generated prose.
fn declaration_block(emitted: &str, header: &str) -> String {
    if header.is_empty() {
        return emitted.to_string();
    }
    let lines: Vec<&str> = emitted.lines().collect();
    match lines.iter().position(|l| l.trim_start() == header) {
        Some(start) => lines[start..].join("\n"),
        None => emitted.to_string(),
    }
}

/// Extract the committed item block that begins at the line whose trimmed text equals `header`,
/// through its terminating `}` (enum) or `];` (const table). Returns the block verbatim (preserving
/// the committed indentation that begins each captured line) or `None` if `header` is absent.
///
/// The committed file may carry attributes/derives/doc-comments around the item; we anchor on the
/// declaration line itself (`pub enum X {` or `pub const X: … = &[`) and capture to its closer, so
/// surrounding prose does not defeat the match. Comparison reuses [`diff_against`], which is line-
/// based and trims a trailing newline, so leading indentation differences DO register (the emitted
/// items are column-0, and so are the committed table items in `libs/render3`).
fn extract_item(committed: &str, header: &str) -> Option<String> {
    if header.is_empty() {
        return None;
    }
    let lines: Vec<&str> = committed.lines().collect();
    let start = lines.iter().position(|l| l.trim_start() == header)?;

    // Determine the closer for the kind of declaration.
    let decl = lines[start].trim_start();
    let (open, close): (char, &str) = if decl.starts_with("pub enum ") || decl.ends_with('{') {
        ('{', "}")
    } else {
        ('[', "];")
    };

    let mut depth = 0i32;
    let mut end = start;
    for (i, line) in lines.iter().enumerate().skip(start) {
        for ch in line.chars() {
            if ch == open {
                depth += 1;
            } else if (open == '{' && ch == '}') || (open == '[' && ch == ']') {
                depth -= 1;
            }
        }
        let trimmed = line.trim_end();
        if depth <= 0 && (trimmed.ends_with(close) || trimmed.ends_with(if open == '{' { "}" } else { "];" })) && i >= start {
            end = i;
            break;
        }
        end = i;
    }

    Some(lines[start..=end].join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An in-memory [`SourceReader`] for deterministic tests.
    #[derive(Default)]
    struct MemReader {
        ts: BTreeMap<String, String>,
        rust: BTreeMap<String, String>,
    }
    impl MemReader {
        fn with_ts(mut self, rel: &str, src: &str) -> Self {
            self.ts.insert(rel.to_string(), src.to_string());
            self
        }
        fn with_rust(mut self, rel: &str, src: &str) -> Self {
            self.rust.insert(rel.to_string(), src.to_string());
            self
        }
    }
    impl SourceReader for MemReader {
        fn ts(&self, rel: &str) -> std::io::Result<Option<String>> {
            Ok(self.ts.get(rel).cloned())
        }
        fn rust(&self, rel: &str) -> std::io::Result<Option<String>> {
            Ok(self.rust.get(rel).cloned())
        }
    }

    // A real symbol-map mechanical row we can drive: render3/r3_identifiers.ts -> identifiers.rs.
    const IDENTIFIERS_TS: &str = "export class Identifiers {
        static element = {name: 'ɵɵelement', moduleName: CORE};
        static elementStart = {name: 'ɵɵelementStart', moduleName: CORE};
    }";

    #[test]
    fn baseline_round_trips_through_json() {
        let reader = MemReader::default()
            .with_ts("render3/r3_identifiers.ts", IDENTIFIERS_TS);
        let baseline = record_baseline(&reader, "v22.0.0").unwrap();

        assert_eq!(baseline.angular_ref, "v22.0.0");
        // Every symbol-map reference file is present as a key (absent ones recorded exists:false).
        assert_eq!(baseline.files.len(), reference_ts_files().len());
        let idents = &baseline.files["render3/r3_identifiers.ts"];
        assert!(idents.exists);
        assert!(idents.symbols.contains_key("Identifiers"));
        // A file the reader did not provide is recorded as absent, not dropped.
        let absent = &baseline.files["core.ts"];
        assert!(!absent.exists);

        let json = serde_json::to_string_pretty(&baseline).expect("serialize");
        let back: Baseline = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(baseline, back);
    }

    #[test]
    fn diff_baseline_clean_when_unchanged() {
        let reader = MemReader::default().with_ts("render3/r3_identifiers.ts", IDENTIFIERS_TS);
        let baseline = record_baseline(&reader, "v22.0.0").unwrap();
        // Same sources -> no drift.
        let report = diff_baseline(&reader, &baseline, "v22.0.0").unwrap();
        assert!(report.is_clean(), "no change must be clean: {report:?}");
        assert!(report.files.is_empty());
        assert!(report.tasks.is_empty());
    }

    #[test]
    fn diff_baseline_flags_a_changed_fingerprint() {
        let reader = MemReader::default().with_ts("render3/r3_identifiers.ts", IDENTIFIERS_TS);
        let baseline = record_baseline(&reader, "v22.0.0").unwrap();

        // Now the reference adds a NEW ɵɵ row and CHANGES an existing one's body.
        let changed = "export class Identifiers {
            static element = {name: 'ɵɵelementRENAMED', moduleName: CORE};
            static elementStart = {name: 'ɵɵelementStart', moduleName: CORE};
            static elementEnd = {name: 'ɵɵelementEnd', moduleName: CORE};
        }";
        let now = MemReader::default().with_ts("render3/r3_identifiers.ts", changed);

        let report = diff_baseline(&now, &baseline, "v22.1.0").unwrap();
        assert!(!report.is_clean(), "a body change must register as drift");
        assert_eq!(report.old_ref, "v22.0.0");
        assert_eq!(report.new_ref, "v22.1.0");

        // One changed file; `Identifiers` (the class span) is Modified.
        assert_eq!(report.files.len(), 1);
        let cf = &report.files[0];
        assert_eq!(cf.ts_file, "render3/r3_identifiers.ts");
        let modified = cf.symbols.iter().find(|s| s.symbol == "Identifiers").unwrap();
        assert_eq!(modified.change, ChangeKind::Modified);

        // It maps to the identifiers.rs Mechanical table -> a single Codegen task with its spec.
        assert_eq!(report.tasks.len(), 1);
        let task = &report.tasks[0];
        assert_eq!(task.rust_file, "identifiers.rs");
        assert_eq!(task.action, TaskAction::Codegen);
        assert_eq!(task.spec.as_deref(), Some("migration/render3-specs/14-identifiers.md"));
    }

    #[test]
    fn diff_baseline_reports_removed_export() {
        let reader = MemReader::default().with_ts("render3/r3_identifiers.ts", IDENTIFIERS_TS);
        let baseline = record_baseline(&reader, "v22.0.0").unwrap();
        // The file vanished entirely at the new ref.
        let now = MemReader::default();
        let report = diff_baseline(&now, &baseline, "v22.1.0").unwrap();
        let cf = report.files.iter().find(|f| f.ts_file == "render3/r3_identifiers.ts").unwrap();
        let removed = cf.symbols.iter().find(|s| s.symbol == "Identifiers").unwrap();
        assert_eq!(removed.change, ChangeKind::Removed);
    }

    #[test]
    fn verify_codegen_in_sync_when_committed_matches_emitted() {
        // Emit the identifiers table to learn its exact committed shape, then feed that verbatim
        // back as the committed Rust -> in sync.
        let emitted = emit_rust(IDENTIFIERS_TS);
        let committed_block = &emitted.emitted[0].rust;
        let committed_file = format!(
            "//! identifiers port.\n\nuse crate::x;\n\n{committed_block}\n\nfn other() {{}}\n"
        );

        let reader = MemReader::default()
            .with_ts("render3/r3_identifiers.ts", IDENTIFIERS_TS)
            .with_rust("identifiers.rs", &committed_file);

        let report = verify_codegen(&reader).unwrap();
        let entry = report
            .entries
            .iter()
            .find(|e| e.ts_file == "render3/r3_identifiers.ts")
            .expect("identifiers row verified");
        assert!(entry.is_in_sync(), "expected in-sync, got: {:?}", entry.status);
    }

    #[test]
    fn verify_codegen_detects_an_intentional_mismatch() {
        let emitted = emit_rust(IDENTIFIERS_TS);
        let committed_block = &emitted.emitted[0].rust;
        // Corrupt one wire name in the committed copy — a real, intentional drift.
        let drifted_block = committed_block.replace("ɵɵelementStart", "ɵɵWRONG");
        let committed_file = format!("//! identifiers port.\n\n{drifted_block}\n");

        let reader = MemReader::default()
            .with_ts("render3/r3_identifiers.ts", IDENTIFIERS_TS)
            .with_rust("identifiers.rs", &committed_file);

        let report = verify_codegen(&reader).unwrap();
        assert!(!report.is_all_in_sync(), "drift must be detected");
        let entry = report
            .entries
            .iter()
            .find(|e| e.ts_file == "render3/r3_identifiers.ts")
            .unwrap();
        match &entry.status {
            VerifyStatus::Drifted { diff } => {
                assert!(diff.contains("ɵɵelementStart"), "diff should name removed value: {diff}");
                assert!(diff.contains("ɵɵWRONG"), "diff should name added value: {diff}");
            }
            other => panic!("expected Drifted, got {other:?}"),
        }
    }

    #[test]
    fn verify_codegen_missing_committed_rust_is_reported() {
        // TS present, committed Rust absent.
        let reader = MemReader::default().with_ts("render3/r3_identifiers.ts", IDENTIFIERS_TS);
        let report = verify_codegen(&reader).unwrap();
        let entry = report
            .entries
            .iter()
            .find(|e| e.ts_file == "render3/r3_identifiers.ts")
            .unwrap();
        assert!(matches!(entry.status, VerifyStatus::Missing { .. }), "{:?}", entry.status);
    }

    #[test]
    fn extract_item_pulls_enum_block() {
        let committed = "//! doc\n\n#[repr(i64)]\npub enum AttributeMarker {\n    Classes = 1,\n    Styles = 2,\n}\n\nfn after() {}\n";
        let block = extract_item(committed, "pub enum AttributeMarker {").unwrap();
        assert!(block.contains("Classes = 1,"));
        assert!(block.contains("Styles = 2,"));
        assert!(block.trim_end().ends_with('}'));
        assert!(!block.contains("fn after"));
    }
}
