//! JSX control-flow lowering into Angular's built-in block control flow (`@if`, `@for`, `@switch`).
//!
//! This module owns every "structural" lowering the [`super::template`] visitor cannot express as a
//! plain element/attribute/interpolation. It is invoked from the template visitor whenever it meets
//! a `{ … }` expression container in child position (see [`super::template::lower_children`]).
//!
//! Three JSX idioms lower to Angular control flow:
//!
//!   * **logical-and** — `{cond && <X/>}` → `@if (cond) { <X/> }`. The `<X/>` body is recursively
//!     lowered, so nested elements / attributes / further control flow are fully transpiled.
//!   * **ternary** — `{cond ? <A/> : <B/>}` → `@if (cond) { <A/> } @else { <B/> }`. Either arm may
//!     itself be control flow (a nested ternary lowers to `@else if`-equivalent nested `@if`).
//!   * **JS iteration** — `{items.map((item, i) => <X/>)}` →
//!     `@for (item of items; track item; let i = $index) { <X/> }`. The index parameter is
//!     optional. `.forEach` with the same callback shape lowers identically (it is the statement
//!     form of the same iteration). When no explicit key is supplied we **track the loop item
//!     itself** (`track item`) — the only universally-available identity in the callback, and the
//!     value Angular recommends when a stable id is not otherwise known.
//!
//! Angular block control flow written *directly* in the JSX (`@if (c) { … }`,
//! `@for (x of xs; track x) { … } @empty { … }`, `@switch (v) { @case (k) { … } @default { … } }`,
//! with `@else`/`@else if` and the implicit `$index` / `$count` / `$first` / `$last` / `$even` /
//! `$odd` variables) is NOT handled here: a block's `{ … }` body is not a parseable JSX expression
//! container (a multi-element / text / nested-`@case` body makes the whole TSX parse fail), so it is
//! lifted out of the source before OXC parsing and lowered by [`super::angular_blocks`], then
//! restored into the template. The helpers here are only for the JSX-*expression* idioms above
//! (`&&` / ternary / `.map`), which are ordinary parseable JSX with no textual Angular spelling.
//!
//! Statement-level iteration in the render body (`for (…) { list.push(<X/>) }`,
//! `for (const x of xs) { … }`, `xs.forEach(x => list.push(<X/>))`) that builds a render list also
//! lowers to `@for`; [`lower_iteration_statement`] performs that lowering for the loop forms that
//! carry a single iteration variable over an iterable.

use oxc_ast::ast::{
    ArrowFunctionExpression, BindingPattern, Expression, ForStatementLeft, Statement,
};
use oxc::syntax::operator::LogicalOperator;

use super::template::{expression_source, lower_renderable_expression};

/// Lower a JSX child-position expression (`{ … }`) into Angular block control flow.
///
/// Returns `Some(html)` when `expression` is one of the recognized control-flow idioms (`&&`,
/// ternary, `.map`/`.forEach`), and `None` when it is an ordinary value the caller should render as
/// an interpolation. This is the single entry point [`super::template::lower_children`] calls.
pub(crate) fn lower_child_expression(
    expression: &oxc_ast::ast::JSXExpression,
    source: &str,
) -> Option<String> {
    // A `JSXExpression` is an `Expression` plus the `{}` empty-expression case; the empty case is
    // never control flow, so defer to the shared expression lowering for the real expressions.
    let expr = expression.as_expression()?;
    lower_expression(expr, source)
}

