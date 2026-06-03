//! End-to-end guard for the two ng-packagr parity fixes:
//!
//!   1. **`.d.ts` re-export barrel flattening** — a `public-api.ts` that only
//!      re-exports its internal modules (`export * from './lib/x.component'`) must
//!      emit a SELF-CONTAINED `index.d.ts` whose declarations are inlined (the
//!      `ɵfac`/`ɵcmp` + class), with NO dangling `export * from './lib/...'` to a
//!      file that packagr never writes to dist. A consumer importing the package
//!      must get resolvable types.
//!
//!   2. **esbuild-equivalent CSS value minification** — the emitted `styles: [...]`
//!      values must match ng-packagr's esbuild `minify: true` pass (`color: blue` →
//!      `#00f`, `font-weight: bold` → `700`, `#ffffff` → `#fff`, `0px` → `0`,
//!      `0.500em` → `.5em`, whitespace collapse) while preserving the
//!      `_ngcontent-%COMP%` scoping placeholder.
//!
//! Both are asserted by PARSING the emitted artifacts (oxc), never by regex.

use std::path::{Path, PathBuf};

use oxc_allocator::Allocator;
use oxc_ast::ast::{Declaration, Statement};
use oxc_parser::Parser;
use oxc_span::SourceType;
use treaty_packagr::{build_to_disk, PackageConfig};

