//! Pillar 1 + pillar 3 **glue against the live tree**: the deterministic logic the CLI wires to
//! the real `tools/angular-ref` reference sources and the committed `libs/treaty-ivy/*` Rust port.
//!
//! Three operations, all NO-AI and reproducible:
//!   * [`record_baseline`] — fingerprint every symbol-map reference TS file at a pinned Angular ref
//!     (a [`Baseline`] artifact, serialized to `baseline.json`).
//!   * [`diff_baseline`] — re-fingerprint the same files now and diff against a recorded
//!     [`Baseline`], producing the standard [`DriftReport`] (Added/Removed/Modified exports, derived
//!     [`PortTask`](crate::report::PortTask)s, and every compiler file that is neither mapped nor
//!     out of scope).
//!   * [`verify_codegen`] — for every `Mechanical` symbol-map row, emit Rust from the vendored TS
//!     and compare it with the committed Rust, reporting in-sync, drifted, or pending.
//!
//! Both baseline and drift (and codegen-verify) first require the reference root to exist and to
//! contain `.ts` files ([`require_reference_root`]); a missing or empty root is a loud error, never
//! a silent "everything was removed" diff.
//!
//! File IO is abstracted behind [`SourceReader`] so the logic is unit-testable with an in-memory
//! map; the CLI injects [`FsReader`], a read-only real-filesystem reader rooted at the repo.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::drift::fingerprint_exports;
use crate::report::{ChangeKind, ChangedFile, ChangedSymbol, DriftReport, PortTask, TaskAction};
use crate::symbol_map::{
    self, ANGULAR_COMPILER_SRC_ROOT, FileClass, MODULE_MAP, PortKind, TREATY_IVY_SRC_ROOT,
};
use crate::ts2rust::{CodegenReport, Emitted, EmittedKind, diff_against, emit_rust};

/// A read-only source provider. `ts(rel)` reads a vendored Angular source RELATIVE to
/// [`ANGULAR_COMPILER_SRC_ROOT`]; `rust(rel)` reads a committed Rust port RELATIVE to
/// [`TREATY_IVY_SRC_ROOT`]. A missing file is `Ok(None)` (NOT an error) — a deleted reference is a
/// legitimate, reportable drift, not a harness failure. A missing reference ROOT is an error, see
/// [`require_reference_root`].
pub trait SourceReader {
    /// Read a TS reference file relative to [`ANGULAR_COMPILER_SRC_ROOT`].
    fn ts(&self, rel: &str) -> io::Result<Option<String>>;
    /// Read a committed Rust port file relative to [`TREATY_IVY_SRC_ROOT`].
    fn rust(&self, rel: &str) -> io::Result<Option<String>>;
    /// Every `.ts` file under the reference root, relative to [`ANGULAR_COMPILER_SRC_ROOT`], sorted.
    /// `Err(NotFound)` when the root directory does not exist.
    fn list_ts(&self) -> io::Result<Vec<String>>;
    /// Where the reference root lives, for error messages.
    fn ts_root_label(&self) -> String;
}

/// A read-only [`SourceReader`] over the real filesystem, rooted at the Treaty repo. Resolves
/// `<root>/tools/angular-ref/packages/compiler/src/<rel>` and `<root>/libs/treaty-ivy/<rel>`
/// (each `rel` already carries its crate's `src/` prefix, e.g. `core/src/identifiers.rs`).
pub struct FsReader {
    root: PathBuf,
}

impl FsReader {
    /// Root at `repo_root` (the directory that contains `tools/` and `libs/`).
    pub fn new(repo_root: impl Into<PathBuf>) -> Self {
        Self {
            root: repo_root.into(),
        }
    }

    fn ts_root(&self) -> PathBuf {
        self.root.join(ANGULAR_COMPILER_SRC_ROOT)
    }

    fn read_opt(path: &Path) -> io::Result<Option<String>> {
        match std::fs::read_to_string(path) {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
}

impl SourceReader for FsReader {
    fn ts(&self, rel: &str) -> io::Result<Option<String>> {
        Self::read_opt(&self.ts_root().join(rel))
    }
    fn rust(&self, rel: &str) -> io::Result<Option<String>> {
        Self::read_opt(&self.root.join(TREATY_IVY_SRC_ROOT).join(rel))
    }
    fn list_ts(&self) -> io::Result<Vec<String>> {
        list_ts_files(&self.ts_root())
    }
    fn ts_root_label(&self) -> String {
        self.ts_root().display().to_string()
    }
}

/// Recursively list the `.ts` files under `dir`, as `/`-separated paths relative to `dir`, sorted.
/// Returns `Err(NotFound)` when `dir` does not exist (the caller turns that into a clear message).
pub fn list_ts_files(dir: &Path) -> io::Result<Vec<String>> {
    if !dir.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} is not a directory", dir.display()),
        ));
    }
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d)? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "ts") {
                let rel = path.strip_prefix(dir).unwrap_or(&path);
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Require the reference root to exist and to hold at least one `.ts` file, returning the listing.
/// A missing or empty root (e.g. `tools/angular-ref` never populated) is an error with the exact
/// fix, so `baseline` and `drift` can never report a silent full removal.
pub fn require_reference_root(reader: &dyn SourceReader) -> io::Result<Vec<String>> {
    let label = reader.ts_root_label();
    let fix = "populate it with tools/render3-sync/populate-angular-ref.sh";
    match reader.list_ts() {
        Err(e) => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("Angular reference root {label} is missing or unreadable ({e}); {fix}"),
        )),
        Ok(files) if files.is_empty() => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("Angular reference root {label} contains no .ts files; {fix}"),
        )),
        Ok(files) => Ok(files),
    }
}

// ------------------------------------------------------------------------------------------------
// Baseline artifact (pillar 1).
// ------------------------------------------------------------------------------------------------

/// The recorded fingerprint of one reference TS file at a baseline ref: the set of its exported
/// symbols keyed by name, each carrying a deterministic body hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileBaseline {
    /// Whether the file existed at the baseline ref. Always `true` in a baseline recorded by this
    /// version (recording fails loudly for a missing mapped file); kept so the artifact format
    /// stays readable.
    pub exists: bool,
    /// Exported symbol name -> body hash, sorted (BTreeMap) for a stable, diffable artifact.
    pub symbols: BTreeMap<String, u64>,
}

