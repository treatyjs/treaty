//! `.treaty` single-file-component compiler (Rust port of the REPL's `treat-to-ivy.ts`).
//!
//! Splits a `.treaty` source into JavaScript / HTML / CSS chunks using the crate's own
//! [`crate::treaty`] lexer + parser (no regex), derives a standalone component, and emits the
//! `ɵɵdefineComponent({...})` definition via the `render3` Ivy engine.
//!
//! Pipeline (mirrors `apps/repl/src/tools/treaty-sfc/treat-to-ivy.ts`):
//! ```text
//! treaty::Lexer  -> tokens
//! treaty::Parser -> Ast { nodes }
//!   JavaScript nodes -> (script, currently unused by render3's template emitter)
//!   Html nodes       -> template
//!   Style nodes      -> styles
//! render3::ml_parser + template_transform -> r3_ast
//! render3 compile_component_from_metadata  -> ɵɵdefineComponent
//! ```
//!
//! Component derivation:
//!   * class name: PascalCase of `file_name` (stem only, extension stripped)
//!   * selector:   `None` (SELECTORLESS / class-name based resolution)
//!   * standalone: `true`
//!   * template:   the joined HTML chunks
//!   * styles:     the CSS chunks (newlines/tabs stripped, as the TS pipeline does)
//!
//! Component references in the template resolve by class name through render3's selectorless
//! binder, and template dependencies are auto-collected — there are no manual `imports`.

use oxc_allocator::Allocator;
use oxc_ast::ast::Expression;
use oxc_parser::Parser as JsParser;
use oxc_span::SourceType;

use render3::compile::{CompiledComponent, RealTemplateBuilder};
use render3::output::emitter::emit_expression;
use render3::output_ast::{self as o, ParseSourceSpan};
use render3::template::template_transform::{
    html_ast_to_render3_ast, BindingParser, Render3ParseOptions,
};
use render3::util::{R3CompiledExpression, R3Reference};
use render3::view::compiler::{
    compile_component_from_metadata, ChangeDetection, ChangeDetectionStrategy, ComponentTemplate,
    DeclarationListEmitMode, Deps, Lifecycle, OrderedMap, R3ComponentDeferMetadata,
    R3ComponentMetadata, R3DirectiveMetadata, R3HostMetadata, R3InputMetadata,
    R3TemplateDependencyMetadata, StubHostBindingsBuilder, ViewEncapsulation,
};

use crate::treaty::ast::AstNode;
use crate::treaty::lexer::Lexer;
use crate::treaty::parser::Parser;

/// The three source kinds extracted from a `.treaty` file.
#[derive(Debug, Default)]
struct TreatyChunks {
    javascript: Vec<String>,
    html: Vec<String>,
    css: Vec<String>,
}

/// Lex + parse `source` and bucket its nodes into JavaScript / HTML / CSS chunks.
///
/// Uses the crate's own treaty lexer/parser (no regex). The treaty lexer keeps `{{ … }}`
/// interpolation *inside* the surrounding HTML chunk, so the HTML bucket is a faithful template.
fn split_chunks(source: &str) -> TreatyChunks {
    let mut lexer = Lexer::new(source);
    let mut tokens = Vec::new();
    while let Some(token) = lexer.next_token() {
        tokens.push(token);
    }

    let mut parser = Parser::new(tokens);
    let ast = parser.parse();

    let mut chunks = TreatyChunks::default();
    for node in ast.nodes {
        match node {
            AstNode::JavaScript(code) => chunks.javascript.push(code),
            AstNode::Html(html) => chunks.html.push(html),
            AstNode::Style(style) => chunks.css.push(style),
            // Top-level interpolation/control-flow markers are not standalone template chunks in
            // the common case (they live inside an HTML chunk); the bare markers carry no body and
            // are ignored here.
            AstNode::TemplateExpression(_) | AstNode::ControlFlow(_) | AstNode::EOF => {}
        }
    }
    chunks
}

