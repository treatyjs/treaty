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
//! treaty_ivy::ml_parser + template_transform -> r3_ast
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

use treaty_ivy::compile::{CompiledComponent, RealTemplateBuilder};
use treaty_ivy::output::emitter::{emit_expression, emit_expression_with_map};
use treaty_ivy::output_ast::{self as o, ParseSourceSpan};
use treaty_ivy::template::template_transform::{
    html_ast_to_render3_ast, BindingParser, Render3ParseOptions,
};
use treaty_ivy::util::{R3CompiledExpression, R3Reference};
use treaty_ivy::view::compiler::{
    compile_component_from_metadata, ChangeDetection, ChangeDetectionStrategy, ComponentTemplate,
    DeclarationListEmitMode, Deps, Lifecycle, OrderedMap, R3ComponentDeferMetadata,
    R3ComponentMetadata, R3DirectiveMetadata, R3HostMetadata, R3InputMetadata,
    R3TemplateDependencyKind, R3TemplateDependencyMetadata, StubHostBindingsBuilder,
    ViewEncapsulation,
};

use crate::plugin::{extract_server_block, rewrite_call_sites, BackendEmit, PluginRegistry, ServerFn};
use crate::source_map::redact_server_bodies_in_map;
use crate::treaty::ast::AstNode;
use crate::treaty::lexer::Lexer;
use crate::treaty::parser::Parser;
use crate::CompiledAuthoring;

/// A `<style>` chunk: its raw content plus optional preprocessor language (`lang="scss"`).
#[derive(Debug)]
struct StyleChunk {
    content: String,
    lang: Option<String>,
}

/// The source kinds extracted from a `.treaty` file.
#[derive(Debug, Default)]
struct TreatyChunks {
    javascript: Vec<String>,
    html: Vec<String>,
    styles: Vec<StyleChunk>,
    /// Raw bodies of compile-time `Macro` chunks. Captured but NOT executed (macro execution is a
    /// later phase); kept out of the JS/template/CSS output so they never break compilation.
    macros: Vec<String>,
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
            AstNode::Style { content, lang } => chunks.styles.push(StyleChunk { content, lang }),
            // A compile-time macro block: capture its raw body for a later execution phase. It is
            // NOT runtime JS/template/CSS, so it never reaches the component output.
            AstNode::Macro { content, .. } => chunks.macros.push(content),
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
/// and whether `.required` was used. Mirrors `treaty_ivy::source_compile::signal_call`.
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
/// component's signal inputs/outputs. Mirrors `treaty_ivy::source_compile` signal extraction:
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
    compile_treaty_file_inner(source, file_name, None).0
}

/// Like [`compile_treaty_file`], but ALSO emits an additive Source Map v3 JSON.
///
/// `source_name` / `source_content` describe the ORIGINAL authoring source embedded in the map's
/// `sources[0]` / `sourcesContent[0]`. `source_content` is the verbatim `.treaty` file text (the
/// caller passes the original, pre-server-strip source so the map carries the author's file); the
/// server-aware wrapper then redacts any lifted server-fn body out of `sourcesContent` for client
/// privacy. The `code` is byte-identical to [`compile_treaty_file`]; the map is additive.
pub fn compile_treaty_file_with_map(
    source: &str,
    file_name: &str,
    source_name: &str,
    source_content: &str,
) -> (CompiledComponent, Option<String>) {
    compile_treaty_file_inner(source, file_name, Some((source_name, source_content)))
}

