//! Showcase integration: package the real `examples/treaty-shadcn` library — six components authored
//! across ALL THREE Treaty surfaces (`.treaty` SFC, Treaty `.tsx`, and PLAIN REACT `.tsx`) — to a
//! publishable Angular-package `dist/`, proving the packagr de-sugars every surface to Ivy.
//!
//! This is the end-to-end proof of the "welcome-to-the-world" library: each component's emitted ESM
//! must be a valid module carrying an `ɵɵdefineComponent` (AOT, no surviving framework decorator and
//! no `react` import), and the published `package.json` exports map must name every entry.

use std::path::Path;

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Declaration, ExportDefaultDeclarationKind, Expression, Statement,
};
use oxc_parser::Parser;
use oxc_span::SourceType;

fn parses_as_module(code: &str) -> bool {
    // The barred-o `ɵ` (U+0275) trips oxc 0.133's member-expression parser even though Node accepts
    // it; fold it to an ASCII letter so we validate MODULE STRUCTURE (the real emitted bytes are
    // unchanged) — the same value-preserving fold the linker's parse-check uses.
    let folded = code.replace('\u{0275}', "Z");
    let alloc = Allocator::default();
    let st = SourceType::default().with_typescript(true).with_module(true);
    Parser::new(&alloc, &folded, st).parse().errors.is_empty()
}

/// Class- and member-level Angular decorators surviving in a compiled module.
#[derive(Default, Debug)]
struct SurvivingDecorators {
    class_level: Vec<String>,
    member_level: Vec<String>,
}

/// PARSE `code` (never a substring scan — `@Component` appears verbatim in the
/// authoring-source COMMENTS that survive into the lowered fn) and collect every
/// surviving Angular decorator AST node, classified as class- or member-level.
/// Mirrors the proven walk in `package_real_library.rs`.
fn collect_surviving_decorators(code: &str, what: &str) -> SurvivingDecorators {
    let allocator = Allocator::default();
    let parsed =
        Parser::new(&allocator, code, SourceType::default().with_typescript(true).with_module(true))
            .parse();
    assert!(parsed.errors.is_empty(), "{what} ivy did not parse: {:?}", parsed.errors);

    let mut found = SurvivingDecorators::default();
    for stmt in &parsed.program.body {
        if let Some(class) = class_of_statement(stmt) {
            for dec in &class.decorators {
                if let Some(name) = decorator_callee_name(&dec.expression) {
                    found.class_level.push(name);
                }
            }
            for member in &class.body.body {
                let decorators = match member {
                    oxc_ast::ast::ClassElement::MethodDefinition(m) => &m.decorators,
                    oxc_ast::ast::ClassElement::PropertyDefinition(p) => &p.decorators,
                    oxc_ast::ast::ClassElement::AccessorProperty(a) => &a.decorators,
                    _ => continue,
                };
                for dec in decorators {
                    if let Some(name) = decorator_callee_name(&dec.expression) {
                        found.member_level.push(name);
                    }
                }
            }
        }
    }
    found
}

fn class_of_statement<'a>(stmt: &'a Statement<'a>) -> Option<&'a oxc_ast::ast::Class<'a>> {
    match stmt {
        Statement::ClassDeclaration(class) => Some(class),
        Statement::ExportNamedDeclaration(export) => match &export.declaration {
            Some(Declaration::ClassDeclaration(class)) => Some(class),
            _ => None,
        },
        Statement::ExportDefaultDeclaration(def) => match &def.declaration {
            ExportDefaultDeclarationKind::ClassDeclaration(class) => Some(class),
            _ => None,
        },
        _ => None,
    }
}

/// `@Foo` → `Foo`, `@Foo(...)` → `Foo`, `@ns.Foo(...)` → `Foo`.
fn decorator_callee_name(expr: &Expression) -> Option<String> {
    match expr {
        Expression::Identifier(id) => Some(id.name.to_string()),
        Expression::CallExpression(call) => decorator_callee_name(&call.callee),
        Expression::StaticMemberExpression(m) => Some(m.property.name.to_string()),
        _ => None,
    }
}

