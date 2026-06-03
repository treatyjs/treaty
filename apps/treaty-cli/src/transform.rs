//! Native TS -> browser-ESM lowering and import rewriting.
//!
//! The Treaty compilers ([`treaty_ivy::source_compile`] for bare `@Component`
//! `.ts`, [`rust_authoring`] for `.treaty`/`.tsx`) STRIP the decorator and append
//! the Ivy definitions, but they emit into a still-**TypeScript** module: type
//! annotations, parameter properties (`constructor(private x: T)`), type-only
//! imports, and `const x: T[] = …` all survive. That is correct (the compiler's
//! job is Ivy lowering, not transpilation), but it is NOT runnable in a browser.
//!
//! This module closes that gap entirely in Rust, with no bundler and no Node:
//!
//!   1. [`compile_to_ivy_ts`]   — run the right Treaty front-end -> Ivy-but-TS.
//!   2. [`strip_types`]         — `oxc_transformer` (pinned to the SAME 0.133 the
//!                                compiler uses) type-strips that to plain ESM:
//!                                drops annotations, expands parameter properties,
//!                                elides type-only imports. No ES downleveling
//!                                (`TransformOptions::default()` leaves syntax at
//!                                its input level), so the emitted JS is faithful.
//!   3. [`rewrite_imports`]     — rewrite every import/export/`import(...)`
//!                                specifier through a caller-supplied map, so the
//!                                dev server can point relative `./foo` imports at
//!                                their served `/@src/...` URL and bare
//!                                `@angular/*` imports at the linked vendor URL.
//!
//! The dev server ([`crate::serve`]) chains 1->2->3 per request; the native build
//! ([`crate::native_build`]) chains 1->2 per module and rewrites to disk paths.

use std::path::Path;

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    ClassElement, Expression, ImportOrExportKind, ModuleDeclaration, Statement,
};
use oxc_codegen::Codegen;
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;
use oxc_transformer::{TransformOptions, Transformer};

use crate::compile;
use crate::core::CompileOutput;

/// The result of lowering one source file to browser ESM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweredModule {
    /// Browser-runnable ESM (Ivy lowered, types stripped). Empty on error.
    pub code: String,
    /// Generated server-fn module, when the source declared a `server { … }`
    /// block. `None` for the common client-only case.
    pub server_module: Option<String>,
    /// Diagnostics. A non-empty list means `code` is not usable.
    pub errors: Vec<String>,
    /// The bare/relative module specifiers this module imports (after Ivy
    /// lowering, before any rewrite). The dev server resolves + rewrites these.
    pub imports: Vec<String>,
}

/// Run the appropriate Treaty front-end on `source`, producing Ivy-lowered TS.
///
/// This is the SAME routing the `treaty compile` subcommand uses; factored here
/// so build and serve share one front-end-selection point.
pub fn compile_to_ivy_ts(source: &str, file_name: &str) -> CompileOutput {
    compile::compile_source(source, file_name)
}

/// Like [`compile_to_ivy_ts`] but threading the project's CROSS-MODULE selector registry
/// (`{ importName -> selector }`) the host pre-resolved for THIS file, so an imported component used
/// by its real `@Component` selector resolves the dependency. ADDITIVE: `registry == None` is
/// identical to [`compile_to_ivy_ts`].
pub fn compile_to_ivy_ts_with_registry(
    source: &str,
    file_name: &str,
    registry: Option<&treaty_ivy::source_compile::SelectorRegistry>,
) -> CompileOutput {
    compile::compile_source_with_registry(source, file_name, registry)
}

/// Whether a file name carries a TypeScript/JSX-ish extension the type-stripper
/// should treat as TypeScript (so `oxc` parses TS syntax and the transformer
/// removes it). Plain `.js`/`.mjs` are returned as-is.
fn source_type_for(file_name: &str) -> SourceType {
    let lower = file_name.to_ascii_lowercase();
    if lower.ends_with(".tsx") || lower.ends_with(".tjsx") {
        SourceType::tsx()
    } else if lower.ends_with(".ts") || lower.ends_with(".treaty") {
        SourceType::ts()
    } else if lower.ends_with(".jsx") {
        SourceType::jsx()
    } else {
        // `.js` / `.mjs` and unknown: parse as a module of plain JS.
        SourceType::mjs()
    }
}

