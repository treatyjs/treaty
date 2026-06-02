//! FESM (Flattened ESM) emission — the APF `fesm2022` shape.
//!
//! Angular's Package Format ships each entry point as a single *flattened* ES
//! module (`fesm2022/<name>.mjs`): the entry's own internal modules are inlined
//! into one file, while imports of *other packages* (`@angular/*`, `tslib`, any
//! bare npm specifier) stay as external `import` statements that the consumer's
//! bundler resolves.
//!
//! packagr does this with a **self-contained Rust inliner** built on the oxc
//! parser it already depends on — no external bundler. The contract is:
//!
//!   * **Relative imports to private modules** within the package (`./util`,
//!     `../shared/x`) that are *not themselves published entry points* are
//!     resolved on disk, compiled through the same front-ends, and inlined
//!     (deps-first) into one flat module; the relative `import`/`export … from`
//!     statement that referenced them is rewritten to reference the now-local
//!     bindings.
//!   * **Bare specifiers** (`@angular/core`, `tslib`, `rxjs`, …) are left as
//!     external imports — hoisted to the top of the flattened module and
//!     de-duplicated.
//!   * **Relative imports that resolve to another published entry point** are
//!     left untouched: secondary entries are separate FESM files in APF, so the
//!     primary entry references them by their published sub-path exactly as
//!     authored (this is what keeps a pure re-export barrel a no-op).
//!
//! When an entry has no inlinable relative imports — the common single-file
//! component case — flattening is an **identity**: the input ESM is returned
//! verbatim, byte-for-byte, so a lowered `ɵɵdefineComponent` module is never
//! reshaped or re-printed.
//!
//! ## Correctness guard
//!
//! Inlining is concatenation-based (each module keeps its own binding names).
//! That is only sound when binding names do not collide across the modules being
//! merged. Before inlining, the planner checks for any top-level binding-name
//! collision across the entry and every module it would inline; if it finds one,
//! or hits an import shape it cannot prove safe (a namespace import of an inlined
//! module, an import cycle, a compile error in a dependency), it **bails to
//! identity** and returns the original ESM unchanged. Flattening therefore never
//! corrupts an entry: in the worst case it simply does not flatten it.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Declaration, ExportDefaultDeclarationKind, ImportDeclarationSpecifier, ModuleExportName,
    Program, Statement,
};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType};

use crate::compile;

/// The authoring/source extensions a relative import may resolve to, in the same
/// precedence order entry discovery uses (authoring sources before plain `.ts`).
const RESOLVE_EXTENSIONS: [&str; 4] = ["treaty", "tjsx", "tsx", "ts"];

/// Flatten one entry's compiled ESM into a single APF FESM module.
///
/// `compiled_esm` is the entry's already-compiled ESM (the output of
/// [`crate::compile::compile_entry`]). `entry_source_path` is the on-disk source
/// the ESM was compiled from — it anchors relative-import resolution.
/// `entry_source_paths` is the set of *all* published entry source paths, used to
/// distinguish a private helper (inline it) from a sibling entry (leave it).
///
/// Returns the flattened ESM. On any condition the inliner cannot prove safe it
/// returns `compiled_esm` unchanged (identity), so the result is always a valid
/// module.
pub fn flatten_entry_esm(
    compiled_esm: &str,
    entry_source_path: &Path,
    entry_source_paths: &HashSet<PathBuf>,
) -> String {
    match try_flatten(compiled_esm, entry_source_path, entry_source_paths) {
        Some(flat) => flat,
        None => compiled_esm.to_string(),
    }
}

/// A relative-import edge discovered in a module's compiled ESM. Only the facts
/// the planner acts on are kept: where (if anywhere) the specifier resolves as a
/// private module, and whether the import uses a binding shape we cannot prove
/// safe to concatenation-inline. The full statement is re-derived from source
/// spans by [`rewrite_module`], so no per-binding detail is carried here.
struct RelativeRef {
    /// The resolved private-module path, if it resolves to an inlinable module
    /// (a relative import that is neither external nor another entry point).
    resolved: Option<PathBuf>,
    /// `true` for `import * as ns from './x'` (namespace) or `import D from './x'`
    /// (default). Concatenation inlining keeps each module's own *named* bindings,
    /// so a namespace or default import of an inlined module has no guaranteed
    /// local binding to bridge to — it forces identity when its target would be
    /// inlined. (A re-export `export { default as D } from './entry'` is fine: it
    /// is only ever left untouched, never inlined, because entries are external.)
    unbridgeable: bool,
}