/// Shared implementation behind [`compile_treaty_file`] and [`compile_treaty_file_with_map`].
///
/// `source` is the (server-stripped) client `.treaty` source that is actually compiled; the
/// optional `source_map` carries the original authoring `(source_name, source_content)` to embed
/// in the emitted map. When `source_map` is `None`, the plain map-free path is used.
fn compile_treaty_file_inner(
    source: &str,
    file_name: &str,
    source_map: Option<(&str, &str)>,
) -> (CompiledComponent, Option<String>) {
    let chunks = split_chunks(source);
    let class_name = to_pascal_case(file_name);

    let template_html = chunks.html.join("");
    let mut javascript = chunks.javascript.join("");

    let mut macro_errors: Vec<String> = Vec::new();

    // Execute any top-of-file macro block (server-side render-time code, like Astro frontmatter /
    // RSC) and inject the produced data into the component. The macro is TypeScript; it is run
    // through `treaty_runtime::run_macro`, which transpiles it to JS and evaluates it on the Nova
    // engine. This is the STATIC prerender path: an empty (`null`) input is passed at compile time.
    //
    // The macro's value is injected as a `const $macro = <json>;` declaration prepended to the
    // component-body JS. Because it is a top-level `const`, it is (a) visible to the rest of the
    // body and (b) collected into the component's returned bindings object by
    // `extract_wrapper_parts`, so the template can bind it directly (e.g. `{{ $macro.title }}`).
    // The macro SOURCE itself is never emitted — only its computed data is.
    if let Some(literal) = run_and_encode_macros(&chunks.macros, &mut macro_errors) {
        javascript = if javascript.trim().is_empty() {
            literal
        } else {
            format!("{literal}\n{javascript}")
        };
    }

    let mut style_errors: Vec<String> = Vec::new();

    // Build the component styles. A `<style lang="scss">` / `lang="sass"` chunk is compiled to CSS
    // with the pure-Rust `grass` sass implementation; plain CSS (no lang) passes through unchanged.
    // A sass compile error is recorded as a component error (never panics) and that chunk is
    // dropped. Match the TS pipeline: strip newlines/tabs from the resulting CSS.
    let mut styles: Vec<String> = Vec::new();
    for chunk in &chunks.styles {
        let css = match chunk.lang.as_deref() {
            Some("scss") | Some("sass") => {
                // Compressed output matches the rest of the pipeline (no superfluous whitespace),
                // so `.x { color: $c; }` emits `.x{color:red}`.
                let options =
                    grass::Options::default().style(grass::OutputStyle::Compressed);
                match grass::from_string(chunk.content.clone(), &options) {
                    Ok(css) => css,
                    Err(e) => {
                        style_errors.push(format!("sass: {e}"));
                        continue;
                    }
                }
            }
            _ => chunk.content.clone(),
        };
        let css = css.replace(['\n', '\r', '\t'], "");
        if !css.is_empty() {
            styles.push(css);
        }
    }

    // The resolved CSS chunks join into a single `styles` string for the shared render3 backend.
    // Current `.treaty` sources carry at most one style chunk, so this round-trips byte-identically.
    let styles = styles.join("");

    let (mut compiled, map) = match source_map {
        Some((source_name, source_content)) => compile_from_parts_with_directives_and_map(
            &class_name,
            &javascript,
            &template_html,
            &styles,
            file_name,
            &[],
            source_name,
            source_content,
        ),
        None => (
            compile_from_parts(&class_name, &javascript, &template_html, &styles, file_name),
            None,
        ),
    };
    // Surface macro and sass diagnostics ahead of the template diagnostics from the backend.
    if !macro_errors.is_empty() || !style_errors.is_empty() {
        let mut errors = macro_errors;
        errors.extend(style_errors);
        errors.extend(compiled.errors);
        compiled.errors = errors;
    }
    (compiled, map)
}

/// Run the captured macro block(s) and encode their combined output as a single injectable JS
/// `const` declaration, or `None` when there are no macros.
///
/// Each macro is server-side render-time TypeScript (the top-of-file fenced block). It is executed
/// via [`treaty_runtime::run_macro`] with an empty compile-time input ([`serde_json::Value::Null`])
/// — the static-prerender path. The produced JSON value is injected as `const $macro = <json>;`
/// (or `$macro0` / `$macro1` / … when a file carries more than one macro block) so the component
/// body and template can reference the data. A macro that fails to transpile or throws records its
/// message in `errors` and contributes no binding; the rest of the component still compiles.
///
/// Returns the declaration text to prepend to the component-body JS, or `None` when no macro
/// produced an injectable value.
fn run_and_encode_macros(macros: &[String], errors: &mut Vec<String>) -> Option<String> {
    if macros.is_empty() {
        return None;
    }

    // The static-prerender input. A `.treaty` macro reads request data via `input`; at build time
    // there is no request, so an empty input is supplied. (The dynamic-prerender path re-runs the
    // same macro per request with real input — handled by the runtime layer, not the compiler.)
    let input = serde_json::Value::Null;

    let mut decls: Vec<String> = Vec::new();
    for (idx, macro_src) in macros.iter().enumerate() {
        // A single macro binds plain `$macro`; multiple blocks are disambiguated by index.
        let name = if macros.len() == 1 {
            "$macro".to_string()
        } else {
            format!("$macro{idx}")
        };
        match treaty_runtime::run_macro(macro_src, &input) {
            Ok(output) => {
                // `serde_json::to_string` of any JSON value is a valid JS expression literal, so
                // the splice is injection-safe.
                let literal = serde_json::to_string(output.value()).unwrap_or_else(|_| "null".to_string());
                decls.push(format!("const {name} = {literal};"));
            }
            Err(e) => errors.push(format!("macro: {e}")),
        }
    }

    if decls.is_empty() {
        None
    } else {
        Some(decls.join("\n"))
    }
}