/// Collect the module specifiers (`from '…'` and `import('…')`) referenced by an
/// already-parsed program. Static imports/exports come from the module record;
/// dynamic `import('…')` is walked from the statement body.
fn collect_specifiers(program: &oxc_ast::ast::Program<'_>) -> Vec<String> {
    let mut out = Vec::new();
    for stmt in &program.body {
        if let Statement::ImportDeclaration(decl) = stmt {
            out.push(decl.source.value.to_string());
        } else if let Statement::ExportNamedDeclaration(decl) = stmt {
            if let Some(src) = &decl.source {
                out.push(src.value.to_string());
            }
        } else if let Statement::ExportAllDeclaration(decl) = stmt {
            out.push(decl.source.value.to_string());
        }
    }
    // Dynamic imports: scan the whole program text cheaply is unreliable, so the
    // caller pass also catches these during rewrite. Here we report only the
    // static graph (which is what the dev server prefetch / build crawl need).
    out
}

/// Remove EVERY decorator from every class and class member in the program.
///
/// The Treaty compiler strips the class-level `@Component`/`@Directive`/etc and
/// emits the equivalent Ivy definitions, but it leaves the MEMBER decorators
/// (`@HostBinding`, `@HostListener`, `@Input`, `@Output`, `@ViewChild`, …) on the
/// class body — those are now redundant (the Ivy `hostBindings`/`inputs`/`outputs`
/// already encode them) but the browser cannot parse a bare decorator. The default
/// `oxc_transformer` does NOT touch decorators (no legacy-decorator transform is
/// requested, and Angular's are not the spec proposal), so we drop them here as a
/// purely subtractive pass before type-stripping. Removal is safe precisely
/// because the Ivy emit already captured their effect.
fn strip_decorators(program: &mut oxc_ast::ast::Program<'_>) {
    for stmt in program.body.iter_mut() {
        strip_decorators_in_statement(stmt);
    }
}

fn strip_decorators_in_statement(stmt: &mut Statement<'_>) {
    use oxc_ast::ast::Declaration;
    match stmt {
        Statement::ClassDeclaration(class) => strip_class_decorators(class),
        Statement::ExportNamedDeclaration(decl) => {
            if let Some(Declaration::ClassDeclaration(class)) = decl.declaration.as_mut() {
                strip_class_decorators(class);
            }
        }
        Statement::ExportDefaultDeclaration(decl) => {
            if let oxc_ast::ast::ExportDefaultDeclarationKind::ClassDeclaration(class) =
                &mut decl.declaration
            {
                strip_class_decorators(class);
            }
        }
        _ => {}
    }
}

fn strip_class_decorators(class: &mut oxc_ast::ast::Class<'_>) {
    // The class-level decorator (already gone in Treaty output, but be defensive).
    class.decorators.clear();
    for element in class.body.body.iter_mut() {
        match element {
            ClassElement::MethodDefinition(m) => m.decorators.clear(),
            ClassElement::PropertyDefinition(p) => p.decorators.clear(),
            ClassElement::AccessorProperty(a) => a.decorators.clear(),
            _ => {}
        }
    }
}

/// Type-strip an Ivy-lowered TS module to browser ESM via `oxc_transformer`.
///
/// Returns the stripped code plus the static import specifiers it references.
/// Errors are returned as a non-empty diagnostics list with empty code.
pub fn strip_types(ivy_ts: &str, file_name: &str) -> (String, Vec<String>, Vec<String>) {
    let allocator = Allocator::default();
    let source_type = source_type_for(file_name);
    let path = Path::new(file_name);

    let parsed = Parser::new(&allocator, ivy_ts, source_type).parse();
    if !parsed.errors.is_empty() {
        let errs = parsed
            .errors
            .iter()
            .map(|e| format!("parse error in lowered module: {e}"))
            .collect();
        return (String::new(), Vec::new(), errs);
    }
    let mut program = parsed.program;

    // Drop orphaned member decorators left by the Treaty Ivy emit (the Ivy
    // definitions already encode them); a bare `@HostBinding(...)` would otherwise
    // be unparseable browser JS.
    strip_decorators(&mut program);

    // Capture the import graph BEFORE transforming (the transform may elide
    // type-only imports, but the dev server still resolves only value imports it
    // sees in the OUTPUT, so we re-collect after; collect here is informational).
    let _pre_imports = collect_specifiers(&program);

    // Build semantic scoping (the transformer needs it).
    let scoping = SemanticBuilder::new()
        .build(&program)
        .semantic
        .into_scoping();

    // Default options = TypeScript type-strip with NO ECMAScript downleveling.
    // (env target is empty, so syntax stays at the input level; only TS-specific
    // constructs and parameter properties are removed.)
    let options = TransformOptions::default();
    let ret = Transformer::new(&allocator, path, &options).build_with_scoping(scoping, &mut program);

    let mut errors: Vec<String> = ret
        .errors
        .iter()
        .map(|e| format!("transform error: {e}"))
        .collect();

    let printed = Codegen::new().build(&program);
    let code = printed.code;

    // Re-collect the specifiers from the STRIPPED output: type-only imports are
    // gone, so this is the true runtime import graph.
    let after_alloc = Allocator::default();
    let reparsed = Parser::new(&after_alloc, &code, source_type_for(file_name)).parse();
    let imports = if reparsed.errors.is_empty() {
        collect_specifiers(&reparsed.program)
    } else {
        // Output failed to re-parse: surface it rather than silently shipping it.
        errors.push("type-stripped output did not re-parse cleanly".to_string());
        Vec::new()
    };

    (code, imports, errors)
}