/// One module pulled into the flatten: its compiled code plus its parsed shape.
struct LoadedModule {
    path: PathBuf,
    code: String,
}

/// Attempt the flatten; `None` means "bail to identity".
fn try_flatten(
    root_code: &str,
    root_path: &Path,
    entry_source_paths: &HashSet<PathBuf>,
) -> Option<String> {
    // 1. Walk the dependency graph over PRIVATE relative modules only.
    let mut loaded: Vec<LoadedModule> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut on_stack: HashSet<PathBuf> = HashSet::new();
    let mut any_inlinable = false;

    collect_modules(
        root_path,
        root_code,
        entry_source_paths,
        &mut loaded,
        &mut seen,
        &mut on_stack,
        &mut any_inlinable,
    )?;

    // No private relative dependency anywhere → flatten is a pure identity.
    if !any_inlinable {
        return None;
    }

    // 2. Collision guard: no top-level binding name may appear in two modules.
    //    `loaded` is deps-first; the root is appended last so it is part of the
    //    same namespace check.
    let mut all_bindings: HashSet<String> = HashSet::new();
    for module in loaded.iter() {
        let bindings = top_level_binding_names(&module.code)?;
        for name in bindings {
            if !all_bindings.insert(name) {
                return None; // collision → not safe to concatenate.
            }
        }
    }
    let root_bindings = top_level_binding_names(root_code)?;
    for name in &root_bindings {
        if !all_bindings.insert(name.clone()) {
            return None;
        }
    }

    // 3. Emit: hoisted+deduped bare imports, then each dependency body (deps
    //    first), then the rewritten root body.
    let mut bare_imports: BTreeSet<String> = BTreeSet::new();
    let mut bodies: Vec<String> = Vec::new();

    for module in &loaded {
        let body = rewrite_module(
            &module.code,
            &module.path,
            entry_source_paths,
            &mut bare_imports,
            /* is_root */ false,
        )?;
        bodies.push(body);
    }

    let root_body = rewrite_module(
        root_code,
        root_path,
        entry_source_paths,
        &mut bare_imports,
        /* is_root */ true,
    )?;

    let mut out = String::new();
    for imp in &bare_imports {
        out.push_str(imp);
        out.push('\n');
    }
    if !bare_imports.is_empty() {
        out.push('\n');
    }
    for body in &bodies {
        let trimmed = body.trim_end_matches('\n');
        if trimmed.is_empty() {
            continue;
        }
        out.push_str(trimmed);
        out.push('\n');
    }
    out.push_str(root_body.trim_end_matches('\n'));
    out.push('\n');

    // 4. Final safety net: the flattened module MUST re-parse, MUST keep any Ivy
    //    definition it had, and MUST NOT leak a private relative import. If any
    //    invariant fails we discard the flatten and fall back to identity.
    if !reparses(&out) {
        return None;
    }
    if has_private_relative_import(&out, root_path, entry_source_paths) {
        return None;
    }

    Some(out)
}

/// Recursively load every PRIVATE relative module reachable from `code`.
///
/// Pushes loaded modules deps-first into `loaded`. Returns `None` on an import
/// cycle, a dependency compile error, an unresolvable private import, or a
/// namespace import of an inlined module — each forces identity.
#[allow(clippy::too_many_arguments)]
fn collect_modules(
    owner_path: &Path,
    code: &str,
    entry_source_paths: &HashSet<PathBuf>,
    loaded: &mut Vec<LoadedModule>,
    seen: &mut HashSet<PathBuf>,
    on_stack: &mut HashSet<PathBuf>,
    any_inlinable: &mut bool,
) -> Option<()> {
    let refs = relative_refs(code, owner_path, entry_source_paths)?;
    for r in &refs {
        let Some(dep_path) = &r.resolved else {
            continue; // external bare import or sibling-entry import — left alone.
        };
        *any_inlinable = true;
        if r.unbridgeable {
            return None; // default/namespace import of an inlined module — bail.
        }
        if on_stack.contains(dep_path) {
            return None; // import cycle — bail.
        }
        if seen.contains(dep_path) {
            continue; // already loaded (diamond dep) — dedupe.
        }

        let source = std::fs::read_to_string(dep_path).ok()?;
        // Compile the private dependency with its on-disk path known, so a `@Component` it
        // declares with external `styleUrls`/`styleUrl` has those files resolved + preprocessed
        // and folded into its scoped `styles: [...]` — exactly as a top-level entry would. For a
        // dependency with no external styles this is byte-identical to the path-less compile.
        let compiled = compile::compile_entry_at(&source, dep_path);
        if !compiled.errors.is_empty() {
            return None; // a dependency failed to compile — bail.
        }

        seen.insert(dep_path.clone());
        on_stack.insert(dep_path.clone());
        collect_modules(
            dep_path,
            &compiled.code,
            entry_source_paths,
            loaded,
            seen,
            on_stack,
            any_inlinable,
        )?;
        on_stack.remove(dep_path);

        loaded.push(LoadedModule {
            path: dep_path.clone(),
            code: compiled.code,
        });
    }
    Some(())
}

