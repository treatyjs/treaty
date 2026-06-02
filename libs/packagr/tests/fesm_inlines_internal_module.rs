//! FESM flattening acceptance — an entry that imports a PRIVATE internal helper
//! module is flattened into a single ES module: the helper is inlined and its
//! relative `import` disappears, while bare specifiers (`@angular/core`) stay
//! external. Proven by PARSING the flattened output (never a regex):
//!
//!   * the flattened entry re-parses as a valid ES module;
//!   * it carries NO surviving relative import (the helper is inlined);
//!   * it still carries the entry's `ɵɵdefineComponent` (Ivy survives intact);
//!   * the inlined helper's binding is present in the flat module;
//!   * the external `@angular/core` import is preserved;
//!   * a *sibling published entry* import is NOT inlined (left as a cross-entry
//!     reference), so secondary entries stay separate FESM files per APF.

use std::path::{Path, PathBuf};

use oxc_allocator::Allocator;
use oxc_ast::ast::Statement;
use oxc_parser::Parser;
use oxc_span::SourceType;
use treaty_packagr::{build_to_disk, PackageConfig};

const DEFINE_COMPONENT: &str = "\u{0275}\u{0275}defineComponent";

fn scratch(tag: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    dir.push(format!("treaty_packagr_fesm_{tag}_{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(root: &Path, rel: &str, contents: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// The relative import specifiers surviving in `code` — proven by walking the
/// AST, not a substring scan. The `ɵ` (U+0275) in lowered Ivy trips oxc's
/// identifier scanner in some positions, so fold it to an ASCII letter purely for
/// this structural walk (the emitted bytes are unchanged).
fn relative_imports(code: &str) -> Vec<String> {
    let folded = code.replace('\u{0275}', "Z");
    let alloc = Allocator::default();
    let st = SourceType::default().with_typescript(true).with_module(true);
    let parsed = Parser::new(&alloc, &folded, st).parse();
    assert!(
        parsed.errors.is_empty(),
        "flattened module did not parse: {:?}\n--- code ---\n{code}",
        parsed.errors
    );
    let mut specs = Vec::new();
    for stmt in &parsed.program.body {
        let spec = match stmt {
            Statement::ImportDeclaration(i) => Some(i.source.value.to_string()),
            Statement::ExportNamedDeclaration(e) => e.source.as_ref().map(|s| s.value.to_string()),
            Statement::ExportAllDeclaration(e) => Some(e.source.value.to_string()),
            _ => None,
        };
        if let Some(s) = spec
            && (s.starts_with("./") || s.starts_with("../"))
        {
            specs.push(s);
        }
    }
    specs
}

/// Assert `code` is a syntactically valid ES module (same `ɵ`-fold parse check).
fn assert_parses(code: &str, what: &str) {
    let folded = code.replace('\u{0275}', "Z");
    let alloc = Allocator::default();
    let st = SourceType::default().with_typescript(true).with_module(true);
    let parsed = Parser::new(&alloc, &folded, st).parse();
    assert!(
        parsed.errors.is_empty(),
        "{what} did not parse: {:?}\n--- code ---\n{code}",
        parsed.errors
    );
}

#[test]
fn fesm_inlines_a_private_internal_helper_module() {
    let root = scratch("inline");

    // A real library whose PRIMARY entry is a bare `@Component` class that imports
    // a PRIVATE helper module (`./internal/classes`) and a bare npm dep
    // (`@angular/core`). The helper is NOT a published entry point.
    write(
        &root,
        "treaty-package.json",
        r#"{
            "name": "@acme/flat",
            "version": "1.0.0",
            "dest": "dist",
            "lib": { "entryFile": "src/public-api.ts" },
            "secondaryEntryPoints": [
                { "path": "card", "entryFile": "src/card/public-api.ts" }
            ]
        }"#,
    );

    // The PRIVATE helper: a plain `.ts` module exporting a couple of helpers. It
    // lives OUTSIDE any entry's published surface — purely internal.
    write(
        &root,
        "src/internal/classes.ts",
        "export const BASE_CLASS = 'acme-base';\n\
         export function joinClasses(a: string, b: string): string { return a + ' ' + b; }\n",
    );

    // PRIMARY entry: a `@Component` that imports the private helper AND a sibling
    // published entry (`./card`). After flattening, the helper must be inlined
    // (no `./internal/classes` import left) while the sibling-entry import to
    // `../card/public-api` stays a cross-entry reference.
    write(
        &root,
        "src/public-api.ts",
        "import { Component } from '@angular/core';\n\
         import { BASE_CLASS, joinClasses } from './internal/classes';\n\
         export { default as Card } from '../card/public-api';\n\
         @Component({ selector: 'acme-panel', template: '<div [class]=\"cls\"><ng-content></ng-content></div>' })\n\
         export class PanelComponent {\n\
           readonly cls = joinClasses(BASE_CLASS, 'acme-panel');\n\
         }\n",
    );

    // The sibling published entry `./card`: a self-contained single-file
    // component (flatten is an identity for it).
    write(
        &root,
        "src/card/public-api.ts",
        "import { Component } from '@angular/core';\n\
         @Component({ selector: 'acme-card', template: '<section><ng-content></ng-content></section>' })\n\
         export default class CardComponent {}\n",
    );

    let cfg = PackageConfig::from_json(
        &std::fs::read_to_string(root.join("treaty-package.json")).unwrap(),
    )
    .expect("descriptor parses");

    let dist = build_to_disk(&root, &cfg).expect("packaging should succeed");

    // ---- the PRIMARY entry: helper inlined, Ivy intact, externals preserved ----
    let primary = dist
        .entries
        .iter()
        .find(|e| e.is_primary())
        .expect("primary entry present");
    let esm = &primary.esm;

    // (1) The flattened module re-parses as a valid ES module.
    assert_parses(esm, "primary flattened ESM");

    // (2) NO private relative import survives — the helper was INLINED. The only
    //     relative specifier allowed to remain is the SIBLING-ENTRY re-export to
    //     `../card/public-api` (a separate FESM file in APF, never inlined).
    let rels = relative_imports(esm);
    assert!(
        !rels.iter().any(|r| r.contains("internal/classes")),
        "private helper `./internal/classes` was NOT inlined; survives in:\n{esm}\nrelatives: {rels:?}"
    );
    for r in &rels {
        assert!(
            r.contains("card"),
            "unexpected surviving relative import `{r}` (only the sibling-entry ref may remain):\n{esm}"
        );
    }

    // (3) The inlined helper's bindings are present in the flat module body.
    assert!(
        esm.contains("BASE_CLASS") && esm.contains("joinClasses"),
        "inlined helper bindings missing from flattened module:\n{esm}"
    );

    // (4) The entry's Ivy definition survived the flatten unchanged.
    assert!(
        esm.contains(DEFINE_COMPONENT),
        "flattened primary lost its ɵɵdefineComponent:\n{esm}"
    );

    // (5) The external `@angular/core` import is preserved (bare specifier stays
    //     external) and the sibling-entry re-export remains.
    assert!(
        esm.contains("@angular/core"),
        "flattened primary dropped its external @angular/core import:\n{esm}"
    );
    assert!(
        esm.contains("Card"),
        "flattened primary dropped the sibling-entry `Card` re-export:\n{esm}"
    );

    // ---- the SIBLING entry stays a self-contained single-file component ----
    let card = dist
        .entries
        .iter()
        .find(|e| e.sub_path == "card")
        .expect("card entry present");
    assert_parses(&card.esm, "card flattened ESM");
    assert!(
        relative_imports(&card.esm).is_empty(),
        "sibling single-file entry should carry no relative imports:\n{}",
        card.esm
    );
    assert!(
        card.esm.contains(DEFINE_COMPONENT),
        "card entry lost its ɵɵdefineComponent:\n{}",
        card.esm
    );

    std::fs::remove_dir_all(&root).ok();
}
