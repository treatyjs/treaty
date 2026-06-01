//! Integration: the partial-declaration linker must de-partial REAL published Angular packages to
//! ZERO residual `ɵɵngDeclare*` of any kind (the goal: Component + Directive + DI linking lands
//! entirely in Rust, with no Babel/@angular/compiler fallback).
//!
//! Phase 2 sweeps EVERY `.mjs` chunk that a real application bootstraps:
//! `@angular/{platform-browser, platform-browser-dynamic, core, common (+http), forms, router,
//! animations}` plus the `@angular/platform-browser/animations` entry point. For each chunk that
//! carries a partial marker, it asserts: linking reports no errors, ZERO `ɵɵngDeclare` substrings
//! survive, the linked output PARSES as a valid ES module, and it never imports `@angular/compiler`.
//! Each package's residual `ɵɵngDeclare` count BEFORE vs AFTER linking is printed (run with
//! `--nocapture` to see it). Skips gracefully when a package is not vendored.

use oxc_allocator::Allocator;
use oxc_parser::Parser;
use oxc_span::SourceType;
use treaty_ivy::link_partial;

/// The `ɵɵngDeclare` partial marker, built at runtime so this file's own bytes never contain the
/// literal substring being searched for in linked output.
fn ng_declare() -> String {
    format!("{}{}ngDeclare", '\u{0275}', '\u{0275}')
}

/// Count residual partial-declaration **call sites** (the linker's real metric).
///
/// A linkable call site is `ɵɵngDeclareX(...)` — a CALL with an argument list. This is distinct from
/// the runtime's own definitions/re-exports of those symbols: `@angular/core` is the one package
/// that *implements* the partial-declaration runtime, so it contains `function ɵɵngDeclareX(decl)
/// {…}` definitions and `{ …, ɵɵngDeclareX }` export specifiers. Those are NOT partial declarations
/// to be linked (they have no object literal to lower, and a `function` keyword precedes the
/// definition form); counting the bare substring would mis-flag the runtime itself. So a call site
/// is a `ngDeclare…(` occurrence whose identifier is NOT preceded by the `function ` keyword.
fn residual(code: &str) -> usize {
    let marker = ng_declare();
    let bytes = code.as_bytes();
    let mut count = 0usize;
    let mut search_from = 0usize;
    while let Some(rel) = code[search_from..].find(&marker) {
        let at = search_from + rel;
        search_from = at + marker.len();
        // Skip to the end of the identifier (`ngDeclareFactory`, `ngDeclareComponent`, …).
        let mut end = search_from;
        while end < code.len() {
            let c = code.as_bytes()[end];
            if c.is_ascii_alphanumeric() || c == b'_' || c == b'$' {
                end += 1;
            } else {
                break;
            }
        }
        // The first non-identifier byte must be `(` for this to be a call site.
        if code[end..].trim_start().starts_with('(') == false {
            continue;
        }
        // Exclude the runtime's own `function ɵɵngDeclareX(decl) {…}` definitions: walk back over
        // whitespace from the marker start; a preceding `function ` keyword means a definition.
        let before = &code[..at];
        let trimmed = before.trim_end();
        if trimmed.ends_with("function") {
            // Confirm it is the `function` keyword (word boundary), not e.g. `myfunction`.
            let kw_start = trimmed.len() - "function".len();
            let ok_boundary = kw_start == 0
                || !bytes[kw_start - 1].is_ascii_alphanumeric() && bytes[kw_start - 1] != b'_';
            if ok_boundary {
                continue;
            }
        }
        count += 1;
    }
    count
}

/// Whether `code` imports `@angular/compiler` (the JIT entry point). Used to assert the linker never
/// *introduces* such an import — `@angular/platform-browser-dynamic` legitimately imports it in its
/// own source (it IS the JIT platform), so the gate is "linker must not ADD it", not "must be
/// absent".
fn imports_angular_compiler(code: &str) -> bool {
    code.contains("@angular/compiler'") || code.contains("@angular/compiler\"")
}