/// The full baseline artifact written to `baseline.json`: the pinned Angular commit SHA plus a
/// per-file fingerprint for every symbol-map reference source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Baseline {
    /// The Angular commit SHA this baseline was recorded at (a full git SHA; see `ANGULAR_REF`).
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

/// Fingerprint every reference TS file at the pinned ref into a [`Baseline`].
///
/// `angular_ref` is recorded verbatim (the commit SHA). Fails loudly if the reference root is
/// missing or empty, or if any mapped file is absent from it: a baseline must be complete.
pub fn record_baseline(reader: &dyn SourceReader, angular_ref: &str) -> io::Result<Baseline> {
    require_reference_root(reader)?;
    let mut files = BTreeMap::new();
    let mut missing = Vec::new();
    for ts_file in reference_ts_files() {
        match reader.ts(ts_file)? {
            Some(src) => {
                let symbols = fingerprint_exports(&src)
                    .into_iter()
                    .map(|(name, fp)| (name, fp.body_hash))
                    .collect();
                files.insert(
                    ts_file.to_string(),
                    FileBaseline {
                        exists: true,
                        symbols,
                    },
                );
            }
            None => missing.push(ts_file),
        }
    }
    if !missing.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "mapped Angular file(s) missing from {}: {}; the reference is not the pinned ref",
                reader.ts_root_label(),
                missing.join(", ")
            ),
        ));
    }
    Ok(Baseline {
        angular_ref: angular_ref.to_string(),
        files,
    })
}

/// Diff the CURRENT reference sources (read via `reader`) against a recorded `baseline`, producing
/// a standard [`DriftReport`]: Added/Removed/Modified exports per mapped file, derived port tasks,
/// and every compiler file that is neither mapped nor out of scope (`unclassified_files`).
///
/// `current_ref` labels the now-side. A symbol present only now is `Added`, only at baseline is
/// `Removed`, in both with a differing hash is `Modified`; equal hashes are unchanged and omitted.
/// Exports listed in [`symbol_map::OUT_OF_SCOPE_SYMBOLS`] are ignored entirely.
pub fn diff_baseline(
    reader: &dyn SourceReader,
    baseline: &Baseline,
    current_ref: &str,
) -> io::Result<DriftReport> {
    let all_files = require_reference_root(reader)?;
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
            changed_files.push(ChangedFile {
                ts_file: ts_file.to_string(),
                mapped,
                symbols,
            });
        }
    }

    let unclassified_files: Vec<String> = all_files
        .into_iter()
        .filter(|f| symbol_map::classify_ts_file(f) == FileClass::Unclassified)
        .collect();

    changed_files.sort_by(|a, b| a.ts_file.cmp(&b.ts_file));
    let mut tasks = derive_tasks(&changed_files);
    tasks.extend(
        unclassified_files
            .iter()
            .map(|f| crate::report::unclassified_task(f)),
    );
    tasks.sort_by(|a, b| {
        a.rust_file
            .cmp(&b.rust_file)
            .then_with(|| a.ts_files.cmp(&b.ts_files))
    });
    Ok(DriftReport {
        old_ref: baseline.angular_ref.clone(),
        new_ref: current_ref.to_string(),
        files: changed_files,
        tasks,
        unclassified_files,
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
        if symbol_map::out_of_scope_symbol(ts_file, name).is_some() {
            continue;
        }
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

/// Resolve the Rust owner(s) of a changed symbol (file-scoped anchors first, then the file-level
/// rows, then unmapped). Mirrors [`crate::drift`]'s attribution so the baseline-driven report has
/// the same shape as the snapshot-driven one.
fn attribute_symbol(symbol: &str, ts_file: &str, change: ChangeKind) -> ChangedSymbol {
    let owners = symbol_map::owners_for_export(ts_file, symbol);

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

    ChangedSymbol {
        symbol: symbol.to_string(),
        ts_file: ts_file.to_string(),
        change,
        rust_files,
        kind,
    }
}

/// Group per-symbol changes into the deduplicated, actionable [`PortTask`] list: a `Codegen` task
/// per mechanical Rust owner (any contributing logic symbol downgrades it to `ManualPort`), and an
/// `UpdateMap` task per unmapped export. Sorted for determinism.
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
                    action: if mechanical {
                        TaskAction::Codegen
                    } else {
                        TaskAction::ManualPort
                    },
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

    tasks.sort_by(|a, b| {
        a.rust_file
            .cmp(&b.rust_file)
            .then_with(|| a.ts_files.cmp(&b.ts_files))
    });
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

/// The outcome of verifying one mechanical symbol-map row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum VerifyStatus {
    /// Emitted Rust matches the committed Rust for every verified symbol (still 1:1).
    InSync,
    /// Emitted Rust differs from the committed Rust; carries the unified diff.
    Drifted {
        /// Unified `--- emitted / +++ committed` diff, one section per differing declaration.
        diff: String,
    },
    /// A required source (TS reference or committed Rust port) was absent.
    Missing {
        /// Which source was missing and where it was looked for.
        reason: String,
    },
    /// The emitter refused a symbol the row claims (outside the mechanical subset).
    Unsupported {
        /// The refusal reason(s) recorded by the emitter.
        reason: String,
    },
    /// The row is marked `pending` and does not verify yet, for the documented reason. Not blocking.
    Pending {
        /// The row's `pending` reason (why the Rust file is not in the required state yet).
        reason: String,
        /// What does not match yet (the diff, or the missing-file message).
        detail: String,
    },
    /// The row is marked `pending` but now verifies in sync: the marker is stale and must be
    /// removed from `symbol_map.rs`. Blocking, so the marker cannot outlive the port.
    StalePending {
        /// The pending reason that no longer applies.
        reason: String,
    },
}

/// One verified mechanical symbol-map row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyEntry {
    /// TS reference file, relative to [`ANGULAR_COMPILER_SRC_ROOT`].
    pub ts_file: String,
    /// Committed Rust port file, relative to [`TREATY_IVY_SRC_ROOT`].
    pub rust_file: String,
    /// The outcome.
    pub status: VerifyStatus,
}

impl VerifyEntry {
    /// True when this row is byte-for-byte (semantically, for enums) in sync.
    pub fn is_in_sync(&self) -> bool {
        matches!(self.status, VerifyStatus::InSync)
    }

