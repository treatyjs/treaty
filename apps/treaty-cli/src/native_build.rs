//! `treaty build` — a Rust-native production build.
//!
//! A real module-graph bundler, in Rust, with no Node and no external bundler in
//! the hot path:
//!
//!   1. Crawl the static import graph from the entry (`oxc_resolver`), in process.
//!   2. Compile every first-party `.ts`/`.treaty`/`.tsx` to Ivy ESM via
//!      [`crate::transform::lower`] (Ivy lowering + oxc type-strip).
//!   3. Link every *partial* `@angular/*` library it pulls in to AOT via the
//!      shared Rust linker ([`treaty_ivy::link_partial`]) — so the dist needs NO
//!      JIT and NO `@angular/compiler`, the exact guarantee the bundler plugins
//!      give.
//!   4. Emit a self-contained dist: one ESM file per graph module under
//!      `dist/_treaty/`, every import rewritten to its sibling dist file, plus an
//!      `index.html` that boots the entry.
//!
//! The output is a directory of native ES modules the browser loads directly (no
//! concatenation needed — modern browsers load ESM graphs natively, which is also
//! what makes the dist bootable in jsdom). This is deliberately the *correctness*
//! bundler: it produces output that RUNS. Minification, tree-shaking, and
//! single-file concatenation are the noted follow-ups; for those, the same crawl
//! feeds the `--bundler rolldown`/`vite` external path (which the CLI already
//! wires) unchanged.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};

use crate::resolve::{is_under_node_modules, ModuleResolver};
use crate::selectors::{self, ProjectSelectors};
use crate::transform::{lower_with_registry, rewrite_imports};

/// The result of a native build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeBuildOutput {
    /// Files written, absolute paths (the index.html first, then modules).
    pub written: Vec<PathBuf>,
    /// Human-readable notes (module count, residual-partial count, etc.).
    pub notes: Vec<String>,
}

/// Options for a native build.
#[derive(Debug, Clone)]
pub struct NativeBuildOptions {
    /// App root (where `index.html` lives).
    pub root: PathBuf,
    /// Absolute entry module path.
    pub entry: PathBuf,
    /// Output directory.
    pub out_dir: PathBuf,
}

/// A processed graph module: its emitted code and the imports to rewrite.
struct GraphModule {
    /// Absolute source path (the cache/identity key).
    abs: PathBuf,
    /// Emitted ESM (Ivy-lowered + linked), imports NOT yet rewritten.
    code: String,
    /// Resolved (specifier -> absolute path) for each import in `code`.
    edges: BTreeMap<String, PathBuf>,
    /// The RAW first-party source (an authoring `.ts`/`.treaty`/`.tsx`), retained so a second pass
    /// can re-lower it with the project's cross-module selector registry. `None` for published /
    /// pass-through modules (which carry no `@Component` template to resolve).
    raw_source: Option<String>,
}

/// A stable, filesystem-safe output file name for an absolute module path.
///
/// We mirror nothing of the source tree (avoids `..`/drive-letter headaches);
/// instead each module gets `<stem>-<hash>.js`, unique by absolute path. The hash
/// is a cheap FNV-1a of the absolute path so two files with the same stem never
/// collide.
fn output_name(abs: &Path) -> String {
    let stem = abs
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "module".to_string());
    // Sanitise the stem to an identifier-ish token.
    let safe: String = stem
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in abs.to_string_lossy().replace('\\', "/").bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{safe}-{hash:016x}.js")
}