/// PascalCase the *stem* of a file name (extension and path separators dropped).
///
/// `"hello-world.treaty"` -> `"HelloWorld"`, `"my_widget.treaty"` -> `"MyWidget"`.
fn to_pascal_case(file_name: &str) -> String {
    // Drop directory components and the extension.
    let base = file_name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(file_name);
    let stem = base.split('.').next().unwrap_or(base);

    let mut out = String::new();
    let mut new_word = true;
    for ch in stem.chars() {
        if ch.is_ascii_alphanumeric() {
            if new_word {
                out.extend(ch.to_uppercase());
                new_word = false;
            } else {
                out.push(ch);
            }
        } else {
            // Any separator (space, '-', '_', etc.) starts a new word.
            new_word = true;
        }
    }

    if out.is_empty() {
        "TreatyComponent".to_string()
    } else {
        out
    }
}

/// Recognizes a signal initializer call: `input()`, `input.required()`, `model()`,
/// `model.required()`, `output()`. Returns the base callee identifier (`input`/`model`/`output`)
/// and whether `.required` was used. Mirrors `render3::source_compile::signal_call`.
fn signal_call<'a>(expr: &'a Expression<'a>) -> Option<(&'a str, bool)> {
    let Expression::CallExpression(call) = expr else {
        return None;
    };
    match &call.callee {
        // `input(...)`, `output(...)`, `model(...)`
        Expression::Identifier(id) => Some((id.name.as_str(), false)),
        // `input.required(...)`, `model.required(...)`
        Expression::StaticMemberExpression(member) => {
            if let Expression::Identifier(base) = &member.object {
                let required = member.property.name.as_str() == "required";
                Some((base.name.as_str(), required))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Parse the component-body JS chunk and extract Treaty signal inputs/outputs.
///
/// In the Treaty SFC model the top-level JS *is* the component body, so top-level
/// `const <name> = input()/input.required()/model()/output()` declarations become the
/// component's signal inputs/outputs. Mirrors `render3::source_compile` signal extraction:
///   * `input()`            → signal input
///   * `input.required()`   → required signal input
///   * `model()`            → signal input + paired `<name>Change` output
///   * `output()`           → output
///
/// Parse failures are non-fatal: the JS chunk is the user's free-form body and may use syntax
/// the template path does not care about, so an unparseable chunk simply yields no I/O.
fn extract_io(
    javascript: &str,
    inputs: &mut OrderedMap<String, R3InputMetadata>,
    outputs: &mut OrderedMap<String, String>,
) {
    if javascript.trim().is_empty() {
        return;
    }

    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true);
    let ret = JsParser::new(&allocator, javascript, source_type).parse();

    for stmt in &ret.program.body {
        let oxc_ast::ast::Statement::VariableDeclaration(decl) = stmt else {
            continue;
        };
        for declarator in &decl.declarations {
            let Some(name) = declarator.id.get_identifier_name() else {
                continue;
            };
            let name = name.to_string();
            let Some(init) = &declarator.init else {
                continue;
            };
            let Some((base, required)) = signal_call(init) else {
                continue;
            };
            match base {
                "input" | "model" => {
                    inputs.insert(
                        name.clone(),
                        R3InputMetadata {
                            class_property_name: name.clone(),
                            binding_property_name: name.clone(),
                            required,
                            is_signal: true,
                            transform_function: None,
                        },
                    );
                    if base == "model" {
                        let change = format!("{name}Change");
                        outputs.insert(change.clone(), change);
                    }
                }
                "output" => {
                    outputs.insert(name.clone(), name.clone());
                }
                _ => {}
            }
        }
    }
}

/// Collect the JS chunk's imported identifier names — the auto-import candidate set. Mirrors
/// `extractImportStrings` in `treat-to-ivy.ts`, but over the AST: every default, namespace and
/// named binding introduced by an `import` declaration. A parse failure yields no candidates.
fn collect_imported_names(javascript: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    if javascript.trim().is_empty() {
        return names;
    }

    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true);
    let ret = JsParser::new(&allocator, javascript, source_type).parse();

    for stmt in &ret.program.body {
        let oxc_ast::ast::Statement::ImportDeclaration(import) = stmt else {
            continue;
        };
        let Some(specifiers) = &import.specifiers else {
            continue;
        };
        for spec in specifiers {
            match spec {
                oxc_ast::ast::ImportDeclarationSpecifier::ImportSpecifier(s) => {
                    names.push(s.local.name.to_string());
                }
                oxc_ast::ast::ImportDeclarationSpecifier::ImportDefaultSpecifier(s) => {
                    names.push(s.local.name.to_string());
                }
                oxc_ast::ast::ImportDeclarationSpecifier::ImportNamespaceSpecifier(s) => {
                    names.push(s.local.name.to_string());
                }
            }
        }
    }
    names
}

/// Pieces extracted from the component-body JS chunk for module assembly.
///
/// Mirrors `createWrapper`/`extractImportStrings`/`removeImportsFromCode` in
/// `apps/repl/src/tools/treaty-sfc/treat-to-ivy.ts`:
///   * `imports` — the import declarations, sliced verbatim from the source.
///   * `body`    — the JS chunk with its import declarations removed.
///   * `bindings`— top-level `const`/`function` declaration names, for the returned object.
#[derive(Default)]
struct WrapperParts {
    imports: Vec<String>,
    body: String,
    bindings: Vec<String>,
}

/// Parse the component-body JS chunk and collect import statements (verbatim), the body with
/// imports stripped, and the top-level `const`/`function` declaration names.
fn extract_wrapper_parts(javascript: &str) -> WrapperParts {
    let mut parts = WrapperParts::default();
    if javascript.trim().is_empty() {
        return parts;
    }

    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true);
    let ret = JsParser::new(&allocator, javascript, source_type).parse();

    // Byte ranges of import declarations, to splice out of the body.
    let mut import_ranges: Vec<(usize, usize)> = Vec::new();

    for stmt in &ret.program.body {
        match stmt {
            oxc_ast::ast::Statement::ImportDeclaration(import) => {
                let start = import.span.start as usize;
                let end = import.span.end as usize;
                parts.imports.push(javascript[start..end].to_string());
                import_ranges.push((start, end));
            }
            oxc_ast::ast::Statement::VariableDeclaration(decl) => {
                for declarator in &decl.declarations {
                    if let Some(name) = declarator.id.get_identifier_name() {
                        parts.bindings.push(name.to_string());
                    }
                }
            }
            oxc_ast::ast::Statement::FunctionDeclaration(func) => {
                if let Some(id) = &func.id {
                    parts.bindings.push(id.name.to_string());
                }
            }
            _ => {}
        }
    }

    // Body = source with the import declaration byte ranges removed.
    if import_ranges.is_empty() {
        parts.body = javascript.to_string();
    } else {
        let mut body = String::with_capacity(javascript.len());
        let mut cursor = 0usize;
        for (start, end) in &import_ranges {
            body.push_str(&javascript[cursor..*start]);
            cursor = *end;
        }
        body.push_str(&javascript[cursor..]);
        parts.body = body;
    }

    parts
}

/// Assemble the full runnable ES module string, mirroring the TS `createWrapper`.
///
/// `render3`'s `emit_expression` prefixes the defineComponent expression with its own
/// `import * as i0 from "@angular/core";` line; the module emits that import once at the top, so
/// any such leading prefix is stripped from the `.ɵcmp` value here.
fn build_module(class_name: &str, javascript: &str, cmp_expression: &str) -> String {
    const I0_IMPORT: &str = "import * as i0 from \"@angular/core\";";
    let cmp_expression = cmp_expression
        .strip_prefix(I0_IMPORT)
        .map(str::trim_start)
        .unwrap_or(cmp_expression);

    let parts = extract_wrapper_parts(javascript);

    let mut module = String::new();
    module.push_str("import * as i0 from \"@angular/core\";\n");
    for import in &parts.imports {
        module.push_str(import);
        module.push('\n');
    }
    module.push_str(&format!("function {class_name}() {{\n"));
    module.push_str(parts.body.trim());
    module.push_str(&format!(
        "\nreturn {{ {} }};\n}}\n",
        parts.bindings.join(", ")
    ));
    module.push_str(&format!(
        "{class_name}.\u{0275}fac = function {class_name}_Factory(t) {{ return (t || {class_name})(); }};\n"
    ));
    module.push_str(&format!("{class_name}.\u{0275}cmp = {cmp_expression};\n"));
    module.push_str(&format!("export default {class_name};\n"));
    module
}

fn class_ref(class_name: &str) -> R3Reference {
    R3Reference {
        value: o::variable(class_name, None),
        ty: o::variable(class_name, None),
    }
}

/// Compile a `.treaty` SFC source into a full runnable ES module.
///
/// `file_name` derives the component class name (PascalCase of the stem). The component is
/// standalone, selectorless (class-name based), with the HTML chunk as its template and the CSS
/// chunk as its styles.
///
/// The emitted module mirrors the TS `createWrapper` (Treaty's runtime is a *function* component):
/// ```text
/// import * as i0 from "@angular/core";
/// <verbatim import statements from the JS chunk>
/// function <Comp>() { <JS chunk body, imports stripped> return { <const/function names> }; }
/// <Comp>.ɵfac = function <Comp>_Factory(t) { return (t || <Comp>)(); };
/// <Comp>.ɵcmp = ɵɵdefineComponent({ ... });
/// export default <Comp>;
/// ```
pub fn compile_treaty_file(source: &str, file_name: &str) -> CompiledComponent {
    let chunks = split_chunks(source);
    let class_name = to_pascal_case(file_name);

    let template_html = chunks.html.join("");
    // Match the TS pipeline: strip newlines/tabs from collected CSS.
    let styles: Vec<String> = chunks
        .css
        .iter()
        .map(|s| s.replace(['\n', '\r', '\t'], ""))
        .filter(|s| !s.is_empty())
        .collect();

    let mut errors: Vec<String> = Vec::new();

    // 0. Extract signal inputs/outputs from the component-body JS chunk.
    let mut inputs: OrderedMap<String, R3InputMetadata> = OrderedMap::new();
    let mut outputs: OrderedMap<String, String> = OrderedMap::new();
    let javascript = chunks.javascript.join("");
    extract_io(&javascript, &mut inputs, &mut outputs);
    let is_signal = inputs.iter().any(|(_, m)| m.is_signal);

    // 1. Template HTML -> HTML AST.
    let parse_result = render3::ml_parser::parse(&template_html, "template.html");
    for e in &parse_result.errors {
        errors.push(e.msg.clone());
    }

    // 2. HTML AST -> r3_ast. The binder resolves selectorless component refs by class name and
    //    collects dependencies automatically (no manual imports).
    let mut binding_parser = BindingParser::new();
    let r3 = html_ast_to_render3_ast(
        &parse_result.root_nodes,
        &mut binding_parser,
        Render3ParseOptions::default(),
    );
    for e in &r3.errors {
        errors.push(e.msg.clone());
    }

    // 2b. AUTO-IMPORT: resolve template dependencies from usage. The candidate set is the JS
    // chunk's imported identifiers; those actually referenced as `<Foo>` / `@Foo` / `<foo>` in the
    // template (via the selectorless binder) become the component's `dependencies`. Unused imports
    // are not emitted — mirroring `treat-to-ivy.ts`, but via the AST + binder rather than regex.
    let candidates = collect_imported_names(&javascript);
    let selectorless_nodes = render3::compile::parse_template_selectorless(&template_html);
    let declarations =
        render3::compile::resolve_template_dependencies(&candidates, &selectorless_nodes);
    let has_directive_dependencies = !declarations.is_empty();

    // 3. Standalone, selectorless component metadata.
    let base = R3DirectiveMetadata {
        name: class_name.clone(),
        ty: class_ref(&class_name),
        type_argument_count: 0,
        type_source_span: ParseSourceSpan::new(0, 0),
        deps: Deps::None,
        // SELECTORLESS: no selector → resolution is class-name based.
        selector: None,
        queries: Vec::new(),
        view_queries: Vec::new(),
        host: R3HostMetadata::default(),
        lifecycle: Lifecycle::default(),
        inputs,
        outputs,
        uses_inheritance: false,
        control_create: None,
        export_as: None,
        providers: None,
        is_standalone: true,
        is_signal,
        host_directives: None,
        legacy_optional_chaining: false,
    };

    let mut meta: R3ComponentMetadata<R3TemplateDependencyMetadata> = R3ComponentMetadata {
        base,
        template: ComponentTemplate {
            nodes: r3.nodes,
            ng_content_selectors: r3.ng_content_selectors,
            preserve_whitespaces: None,
        },
        declarations,
        defer: R3ComponentDeferMetadata::PerComponent {
            dependencies_fn: None,
        },
        declaration_list_emit_mode: DeclarationListEmitMode::Direct,
        styles,
        external_styles: None,
        encapsulation: ViewEncapsulation::Emulated,
        animations: None,
        view_providers: None,
        relative_context_file_path: file_name.to_string(),
        i18n_use_external_ids: false,
        change_detection: Some(ChangeDetection::Strategy(ChangeDetectionStrategy::OnPush)),
        relative_template_path: None,
        has_directive_dependencies,
        raw_imports: None,
        foreign_imports: None,
    };

    // 4. Emit the definition.
    let mut template_builder = RealTemplateBuilder;
    let mut host_builder = StubHostBindingsBuilder;
    let mut pool_statements = Vec::new();
    let compiled: R3CompiledExpression = compile_component_from_metadata(
        &mut meta,
        &mut template_builder,
        &mut host_builder,
        &mut pool_statements,
    );

    let cmp_expression = emit_expression(&compiled.expression);
    let code = build_module(&class_name, &javascript, &cmp_expression);
    CompiledComponent { code, errors }
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const DEFINE: &str = "\u{0275}\u{0275}defineComponent";

    #[test]
    fn pascal_case_from_file_name() {
        assert_eq!(to_pascal_case("hello-world.treaty"), "HelloWorld");
        assert_eq!(to_pascal_case("my_widget.treaty"), "MyWidget");
        assert_eq!(to_pascal_case("src/foo/Bar.treaty"), "Bar");
        assert_eq!(to_pascal_case("name"), "Name");
    }

    #[test]
    fn compiles_treaty_file_template_and_interpolation() {
        let source = "const name = 'World';\n<div>{{ name }}</div>";
        let out = compile_treaty_file(source, "greeting.treaty");

        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        // Emits a real ɵɵdefineComponent with a real template fn.
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        assert!(code.contains("Greeting"), "class name missing; got: {code}");
        assert!(code.contains("Greeting_Template"), "no template fn; got: {code}");
        // The interpolation binds against the component context.
        assert!(
            code.contains("\u{0275}\u{0275}textInterpolate"),
            "no interpolation instruction; got: {code}"
        );
        assert!(code.contains("ctx.name"), "did not bind ctx.name; got: {code}");
    }

    #[test]
    fn emits_full_runnable_module() {
        let source = "import { foo } from './foo';\nconst name = 'World';\nfunction greet() {}\n<div>{{ name }}</div>";
        let out = compile_treaty_file(source, "greeting.treaty");

        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        // i0 core import is always present.
        assert!(
            code.contains("import * as i0 from \"@angular/core\";"),
            "no i0 import; got: {code}"
        );
        // The user's import statement is emitted verbatim.
        assert!(
            code.contains("import { foo } from './foo';"),
            "verbatim import missing; got: {code}"
        );
        // The function component wrapper.
        assert!(
            code.contains("function Greeting() {"),
            "no function component; got: {code}"
        );
        // The body is present with imports stripped.
        assert!(code.contains("const name = 'World';"), "body const missing; got: {code}");
        assert!(code.contains("function greet() {}"), "body fn missing; got: {code}");
        assert!(
            !code.contains("function Greeting() {\nimport"),
            "import leaked into body; got: {code}"
        );
        // Top-level const/function names are returned as the bindings object.
        assert!(
            code.contains("return { name, greet };"),
            "bindings return missing; got: {code}"
        );
        // ɵfac factory.
        assert!(
            code.contains("Greeting.\u{0275}fac = function Greeting_Factory(t) { return (t || Greeting)(); };"),
            "no ɵfac; got: {code}"
        );
        // ɵcmp = the ɵɵdefineComponent expression.
        assert!(
            code.contains(&format!("Greeting.\u{0275}cmp = i0.{DEFINE}")),
            "no ɵcmp = defineComponent; got: {code}"
        );
        // export default.
        assert!(
            code.contains("export default Greeting;"),
            "no export default; got: {code}"
        );
    }

    #[test]
    fn extracts_signal_input_from_js_chunk() {
        let source = "const name = input();\n<div>{{ name() }}</div>";
        let out = compile_treaty_file(source, "greeting.treaty");

        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        // The signal input is emitted into the inputs map and marked as a signal input.
        assert!(code.contains("inputs"), "no inputs map; got: {code}");
        assert!(code.contains("name"), "inputs missing 'name'; got: {code}");
    }

    #[test]
    fn compiles_realistic_sfc_layout() {
        // The real REPL `.treaty` layout: a leading <style> CSS block, top-level JS
        // (imports + const), an HTML template region with {{ }} interpolation, and
        // trailing JS (console.log).
        let source = "<style>\n  .name { color: purple; }\n</style>\n\
import { input } from '@angular/core'\n\
const name = input('name')\n\
<div class=\"name\">{{ name() }}</div>\n\
console.log('hi')\n";
        let out = compile_treaty_file(source, "treat-example.treaty");

        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        // Valid, parseable ES module (no stray '<', no EOF errors).
        let allocator = Allocator::default();
        let module_type = SourceType::default().with_module(true);
        let parsed = JsParser::new(&allocator, code, module_type).parse();
        assert!(
            parsed.errors.is_empty(),
            "module did not parse as valid JS: {:?}\n--- code ---\n{code}",
            parsed.errors
        );

        // The defineComponent definition is present.
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");

        // The user import is emitted at MODULE TOP, before the function wrapper — never inside it.
        let import_idx = code
            .find("import { input } from '@angular/core'")
            .expect("user import missing");
        let fn_idx = code.find("function TreatExample() {").expect("no fn wrapper");
        assert!(
            import_idx < fn_idx,
            "user import is not above the function wrapper; got: {code}"
        );
        assert!(
            !code.contains("function TreatExample() {\nimport"),
            "import leaked into the function body; got: {code}"
        );

        // The JS *body* (inside the function wrapper) must contain no raw template markup.
        let body = &code[fn_idx..];
        let body = &body[..body.find("\nreturn {").unwrap_or(body.len())];
        assert!(
            !body.contains('<'),
            "raw template markup leaked into JS body; got body: {body}"
        );

        // Template markup went to render3, and the trailing JS stayed in the body.
        assert!(code.contains("ctx.name"), "template did not bind ctx.name; got: {code}");
        assert!(body.contains("console.log('hi')"), "trailing JS missing from body; got: {code}");
        assert!(code.contains("styles"), "no styles emitted; got: {code}");
    }

    #[test]
    fn auto_imports_used_component_into_dependencies() {
        // The author imports `Foo` and uses `<Foo>` in the template, with NO manual imports array.
        // `Foo` must land in the emitted `dependencies`; the unused `Bar` import must NOT.
        let source = "import { Foo } from './foo';\n\
import { Bar } from './bar';\n\
<div><Foo></Foo></div>";
        let out = compile_treaty_file(source, "host.treaty");

        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        assert!(code.contains("dependencies"), "no dependencies array; got: {code}");
        // The dependencies array references Foo (the used import).
        assert!(
            code.contains("dependencies: [Foo]") || code.contains("dependencies:[Foo]"),
            "Foo not in dependencies array; got: {code}"
        );
        // Bar is imported verbatim at module top but, being unused, is NOT in dependencies.
        assert!(
            !code.contains("[Bar]") && !code.contains("Bar]") && !code.contains("[Foo, Bar"),
            "unused import Bar leaked into dependencies; got: {code}"
        );
    }

    #[test]
    fn unused_import_not_added_to_dependencies_treaty() {
        let source = "import { Foo } from './foo';\n<div>hi</div>";
        let out = compile_treaty_file(source, "host.treaty");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        assert!(
            !code.contains("dependencies"),
            "dependencies emitted for an unused import; got: {code}"
        );
    }

    #[test]
    fn compiles_treaty_file_with_styles() {
        let source = "<div>hi</div>\n<style>.box {\n color: red;\n}</style>";
        let out = compile_treaty_file(source, "boxed.treaty");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        assert!(code.contains("styles"), "no styles emitted; got: {code}");
    }
}
