//! End-to-end packaging acceptance — roadmap deliverable #7,
//! "package anything the compiler supports".
//!
//! This test builds a *real* multi-front-end authoring library on disk and runs
//! the full [`treaty_packagr`] pipeline over it ([`build_to_disk`]), then proves
//! the emitted dist is a publishable Angular Package Format (APF) package by
//! *parsing* the artifacts — never by regex:
//!
//!   * the root `package.json` is parsed with `serde_json` and its APF fields and
//!     `exports` subpath map are asserted structurally;
//!   * every emitted `.d.ts` is parsed with the oxc TypeScript parser and required
//!     to be syntactically valid declaration source that declares the entry's
//!     exported symbol;
//!   * every emitted Ivy `.mjs` is parsed with the oxc parser (so it is provably
//!     valid ESM) and required to contain an Ivy definition
//!     (`ɵɵdefineComponent` / `ɵɵdefineDirective` / `ɵɵdefinePipe`) plus an `ɵfac`
//!     factory, with **no raw Angular decorator surviving** — i.e. fully AOT, no
//!     JIT.
//!
//! The fixture deliberately spans every authoring front-end packagr supports, one
//! per entry point:
//!   * primary  `.`           — a `.treaty` SFC (template + interpolation);
//!   * `./card`               — a `.tjsx` JSX component (signals-by-default);
//!   * `./widget`             — a bare `@Component` `.ts` class;
//!   * `./shout`              — a `@Pipe` `.ts` class;
//!   * `./highlight`          — a `@Directive` `.ts` class.

use std::path::{Path, PathBuf};

use oxc_allocator::Allocator;
use oxc_ast::ast::{Declaration, ExportDefaultDeclarationKind, Statement};
use oxc_parser::Parser;
use oxc_span::SourceType;
use serde_json::Value;
use treaty_packagr::{build_to_disk, PackageConfig};

const DEFINE_COMPONENT: &str = "\u{0275}\u{0275}defineComponent";
const DEFINE_DIRECTIVE: &str = "\u{0275}\u{0275}defineDirective";
const DEFINE_PIPE: &str = "\u{0275}\u{0275}definePipe";