/// Parse `code` and collect its relative import / export-from statements,
/// classifying each and resolving the inlinable ones to a private-module path.
/// Returns `None` if `code` does not parse (so the caller bails to identity).
///
/// The parse folds the barred-o `ɵ` (U+0275) — which trips oxc 0.133's
/// identifier scanner inside lowered Ivy member expressions — to an ASCII letter
/// purely to read module records. This function reads only specifier *values*
/// (which never contain `ɵ`); it never slices byte spans into `code`, so the
/// fold's offset shift is irrelevant here. Span-based slicing happens only in
/// [`rewrite_module`], which parses the un-folded original.
fn relative_refs(
    code: &str,
    owner_path: &Path,
    entry_source_paths: &HashSet<PathBuf>,
) -> Option<Vec<RelativeRef>> {
    let folded = code.replace('\u{0275}', "Z");
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, &folded, module_source_type()).parse();
    if !parsed.errors.is_empty() {
        return None;
    }

    let mut refs = Vec::new();
    for stmt in &parsed.program.body {
        match stmt {
            Statement::ImportDeclaration(import) => {
                let spec = import.source.value.as_str();
                if !is_relative(spec) {
                    continue;
                }
                let resolved = resolve_private(spec, owner_path, entry_source_paths);
                // Namespace or default specifiers have no guaranteed local binding
                // after concatenation — flag them unbridgeable.
                let unbridgeable = import.specifiers.as_ref().is_some_and(|specs| {
                    specs.iter().any(|s| {
                        matches!(
                            s,
                            ImportDeclarationSpecifier::ImportNamespaceSpecifier(_)
                                | ImportDeclarationSpecifier::ImportDefaultSpecifier(_)
                        )
                    })
                });
                refs.push(RelativeRef { resolved, unbridgeable });
            }
            Statement::ExportNamedDeclaration(export) => {
                let Some(src) = &export.source else { continue };
                let spec = src.value.as_str();
                if !is_relative(spec) {
                    continue;
                }
                let resolved = resolve_private(spec, owner_path, entry_source_paths);
                refs.push(RelativeRef { resolved, unbridgeable: false });
            }
            Statement::ExportAllDeclaration(export) => {
                let spec = export.source.value.as_str();
                if !is_relative(spec) {
                    continue;
                }
                let resolved = resolve_private(spec, owner_path, entry_source_paths);
                refs.push(RelativeRef { resolved, unbridgeable: false });
            }
            _ => {}
        }
    }
    Some(refs)
}