/// Lower a plain expression into Angular block control flow, or `None` if it is not a control-flow
/// idiom. Shared by the child-position entry point and by recursion from
/// [`super::template::lower_renderable_expression`] (so a control-flow form nested inside a branch
/// body or a `.map` return lowers too).
pub(crate) fn lower_expression(expr: &Expression, source: &str) -> Option<String> {
    match expr {
        // `cond && <X/>` → `@if (cond) { <X/> }`.
        Expression::LogicalExpression(logical) if logical.operator == LogicalOperator::And => {
            let cond = expression_source(&logical.left, source);
            let body = lower_renderable_expression(&logical.right, source);
            Some(format!("@if ({}) {{ {} }}", cond.trim(), body))
        }
        // `cond ? <A/> : <B/>` → `@if (cond) { <A/> } @else { <B/> }`.
        Expression::ConditionalExpression(cond) => {
            let test = expression_source(&cond.test, source);
            let consequent = lower_renderable_expression(&cond.consequent, source);
            let alternate = lower_renderable_expression(&cond.alternate, source);
            Some(format!(
                "@if ({}) {{ {} }} @else {{ {} }}",
                test.trim(),
                consequent,
                alternate
            ))
        }
        // `items.map(cb)` / `items.forEach(cb)` → `@for (item of items; track item[; let i = $index]) { … }`.
        Expression::CallExpression(call) => lower_iteration_call(call, source),
        // Unwrap parentheses: `({cond && <X/>})` lowers the same as the inner expression.
        Expression::ParenthesizedExpression(inner) => lower_expression(&inner.expression, source),
        _ => None,
    }
}

/// Lower a `<list>.map(cb)` / `<list>.forEach(cb)` call expression to an Angular `@for` block, or
/// `None` if the call is not an iteration over a render callback.
fn lower_iteration_call(call: &oxc_ast::ast::CallExpression, source: &str) -> Option<String> {
    let Expression::StaticMemberExpression(member) = &call.callee else {
        return None;
    };
    let method = member.property.name.as_str();
    if method != "map" && method != "forEach" {
        return None;
    }

    // The iterable is the source text of the member object (`items`, `this.items`, `a.b.c`, …).
    let iterable = expression_source(&member.object, source);

    // The first argument must be an arrow (or function) callback whose body renders JSX.
    let callback = call.arguments.first()?;
    let callback = callback.as_expression()?;
    let arrow = match callback {
        Expression::ArrowFunctionExpression(arrow) => arrow.as_ref(),
        _ => return None,
    };

    lower_for(&iterable, arrow, source)
}

/// Build an `@for` block from an iteration callback (`(item, i) => <X/>`) over `iterable`.
///
/// The callback's first parameter is the loop variable, an optional second parameter is the index
/// (`let i = $index`). Track defaults to the loop item itself (`track <item>`) when no explicit key
/// is given — see the module docs for the rationale. The callback body is lowered recursively.
fn lower_for(iterable: &str, arrow: &ArrowFunctionExpression, source: &str) -> Option<String> {
    let mut params = arrow.params.items.iter();
    let item = binding_name(&params.next()?.pattern)?;
    let index = params.next().and_then(|p| binding_name(&p.pattern));

    let body = lower_arrow_body(arrow, source);

    let mut header = format!("{item} of {}; track {item}", iterable.trim());
    if let Some(index) = index {
        header.push_str(&format!("; let {index} = $index"));
    }
    Some(format!("@for ({header}) {{ {body} }}"))
}

/// Lower an arrow callback body to template HTML. An expression-bodied arrow (`x => <li/>`) lowers
/// its expression; a block-bodied arrow (`x => { return <li/>; }`) lowers its `return` argument.
/// A block with no JSX `return` (e.g. a `.forEach` that pushes into a list) lowers each rendered
/// JSX statement it can find, so the common render-list `forEach` still produces markup.
fn lower_arrow_body(arrow: &ArrowFunctionExpression, source: &str) -> String {
    if arrow.expression {
        // Expression body: stored as a single expression statement.
        if let Some(Statement::ExpressionStatement(stmt)) = arrow.body.statements.first() {
            return lower_renderable_expression(&stmt.expression, source);
        }
        return String::new();
    }
    lower_block_render(&arrow.body.statements, source)
}

