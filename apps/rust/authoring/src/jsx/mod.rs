//! JSX authoring front-end: compile a `.tsx` / `.tjsx` component source into an Angular Ivy
//! component, reusing the shared render3 backend ([`crate::sfc::compile_from_parts`]).
//!
//! Strategy (deliberately NOT a new template IR): a JSX component is a function that returns a JSX
//! tree. We parse the source with OXC (JSX + TypeScript), locate the component function, lower its
//! returned JSX into an Angular template HTML string ([`template`]), strip that return out of the
//! JavaScript body, assemble a component-body JS chunk, lift any `server { … }` block
//! ([`crate::plugin::extract_server_block`]), then hand the parts to the same Ivy codegen the
//! `.treaty` path uses.
//!
//! Full element/attribute lowering lives in [`template`] (and the name-level policy in
//! [`directives`]): HTML/component tags, fragments, text + `{expr}` interpolation, property
//! bindings, `class`/`style` string/object/array forms, `onX` event handlers, and `{...spread}`.
//! The [`control_flow`] and [`signals`] submodules (JSX `&&`/`.map`/ternary → `@if`/`@for`, and
//! hook-style signal lowering) are filled in by later phases.

pub mod angular_blocks;
pub mod control_flow;
pub mod directives;
pub mod signals;
pub mod template;
pub mod ts_erase;

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    ArrowFunctionExpression, Declaration, ExportDefaultDeclarationKind, Expression, Function,
    Statement,
};
use oxc_parser::Parser as JsParser;
use oxc_span::SourceType;

use crate::plugin::{extract_server_block, rewrite_call_sites, BackendPlugin, ElysiaEdenPlugin};
use crate::sfc::compile_from_parts_with_directives_and_map;
use crate::source_map::redact_server_bodies_in_map;
use crate::CompiledAuthoring;

/// PascalCase the stem of a file name, reused as the component class name. Mirrors the `.treaty`
/// derivation so JSX and `.treaty` components name themselves the same way.
fn to_pascal_case(file_name: &str) -> String {
    let base = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name);
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
            new_word = true;
        }
    }

    if out.is_empty() {
        "JsxComponent".to_string()
    } else {
        out
    }
}

/// Collect the imported binding names from an already-parsed program — the selectorless auto-import
/// candidate set (every default / namespace / named import local name). Parsing here is the
/// JSX-aware parse the front-end already performed, so JSX in the body does not interfere.
fn collect_import_names(body: &[Statement]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for stmt in body {
        let Statement::ImportDeclaration(import) = stmt else {
            continue;
        };
        let Some(specifiers) = &import.specifiers else {
            continue;
        };
        for spec in specifiers {
            use oxc_ast::ast::ImportDeclarationSpecifier::*;
            match spec {
                ImportSpecifier(s) => names.push(s.local.name.to_string()),
                ImportDefaultSpecifier(s) => names.push(s.local.name.to_string()),
                ImportNamespaceSpecifier(s) => names.push(s.local.name.to_string()),
            }
        }
    }
    names
}

/// The located component and its lowered template.
///
/// The JSX front-end deliberately does NOT splice the author's source verbatim into the module: the
/// author writes `export default function App() { …body…; return <JSX/> }` (or a named-function /
/// arrow form), and the shared backend ([`crate::sfc::build_module`]) re-wraps a *flat* component
/// body inside its own `function {Class}() { … return { bindings }; }` envelope. Leaving the
/// author's enclosing `export default function`/`const X = () =>` in the body produces an illegal
/// nested export and stray braces (the malformed-module bug). So we lower the component to its
/// FLAT inner body here — the author's function/arrow envelope and its JSX `return` are dropped,
/// exactly as the `.treaty` path hands the backend a flat declaration list.
struct LoweredComponent {
    /// The Angular template HTML lowered from the component's returned JSX.
    template_html: String,
    /// The byte span of the WHOLE component declaration statement in the (preprocessed) source —
    /// the `export default function App(){…}` / `function App(){…}` / `const App = () => …;` /
    /// `export default App;` etc. This entire region is removed from the module-level body and
    /// replaced by [`Self::component_body`], so no `export default`/function envelope survives.
    declaration_span: (usize, usize),
    /// The flattened component-body JavaScript: the statements *inside* the component function/arrow
    /// body, with the JSX `return`/expression-body sliced out (the lowered template is the single
    /// source of truth for markup). This is module-top-level shaped — the same flat form the
    /// `.treaty` path produces — so the shared backend's `build_module` and the signals pass both
    /// treat its declarations as component state.
    component_body: String,
    /// The author's name for the component, when it has one (a named `function App` or a
    /// `const App = …`). Used to drop a sibling bare `export default App;` statement that re-exports
    /// the component by name — the backend emits its own `export default {Class};`. A bare default
    /// export (`export default function () {}` / `export default () => …`) has no name and is `None`.
    name: Option<String>,
}