    /// True when this row fails the check: anything other than in sync or a documented pending row.
    pub fn is_blocking(&self) -> bool {
        !matches!(
            self.status,
            VerifyStatus::InSync | VerifyStatus::Pending { .. }
        )
    }
}

/// The result of verifying every mechanical row against the committed Rust.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct VerifyReport {
    /// One entry per `Mechanical` symbol-map row, in symbol-map order.
    pub entries: Vec<VerifyEntry>,
}

impl VerifyReport {
    /// True when every verified row is in sync (no pending rows either).
    pub fn is_all_in_sync(&self) -> bool {
        self.entries.iter().all(VerifyEntry::is_in_sync)
    }

    /// The rows that block the check (drifted, missing, unsupported, or a stale pending marker).
    pub fn out_of_sync(&self) -> Vec<&VerifyEntry> {
        self.entries.iter().filter(|e| e.is_blocking()).collect()
    }

    /// The rows that are pending for a documented reason (reported, not blocking).
    pub fn pending(&self) -> Vec<&VerifyEntry> {
        self.entries
            .iter()
            .filter(|e| matches!(e.status, VerifyStatus::Pending { .. }))
            .collect()
    }
}

/// For every [`PortKind::Mechanical`] symbol-map row, emit Rust from the vendored TS and compare it
/// with the committed Rust port, returning a [`VerifyReport`].
///
/// A row verifies the exports it anchors (all emitted exports when it anchors none). Each emitted
/// export is split into its declarations (`pub enum`, `pub const`, `impl`, `pub fn`); each one is
/// located in the committed file by its header line and compared:
///   * enums are compared by variant name and discriminant value (so `0b01` and `1` agree);
///   * everything else is compared line by line after dropping blank and comment-only lines.
///
/// Rows marked `pending` are never blocking while they drift; see [`VerifyStatus::Pending`].
pub fn verify_codegen(reader: &dyn SourceReader) -> io::Result<VerifyReport> {
    verify_codegen_with(reader, MODULE_MAP)
}

/// [`verify_codegen`] over an explicit module map. The live `MODULE_MAP` is the production input; the
/// tests of the `pending` mechanism pass their own rows so they do not depend on which live rows are
/// currently pending.
pub fn verify_codegen_with(
    reader: &dyn SourceReader,
    module_map: &[symbol_map::ModuleMapping],
) -> io::Result<VerifyReport> {
    require_reference_root(reader)?;
    let mut entries = Vec::new();

    for m in module_map {
        if m.kind != PortKind::Mechanical {
            continue;
        }
        let entry = |status| VerifyEntry {
            ts_file: m.ts_file.to_string(),
            rust_file: m.rust_file.to_string(),
            status,
        };

        let ts_src = match reader.ts(m.ts_file)? {
            Some(s) => s,
            None => {
                entries.push(entry(VerifyStatus::Missing {
                    reason: format!("vendored TS reference {} not found", m.ts_file),
                }));
                continue;
            }
        };
        let rust_src = match reader.rust(m.rust_file)? {
            Some(s) => s,
            None => {
                let reason = format!("committed Rust port {} not found", m.rust_file);
                entries.push(entry(match m.pending {
                    Some(p) => VerifyStatus::Pending {
                        reason: p.to_string(),
                        detail: reason,
                    },
                    None => VerifyStatus::Missing { reason },
                }));
                continue;
            }
        };

        let codegen = emit_rust(&ts_src);
        let (items, unsupported) = select_items(m.anchor_symbols, &codegen);
        let mut diffs = String::new();
        for item in items {
            if let Some(diff) = verify_item(item, &rust_src) {
                diffs.push_str(&diff);
            }
        }

        let blocking_status = if !diffs.is_empty() {
            let mut diff = String::new();
            if !unsupported.is_empty() {
                diff.push_str(&format!(
                    "# refused by the emitter: {}\n",
                    unsupported.join("; ")
                ));
            }
            diff.push_str(diffs.trim_end());
            VerifyStatus::Drifted { diff }
        } else if !unsupported.is_empty() {
            VerifyStatus::Unsupported {
                reason: unsupported.join("; "),
            }
        } else {
            VerifyStatus::InSync
        };

        let status = match (m.pending, blocking_status) {
            (None, s) => s,
            (Some(reason), VerifyStatus::InSync) => VerifyStatus::StalePending {
                reason: reason.to_string(),
            },
            (Some(reason), VerifyStatus::Drifted { diff }) => VerifyStatus::Pending {
                reason: reason.to_string(),
                detail: diff,
            },
            (Some(reason), VerifyStatus::Unsupported { reason: detail }) => VerifyStatus::Pending {
                reason: reason.to_string(),
                detail,
            },
            (Some(_), other) => other,
        };
        entries.push(entry(status));
    }

    Ok(VerifyReport { entries })
}

/// Pick the emitted items a row verifies: its anchored exports (all emitted exports when it anchors
/// none). Returns the items and, for each anchor the emitter did not produce, the refusal reason.
fn select_items<'a>(
    anchors: &[&str],
    codegen: &'a CodegenReport,
) -> (Vec<&'a Emitted>, Vec<String>) {
    if anchors.is_empty() {
        return (codegen.emitted.iter().collect(), Vec::new());
    }
    let mut items = Vec::new();
    let mut unsupported = Vec::new();
    for anchor in anchors {
        if let Some(item) = codegen.emitted.iter().find(|e| e.name == *anchor) {
            items.push(item);
            continue;
        }
        let reason = codegen
            .unsupported
            .iter()
            .find(|u| u.name == *anchor)
            .map(|u| format!("{}: {}", u.name, u.reason))
            .unwrap_or_else(|| format!("{anchor}: not an export the emitter produced"));
        unsupported.push(reason);
    }
    (items, unsupported)
}