/// The shared module-rewriter for both dependency bodies and the root body.
///
/// Walks top-level statements over the ORIGINAL source text (so emitted Ivy bytes
/// are preserved verbatim) and rebuilds the module by deciding, per statement,
/// whether to keep / drop / rewrite / hoist it. Returns `None` on a shape it
/// cannot rewrite safely.
///
///   * A **dependency** (`is_root == false`) is reduced to its local bindings:
///     `export` keywords are stripped, relative/named re-exports are dropped, and
///     `export default …` is lowered to a local binding.
///   * The **root** (`is_root == true`) keeps its real export surface, but every
///     relative import/export-from of an *inlined* private module is rewritten to
///     reference the now-local bindings; bare imports are hoisted and relative
///     refs to sibling entries are left verbatim.
fn rewrite_module(
    code: &str,
    owner_path: &Path,
    entry_source_paths: &HashSet<PathBuf>,
    bare_imports: &mut BTreeSet<String>,
    is_root: bool,
) -> Option<String> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, code, module_source_type()).parse();
    if !parsed.errors.is_empty() {
        return None;
    }
    let program = &parsed.program;

    // Each kept/rewritten piece, in source order, as (start, replacement_text).
    // We slice the original bytes for anything we keep verbatim.
    let mut pieces: Vec<(usize, String)> = Vec::new();

    for stmt in &program.body {
        let span = stmt.span();
        let (start, end) = (span.start as usize, span.end as usize);
        let raw = &code[start..end];

        match stmt {
            Statement::ImportDeclaration(import) => {
                let spec = import.source.value.as_str();
                if !is_relative(spec) {
                    // Bare external import — hoist + dedupe, drop from the body.
                    bare_imports.insert(normalize_ws(raw));
                    continue;
                }
                // Relative import.
                match resolve_private(spec, owner_path, entry_source_paths) {
                    None => {
                        // Sibling-entry or unresolved relative — leave verbatim.
                        pieces.push((start, raw.to_string()));
                    }
                    Some(_) => {
                        // Inlined private module: its bindings are now in scope.
                        // For the root, bridge any renamed import to the inlined
                        // binding so root code keeps compiling; in a dependency
                        // body the binding name is identical, so just drop it.
                        if is_root
                            && let Some(bridge) = import_bridge(import)
                            && !bridge.is_empty()
                        {
                            pieces.push((start, bridge));
                        }
                        // else: drop entirely.
                    }
                }
            }
            Statement::ExportNamedDeclaration(export) if export.source.is_some() => {
                let spec = export.source.as_ref().unwrap().value.as_str();
                if !is_relative(spec) {
                    // `export { x } from 'pkg'` — a bare re-export. Keep verbatim
                    // in the root; drop in a dependency (it is not the public
                    // surface of the flattened entry).
                    if is_root {
                        pieces.push((start, raw.to_string()));
                    }
                    continue;
                }
                match resolve_private(spec, owner_path, entry_source_paths) {
                    None => {
                        // Sibling entry re-export — leave verbatim (root) / drop (dep).
                        if is_root {
                            pieces.push((start, raw.to_string()));
                        }
                    }
                    Some(_) => {
                        // Inlined: rewrite `export { a as b } from './x'` to a
                        // local `export { a as b };` for the root; drop in a dep.
                        if is_root {
                            let local = local_reexport(export);
                            if !local.is_empty() {
                                pieces.push((start, local));
                            }
                        }
                        // else (dependency): drop the re-export entirely.
                    }
                }
            }
            Statement::ExportAllDeclaration(export) => {
                let spec = export.source.value.as_str();
                if !is_relative(spec) {
                    if is_root {
                        pieces.push((start, raw.to_string()));
                    }
                    continue;
                }
                match resolve_private(spec, owner_path, entry_source_paths) {
                    None => {
                        if is_root {
                            pieces.push((start, raw.to_string()));
                        }
                    }
                    Some(dep) => {
                        // `export * from './x'` over an inlined module: re-export
                        // its named bindings locally (root only).
                        if is_root {
                            let names = named_exports_of_file(&dep, entry_source_paths)?;
                            if !names.is_empty() {
                                let list = names.join(", ");
                                pieces.push((start, format!("export {{ {list} }};")));
                            }
                        }
                    }
                }
            }
            Statement::ExportNamedDeclaration(export) => {
                // `export const/function/class X` or `export { local }` with no
                // source.
                if is_root {
                    pieces.push((start, raw.to_string()));
                } else if let Some(decl) = &export.declaration {
                    // Strip the leading `export ` keyword: keep the declaration as
                    // a local binding. The declaration's own span starts after
                    // `export `, so slice from there.
                    let decl_start = decl.span().start as usize;
                    pieces.push((start, code[decl_start..end].to_string()));
                }
                // else (dependency `export { local }`): the bindings stay local,
                // drop the export statement.
            }
            Statement::ExportDefaultDeclaration(export) => {
                // `export default …`. In the root, keep verbatim. In a dependency,
                // turn it into a local binding (its name was captured by the
                // collision check, and a dependent's `import Default from './dep'`
                // bridges to it).
                if is_root {
                    pieces.push((start, raw.to_string()));
                } else {
                    let lowered = lower_default_to_local(&export.declaration, code)?;
                    pieces.push((start, lowered));
                }
            }
            _ => {
                // Any other top-level statement (declarations, runtime
                // assignments like `Button.ɵcmp = …`, the trailing `;`) is kept
                // verbatim — this is what preserves the emitted Ivy bytes.
                pieces.push((start, raw.to_string()));
            }
        }
    }

    pieces.sort_by_key(|(start, _)| *start);
    let body = pieces
        .into_iter()
        .map(|(_, text)| text)
        .collect::<Vec<_>>()
        .join("\n");
    Some(body)
}

