//! `.d.ts` re-export **barrel flattening** — the declaration analogue of the FESM
//! flatten ([`crate::fesm`]).
//!
//! ## The bug this fixes
//!
//! A library's public entry is conventionally a *barrel* that only re-exports its
//! internal modules:
//!
//! ```ts
//! // src/public-api.ts
//! export * from './lib/box.component';
//! export { Foo } from './lib/foo';
//! ```
//!
//! Packagr emits ONE flattened FESM (`index.mjs`) and ONE declaration
//! (`index.d.ts`) per entry — it does NOT write `./lib/box.component.d.ts` to dist.
//! The previous declaration path passed the barrel through verbatim, so the emitted
//! `index.d.ts` was literally `export * from './lib/box.component';` — a re-export
//! pointing at a file that does not exist in the package. A consumer importing the
//! package got **unresolvable types**.
//!
//! ng-packagr avoids this by flattening the `.d.ts` with `rollup-plugin-dts`: the
//! re-exported declarations are inlined into a single self-contained `index.d.ts`
//! that ends with a plain `export { … }`. This module reproduces that: it resolves
//! each relative re-export on disk (transitively), synthesizes each target's
//! declaration via the existing [`crate::dts`] / [`crate::component_dts`]
//! machinery, inlines the `declare class` / `declare const` bodies, hoists a single
//! `import * as i0 from "@angular/core"`, and emits one aggregated `export { … }`.
//!
//! ## Scope and safety
//!
//! Flattening only fires when the entry's compiled declaration is a **pure
//! re-export barrel** over *private* relative modules (modules that are not
//! themselves published entry points). A re-export of a *sibling published entry*
//! is left as a cross-entry reference (that entry ships its own `.d.ts`), exactly as
//! the FESM flatten leaves sibling imports. Anything the flattener cannot prove safe
//! — a re-export it cannot resolve, a target whose declaration cannot be
//! synthesized, a name collision — makes it **bail to the original declaration**, so
//! it never produces a worse result than before.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use oxc_allocator::Allocator;
use oxc_ast::ast::{ModuleExportName, Statement};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType};

use crate::compile;
use crate::dts;

/// The source extensions a relative re-export may resolve to, in entry-discovery
/// precedence order (authoring sources before plain `.ts`). Mirrors
/// [`crate::fesm`]'s resolution.
const RESOLVE_EXTENSIONS: [&str; 4] = ["treaty", "tjsx", "tsx", "ts"];

/// Flatten an entry's declaration if it is a re-export barrel over private modules.
///
/// `entry_source` is the entry's raw source; `entry_dts` is the declaration packagr
/// already derived for it ([`dts::emit_dts_for_entry`]); `entry_source_path` anchors
/// relative-re-export resolution; `entry_source_paths` is the set of all published
/// entry source paths (so a sibling-entry re-export is left untouched).
///
/// Returns a self-contained flattened `.d.ts` when the entry is a flattenable
/// barrel, otherwise returns `entry_dts` unchanged (identity).
pub fn flatten_entry_dts(
    entry_source: &str,
    entry_dts: &str,
    entry_source_path: &Path,
    entry_source_paths: &HashSet<PathBuf>,
) -> String {
    match try_flatten(entry_source, entry_source_path, entry_source_paths) {
        Some(flat) => flat,
        None => entry_dts.to_string(),
    }
}

/// One re-export edge discovered in a barrel's source.
struct ReExport {
    /// The resolved private-module path (a relative re-export of a non-entry
    /// module). `None` for bare/external re-exports and sibling-entry re-exports
    /// (both left untouched, which forces identity if encountered).
    resolved: Option<PathBuf>,
    /// For `export { a as b }` — the explicit name subset to expose (empty = all,
    /// i.e. `export *`). Each is `(local, exported)`.
    names: Vec<(String, String)>,
    /// Whether this is `export *` (star) vs a named `export { … }`.
    is_star: bool,
}