/// Verify one emitted item against the committed Rust. `None` when every declaration matches.
///
/// The `R3Identifiers` shape is compared without regard to order: the committed port groups its
/// instruction names by purpose, not in the TS source order, so the enum is compared as a set of
/// names and the `name()` match as a map from variant to wire string.
fn verify_item(item: &Emitted, committed: &str) -> Option<String> {
    let mut out = String::new();
    let headers = declaration_headers(&item.rust);
    if headers.is_empty() {
        return Some(format!("# item `{}`: emitted no declaration\n", item.name));
    }
    let unordered = item.kind == EmittedKind::R3Identifiers;
    for header in headers {
        let Some(emitted_block) = extract_item(&item.rust, &header) else {
            continue;
        };
        match extract_item(committed, &header) {
            Some(committed_block) => {
                let result = if header.starts_with("pub enum ") && unordered {
                    compare_enum_names(&emitted_block, &committed_block)
                } else if header.starts_with("pub fn name(") && unordered {
                    compare_wire_names(&emitted_block, &committed_block)
                } else if header.starts_with("pub enum ") {
                    compare_enums(&emitted_block, &committed_block)
                } else {
                    diff_against(
                        &normalize_block(&emitted_block),
                        &normalize_block(&committed_block),
                    )
                };
                if let Err(diff) = result {
                    out.push_str(&format!(
                        "# item `{}` declaration `{header}`\n{diff}\n",
                        item.name
                    ));
                }
            }
            None => {
                out.push_str(&format!(
                    "# item `{}` declaration `{header}` not found in the committed Rust\n",
                    item.name
                ));
                out.push_str("--- emitted\n+++ committed (absent)\n");
                for line in normalize_block(&emitted_block).lines() {
                    out.push_str(&format!("-{line}\n"));
                }
                out.push('\n');
            }
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

/// The trimmed header lines of the TOP-LEVEL declarations an emitted item contains: `pub enum`,
/// `pub const`, `impl`, and `pub fn` lines, in source order. A declaration nested inside an
/// enclosing block (the `pub const Eager` alias inside `impl X {`) is covered by that block and is
/// not listed again.
fn declaration_headers(emitted: &str) -> Vec<String> {
    let lines: Vec<&str> = emitted.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim_start();
        let is_header = t.starts_with("pub enum ")
            || t.starts_with("pub const ")
            || t.starts_with("impl ")
            || t.starts_with("pub fn ");
        if is_header {
            out.push(t.to_string());
            i = block_end(&lines, i).map_or(i + 1, |end| end + 1);
        } else {
            i += 1;
        }
    }
    out
}

/// The `(variant, wire)` pairs of a `name()`-style match block, in source order.
fn match_arms(block: &str) -> Vec<(String, String)> {
    block
        .lines()
        .filter_map(|l| {
            let t = l.trim().trim_end_matches(',');
            let (pat, value) = t.split_once(" => ")?;
            let variant = pat.trim().strip_prefix("R3::")?;
            let wire = value.trim().strip_prefix('"')?.strip_suffix('"')?;
            Some((variant.to_string(), wire.to_string()))
        })
        .collect()
}

/// Compare two `R3` enums as sets of variant names (order-insensitive). Returns a `-`/`+` diff.
fn compare_enum_names(emitted: &str, committed: &str) -> Result<(), String> {
    let names = |block: &str| -> Vec<String> {
        normalize_block(block)
            .lines()
            .skip(1)
            .map(|l| l.trim().trim_end_matches(',').to_string())
            .filter(|l| !l.is_empty() && l != "}")
            .collect()
    };
    let e = names(emitted);
    let c = names(committed);
    let mut out = String::new();
    for n in &e {
        if !c.contains(n) {
            out.push_str(&format!("-{n},\n"));
        }
    }
    for n in &c {
        if !e.contains(n) {
            out.push_str(&format!("+{n},\n"));
        }
    }
    if out.is_empty() {
        Ok(())
    } else {
        Err(format!("--- emitted\n+++ committed\n{out}"))
    }
}

/// Compare two `name()` matches as maps from variant to wire string (order-insensitive). Reports
/// missing, extra, and changed arms.
fn compare_wire_names(emitted: &str, committed: &str) -> Result<(), String> {
    let e = match_arms(emitted);
    let c: BTreeMap<String, String> = match_arms(committed).into_iter().collect();
    let mut out = String::new();
    for (variant, wire) in &e {
        match c.get(variant) {
            Some(cw) if cw == wire => {}
            Some(cw) => out.push_str(&format!(
                "-R3::{variant} => \"{wire}\"\n+R3::{variant} => \"{cw}\"\n"
            )),
            None => out.push_str(&format!("-R3::{variant} => \"{wire}\"\n")),
        }
    }
    let known: std::collections::BTreeSet<&str> = e.iter().map(|(v, _)| v.as_str()).collect();
    for (variant, wire) in &c {
        if !known.contains(variant.as_str()) {
            out.push_str(&format!("+R3::{variant} => \"{wire}\"\n"));
        }
    }
    if out.is_empty() {
        Ok(())
    } else {
        Err(format!("--- emitted\n+++ committed\n{out}"))
    }
}

/// The index of the line that closes the declaration starting at `start`: the matching `}` for a
/// `{`-opened header, or `];` for a `&[`-opened const table. `None` when it never closes.
fn block_end(lines: &[&str], start: usize) -> Option<usize> {
    let decl = lines[start].trim_start();
    let (open, close_suffix): (char, &str) = if decl.starts_with("pub enum ") || decl.ends_with('{')
    {
        ('{', "}")
    } else {
        ('[', "];")
    };
    let mut depth = 0i32;
    for (i, line) in lines.iter().enumerate().skip(start) {
        for ch in line.chars() {
            if ch == open {
                depth += 1;
            } else if (open == '{' && ch == '}') || (open == '[' && ch == ']') {
                depth -= 1;
            }
        }
        if depth <= 0 && line.trim_end().ends_with(close_suffix) {
            return Some(i);
        }
    }
    None
}

/// Remove lines that carry no code for comparison purposes (blank, `//`/`///`/`//!` comments and
/// `#[...]` attributes), and trailing whitespace. Indentation is kept.
fn normalize_block(block: &str) -> String {
    block
        .lines()
        .map(str::trim_end)
        .filter(|l| {
            let t = l.trim_start();
            !(t.is_empty() || t.starts_with("//") || t.starts_with("#["))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Compare two enum declarations by their variant names and discriminant values, in order.
/// Unspecified discriminants count up from the previous one, as in Rust. Returns a short diff of
/// the differing variants (or the reason a value could not be read).
fn compare_enums(emitted: &str, committed: &str) -> Result<(), String> {
    let e = enum_variants(emitted).map_err(|r| format!("cannot compare emitted enum: {r}"))?;
    let c = enum_variants(committed).map_err(|r| format!("cannot compare committed enum: {r}"))?;
    if e == c {
        return Ok(());
    }
    let mut out = String::from("--- emitted\n+++ committed\n");
    let max = e.len().max(c.len());
    for i in 0..max {
        match (e.get(i), c.get(i)) {
            (Some(a), Some(b)) if a == b => {}
            (Some((n, v)), Some((cn, cv))) => {
                out.push_str(&format!("-{n} = {v},\n+{cn} = {cv},\n"));
            }
            (Some((n, v)), None) => out.push_str(&format!("-{n} = {v},\n")),
            (None, Some((cn, cv))) => out.push_str(&format!("+{cn} = {cv},\n")),
            (None, None) => {}
        }
    }
    Err(out)
}

/// The `(name, discriminant)` list of an enum declaration block.
fn enum_variants(block: &str) -> Result<Vec<(String, i64)>, String> {
    let normalized = normalize_block(block);
    let lines: Vec<&str> = normalized.lines().collect();
    if lines.len() < 2 {
        return Err("empty enum".into());
    }
    let mut out = Vec::new();
    let mut next: i64 = 0;
    for line in &lines[1..lines.len() - 1] {
        let t = line.trim().trim_end_matches(',').trim();
        if t.is_empty() {
            continue;
        }
        let (name, value) = match t.split_once('=') {
            Some((n, expr)) => {
                let v = eval_const(expr.trim()).map_err(|e| format!("`{t}`: {e}"))?;
                (n.trim().to_string(), v)
            }
            None => (t.to_string(), next),
        };
        next = value + 1;
        out.push((name, value));
    }
    Ok(out)
}

/// Evaluate the integer constant forms enum discriminants use: decimal, `0b`, `0o`, `0x` literals,
/// `<<`, and `|` (with `<<` binding tighter, as in Rust). Anything else is an error, never a guess.
fn eval_const(expr: &str) -> Result<i64, String> {
    let mut acc: i64 = 0;
    for term in expr.split('|') {
        let mut parts = term.split("<<");
        let mut v = parse_int_literal(parts.next().unwrap_or("").trim())?;
        for shift in parts {
            let n = parse_int_literal(shift.trim())?;
            let n = u32::try_from(n).map_err(|_| format!("bad shift `{shift}`"))?;
            v = v
                .checked_shl(n)
                .ok_or_else(|| format!("shift overflow in `{expr}`"))?;
        }
        acc |= v;
    }
    Ok(acc)
}

fn parse_int_literal(s: &str) -> Result<i64, String> {
    let clean: String = s.chars().filter(|c| *c != '_').collect();
    let parsed = if let Some(rest) = clean.strip_prefix("0b") {
        i64::from_str_radix(rest, 2)
    } else if let Some(rest) = clean.strip_prefix("0o") {
        i64::from_str_radix(rest, 8)
    } else if let Some(rest) = clean.strip_prefix("0x") {
        i64::from_str_radix(rest, 16)
    } else {
        clean.parse::<i64>()
    };
    parsed.map_err(|_| format!("`{s}` is not an integer literal this check can evaluate"))
}

/// Slice a declaration out of `source` by its exact trimmed `header` line, through its closer: the
/// matching `}` for a `{`-opened declaration, or `];` for a `&[`-opened const table. Returns the
/// block verbatim (committed indentation kept), or `None` when `header` is absent.
fn extract_item(source: &str, header: &str) -> Option<String> {
    if header.is_empty() {
        return None;
    }
    let lines: Vec<&str> = source.lines().collect();
    let start = lines.iter().position(|l| l.trim_start() == header)?;
    let end = block_end(&lines, start).unwrap_or(lines.len().saturating_sub(1));
    Some(lines[start..=end].join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbol_map::{ModuleMapping, PortKind as Kind};

    /// Two pending `core.ts` enum rows, as the live map carried them before the decorators port
    /// landed. The `pending` tests exercise the pending mechanism itself, so they run over these
    /// rows rather than over whichever live rows happen to be pending.
    const PENDING_ENUM_ROWS: &[ModuleMapping] = &[
        ModuleMapping {
            ts_file: "core.ts",
            rust_file: "decorators/src/compiler.rs",
            kind: Kind::Mechanical,
            spec: None,
            anchor_symbols: &["ViewEncapsulation"],
            pending: Some(
                "fixture: decorators/src/compiler.rs ViewEncapsulation lacks the ExperimentalIsolatedShadowDom = 4 member",
            ),
        },
        ModuleMapping {
            ts_file: "core.ts",
            rust_file: "decorators/src/compiler.rs",
            kind: Kind::Mechanical,
            spec: None,
            anchor_symbols: &["ChangeDetectionStrategy"],
            pending: Some(
                "fixture: decorators/src/compiler.rs ChangeDetectionStrategy lacks the Eager = 1 alias",
            ),
        },
    ];


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
        /// A reader holding a stub for EVERY mapped reference file, so baseline/drift see a complete
        /// reference. The stubs are identical across calls, so they never drift.
        fn full_stub() -> Self {
            let mut r = MemReader::default();
            for f in reference_ts_files() {
                r = r.with_ts(f, "export const STUB = 0;\n");
            }
            r
        }
        fn without_ts(mut self, rel: &str) -> Self {
            self.ts.remove(rel);
            self
        }
    }
    impl SourceReader for MemReader {
        fn ts(&self, rel: &str) -> io::Result<Option<String>> {
            Ok(self.ts.get(rel).cloned())
        }
        fn rust(&self, rel: &str) -> io::Result<Option<String>> {
            Ok(self.rust.get(rel).cloned())
        }
        fn list_ts(&self) -> io::Result<Vec<String>> {
            Ok(self.ts.keys().cloned().collect())
        }
        fn ts_root_label(&self) -> String {
            "<memory>".into()
        }
    }

    // A real symbol-map mechanical row we can drive: render3/r3_identifiers.ts -> identifiers.rs.
    const IDENTIFIERS_TS: &str = "export class Identifiers {
        static element = {name: 'ɵɵelement', moduleName: CORE};
        static elementStart = {name: 'ɵɵelementStart', moduleName: CORE};
    }";

    /// The committed-shape Rust for [`IDENTIFIERS_TS`] (what `identifiers.rs` must contain).
    fn committed_identifiers(wire_start: &str) -> String {
        format!(
            "//! identifiers port.\n\npub enum R3 {{\n    // Element\n    Element,\n    ElementStart,\n}}\n\nimpl R3 {{\n    /// wire name\n    pub fn name(self) -> &'static str {{\n        match self {{\n            R3::Element => \"ɵɵelement\",\n            R3::ElementStart => \"{wire_start}\",\n        }}\n    }}\n\n    pub fn reference(self) {{}}\n}}\n\nfn other() {{}}\n"
        )
    }

    #[test]
    fn baseline_round_trips_through_json() {
        let reader = MemReader::full_stub().with_ts("render3/r3_identifiers.ts", IDENTIFIERS_TS);
        let baseline =
            record_baseline(&reader, "78d2a67d922b2a2af29d50aed587bc78a99a5450").unwrap();

        assert_eq!(
            baseline.angular_ref,
            "78d2a67d922b2a2af29d50aed587bc78a99a5450"
        );
        assert_eq!(baseline.files.len(), reference_ts_files().len());
        let idents = &baseline.files["render3/r3_identifiers.ts"];
        assert!(idents.exists);
        assert!(idents.symbols.contains_key("Identifiers"));

        let json = serde_json::to_string_pretty(&baseline).expect("serialize");
        let back: Baseline = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(baseline, back);
    }

    #[test]
    fn baseline_fails_loudly_when_reference_root_is_missing_or_empty() {
        let err = record_baseline(&MemReader::default(), "sha").unwrap_err();
        assert!(err.to_string().contains("contains no .ts files"), "{err}");
        assert!(err.to_string().contains("populate-angular-ref.sh"), "{err}");
    }

    #[test]
    fn baseline_fails_loudly_when_a_mapped_file_is_missing() {
        // A partial reference is not the pinned ref; recording must not silently mark it absent.
        let reader = MemReader::default().with_ts("render3/r3_identifiers.ts", IDENTIFIERS_TS);
        let err = record_baseline(&reader, "sha").unwrap_err();
        assert!(
            err.to_string().contains("mapped Angular file(s) missing"),
            "{err}"
        );
        assert!(err.to_string().contains("core.ts"), "{err}");
    }

    #[test]
    fn drift_fails_loudly_when_reference_root_is_empty() {
        let baseline = Baseline {
            angular_ref: "sha".into(),
            files: BTreeMap::new(),
        };
        let err = diff_baseline(&MemReader::default(), &baseline, "now").unwrap_err();
        assert!(err.to_string().contains("contains no .ts files"), "{err}");
    }

    #[test]
    fn diff_baseline_clean_when_unchanged() {
        let reader = MemReader::full_stub().with_ts("render3/r3_identifiers.ts", IDENTIFIERS_TS);
        let baseline = record_baseline(&reader, "sha").unwrap();
        // Same sources -> no drift.
        let report = diff_baseline(&reader, &baseline, "sha").unwrap();
        assert!(report.is_clean(), "no change must be clean: {report:?}");
        assert!(report.files.is_empty());
        assert!(report.tasks.is_empty());
        assert!(report.unclassified_files.is_empty());
    }

    #[test]
    fn diff_baseline_flags_a_changed_fingerprint() {
        let reader = MemReader::full_stub().with_ts("render3/r3_identifiers.ts", IDENTIFIERS_TS);
        let baseline = record_baseline(&reader, "v22.0.0").unwrap();

        // Now the reference adds a NEW ɵɵ row and CHANGES an existing one's body.
        let changed = "export class Identifiers {
            static element = {name: 'ɵɵelementRENAMED', moduleName: CORE};
            static elementStart = {name: 'ɵɵelementStart', moduleName: CORE};
            static elementEnd = {name: 'ɵɵelementEnd', moduleName: CORE};
        }";
        let now = MemReader::full_stub().with_ts("render3/r3_identifiers.ts", changed);

        let report = diff_baseline(&now, &baseline, "v22.1.0").unwrap();
        assert!(!report.is_clean(), "a body change must register as drift");
        assert_eq!(report.old_ref, "v22.0.0");
        assert_eq!(report.new_ref, "v22.1.0");

        assert_eq!(report.files.len(), 1);
        let cf = &report.files[0];
        assert_eq!(cf.ts_file, "render3/r3_identifiers.ts");
        let modified = cf
            .symbols
            .iter()
            .find(|s| s.symbol == "Identifiers")
            .unwrap();
        assert_eq!(modified.change, ChangeKind::Modified);

        assert_eq!(report.tasks.len(), 1);
        let task = &report.tasks[0];
        assert_eq!(task.rust_file, "core/src/identifiers.rs");
        assert_eq!(task.action, TaskAction::Codegen);
        assert_eq!(
            task.spec.as_deref(),
            Some("migration/render3-specs/14-identifiers.md")
        );
    }

    #[test]
    fn diff_baseline_reports_removed_export() {
        let reader = MemReader::full_stub().with_ts("render3/r3_identifiers.ts", IDENTIFIERS_TS);
        let baseline = record_baseline(&reader, "v22.0.0").unwrap();
        // The file vanished from the reference; the other mapped files are still present.
        let now = MemReader::full_stub().without_ts("render3/r3_identifiers.ts");
        let report = diff_baseline(&now, &baseline, "v22.1.0").unwrap();
        let cf = report
            .files
            .iter()
            .find(|f| f.ts_file == "render3/r3_identifiers.ts")
            .unwrap();
        let removed = cf
            .symbols
            .iter()
            .find(|s| s.symbol == "Identifiers")
            .unwrap();
        assert_eq!(removed.change, ChangeKind::Removed);
    }

    #[test]
    fn diff_baseline_reports_an_unclassified_compiler_file() {
        let reader =
            MemReader::full_stub().with_ts("brand/new_angular_file.ts", "export const X = 1;");
        let baseline = record_baseline(&MemReader::full_stub(), "sha").unwrap();
        let report = diff_baseline(&reader, &baseline, "sha").unwrap();
        assert_eq!(
            report.unclassified_files,
            vec!["brand/new_angular_file.ts".to_string()]
        );
        assert!(
            !report.is_clean(),
            "an unclassified file must not read as clean"
        );
        assert!(
            report
                .tasks
                .iter()
                .any(|t| t.action == TaskAction::UpdateMap
                    && t.ts_files == vec!["brand/new_angular_file.ts".to_string()])
        );
    }

    #[test]
    fn diff_baseline_ignores_out_of_scope_exports() {
        // `Type` is an out-of-scope export of core.ts: changing it must not drift.
        let base = MemReader::full_stub().with_ts("core.ts", "export const Type = Function;");
        let baseline = record_baseline(&base, "sha").unwrap();
        let now = MemReader::full_stub().with_ts("core.ts", "export const Type = Object;");
        let report = diff_baseline(&now, &baseline, "sha").unwrap();
        assert!(report.is_clean(), "out-of-scope export drifted: {report:?}");
    }

    #[test]
    fn list_ts_files_is_sorted_relative_and_reports_missing_dirs() {
        let base = std::env::temp_dir().join(format!("render3-sync-list-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("sub")).unwrap();
        std::fs::write(base.join("b.ts"), "").unwrap();
        std::fs::write(base.join("sub/a.ts"), "").unwrap();
        std::fs::write(base.join("notes.md"), "").unwrap();
        let listed = list_ts_files(&base).unwrap();
        assert_eq!(listed, vec!["b.ts".to_string(), "sub/a.ts".to_string()]);
        let _ = std::fs::remove_dir_all(&base);
        let missing = list_ts_files(&base).unwrap_err();
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn verify_codegen_in_sync_when_committed_matches_emitted() {
        // The committed file holds the same R3 enum + name() match (comments and extra items are
        // ignored), so the identifiers row is in sync.
        let committed = committed_identifiers("ɵɵelementStart");
        let reader = MemReader::default()
            .with_ts("render3/r3_identifiers.ts", IDENTIFIERS_TS)
            .with_rust("core/src/identifiers.rs", &committed);

        let report = verify_codegen(&reader).unwrap();
        let entry = report
            .entries
            .iter()
            .find(|e| e.ts_file == "render3/r3_identifiers.ts")
            .expect("identifiers row verified");
        assert!(
            entry.is_in_sync(),
            "expected in-sync, got: {:?}",
            entry.status
        );
    }

    #[test]
    fn verify_codegen_detects_an_intentional_mismatch() {
        // Corrupt one wire name in the committed copy — a real, intentional drift.
        let committed = committed_identifiers("ɵɵWRONG");
        let reader = MemReader::default()
            .with_ts("render3/r3_identifiers.ts", IDENTIFIERS_TS)
            .with_rust("core/src/identifiers.rs", &committed);

        let report = verify_codegen(&reader).unwrap();
        assert!(!report.is_all_in_sync(), "drift must be detected");
        let entry = report
            .entries
            .iter()
            .find(|e| e.ts_file == "render3/r3_identifiers.ts")
            .unwrap();
        match &entry.status {
            VerifyStatus::Drifted { diff } => {
                assert!(
                    diff.contains("ɵɵelementStart"),
                    "diff should name removed value: {diff}"
                );
                assert!(
                    diff.contains("ɵɵWRONG"),
                    "diff should name added value: {diff}"
                );
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
        assert!(
            matches!(entry.status, VerifyStatus::Missing { .. }),
            "{:?}",
            entry.status
        );
    }

    #[test]
    fn verify_codegen_fails_loudly_on_an_empty_reference_root() {
        assert!(verify_codegen(&MemReader::default()).is_err());
    }

    #[test]
    fn enum_check_compares_values_not_spelling() {
        // RenderFlags is `Create = 0b01, Update = 0b10` in core.ts and the committed port spells the
        // same values in binary, with doc comments: in sync.
        let reader = MemReader::default()
            .with_ts("core.ts", "export const enum RenderFlags {\n  Create = 0b01,\n  Update = 0b10,\n}\n")
            .with_rust(
                "template/src/view/queries.rs",
                "/// doc\n#[derive(Debug)]\npub enum RenderFlags {\n    /// c\n    Create = 0b01,\n    // u\n    Update = 0b10,\n}\n",
            );
        let report = verify_codegen(&reader).unwrap();
        let entry = report
            .entries
            .iter()
            .find(|e| e.ts_file == "core.ts" && e.rust_file == "template/src/view/queries.rs")
            .expect("RenderFlags row");
        assert!(entry.is_in_sync(), "{:?}", entry.status);
    }

    #[test]
    fn enum_check_detects_a_changed_discriminant() {
        let reader = MemReader::default()
            .with_ts(
                "core.ts",
                "export const enum RenderFlags {\n  Create = 0b01,\n  Update = 0b10,\n}\n",
            )
            .with_rust(
                "template/src/view/queries.rs",
                "pub enum RenderFlags {\n    Create = 0b100,\n    Update = 0b10,\n}\n",
            );
        let report = verify_codegen(&reader).unwrap();
        let entry = report
            .entries
            .iter()
            .find(|e| e.ts_file == "core.ts" && e.rust_file == "template/src/view/queries.rs")
            .unwrap();
        match &entry.status {
            VerifyStatus::Drifted { diff } => assert!(
                diff.contains("Create = 1") && diff.contains("Create = 4"),
                "{diff}"
            ),
            other => panic!("expected Drifted, got {other:?}"),
        }
    }

    #[test]
    fn pending_row_that_drifts_is_reported_but_not_blocking() {
        // ViewEncapsulation is pending: the committed enum lacks ExperimentalIsolatedShadowDom.
        let reader = MemReader::default()
            .with_ts(
                "core.ts",
                "export enum ViewEncapsulation {\n  Emulated = 0,\n  None = 2,\n  ShadowDom = 3,\n  ExperimentalIsolatedShadowDom = 4,\n}\n",
            )
            .with_rust(
                "decorators/src/compiler.rs",
                "pub enum ViewEncapsulation {\n    Emulated = 0,\n    None = 2,\n    ShadowDom = 3,\n}\n",
            );
        let report = verify_codegen_with(&reader, PENDING_ENUM_ROWS).unwrap();
        let entry = report
            .entries
            .iter()
            .find(|e| {
                e.ts_file == "core.ts"
                    && e.rust_file == "decorators/src/compiler.rs"
                    && matches!(e.status, VerifyStatus::Pending { .. })
            })
            .expect("ViewEncapsulation pending row");
        match &entry.status {
            VerifyStatus::Pending { reason, detail } => {
                assert!(reason.contains("ExperimentalIsolatedShadowDom"), "{reason}");
                assert!(detail.contains("ExperimentalIsolatedShadowDom"), "{detail}");
            }
            other => panic!("expected Pending, got {other:?}"),
        }
        assert!(
            !report
                .out_of_sync()
                .iter()
                .any(|e| e.rust_file == "decorators/src/compiler.rs"
                    && e.ts_file == "core.ts"
                    && matches!(e.status, VerifyStatus::Pending { .. }))
        );
    }

    #[test]
    fn pending_row_that_verifies_is_a_blocking_stale_marker() {
        // The port has landed (all four members present): the pending marker must be removed.
        let reader = MemReader::default()
            .with_ts(
                "core.ts",
                "export enum ViewEncapsulation {\n  Emulated = 0,\n  None = 2,\n  ShadowDom = 3,\n  ExperimentalIsolatedShadowDom = 4,\n}\n",
            )
            .with_rust(
                "decorators/src/compiler.rs",
                "pub enum ViewEncapsulation {\n    Emulated = 0,\n    None = 2,\n    ShadowDom = 3,\n    ExperimentalIsolatedShadowDom = 4,\n}\n",
            );
        let report = verify_codegen_with(&reader, PENDING_ENUM_ROWS).unwrap();
        let stale = report
            .entries
            .iter()
            .find(|e| {
                e.rust_file == "decorators/src/compiler.rs"
                    && e.ts_file == "core.ts"
                    && matches!(e.status, VerifyStatus::StalePending { .. })
            })
            .expect("stale pending row");
        assert!(stale.is_blocking());
    }

    #[test]
    fn enum_with_an_alias_checks_the_alias_block() {
        // ChangeDetectionStrategy's `Eager = 1` alias is emitted as an `impl` const; a committed copy
        // without it is a difference, not silently ignored.
        let ts = "export enum ChangeDetectionStrategy { OnPush = 0, Default = 1, Eager = 1 }";
        let reader = MemReader::default().with_ts("core.ts", ts).with_rust(
            "decorators/src/compiler.rs",
            "pub enum ChangeDetectionStrategy {\n    OnPush = 0,\n    Default = 1,\n}\n",
        );
        let report = verify_codegen_with(&reader, PENDING_ENUM_ROWS).unwrap();
        let detail = report
            .entries
            .iter()
            .find_map(|e| match &e.status {
                VerifyStatus::Pending { detail, .. }
                    if e.rust_file == "decorators/src/compiler.rs" && detail.contains("Eager") =>
                {
                    Some(detail.clone())
                }
                _ => None,
            })
            .expect("a Pending ChangeDetectionStrategy row whose detail names the Eager alias");
        assert!(detail.contains("pub const Eager"), "{detail}");
    }

    #[test]
    fn identifier_check_ignores_the_committed_group_order() {
        // The committed port groups instruction names by purpose, not in TS order. Same names and
        // wire strings in a different order are in sync.
        let committed = "pub enum R3 {\n    ElementStart,\n    Element,\n}\n\nimpl R3 {\n    pub fn name(self) -> &'static str {\n        match self {\n            R3::ElementStart => \"\u{275}\u{275}elementStart\",\n            R3::Element => \"\u{275}\u{275}element\",\n        }\n    }\n}\n";
        let reader = MemReader::default()
            .with_ts("render3/r3_identifiers.ts", IDENTIFIERS_TS)
            .with_rust("core/src/identifiers.rs", committed);
        let report = verify_codegen(&reader).unwrap();
        let entry = report
            .entries
            .iter()
            .find(|e| e.ts_file == "render3/r3_identifiers.ts")
            .unwrap();
        assert!(entry.is_in_sync(), "{:?}", entry.status);
    }

    #[test]
    fn identifier_check_detects_a_missing_variant() {
        // The committed port lacks `ElementStart` entirely: enum and match both report it.
        let committed = "pub enum R3 {\n    Element,\n}\n\nimpl R3 {\n    pub fn name(self) -> &'static str {\n        match self {\n            R3::Element => \"\u{275}\u{275}element\",\n        }\n    }\n}\n";
        let reader = MemReader::default()
            .with_ts("render3/r3_identifiers.ts", IDENTIFIERS_TS)
            .with_rust("core/src/identifiers.rs", committed);
        let report = verify_codegen(&reader).unwrap();
        let entry = report
            .entries
            .iter()
            .find(|e| e.ts_file == "render3/r3_identifiers.ts")
            .unwrap();
        match &entry.status {
            VerifyStatus::Drifted { diff } => {
                assert!(diff.contains("-ElementStart,"), "{diff}");
                assert!(
                    diff.contains("-R3::ElementStart => \"\u{275}\u{275}elementStart\""),
                    "{diff}"
                );
            }
            other => panic!("expected Drifted, got {other:?}"),
        }
    }

    #[test]
    fn declaration_headers_list_top_level_items_only() {
        // The alias `pub const Eager` sits inside `impl ChangeDetectionStrategy {` and is covered by
        // that block, so it must not be listed (and reported) as a separate declaration.
        let emitted =
            emit_rust("export enum ChangeDetectionStrategy { OnPush = 0, Default = 1, Eager = 1 }");
        let headers = declaration_headers(&emitted.emitted[0].rust);
        assert_eq!(
            headers,
            vec![
                "pub enum ChangeDetectionStrategy {".to_string(),
                "impl ChangeDetectionStrategy {".to_string()
            ]
        );
    }

    #[test]
    fn out_of_scope_files_are_not_reported_by_drift() {
        // `indexer/*` and `typecheck/*` are out of scope: a new file there is classified, not missing.
        let baseline = record_baseline(&MemReader::full_stub(), "sha").unwrap();
        let now = MemReader::full_stub().with_ts("indexer/brand_new.ts", "export const X = 1;");
        let report = diff_baseline(&now, &baseline, "sha").unwrap();
        assert!(
            report.unclassified_files.is_empty(),
            "{:?}",
            report.unclassified_files
        );
        assert!(report.is_clean(), "{report:?}");
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

    #[test]
    fn eval_const_handles_the_flag_forms() {
        assert_eq!(eval_const("0b1000").unwrap(), 8);
        assert_eq!(eval_const("1 << 3").unwrap(), 8);
        assert_eq!(eval_const("0b0001 | 0b0100").unwrap(), 5);
        assert_eq!(eval_const("0x10").unwrap(), 16);
        assert!(eval_const("someFn()").is_err());
    }
}