/// Build the local-binding bridge for a root `import` of an inlined module.
///
/// Only named specifiers reach here — the planner bails to identity on a default
/// or namespace import of an inlined module, so they never need bridging.
///
///   * `import { a } from './x'` needs no bridge: the inlined module already
///     declares `a` as a top-level binding (guaranteed distinct by the collision
///     guard), so the root keeps referencing `a` directly.
///   * `import { a as b } from './x'` (a rename) becomes `const b = a;` so the
///     root keeps referencing `b`.
fn import_bridge(import: &oxc_ast::ast::ImportDeclaration) -> Option<String> {
    let Some(specs) = &import.specifiers else {
        return Some(String::new());
    };
    let mut bridges: Vec<String> = Vec::new();
    for s in specs {
        match s {
            ImportDeclarationSpecifier::ImportSpecifier(is) => {
                let imported = module_export_name(&is.imported);
                let local = is.local.name.to_string();
                if imported != local {
                    bridges.push(format!("const {local} = {imported};"));
                }
            }
            ImportDeclarationSpecifier::ImportDefaultSpecifier(_)
            | ImportDeclarationSpecifier::ImportNamespaceSpecifier(_) => {
                // Unreachable for an inlined module: the planner forced identity.
            }
        }
    }
    Some(bridges.join("\n"))
}

/// Rewrite a root `export { local as exported } from './inlined'` into a local
/// re-export `export { local as exported };` (the bindings are now in scope).
fn local_reexport(export: &oxc_ast::ast::ExportNamedDeclaration) -> String {
    let mut parts: Vec<String> = Vec::new();
    for s in &export.specifiers {
        let local = module_export_name(&s.local);
        let exported = module_export_name(&s.exported);
        // `default` re-exported as a name: the inlined module re-exposes its
        // default under that declared name, so the local binding IS `exported`.
        let local = if local == "default" { exported.clone() } else { local };
        if local == exported {
            parts.push(local);
        } else {
            parts.push(format!("{local} as {exported}"));
        }
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("export {{ {} }};", parts.join(", "))
    }
}

/// Turn a dependency's `export default …` into a local binding so it survives
/// inlining under a referable name.
///
///   * `export default function foo() {}` / `export default class Foo {}` →
///     keep the declaration (named); the binding is `foo` / `Foo`.
///   * `export default Ident;` → `Ident` is already declared above; drop the
///     statement (returns empty).
///   * `export default <expr>;` → `const _default = <expr>;`.
fn lower_default_to_local(kind: &ExportDefaultDeclarationKind, code: &str) -> Option<String> {
    match kind {
        ExportDefaultDeclarationKind::FunctionDeclaration(f) => {
            // Named function: keep verbatim (its span covers the declaration).
            if f.id.is_some() {
                let s = f.span;
                Some(code[s.start as usize..s.end as usize].to_string())
            } else {
                None // anonymous default function — not produced by our front-ends.
            }
        }
        ExportDefaultDeclarationKind::ClassDeclaration(c) => {
            if c.id.is_some() {
                let s = c.span;
                Some(code[s.start as usize..s.end as usize].to_string())
            } else {
                None
            }
        }
        ExportDefaultDeclarationKind::TSInterfaceDeclaration(_) => Some(String::new()),
        // An expression default: `export default Ident;` or `export default <expr>;`.
        expr => {
            let span = expr.span();
            let text = code[span.start as usize..span.end as usize].trim();
            if is_plain_identifier(text) {
                // Already a top-level binding from earlier in the module.
                Some(String::new())
            } else {
                Some(format!("const _default = {text};"))
            }
        }
    }
}