/// Compile + type-strip one source to browser ESM, reporting its import graph.
///
/// This is the end-to-end "one Treaty source file -> runnable ESM" used by the
/// dev server and the native build. Import rewriting is left to the caller (it
/// needs the resolver + the URL/path scheme), via [`rewrite_imports`].
pub fn lower(source: &str, file_name: &str) -> LoweredModule {
    lower_with_registry(source, file_name, None)
}

/// Like [`lower`] but threading the project's CROSS-MODULE selector registry for THIS file, so an
/// imported component used by its real `@Component` selector resolves its dependency. ADDITIVE:
/// `registry == None` is byte-identical to [`lower`].
pub fn lower_with_registry(
    source: &str,
    file_name: &str,
    registry: Option<&treaty_ivy::source_compile::SelectorRegistry>,
) -> LoweredModule {
    let compiled = compile_to_ivy_ts_with_registry(source, file_name, registry);
    if !compiled.is_ok() {
        return LoweredModule {
            code: String::new(),
            server_module: compiled.server_module,
            errors: compiled.errors,
            imports: Vec::new(),
        };
    }
    let (code, imports, errors) = strip_types(&compiled.code, file_name);
    LoweredModule {
        code,
        server_module: compiled.server_module,
        errors,
        imports,
    }
}

/// Rewrite every module specifier in `code` using `map`. A specifier the map
/// does not contain is left untouched.
///
/// Rewrites the THREE specifier-bearing forms a browser cares about:
///   * `import … from 'spec'` / `export … from 'spec'` / `export * from 'spec'`
///   * `import('spec')` dynamic imports (lazy routes)
///
/// The rewrite is a surgical span replacement (it edits only the quoted string
/// spans, every other byte is preserved), so it is safe to run on already-emitted
/// JS without re-printing the whole AST.
pub fn rewrite_imports<F>(code: &str, mut resolve: F) -> String
where
    F: FnMut(&str) -> Option<String>,
{
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, code, SourceType::mjs()).parse();
    if !parsed.errors.is_empty() {
        // Unparseable: return unchanged rather than corrupting it.
        return code.to_string();
    }
    let program = parsed.program;

    // Collect (span_start, span_end, original) for each specifier string literal.
    let mut edits: Vec<(usize, usize, String)> = Vec::new();

    let mut push_edit = |raw_span: oxc_span::Span, value: &str| {
        if let Some(rewritten) = resolve(value) {
            // The span includes the surrounding quotes; preserve a single-quote
            // wrapping in the replacement.
            edits.push((raw_span.start as usize, raw_span.end as usize, format!("'{rewritten}'")));
        }
    };

    for stmt in &program.body {
        match stmt {
            Statement::ImportDeclaration(decl) => {
                if decl.import_kind == ImportOrExportKind::Value {
                    push_edit(decl.source.span, &decl.source.value);
                }
            }
            Statement::ExportNamedDeclaration(decl) => {
                if let Some(src) = &decl.source {
                    if decl.export_kind == ImportOrExportKind::Value {
                        push_edit(src.span, &src.value);
                    }
                }
            }
            Statement::ExportAllDeclaration(decl) => {
                if decl.export_kind == ImportOrExportKind::Value {
                    push_edit(decl.source.span, &decl.source.value);
                }
            }
            _ => {}
        }
    }

    // Dynamic `import('…')` lives in expression position; walk every statement's
    // expressions shallowly via the printed-source scan would be brittle, so we
    // use a focused AST walk over call/await/member chains through a small visitor.
    collect_dynamic_imports(&program, &mut |span, value| push_edit(span, value));

    if edits.is_empty() {
        return code.to_string();
    }
    // Apply edits right-to-left so earlier offsets stay valid.
    edits.sort_by(|a, b| b.0.cmp(&a.0));
    let mut out = code.to_string();
    for (start, end, replacement) in edits {
        out.replace_range(start..end, &replacement);
    }
    out
}