fn scratch(tag: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    dir.push(format!("treaty_packagr_barrel_{tag}_{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(root: &Path, rel: &str, contents: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// Parse a `.d.ts` and return whether it (a) declares `symbol` as a class and
/// (b) re-exports it, while (c) carrying NO relative re-export. Parsing proves the
/// declaration is self-contained and resolvable.
struct DtsFacts {
    declares_symbol: bool,
    exports_symbol: bool,
    has_relative_reexport: bool,
    has_fac: bool,
    has_cmp: bool,
}

fn analyze_dts(dts: &str, symbol: &str) -> DtsFacts {
    let allocator = Allocator::default();
    let st = SourceType::default()
        .with_typescript(true)
        .with_typescript_definition(true)
        .with_module(true);
    let parsed = Parser::new(&allocator, dts, st).parse();
    assert!(
        parsed.errors.is_empty(),
        "flattened .d.ts did not parse cleanly: {:?}\n--- dts ---\n{dts}",
        parsed.errors
    );

    let mut declares_symbol = false;
    let mut exports_symbol = false;
    let mut has_relative_reexport = false;

    for stmt in &parsed.program.body {
        match stmt {
            Statement::ExportAllDeclaration(e) => {
                if e.source.value.starts_with("./") || e.source.value.starts_with("../") {
                    has_relative_reexport = true;
                }
            }
            Statement::ExportNamedDeclaration(e) => {
                if let Some(src) = &e.source {
                    if src.value.starts_with("./") || src.value.starts_with("../") {
                        has_relative_reexport = true;
                    }
                }
                if let Some(Declaration::ClassDeclaration(c)) = &e.declaration {
                    if c.id.as_ref().is_some_and(|i| i.name == symbol) {
                        declares_symbol = true;
                    }
                }
                for spec in &e.specifiers {
                    if spec.exported.name() == symbol {
                        exports_symbol = true;
                    }
                }
            }
            Statement::ClassDeclaration(c) => {
                if c.id.as_ref().is_some_and(|i| i.name == symbol) {
                    declares_symbol = true;
                }
            }
            _ => {}
        }
    }

    DtsFacts {
        declares_symbol,
        exports_symbol,
        has_relative_reexport,
        has_fac: dts.contains('\u{0275}'), // ɵ present at all
        has_cmp: dts.contains("\u{0275}\u{0275}ComponentDeclaration"),
    }
}

/// Extract the first `styles: ["…"]` string literal from a compiled module by
/// parsing it (no regex), returning the literal's cooked value.
fn first_styles_value(mjs: &str) -> Option<String> {
    use oxc_ast::ast::{Argument, Expression, ObjectPropertyKind, PropertyKey};

    fn key_is_styles(key: &PropertyKey) -> bool {
        matches!(key, PropertyKey::StaticIdentifier(id) if id.name == "styles")
            || matches!(key, PropertyKey::StringLiteral(s) if s.value == "styles")
    }

    fn from_expr(expr: &Expression) -> Option<String> {
        match expr {
            Expression::AssignmentExpression(a) => from_expr(&a.right),
            Expression::CallExpression(call) => {
                for arg in &call.arguments {
                    if let Argument::ObjectExpression(obj) = arg {
                        for prop in &obj.properties {
                            if let ObjectPropertyKind::ObjectProperty(op) = prop
                                && key_is_styles(&op.key)
                                && let Expression::ArrayExpression(arr) = &op.value
                            {
                                for el in &arr.elements {
                                    if let Some(Expression::StringLiteral(s)) = el.as_expression() {
                                        return Some(s.value.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
                None
            }
            _ => None,
        }
    }

    let allocator = Allocator::default();
    let st = SourceType::default().with_typescript(true).with_module(true);
    let parsed = Parser::new(&allocator, mjs, st).parse();
    if !parsed.errors.is_empty() {
        return None;
    }
    for stmt in &parsed.program.body {
        let expr = match stmt {
            Statement::ExpressionStatement(e) => Some(&e.expression),
            _ => None,
        };
        if let Some(e) = expr
            && let Some(v) = from_expr(e)
        {
            return Some(v);
        }
    }
    None
}

#[test]
fn barrel_lib_emits_self_contained_flattened_dts() {
    let root = scratch("dts");
    write(
        &root,
        "treaty-package.json",
        r#"{ "name": "@acme/box", "version": "1.0.0", "dest": "dist",
             "lib": { "entryFile": "src/public-api.ts" } }"#,
    );
    // A pure re-export BARREL — the exact shape that produced the dangling
    // `export * from './lib/box.component'` bug.
    write(&root, "src/public-api.ts", "export * from './lib/box.component';\n");
    write(
        &root,
        "src/lib/box.component.ts",
        "import { Component } from '@angular/core';\n\
         @Component({ selector: 'acme-box', standalone: true, template: '<div></div>' })\n\
         export class BoxComponent {}\n",
    );

    let cfg = PackageConfig::from_json(
        &std::fs::read_to_string(root.join("treaty-package.json")).unwrap(),
    )
    .unwrap();
    let dist = build_to_disk(&root, &cfg).expect("packaging should succeed");
    assert_eq!(dist.entries.len(), 1);

    let dts_path = root.join("dist/index.d.ts");
    let dts = std::fs::read_to_string(&dts_path).unwrap();

    let facts = analyze_dts(&dts, "BoxComponent");
    // The barrel is FLATTENED: the class is inlined, re-exported, and NO dangling
    // relative re-export survives (the real bug).
    assert!(
        !facts.has_relative_reexport,
        "index.d.ts still carries a dangling relative re-export (the bug):\n{dts}"
    );
    assert!(facts.declares_symbol, "BoxComponent class not inlined:\n{dts}");
    assert!(facts.exports_symbol, "BoxComponent not re-exported:\n{dts}");
    assert!(facts.has_fac && facts.has_cmp, "Ivy ɵfac/ɵcmp declarations missing:\n{dts}");

    // The dangling target file is NOT written to dist (proving the re-export would
    // have been unresolvable had it survived).
    assert!(
        !root.join("dist/lib/box.component.d.ts").exists(),
        "the re-export target was written to dist; the barrel must instead be flattened"
    );

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn styled_component_styles_match_esbuild_minification() {
    let root = scratch("css");
    write(
        &root,
        "treaty-package.json",
        r#"{ "name": "@acme/styled", "version": "1.0.0", "dest": "dist",
             "lib": { "entryFile": "src/public-api.ts" } }"#,
    );
    write(&root, "src/public-api.ts", "export * from './lib/box.component';\n");
    write(
        &root,
        "src/lib/box.component.ts",
        "import { Component } from '@angular/core';\n\
         @Component({\n\
           selector: 'acme-box', standalone: true, template: '<div class=\"box\"></div>',\n\
           styles: [`\n\
             .box {\n\
               color: blue;\n\
               font-weight: bold;\n\
               background: #ffffff;\n\
               margin: 0px;\n\
               padding: 0.500em;\n\
               border: 1px solid rgb(170, 187, 204);\n\
             }\n\
           `]\n\
         })\n\
         export class BoxComponent {}\n",
    );

    let cfg = PackageConfig::from_json(
        &std::fs::read_to_string(root.join("treaty-package.json")).unwrap(),
    )
    .unwrap();
    build_to_disk(&root, &cfg).expect("packaging should succeed");

    let mjs = std::fs::read_to_string(root.join("dist/index.mjs")).unwrap();
    let styles = first_styles_value(&mjs).expect("a styles literal must be present");

    // The scoped, esbuild-minified value byte-for-byte (full/AOT mode), which is
    // exactly what a real ng-packagr@21 `compilationMode: full` build emits.
    assert_eq!(
        styles,
        ".box[_ngcontent-%COMP%]{color:#00f;font-weight:700;background:#fff;margin:0;padding:.5em;border:1px solid rgb(170,187,204)}",
        "styles do not match ng-packagr's esbuild-minified value"
    );

    std::fs::remove_dir_all(&root).ok();
}