/// Compile a component from already-split parts into the `ɵɵdefineComponent` ES module.
///
/// This is the render3-backend half of [`compile_treaty_file`], factored out so any authoring
/// front-end (the `.treaty` lexer, a JSX transpiler, …) can lower its source to a component class
/// name, a JavaScript body, an Angular template HTML string, and resolved CSS, then reuse the same
/// Ivy codegen path. `styles` is the final CSS (any preprocessor compilation already done); it is
/// emitted as the component's single `styles` entry when non-empty.
///
/// Pipeline (identical to the back half of the `.treaty` path):
/// ```text
/// treaty_ivy::ml_parser::parse(template_html)        -> HTML AST
/// html_ast_to_render3_ast                          -> r3 AST
/// resolve_template_dependencies(imports, template) -> auto dependencies
/// compile_component_from_metadata                  -> ɵɵdefineComponent
/// build_module                                     -> runnable ES module
/// ```
pub fn compile_from_parts(
    class_name: &str,
    javascript: &str,
    template_html: &str,
    styles: &str,
    file_name: &str,
) -> CompiledComponent {
    compile_from_parts_with_directives(class_name, javascript, template_html, styles, file_name, &[])
}

/// Like [`compile_from_parts`], but with an additional set of directive class names that the
/// front-end resolved by selectorless auto-import (e.g. the JSX directive syntaxes lowered in
/// [`crate::jsx::directives`]).
///
/// These names are merged into the component's `dependencies` in addition to the component/directive
/// references the render3 binder discovers in the template. A directive applied via an attribute
/// (`use:tooltip`, `<input Autofocus/>`, `*highlight`) lowers to plain attribute markup that the
/// instruction parser accepts but that the selectorless binder cannot itself attribute back to a
/// class — so the class names are threaded explicitly here. Each name still only becomes a
/// dependency when it was actually applied in the template (the front-end only collects applied
/// directives), preserving the "unused imports are not emitted" contract. Names are deduplicated and
/// appended after the binder-resolved dependencies, in first-seen order.
pub fn compile_from_parts_with_directives(
    class_name: &str,
    javascript: &str,
    template_html: &str,
    styles: &str,
    file_name: &str,
    extra_directives: &[String],
) -> CompiledComponent {
    compile_from_parts_inner(
        class_name,
        javascript,
        template_html,
        styles,
        file_name,
        extra_directives,
        None,
    )
    .0
}

/// Like [`compile_from_parts_with_directives`], but ALSO emits an additive Source Map v3 JSON
/// alongside the compiled module.
///
/// The map is produced through render3's source-map emitter
/// ([`treaty_ivy::output::emitter::emit_expression_with_map`]) for the lowered `ɵɵdefineComponent`
/// expression, embedding `source_content` as the map's `sourcesContent[0]` and `source_name` as
/// its `sources[0]`. `source_content` is the ORIGINAL authoring source text (the verbatim
/// `.treaty` / `.tjsx` file), so the client map carries the author's source — exactly as the base
/// `@Component` `.ts` path does via [`treaty_ivy::source_compile::compile_component_source_with_map`].
///
/// The returned `code` is byte-identical to [`compile_from_parts_with_directives`] (the map is
/// additive and never reprints the module). The second tuple element is `None` only when the
/// emitter produced an empty map.
pub fn compile_from_parts_with_directives_and_map(
    class_name: &str,
    javascript: &str,
    template_html: &str,
    styles: &str,
    file_name: &str,
    extra_directives: &[String],
    source_name: &str,
    source_content: &str,
) -> (CompiledComponent, Option<String>) {
    compile_from_parts_inner(
        class_name,
        javascript,
        template_html,
        styles,
        file_name,
        extra_directives,
        Some((source_name, source_content)),
    )
}

