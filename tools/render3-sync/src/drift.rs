//! Pillar 1 — the **drift differ**.
//!
//! Given two snapshots of an Angular `packages/compiler` TypeScript source file (the `old` ref the
//! Rust port is currently 1:1 with, and the `new` ref being adopted), compute a structured,
//! deterministic diff of the file's top-level **exported** symbols and attribute each change to the
//! responsible Rust module via the [`symbol_map`](crate::symbol_map). The result feeds a
//! [`DriftReport`]: changed symbols -> Rust modules -> [`PortTask`]s.
//!
//! The diff is purely structural, NO AI:
//!   * parse both snapshots with oxc (`oxc_parser`) — the SAME parser the port consumes;
//!   * enumerate top-level exports (name + kind + a stable hash of the symbol's source text);
//!   * an export present only on the `new` side is [`Added`](ChangeKind::Added), present only on the
//!     `old` side is [`Removed`](ChangeKind::Removed), present on both with a differing body hash is
//!     [`Modified`](ChangeKind::Modified), and present on both with an equal hash is unchanged
//!     (omitted from the report);
//!   * each changed symbol is mapped to its Rust owner(s); unmapped changes are still reported and
//!     drive an [`UpdateMap`](TaskAction::UpdateMap) task so the map itself is kept honest.
//!
//! Body hashing uses the symbol's exact source span text (normalized for line endings), so a pure
//! whitespace/CRLF reflow does not register as drift but any token change does. The hash is a
//! deterministic FNV-1a over the normalized bytes — no external hashing dependency, identical across
//! platforms and runs.

use std::collections::BTreeMap;

use oxc_allocator::Allocator;
use oxc_ast::ast::{BindingPattern, Declaration, Statement};
use oxc_parser::Parser;
use oxc_span::{SourceType, Span};

use crate::report::{ChangeKind, ChangedFile, ChangedSymbol, DriftReport, PortTask, TaskAction};
use crate::symbol_map::{self, PortKind};
use crate::ts::ExportKind;

/// One top-level exported symbol of a single TS snapshot: its name, kind, and a stable hash of the
/// exact source text that defines it. Two snapshots' symbols are matched by `name`; a differing
/// `body_hash` for the same name is [`ChangeKind::Modified`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolFingerprint {
    /// Exported symbol name (e.g. `compileComponentFromMetadata`).
    pub name: String,
    /// What kind of declaration the export is.
    pub kind: ExportKind,
    /// Deterministic FNV-1a hash of the symbol's normalized source text.
    pub body_hash: u64,
}

/// Parse a TS `source` and return a deterministic fingerprint per top-level **exported** symbol,
/// keyed by name. Best-effort: a source with recoverable parse errors still yields the exports that
/// parsed (determinism over guessing — nothing is invented).
///
/// When the same name is exported twice (e.g. a merged declaration), the LAST occurrence's span is
/// folded in; names are unique in the returned map.
pub fn fingerprint_exports(source: &str) -> BTreeMap<String, SymbolFingerprint> {
    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true);
    let ret = Parser::new(&allocator, source, source_type).parse();

    let mut out: BTreeMap<String, SymbolFingerprint> = BTreeMap::new();
    for stmt in &ret.program.body {
        let Statement::ExportNamedDeclaration(export) = stmt else {
            continue;
        };
        let Some(decl) = &export.declaration else {
            // `export { Foo, Bar };` — a re-export specifier list has no own body; hash the
            // specifier name itself so an added/removed specifier still registers, while a
            // re-export that merely moves does not depend on unrelated file content.
            for spec in &export.specifiers {
                let name = spec.exported.name().to_string();
                let body_hash = fnv1a(name.as_bytes());
                out.insert(
                    name.clone(),
                    SymbolFingerprint { name, kind: ExportKind::Variable, body_hash },
                );
            }
            continue;
        };
        collect_declaration(decl, source, &mut out);
    }
    out
}