#[test]
fn package_treaty_shadcn_showcase_to_dist() {
    let lib = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/treaty-shadcn");
    if !lib.join("treaty-package.json").is_file() {
        eprintln!("skipping: examples/treaty-shadcn not present at {}", lib.display());
        return;
    }

    let manifest = treaty_packagr::package_library_at(&lib)
        .unwrap_or_else(|e| panic!("packaging treaty-shadcn failed: {e}"));

    // Each of the SIX COMPONENT entries (the secondaries) must lower to an Ivy component (define
    // block), re-parse as a valid ES module, carry no `react` import / no surviving Angular
    // decorator, AND carry a re-parseable component-class `.d.ts` (typed signal inputs + ɵfac/ɵcmp)
    // — NOT isolated-declarations over the lowered fn (which would TS9007). The primary entry is
    // the `public-api` barrel: a re-export module, validated separately below.
    let define = format!("{}{}defineComponent", '\u{0275}', '\u{0275}');
    let define_decl = format!("{}{}ComponentDeclaration", '\u{0275}', '\u{0275}');
    let mut entry_dirs: Vec<String> = Vec::new();
    let mut component_count = 0usize;
    for entry in &manifest.entries {
        let dir = entry.dir.clone();
        entry_dirs.push(dir.clone());

        if entry.is_primary() {
            // The public-api barrel: a valid module that re-exports every component by name,
            // with a re-parseable `.d.ts`. It carries no `ɵɵdefineComponent` (it defines nothing).
            assert!(parses_as_module(&entry.esm), "primary `{dir}` ESM did not parse as a module");
            for want in ["Button", "Badge", "Card", "Alert", "Input", "Switch"] {
                assert!(
                    entry.esm.contains(want),
                    "public-api barrel does not re-export `{want}`:\n{}",
                    entry.esm
                );
            }
            assert!(!entry.declarations.trim().is_empty(), "primary `{dir}` produced no .d.ts");
            assert!(
                parses_as_module(&entry.declarations),
                "primary `{dir}` .d.ts did not re-parse as TypeScript:\n{}",
                entry.declarations
            );
            continue;
        }

        component_count += 1;
        assert!(
            entry.esm.contains(&define),
            "entry `{dir}` carries no ɵɵdefineComponent (not lowered to Ivy):\n{}",
            &entry.esm[..entry.esm.len().min(400)]
        );
        assert!(parses_as_module(&entry.esm), "entry `{dir}` ESM did not parse as a module");
        assert!(
            !entry.esm.contains("from 'react'") && !entry.esm.contains("from \"react\""),
            "entry `{dir}` leaked a `react` import (React→Angular lowering incomplete)"
        );
        // No surviving Angular decorator — verified by PARSING (the authoring sources mention
        // `@Component`/`@Directive` in COMMENTS that survive into the lowered fn, so a substring
        // scan would false-positive). Fully AOT: zero class- or member-level decorators remain.
        let survivors = collect_surviving_decorators(&entry.esm, &format!("{dir}/index.mjs"));
        assert!(
            survivors.class_level.is_empty() && survivors.member_level.is_empty(),
            "entry `{dir}` left raw Angular decorator(s) {survivors:?} (not AOT-compiled)"
        );
        // The .d.ts is a valid, re-parseable TS declaration — and for a component entry it is the
        // reconstructed Angular component class (typed signal inputs + ɵfac/ɵcmp).
        assert!(!entry.declarations.trim().is_empty(), "entry `{dir}` produced no .d.ts");
        assert!(
            parses_as_module(&entry.declarations),
            "entry `{dir}` .d.ts did not re-parse as TypeScript:\n{}",
            entry.declarations
        );
        assert!(
            entry.declarations.contains("export declare class")
                && entry.declarations.contains(&define_decl),
            "entry `{dir}` .d.ts is not a typed Angular component class:\n{}",
            entry.declarations
        );
    }
    assert_eq!(component_count, 6, "expected exactly 6 component entries: {entry_dirs:?}");

    // The 6 components are all packaged (by entry sub-path).
    for want in ["button", "badge", "card", "alert", "input", "switch"] {
        assert!(
            entry_dirs.iter().any(|d| d.contains(want)),
            "component `{want}` missing from packaged entries: {entry_dirs:?}"
        );
    }

    // The published package.json (APF) names every entry in its `exports` map.
    assert!(manifest.manifest.contains("\"exports\""), "no exports map in published package.json");
    for want in [".", "./button", "./badge", "./card", "./alert", "./input", "./switch"] {
        assert!(
            manifest.manifest.contains(&format!("\"{want}\"")),
            "exports map is missing entry `{want}`:\n{}",
            manifest.manifest
        );
    }

    // Emit to dist/ so it is a real, inspectable, publishable artifact.
    let dest = lib.join("dist");
    let written = manifest.write_to(&dest).unwrap_or_else(|e| panic!("write dist failed: {e}"));
    assert!(
        written.iter().any(|p| p.file_name().is_some_and(|n| n == "package.json")),
        "dist/package.json not written"
    );
    eprintln!(
        "treaty-shadcn packaged: {} entries → {} files in {}",
        manifest.entries.len(),
        written.len(),
        dest.display()
    );
}