/// Compile a JSX component source into an Angular Ivy component.
///
/// `file_name` derives the component class name (PascalCase of the stem). The component is
/// standalone and selectorless, exactly like the `.treaty` path. A `server { … }` block is lifted
/// and routed through the reference Elysia/Eden backend, mirroring `sfc::compile_treaty_authoring`.
pub fn compile(source: &str, file_name: &str) -> CompiledAuthoring {
    // 1. Lift any server block first, so server-only code never reaches JSX parsing/lowering.
    let extraction = extract_server_block(source);

    // 1b. Lower Angular control-flow blocks written directly inside JSX (`@if`/`@for`/`@switch`)
    //     out of the source *before* OXC parses it: their `{ … }` bodies are not parseable JSX
    //     expression containers (a multi-element / text / nested-block body makes the whole TSX
    //     parse fail). Each block is replaced by a `<treaty-cf-N />` placeholder element (valid JSX)
    //     and its lowered Angular HTML stashed; the placeholders are restored into the template
    //     after JSX lowering (step 3b). All later byte-offset work uses this preprocessed source.
    let preprocessed = angular_blocks::preprocess(&extraction.client_source);
    let client_source = preprocessed.source;
    let cf_blocks = preprocessed.blocks;

    // 2. Parse the client source as TSX (TypeScript + JSX).
    let allocator = Allocator::default();
    let source_type = SourceType::tsx();
    let ret = JsParser::new(&allocator, &client_source, source_type).parse();

    let mut errors: Vec<String> = ret.errors.iter().map(|e| e.to_string()).collect();

    // 3. Locate the component function and lower its returned JSX to a template HTML string. Seed
    //    the directive lowering pass with the imported class names first, so the bare-lowercase
    //    Angular-attribute directive form (`tooltip={x}` matching an imported `Tooltip`) resolves,
    //    and so every applied directive is collected for selectorless auto-import.
    let class_name = to_pascal_case(file_name);
    // Candidates come from the JSX-aware parse already in hand (`ret`): re-parsing the client source
    // as TS-only would choke on its JSX and yield no imports, so collect imports from `ret.program`.
    let directive_candidates = collect_import_names(&ret.program.body);
    directives::begin_pass(&directive_candidates);
    let lowered = find_component(&ret.program.body, &client_source);
    let directive_refs = directives::take_directive_references();

    let (template_html, javascript) = match lowered {
        Some(component) => {
            // Assemble a FLAT module-top-level body: every top-level statement EXCEPT the component
            // declaration is kept verbatim (this hoists the author's `import`s and sibling helpers),
            // and the component declaration is replaced by its flattened inner body (the function /
            // arrow envelope and the JSX `return` are dropped). A bare `export default <Component>;`
            // statement naming the component is also dropped — the backend's `build_module` emits
            // its own single top-level `export default {Class};`. The result is the same flat shape
            // the `.treaty` path hands the shared backend, so no nested `export`/function envelope
            // can survive into the emitted module.
            let body = assemble_flat_body(
                &ret.program.body,
                &client_source,
                &component,
                &class_name,
            );
            (component.template_html, body)
        }
        None => {
            errors.push("jsx: no component (default-export or named function returning JSX) found".to_string());
            (String::new(), client_source.clone())
        }
    };

    // 3b. Restore the Angular control-flow blocks lifted in step 1b: swap each `<treaty-cf-N />`
    //     placeholder back to its lowered `@if`/`@for`/`@switch` HTML. Done before the signals pass
    //     so block-body interpolations are auto-called consistently with the rest of the template.
    let template_html = angular_blocks::restore(&template_html, &cf_blocks);

    // 4. When a server block was present, emit it and rewrite client call sites — same contract as
    //    the `.treaty` path. The lifted server-fn body texts are kept so they can be redacted out of
    //    the client map's `sourcesContent` below.
    let (javascript, server_module, server_bodies) = if extraction.server_fns.is_empty() {
        (javascript, None, Vec::new())
    } else {
        let emit = ElysiaEdenPlugin.emit(&extraction.server_fns);
        let rewritten = rewrite_call_sites(&javascript, &emit.client_bindings);
        let bodies: Vec<String> =
            extraction.server_fns.iter().map(|f| f.source.clone()).collect();
        (rewritten, Some(emit.server_module), bodies)
    };

    // 5. Signals-by-default: every component variable is a signal. Wrap simple-value declarations in
    //    `signal(...)`, rewrite writes to `.set(...)` / `.update(...)`, and inject the `signal`
    //    import. The discovered signal names drive the template auto-call so a bare `{{ x }}` read of
    //    a signal becomes `{{ x() }}`. Run after the server rewrite so it sees the final client JS.
    let transform = signals::transform(&javascript);
    let javascript = transform.javascript;
    let template_html = signals::auto_call_template(&template_html, &transform.signals);

    // 6. Reuse the shared render3 backend. JSX components carry no `<style>` chunk yet, so styles
    //    are empty for this phase. The directive classes applied in the template (collected during
    //    lowering) are threaded in so they auto-import into the component's `dependencies`. The map
    //    embeds the ORIGINAL authoring source (`source`) as `sourcesContent`, named by `file_name`,
    //    exactly as the base `@Component` `.ts` and `.treaty` paths do.
    let (compiled, map) = compile_from_parts_with_directives_and_map(
        &class_name,
        &javascript,
        &template_html,
        "",
        file_name,
        &directive_refs,
        file_name,
        source,
    );

    // CLIENT PRIVACY: the map's `sourcesContent` is the original `.tjsx` source, which still carries
    // any `server { … }` block. Redact each lifted server-fn body out of the map content (blanked to
    // position-preserving whitespace) so the server source never reaches the client map — the same
    // guarantee the `.treaty` and base `@Component` paths provide.
    let map = map.map(|m| redact_server_bodies_in_map(&m, &server_bodies));

    let mut all_errors = errors;
    all_errors.extend(compiled.errors);

    CompiledAuthoring {
        code: compiled.code,
        server_module,
        map,
        errors: all_errors,
    }
}

/// The raw lowering of a single component function/arrow, before module assembly: the lowered
/// template HTML and the component's FLAT inner body (statements inside the function/arrow body with
/// the JSX `return` removed). `find_component` wraps this with the enclosing declaration's span and
/// the component name to build a [`LoweredComponent`].
struct LoweredBody {
    template_html: String,
    /// The component function/arrow inner body, JSX `return` removed. Empty for an expression-bodied
    /// arrow (`() => <JSX/>`), which has no statements besides the returned JSX.
    inner: String,
}