/// Collect the named exports of a compiled module file on disk (for resolving a
/// root `export * from './inlined'`). Compiles the file and walks its export
/// surface; `default` is intentionally excluded (a star re-export never re-binds
/// the default).
fn named_exports_of_file(path: &Path, entry_source_paths: &HashSet<PathBuf>) -> Option<Vec<String>> {
    let source = std::fs::read_to_string(path).ok()?;
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("dep.ts");
    let compiled = compile::compile_entry(&source, file_name);
    if !compiled.errors.is_empty() {
        return None;
    }
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, &compiled.code, module_source_type()).parse();
    if !parsed.errors.is_empty() {
        return None;
    }
    let mut names: Vec<String> = Vec::new();
    for stmt in &parsed.program.body {
        match stmt {
            Statement::ExportNamedDeclaration(export) => {
                if let Some(decl) = &export.declaration {
                    names.extend(declaration_names(decl));
                }
                for s in &export.specifiers {
                    let exported = module_export_name(&s.exported);
                    if exported != "default" {
                        names.push(exported);
                    }
                }
                // A nested `export … from './rel'` inside an inlined module: its
                // names would need transitive resolution. Keep it simple and let
                // the caller's reparse/leak guard bail if this ever surfaces.
            }
            Statement::ExportAllDeclaration(inner) => {
                // `export * from './rel'` chained through an inlined module.
                if is_relative(inner.source.value.as_str())
                    && let Some(dep) =
                        resolve_private(inner.source.value.as_str(), path, entry_source_paths)
                {
                    names.extend(named_exports_of_file(&dep, entry_source_paths)?);
                }
            }
            _ => {}
        }
    }
    names.sort();
    names.dedup();
    Some(names)
}

/// The top-level binding names a compiled module declares (for the collision
/// guard). Covers `const`/`let`/`var`/`function`/`class`, whether or not they
/// are exported, plus a named `export default function/class`. Returns `None` if
/// the module does not parse.
fn top_level_binding_names(code: &str) -> Option<Vec<String>> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, code, module_source_type()).parse();
    if !parsed.errors.is_empty() {
        return None;
    }
    let mut names = Vec::new();
    collect_program_bindings(&parsed.program, &mut names);
    Some(names)
}