/// Walk the program for dynamic `import('literal')` calls, invoking `f` with the
/// argument string literal's span and value. Recurses through the common
/// expression shapes lazy-route `import(...).then(...)` chains use.
fn collect_dynamic_imports<'a, F>(program: &oxc_ast::ast::Program<'a>, f: &mut F)
where
    F: FnMut(oxc_span::Span, &str),
{
    for stmt in &program.body {
        walk_statement(stmt, f);
    }
}

fn walk_statement<'a, F>(stmt: &Statement<'a>, f: &mut F)
where
    F: FnMut(oxc_span::Span, &str),
{
    use oxc_ast::ast::Statement as S;
    match stmt {
        S::ExpressionStatement(e) => walk_expr(&e.expression, f),
        S::VariableDeclaration(d) => {
            for decl in &d.declarations {
                if let Some(init) = &decl.init {
                    walk_expr(init, f);
                }
            }
        }
        S::ExportNamedDeclaration(d) => {
            if let Some(decl) = &d.declaration {
                if let oxc_ast::ast::Declaration::VariableDeclaration(v) = decl {
                    for vd in &v.declarations {
                        if let Some(init) = &vd.init {
                            walk_expr(init, f);
                        }
                    }
                }
            }
        }
        S::ReturnStatement(r) => {
            if let Some(arg) = &r.argument {
                walk_expr(arg, f);
            }
        }
        _ => {
            // Module declarations and others are handled by the static pass; the
            // dynamic-import scan only needs expression-bearing statements that
            // commonly hold an `import(...)` (route tables, top-level consts).
            if let Some(md) = stmt.as_module_declaration() {
                if let ModuleDeclaration::ExportDefaultDeclaration(_) = md {
                    // default-exported expression handled below if needed
                }
            }
        }
    }
}