/// Scan top-level statements for the component function and lower its returned JSX.
///
/// Recognized component forms:
///   * `export default function Name() { …; return <JSX/>; }`
///   * `export default () => <JSX/>` / `export default () => { …; return <JSX/>; }`
///   * a named `function Name() { …; return <JSX/>; }` declaration (with optional sibling
///     `export default Name;`)
///   * `const Name = () => <JSX/>` / `const Name = () => { …; return <JSX/>; }`
///   * `export function Name()` / `export const Name = () => …`
///
/// The first form that yields a JSX return wins. The returned [`LoweredComponent`] carries the whole
/// declaration statement's byte span (so it can be excised from the module-level body) plus the
/// component's flattened inner body.
fn find_component(body: &[Statement], source: &str) -> Option<LoweredComponent> {
    // Prefer the default export, then fall back to the first named function/arrow that returns JSX.
    for stmt in body {
        if let Statement::ExportDefaultDeclaration(export) = stmt {
            let (lowered, name) = match &export.declaration {
                ExportDefaultDeclarationKind::FunctionDeclaration(func) => (
                    lower_function(func, source),
                    func.id.as_ref().map(|id| id.name.to_string()),
                ),
                ExportDefaultDeclarationKind::ArrowFunctionExpression(arrow) => {
                    (lower_arrow(arrow, source), None)
                }
                _ => (None, None),
            };
            if let Some(lowered) = lowered {
                return Some(component_from(stmt, lowered, name));
            }
        }
    }

    for stmt in body {
        match stmt {
            Statement::FunctionDeclaration(func) => {
                if let Some(lowered) = lower_function(func, source) {
                    let name = func.id.as_ref().map(|id| id.name.to_string());
                    return Some(component_from(stmt, lowered, name));
                }
            }
            Statement::VariableDeclaration(decl) => {
                for declarator in &decl.declarations {
                    if let Some(Expression::ArrowFunctionExpression(arrow)) = &declarator.init {
                        if let Some(lowered) = lower_arrow(arrow, source) {
                            let name = declarator.id.get_identifier_name().map(|n| n.to_string());
                            return Some(component_from(stmt, lowered, name));
                        }
                    }
                }
            }
            // `export default` handled above; an `export function`/`export const` wraps the same
            // declaration kinds, so descend into it.
            Statement::ExportNamedDeclaration(export) => {
                if let Some(decl) = &export.declaration {
                    match decl {
                        Declaration::FunctionDeclaration(func) => {
                            if let Some(lowered) = lower_function(func, source) {
                                let name = func.id.as_ref().map(|id| id.name.to_string());
                                return Some(component_from(stmt, lowered, name));
                            }
                        }
                        Declaration::VariableDeclaration(var) => {
                            for declarator in &var.declarations {
                                if let Some(Expression::ArrowFunctionExpression(arrow)) =
                                    &declarator.init
                                {
                                    if let Some(lowered) = lower_arrow(arrow, source) {
                                        let name =
                                            declarator.id.get_identifier_name().map(|n| n.to_string());
                                        return Some(component_from(stmt, lowered, name));
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    None
}

/// Build a [`LoweredComponent`] from the located declaration statement, its raw lowering, and the
/// component's author-given name (if any). The declaration's whole byte span is recorded so the
/// module assembler can excise it and substitute the flattened inner body.
fn component_from(stmt: &Statement, lowered: LoweredBody, name: Option<String>) -> LoweredComponent {
    let span = oxc_span::GetSpan::span(stmt);
    LoweredComponent {
        template_html: lowered.template_html,
        declaration_span: (span.start as usize, span.end as usize),
        component_body: lowered.inner,
        name,
    }
}

/// Lower a `function` component: find the `return <JSX>` in its body, and extract the body's inner
/// statements with that return removed.
fn lower_function(func: &Function, source: &str) -> Option<LoweredBody> {
    let body = func.body.as_deref()?;
    let return_span = jsx_return_span(&body.statements, source)?;
    let inner = body_inner_without_return(
        body.span.start as usize,
        body.span.end as usize,
        return_span,
        source,
    );
    let template_html = lower_jsx_return(&body.statements, source)?;
    Some(LoweredBody { template_html, inner })
}

/// Lower an arrow component: either an expression body that is JSX, or a block body with a
/// `return <JSX>`.
fn lower_arrow(arrow: &ArrowFunctionExpression, source: &str) -> Option<LoweredBody> {
    // An expression-bodied arrow (`() => <JSX/>`) stores the expression as a single `return`
    // statement in `body.statements` with `expression == true`. Its only content is the returned
    // JSX, so the flattened inner body is empty.
    if arrow.expression {
        let stmt = arrow.body.statements.first()?;
        if let Statement::ExpressionStatement(expr_stmt) = stmt {
            if let Some(html) = lower_jsx_expression(&expr_stmt.expression, source) {
                return Some(LoweredBody {
                    template_html: html,
                    inner: String::new(),
                });
            }
        }
        return None;
    }
    let return_span = jsx_return_span(&arrow.body.statements, source)?;
    let inner = body_inner_without_return(
        arrow.body.span.start as usize,
        arrow.body.span.end as usize,
        return_span,
        source,
    );
    let template_html = lower_jsx_return(&arrow.body.statements, source)?;
    Some(LoweredBody { template_html, inner })
}

/// The byte span of the first `return <JSX>` statement among `statements`, or `None`.
fn jsx_return_span(statements: &[Statement], source: &str) -> Option<(usize, usize)> {
    for stmt in statements {
        if let Statement::ReturnStatement(ret) = stmt {
            let argument = ret.argument.as_ref()?;
            if lower_jsx_expression(argument, source).is_some() {
                return Some((ret.span.start as usize, ret.span.end as usize));
            }
        }
    }
    None
}

/// Lower the first `return <JSX>` statement among `statements` to template HTML.
fn lower_jsx_return(statements: &[Statement], source: &str) -> Option<String> {
    for stmt in statements {
        if let Statement::ReturnStatement(ret) = stmt {
            let argument = ret.argument.as_ref()?;
            if let Some(html) = lower_jsx_expression(argument, source) {
                return Some(html);
            }
        }
    }
    None
}

/// Extract the interior of a function/arrow block body (`{ … }`, spanning `body_start..body_end`),
/// with the JSX `return` statement (`return_span`) removed. The enclosing braces are dropped so the
/// result is a flat statement list suitable for the shared backend's own component wrapper.
fn body_inner_without_return(
    body_start: usize,
    body_end: usize,
    return_span: (usize, usize),
    source: &str,
) -> String {
    // The body span includes the surrounding braces; the inner statements live strictly between
    // them. Guard the brace trim against a degenerate (empty) body.
    let inner_start = body_start.saturating_add(1).min(source.len());
    let inner_end = body_end.saturating_sub(1).max(inner_start);
    let (ret_start, ret_end) = return_span;

    let mut inner = String::with_capacity(inner_end - inner_start);
    // Everything from the body's first inner byte up to the JSX return.
    if ret_start > inner_start {
        inner.push_str(&source[inner_start..ret_start.min(inner_end)]);
    }
    // Everything after the JSX return up to the body's last inner byte. (Anything textually after a
    // `return` is dead code, but preserving it keeps the body byte-faithful for the rare early
    // helper-after-return; it is harmless inside the synthesized wrapper.)
    if ret_end < inner_end {
        inner.push_str(&source[ret_end..inner_end]);
    }
    inner
}

/// Lower an expression to template HTML if it is a JSX element or fragment.
fn lower_jsx_expression(expression: &Expression, source: &str) -> Option<String> {
    match expression {
        Expression::JSXElement(element) => Some(template::lower_element(element, source)),
        Expression::JSXFragment(fragment) => Some(template::lower_fragment(fragment, source)),
        // `return ( <JSX/> )` — OXC preserves the parentheses as a wrapper node; unwrap and recurse
        // so the common parenthesized-return authoring form is supported.
        Expression::ParenthesizedExpression(paren) => {
            lower_jsx_expression(&paren.expression, source)
        }
        _ => None,
    }
}

/// Assemble the FLAT module-top-level body the shared backend's `build_module` expects, from the
/// (preprocessed) `source`, the located `component`, and the derived `class_name`.
///
/// The shared backend re-wraps a flat component body inside its own `function {Class}() { … return
/// { bindings }; }` envelope and emits exactly one top-level `export default {Class};`. So the body
/// handed to it must NOT contain the author's enclosing `export default function`/`const X = () =>`
/// declaration (that was the malformed-module bug — a nested export + stray braces). This walks the
/// top-level statements and, for each:
///   * the located component declaration is REPLACED by its flattened inner body (the function/arrow
///     envelope and the JSX `return` already removed);
///   * a sibling bare `export default <Component>;` re-exporting the component by name is DROPPED
///     (the backend emits its own default export);
///   * every other statement (the author's `import`s, helper functions, type aliases, …) is kept
///     verbatim — `build_module` itself hoists any `import` declarations to module scope.
///
/// The result is the same flat shape the `.treaty` path produces, so it re-parses as a valid module
/// once wrapped, with no nested export and no stray braces.
fn assemble_flat_body(
    body: &[Statement],
    source: &str,
    component: &LoweredComponent,
    class_name: &str,
) -> String {
    let (decl_start, decl_end) = component.declaration_span;
    let mut out = String::with_capacity(source.len());

    for stmt in body {
        let span = oxc_span::GetSpan::span(stmt);
        let (start, end) = (span.start as usize, span.end as usize);

        // The component declaration → its flattened inner body.
        if start == decl_start && end == decl_end {
            out.push_str(component.component_body.trim());
            out.push('\n');
            continue;
        }

        // A bare `export default <Component>;` (or `export default <Component>`) that re-exports the
        // component by name is dropped — the backend emits `export default {Class};` itself, and a
        // second top-level default export would be illegal. Match on the export's referenced name so
        // an unrelated `export default <expr>` is preserved (it would be a second component, out of
        // scope, and is harmless to keep).
        if is_default_export_of(stmt, component, class_name) {
            continue;
        }

        out.push_str(&source[start..end]);
        out.push('\n');
    }

    out
}

/// Whether `stmt` is a bare `export default <Ident>;` re-exporting the located component by its
/// author name (or by the derived `class_name`, in case the author already named it the class name).
fn is_default_export_of(
    stmt: &Statement,
    component: &LoweredComponent,
    class_name: &str,
) -> bool {
    let Statement::ExportDefaultDeclaration(export) = stmt else {
        return false;
    };
    let ExportDefaultDeclarationKind::Identifier(ident) = &export.declaration else {
        return false;
    };
    let referenced = ident.name.as_str();
    referenced == class_name
        || component.name.as_deref() == Some(referenced)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFINE: &str = "\u{0275}\u{0275}defineComponent";

    #[test]
    fn pascal_case_from_tsx_name() {
        assert_eq!(to_pascal_case("hello-world.tsx"), "HelloWorld");
        assert_eq!(to_pascal_case("my_widget.tjsx"), "MyWidget");
    }

    /// Parse `code` as an ES module and assert it is well-formed: no parse errors, exactly one
    /// top-level `export default`, and no `export`/`import` nested inside a function/block (the
    /// nested-export class of bug). Returns the parsed-OK result for the caller's further assertions.
    fn assert_well_formed_module(code: &str) {
        let allocator = Allocator::default();
        let module_type = SourceType::default().with_module(true);
        let parsed = JsParser::new(&allocator, code, module_type).parse();
        assert!(
            parsed.errors.is_empty(),
            "emitted module did not RE-PARSE as a valid ES module: {:?}\n--- code ---\n{code}",
            parsed.errors
        );

        // Exactly one top-level `export default` — the backend's `export default {Class};`.
        let top_level_default_exports = parsed
            .program
            .body
            .iter()
            .filter(|s| matches!(s, Statement::ExportDefaultDeclaration(_)))
            .count();
        assert_eq!(
            top_level_default_exports, 1,
            "expected exactly one top-level `export default`, found {top_level_default_exports}\n--- code ---\n{code}"
        );

        // No `export`/`import` may appear anywhere other than the module top level. Re-parsing a body
        // that contains a nested `export default function …` succeeds only because OXC is lenient in
        // some configs; the robust guarantee is that the synthesized component wrapper
        // `function {Class}() { … }` contains NEITHER an `export` keyword NOR a nested function-scope
        // `export default`. Assert the wrapper body holds no `export ` token.
        if let Some(fn_idx) = code.find("function ") {
            // The component wrapper is the function whose body precedes the `.ɵfac` static assignment.
            if let Some(fac_idx) = code.find("\u{0275}fac") {
                let wrapper = &code[fn_idx..fac_idx];
                assert!(
                    !wrapper.contains("export "),
                    "an `export` leaked inside the synthesized component wrapper (nested export bug); got wrapper:\n{wrapper}"
                );
            }
        }
    }

    #[test]
    fn compiles_trivial_default_export_function_component() {
        let source = "export default function App() {\n  return <div>hi</div>;\n}\n";
        let out = compile(source, "app.tsx");

        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(out.code.contains(DEFINE), "no defineComponent; got: {}", out.code);
        assert!(out.code.contains("App"), "class name missing; got: {}", out.code);
        assert!(out.server_module.is_none(), "unexpected server module");
        // The lowered template text reached the backend.
        assert!(out.code.contains("App_Template"), "no template fn; got: {}", out.code);
    }

    #[test]
    fn compiles_arrow_expression_body_component() {
        let source = "const Widget = () => <span>x</span>;\n";
        let out = compile(source, "widget.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(out.code.contains(DEFINE), "no defineComponent; got: {}", out.code);
        assert!(out.code.contains("Widget"), "class name missing; got: {}", out.code);
    }

    #[test]
    fn compiles_parenthesized_return() {
        // The common `return ( <JSX/> )` form: OXC keeps the parens as a wrapper node.
        let source = "export default function App() {\n  return (\n    <div>hi</div>\n  );\n}\n";
        let out = compile(source, "app.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(out.code.contains(DEFINE), "no defineComponent; got: {}", out.code);
        assert!(out.code.contains("App_Template"), "no template fn; got: {}", out.code);
    }

    #[test]
    fn in_component_server_block_emits_module_and_rewrites_call_site() {
        // A `.tsx` COMPONENT whose body declares an in-component `server { … }` block must yield a
        // non-None server_module and rewrite the client call site to the plugin's client binding —
        // same contract as a top-level block on the `.treaty` path.
        let source = "export default function App() {\n\
  function onSave(user) { return save(user); }\n\
  server {\n\
    async function save(user: User) { return db.insert(user); }\n\
  }\n\
  return <button onClick={onSave}>save</button>;\n\
}\n";
        let out = compile(source, "app.tsx");

        // The in-component server fn is extracted and emitted (Elysia/Eden reference backend).
        let server_module = out.server_module.expect("expected a server module for in-component block");
        assert!(
            server_module.contains(".post('/__server/save'"),
            "no save route in server module; got: {server_module}"
        );
        // The free call to `save` in the component body is rewritten to the Eden client binding,
        // and the server body never leaks into the client JS.
        assert!(
            out.code.contains("client.__server.save.post"),
            "call site not rewritten to client binding; got: {}",
            out.code
        );
        assert!(
            !out.code.contains("db.insert"),
            "server body leaked into client JS; got: {}",
            out.code
        );
    }

    #[test]
    fn tjsx_without_server_block_carries_a_v3_map() {
        // A `.tjsx` component (no server block) compiles WITH an additive v3 map whose
        // `sourcesContent` embeds the original authoring source, named by the file.
        let source = "export default function App() {\n  return <div>hi</div>;\n}\n";
        let out = compile(source, "app.tjsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);

        let map = out.map.expect("expected a source map for a `.tjsx` component");
        let value: serde_json::Value =
            serde_json::from_str(&map).expect("map should be valid JSON");
        assert_eq!(value["version"], serde_json::json!(3), "not a v3 map: {map}");
        assert_eq!(value["sources"][0], serde_json::json!("app.tjsx"), "wrong source name");
        let contents = value["sourcesContent"].as_array().expect("sourcesContent array");
        assert!(
            contents.iter().any(|c| c.as_str() == Some(source)),
            "authoring source not embedded as sourcesContent; got: {map}"
        );
    }

    #[test]
    fn tjsx_server_block_body_is_absent_from_client_map() {
        // CLIENT PRIVACY: a `.tjsx` whose component body declares an in-component `server { … }`
        // block must compile to a v3 map whose `sourcesContent` does NOT contain the server body.
        let source = "export default function App() {\n\
  function onSave(user) { return save(user); }\n\
  server {\n\
    async function save(user: User) { return db.insert(user); }\n\
  }\n\
  return <button onClick={onSave}>save</button>;\n\
}\n";
        let out = compile(source, "app.tjsx");
        assert!(out.server_module.is_some(), "expected a server module");

        let map = out.map.expect("expected a source map for a server-block `.tjsx`");
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
        // The surviving client text (the handler) is still present in the redacted map.
        let joined: String = contents
            .iter()
            .filter_map(|c| c.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("function onSave"), "client body lost from map: {joined}");
    }

    #[test]
    fn compiles_tsx_with_non_ascii_without_panicking() {
        // FIX #1: a `.tsx` whose body and JSX template carry non-ASCII text (accented words + an
        // emoji) must compile without a mid-UTF-8-char byte-slice panic.
        let source = "export default function Saludo() {\n  \
const t\u{00ed}tulo = 'caf\u{00e9} \u{1F680}';\n  \
return <div title=\"na\u{00ef}ve \u{1F4A1}\">Hola caf\u{00e9} \u{1F600}</div>;\n}\n";
        let out = compile(source, "saludo.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(out.code.contains(DEFINE), "no defineComponent; got: {}", out.code);
        // The non-ASCII template text survived lowering.
        assert!(
            out.code.contains("Hola caf\u{00e9}"),
            "non-ASCII template text lost; got: {}",
            out.code
        );
    }

    #[test]
    fn lowers_interpolation_container() {
        let source = "export default function App() {\n  const name = 'World';\n  return <div>{name}</div>;\n}\n";
        let out = compile(source, "app.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        // `{name}` lowers to Angular interpolation binding against the component context. `name` is a
        // signal-by-default (simple string init), so the read auto-calls: `ctx.name()`.
        assert!(out.code.contains("\u{0275}\u{0275}textInterpolate"), "no interpolation; got: {}", out.code);
        assert!(out.code.contains("ctx.name()"), "did not auto-call signal read; got: {}", out.code);
    }

    #[test]
    fn signals_by_default_end_to_end_counter() {
        // The headline acceptance case for signals-by-default: a `let count = 0` becomes a signal, a
        // template `{count}` read auto-calls (`{{ count() }}` → `ctx.count()`), and a `count++` in an
        // event handler lowers to `count.update(prev => prev + 1)`.
        let source = "export default function Counter() {\n\
  let count = 0;\n\
  const inc = () => { count++; };\n\
  return <button onClick={inc}>{count}</button>;\n\
}\n";
        let out = compile(source, "counter.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        // `count` is wrapped as a signal and the `signal` import is injected.
        assert!(
            code.contains("count = signal(0)"),
            "count not wrapped as signal; got: {code}"
        );
        assert!(
            code.contains("import { signal } from \"@angular/core\";"),
            "signal import missing; got: {code}"
        );
        // The template read auto-calls the signal: `{{ count() }}` binds `ctx.count()`.
        assert!(
            code.contains("ctx.count()"),
            "template did not auto-call signal read; got: {code}"
        );
        // The handler write lowers to the signal update API.
        assert!(
            code.contains("count.update(prev => prev + 1)"),
            "count++ not lowered to update; got: {code}"
        );
        // `inc` is behaviour, not state: it is NOT wrapped in `signal(...)`.
        assert!(
            !code.contains("signal(() =>"),
            "arrow handler wrongly wrapped as signal; got: {code}"
        );
    }

    #[test]
    fn directive_namespace_form_applies_and_auto_imports() {
        // PREFERRED form: `use:autofocus` applies the `Autofocus` directive and auto-imports it into
        // the component's dependencies via selectorless resolution (no manual imports array).
        let source = "export default function Form() {\n  return <input use:autofocus />;\n}\n";
        let out = compile(source, "form.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        // The directive class lands in `dependencies` by selectorless auto-import.
        assert!(
            code.contains("dependencies: [Autofocus]")
                || code.contains("dependencies:[Autofocus]"),
            "Autofocus not auto-imported into dependencies; got: {code}"
        );
        // The host carries the directive's bare input attribute (no value-less directive is dropped).
        assert!(code.contains("autofocus"), "directive attr missing; got: {code}");
    }

    #[test]
    fn directive_namespace_form_binds_input_and_auto_imports() {
        // `use:tooltip={msg}` binds the directive's `tooltip` input and auto-imports `Tooltip`.
        let source = "export default function Btn() {\n\
  const msg = 'hi';\n\
  return <button use:tooltip={msg}>x</button>;\n\
}\n";
        let out = compile(source, "btn.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        assert!(
            code.contains("dependencies: [Tooltip]") || code.contains("dependencies:[Tooltip]"),
            "Tooltip not auto-imported; got: {code}"
        );
        // The input binding reaches the template (a property instruction binds `tooltip`).
        assert!(
            code.contains("\u{0275}\u{0275}property") || code.contains("tooltip"),
            "tooltip input binding missing; got: {code}"
        );
    }

    #[test]
    fn directive_capitalized_attribute_applies_and_auto_imports() {
        // `<input Autofocus />` applies the `Autofocus` directive.
        let source = "export default function Form() {\n  return <input Autofocus />;\n}\n";
        let out = compile(source, "form.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(
            code.contains("dependencies: [Autofocus]")
                || code.contains("dependencies:[Autofocus]"),
            "Autofocus not auto-imported; got: {code}"
        );
    }

    #[test]
    fn directive_structural_form_lowers_and_auto_imports() {
        // A STRUCTURAL directive lowers to an `<ng-template>` host carrying the structural binding,
        // and the `Highlight` directive auto-imports. The JSX spelling of `*highlight` is the
        // `structural:highlight` namespace; the author imports `Highlight` so the name resolves.
        let source = "import { Highlight } from './highlight';\n\
export default function Card() {\n\
  const c = 'yellow';\n\
  return <div structural:highlight={c}>x</div>;\n\
}\n";
        let out = compile(source, "card.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        // Structural application: an embedded `<ng-template>` view is emitted (a nested template fn).
        assert!(
            code.contains("Card_Template") && code.contains("\u{0275}\u{0275}template"),
            "no embedded template for structural directive; got: {code}"
        );
        assert!(
            code.contains("dependencies: [Highlight]")
                || code.contains("dependencies:[Highlight]"),
            "Highlight not auto-imported; got: {code}"
        );
    }

    #[test]
    fn directive_bare_lowercase_matches_imported_class() {
        // The Angular-attribute form: a bare lowercase `tooltip` attribute matching an imported
        // `Tooltip` class applies the directive and auto-imports it.
        let source = "import { Tooltip } from './tooltip';\n\
export default function Btn() {\n\
  const msg = 'hi';\n\
  return <span tooltip={msg}>x</span>;\n\
}\n";
        let out = compile(source, "btn.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(
            code.contains("dependencies: [Tooltip]") || code.contains("dependencies:[Tooltip]"),
            "Tooltip not auto-imported; got: {code}"
        );
    }

    #[test]
    fn unused_directive_import_is_not_a_dependency() {
        // An imported directive that is never applied in the template must NOT become a dependency —
        // the "unused imports are not emitted" contract holds for directives too.
        let source = "import { Tooltip } from './tooltip';\n\
export default function Btn() {\n  return <span>x</span>;\n}\n";
        let out = compile(source, "btn.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(
            !out.code.contains("dependencies"),
            "unused directive import leaked into dependencies; got: {}",
            out.code
        );
    }

    #[test]
    fn realistic_counter_tsx_compiles_all_features_to_ivy() {
        // Headline end-to-end acceptance: a realistic Counter component that exercises, in one source,
        // every JSX authoring feature and must compile all the way to Ivy `defineComponent` output:
        //   * signals-by-default state (`let count = 0` / `let items = [...]`),
        //   * an `onClick` event handler that increments (`count++`),
        //   * a static `class` attribute,
        //   * a `{count}` text interpolation that auto-calls the signal,
        //   * an `items.map(...)` lowered to an `@for` list,
        //   * a `{cond && <JSX/>}` lowered to an `@if`,
        //   * a `use:` directive (selectorless auto-import).
        let source = "export default function Counter() {\n\
  let count = 0;\n\
  let items = [1, 2, 3];\n\
  const inc = () => { count++; };\n\
  return <div class=\"counter\">\n\
      <button onClick={inc} use:autofocus>{count}</button>\n\
      {count() > 0 && <p>positive</p>}\n\
      <ul>\n\
        {items.map((item) => <li>{item}</li>)}\n\
      </ul>\n\
    </div>;\n\
}\n";
        let out = compile(source, "counter.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        // (0) Compiled all the way to an Ivy component definition.
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        assert!(code.contains("Counter"), "class name missing; got: {code}");
        assert!(code.contains("Counter_Template"), "no template fn; got: {code}");

        // (1) signals-by-default: `count` and `items` are wrapped, `signal` is imported, `inc` is not
        //     state so it is never wrapped.
        assert!(code.contains("count = signal(0)"), "count not a signal; got: {code}");
        assert!(code.contains("items = signal("), "items not a signal; got: {code}");
        assert!(
            code.contains("import { signal } from \"@angular/core\";"),
            "signal import missing; got: {code}"
        );
        assert!(!code.contains("signal(() =>"), "handler wrongly wrapped; got: {code}");

        // (2) onClick increment lowers to the signal update API and binds a listener instruction.
        assert!(
            code.contains("count.update(prev => prev + 1)"),
            "count++ not lowered to update; got: {code}"
        );
        assert!(
            code.contains("\u{0275}\u{0275}listener"),
            "no listener instruction for onClick; got: {code}"
        );

        // (3) the static `class` reaches the const pool / element instruction.
        assert!(code.contains("counter"), "class attr missing; got: {code}");

        // (4) `{count}` interpolation auto-calls the signal read.
        assert!(
            code.contains("\u{0275}\u{0275}textInterpolate"),
            "no interpolation instruction; got: {code}"
        );
        assert!(code.contains("ctx.count()"), "signal read not auto-called; got: {code}");

        // (5) `items.map(...)` lowered to an `@for` → Ivy repeater.
        assert!(
            code.contains("\u{0275}\u{0275}repeaterCreate") || code.contains("\u{0275}\u{0275}repeater"),
            "items.map did not lower to a repeater; got: {code}"
        );

        // (6) `{cond && <p/>}` lowered to an `@if` → Ivy conditional / embedded template.
        assert!(
            code.contains("\u{0275}\u{0275}conditional") || code.contains("\u{0275}\u{0275}template"),
            "&& did not lower to a conditional; got: {code}"
        );

        // (7) the `use:autofocus` directive auto-imports selectorlessly.
        assert!(
            code.contains("dependencies: [Autofocus]")
                || code.contains("dependencies:[Autofocus]"),
            "Autofocus directive not auto-imported; got: {code}"
        );
    }

    #[test]
    fn angular_if_block_in_jsx_lowers_and_compiles_to_ivy() {
        // An Angular `@if` block written DIRECTLY inside the returned JSX (not the `&&`/ternary
        // idiom) lowers to an Angular `@if` and compiles to Ivy. OXC cannot parse the block's `{ … }`
        // body as a JSX expression container, so it is lifted out of the source before parsing and
        // restored into the template after lowering.
        let source = "export default function App() {\n\
  const show = true;\n\
  return <div>@if (show) { <p>hi</p> }</div>;\n\
}\n";
        let out = compile(source, "app.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        // The block lowered to an Ivy conditional / embedded template (the `@if` reached render3).
        assert!(
            code.contains("\u{0275}\u{0275}conditional") || code.contains("\u{0275}\u{0275}template"),
            "@if did not lower to a conditional; got: {code}"
        );
        // The condition reached the Ivy conditional binding (`ctx.show ? 1 : -1`).
        assert!(
            code.contains("ctx.show"),
            "@if condition lost; got: {code}"
        );
    }

    #[test]
    fn angular_for_block_in_jsx_lowers_and_compiles_to_ivy() {
        // An Angular `@for` block written directly inside JSX lowers to `@for` → Ivy repeater.
        let source = "export default function List() {\n\
  let xs = [1, 2, 3];\n\
  return <ul>@for (x of xs; track x) { <li>{x}</li> }</ul>;\n\
}\n";
        let out = compile(source, "list.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        // `@for` lowered to an Ivy repeater.
        assert!(
            code.contains("\u{0275}\u{0275}repeaterCreate") || code.contains("\u{0275}\u{0275}repeater"),
            "@for did not lower to a repeater; got: {code}"
        );
    }

    #[test]
    fn angular_if_else_block_in_jsx_compiles_to_ivy() {
        // `@if (…) { … } @else { … }` written directly in JSX compiles, exercising the continuation
        // (`@else`) parsing of the block scanner.
        let source = "export default function App() {\n\
  const ok = true;\n\
  return <div>@if (ok) { <p>yes</p> } @else { <p>no</p> }</div>;\n\
}\n";
        let out = compile(source, "app.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        assert!(
            code.contains("\u{0275}\u{0275}conditional") || code.contains("\u{0275}\u{0275}template"),
            "@if/@else did not lower to a conditional; got: {code}"
        );
        // The `@else` produced a two-arm conditional: the selector picks branch 1 or 2 on the
        // condition (`ctx.ok ? 1 : 2`), not the single-arm `? 1 : -1` form.
        assert!(
            code.contains("ctx.ok ? 1 : 2"),
            "@else branch not emitted as a second conditional arm; got: {code}"
        );
    }

    #[test]
    fn angular_for_block_body_signal_read_auto_calls_in_template() {
        // The restored control-flow block is part of the template HTML *before* the signals pass, so
        // a `{count}` read inside a `@for` body auto-calls the signal exactly like ordinary template
        // text. This is asserted at the template layer (the JSX front-end owns it); the render3
        // backend emits the auto-called read into the block's embedded view function.
        use std::collections::HashSet;
        let source = "export default function App() {\n\
  let count = 0;\n\
  let xs = [1];\n\
  return <ul>@for (x of xs; track x) { <li>{count}</li> }</ul>;\n\
}\n";
        // Lower the source the way `compile` does up to the template, then run the signals auto-call.
        let extraction = crate::plugin::extract_server_block(source);
        let pre = angular_blocks::preprocess(&extraction.client_source);
        let allocator = Allocator::default();
        let ret = JsParser::new(&allocator, &pre.source, SourceType::tsx()).parse();
        let lowered = find_component(&ret.program.body, &pre.source).expect("component");
        let template = angular_blocks::restore(&lowered.template_html, &pre.blocks);
        let signals: HashSet<String> = ["count".to_string(), "xs".to_string()].into_iter().collect();
        let template = signals::auto_call_template(&template, &signals);
        assert!(
            template.contains("count()"),
            "block-body signal read not auto-called in template; got: {template}"
        );
        // And the whole thing still compiles to Ivy end to end.
        let out = compile(source, "app.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(out.code.contains(DEFINE), "no defineComponent; got: {}", out.code);
    }

    #[test]
    fn angular_switch_block_in_jsx_compiles_to_ivy() {
        // A `@switch`/`@case`/`@default` block — the form OXC outright rejects (nested `@case` chain
        // inside the switch braces) — compiles end to end after the block preprocessor lifts it out.
        let source = "export default function App() {\n\
  let v = 1;\n\
  return <div>@switch (v) { @case (1) { <p>one</p> } @default { <p>other</p> } }</div>;\n\
}\n";
        let out = compile(source, "app.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(out.code.contains(DEFINE), "no defineComponent; got: {}", out.code);
    }

    #[test]
    fn angular_block_and_jsx_idioms_coexist() {
        // A direct `@if` block alongside the `.map` JSX idiom in the same component: the preprocessor
        // lifts only the `@`-block, leaving the `.map` for the ordinary JSX control-flow lowering.
        let source = "export default function App() {\n\
  let show = true;\n\
  let xs = [1, 2];\n\
  return <div>@if (show) { <p>hi</p> }<ul>{xs.map(x => <li>{x}</li>)}</ul></div>;\n\
}\n";
        let out = compile(source, "app.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        // The `@if` lowered to a conditional and the `.map` lowered to a repeater.
        assert!(code.contains("ctx.show"), "@if condition lost; got: {code}");
        assert!(
            code.contains("\u{0275}\u{0275}repeaterCreate") || code.contains("\u{0275}\u{0275}repeater"),
            ".map did not lower to a repeater; got: {code}"
        );
    }

    #[test]
    fn signals_by_default_skips_reactive_primitives_end_to_end() {
        // An existing `input()` / `computed(...)` must not be double-wrapped, and a `computed`
        // reference pulls `computed` into the injected import alongside `signal`.
        let source = "export default function Widget() {\n\
  const label = input('hi');\n\
  let count = 0;\n\
  const doubled = computed(() => count() * 2);\n\
  return <div>{doubled} {label}</div>;\n\
}\n";
        let out = compile(source, "widget.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        assert!(
            !code.contains("signal(input(") && !code.contains("signal(computed("),
            "reactive primitive double-wrapped; got: {code}"
        );
        assert!(
            code.contains("count = signal(0)"),
            "plain count not wrapped; got: {code}"
        );
        assert!(
            code.contains("import { signal, computed } from \"@angular/core\";"),
            "computed not added to import; got: {code}"
        );
    }

    // --- Module assembly: the emitted module must be a VALID re-parseable ES module ------------
    // These guard the malformed-module bug: the JSX front-end used to splice the author's
    // `export default function …` verbatim inside a synthesized `function {Class}() { … }` wrapper,
    // producing an illegal nested export, stray braces, and an empty `return {}`. The fix flattens
    // the component body and lets the backend emit exactly one top-level `export default {Class};`.

    #[test]
    fn reported_bug_about_reparses_as_valid_module() {
        // The exact reported case: `export default function About(){ const team=[1,2,3]; return
        // <div>{team.length}</div> }`. It must emit a VALID module — no nested `export default`, no
        // stray brace, no empty `return {}` — and signals-by-default still applies.
        let source = "export default function About() {\n\
  const team = [1, 2, 3];\n\
  return <div>{team.length}</div>;\n\
}\n";
        let out = compile(source, "About.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        assert_well_formed_module(code);
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        // The synthesized wrapper exists and the backend's single default export is the class.
        assert!(code.contains("function About() {"), "no component wrapper; got: {code}");
        assert!(code.contains("export default About;"), "no class default export; got: {code}");
        // The author's `export default function About` does NOT survive as a nested declaration.
        assert!(
            !code.contains("export default function About"),
            "author default-export function leaked into the body; got: {code}"
        );
        // signals-by-default lowered the array initializer.
        assert!(code.contains("team = signal([1, 2, 3])"), "team not a signal; got: {code}");
    }

    #[test]
    fn named_function_with_separate_default_export_reparses() {
        // `function Card(){ … } export default Card;` — the named function is the component and the
        // sibling `export default Card;` must be DROPPED (the backend emits its own default export),
        // leaving exactly one top-level default export.
        let source = "function Card() {\n\
  const label = 'hi';\n\
  return <span>{label}</span>;\n\
}\n\
export default Card;\n";
        let out = compile(source, "card.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        assert_well_formed_module(code);
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        assert!(code.contains("export default Card;"), "no class default export; got: {code}");
        // The author's named `function Card` does not survive as a nested declaration in the wrapper.
        assert!(
            !code.contains("return <span>"),
            "JSX return leaked into body; got: {code}"
        );
    }

    #[test]
    fn const_arrow_with_separate_default_export_reparses() {
        // `const Widget = () => { … }; export default Widget;` — same contract for the arrow form.
        let source = "const Widget = () => {\n\
  const n = 5;\n\
  return <p>{n}</p>;\n\
};\n\
export default Widget;\n";
        let out = compile(source, "widget.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        assert_well_formed_module(code);
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        assert!(code.contains("export default Widget;"), "no class default export; got: {code}");
        // signals-by-default applied to the inner declaration.
        assert!(code.contains("n = signal(5)"), "n not a signal; got: {code}");
    }

    #[test]
    fn arrow_expression_body_reparses_as_valid_module() {
        // `const Widget = () => <span>x</span>;` (no block body): the flattened inner body is empty,
        // and the emitted module is still valid with one default export.
        let source = "const Widget = () => <span>x</span>;\n";
        let out = compile(source, "widget.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert_well_formed_module(&out.code);
        assert!(out.code.contains("export default Widget;"), "no default export; got: {}", out.code);
    }

    #[test]
    fn import_plus_default_export_function_reparses() {
        // The author's top-level `import` must be HOISTED to module scope (never spliced into the
        // wrapper), and the `export default function App` must NOT nest. This is the App.tjsx form.
        let source = "import { Helper } from './helper';\n\
export default function App() {\n\
  const greeting = 'hi';\n\
  return <div>{greeting}</div>;\n\
}\n";
        let out = compile(source, "App.tjsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        assert_well_formed_module(code);
        // The user import is hoisted above the wrapper (module scope), not inside it.
        let import_idx = code
            .find("import { Helper } from './helper';")
            .expect("user import missing");
        let wrapper_idx = code.find("function App() {").expect("no wrapper");
        assert!(
            import_idx < wrapper_idx,
            "user import not hoisted above the wrapper; got: {code}"
        );
        assert!(
            !code.contains("export default function App"),
            "author default-export function leaked into the body; got: {code}"
        );
    }

    #[test]
    fn capitalized_and_lowercase_names_both_reparse() {
        // The component name policy is PascalCase of the file stem regardless of the author's casing;
        // a lowercase authoring name must still yield a valid module.
        let lower = "export default function about() {\n  return <div>x</div>;\n}\n";
        let out = compile(lower, "about.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert_well_formed_module(&out.code);
        assert!(out.code.contains("export default About;"), "wrong default export; got: {}", out.code);
    }
}