/// Crawl + compile + link the whole graph reachable from the entry.
///
/// Returns the processed modules keyed by absolute path. First-party sources are
/// Ivy-lowered + type-stripped; published partial Angular libs are linked to AOT;
/// other published ESM passes through. Every module's static import edges are
/// resolved so the emit step can rewrite them to sibling dist files.
fn build_graph(
    resolver: &ModuleResolver,
    entry: &Path,
) -> Result<BTreeMap<PathBuf, GraphModule>, String> {
    let mut graph: BTreeMap<PathBuf, GraphModule> = BTreeMap::new();
    let mut queue: VecDeque<PathBuf> = VecDeque::new();
    queue.push_back(entry.to_path_buf());

    // PROJECT SELECTOR SCAN (cross-module selector resolution). Accumulate every first-party `.ts`
    // class's `@Component`/`@Directive` selector as the crawl reads it, so by the end of the crawl we
    // hold the whole-project `className -> selector` map. File I/O / module resolution is the host's
    // job — the compiler never reads another file; it consumes the per-file `{ importName -> selector }`
    // registry we derive from this scan in the second pass below.
    let mut project_selectors: ProjectSelectors = ProjectSelectors::new();

    while let Some(abs) = queue.pop_front() {
        if graph.contains_key(&abs) {
            continue;
        }
        let source = std::fs::read_to_string(&abs)
            .map_err(|e| format!("read {}: {e}", abs.display()))?;
        let importer_dir = abs.parent().unwrap_or(entry).to_path_buf();

        // Scan a first-party `.ts` for its component/directive selectors (keyed by class name).
        if is_authoring(&abs) && selectors::is_scannable_ts(&abs) {
            selectors::scan_source_into(&source, &mut project_selectors);
        }

        // Lower/link to emit-ready code. First pass uses NO registry — the registry is not yet
        // complete (the crawl is still discovering files), and edges do not depend on it (a resolved
        // dependency is an already-imported symbol). The second pass re-lowers with the registry.
        let raw_source = if is_authoring(&abs) {
            Some(source.clone())
        } else {
            None
        };
        let code = if is_authoring(&abs) {
            let lowered = lower_with_registry(&source, &abs.to_string_lossy(), None);
            if !lowered.errors.is_empty() {
                return Err(format!(
                    "{}: {}",
                    abs.display(),
                    lowered.errors.join("; ")
                ));
            }
            lowered.code
        } else if is_under_node_modules(&abs) && source.contains("ɵɵngDeclare") {
            treaty_ivy::link_partial(&source, &abs.to_string_lossy()).code
        } else {
            source
        };

        // Resolve every import edge so emit can rewrite + the crawl can follow it.
        let mut edges = BTreeMap::new();
        let mut to_enqueue: Vec<PathBuf> = Vec::new();
        let _ = rewrite_imports(&code, |spec| {
            if let Some(resolved) = resolver.resolve(&importer_dir, spec) {
                edges.insert(spec.to_string(), resolved.clone());
                to_enqueue.push(resolved);
            }
            // We do not actually mutate here; rewrite happens in emit. Returning
            // None keeps the scan side-effect-only.
            None
        });
        // CROSS-MODULE DISCOVERY: also enqueue every import target named in the RAW (pre-lowering)
        // source. Ivy lowering + type-strip ELIDES an import that the FIRST pass (no registry) judged
        // unused — exactly a child component the parent references by its real selector (`<app-stat-
        // card>` for an imported `StatCard`), whose `dependencies[]` entry only appears once the
        // registry is applied. Following the lowered edges alone would therefore never reach that
        // child's file, so the selector scan would miss its `@Component.selector` and the registry
        // would be empty for the parent. Crawling the raw specifiers guarantees every first-party
        // module is read + scanned. (Discovery only — the EMIT still rewrites the lowered `edges`.)
        if let Some(raw) = raw_source.as_deref() {
            for spec in raw_import_specifiers(raw) {
                if let Some(resolved) = resolver.resolve(&importer_dir, &spec) {
                    if !graph.contains_key(&resolved) {
                        queue.push_back(resolved);
                    }
                }
            }
        }

        for dep in to_enqueue {
            if !graph.contains_key(&dep) {
                queue.push_back(dep);
            }
        }

        graph.insert(
            abs.clone(),
            GraphModule { abs, code, edges, raw_source },
        );
    }

    // SECOND PASS: now that `project_selectors` covers the WHOLE graph, re-lower each first-party
    // authoring module with its per-file `{ importName -> selector }` registry. A module whose
    // imports resolve to NO known selector gets `None` and is byte-identical to the first pass — so
    // this only changes the emit of a file that imports a component used by its REAL selector (the
    // cross-module case the registry exists for).
    //
    // The re-lowered code may RE-INTRODUCE an import the first (registry-free) pass elided as unused
    // (the child component now referenced in `dependencies[]`), so its import EDGES must be recomputed
    // from the new code — otherwise the emit's import rewrite would leave that import pointing at the
    // original source specifier instead of the sibling dist file. The newly-resolved target is already
    // in the graph (raw-source discovery enqueued it during the crawl).
    if !project_selectors.is_empty() {
        for module in graph.values_mut() {
            let Some(raw) = module.raw_source.as_deref() else {
                continue;
            };
            let Some(registry) = selectors::registry_for_source(raw, &project_selectors) else {
                continue;
            };
            let lowered =
                lower_with_registry(raw, &module.abs.to_string_lossy(), Some(&registry));
            if lowered.errors.is_empty() && !lowered.code.is_empty() {
                module.code = lowered.code;
                // Recompute edges from the re-lowered code so a re-introduced import rewrites to its
                // sibling dist file.
                let importer_dir = module
                    .abs
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| module.abs.clone());
                let mut edges = BTreeMap::new();
                let _ = rewrite_imports(&module.code, |spec| {
                    if let Some(resolved) = resolver.resolve(&importer_dir, spec) {
                        edges.insert(spec.to_string(), resolved);
                    }
                    None
                });
                module.edges = edges;
            }
        }
    }

    Ok(graph)
}