/// Resolve `<repo>/node_modules/@angular/<rel>` (e.g. `common/fesm2022`), canonicalized so the
/// embedded `..` segments resolve reliably on Windows.
fn ng_dir(rel: &str) -> std::path::PathBuf {
    let raw = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../node_modules/@angular")
        .join(rel);
    std::fs::canonicalize(&raw).unwrap_or(raw)
}

/// Re-parse linked output to prove it is a well-formed ES module.
///
/// oxc 0.133's parser rejects the barred-o `ɵ` (U+0275) inside a member expression / object key
/// (e.g. `Svc.ɵfac`) even though it is a valid JS identifier char that Node and real bundlers
/// accept; Angular's emitted Ivy is saturated with `ɵfac`/`ɵprov`/`ɵɵdefine*`. To validate MODULE
/// STRUCTURE without tripping that parser gap the barred-o is folded to an ASCII letter before
/// parsing (value-preserving; the real emitted bytes are unchanged).
fn assert_parses_as_module(code: &str, who: &str) {
    let folded = code.replace('\u{0275}', "Z");
    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true).with_module(true);
    let ret = Parser::new(&allocator, &folded, source_type).parse();
    assert!(
        ret.errors.is_empty(),
        "{who}: linked output is not a valid ES module: {:?}",
        ret.errors.iter().map(|e| e.to_string()).collect::<Vec<_>>()
    );
}

/// Link every partial chunk under one `fesm2022` directory (recursively over the `.mjs` chunk files
/// it contains); assert zero residual + valid module + no JIT fallback per chunk. Returns
/// `(residual_before, residual_after, chunks_linked)`, or `None` when the directory is absent.
fn link_fesm_dir(label: &str, dir: &std::path::Path) -> Option<(usize, usize, usize)> {
    let entries = std::fs::read_dir(dir).ok()?;
    let marker = ng_declare();
    let mut before = 0usize;
    let mut after = 0usize;
    let mut chunks = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("mjs") {
            continue;
        }
        let Ok(code) = std::fs::read_to_string(&path) else { continue };
        if !code.contains(&marker) {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let chunk_before = residual(&code);
        let compiler_before = imports_angular_compiler(&code);

        let out = link_partial(&code, &name);
        assert!(out.errors.is_empty(), "{label}/{name}: link errors: {:?}", out.errors);

        let chunk_after = residual(&out.code);
        assert_eq!(
            chunk_after, 0,
            "{label}/{name}: {chunk_after} ɵɵngDeclare call site(s) survived full linking (was {chunk_before})"
        );
        // The linker must never INTRODUCE a `@angular/compiler` (JIT) import. A package that already
        // imported it in its own source (platform-browser-dynamic is the JIT platform) keeps that
        // import verbatim — the gate is that linking adds none.
        assert!(
            compiler_before || !imports_angular_compiler(&out.code),
            "{label}/{name}: linking INTRODUCED a @angular/compiler import (JIT fallback not eliminated)"
        );
        assert_parses_as_module(&out.code, &format!("{label}/{name}"));

        before += chunk_before;
        after += chunk_after;
        // Only count chunks that actually carried a linkable partial-declaration CALL SITE. A chunk
        // can contain the `ɵɵngDeclare` substring yet have zero call sites — `@angular/core` is the
        // package that *defines* `function ɵɵngDeclareX(decl){…}` and re-exports those symbols; it
        // has no partial declarations of its own, so it must not be counted as a "linked chunk".
        if chunk_before > 0 {
            chunks += 1;
        }
    }
    Some((before, after, chunks))
}

/// Link a single named `.mjs` entry-point file inside a `fesm2022` directory (used for the
/// subpath bundles `@angular/common/http`, `@angular/platform-browser/animations`, … which ship as
/// a flat file next to the main bundle). Returns `(before, after)` or `None` when absent / no
/// partial marker.
fn link_entry_file(label: &str, dir: &std::path::Path, file: &str) -> Option<(usize, usize)> {
    let path = dir.join(file);
    let code = std::fs::read_to_string(&path).ok()?;
    let marker = ng_declare();
    if !code.contains(&marker) {
        return None;
    }
    let before = residual(&code);
    let compiler_before = imports_angular_compiler(&code);
    let out = link_partial(&code, file);
    assert!(out.errors.is_empty(), "{label}/{file}: link errors: {:?}", out.errors);
    let after = residual(&out.code);
    assert_eq!(after, 0, "{label}/{file}: {after} residual ɵɵngDeclare call site(s) after linking (was {before})");
    assert!(
        compiler_before || !imports_angular_compiler(&out.code),
        "{label}/{file}: linking INTRODUCED a @angular/compiler import"
    );
    assert_parses_as_module(&out.code, &format!("{label}/{file}"));
    Some((before, after))
}