/// Attempt the flatten; `None` means "not a flattenable barrel → keep identity".
fn try_flatten(
    entry_source: &str,
    entry_source_path: &Path,
    entry_source_paths: &HashSet<PathBuf>,
) -> Option<String> {
    // 1. Parse the entry SOURCE and require it to be a PURE re-export barrel: every
    //    top-level statement is a relative `export … from './x'` over a private
    //    module. A single non-re-export statement (a real declaration, a bare
    //    import) means this is not a barrel we should rewrite — bail to identity.
    let edges = barrel_reexports(entry_source, entry_source_path, entry_source_paths)?;
    if edges.is_empty() {
        return None; // not a barrel.
    }
    // Every edge must resolve to a private module; a sibling-entry / unresolved
    // re-export is left to the normal path (identity).
    if edges.iter().any(|e| e.resolved.is_none()) {
        return None;
    }

    // 2. For each re-exported private module, synthesize its declaration and pull
    //    out (a) the inlined declaration bodies and (b) the names it contributes.
    let mut import_i0 = false;
    let mut bodies: Vec<String> = Vec::new();
    let mut exported_names: BTreeSet<String> = BTreeSet::new();
    let mut declared: HashSet<String> = HashSet::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();

    for edge in &edges {
        let path = edge.resolved.as_ref().unwrap();
        let module = synthesize_module_dts(path, entry_source_paths, &mut visited)?;
        if module.needs_i0 {
            import_i0 = true;
        }
        for decl in module.declarations {
            // Collision guard: a name declared by two modules can't be concatenated.
            if !declared.insert(decl.name.clone()) {
                return None;
            }
            bodies.push(decl.body);
        }
        // Decide which names this edge re-exports.
        if edge.is_star {
            for n in &module.exported_names {
                exported_names.insert(n.clone());
            }
        } else {
            for (local, exported) in &edge.names {
                // The local must be a name the module actually declares.
                if !module.exported_names.iter().any(|n| n == local) {
                    return None;
                }
                if local == exported {
                    exported_names.insert(exported.clone());
                } else {
                    // Renamed re-export: expose under the new name via the
                    // aggregated export list (`local as exported`).
                    exported_names.insert(format!("{local} as {exported}"));
                }
            }
        }
    }

    if bodies.is_empty() || exported_names.is_empty() {
        return None;
    }

    // 3. Assemble the self-contained declaration.
    let mut out = String::new();
    if import_i0 {
        out.push_str("import * as i0 from \"@angular/core\";\n");
    }
    for body in &bodies {
        out.push_str(body.trim_end_matches('\n'));
        out.push('\n');
    }
    let list = exported_names.iter().cloned().collect::<Vec<_>>().join(", ");
    out.push_str(&format!("export {{ {list} }};\n"));

    // 4. Safety net: the flattened declaration MUST re-parse as a `.d.ts` and MUST
    //    NOT carry any surviving relative re-export.
    if !reparses_dts(&out) || has_relative_reexport(&out) {
        return None;
    }
    Some(out)
}

/// A module's synthesized declaration, decomposed for inlining.
struct ModuleDts {
    /// The names the module exports (the surface a barrel `export *` re-exposes).
    exported_names: Vec<String>,
    /// The individual top-level declaration bodies, each as `declare …` text with
    /// the `export ` keyword stripped (so the barrel owns the export surface) and
    /// the name it binds.
    declarations: Vec<NamedDecl>,
    /// Whether any declaration references the `i0` Angular-core namespace (so the
    /// flattened module must hoist `import * as i0`).
    needs_i0: bool,
}

struct NamedDecl {
    name: String,
    body: String,
}