/// Count residual partial-declaration CALLS (`ɵɵngDeclareComponent(` etc.). A
/// call is an Ivy partial that did NOT get linked — the load-bearing "needs JIT"
/// signal. Bare identifiers (the names of core's own `ɵɵngDeclare*` exports) are
/// not calls and are ignored, mirroring the bench's `ɵɵngDeclare[A-Za-z]+\s*\(`.
fn count_ngdeclare_calls(code: &str) -> usize {
    let mut count = 0;
    let bytes = code.as_bytes();
    let needle = "ɵɵngDeclare";
    let mut from = 0;
    while let Some(pos) = code[from..].find(needle) {
        let start = from + pos;
        // Advance past the identifier (`ɵɵngDeclare` + following ident chars).
        let mut i = start + needle.len();
        while i < bytes.len() {
            let c = bytes[i] as char;
            if c.is_ascii_alphanumeric() || c == '_' {
                i += 1;
            } else {
                break;
            }
        }
        // Skip whitespace, then check for a `(` (a call).
        let mut j = i;
        while j < bytes.len() && (bytes[j] as char).is_whitespace() {
            j += 1;
        }
        if j < bytes.len() && bytes[j] == b'(' {
            count += 1;
        }
        from = i.max(start + needle.len());
    }
    count
}

/// Whether `code` actually IMPORTS `@angular/compiler` (a real JIT dependency),
/// as opposed to merely mentioning it inside a diagnostic string.
fn imports_angular_compiler(code: &str) -> bool {
    code.contains("from '@angular/compiler'")
        || code.contains("from \"@angular/compiler\"")
        || code.contains("import('@angular/compiler')")
        || code.contains("import(\"@angular/compiler\")")
}

fn is_authoring(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("ts") | Some("tsx") | Some("tjsx") | Some("treaty")
    )
}

/// Collect every STATIC import/export module specifier in a RAW TypeScript source (`from '…'`),
/// INCLUDING type-only imports — discovery must see a child even if its only reference is one the
/// lowering would later elide. Parse failures yield no specifiers (the file simply contributes no
/// extra discovery edges). Used only to widen the crawl so every first-party module is read + scanned
/// for selectors; it does not affect the emitted import rewrite (that still follows the lowered code).
fn raw_import_specifiers(source: &str) -> Vec<String> {
    use oxc_allocator::Allocator;
    use oxc_ast::ast::Statement;
    use oxc_parser::Parser;
    use oxc_span::SourceType;

    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true);
    let ret = Parser::new(&allocator, source, source_type).parse();
    if !ret.errors.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for stmt in &ret.program.body {
        match stmt {
            Statement::ImportDeclaration(decl) => out.push(decl.source.value.to_string()),
            Statement::ExportNamedDeclaration(decl) => {
                if let Some(src) = &decl.source {
                    out.push(src.value.to_string());
                }
            }
            Statement::ExportAllDeclaration(decl) => out.push(decl.source.value.to_string()),
            _ => {}
        }
    }
    out
}

