//! Integration: the partial-declaration linker must de-partial REAL published Angular packages to
//! ZERO residual `ɵɵngDeclare*` of any kind (the goal: Component + Directive linking lands entirely
//! in Rust, with no Babel/@angular/compiler fallback).
//!
//! Sweeps every `.mjs` chunk of `@angular/{common,router,forms}` fesm2022 and, for each chunk that
//! carries a partial marker, asserts: linking reports no errors, ZERO `ɵɵngDeclare` substrings
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

/// Count the `ɵɵngDeclare` partial markers in a source string (the residual metric).
fn residual(code: &str) -> usize {
    code.matches(&ng_declare()).count()
}

fn pkg_dir(pkg: &str) -> std::path::PathBuf {
    let raw = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../node_modules/@angular")
        .join(pkg)
        .join("fesm2022");
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

/// Link every partial chunk of one package; assert zero residual + valid module + no JIT fallback.
/// Returns `(residual_before, residual_after, chunks_linked)`, or `None` when the package is absent.
fn link_package(pkg: &str) -> Option<(usize, usize, usize)> {
    let dir = pkg_dir(pkg);
    let entries = std::fs::read_dir(&dir).ok()?;
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

        let out = link_partial(&code, &name);
        assert!(out.errors.is_empty(), "{pkg}/{name}: link errors: {:?}", out.errors);

        let chunk_after = residual(&out.code);
        assert_eq!(
            chunk_after, 0,
            "{pkg}/{name}: {chunk_after} ɵɵngDeclare marker(s) survived full linking (was {chunk_before})"
        );
        assert!(
            !out.code.contains("@angular/compiler"),
            "{pkg}/{name}: linked output still references @angular/compiler (JIT fallback not eliminated)"
        );
        assert_parses_as_module(&out.code, &format!("{pkg}/{name}"));

        before += chunk_before;
        after += chunk_after;
        chunks += 1;
    }
    Some((before, after, chunks))
}

/// Assert a single package links to zero residual and report its before/after count.
fn assert_pkg_links_clean(pkg: &str) {
    match link_package(pkg) {
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

/// All three packages together: each present one must carry partial chunks and link to ZERO
/// residual, and at least one package must have been present (so the gate is never vacuous).
#[test]
fn whole_angular_packages_link_to_zero_residual() {
    let mut any_present = false;
    for pkg in ["common", "forms", "router"] {
        if let Some((before, after, chunks)) = link_package(pkg) {
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
        "none of @angular/{{common,forms,router}} were installed; cannot verify zero-residual linking"
    );
}