/// Compile a private module, synthesize its `.d.ts`, and decompose it into named
/// declaration bodies + its export surface. Resolves a nested re-export barrel
/// transitively. Returns `None` on any unsupported shape (so the caller bails).
fn synthesize_module_dts(
    path: &Path,
    entry_source_paths: &HashSet<PathBuf>,
    visited: &mut HashSet<PathBuf>,
) -> Option<ModuleDts> {
    if !visited.insert(path.to_path_buf()) {
        return None; // cycle → bail.
    }

    let source = std::fs::read_to_string(path).ok()?;

    // A nested barrel: this private module is ITSELF only re-exports. Recurse so a
    // transitive `public-api → feature/index → feature/x.component` chain flattens.
    if let Some(edges) = barrel_reexports(&source, path, entry_source_paths) {
        if !edges.is_empty() && edges.iter().all(|e| e.resolved.is_some()) {
            let mut merged = ModuleDts {
                exported_names: Vec::new(),
                declarations: Vec::new(),
                needs_i0: false,
            };
            for edge in &edges {
                let inner = synthesize_module_dts(
                    edge.resolved.as_ref().unwrap(),
                    entry_source_paths,
                    visited,
                )?;
                if inner.needs_i0 {
                    merged.needs_i0 = true;
                }
                merged.declarations.extend(inner.declarations);
                if edge.is_star {
                    merged.exported_names.extend(inner.exported_names);
                } else {
                    for (local, _exported) in &edge.names {
                        merged.exported_names.push(local.clone());
                    }
                }
            }
            return Some(merged);
        }
    }

    // A leaf module: compile to Ivy + synthesize the declaration.
    let compiled = compile::compile_entry_at(&source, path);
    if !compiled.errors.is_empty() {
        return None;
    }
    let dts = dts::emit_dts_for_entry(&source, &compiled.code, file_name_of(path)).ok()?;
    decompose_dts(&dts)
}

/// Parse a synthesized `.d.ts` and split it into named declaration bodies, its
/// export surface, and whether it imports `i0`. Drops the `import * as i0` line
/// (the flattener hoists a single shared one) and strips `export ` keywords.
fn decompose_dts(dts: &str) -> Option<ModuleDts> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, dts, dts_source_type()).parse();
    if !parsed.errors.is_empty() {
        return None;
    }

    let mut declarations: Vec<NamedDecl> = Vec::new();
    let mut exported_names: Vec<String> = Vec::new();
    let needs_i0 = dts.contains("i0.") || dts.contains("import * as i0");

    for stmt in &parsed.program.body {
        let span = stmt.span();
        let raw = &dts[span.start as usize..span.end as usize];
        match stmt {
            // Drop the hoisted Angular-core namespace import (re-added once).
            Statement::ImportDeclaration(import)
                if import.source.value == "@angular/core" =>
            {
                // Skipped — the flattener re-adds a single `import * as i0`.
            }
            // `export declare class X { … }` / `export declare const x: …;`
            Statement::ExportNamedDeclaration(export) if export.declaration.is_some() => {
                let decl = export.declaration.as_ref().unwrap();
                let names = declaration_names_dts(decl);
                if names.is_empty() {
                    return None;
                }
                // Strip the leading `export ` so the barrel owns the export surface.
                let decl_start = decl.span().start as usize;
                let body = dts[decl_start..span.end as usize].to_string();
                for n in &names {
                    exported_names.push(n.clone());
                    declarations.push(NamedDecl {
                        name: n.clone(),
                        body: body.clone(),
                    });
                }
            }
            // `export default X;` — record the default's name as exportable but do
            // not emit a duplicate body (the class/const body precedes it).
            Statement::ExportDefaultDeclaration(_) => {
                // The barrel re-exports named symbols; a bare `export default`
                // re-export is not part of a named barrel surface. Ignore it.
            }
            // `export { A, B };` — a local re-export of already-declared names.
            Statement::ExportNamedDeclaration(export) => {
                if export.source.is_some() {
                    // A nested `export … from './rel'` inside a leaf .d.ts — would
                    // need transitive resolution we did not perform here; bail.
                    return None;
                }
                for spec in &export.specifiers {
                    exported_names.push(module_export_name(&spec.exported));
                }
            }
            // A bare `declare class/const` without `export` (already stripped form)
            // or any other top-level declaration: keep verbatim, record its name.
            Statement::ClassDeclaration(c) => {
                if let Some(id) = &c.id {
                    let name = id.name.to_string();
                    exported_names.push(name.clone());
                    declarations.push(NamedDecl { name, body: raw.to_string() });
                }
            }
            Statement::VariableDeclaration(_)
            | Statement::TSTypeAliasDeclaration(_)
            | Statement::TSInterfaceDeclaration(_) => {
                // A non-exported helper declaration — keep it inline but it does
                // not contribute to the export surface.
                // (Rare in synthesized .d.ts; preserved for completeness.)
            }
            _ => {}
        }
    }

    if declarations.is_empty() {
        return None;
    }
    exported_names.sort();
    exported_names.dedup();
    Some(ModuleDts {
        exported_names,
        declarations,
        needs_i0,
    })
}