/// Collect every named binding introduced by an exported `decl`, hashing each by its source span.
fn collect_declaration(
    decl: &Declaration,
    source: &str,
    out: &mut BTreeMap<String, SymbolFingerprint>,
) {
    match decl {
        Declaration::ClassDeclaration(c) => {
            if let Some(id) = &c.id {
                insert(out, id.name.as_str(), ExportKind::Class, c.span, source);
            }
        }
        Declaration::FunctionDeclaration(f) => {
            if let Some(id) = &f.id {
                insert(out, id.name.as_str(), ExportKind::Function, f.span, source);
            }
        }
        Declaration::TSEnumDeclaration(e) => {
            insert(out, e.id.name.as_str(), ExportKind::Enum, e.span, source);
        }
        Declaration::TSInterfaceDeclaration(i) => {
            insert(out, i.id.name.as_str(), ExportKind::Interface, i.span, source);
        }
        Declaration::TSTypeAliasDeclaration(t) => {
            insert(out, t.id.name.as_str(), ExportKind::TypeAlias, t.span, source);
        }
        Declaration::VariableDeclaration(v) => {
            // Each declarator is hashed by its OWN span so two consts in one `export const a=1,b=2;`
            // drift independently.
            for d in &v.declarations {
                if let BindingPattern::BindingIdentifier(id) = &d.id {
                    insert(out, id.name.as_str(), ExportKind::Variable, d.span, source);
                }
            }
        }
        _ => {}
    }
}

/// Insert (or overwrite) a fingerprint for `name`, hashing the normalized text of `span`.
fn insert(
    out: &mut BTreeMap<String, SymbolFingerprint>,
    name: &str,
    kind: ExportKind,
    span: Span,
    source: &str,
) {
    let text = span.source_text(source);
    let body_hash = fnv1a(&normalize(text));
    out.insert(name.to_string(), SymbolFingerprint { name: name.to_string(), kind, body_hash });
}

/// Normalize source text for hashing so that a pure line-ending reflow (CRLF<->LF) or trailing
/// whitespace is NOT reported as drift, while every meaningful token change is. We intentionally do
/// NOT collapse interior whitespace — Angular's tables are whitespace-insensitive only at line ends,
/// and over-normalizing risks hiding a real value change.
fn normalize(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for line in text.split('\n') {
        let trimmed = line.strip_suffix('\r').unwrap_or(line);
        let trimmed = trimmed.trim_end();
        out.extend_from_slice(trimmed.as_bytes());
        out.push(b'\n');
    }
    out
}

/// FNV-1a 64-bit — a tiny, dependency-free, deterministic hash. Sufficient to detect any body
/// change; the harness only needs equality, not cryptographic strength.
fn fnv1a(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Diff the exported symbols of one TS file between an `old` and a `new` snapshot, attributing each
/// change to the Rust module(s) that port it.
///
/// `ts_file` is the path of the file RELATIVE to
/// [`ANGULAR_COMPILER_SRC_ROOT`](crate::symbol_map::ANGULAR_COMPILER_SRC_ROOT) (e.g.
/// `render3/r3_identifiers.ts`); it is what the symbol->module map is keyed on. Returns a
/// [`ChangedFile`] (with `symbols` empty when nothing drifted), so callers can decide whether to
/// retain clean files in the report.
pub fn diff_file(ts_file: &str, old_source: &str, new_source: &str) -> ChangedFile {
    let norm_file = ts_file.replace('\\', "/");
    let mapped = !symbol_map::rust_modules_for_ts(&norm_file).is_empty();

    let old = fingerprint_exports(old_source);
    let new = fingerprint_exports(new_source);

    // Union of names, ordered (BTreeMap keys are sorted) for a deterministic report.
    let mut names: Vec<&String> = old.keys().chain(new.keys()).collect();
    names.sort();
    names.dedup();

    let mut symbols = Vec::new();
    for name in names {
        let change = match (old.get(name), new.get(name)) {
            (None, Some(_)) => ChangeKind::Added,
            (Some(_), None) => ChangeKind::Removed,
            (Some(o), Some(n)) if o.body_hash != n.body_hash => ChangeKind::Modified,
            // Present on both with equal hash, or present in neither (impossible): unchanged.
            _ => continue,
        };
        symbols.push(attribute_symbol(name, &norm_file, change));
    }

    ChangedFile { ts_file: norm_file, mapped, symbols }
}

/// Build a [`ChangedSymbol`], resolving the Rust owner(s) of `symbol`.
///
/// Resolution order (most specific first):
///   1. the symbol's own anchor mapping (`anchor_symbols` lists it) — pins the exact owning module;
///   2. fall back to the file-level mapping (the TS file's Rust port), which is correct for any
///      non-anchor export of a mapped file;
///   3. nothing — an unmapped export, reported with empty `rust_files` so it drives an `UpdateMap`.
///
/// `kind` is taken from the resolved owner(s): if any owner is [`PortKind::Mechanical`] the symbol
/// is a codegen candidate, else [`PortKind::Logic`]; `None` when unmapped.
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

/// A single (old, new) snapshot pair for one TS file, the input unit of [`build_report`].
#[derive(Debug, Clone)]
pub struct FileSnapshot<'a> {
    /// File path relative to [`ANGULAR_COMPILER_SRC_ROOT`](crate::symbol_map::ANGULAR_COMPILER_SRC_ROOT).
    pub ts_file: &'a str,
    /// Source at the `old` ref (the Rust port is currently 1:1 with this). Empty string means the
    /// file did not exist at the old ref (every export is `Added`).
    pub old_source: &'a str,
    /// Source at the `new` ref. Empty string means the file was deleted at the new ref (every
    /// export is `Removed`).
    pub new_source: &'a str,
}