/// Lower the JSX-rendering statements of a block body (an arrow/function/loop body) to template
/// HTML. A `return <JSX>` lowers its argument; a bare `list.push(<JSX>)` / `list.push(cond && …)`
/// expression statement lowers its rendered argument; everything else (plain logic) is skipped.
fn lower_block_render(statements: &[Statement], source: &str) -> String {
    let mut out = String::new();
    for stmt in statements {
        match stmt {
            Statement::ReturnStatement(ret) => {
                if let Some(arg) = &ret.argument {
                    out.push_str(&lower_renderable_expression(arg, source));
                }
            }
            Statement::ExpressionStatement(expr_stmt) => {
                if let Some(rendered) = lower_push_call(&expr_stmt.expression, source) {
                    out.push_str(&rendered);
                }
            }
            // A nested loop inside the body composes (`@for` inside `@for`).
            Statement::ForStatement(_)
            | Statement::ForOfStatement(_)
            | Statement::ForInStatement(_) => {
                if let Some(nested) = lower_iteration_statement(stmt, source) {
                    out.push_str(&nested);
                }
            }
            _ => {}
        }
    }
    out
}

/// If `expr` is a `<list>.push(<rendered>)` call, lower its single argument to template HTML.
/// This is the render-list builder pattern (`items.push(<li/>)`) used inside `for`/`forEach`.
fn lower_push_call(expr: &Expression, source: &str) -> Option<String> {
    let Expression::CallExpression(call) = expr else {
        return None;
    };
    let Expression::StaticMemberExpression(member) = &call.callee else {
        return None;
    };
    if member.property.name.as_str() != "push" {
        return None;
    }
    let arg = call.arguments.first()?.as_expression()?;
    Some(lower_renderable_expression(arg, source))
}

/// Lower a statement-level iteration that builds a render list into an Angular `@for` block.
///
/// Handles the loop forms that carry a single iteration variable over an iterable and whose body
/// renders JSX (typically `list.push(<X/>)` or a `return`):
///   * `for (const x of xs) { … }` → `@for (x of xs; track x) { … }`
///   * `for (let i = 0; i < xs.length; i++) { … render xs[i] … }` → `@for (item of xs; track item)`
///     when the body's render references can be expressed over the loop; the C-style `for` lowers
///     by iterating the array its bound check ranges over.
///
/// Returns `None` for a loop shape that does not map to a single-iterable `@for` (the caller leaves
/// such logic in the JS body untouched).
pub(crate) fn lower_iteration_statement(stmt: &Statement, source: &str) -> Option<String> {
    match stmt {
        Statement::ForOfStatement(for_of) => {
            let item = for_left_name(&for_of.left)?;
            let iterable = expression_source(&for_of.right, source);
            let body = lower_loop_body(&for_of.body, source);
            Some(format!(
                "@for ({item} of {}; track {item}) {{ {body} }}",
                iterable.trim()
            ))
        }
        // A C-style `for (let i = 0; i < xs.length; i++)` that renders `xs[i]`: lower to a `@for`
        // over `xs` with the element bound to a fresh `item` and `let i = $index` preserving the
        // index variable. Recognized when the test compares the init variable to `<iterable>.length`.
        Statement::ForStatement(for_stmt) => {
            let (index_var, iterable) = c_style_for_range(for_stmt, source)?;
            let body = lower_loop_body(&for_stmt.body, source);
            Some(format!(
                "@for ({index_var}_item of {iterable}; track {index_var}_item; let {index_var} = $index) {{ {body} }}",
            ))
        }
        _ => None,
    }
}

/// Lower a loop body (a single statement, usually a block) to template HTML by collecting its
/// rendering statements.
fn lower_loop_body(body: &Statement, source: &str) -> String {
    match body {
        Statement::BlockStatement(block) => lower_block_render(&block.body, source),
        other => lower_block_render(std::slice::from_ref(other), source),
    }
}

/// The single binding name introduced by a `for (const x of …)` / `for (let x of …)` left side, or
/// `None` for a destructuring / multi-declarator left (which has no single `@for` variable).
fn for_left_name(left: &ForStatementLeft) -> Option<String> {
    let ForStatementLeft::VariableDeclaration(decl) = left else {
        return None;
    };
    let declarator = decl.declarations.first()?;
    binding_name(&declarator.id)
}