/// Shared implementation behind [`compile_from_parts_with_directives`] and
/// [`compile_from_parts_with_directives_and_map`].
///
/// When `source_map` is `Some((source_name, source_content))` the lowered `ɵɵdefineComponent`
/// expression is emitted through render3's `emit_expression_with_map`, threading out the additive
/// v3 map (with `source_content` embedded as `sourcesContent[0]`). When `None`, the plain
/// (map-free) `emit_expression` path is used and the second tuple element is `None`. Both paths
/// share the identical lowering + module assembly, so `code` is byte-identical between them.
fn compile_from_parts_inner(
    class_name: &str,
    javascript: &str,
    template_html: &str,
    styles: &str,
    file_name: &str,
    extra_directives: &[String],
    source_map: Option<(&str, &str)>,
) -> (CompiledComponent, Option<String>) {
    let mut errors: Vec<String> = Vec::new();

    let style_list: Vec<String> = if styles.is_empty() {
        Vec::new()
    } else {
        vec![styles.to_string()]
    };

    // 0. Extract signal inputs/outputs from the component-body JS chunk.
    let mut inputs: OrderedMap<String, R3InputMetadata> = OrderedMap::new();
    let mut outputs: OrderedMap<String, String> = OrderedMap::new();
    extract_io(javascript, &mut inputs, &mut outputs);
    let is_signal = inputs.iter().any(|(_, m)| m.is_signal);

    // 1. Template HTML -> HTML AST.
    let parse_result = treaty_ivy::ml_parser::parse(template_html, "template.html");
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
    let candidates = collect_imported_names(javascript);
    let selectorless_nodes = treaty_ivy::compile::parse_template_selectorless(template_html);
    let mut declarations =
        treaty_ivy::compile::resolve_template_dependencies(&candidates, &selectorless_nodes);

    // Append the front-end-resolved directive dependencies (the JSX directive syntaxes). These were
    // applied as plain attribute markup, which the selectorless binder cannot attribute back to a
    // class, so they are merged in here. Skip any already present (e.g. a directive also written as
    // a `<Foo>` selectorless tag) to keep the dependency list unique.
    for name in extra_directives {
        let already = declarations.iter().any(|d| {
            matches!(&d.ty.kind, treaty_ivy::output_ast::ExprKind::ReadVar { name: n } if n == name)
        });
        if !already {
            declarations.push(R3TemplateDependencyMetadata {
                kind: R3TemplateDependencyKind::Directive,
                ty: o::variable(name, None),
            });
        }
    }
    let has_directive_dependencies = !declarations.is_empty();

    // 3. Standalone, selectorless component metadata.
    let base = R3DirectiveMetadata {
        name: class_name.to_string(),
        ty: class_ref(class_name),
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
        styles: style_list,
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

    // Emit the lowered definition expression. When a source map was requested, route through
    // render3's `emit_expression_with_map` (byte-identical code, plus an additive v3 map embedding
    // the original authoring source as `sourcesContent`); otherwise use the plain emitter.
    let (cmp_expression, map) = match source_map {
        Some((source_name, source_content)) => {
            // The map's `file` is the generated artifact (the `.js` sibling of the authoring file);
            // `source_name` / `source_content` describe the original authoring source.
            let generated_name = generated_name_for(file_name);
            let (code, map_json) = emit_expression_with_map(
                &compiled.expression,
                &generated_name,
                source_name,
                source_content,
            );
            (code, map_or_none(map_json))
        }
        None => (emit_expression(&compiled.expression), None),
    };
    let code = build_module(class_name, javascript, &cmp_expression);
    (CompiledComponent { code, errors }, map)
}

/// Derive the generated-artifact name (`*.js`) for the map's `file` from the authoring file name,
/// preserving directory components. `"src/app.treaty"` -> `"src/app.js"`, `"counter.tjsx"` ->
/// `"counter.js"`; a name with no extension gains a `.js` suffix.
fn generated_name_for(file_name: &str) -> String {
    match file_name.rsplit_once('.') {
        Some((stem, _ext)) if !stem.is_empty() => format!("{stem}.js"),
        _ => format!("{file_name}.js"),
    }
}

/// Normalize render3's empty-string "no map" sentinel into `None`. render3 returns an empty `map`
/// when emission produced nothing mappable; any non-empty value is a real v3 JSON document.
fn map_or_none(map: String) -> Option<String> {
    if map.trim().is_empty() {
        None
    } else {
        Some(map)
    }
}

/// Compile a `.treaty` SFC, handling a top-level `server { … }` block via the [`plugin`] system.
///
/// This is the server-aware wrapper around [`compile_treaty_file`]:
///   1. [`extract_server_block`] lifts any `server { … }` block out of the source.
///   2. The cleaned `client_source` compiles through [`compile_treaty_file`].
///   3. When server functions were present, the active backend plugin — the
///      [`PluginRegistry`](crate::plugin::PluginRegistry) default (axum + typesafe resource HTTP
///      client) — emits a server module + per-fn client bindings, and [`rewrite_call_sites`]
///      rewrites free references to each server fn in the client source to its plugin-provided
///      binding. The backend is never hardcoded; opting into another (e.g. `elysia-eden`) is a
///      registry-name lookup via [`compile_treaty_authoring_with`].
///
/// When no `server { … }` block is present the source compiles unchanged and `server_module` is
/// `None`.
///
/// [`plugin`]: crate::plugin
pub fn compile_treaty_authoring(source: &str, file_name: &str) -> CompiledAuthoring {
    let registry = PluginRegistry::with_defaults();
    let plugin = registry
        .default_plugin()
        .expect("registry seeded with a default backend plugin");
    compile_treaty_authoring_with(source, file_name, |fns| plugin.emit(fns))
}

/// Like [`compile_treaty_authoring`], but emits server functions through `emit` (the caller's chosen
/// backend) rather than the registry default. Used to opt into a non-default backend such as
/// `elysia-eden` (`PluginRegistry::get("elysia-eden")`).
pub fn compile_treaty_authoring_with(
    source: &str,
    file_name: &str,
    emit: impl FnOnce(&[ServerFn]) -> BackendEmit,
) -> CompiledAuthoring {
    let extraction = extract_server_block(source);

    // The map embeds the ORIGINAL authoring file text as `sourcesContent`, named by `file_name`,
    // mirroring the base `@Component` `.ts` path. The original `source` (pre-server-strip) is used
    // so the author's verbatim file is the map content; any lifted server-fn body is then redacted
    // out of that content below.
    if extraction.server_fns.is_empty() {
        let (compiled, map) =
            compile_treaty_file_with_map(&extraction.client_source, file_name, file_name, source);
        return CompiledAuthoring {
            code: compiled.code,
            server_module: None,
            errors: compiled.errors,
            map,
        };
    }

    // Rewrite free references to each server fn into its plugin-provided client binding *in the
    // client source*, before compilation, so the swap lands on the author's free `save(...)`
    // identifier rather than a lowered `ctx.save(...)` member access in the emitted output. The
    // binding text comes straight from the active plugin's `client_bindings` map — no backend path
    // is hardcoded here.
    let emit = emit(&extraction.server_fns);
    let client_source = rewrite_call_sites(&extraction.client_source, &emit.client_bindings);
    let (compiled, map) =
        compile_treaty_file_with_map(&client_source, file_name, file_name, source);

    // CLIENT PRIVACY: the map embeds the original authoring source as `sourcesContent`, which still
    // carries the verbatim `server { … }` block. Redact each lifted server-fn body out of the map's
    // content (blanked to position-preserving whitespace) so the server source never reaches the
    // client map — the same guarantee the base `@Component` `.ts` path provides.
    let server_bodies: Vec<String> =
        extraction.server_fns.iter().map(|f| f.source.clone()).collect();
    let map = map.map(|m| redact_server_bodies_in_map(&m, &server_bodies));

    CompiledAuthoring {
        code: compiled.code,
        server_module: Some(emit.server_module),
        errors: compiled.errors,
        map,
    }
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
    fn compiles_treaty_without_template_wrapper_interleaving_ts_and_html() {
        // FIX #2: NO <template> wrapper. TS, then an HTML element, then more TS, then more HTML —
        // the HTML regions become the component template; the TS stays in the component body.
        let source = "const greeting = 'Hi';\n\
<header>{{ greeting }}</header>\n\
const footerText = 'Bye';\n\
<footer>{{ footerText }}</footer>\n";
        let out = compile_treaty_file(source, "page.treaty");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        // Both HTML elements became part of the template (header + footer rendered as DOM).
        assert!(code.contains("\"header\""), "header element missing from template; got: {code}");
        assert!(code.contains("\"footer\""), "footer element missing from template; got: {code}");
        // Both interpolations bind against the component context.
        assert!(code.contains("ctx.greeting"), "first interpolation not bound; got: {code}");
        assert!(code.contains("ctx.footerText"), "second interpolation not bound; got: {code}");
        // The TS bodies are kept (returned bindings include both consts).
        assert!(
            code.contains("const greeting = 'Hi';"),
            "leading TS body missing; got: {code}"
        );
        assert!(
            code.contains("const footerText = 'Bye';"),
            "interleaved TS body missing; got: {code}"
        );
    }

    #[test]
    fn compiles_treaty_with_optional_template_wrapper() {
        // FIX #2: a file that DID use <template> still works — the wrapper is unwrapped and only its
        // inner markup becomes the template (no literal <template> element in the output).
        let source = "const name = 'World';\n\
<template>\n  <div>{{ name }}</div>\n</template>\n";
        let out = compile_treaty_file(source, "wrapped.treaty");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        assert!(code.contains("ctx.name"), "interpolation not bound; got: {code}");
        // The wrapper itself must NOT appear as a rendered element.
        assert!(
            !code.contains("\"template\""),
            "the <template> wrapper leaked into the rendered template; got: {code}"
        );
    }

    #[test]
    fn compiles_treaty_with_non_ascii_without_panicking() {
        // FIX #1: non-ASCII in the macro, body, template text, attribute, interpolation and <style>
        // must compile without a mid-UTF-8-char byte-slice panic, and the text must survive.
        let source = "```\nreturn { saludo: 'Hola caf\u{00e9} \u{1F680}' };\n```\n\
const titulo = 'na\u{00ef}ve \u{1F600}';\n\
<section title=\"caf\u{00e9} \u{1F4A1}\">na\u{00ef}ve \u{1F680} {{ $macro.saludo }} \u{2013} {{ titulo }}</section>\n\
<style>/* caf\u{00e9} \u{1F680} */ .a { content: \"\u{00e9}\"; }</style>\n";
        let out = compile_treaty_file(source, "intl.treaty");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        // The macro ran and its non-ASCII data was injected.
        assert!(
            code.contains("Hola caf\u{00e9} \u{1F680}"),
            "macro non-ASCII data missing; got: {code}"
        );
        // The non-ASCII attribute survived into the template.
        assert!(
            code.contains("caf\u{00e9} \u{1F4A1}"),
            "non-ASCII attribute lost; got: {code}"
        );
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

    #[test]
    fn compiles_scss_style_to_css() {
        // `lang="scss"` styles are compiled by grass before being added to the component styles.
        // The SCSS variable `$c` resolves to `red`, so the emitted CSS contains `color:red`.
        let source = "<div>hi</div>\n<style lang=\"scss\"> $c: red; .x { color: $c; }</style>";
        let out = compile_treaty_file(source, "themed.treaty");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        assert!(code.contains("styles"), "no styles emitted; got: {code}");
        // Compiled SCSS: variable resolved, newlines stripped → `color:red`.
        assert!(
            code.contains("color:red"),
            "scss did not compile to `color:red`; got: {code}"
        );
    }

    #[test]
    fn treaty_server_block_extracts_route_and_rewrites_call_through_default_axum() {
        // A `.treaty` SFC with a server block declaring `save`, a body that calls `save`, and a
        // template. The DEFAULT backend (axum + typesafe resource HTTP client) is applied via the
        // PluginRegistry: the client routes the call through the resource binding (not the original
        // fn, not an eden path) and a Rust/axum server module carries the POST route for `save`.
        let source = "import { User } from './user';\n\
server {\n\
  async function save(user: User) { return db.insert(user); }\n\
}\n\
function onClick(user) { return save(user); }\n\
<div>{{ onClick }}</div>\n";

        let out = compile_treaty_authoring(source, "form.treaty");

        // Server module generated as a Rust/axum service with the POST route for `save`.
        let server_module = out.server_module.expect("expected a server module");
        assert!(
            server_module.contains("\"/__server/save\""),
            "no save route in axum server module; got: {server_module}"
        );
        assert!(
            server_module.contains("pub fn build_router() -> Router"),
            "no axum router builder in server module; got: {server_module}"
        );
        assert!(
            !server_module.contains("new Elysia()"),
            "default path should not emit an Elysia app; got: {server_module}"
        );

        // The compiled client routes the call through the axum typesafe resource client binding.
        assert!(
            out.code.contains("edenHttpResource") && out.code.contains("'/__server/save'"),
            "call not rewritten to axum resource client; got: {}",
            out.code
        );
        assert!(
            !out.code.contains("client.__server.save.post"),
            "default path leaked the eden binding; got: {}",
            out.code
        );
        // The server fn body never reaches the client bundle.
        assert!(
            !out.code.contains("db.insert"),
            "server body leaked into client; got: {}",
            out.code
        );
    }

    #[test]
    fn treaty_server_block_opt_in_elysia_eden_binding() {
        // Opting into the `elysia-eden` backend by registry name yields the Eden client binding and
        // an Elysia server module instead of the default axum output.
        let source = "import { User } from './user';\n\
server {\n\
  async function save(user: User) { return db.insert(user); }\n\
}\n\
function onClick(user) { return save(user); }\n\
<div>{{ onClick }}</div>\n";

        let registry = PluginRegistry::with_defaults();
        let elysia = registry.get("elysia-eden").expect("elysia-eden registered");
        let out = compile_treaty_authoring_with(source, "form.treaty", |fns| elysia.emit(fns));

        let server_module = out.server_module.expect("expected a server module");
        assert!(
            server_module.contains(".post('/__server/save'"),
            "no save route in Elysia server module; got: {server_module}"
        );
        assert!(
            server_module.contains("new Elysia()"),
            "no Elysia app in opt-in server module; got: {server_module}"
        );

        // The compiled client routes the call through the Eden client.
        assert!(
            out.code.contains("client.__server.save.post"),
            "call not rewritten to eden client; got: {}",
            out.code
        );
        assert!(
            !out.code.contains("db.insert"),
            "server body leaked into client; got: {}",
            out.code
        );
    }

    #[test]
    fn treaty_in_function_server_block_extracts_and_rewrites() {
        // A `server { … }` block nested inside a function body (brace depth > 0) in a `.treaty` SFC
        // must still be lifted and its call site rewritten — exercising the depth-agnostic block scan.
        let source = "import { User } from './user';\n\
function setup(user) {\n\
  server {\n\
    async function save(u: User) { return db.insert(u); }\n\
  }\n\
  return save(user);\n\
}\n\
<div>{{ setup }}</div>\n";

        let out = compile_treaty_authoring(source, "form.treaty");

        let server_module = out.server_module.expect("expected a server module for nested block");
        assert!(
            server_module.contains("\"/__server/save\""),
            "no save route in axum server module; got: {server_module}"
        );
        assert!(
            out.code.contains("'/__server/save'"),
            "call not rewritten to client binding; got: {}",
            out.code
        );
        assert!(
            !out.code.contains("db.insert"),
            "server body leaked into client; got: {}",
            out.code
        );
    }

    #[test]
    fn treaty_without_server_block_carries_a_v3_map() {
        // A plain `.treaty` SFC (no server block) compiles WITH an additive v3 map whose
        // `sourcesContent` embeds the original authoring source.
        let source = "const name = 'World';\n<div>{{ name }}</div>";
        let out = compile_treaty_authoring(source, "greeting.treaty");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);

        let map = out.map.expect("expected a source map for a `.treaty` SFC");
        let value: serde_json::Value =
            serde_json::from_str(&map).expect("map should be valid JSON");
        assert_eq!(value["version"], serde_json::json!(3), "not a v3 map: {map}");
        // The original authoring source is embedded as `sourcesContent` and named by the file.
        assert_eq!(value["sources"][0], serde_json::json!("greeting.treaty"), "wrong source name");
        let contents = value["sourcesContent"].as_array().expect("sourcesContent array");
        assert!(
            contents.iter().any(|c| c.as_str() == Some(source)),
            "authoring source not embedded as sourcesContent; got: {map}"
        );
    }

    #[test]
    fn treaty_server_block_body_is_absent_from_client_map() {
        // CLIENT PRIVACY: a `.treaty` SFC with an inline server fn must compile to a v3 map whose
        // `sourcesContent` does NOT contain the server fn body text.
        let source = "import { User } from './user';\n\
server {\n\
  async function save(user: User) { return db.insert(user); }\n\
}\n\
function onClick(user) { return save(user); }\n\
<div>{{ onClick }}</div>\n";

        let out = compile_treaty_authoring(source, "form.treaty");
        assert!(out.server_module.is_some(), "expected a server module");

        let map = out.map.expect("expected a source map for a server-block `.treaty`");
        let value: serde_json::Value =
            serde_json::from_str(&map).expect("map should be valid JSON");
        assert_eq!(value["version"], serde_json::json!(3), "not a v3 map: {map}");

        let contents = value["sourcesContent"].as_array().expect("sourcesContent array");
        for c in contents {
            let text = c.as_str().unwrap_or("");
            assert!(!text.contains("db.insert"), "server body leaked into map content: {text}");
            assert!(
                !text.contains("async function save"),
                "server signature leaked into map content: {text}"
            );
        }
        // The redaction preserves the surrounding client text and the file name.
        assert_eq!(value["sources"][0], serde_json::json!("form.treaty"), "wrong source name");
        let joined: String = contents
            .iter()
            .filter_map(|c| c.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("function onClick"), "client body lost from map: {joined}");
    }

    #[test]
    fn greeter_shaped_sfc_lowers_to_valid_client_module_and_server_module() {
        // The greeter.treaty shape that previously emitted malformed output: a `.treaty` SFC with
        // module imports WITHOUT trailing semicolons (TS-by-default), a `//`-comment directly above
        // a `server { … }` block, reactive bindings, and a handler that calls the server fn. The
        // result must be a VALID ES module — imports hoisted to module scope (never inside the fn
        // wrapper), no raw `server {` text in the client, bindings collected into the returned
        // object — plus a populated server module carrying the `greet` body.
        let source = "import { signal, computed } from '@angular/core'\n\
import { type Greeting } from './greeting.types'\n\
\n\
const name = signal('Ada')\n\
const greeting = signal<Greeting | null>(null)\n\
const headline = computed(() => greeting()?.text ?? `Say hello to ${name()}`)\n\
\n\
// Every function inside this block is server-only: extracted to a sibling module.\n\
server {\n\
\tasync function greet(who: string): Promise<Greeting> {\n\
\t\tconst text = `Hello, ${who}!`\n\
\t\treturn { text, at: Date.now() }\n\
\t}\n\
}\n\
\n\
async function sayHello(): Promise<void> {\n\
\tgreeting.set(await greet(name().trim() || 'world'))\n\
}\n\
\n\
<section class=\"greeter\">\n\
  <h2>{{ headline() }}</h2>\n\
  <button (click)=\"sayHello()\">Greet</button>\n\
</section>\n";

        let out = compile_treaty_authoring(source, "greeter.treaty");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        // (b) The raw `server {` block text must NOT survive into the client module.
        assert!(
            !code.contains("server {"),
            "raw server block leaked into client; got: {code}"
        );
        // The server fn body must NOT reach the client bundle.
        assert!(
            !code.contains("Date.now()") && !code.contains("Hello, ${who}"),
            "server body leaked into client; got: {code}"
        );

        // The whole client module RE-PARSES as a valid ES module via oxc. The emitted body keeps
        // the author's TypeScript (e.g. `signal<Greeting | null>(...)`), so parse as a TS module.
        let allocator = Allocator::default();
        let module_type = SourceType::default().with_module(true).with_typescript(true);
        let parsed = JsParser::new(&allocator, code, module_type).parse();
        assert!(
            parsed.errors.is_empty(),
            "client module did not parse as valid TS module: {:?}\n--- code ---\n{code}",
            parsed.errors
        );

        // (a) Imports are HOISTED to module scope, above the function wrapper — never inside it.
        let fn_idx = code.find("function Greeter() {").expect("no fn wrapper");
        let core_import_idx = code
            .find("import { signal, computed } from '@angular/core'")
            .expect("user core import missing");
        let types_import_idx = code
            .find("import { type Greeting } from './greeting.types'")
            .expect("types import missing");
        assert!(
            core_import_idx < fn_idx && types_import_idx < fn_idx,
            "an import is not above the function wrapper; got: {code}"
        );
        // No `import` statement appears anywhere inside the function body.
        let body_start = fn_idx;
        let body = &code[body_start..];
        let body = &body[..body.find("\nreturn {").unwrap_or(body.len())];
        assert!(
            !body.contains("import "),
            "import leaked into the function body; got body: {body}"
        );

        // (c) The reactive bindings are collected into the returned object (non-empty return).
        for binding in ["name", "greeting", "headline", "sayHello"] {
            assert!(
                code.contains(&format!("return {{ ")) && code.contains(binding),
                "binding `{binding}` not collected into the returned object; got: {code}"
            );
        }
        // The return object is not empty.
        assert!(
            !code.contains("return {  };") && !code.contains("return { };"),
            "bindings return is empty; got: {code}"
        );

        // A server module IS produced and carries the `greet` route + body.
        let server_module = out.server_module.expect("expected a server module for greet");
        assert!(
            server_module.contains("\"/__server/greet\""),
            "no greet route in server module; got: {server_module}"
        );

        // The component definition is otherwise intact.
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        assert!(code.contains("ctx.headline"), "template did not bind headline; got: {code}");
    }

    #[test]
    fn treaty_without_server_block_has_no_server_module() {
        let source = "const name = 'World';\n<div>{{ name }}</div>";
        let out = compile_treaty_authoring(source, "greeting.treaty");
        assert!(out.server_module.is_none(), "unexpected server module");
        assert!(out.code.contains(DEFINE), "no defineComponent; got: {}", out.code);
    }

    #[test]
    fn executes_macro_block_without_breaking_compilation() {
        // A top-level fenced macro block is EXECUTED (its computed value injected); the macro
        // SOURCE itself must not leak into the JS body or the template. This statement-only macro
        // produces no value, so it injects `const $macro = null;` and the component still compiles.
        let source = "```\nconst x = 1;\n```\nconst name = 'World';\n<div>{{ name }}</div>";
        let out = compile_treaty_file(source, "withmacro.treaty");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        // Component still compiles.
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        assert!(code.contains("function Withmacro() {"), "no fn wrapper; got: {code}");
        // The macro SOURCE is NOT emitted into the module — only its computed value is.
        assert!(
            !code.contains("const x = 1;"),
            "macro source leaked into output; got: {code}"
        );
        // The macro injected its (empty) result as `$macro`.
        assert!(code.contains("const $macro = null;"), "macro value not injected; got: {code}");
        // The real component body and template are intact.
        assert!(code.contains("const name = 'World';"), "body const missing; got: {code}");
        assert!(code.contains("ctx.name"), "template did not bind ctx.name; got: {code}");
    }

    #[test]
    fn macro_data_is_injected_and_bindable_in_template() {
        // A macro that produces data: its computed value is injected as `const $macro = {...};`,
        // exposed as a component binding, and bindable in the template — while the macro SOURCE
        // (the `title`/`count` computation) never reaches the emitted module.
        let source = "```\n\
const title: string = 'Hello from macro';\n\
const count: number = 2 * 21;\n\
return { title, count };\n\
```\n\
<h1>{{ $macro.title }}</h1>\n";
        let out = compile_treaty_file(source, "page.treaty");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        // The component compiled.
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");

        // The macro's computed value is injected as a JSON object literal `const`. The macro ran
        // the arithmetic and string ops, so the literal carries the RESULTS, not the source. (Key
        // order in the JSON encoding is not significant, so each field is checked individually.)
        assert!(code.contains("const $macro = {"), "macro data literal not injected; got: {code}");
        assert!(
            code.contains("\"title\":\"Hello from macro\""),
            "macro `title` result not injected; got: {code}"
        );
        assert!(
            code.contains("\"count\":42"),
            "macro `count` result not injected; got: {code}"
        );

        // The macro SOURCE never leaks (no `return { title, count }`, no `2 * 21`).
        assert!(!code.contains("2 * 21"), "macro source leaked; got: {code}");
        assert!(
            !code.contains("return { title, count }"),
            "macro source leaked; got: {code}"
        );
        assert!(
            !code.contains(": string") && !code.contains(": number"),
            "macro TS annotations leaked; got: {code}"
        );

        // `$macro` is collected into the component's returned bindings, so the template context
        // sees it.
        assert!(
            code.contains("$macro"),
            "macro binding not returned to component context; got: {code}"
        );
        // The template binds the macro data against the component context.
        assert!(
            code.contains("ctx.$macro") || code.contains("ctx.$macro.title"),
            "template did not bind macro data; got: {code}"
        );

        // The emitted module is valid, parseable JS.
        let allocator = Allocator::default();
        let module_type = SourceType::default().with_module(true);
        let parsed = JsParser::new(&allocator, code, module_type).parse();
        assert!(
            parsed.errors.is_empty(),
            "module did not parse as valid JS: {:?}\n--- code ---\n{code}",
            parsed.errors
        );
    }
}