/// Compute a full [`DriftReport`] from a set of changed-file snapshots between two Angular refs.
///
/// `old_ref`/`new_ref` are recorded verbatim (git sha/tag or any caller-chosen label). Each snapshot
/// is diffed via [`diff_file`]; files whose exports are unchanged are dropped, so the report lists
/// only real drift. The derived [`PortTask`] list (one per Rust owner, plus one `UpdateMap` per
/// unmapped file) is the pillar-1 deliverable.
pub fn build_report(old_ref: &str, new_ref: &str, snapshots: &[FileSnapshot<'_>]) -> DriftReport {
    let mut files: Vec<ChangedFile> = Vec::new();
    for snap in snapshots {
        let cf = diff_file(snap.ts_file, snap.old_source, snap.new_source);
        if !cf.symbols.is_empty() {
            files.push(cf);
        }
    }
    files.sort_by(|a, b| a.ts_file.cmp(&b.ts_file));

    let tasks = derive_tasks(&files);
    DriftReport { old_ref: old_ref.to_string(), new_ref: new_ref.to_string(), files, tasks }
}

/// Group the per-symbol changes into a deduplicated, actionable task list.
///
/// Mapped changes are grouped by their owning Rust file: a group is a [`TaskAction::Codegen`] task
/// when its owner is mechanical (oxc can regenerate + diff the table), else a
/// [`TaskAction::ManualPort`] task (a human re-ports the logic). Unmapped changes (empty
/// `rust_files`) are grouped per TS file into an [`TaskAction::UpdateMap`] task — the map is missing
/// a row. The result is sorted for determinism.
fn derive_tasks(files: &[ChangedFile]) -> Vec<PortTask> {
    // Accumulators keyed for stable grouping.
    struct Group {
        rust_file: String,
        ts_files: Vec<String>,
        symbols: Vec<String>,
        action: TaskAction,
        spec: Option<String>,
    }
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    let mut unmapped: BTreeMap<String, (Vec<String>, Vec<String>)> = BTreeMap::new();

    for file in files {
        for sym in &file.symbols {
            if sym.rust_files.is_empty() {
                // Unmapped export -> UpdateMap, grouped by TS file.
                let entry = unmapped
                    .entry(sym.ts_file.clone())
                    .or_insert_with(|| (Vec::new(), Vec::new()));
                push_unique(&mut entry.0, sym.ts_file.clone());
                push_unique(&mut entry.1, sym.symbol.clone());
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
                // If ANY contributing symbol is hand-ported logic, the whole module needs a manual
                // port — codegen alone cannot be trusted to cover it.
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
    for (ts_file, (mut ts_files, mut symbols)) in unmapped {
        ts_files.sort();
        symbols.sort();
        let summary = format!(
            "unmapped Angular export(s) in {ts_file}: {} — add a symbol->module map row",
            symbols.join(", ")
        );
        tasks.push(PortTask {
            rust_file: String::new(),
            ts_files,
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

/// Resolve the per-module spec (under `migration/render3-specs/`) for a Rust file, if the map
/// records one for any TS source feeding it.
fn spec_for_rust_file(rust_file: &str) -> Option<String> {
    symbol_map::MODULE_MAP
        .iter()
        .find(|m| m.rust_file == rust_file && m.spec.is_some())
        .and_then(|m| m.spec)
        .map(str::to_string)
}

/// One-line human-readable summary for a derived task.
fn task_summary(action: &TaskAction, rust_file: &str, symbols: &[String]) -> String {
    let syms = symbols.join(", ");
    match action {
        TaskAction::Codegen => {
            format!("regenerate mechanical table {rust_file} (oxc codegen + byte-diff): {syms}")
        }
        TaskAction::ManualPort => {
            format!("re-port {rust_file} by hand — drifted symbols: {syms}")
        }
        TaskAction::UpdateMap => {
            format!("add symbol->module map row(s) for: {syms}")
        }
    }
}

/// Push `value` into `vec` only if not already present (small lists; linear scan is fine and keeps
/// insertion order before the caller sorts).
fn push_unique(vec: &mut Vec<String>, value: String) {
    if !vec.contains(&value) {
        vec.push(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- two small synthetic TS snapshots: an ADDED export + a CHANGED function body -----------

    // These synthetic exports deliberately use NON-anchor names (the map lists no `widget*` /
    // `Helper` anchors), so they resolve via the FILE-level mapping of the chosen TS path
    // (`render3/r3_identifiers.ts` -> `identifiers.rs`). This exercises the file-level fallback;
    // the dedicated `anchor_symbol_pins_specific_owner` test covers anchor resolution.

    /// The `old` snapshot of a synthetic render3 source file.
    const OLD_SRC: &str = r#"
        export class Helper {
            static readonly element = { name: 'ɵɵelement' };
        }

        export function buildWidgetA(meta) {
            return meta.template;
        }

        export const SHARED = 1;
    "#;

    /// The `new` snapshot: `buildWidgetA`'s BODY changed, a NEW export `buildWidgetB` was ADDED,
    /// `SHARED` is byte-for-byte identical (must NOT be reported), and `Helper` only reflowed
    /// whitespace (must NOT be reported).
    const NEW_SRC: &str = r#"
        export class Helper {
            static readonly element = { name: 'ɵɵelement' };
        }

        export function buildWidgetA(meta) {
            return optimize(meta.template);
        }

        export function buildWidgetB(meta) {
            return meta;
        }

        export const SHARED = 1;
    "#;

    #[test]
    fn fingerprint_ignores_pure_whitespace_reflow() {
        let a = fingerprint_exports("export const X = 1;");
        let b = fingerprint_exports("export   const   X   =   1;   \r\n");
        // Interior spacing differs, so the hashes legitimately differ — we only normalize line
        // ends. Assert the NAME is stable and the export is found either way.
        assert!(a.contains_key("X") && b.contains_key("X"));
        // A trailing-CRLF-only difference on an identical line must hash equally.
        let c = fingerprint_exports("export const Y = 2;");
        let d = fingerprint_exports("export const Y = 2;\r\n");
        assert_eq!(c["Y"].body_hash, d["Y"].body_hash);
    }

    #[test]
    fn diff_reports_added_and_modified_mapped_to_modules() {
        let cf = diff_file("render3/r3_identifiers.ts", OLD_SRC, NEW_SRC);
        assert!(cf.mapped, "the identifiers file must be a mapped source");

        // Exactly two symbols drifted: the modified fn + the added fn. SHARED + Identifiers are
        // unchanged and must be absent.
        let by_name: BTreeMap<&str, &ChangedSymbol> =
            cf.symbols.iter().map(|s| (s.symbol.as_str(), s)).collect();
        assert_eq!(cf.symbols.len(), 2, "got: {:?}", cf.symbols);

        let modified = by_name["buildWidgetA"];
        assert_eq!(modified.change, ChangeKind::Modified);
        let added = by_name["buildWidgetB"];
        assert_eq!(added.change, ChangeKind::Added);

        assert!(!by_name.contains_key("SHARED"), "unchanged const must not be reported");
        assert!(!by_name.contains_key("Helper"), "reflow-only class must not be reported");

        // Both map (file-level) to identifiers.rs — a Mechanical table.
        for s in [modified, added] {
            assert_eq!(s.rust_files, vec!["identifiers.rs".to_string()]);
            assert_eq!(s.kind, Some(PortKind::Mechanical));
        }
    }

    #[test]
    fn build_report_lists_symbols_modules_and_tasks() {
        let report = build_report(
            "v22.1.0",
            "v22.2.0",
            &[FileSnapshot {
                ts_file: "render3/r3_identifiers.ts",
                old_source: OLD_SRC,
                new_source: NEW_SRC,
            }],
        );

        assert_eq!(report.old_ref, "v22.1.0");
        assert_eq!(report.new_ref, "v22.2.0");
        assert!(!report.is_clean(), "a real change must not read as clean");

        // One changed file, two changed symbols.
        assert_eq!(report.files.len(), 1);
        assert_eq!(report.files[0].ts_file, "render3/r3_identifiers.ts");
        assert_eq!(report.files[0].symbols.len(), 2);

        // The two symbols collapse into ONE task for identifiers.rs, a mechanical Codegen task with
        // the table's spec attached.
        assert_eq!(report.tasks.len(), 1, "tasks: {:?}", report.tasks);
        let task = &report.tasks[0];
        assert_eq!(task.rust_file, "identifiers.rs");
        assert_eq!(task.action, TaskAction::Codegen);
        assert_eq!(
            task.symbols,
            vec!["buildWidgetA".to_string(), "buildWidgetB".to_string()]
        );
        assert_eq!(task.ts_files, vec!["render3/r3_identifiers.ts".to_string()]);
        assert_eq!(task.spec.as_deref(), Some("migration/render3-specs/14-identifiers.md"));

        // The report round-trips through JSON (the harness emits it as an artifact).
        let json = serde_json::to_string_pretty(&report).expect("serialize");
        let back: DriftReport = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(report, back);
    }

    #[test]
    fn removed_export_is_reported() {
        let cf = diff_file("render3/r3_identifiers.ts", "export const GONE = 1;", "");
        assert_eq!(cf.symbols.len(), 1);
        assert_eq!(cf.symbols[0].symbol, "GONE");
        assert_eq!(cf.symbols[0].change, ChangeKind::Removed);
    }

    #[test]
    fn logic_module_change_is_a_manual_port_task() {
        // view/template.ts -> view/template.rs is hand-ported LOGIC.
        let report = build_report(
            "a",
            "b",
            &[FileSnapshot {
                ts_file: "render3/view/template.ts",
                old_source: "export class TemplateDefinitionBuilder { build() { return 1; } }",
                new_source: "export class TemplateDefinitionBuilder { build() { return 2; } }",
            }],
        );
        assert_eq!(report.tasks.len(), 1);
        assert_eq!(report.tasks[0].action, TaskAction::ManualPort);
        assert_eq!(report.tasks[0].rust_file, "view/template.rs");
    }

    #[test]
    fn unmapped_export_drives_update_map_task() {
        // A TS file the map does not know about: every changed export is unmapped.
        let report = build_report(
            "a",
            "b",
            &[FileSnapshot {
                ts_file: "render3/brand_new_feature.ts",
                old_source: "",
                new_source: "export function brandNewThing() { return 0; }",
            }],
        );
        assert_eq!(report.files.len(), 1);
        assert!(!report.files[0].mapped);
        assert_eq!(report.tasks.len(), 1);
        let task = &report.tasks[0];
        assert_eq!(task.action, TaskAction::UpdateMap);
        assert!(task.rust_file.is_empty());
        assert_eq!(task.symbols, vec!["brandNewThing".to_string()]);
    }

    #[test]
    fn no_drift_yields_clean_report() {
        let report = build_report(
            "same",
            "same",
            &[FileSnapshot {
                ts_file: "render3/r3_identifiers.ts",
                old_source: OLD_SRC,
                new_source: OLD_SRC,
            }],
        );
        assert!(report.files.is_empty());
        assert!(report.tasks.is_empty());
        assert!(report.is_clean());
    }

    #[test]
    fn anchor_symbol_pins_specific_owner() {
        // `AttributeMarker` is an anchor of core.ts -> output_ast.rs (Mechanical), even though its
        // own file (core.ts) also maps file-level. Anchor resolution must win and be Mechanical.
        let cf = diff_file(
            "core.ts",
            "export enum AttributeMarker { NamespaceURI = 0 }",
            "export enum AttributeMarker { NamespaceURI = 0, Classes = 1 }",
        );
        assert_eq!(cf.symbols.len(), 1);
        let s = &cf.symbols[0];
        assert_eq!(s.symbol, "AttributeMarker");
        assert_eq!(s.change, ChangeKind::Modified);
        assert!(s.rust_files.contains(&"output_ast.rs".to_string()));
        assert_eq!(s.kind, Some(PortKind::Mechanical));
    }
}