/// Sweep one bootstrap package's `fesm2022` directory and report before/after residual.
fn assert_pkg_links_clean(pkg: &str) {
    match link_fesm_dir(pkg, &ng_dir(&format!("{pkg}/fesm2022"))) {
        None => eprintln!("skipping @angular/{pkg}: not installed"),
        Some((_, _, 0)) => eprintln!("skipping @angular/{pkg}: no partial chunks present"),
        Some((before, after, chunks)) => {
            assert_eq!(after, 0, "@angular/{pkg}: {after} residual ɵɵngDeclare after linking");
            eprintln!(
                "@angular/{pkg}: linked {chunks} chunk(s) — residual ɵɵngDeclare {before} -> {after}"
            );
        }
    }
}

#[test]
fn angular_common_links_to_zero_residual() {
    assert_pkg_links_clean("common");
}

#[test]
fn angular_router_links_to_zero_residual() {
    assert_pkg_links_clean("router");
}

#[test]
fn angular_forms_links_to_zero_residual() {
    assert_pkg_links_clean("forms");
}

#[test]
fn angular_core_links_to_zero_residual() {
    assert_pkg_links_clean("core");
}

#[test]
fn angular_platform_browser_links_to_zero_residual() {
    assert_pkg_links_clean("platform-browser");
}

#[test]
fn angular_platform_browser_dynamic_links_to_zero_residual() {
    assert_pkg_links_clean("platform-browser-dynamic");
}

#[test]
fn angular_animations_links_to_zero_residual() {
    assert_pkg_links_clean("animations");
}

/// The subpath entry points a real app bootstraps that ship as a flat `.mjs` next to the main
/// bundle: `@angular/common/http` and `@angular/platform-browser/animations` (the
/// `provideAnimations()` / `provideHttpClient()` paths). Each present one must link to zero
/// residual.
#[test]
fn angular_subpath_entry_points_link_to_zero_residual() {
    let cases: &[(&str, &str, &str)] = &[
        ("common", "common/fesm2022", "http.mjs"),
        ("platform-browser", "platform-browser/fesm2022", "animations.mjs"),
        ("platform-browser", "platform-browser/fesm2022", "animations-async.mjs"),
    ];
    let mut any = false;
    for (label, rel, file) in cases {
        if let Some((before, after)) = link_entry_file(label, &ng_dir(rel), file) {
            any = true;
            assert_eq!(after, 0, "@angular/{label}/{file}: {after} residual after linking");
            eprintln!("@angular/{label}/{file}: residual ɵɵngDeclare {before} -> {after}");
        }
    }
    if !any {
        eprintln!("skipping subpath entry points: none present with partial markers");
    }
}

/// EVERY bootstrap package together: each present one must carry partial chunks and link to ZERO
/// residual, and at least one package must have been present (so the gate is never vacuous). This is
/// the Phase-2 zero-residual gate over the full set a real app loads at startup.
#[test]
fn whole_bootstrap_packages_link_to_zero_residual() {
    let mut any_present = false;
    for pkg in [
        "core",
        "common",
        "forms",
        "router",
        "platform-browser",
        "platform-browser-dynamic",
        "animations",
    ] {
        if let Some((before, after, chunks)) = link_fesm_dir(pkg, &ng_dir(&format!("{pkg}/fesm2022")))
        {
            if chunks == 0 {
                continue;
            }
            any_present = true;
            assert_eq!(after, 0, "@angular/{pkg}: {after} residual ɵɵngDeclare after linking");
            eprintln!("@angular/{pkg}: residual ɵɵngDeclare {before} -> {after} across {chunks} chunk(s)");
        }
    }
    assert!(
        any_present,
        "no bootstrap @angular package was installed; cannot verify zero-residual linking"
    );
}