/// Run a native build, writing a bootable dist to `out_dir`.
pub fn build(opts: &NativeBuildOptions) -> Result<NativeBuildOutput, String> {
    let resolver = ModuleResolver::new();
    let entry = opts.entry.clone();
    if !entry.exists() {
        return Err(format!("entry not found: {}", entry.display()));
    }

    let graph = build_graph(&resolver, &entry)?;

    let modules_dir = opts.out_dir.join("_treaty");
    std::fs::create_dir_all(&modules_dir).map_err(|e| format!("mkdir dist: {e}"))?;

    // Precompute every module's output file name.
    let mut names: BTreeMap<PathBuf, String> = BTreeMap::new();
    for abs in graph.keys() {
        names.insert(abs.clone(), output_name(abs));
    }

    let mut written = Vec::new();
    let mut residual_partials = 0usize;
    let mut imports_compiler = false;

    // Emit each module, rewriting its import edges to sibling dist files.
    for module in graph.values() {
        let body = rewrite_imports(&module.code, |spec| {
            module
                .edges
                .get(spec)
                .and_then(|dep| names.get(dep))
                .map(|name| format!("./{name}"))
        });

        // AOT health signals folded into the notes — using the SAME precise
        // patterns the bench's `inspectBundle` uses, so the note matches the boot
        // verdict: a residual partial is a `ɵɵngDeclare*(` CALL (not the identifier
        // that names the core export), and a compiler dependency is an actual
        // `from '@angular/compiler'` / `import('@angular/compiler')` (not the
        // identifier inside core's JIT-failure error-message string).
        residual_partials += count_ngdeclare_calls(&body);
        if imports_angular_compiler(&body) {
            imports_compiler = true;
        }

        let out_path = modules_dir.join(names.get(&module.abs).unwrap());
        std::fs::write(&out_path, &body).map_err(|e| format!("write module: {e}"))?;
        written.push(out_path);
    }

    // Emit index.html that boots the entry (its dist module).
    let entry_name = names
        .get(&entry)
        .ok_or_else(|| "entry was not in the build graph".to_string())?;
    let index_html = render_index(&opts.root, &format!("./_treaty/{entry_name}"));
    let index_path = opts.out_dir.join("index.html");
    std::fs::write(&index_path, index_html).map_err(|e| format!("write index.html: {e}"))?;
    // index.html first in the written list (convention).
    let mut all = vec![index_path];
    all.append(&mut written);

    let notes = vec![
        format!("native build: {} module(s) emitted", graph.len()),
        format!(
            "AOT health: residualNgDeclare={residual_partials} importsCompiler={imports_compiler} (both 0/false = no JIT needed)"
        ),
        "tree-shaking + minify + single-file concat are follow-ups (use --bundler rolldown for those)".to_string(),
    ];

    Ok(NativeBuildOutput { written: all, notes })
}