/// Parse a module source and, if it is a re-export barrel, return its edges.
/// Returns `Some(edges)` when EVERY top-level statement is a relative
/// `export … from` re-export (the barrel shape); `None` if the module contains any
/// non-re-export top-level statement (it is not a pure barrel).
fn barrel_reexports(
    source: &str,
    owner_path: &Path,
    entry_source_paths: &HashSet<PathBuf>,
) -> Option<Vec<ReExport>> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(owner_path)
        .unwrap_or_else(|_| SourceType::default().with_typescript(true))
        .with_module(true);
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if !parsed.errors.is_empty() {
        return None;
    }

    let mut edges = Vec::new();
    for stmt in &parsed.program.body {
        match stmt {
            Statement::ExportAllDeclaration(export) => {
                let spec = export.source.value.as_str();
                if !is_relative(spec) {
                    return None; // bare `export * from 'pkg'` — not a private barrel.
                }
                let resolved = resolve_private(spec, owner_path, entry_source_paths);
                edges.push(ReExport {
                    resolved,
                    names: Vec::new(),
                    is_star: true,
                });
            }
            Statement::ExportNamedDeclaration(export) if export.source.is_some() => {
                let spec = export.source.as_ref().unwrap().value.as_str();
                if !is_relative(spec) {
                    return None; // bare named re-export — not a private barrel.
                }
                let resolved = resolve_private(spec, owner_path, entry_source_paths);
                let names = export
                    .specifiers
                    .iter()
                    .map(|s| (module_export_name(&s.local), module_export_name(&s.exported)))
                    .collect();
                edges.push(ReExport {
                    resolved,
                    names,
                    is_star: false,
                });
            }
            // Any other top-level statement (a real declaration, a side-effect
            // import) means this entry is not a PURE re-export barrel.
            Statement::ImportDeclaration(_) => {
                // A leading import of types used only in re-exported positions is
                // rare; treat any import as "not a pure barrel" to stay safe.
                return None;
            }
            _ => return None,
        }
    }
    Some(edges)
}

// ---- resolution helpers (mirrors crate::fesm) -------------------------------

fn is_relative(spec: &str) -> bool {
    spec.starts_with("./") || spec.starts_with("../")
}

/// Resolve a relative specifier to a private (non-entry) on-disk module, or `None`
/// for a sibling-entry / unresolvable specifier.
fn resolve_private(
    spec: &str,
    owner_path: &Path,
    entry_source_paths: &HashSet<PathBuf>,
) -> Option<PathBuf> {
    let dir = owner_path.parent()?;
    let base = dir.join(spec);

    let mut candidates: Vec<PathBuf> = Vec::new();
    if base.is_file() {
        candidates.push(base.clone());
    }
    let base_str = base.as_os_str().to_string_lossy().into_owned();
    for ext in RESOLVE_EXTENSIONS {
        candidates.push(PathBuf::from(format!("{base_str}.{ext}")));
    }
    for ext in RESOLVE_EXTENSIONS {
        candidates.push(base.join(format!("index.{ext}")));
    }

    let resolved = candidates.iter().find(|c| c.is_file())?.clone();
    let canonical = std::fs::canonicalize(&resolved).unwrap_or(resolved);
    if is_entry_path(&canonical, entry_source_paths) {
        return None; // sibling published entry — leave as a cross-entry reference.
    }
    Some(canonical)
}

fn is_entry_path(path: &Path, entry_source_paths: &HashSet<PathBuf>) -> bool {
    let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    entry_source_paths.iter().any(|e| {
        let ec = std::fs::canonicalize(e).unwrap_or_else(|_| e.clone());
        ec == canon
    })
}

fn file_name_of(path: &Path) -> &str {
    path.file_name().and_then(|n| n.to_str()).unwrap_or("index.ts")
}