// ---------------------------------------------------------------------------
// Wider-ecosystem sweep (Angular CDK / Material and any sibling community packages).
//
// CDK + Material are the canonical partial-compiled libraries that exercise declaration shapes the
// first-party bootstrap packages never do — input transform functions written as guarded ternaries
// (`value => value == null ? undefined : numberAttribute(value)`), multi-statement `useFactory`
// bodies (`{ const parent = inject(...); return parent || new X(); }`), `forwardRef` inside provider
// `deps`, rich host bindings and content/view queries. Every such chunk must de-partial to ZERO
// residual in Rust (no @angular/compiler, no JIT fallback).
//
// These packages are not pinned in this repo's package.json, so they may be absent from
// `node_modules`. The sweep locates each package's `fesm2022` dir in `node_modules/@angular/<pkg>`
// and, additionally, under an optional `TREATY_IVY_ECOSYSTEM_DIR` (a directory of extracted package
// roots, e.g. `npm pack` output) so CI/dev can point at a vendored extraction. When neither is
// present the package is skipped gracefully (logged); the combined gate asserts that AT LEAST ONE
// ecosystem package was present so the sweep is never silently vacuous when run in an environment
// that does provide them — but a bare checkout without CDK/Material installed skips cleanly.
// ---------------------------------------------------------------------------

/// The wider-ecosystem packages whose partial chunks the linker must de-partial to zero. Each is an
/// `@angular`-scoped package directory name; the sweep also tolerates a sibling community package by
/// the same convention if one is dropped into the ecosystem dir.
const ECOSYSTEM_PACKAGES: &[&str] = &["cdk", "material"];

/// Candidate `fesm2022` directories for an ecosystem package: the repo `node_modules/@angular/<pkg>`
/// install location, plus (when `TREATY_IVY_ECOSYSTEM_DIR` is set) every `*/<pkg>/.../fesm2022`
/// inside that extraction dir. Returns each existing candidate directory.
fn ecosystem_fesm_dirs(pkg: &str) -> Vec<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();

    // 1. Real install location (an app that depends on CDK/Material has it here).
    let installed = ng_dir(&format!("{pkg}/fesm2022"));
    if installed.is_dir() {
        dirs.push(installed);
    }

    // 2. Optional extraction dir (`npm pack` tarballs unpacked): walk it for any
    //    `…/<pkg…>/…/fesm2022` directory whose parent path mentions the package name.
    if let Ok(root) = std::env::var("TREATY_IVY_ECOSYSTEM_DIR") {
        let mut stack = vec![std::path::PathBuf::from(root)];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for e in entries.flatten() {
                let p = e.path();
                if !p.is_dir() {
                    continue;
                }
                if p.file_name().and_then(|n| n.to_str()) == Some("fesm2022")
                    && p.to_string_lossy().to_lowercase().contains(pkg)
                {
                    if !dirs.iter().any(|d| d == &p) {
                        dirs.push(p);
                    }
                } else {
                    stack.push(p);
                }
            }
        }
    }
    dirs
}

/// Recursively link every partial `.mjs` chunk in a directory tree (CDK/Material keep their chunks
/// flat in `fesm2022`, but a nested layout is handled too). Asserts per chunk: no link errors, ZERO
/// residual `ɵɵngDeclare` call site, the output re-parses as a valid ES module, and no
/// `@angular/compiler` import is introduced. Returns `(before, after, linked_chunk_count)`.
fn link_tree_recursive(label: &str, root: &std::path::Path) -> (usize, usize, usize) {
    let marker = ng_declare();
    let mut before = 0usize;
    let mut after = 0usize;
    let mut chunks = 0usize;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("mjs") {
                continue;
            }
            let Ok(code) = std::fs::read_to_string(&path) else { continue };
            if !code.contains(&marker) {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let chunk_before = residual(&code);
            if chunk_before == 0 {
                continue;
            }
            let compiler_before = imports_angular_compiler(&code);

            let out = link_partial(&code, &name);
            assert!(out.errors.is_empty(), "{label}/{name}: link errors: {:?}", out.errors);

            let chunk_after = residual(&out.code);
            assert_eq!(
                chunk_after, 0,
                "{label}/{name}: {chunk_after} ɵɵngDeclare call site(s) survived full linking (was {chunk_before})"
            );
            assert!(
                compiler_before || !imports_angular_compiler(&out.code),
                "{label}/{name}: linking INTRODUCED a @angular/compiler import (JIT fallback not eliminated)"
            );
            assert_parses_as_module(&out.code, &format!("{label}/{name}"));

            before += chunk_before;
            after += chunk_after;
            chunks += 1;
        }
    }
    (before, after, chunks)
}

