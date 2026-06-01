//! Integration: the partial-declaration linker must de-partial REAL published Angular packages to
//! ZERO residual `ɵɵngDeclare*` of any kind (the Phase-2 goal: no Babel/@angular/compiler fallback).
//! Sweeps `@angular/{common,router,forms}` fesm2022 chunks. Skips gracefully when not vendored.

use treaty_ivy::link_partial;

fn ng_declare() -> String {
    format!("{}{}ngDeclare", '\u{0275}', '\u{0275}')
}

fn pkg_dir(pkg: &str) -> std::path::PathBuf {
    let raw = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../node_modules/@angular")
        .join(pkg)
        .join("fesm2022");
    std::fs::canonicalize(&raw).unwrap_or(raw)
}

fn assert_pkg_links_clean(pkg: &str) {
    let dir = pkg_dir(pkg);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!("skipping {pkg}: {} absent", dir.display());
        return;
    };
    let marker = ng_declare();
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
        let out = link_partial(&code, &name);
        assert!(out.errors.is_empty(), "{pkg}/{name}: link errors: {:?}", out.errors);
        assert!(
            !out.code.contains(&marker),
            "{pkg}/{name}: a ɵɵngDeclare marker survived full linking"
        );
        assert!(
            !out.code.contains("@angular/compiler"),
            "{pkg}/{name}: linked output still references @angular/compiler"
        );
        chunks += 1;
    }
    if chunks == 0 {
        eprintln!("skipping {pkg}: no partial chunks present");
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