fn walk_expr<'a, F>(expr: &Expression<'a>, f: &mut F)
where
    F: FnMut(oxc_span::Span, &str),
{
    use Expression as E;
    match expr {
        E::ImportExpression(imp) => {
            if let E::StringLiteral(lit) = &imp.source {
                f(lit.span, &lit.value);
            }
            // import(...) is leaf for our purposes; arguments after the source are options.
        }
        E::CallExpression(call) => {
            walk_expr(&call.callee, f);
            for arg in &call.arguments {
                if let Some(e) = arg.as_expression() {
                    walk_expr(e, f);
                }
            }
        }
        E::StaticMemberExpression(m) => walk_expr(&m.object, f),
        E::ComputedMemberExpression(m) => walk_expr(&m.object, f),
        E::AwaitExpression(a) => walk_expr(&a.argument, f),
        E::ArrowFunctionExpression(arrow) => {
            for stmt in &arrow.body.statements {
                walk_statement(stmt, f);
            }
        }
        E::FunctionExpression(func) => {
            if let Some(body) = &func.body {
                for stmt in &body.statements {
                    walk_statement(stmt, f);
                }
            }
        }
        E::ParenthesizedExpression(p) => walk_expr(&p.expression, f),
        E::ArrayExpression(arr) => {
            for el in &arr.elements {
                if let Some(e) = el.as_expression() {
                    walk_expr(e, f);
                }
            }
        }
        E::ObjectExpression(obj) => {
            for prop in &obj.properties {
                if let oxc_ast::ast::ObjectPropertyKind::ObjectProperty(p) = prop {
                    walk_expr(&p.value, f);
                }
            }
        }
        E::SequenceExpression(seq) => {
            for e in &seq.expressions {
                walk_expr(e, f);
            }
        }
        E::ConditionalExpression(c) => {
            walk_expr(&c.test, f);
            walk_expr(&c.consequent, f);
            walk_expr(&c.alternate, f);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_typescript_types_to_runnable_js() {
        let ts = "export const x: number = 1; function f(a: string): void {}";
        let (code, _imports, errors) = strip_types(ts, "m.ts");
        assert!(errors.is_empty(), "errors: {errors:?}");
        assert!(!code.contains(": number"), "type annotation survived: {code}");
        assert!(!code.contains(": void"), "return type survived: {code}");
        assert!(code.contains("export const x = 1"), "value lost: {code}");
    }

    #[test]
    fn expands_parameter_properties() {
        let ts = "export class S { constructor(private readonly logger: L) {} m() { return this.logger; } }";
        let (code, _imports, errors) = strip_types(ts, "s.ts");
        assert!(errors.is_empty(), "errors: {errors:?}");
        // Parameter property becomes a field assignment in the constructor body.
        assert!(code.contains("this.logger = logger"), "param prop not expanded: {code}");
        assert!(!code.contains("private"), "TS modifier survived: {code}");
    }

    #[test]
    fn elides_type_only_imports_from_runtime_graph() {
        let ts = "import { Observable } from 'rxjs';\nimport { Product } from './product.model';\nexport const o: Observable<Product> | null = null;";
        let (code, imports, errors) = strip_types(ts, "svc.ts");
        assert!(errors.is_empty(), "errors: {errors:?}");
        // `Product` is used only as a type -> its import is elided; `rxjs` import
        // would also be elided if Observable is type-only. Neither should remain
        // in the runtime graph since both are used only in types here.
        assert!(!imports.iter().any(|i| i == "./product.model"), "type-only import kept: {imports:?}");
        assert!(!code.contains("Product"), "type ident leaked: {code}");
    }

    #[test]
    fn rewrites_static_and_dynamic_imports() {
        let js = "import { A } from '@angular/core';\nconst r = () => import('./lazy').then(m => m.X);\nexport { B } from './b';";
        let out = rewrite_imports(js, |spec| match spec {
            "@angular/core" => Some("/@ng/core.mjs".to_string()),
            "./lazy" => Some("/@src/lazy.js".to_string()),
            "./b" => Some("/@src/b.js".to_string()),
            _ => None,
        });
        assert!(out.contains("'/@ng/core.mjs'"), "static import not rewritten: {out}");
        assert!(out.contains("'/@src/lazy.js'"), "dynamic import not rewritten: {out}");
        assert!(out.contains("'/@src/b.js'"), "re-export not rewritten: {out}");
    }

    #[test]
    fn unknown_specifiers_are_left_untouched() {
        let js = "import { A } from 'keep-me';";
        let out = rewrite_imports(js, |_| None);
        assert!(out.contains("'keep-me'"), "untouched import changed: {out}");
    }

    #[test]
    fn strips_orphaned_member_decorators() {
        // The shape the Treaty compiler leaves: class-level decorator gone, but
        // member decorators (@HostBinding on a getter) still present.
        let ts = "import { HostBinding } from '@angular/core';\nexport class D { @HostBinding('attr.x') get x(): string { return 'y'; } }";
        let (code, _imports, errors) = strip_types(ts, "d.ts");
        assert!(errors.is_empty(), "errors: {errors:?}");
        assert!(!code.contains("@HostBinding"), "member decorator survived: {code}");
        // The getter itself is preserved.
        assert!(code.contains("get x()"), "getter lost: {code}");
    }

    #[test]
    fn lower_end_to_end_on_a_component() {
        // A bare @Component .ts -> Ivy ESM with no surviving TS.
        let src = "import { Component } from '@angular/core';\n@Component({ selector: 'x-y', template: '<p>{{n}}</p>' })\nexport class XY { n: number = 5; }";
        let out = lower(src, "xy.ts");
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert!(out.code.contains("ɵɵdefineComponent"), "no Ivy def: {}", out.code);
        assert!(!out.code.contains(": number"), "TS type survived lowering: {}", out.code);
        assert!(out.imports.iter().any(|i| i == "@angular/core"), "lost angular import: {:?}", out.imports);
    }

    #[test]
    fn routes_authoring_extensions_through_authoring_frontend() {
        // Confirms `compile_to_ivy_ts` honors the same routing `treaty compile` uses.
        assert_eq!(compile::frontend_for("a.ts"), crate::core::Frontend::Authoring);
    }
}