/// Render the dist `index.html`: reuse the app's own shell when present (so the
/// `<app-root>` / `<base href>` match), pointing its module script at the entry.
fn render_index(root: &Path, entry_rel: &str) -> String {
    let raw = std::fs::read_to_string(root.join("index.html")).unwrap_or_else(|_| {
        "<!doctype html><html><head><base href=\"/\"></head><body><app-root></app-root></body></html>".to_string()
    });
    let script = format!("\n<script type=\"module\" src=\"{entry_rel}\"></script>\n");
    if let Some(idx) = raw.rfind("</body>") {
        let mut out = String::with_capacity(raw.len() + script.len());
        out.push_str(&raw[..idx]);
        out.push_str(&script);
        out.push_str(&raw[idx..]);
        out
    } else {
        format!("{raw}{script}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_name_is_stable_and_unique() {
        let a = output_name(Path::new("/app/src/app.ts"));
        let b = output_name(Path::new("/app/src/app.ts"));
        let c = output_name(Path::new("/app/other/app.ts"));
        assert_eq!(a, b, "name not stable for same path");
        assert_ne!(a, c, "same stem different dir must differ");
        assert!(a.ends_with(".js"));
        assert!(a.starts_with("app-"));
    }

    #[test]
    fn builds_a_two_module_graph_to_bootable_esm() {
        let dir = std::env::temp_dir().join(format!("treaty-nb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // dep.ts <- entry main.ts
        std::fs::write(dir.join("dep.ts"), "export const dep = 41;").unwrap();
        std::fs::write(
            dir.join("main.ts"),
            "import { dep } from './dep';\nexport const answer = dep + 1;",
        )
        .unwrap();
        std::fs::write(
            dir.join("index.html"),
            "<!doctype html><html><body><app-root></app-root></body></html>",
        )
        .unwrap();

        let out = build(&NativeBuildOptions {
            root: dir.clone(),
            entry: dir.join("main.ts"),
            out_dir: dir.join("dist"),
        })
        .expect("build ok");

        // index.html + 2 modules.
        assert!(out.written.len() >= 3, "too few outputs: {:?}", out.written);
        let index = std::fs::read_to_string(dir.join("dist").join("index.html")).unwrap();
        assert!(index.contains("./_treaty/main-"), "index does not boot entry: {index}");

        // The entry's emitted module imports the dep by its sibling dist name.
        let modules_dir = dir.join("dist").join("_treaty");
        let entry_file = std::fs::read_dir(&modules_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .find(|e| e.file_name().to_string_lossy().starts_with("main-"))
            .map(|e| e.path())
            .unwrap();
        let entry_code = std::fs::read_to_string(&entry_file).unwrap();
        assert!(entry_code.contains("./dep-"), "edge not rewritten to sibling: {entry_code}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// CROSS-MODULE SELECTOR RESOLUTION end-to-end through the native build: a PARENT imports a CHILD
    /// component and uses it by the child's REAL `@Component` selector (`<app-stat-card>`), which does
    /// NOT fold to the imported class name (`StatCard`). The build must scan the child's selector, map
    /// the parent's import to it, resolve `StatCard` into the parent's `dependencies[]`, and rewrite
    /// the (re-introduced) import to the child's sibling dist file — so the parent renders the child
    /// rather than an empty host.
    #[test]
    fn native_build_resolves_imported_real_selector_dependency() {
        let dir = std::env::temp_dir().join(format!("treaty-nb-sel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Child: conventional Angular-CLI selector that does NOT fold to its class name.
        std::fs::write(
            dir.join("stat-card.ts"),
            "import { Component } from '@angular/core';\n\
             @Component({ selector: 'app-stat-card', template: '<p>card</p>' })\n\
             export class StatCard {}\n",
        )
        .unwrap();
        // Parent (the entry): imports the child by class name, uses it by its real selector.
        std::fs::write(
            dir.join("main.ts"),
            "import { Component } from '@angular/core';\n\
             import { StatCard } from './stat-card';\n\
             @Component({ selector: 'app-root', imports: [StatCard], template: '<app-stat-card></app-stat-card>' })\n\
             export class App {}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("index.html"),
            "<!doctype html><html><body><app-root></app-root></body></html>",
        )
        .unwrap();

        let out = build(&NativeBuildOptions {
            root: dir.clone(),
            entry: dir.join("main.ts"),
            out_dir: dir.join("dist"),
        })
        .expect("build ok");
        assert!(!out.written.is_empty());

        let modules_dir = dir.join("dist").join("_treaty");
        let entry_file = std::fs::read_dir(&modules_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .find(|e| e.file_name().to_string_lossy().starts_with("main-"))
            .map(|e| e.path())
            .expect("entry module emitted");
        let entry_code = std::fs::read_to_string(&entry_file).unwrap();

        // The imported child resolved into the parent's runtime dependencies via its REAL selector.
        assert!(
            entry_code.contains("dependencies: [StatCard]"),
            "imported <app-stat-card> did not resolve StatCard via the registry; got:\n{entry_code}"
        );
        // The re-introduced import was rewritten to the child's sibling dist file (not left dangling
        // at the original specifier).
        assert!(
            entry_code.contains("./stat_card-") && !entry_code.contains("'./stat-card'"),
            "child import not rewritten to its sibling dist module; got:\n{entry_code}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