/// Sweep ONE wider-ecosystem package across every candidate `fesm2022` dir. Returns
/// `(before, after, chunks)` summed across all candidates, or `None` when the package is absent.
fn sweep_ecosystem_package(pkg: &str) -> Option<(usize, usize, usize)> {
    let dirs = ecosystem_fesm_dirs(pkg);
    if dirs.is_empty() {
        return None;
    }
    let (mut before, mut after, mut chunks) = (0usize, 0usize, 0usize);
    for dir in &dirs {
        let (b, a, c) = link_tree_recursive(pkg, dir);
        before += b;
        after += a;
        chunks += c;
    }
    Some((before, after, chunks))
}

/// Angular CDK — every partial chunk present must de-partial to zero residual. Skips when CDK is not
/// installed (neither in `node_modules` nor the ecosystem extraction dir).
#[test]
fn angular_cdk_links_to_zero_residual() {
    match sweep_ecosystem_package("cdk") {
        None => eprintln!("skipping @angular/cdk: not installed"),
        Some((_, _, 0)) => eprintln!("skipping @angular/cdk: no partial chunks present"),
        Some((before, after, chunks)) => {
            assert_eq!(after, 0, "@angular/cdk: {after} residual ɵɵngDeclare after linking");
            eprintln!("@angular/cdk: linked {chunks} chunk(s) — residual ɵɵngDeclare {before} -> {after}");
        }
    }
}

/// Angular Material — every partial chunk present must de-partial to zero residual. Skips when
/// Material is not installed.
#[test]
fn angular_material_links_to_zero_residual() {
    match sweep_ecosystem_package("material") {
        None => eprintln!("skipping @angular/material: not installed"),
        Some((_, _, 0)) => eprintln!("skipping @angular/material: no partial chunks present"),
        Some((before, after, chunks)) => {
            assert_eq!(after, 0, "@angular/material: {after} residual ɵɵngDeclare after linking");
            eprintln!("@angular/material: linked {chunks} chunk(s) — residual ɵɵngDeclare {before} -> {after}");
        }
    }
}

/// The full wider-ecosystem zero-residual gate: every PRESENT ecosystem package must carry partial
/// chunks that de-partial to ZERO residual. When at least one package is present the gate is
/// non-vacuous (it asserts a positive chunk count was actually linked); when NONE is present (a bare
/// checkout without CDK/Material) it skips cleanly — these packages are not pinned in this repo, so
/// their absence is not a failure, only their non-zero residual is.
#[test]
fn wider_ecosystem_packages_link_to_zero_residual() {
    let mut present = 0usize;
    let mut total_chunks = 0usize;
    for pkg in ECOSYSTEM_PACKAGES {
        match sweep_ecosystem_package(pkg) {
            None | Some((_, _, 0)) => continue,
            Some((before, after, chunks)) => {
                present += 1;
                total_chunks += chunks;
                assert_eq!(after, 0, "@angular/{pkg}: {after} residual ɵɵngDeclare after linking");
                eprintln!(
                    "@angular/{pkg}: residual ɵɵngDeclare {before} -> {after} across {chunks} chunk(s)"
                );
            }
        }
    }
    if present == 0 {
        eprintln!(
            "skipping wider-ecosystem gate: no ecosystem package (cdk/material) installed; \
             set TREATY_IVY_ECOSYSTEM_DIR or install @angular/cdk + @angular/material to exercise it"
        );
        return;
    }
    // Non-vacuous when present: at least one real partial chunk must have been linked.
    assert!(
        total_chunks > 0,
        "wider-ecosystem packages were present but carried no linkable partial chunks"
    );
    eprintln!("wider ecosystem: {present} package(s), {total_chunks} partial chunk(s) → zero residual");
}