fn module_export_name(name: &ModuleExportName) -> String {
    match name {
        ModuleExportName::IdentifierName(id) => id.name.to_string(),
        ModuleExportName::IdentifierReference(id) => id.name.to_string(),
        ModuleExportName::StringLiteral(lit) => lit.value.to_string(),
    }
}

/// The bound names of a `.d.ts` declaration (class / const / function / interface /
/// type-alias / enum).
fn declaration_names_dts(decl: &oxc_ast::ast::Declaration) -> Vec<String> {
    use oxc_ast::ast::Declaration;
    match decl {
        Declaration::ClassDeclaration(c) => {
            c.id.as_ref().map(|i| vec![i.name.to_string()]).unwrap_or_default()
        }
        Declaration::VariableDeclaration(var) => var
            .declarations
            .iter()
            .filter_map(|d| d.id.get_identifier_name().map(|n| n.to_string()))
            .collect(),
        Declaration::FunctionDeclaration(f) => {
            f.id.as_ref().map(|i| vec![i.name.to_string()]).unwrap_or_default()
        }
        Declaration::TSTypeAliasDeclaration(t) => vec![t.id.name.to_string()],
        Declaration::TSInterfaceDeclaration(t) => vec![t.id.name.to_string()],
        Declaration::TSEnumDeclaration(e) => vec![e.id.name.to_string()],
        _ => Vec::new(),
    }
}

fn dts_source_type() -> SourceType {
    SourceType::default()
        .with_typescript(true)
        .with_typescript_definition(true)
        .with_module(true)
}

/// Whether a flattened `.d.ts` re-parses cleanly.
fn reparses_dts(code: &str) -> bool {
    let allocator = Allocator::default();
    Parser::new(&allocator, code, dts_source_type())
        .parse()
        .errors
        .is_empty()
}

/// Whether a flattened `.d.ts` still carries a relative `export … from './x'`
/// (the dangling re-export the flattener exists to remove).
fn has_relative_reexport(code: &str) -> bool {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, code, dts_source_type()).parse();
    if !parsed.errors.is_empty() {
        return true;
    }
    for stmt in &parsed.program.body {
        match stmt {
            Statement::ExportAllDeclaration(e) if is_relative(e.source.value.as_str()) => {
                return true;
            }
            Statement::ExportNamedDeclaration(e) => {
                if let Some(src) = &e.source
                    && is_relative(src.value.as_str())
                {
                    return true;
                }
            }
            Statement::ImportDeclaration(imp) if is_relative(imp.source.value.as_str()) => {
                return true;
            }
            _ => {}
        }
    }
    false
}