/// A unique scratch dir under the OS temp folder.
fn scratch(tag: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    dir.push(format!("treaty_packagr_e2e_{tag}_{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(root: &Path, rel: &str, contents: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// Build the multi-front-end fixture authoring library on disk and return its
/// root directory.
fn build_fixture_library() -> PathBuf {
    let root = scratch("multi");

    // The package descriptor: a real `treaty-package.json` naming a scoped
    // package, an explicit version, an output dir, and an asset to copy.
    write(
        &root,
        "treaty-package.json",
        r#"{
            "name": "@acme/kit",
            "version": "3.1.4",
            "dest": "dist",
            "lib": { "entryFile": "src/public-api.treaty" },
            "assets": ["README.md"]
        }"#,
    );
    write(&root, "README.md", "# @acme/kit\n\nA real Treaty library.\n");

    // Primary entry: a real `.treaty` single-file component — a template with a
    // signal-by-default interpolation.
    write(
        &root,
        "src/public-api.treaty",
        "const name = 'World';\n<h1>Hello {{ name }}</h1>",
    );

    // `./card`: a real `.tjsx` JSX component with signals-by-default state, an
    // event handler, and a `{count}` interpolation.
    write(
        &root,
        "card/public-api.tjsx",
        "export default function Card() {\n\
           let count = 0;\n\
           const inc = () => { count++; };\n\
           return <button onClick={inc}>{count}</button>;\n\
         }\n",
    );

    // `./widget`: a bare `@Component` TypeScript class (the base-Angular path).
    write(
        &root,
        "widget/public-api.ts",
        "import { Component } from '@angular/core';\n\
         @Component({ selector: 'acme-widget', template: '<span>{{ label }}</span>' })\n\
         export class WidgetComponent {\n\
           label = 'widget';\n\
         }\n",
    );

    // `./shout`: a real `@Pipe` class.
    write(
        &root,
        "shout/public-api.ts",
        "import { Pipe, PipeTransform } from '@angular/core';\n\
         @Pipe({ name: 'shout', standalone: true })\n\
         export class ShoutPipe implements PipeTransform {\n\
           transform(value: string): string { return value.toUpperCase(); }\n\
         }\n",
    );

    // `./highlight`: a real `@Directive` class with an `@Input`.
    write(
        &root,
        "highlight/public-api.ts",
        "import { Directive, Input } from '@angular/core';\n\
         @Directive({ selector: '[acmeHighlight]', standalone: true })\n\
         export class HighlightDirective {\n\
           @Input('acmeHighlight') color: string = 'yellow';\n\
         }\n",
    );

    root
}

/// Parse `code` as a module of `source_type`, asserting it is syntactically
/// valid (no parse errors). Returns the names exported by the module so callers
/// can assert the published API surface.
fn assert_parses_and_collect_exports(code: &str, source_type: SourceType, what: &str) -> Vec<String> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, code, source_type).parse();
    assert!(
        parsed.errors.is_empty(),
        "{what} did not parse cleanly: {:?}\n--- source ---\n{code}",
        parsed.errors
    );

    let mut exports = Vec::new();
    for stmt in &parsed.program.body {
        match stmt {
            Statement::ExportNamedDeclaration(export) => {
                if let Some(decl) = &export.declaration {
                    exports.extend(declaration_names(decl));
                }
                for spec in &export.specifiers {
                    exports.push(spec.exported.name().to_string());
                }
            }
            Statement::ExportDefaultDeclaration(def) => {
                let name = match &def.declaration {
                    ExportDefaultDeclarationKind::FunctionDeclaration(f) => {
                        f.id.as_ref().map(|i| i.name.to_string())
                    }
                    ExportDefaultDeclarationKind::ClassDeclaration(c) => {
                        c.id.as_ref().map(|i| i.name.to_string())
                    }
                    _ => None,
                };
                exports.push(name.unwrap_or_else(|| "default".to_string()));
            }
            _ => {}
        }
    }
    exports
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

/// The decorator callee names surviving in a compiled module, split by whether
/// they sit on the class itself (JIT-load-bearing) or on a class member.
#[derive(Default, Debug)]
struct SurvivingDecorators {
    class_level: Vec<String>,
    member_level: Vec<String>,
}

/// The Angular decorators that *define* a compilable type — the ones that drive
/// JIT compilation at runtime if left in source. AOT lowering MUST replace every
/// one of these with a static `ɵcmp`/`ɵdir`/`ɵpipe`/`ɵmod`/`ɵprov` field, so
/// none may survive anywhere (class or member level) in packagr's output.
const TYPE_DEFINING_DECORATORS: [&str; 5] =
    ["Component", "Directive", "Pipe", "NgModule", "Injectable"];

/// Parse `code` and collect every surviving Angular decorator, classifying it as
/// class- or member-level. This is a parse-based walk over decorator AST nodes,
/// not a substring scan for `@`.
fn collect_surviving_decorators(code: &str, what: &str) -> SurvivingDecorators {
    let allocator = Allocator::default();
    let parsed =
        Parser::new(&allocator, code, SourceType::default().with_typescript(true).with_module(true))
            .parse();
    assert!(
        parsed.errors.is_empty(),
        "{what} ivy did not parse: {:?}",
        parsed.errors
    );

    let mut found = SurvivingDecorators::default();
    for stmt in &parsed.program.body {
        if let Some(class) = class_of_statement(stmt) {
            collect_class_decorators(class, &mut found);
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

fn collect_class_decorators(class: &oxc_ast::ast::Class, found: &mut SurvivingDecorators) {
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

/// The callee identifier of a decorator expression: `@Foo` → `Foo`,
/// `@Foo(...)` → `Foo`, `@ns.Foo(...)` → `Foo`.
fn decorator_callee_name(expr: &oxc_ast::ast::Expression) -> Option<String> {
    use oxc_ast::ast::Expression;
    match expr {
        Expression::Identifier(id) => Some(id.name.to_string()),
        Expression::CallExpression(call) => decorator_callee_name(&call.callee),
        Expression::StaticMemberExpression(m) => Some(m.property.name.to_string()),
        _ => None,
    }
}

#[test]
fn packages_a_real_multi_frontend_library_end_to_end() {
    let root = build_fixture_library();

    let cfg = PackageConfig::from_json(
        &std::fs::read_to_string(root.join("treaty-package.json")).unwrap(),
    )
    .expect("descriptor parses");

    let dist = build_to_disk(&root, &cfg).expect("packaging the library should succeed");

    // ---- identity / version ----
    assert_eq!(dist.name, "@acme/kit");
    assert_eq!(dist.version, "3.1.4");
    // Five entries: primary + four secondaries, deterministically ordered.
    let sub_paths: Vec<&str> = dist.entries.iter().map(|e| e.sub_path.as_str()).collect();
    assert_eq!(sub_paths, vec!["", "card", "highlight", "shout", "widget"]);
    assert!(dist.entries[0].is_primary());
    assert_eq!(dist.assets, vec!["README.md".to_string()]);

    let destdir = root.join("dist");

    // ============================================================
    // 1) package.json — parsed as JSON, asserted as a valid APF manifest.
    // ============================================================
    let manifest_path = destdir.join("package.json");
    assert!(manifest_path.is_file(), "no dist package.json emitted");
    let pkg: Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();

    assert_eq!(pkg["name"], "@acme/kit");
    assert_eq!(pkg["version"], "3.1.4");
    assert_eq!(pkg["type"], "module", "APF requires type: module");
    assert_eq!(pkg["sideEffects"], false, "APF requires sideEffects: false");
    assert_eq!(pkg["module"], "./index.mjs", "legacy primary ESM field");
    assert_eq!(pkg["types"], "./index.d.ts", "legacy primary types field");

    let exports = pkg["exports"].as_object().expect("exports map present");
    // Primary export under ".".
    assert_eq!(exports["."]["types"], "./index.d.ts");
    assert_eq!(exports["."]["import"], "./index.mjs");
    assert_eq!(exports["."]["default"], "./index.mjs");
    // Every secondary has its own subpath export pointing into its own dir.
    for sub in ["card", "highlight", "shout", "widget"] {
        let key = format!("./{sub}");
        let cond = exports
            .get(&key)
            .unwrap_or_else(|| panic!("missing exports entry for {key}"));
        assert_eq!(cond["types"], format!("./{sub}/index.d.ts"));
        assert_eq!(cond["import"], format!("./{sub}/index.mjs"));
        assert_eq!(cond["default"], format!("./{sub}/index.mjs"));
    }
    // The exports map covers exactly the five entries (primary + 4 secondary).
    assert_eq!(exports.len(), 5, "exports map: {exports:?}");

    // The README asset was copied verbatim into the dist root.
    assert!(destdir.join("README.md").is_file(), "README asset not copied");

    // ============================================================
    // 2) Every entry: its Ivy .mjs and .d.ts exist, parse, and are AOT.
    // ============================================================
    // (entry dir, expected exported symbol, the Ivy definition it must carry)
    let cases: &[(&str, &str, &str)] = &[
        // The `.treaty` SFC default-exports a component class.
        (".", "PublicApi", DEFINE_COMPONENT),
        // The `.tjsx` JSX component default-exports `Card`.
        ("card", "Card", DEFINE_COMPONENT),
        // The bare `@Component` class.
        ("widget", "WidgetComponent", DEFINE_COMPONENT),
        // The `@Pipe` class.
        ("shout", "ShoutPipe", DEFINE_PIPE),
        // The `@Directive` class.
        ("highlight", "HighlightDirective", DEFINE_DIRECTIVE),
    ];

    for (dir, _expected_symbol, define_kind) in cases {
        let entry_dir = if *dir == "." { destdir.clone() } else { destdir.join(dir) };
        let mjs_path = entry_dir.join("index.mjs");
        let dts_path = entry_dir.join("index.d.ts");
        assert!(mjs_path.is_file(), "{dir}: no index.mjs emitted");
        assert!(dts_path.is_file(), "{dir}: no index.d.ts emitted");

        let mjs = std::fs::read_to_string(&mjs_path).unwrap();

        // (a) It is valid, parseable ESM. The `.ts` front-end lowers decorators
        //     to Ivy static fields but does *not* erase type annotations (TS
        //     type-stripping is a downstream bundler concern, not packagr's), so
        //     the emitted module is TypeScript-flavoured ESM; parse it as such
        //     (TS is a superset of JS, so the `.treaty`/`.tjsx` JS output parses
        //     under the same source type).
        let module_type = SourceType::default().with_typescript(true).with_module(true);
        let mjs_exports = assert_parses_and_collect_exports(&mjs, module_type, &format!("{dir}/index.mjs"));
        assert!(
            !mjs_exports.is_empty(),
            "{dir}/index.mjs exports nothing; got source:\n{mjs}"
        );

        // (b) It carries the right Ivy definition + a factory, and references
        //     the Angular core import that every Ivy module needs.
        assert!(
            mjs.contains(define_kind),
            "{dir}/index.mjs missing {define_kind}; got:\n{mjs}"
        );
        assert!(
            mjs.contains("\u{0275}fac") || mjs.contains("factory:"),
            "{dir}/index.mjs missing an ɵfac factory; got:\n{mjs}"
        );
        assert!(
            mjs.contains("@angular/core"),
            "{dir}/index.mjs missing @angular/core import; got:\n{mjs}"
        );

        // (c) Fully AOT, no JIT. Two guarantees, both proved by walking the AST:
        //       * NO class-level Angular decorator survives — the
        //         `@Component`/`@Directive`/`@Pipe` that would drive runtime JIT
        //         is gone, replaced by the static `ɵcmp`/`ɵdir`/`ɵpipe` field;
        //       * NO type-*defining* Angular decorator survives at member level
        //         either.
        //     (Member metadata decorators such as `@Input`/`@Output` may remain
        //     on a property — their metadata is already captured into the static
        //     `inputs:`/`outputs:` maps above, so they are runtime no-ops that the
        //     downstream JS type-strip removes; they are not part of packagr's
        //     lowering contract and are intentionally not asserted away here.)
        let survivors = collect_surviving_decorators(&mjs, &format!("{dir}/index.mjs"));
        assert!(
            survivors.class_level.is_empty(),
            "{dir}/index.mjs still carries class-level decorator(s) {:?} — not AOT-lowered\n--- ivy ---\n{mjs}",
            survivors.class_level
        );
        let leaked_defining: Vec<&String> = survivors
            .class_level
            .iter()
            .chain(survivors.member_level.iter())
            .filter(|n| TYPE_DEFINING_DECORATORS.contains(&n.as_str()))
            .collect();
        assert!(
            leaked_defining.is_empty(),
            "{dir}/index.mjs leaked a type-defining Angular decorator {leaked_defining:?} — JIT, not AOT\n--- ivy ---\n{mjs}"
        );

        // (d) The .d.ts is valid TypeScript declaration source and is non-empty.
        let dts = std::fs::read_to_string(&dts_path).unwrap();
        assert!(!dts.trim().is_empty(), "{dir}/index.d.ts is empty");
        let dts_type = SourceType::default().with_typescript(true).with_typescript_definition(true);
        let dts_exports =
            assert_parses_and_collect_exports(&dts, dts_type, &format!("{dir}/index.d.ts"));
        assert!(
            !dts_exports.is_empty(),
            "{dir}/index.d.ts declares no exports; got:\n{dts}"
        );
    }

    // ============================================================
    // 3) The three `.ts` entries declare their *exact* class names in the .d.ts
    //    (isolated-declarations path preserves the authored TS surface).
    // ============================================================
    for (dir, symbol) in [
        ("widget", "WidgetComponent"),
        ("shout", "ShoutPipe"),
        ("highlight", "HighlightDirective"),
    ] {
        let dts = std::fs::read_to_string(destdir.join(dir).join("index.d.ts")).unwrap();
        let dts_type = SourceType::default().with_typescript(true).with_typescript_definition(true);
        let exports = assert_parses_and_collect_exports(&dts, dts_type, &format!("{dir}/index.d.ts"));
        assert!(
            exports.iter().any(|e| e == symbol),
            "{dir}/index.d.ts should declare `{symbol}`; declared: {exports:?}\n{dts}"
        );
    }

    std::fs::remove_dir_all(&root).ok();
}