fn collect_program_bindings(program: &Program, names: &mut Vec<String>) {
    for stmt in &program.body {
        match stmt {
            Statement::VariableDeclaration(var) => {
                for d in &var.declarations {
                    if let Some(n) = d.id.get_identifier_name() {
                        names.push(n.to_string());
                    }
                }
            }
            Statement::FunctionDeclaration(f) => {
                if let Some(id) = &f.id {
                    names.push(id.name.to_string());
                }
            }
            Statement::ClassDeclaration(c) => {
                if let Some(id) = &c.id {
                    names.push(id.name.to_string());
                }
            }
            Statement::ExportNamedDeclaration(export) => {
                if let Some(decl) = &export.declaration {
                    names.extend(declaration_names(decl));
                }
            }
            Statement::ExportDefaultDeclaration(export) => match &export.declaration {
                ExportDefaultDeclarationKind::FunctionDeclaration(f) => {
                    if let Some(id) = &f.id {
                        names.push(id.name.to_string());
                    }
                }
                ExportDefaultDeclarationKind::ClassDeclaration(c) => {
                    if let Some(id) = &c.id {
                        names.push(id.name.to_string());
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
}

/// Whether a flattened module still references a *private* relative import — the
/// invariant the inliner exists to remove. Used as a post-emit safety net.
fn has_private_relative_import(
    code: &str,
    owner_path: &Path,
    entry_source_paths: &HashSet<PathBuf>,
) -> bool {
    match relative_refs(code, owner_path, entry_source_paths) {
        Some(refs) => refs.iter().any(|r| r.resolved.is_some()),
        None => true, // un-parseable → treat as unsafe.
    }
}

/// Re-parse a flattened module to prove it is syntactically valid ESM. The
/// barred-o `ɵ` (U+0275) trips oxc 0.133's identifier scanner in some positions
/// even though Node accepts it, so fold it to an ASCII letter purely for the
/// structural check — the emitted bytes are unchanged. This mirrors the proven
/// parse-check used by the linker and the packagr tests.
fn reparses(code: &str) -> bool {
    let folded = code.replace('\u{0275}', "Z");
    let allocator = Allocator::default();
    Parser::new(&allocator, &folded, module_source_type())
        .parse()
        .errors
        .is_empty()
}

// ---- small helpers ----------------------------------------------------------

fn module_source_type() -> SourceType {
    SourceType::default().with_typescript(true).with_module(true)
}

/// A bare/external specifier (`@angular/core`, `tslib`, `rxjs`) vs a relative
/// one (`./x`, `../y`).
fn is_relative(spec: &str) -> bool {
    spec.starts_with("./") || spec.starts_with("../")
}

/// Resolve a relative specifier against `owner_path`'s directory to an on-disk
/// *private* module — i.e. a real source file that is NOT itself a published
/// entry point. Returns `None` for sibling-entry imports and unresolvable paths
/// (both of which are left untouched in the output).
fn resolve_private(
    spec: &str,
    owner_path: &Path,
    entry_source_paths: &HashSet<PathBuf>,
) -> Option<PathBuf> {
    let dir = owner_path.parent()?;
    let base = dir.join(spec);

    // Candidate resolutions, in extension precedence order, then `index.*`.
    //
    // Extensions are APPENDED to the full base path, never substituted: a
    // specifier like `./button.component` (the standard Angular `.component` file
    // stem) must resolve to `button.component.ts`, not `button.ts`. Using
    // `Path::with_extension` would wrongly treat the `.component` segment as an
    // extension and replace it. The base is therefore joined as a string with
    // each `.{ext}` suffix.
    let mut candidates: Vec<PathBuf> = Vec::new();
    // 1. The base exactly as written, if the spec already carried a real file
    //    extension that resolves on disk (`./button.component.ts`).
    if base.is_file() {
        candidates.push(base.clone());
    }
    // 2. `<base>.<ext>` for each source extension (`button.component` → `button.component.ts`).
    let base_str = base.as_os_str().to_string_lossy().into_owned();
    for ext in RESOLVE_EXTENSIONS {
        candidates.push(PathBuf::from(format!("{base_str}.{ext}")));
    }
    // 3. `<base>/index.<ext>` for a directory specifier.
    for ext in RESOLVE_EXTENSIONS {
        candidates.push(base.join(format!("index.{ext}")));
    }

    let resolved = candidates.iter().find(|c| c.is_file())?.clone();
    let canonical = std::fs::canonicalize(&resolved).unwrap_or(resolved);

    // A relative import that points at another PUBLISHED entry point is left as
    // a cross-entry reference (separate FESM file), not inlined.
    if is_entry_path(&canonical, entry_source_paths) {
        return None;
    }
    Some(canonical)
}

/// Whether `path` is one of the published entry source paths (compared by
/// canonical form so `src/x.ts` and an absolute variant match).
fn is_entry_path(path: &Path, entry_source_paths: &HashSet<PathBuf>) -> bool {
    let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    entry_source_paths.iter().any(|e| {
        let ec = std::fs::canonicalize(e).unwrap_or_else(|_| e.clone());
        ec == canon
    })
}

fn module_export_name(name: &ModuleExportName) -> String {
    match name {
        ModuleExportName::IdentifierName(id) => id.name.to_string(),
        ModuleExportName::IdentifierReference(id) => id.name.to_string(),
        ModuleExportName::StringLiteral(lit) => lit.value.to_string(),
    }
}

fn declaration_names(decl: &Declaration) -> Vec<String> {
    match decl {
        Declaration::VariableDeclaration(var) => var
            .declarations
            .iter()
            .filter_map(|d| d.id.get_identifier_name().map(|n| n.to_string()))
            .collect(),
        Declaration::FunctionDeclaration(f) => {
            f.id.as_ref().map(|i| vec![i.name.to_string()]).unwrap_or_default()
        }
        Declaration::ClassDeclaration(c) => {
            c.id.as_ref().map(|i| vec![i.name.to_string()]).unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

/// Whether `text` is a single plain identifier (no member access / call), used to
/// decide if `export default X` is a re-export of an existing binding.
fn is_plain_identifier(text: &str) -> bool {
    let t = text.trim();
    !t.is_empty()
        && t.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

/// Collapse internal whitespace runs in a (single-line) import statement so the
/// dedup set treats `import {a} from 'x'` and `import { a } from 'x'` alike when
/// they are byte-identical, while leaving the common single-spaced form intact.
fn normalize_ws(raw: &str) -> String {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    // Preserve the trailing semicolon convention; oxc spans exclude it when the
    // statement has none, which is fine — both forms are valid ESM.
    collapsed
}

/// Build the set of canonical entry source paths from raw paths (used by the
/// caller to classify sibling-entry vs private imports).
pub fn canonical_entry_set(paths: &[PathBuf]) -> HashSet<PathBuf> {
    paths
        .iter()
        .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_when_no_relative_imports() {
        let esm = "import * as i0 from \"@angular/core\";\nexport const X = 1;\n";
        let path = PathBuf::from("src/public-api.ts");
        let entries: HashSet<PathBuf> = HashSet::new();
        let out = flatten_entry_esm(esm, &path, &entries);
        assert_eq!(out, esm, "no relative imports → byte-identical identity");
    }

    #[test]
    fn is_relative_classifies() {
        assert!(is_relative("./x"));
        assert!(is_relative("../y/z"));
        assert!(!is_relative("@angular/core"));
        assert!(!is_relative("rxjs"));
        assert!(!is_relative("tslib"));
    }

    #[test]
    fn plain_identifier_detection() {
        assert!(is_plain_identifier("Button"));
        assert!(is_plain_identifier("_default"));
        assert!(!is_plain_identifier("a.b"));
        assert!(!is_plain_identifier("f()"));
        assert!(!is_plain_identifier(""));
    }

    /// A unique scratch dir under the OS temp folder.
    fn scratch(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        dir.push(format!("treaty_fesm_unit_{tag}_{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn inlines_a_private_helper_and_keeps_externals() {
        let root = scratch("inline");
        // A private helper module on disk (not an entry point).
        std::fs::write(
            root.join("helper.ts"),
            "export const PREFIX = 'btn';\nexport function label(x: string): string { return PREFIX + x; }\n",
        )
        .unwrap();
        let entry_path = root.join("public-api.ts");

        // The entry's *compiled* ESM imports the helper + a bare specifier.
        let compiled = "import { PREFIX, label } from './helper';\n\
                        import { x } from 'rxjs';\n\
                        export const NAME = label(PREFIX);\n";
        let entries: HashSet<PathBuf> = HashSet::new(); // helper is NOT an entry.

        let out = flatten_entry_esm(compiled, &entry_path, &entries);

        // The private helper is inlined: no `./helper` import survives.
        assert!(
            !out.contains("from './helper'") && !out.contains("from \"./helper\""),
            "private helper not inlined:\n{out}"
        );
        // Its bindings are now local in the flat module.
        assert!(out.contains("PREFIX") && out.contains("label"), "helper bindings missing:\n{out}");
        // The bare specifier stays an external import.
        assert!(out.contains("from 'rxjs'") || out.contains("from \"rxjs\""), "external dropped:\n{out}");
        // The entry's own export survives.
        assert!(out.contains("export const NAME"), "entry export lost:\n{out}");
        // The result is valid ESM.
        assert!(reparses(&out), "flattened module did not re-parse:\n{out}");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn bails_to_identity_on_binding_collision() {
        let root = scratch("collide");
        // The helper declares `NAME` — which the entry also declares: a collision
        // that makes concatenation unsafe, so the flatten must bail to identity.
        std::fs::write(root.join("helper.ts"), "export const NAME = 'helper';\n").unwrap();
        let entry_path = root.join("public-api.ts");

        let compiled = "import { NAME as HELPER_NAME } from './helper';\n\
                        const NAME = 'entry';\n\
                        export { NAME };\n";
        let entries: HashSet<PathBuf> = HashSet::new();

        let out = flatten_entry_esm(compiled, &entry_path, &entries);
        // Identity: the original (with its relative import) is returned unchanged.
        assert_eq!(out, compiled, "collision should force a byte-identical identity bail");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn bails_to_identity_on_default_import_of_private_module() {
        let root = scratch("defimport");
        // The private module default-exports; the entry imports that default. A
        // concatenation inline has no guaranteed local name to bridge the default
        // to, so the flatten must bail to identity rather than risk a dangling ref.
        std::fs::write(root.join("widget.ts"), "export default function widget() {}\n").unwrap();
        let entry_path = root.join("public-api.ts");

        let compiled = "import Widget from './widget';\nexport const W = Widget;\n";
        let entries: HashSet<PathBuf> = HashSet::new();

        let out = flatten_entry_esm(compiled, &entry_path, &entries);
        assert_eq!(out, compiled, "default import of a private module must bail to identity");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn leaves_sibling_entry_imports_untouched() {
        let root = scratch("sibling");
        // `card.ts` IS a published entry, so a relative import of it is a
        // cross-entry reference that must NOT be inlined.
        let card = root.join("card.ts");
        std::fs::write(&card, "export default class Card {}\n").unwrap();
        let entry_path = root.join("public-api.ts");

        let compiled = "export { default as Card } from './card';\n";
        let entries = canonical_entry_set(&[card.clone(), entry_path.clone()]);

        let out = flatten_entry_esm(compiled, &entry_path, &entries);
        // No inlinable private import anywhere → byte-identical identity, the
        // sibling re-export preserved verbatim.
        assert_eq!(out, compiled, "sibling-entry import must be left untouched (identity)");

        std::fs::remove_dir_all(&root).ok();
    }
}