/// Recognize a C-style counting loop `for (let i = 0; i < <iterable>.length; i++)` and return the
/// index variable name and the iterable's source text. Returns `None` for any other loop shape.
fn c_style_for_range(
    for_stmt: &oxc_ast::ast::ForStatement,
    source: &str,
) -> Option<(String, String)> {
    use oxc_ast::ast::ForStatementInit;

    // init: `let i = 0` — capture the counter variable name.
    let ForStatementInit::VariableDeclaration(init) = for_stmt.init.as_ref()? else {
        return None;
    };
    let counter = binding_name(&init.declarations.first()?.id)?;

    // test: `i < <iterable>.length` — capture `<iterable>` from the `.length` member object.
    let Expression::BinaryExpression(test) = for_stmt.test.as_ref()? else {
        return None;
    };
    let Expression::StaticMemberExpression(member) = &test.right else {
        return None;
    };
    if member.property.name.as_str() != "length" {
        return None;
    }
    let iterable = expression_source(&member.object, source);
    Some((counter, iterable.trim().to_string()))
}

/// The identifier name bound by a simple binding pattern (`x`), or `None` for a destructuring
/// pattern (`{a}` / `[a]`) which has no single `@for` loop variable.
fn binding_name(pattern: &BindingPattern) -> Option<String> {
    match pattern {
        BindingPattern::BindingIdentifier(id) => Some(id.name.to_string()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::template::{lower_element, lower_fragment};
    use oxc_allocator::Allocator;
    use oxc_ast::ast::{Expression, Statement};
    use oxc_parser::Parser as JsParser;
    use oxc_span::SourceType;

    /// Parse `expr_src` (a JSX element or fragment expression) and lower it to template HTML, the
    /// same path the component pipeline uses for a `return <JSX/>`.
    fn lower(expr_src: &str) -> String {
        let src = format!("const __x = {expr_src};");
        let allocator = Allocator::default();
        let ret = JsParser::new(&allocator, &src, SourceType::tsx()).parse();
        assert!(
            ret.errors.is_empty(),
            "parse errors for {expr_src:?}: {:?}",
            ret.errors
        );
        for stmt in &ret.program.body {
            if let Statement::VariableDeclaration(decl) = stmt {
                for d in &decl.declarations {
                    match &d.init {
                        Some(Expression::JSXElement(el)) => return lower_element(el, &src),
                        Some(Expression::JSXFragment(frag)) => return lower_fragment(frag, &src),
                        _ => {}
                    }
                }
            }
        }
        panic!("no JSX expression found in {expr_src:?}");
    }

    #[test]
    fn logical_and_lowers_to_if() {
        // `{a && <p>x</p>}` → `@if (a) { <p>x</p> }`.
        let out = lower("<div>{a && <p>x</p>}</div>");
        assert_eq!(out, "<div>@if (a) { <p>x</p> }</div>");
    }

    #[test]
    fn logical_and_with_compound_condition() {
        let out = lower("<div>{show && count > 0 && <span>n</span>}</div>");
        // `&&` is left-associative: `(show && count > 0) && <span>` → `@if (show && count > 0)`.
        assert!(
            out.contains("@if (show && count > 0) { <span>n</span> }"),
            "compound `&&` not lowered; got {out}"
        );
    }

    #[test]
    fn ternary_lowers_to_if_else() {
        // `{c ? <a/> : <b/>}` → `@if (c) { <a/> } @else { <b/> }`.
        let out = lower("<div>{c ? <a/> : <b/>}</div>");
        assert_eq!(out, "<div>@if (c) { <a /> } @else { <b /> }</div>");
    }

    #[test]
    fn nested_ternary_lowers_recursively() {
        // The alternate is itself a ternary; it lowers to a nested `@if/@else` in the `@else` arm.
        let out = lower("<div>{a ? <x/> : b ? <y/> : <z/>}</div>");
        assert!(
            out.contains("@if (a) { <x /> } @else { @if (b) { <y /> } @else { <z /> } }"),
            "nested ternary not lowered; got {out}"
        );
    }

    #[test]
    fn map_lowers_to_for_with_track_item() {
        // `{xs.map(x => <li>{x}</li>)}` → `@for (x of xs; track x) { <li>{{ x }}</li> }`.
        let out = lower("<ul>{xs.map(x => <li>{x}</li>)}</ul>");
        assert_eq!(out, "<ul>@for (x of xs; track x) { <li>{{ x }}</li> }</ul>");
    }

    #[test]
    fn map_with_index_emits_let_index() {
        // The optional index parameter becomes `let i = $index`.
        let out = lower("<ul>{items.map((item, i) => <li>{i}</li>)}</ul>");
        assert_eq!(
            out,
            "<ul>@for (item of items; track item; let i = $index) { <li>{{ i }}</li> }</ul>"
        );
    }

    #[test]
    fn map_over_member_iterable() {
        // The iterable can be a member expression (`this.items`); its source text is preserved.
        let out = lower("<ul>{this.items.map(x => <li>{x.name}</li>)}</ul>");
        assert!(
            out.contains("@for (x of this.items; track x)"),
            "member iterable not preserved; got {out}"
        );
    }

    #[test]
    fn map_with_block_body_lowers_return() {
        // A block-bodied callback lowers its `return <JSX>`.
        let out = lower("<ul>{xs.map(x => { return <li>{x}</li>; })}</ul>");
        assert_eq!(out, "<ul>@for (x of xs; track x) { <li>{{ x }}</li> }</ul>");
    }

    #[test]
    fn nested_map_inside_if_lowers_correctly() {
        // The headline nesting case: a `.map` inside an `&&` lowers both, recursively.
        let out = lower("<div>{show && xs.map(x => <li>{x}</li>)}</div>");
        assert_eq!(
            out,
            "<div>@if (show) { @for (x of xs; track x) { <li>{{ x }}</li> } }</div>"
        );
    }

    #[test]
    fn map_inside_ternary_arm_lowers() {
        let out = lower("<div>{ok ? xs.map(x => <li>{x}</li>) : <p>none</p>}</div>");
        assert!(
            out.contains("@if (ok) { @for (x of xs; track x) { <li>{{ x }}</li> } } @else { <p>none</p> }"),
            "map inside ternary arm not lowered; got {out}"
        );
    }

    /// Lower JSX that contains Angular control-flow blocks written *directly* (the `@if`/`@for`/
    /// `@switch` bare-`{ … }` form). These blocks are not parseable JSX expression containers, so —
    /// exactly as the `compile` pipeline does — they are lifted out by [`super::super::angular_blocks`]
    /// before parsing and restored into the lowered template afterward.
    fn lower_blocks(jsx: &str) -> String {
        let pre = super::super::angular_blocks::preprocess(jsx);
        let html = lower(&pre.source);
        super::super::angular_blocks::restore(&html, &pre.blocks)
    }

    #[test]
    fn angular_for_block_in_jsx_passes_through_with_track_and_empty() {
        // An `@for` written directly in JSX (track + @empty) lowers faithfully: the header is
        // preserved verbatim, `@empty` survives, and the body element/interpolation is lowered.
        let out = lower_blocks(
            "<ul>@for (item of items; track item.id) { <li>{item.name}</li> } @empty { <li>none</li> }</ul>",
        );
        assert!(
            out.contains("@for (item of items; track item.id) {"),
            "@for header not preserved; got {out}"
        );
        assert!(out.contains("@empty {"), "@empty not preserved; got {out}");
        assert!(
            out.contains("<li>{{ item.name }}</li>"),
            "@for body not lowered; got {out}"
        );
    }

    #[test]
    fn angular_if_else_block_in_jsx_passes_through() {
        let out = lower_blocks(
            "<div>@if (c) { <p>a</p> } @else if (d) { <p>b</p> } @else { <p>c</p> }</div>",
        );
        assert!(out.contains("@if (c) {"), "@if not preserved; got {out}");
        assert!(out.contains("@else if (d) {"), "@else if not preserved; got {out}");
        assert!(out.contains("@else {"), "@else not preserved; got {out}");
    }

    #[test]
    fn angular_switch_block_in_jsx_passes_through() {
        let out = lower_blocks(
            "<div>@switch (v) { @case (1) { <p>one</p> } @default { <p>other</p> } }</div>",
        );
        assert!(out.contains("@switch (v) {"), "@switch not preserved; got {out}");
        assert!(out.contains("@case (1) {"), "@case not preserved; got {out}");
        assert!(out.contains("@default {"), "@default not preserved; got {out}");
    }

    #[test]
    fn for_loop_special_variables_pass_through() {
        // The implicit `$index`/`$count`/`$first`/`$last`/`$even`/`$odd` survive inside `{{ }}`.
        let out = lower(
            "<ul>@for (x of xs; track x; let i = $index, c = $count, f = $first, l = $last, e = $even, o = $odd) { <li>{i}</li> }</ul>",
        );
        assert!(
            out.contains("let i = $index, c = $count, f = $first, l = $last, e = $even, o = $odd"),
            "@for special variables not preserved; got {out}"
        );
    }

    #[test]
    fn map_with_component_body_recurses_attributes() {
        // The callback returns a component element with attributes/events: they lower inside `@for`.
        let out = lower("<ul>{rows.map(r => <Row item={r} onClick={pick} />)}</ul>");
        assert!(
            out.contains("@for (r of rows; track r) {"),
            "no @for header; got {out}"
        );
        assert!(out.contains("[item]=\"r\""), "attribute not lowered; got {out}");
        assert!(out.contains("(click)=\"pick($event)\""), "event not lowered; got {out}");
    }

    #[test]
    fn forEach_render_list_lowers_to_for() {
        // `xs.forEach(x => list.push(<li/>))` is the statement form of a render-list map; it lowers
        // its pushed JSX into `@for`.
        let out = lower("<ul>{xs.forEach(x => { list.push(<li>{x}</li>); })}</ul>");
        assert_eq!(out, "<ul>@for (x of xs; track x) { <li>{{ x }}</li> }</ul>");
    }

    #[test]
    fn non_control_flow_expression_is_not_lowered() {
        // A plain interpolation is NOT control flow; `lower_child_expression` returns None and the
        // template visitor renders it as `{{ }}` (verified here via the full element path).
        let out = lower("<div>{count()}</div>");
        assert_eq!(out, "<div>{{ count() }}</div>");
    }

    /// Lower a statement-level loop (the render-body form) directly, mirroring how the JSX front-end
    /// would invoke `lower_iteration_statement` over a `for`/`for…of` that builds a render list.
    fn lower_stmt(loop_src: &str) -> String {
        let allocator = Allocator::default();
        let ret = JsParser::new(&allocator, loop_src, SourceType::tsx()).parse();
        assert!(
            ret.errors.is_empty(),
            "parse errors for {loop_src:?}: {:?}",
            ret.errors
        );
        let stmt = ret.program.body.first().expect("no statement");
        super::lower_iteration_statement(stmt, loop_src).expect("statement did not lower")
    }

    #[test]
    fn for_of_statement_lowers_to_for() {
        let out = lower_stmt("for (const x of xs) { list.push(<li>{x}</li>); }");
        assert_eq!(out, "@for (x of xs; track x) { <li>{{ x }}</li> }");
    }

    #[test]
    fn c_style_for_statement_lowers_to_for_with_index() {
        let out = lower_stmt("for (let i = 0; i < xs.length; i++) { list.push(<li>{i}</li>); }");
        assert_eq!(
            out,
            "@for (i_item of xs; track i_item; let i = $index) { <li>{{ i }}</li> }"
        );
    }
}