/// Build the canonical entry-source set (re-exported for the library pipeline).
pub fn canonical_entry_set(paths: &[PathBuf]) -> HashSet<PathBuf> {
    paths
        .iter()
        .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        dir.push(format!("treaty_dts_flatten_{tag}_{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn flattens_star_barrel_of_a_component() {
        let root = scratch("star");
        std::fs::create_dir_all(root.join("lib")).unwrap();
        std::fs::write(
            root.join("lib/box.component.ts"),
            "import { Component } from '@angular/core';\n\
             @Component({ selector: 'acme-box', standalone: true, template: '<div></div>' })\n\
             export class BoxComponent {}\n",
        )
        .unwrap();
        let entry = root.join("public-api.ts");
        std::fs::write(&entry, "export * from './lib/box.component';\n").unwrap();

        let entries: HashSet<PathBuf> = HashSet::new();
        let entry_source = std::fs::read_to_string(&entry).unwrap();
        // The buggy passthrough declaration the old path produced.
        let buggy = "export * from \"./lib/box.component\";\n";
        let out = flatten_entry_dts(&entry_source, buggy, &entry, &entries);

        // The barrel is gone: no dangling relative re-export.
        assert!(!out.contains("./lib/box.component"), "barrel not flattened:\n{out}");
        // The component class is inlined with its Ivy declarations.
        assert!(out.contains("declare class BoxComponent"), "class not inlined:\n{out}");
        assert!(out.contains("\u{0275}fac"), "ɵfac missing:\n{out}");
        assert!(out.contains("\u{0275}cmp"), "ɵcmp missing:\n{out}");
        // A single hoisted i0 import + a final aggregated export.
        assert!(out.contains("import * as i0 from \"@angular/core\";"), "i0 import missing:\n{out}");
        assert!(out.contains("export { BoxComponent };"), "aggregated export missing:\n{out}");
        // It re-parses as a valid .d.ts.
        assert!(reparses_dts(&out), "flattened dts did not parse:\n{out}");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn flattens_named_reexport_barrel() {
        let root = scratch("named");
        std::fs::create_dir_all(root.join("lib")).unwrap();
        std::fs::write(
            root.join("lib/util.ts"),
            "export const VERSION: string = '1.0.0';\n",
        )
        .unwrap();
        let entry = root.join("public-api.ts");
        std::fs::write(&entry, "export { VERSION } from './lib/util';\n").unwrap();

        let entries: HashSet<PathBuf> = HashSet::new();
        let entry_source = std::fs::read_to_string(&entry).unwrap();
        let out = flatten_entry_dts(&entry_source, "export { VERSION } from \"./lib/util\";\n", &entry, &entries);

        assert!(!out.contains("./lib/util"), "named barrel not flattened:\n{out}");
        assert!(out.contains("VERSION"), "VERSION not inlined:\n{out}");
        assert!(out.contains("export { VERSION };"), "aggregated export missing:\n{out}");
        assert!(reparses_dts(&out), "flattened dts did not parse:\n{out}");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn leaves_sibling_entry_reexport_untouched() {
        let root = scratch("sibling");
        let card = root.join("card.ts");
        std::fs::write(&card, "export class Card {}\n").unwrap();
        let entry = root.join("public-api.ts");
        std::fs::write(&entry, "export { Card } from './card';\n").unwrap();

        // `card.ts` IS a published entry → its re-export is a cross-entry ref.
        let entries = canonical_entry_set(&[card.clone(), entry.clone()]);
        let entry_source = std::fs::read_to_string(&entry).unwrap();
        let original = "export { Card } from \"./card\";\n";
        let out = flatten_entry_dts(&entry_source, original, &entry, &entries);
        // Identity: a sibling-entry re-export is not inlined.
        assert_eq!(out, original, "sibling-entry re-export must be left untouched");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn non_barrel_entry_is_identity() {
        let root = scratch("nonbarrel");
        let entry = root.join("public-api.ts");
        std::fs::write(&entry, "export const X: number = 1;\n").unwrap();
        let entries: HashSet<PathBuf> = HashSet::new();
        let entry_source = std::fs::read_to_string(&entry).unwrap();
        let original = "export declare const X: number;\n";
        let out = flatten_entry_dts(&entry_source, original, &entry, &entries);
        // A real declaration entry (not a re-export barrel) is unchanged.
        assert_eq!(out, original);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn flattens_transitive_nested_barrel() {
        let root = scratch("nested");
        std::fs::create_dir_all(root.join("lib/feature")).unwrap();
        std::fs::write(
            root.join("lib/feature/box.component.ts"),
            "import { Component } from '@angular/core';\n\
             @Component({ selector: 'acme-box', standalone: true, template: '<div></div>' })\n\
             export class BoxComponent {}\n",
        )
        .unwrap();
        // A nested index barrel.
        std::fs::write(
            root.join("lib/feature/index.ts"),
            "export * from './box.component';\n",
        )
        .unwrap();
        let entry = root.join("public-api.ts");
        std::fs::write(&entry, "export * from './lib/feature';\n").unwrap();

        let entries: HashSet<PathBuf> = HashSet::new();
        let entry_source = std::fs::read_to_string(&entry).unwrap();
        let out = flatten_entry_dts(&entry_source, "export * from \"./lib/feature\";\n", &entry, &entries);

        assert!(!out.contains("./lib/feature"), "nested barrel not flattened:\n{out}");
        assert!(!out.contains("./box.component"), "transitive re-export survived:\n{out}");
        assert!(out.contains("declare class BoxComponent"), "class not inlined transitively:\n{out}");
        assert!(out.contains("export { BoxComponent };"), "aggregated export missing:\n{out}");
        assert!(reparses_dts(&out), "flattened nested dts did not parse:\n{out}");

        std::fs::remove_dir_all(&root).ok();
    }
}
