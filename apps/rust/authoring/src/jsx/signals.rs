//! Signals-by-default lowering for the JSX authoring front-end.
//!
//! Treaty's core rule: **every component variable is a signal by default**. A plain
//! `let count = 0` in a JSX component body is not an inert local — it is reactive state, and
//! compiles to an Angular `signal(0)`. This module performs that lowering over the component-body
//! JavaScript (the chunk [`super`] assembles before handing it to the render3 backend) and reports
//! which names became signals so the template can auto-call their reads.
//!
//! What is lowered (see [`transform`]):
//!   * **declarations** — a top-level `let`/`const`/`var x = INIT` with a *simple value*
//!     initializer (literal, identifier, call, object/array/template literal, member access,
//!     parenthesized/`as`-cast forms of those) becomes `x = signal(INIT)`. The declaration keyword
//!     is preserved (so `let count = 0` → `let count = signal(0)`), and `x` is recorded as a signal.
//!   * **writes** — anywhere in the body (typically inside an event handler), an assignment or
//!     update to a signal variable is rewritten to the signal's mutation API:
//!       * `x = v`        → `x.set(v)`
//!       * `x += n`       → `x.update(prev => prev + (n))`  (and `-=`, `*=`, `/=`, `%=`, `**=`)
//!       * `x++` / `++x`  → `x.update(prev => prev + 1)`
//!       * `x--` / `--x`  → `x.update(prev => prev - 1)`
//!   * **imports** — `import { signal } from "@angular/core"` is injected when any declaration was
//!     lowered (and `computed` is added to that import when the body references `computed(...)`).
//!
//! Conservative skips (NOT wrapped as signals):
//!   * function / arrow declarations (`const inc = () => {}`, `function f() {}`) — behaviour, not
//!     state;
//!   * destructuring declarations (`const { a } = obj`, `const [a] = xs`) — no single signal name;
//!   * an initializer that is already a reactive primitive call: `signal(...)`, `computed(...)`,
//!     `input(...)` / `input.required(...)`, `model(...)` / `model.required(...)`, `output(...)`,
//!     `inject(...)`, `viewChild(...)` / `viewChild.required(...)`, `viewChildren(...)`,
//!     `contentChild(...)` / `contentChild.required(...)`, `contentChildren(...)`,
//!     `linkedSignal(...)`, `effect(...)`, `toSignal(...)`, `resource(...)`. These are already the
//!     reactive form the author asked for; double-wrapping would be wrong.
//!
//! Both passes are span-based edits collected over the OXC AST and applied right-to-left, so the
//! rest of the body (comments, formatting, unrelated code) is preserved byte-for-byte. The template
//! auto-call ([`auto_call_template`]) is a sibling rewrite over the already-lowered template HTML:
//! a bare `{{ x }}` interpolation reading a signal `x` becomes `{{ x() }}`.

use std::collections::HashSet;

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, ArrowFunctionExpression, AssignmentExpression, AssignmentTarget, Expression,
    Function, SimpleAssignmentTarget, Statement, UpdateExpression,
};
use oxc::syntax::operator::{AssignmentOperator, UpdateOperator};
use oxc_parser::Parser as JsParser;
use oxc_span::SourceType;

/// The result of the signals-by-default JS lowering.
pub struct SignalTransform {
    /// The component-body JavaScript with declarations wrapped in `signal(...)`, writes rewritten to
    /// `.set(...)` / `.update(...)`, and the `signal`/`computed` import injected when needed.
    pub javascript: String,
    /// The names of the top-level variables that became signals — the auto-call candidate set for
    /// template interpolation reads.
    pub signals: HashSet<String>,
}

/// A single byte-range replacement in the source, applied right-to-left so earlier edits do not
/// shift later spans.
struct Edit {
    start: usize,
    end: usize,
    text: String,
}

/// Lower a JSX component-body JS chunk to signals-by-default.
///
/// See the module docs for the full set of rules. Parse failures are non-fatal: an unparseable
/// chunk is returned unchanged with an empty signal set (the body is the author's free-form code and
/// the backend reports any real syntax errors).
pub fn transform(javascript: &str) -> SignalTransform {
    if javascript.trim().is_empty() {
        return SignalTransform {
            javascript: javascript.to_string(),
            signals: HashSet::new(),
        };
    }

    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true);
    let ret = JsParser::new(&allocator, javascript, source_type).parse();

    // A parse failure means we cannot reason about the body safely; leave it untouched.
    if !ret.errors.is_empty() {
        return SignalTransform {
            javascript: javascript.to_string(),
            signals: HashSet::new(),
        };
    }

    // Pass 1: discover which component-scope declarations become signals, and queue the
    // init-wrapping edits. The component scope is the set of variable declarations that live at the
    // component's top level — which is either the module top level (the `.treaty` shape, where the
    // JS chunk *is* the body) or the body of the component function (the JSX shape,
    // `function App() { let count = 0; … }`). We collect from both so a signal declared inside the
    // component function is found. The signal-name set must be complete before pass 2 so writes
    // anywhere (including in handlers declared before the variable textually) resolve correctly.
    let mut signals: HashSet<String> = HashSet::new();
    let mut edits: Vec<Edit> = Vec::new();
    for stmt in &ret.program.body {
        collect_component_scope_declarations(stmt, javascript, &mut signals, &mut edits);
    }

    // Pass 2: rewrite reads-as-writes (assignments / updates) to signal mutation calls, recursing
    // through the whole program so writes inside handlers/closures are covered.
    let mut writes: Vec<Edit> = Vec::new();
    for stmt in &ret.program.body {
        collect_write_edits_in_statement(stmt, javascript, &signals, &mut writes);
    }
    edits.append(&mut writes);

    let body = apply_edits(javascript, edits);

    // Inject the reactive `@angular/core` import for every primitive the (possibly already
    // react-lowered) body references. We wrap declarations into `signal(...)` above AND the React
    // pre-pass may have already produced `signal(...)` / `computed(...)` / `effect(...)` / `input(...)`
    // / `inject(...)` calls before this pass ran — so the import set is driven by what the body
    // actually references, not only by whether THIS pass wrapped anything. `ensure_core_import` is a
    // no-op for any name already imported (so a hand-written `import { signal } from '@angular/core'`
    // is not duplicated), and merges all missing names into a single `@angular/core` import.
    let javascript = ensure_core_import(&body);

    SignalTransform { javascript, signals }
}

/// Auto-call signal reads in an already-lowered template HTML string.
///
/// A bare interpolation `{{ x }}` whose expression reads a signal variable `x` becomes `{{ x() }}`
/// so the value (not the signal function) renders. The rewrite is identifier-scoped: it auto-calls
/// `x` as a standalone read and as the *object* of a member/access chain (`{{ x.name }}` →
/// `{{ x().name }}`, `{{ x[0] }}` → `{{ x()[0] }}`), but never an `x` that is already a call
/// (`{{ x() }}` stays `{{ x() }}`) or a property name (`{{ obj.x }}` is untouched). Identifiers that
/// are not in `signals` pass through unchanged.
///
/// Only `{{ … }}` interpolation regions are touched; element/attribute markup outside them is left
/// exactly as produced by the template lowering.
pub fn auto_call_template(template_html: &str, signals: &HashSet<String>) -> String {
    if signals.is_empty() || !template_html.contains("{{") {
        return template_html.to_string();
    }

    let bytes = template_html.as_bytes();
    let mut out = String::with_capacity(template_html.len());
    let mut i = 0usize;
    while i < bytes.len() {
        // Find the next interpolation open `{{`.
        if bytes[i] == b'{' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            // Locate the matching `}}`.
            if let Some(close) = find_interpolation_close(template_html, i + 2) {
                let expr = &template_html[i + 2..close];
                out.push_str("{{");
                out.push_str(&auto_call_expression(expr, signals));
                out.push_str("}}");
                i = close + 2;
                continue;
            }
        }
        // Copy this byte verbatim (UTF-8 safe: we only special-case ASCII `{`).
        let ch_len = utf8_char_len(bytes[i]);
        out.push_str(&template_html[i..i + ch_len]);
        i += ch_len;
    }
    out
}

// ---------------------------------------------------------------------------
// Declaration lowering (pass 1).
// ---------------------------------------------------------------------------

/// Queue signal-wrapping edits for a component-scope statement.
///
/// A top-level variable declaration is wrapped directly (the `.treaty` shape). A component function
/// — `export default function App() {…}`, a named `function App() {…}`, an `export default () =>
/// {…}`, or a `const App = () => {…}` — is the JSX shape: its *direct* body statements are the
/// component scope, so we descend exactly one level and wrap the variable declarations there. We do
/// not descend further (declarations inside a handler/closure are locals, not component state).
fn collect_component_scope_declarations(
    stmt: &Statement,
    source: &str,
    signals: &mut HashSet<String>,
    edits: &mut Vec<Edit>,
) {
    match stmt {
        // Module-top-level declaration (the `.treaty` body shape).
        Statement::VariableDeclaration(_) => {
            collect_declaration_edits(stmt, source, signals, edits);
        }
        // `function App() { … }` — descend into the component body.
        Statement::FunctionDeclaration(func) => {
            collect_function_body_declarations(func.body.as_deref(), source, signals, edits);
        }
        // `export default function App() {…}` / `export default () => {…}`.
        Statement::ExportDefaultDeclaration(export) => {
            use oxc_ast::ast::ExportDefaultDeclarationKind;
            match &export.declaration {
                ExportDefaultDeclarationKind::FunctionDeclaration(func) => {
                    collect_function_body_declarations(
                        func.body.as_deref(),
                        source,
                        signals,
                        edits,
                    );
                }
                ExportDefaultDeclarationKind::ArrowFunctionExpression(arrow) => {
                    collect_arrow_body_declarations(arrow, source, signals, edits);
                }
                _ => {}
            }
        }
        // `export const App = () => {…}` / `export function App() {…}`.
        Statement::ExportNamedDeclaration(export) => {
            use oxc_ast::ast::Declaration;
            match &export.declaration {
                Some(Declaration::VariableDeclaration(_)) => {
                    if let Some(Declaration::VariableDeclaration(decl)) = &export.declaration {
                        collect_arrow_initializer_declarations(decl, source, signals, edits);
                        // A top-level `export const x = 0` is also component state; wrap it directly.
                        collect_var_declaration(decl, source, signals, edits);
                    }
                }
                Some(Declaration::FunctionDeclaration(func)) => {
                    collect_function_body_declarations(
                        func.body.as_deref(),
                        source,
                        signals,
                        edits,
                    );
                }
                _ => {}
            }
        }
        _ => {}
    }
}

/// Descend into a `const App = () => { … }` arrow component's body to wrap its declarations, OR
/// (when the statement is a plain top-level `const x = 0`) wrap it directly. Both are reachable
/// from a module-top-level `VariableDeclaration`.
fn collect_declaration_edits(
    stmt: &Statement,
    source: &str,
    signals: &mut HashSet<String>,
    edits: &mut Vec<Edit>,
) {
    let Statement::VariableDeclaration(decl) = stmt else {
        return;
    };
    // A `const App = () => { … }` is an arrow component: descend into its body. (Its own name is not
    // wrapped — an arrow initializer is behaviour, not state, and is skipped by the value check.)
    collect_arrow_initializer_declarations(decl, source, signals, edits);
    // The declaration itself (plain `let count = 0`) is component state at the module top level.
    collect_var_declaration(decl, source, signals, edits);
}

/// If `decl` is `const X = () => { … }` (a single arrow initializer with a block body), descend into
/// the arrow body and wrap its direct declarations as component-scope signals.
fn collect_arrow_initializer_declarations(
    decl: &oxc_ast::ast::VariableDeclaration,
    source: &str,
    signals: &mut HashSet<String>,
    edits: &mut Vec<Edit>,
) {
    for declarator in &decl.declarations {
        if let Some(Expression::ArrowFunctionExpression(arrow)) = &declarator.init {
            if !arrow.expression {
                collect_arrow_body_declarations(arrow, source, signals, edits);
            }
        }
    }
}

/// Wrap the direct variable declarations in a function component body.
fn collect_function_body_declarations(
    body: Option<&oxc_ast::ast::FunctionBody>,
    source: &str,
    signals: &mut HashSet<String>,
    edits: &mut Vec<Edit>,
) {
    let Some(body) = body else {
        return;
    };
    for stmt in &body.statements {
        if let Statement::VariableDeclaration(decl) = stmt {
            collect_var_declaration(decl, source, signals, edits);
        }
    }
}

/// Wrap the direct variable declarations in a block-bodied arrow component.
fn collect_arrow_body_declarations(
    arrow: &ArrowFunctionExpression,
    source: &str,
    signals: &mut HashSet<String>,
    edits: &mut Vec<Edit>,
) {
    if arrow.expression {
        return;
    }
    for stmt in &arrow.body.statements {
        if let Statement::VariableDeclaration(decl) = stmt {
            collect_var_declaration(decl, source, signals, edits);
        }
    }
}

/// Wrap each simple-value declarator of a single `VariableDeclaration` in `signal(...)` and record
/// its name. Destructuring, function/arrow inits, and existing reactive primitives are skipped.
fn collect_var_declaration(
    decl: &oxc_ast::ast::VariableDeclaration,
    source: &str,
    signals: &mut HashSet<String>,
    edits: &mut Vec<Edit>,
) {
    for declarator in &decl.declarations {
        // Only a plain `name = …` declarator (no destructuring) carries a single signal name.
        let Some(name) = declarator.id.get_identifier_name() else {
            continue;
        };
        let Some(init) = &declarator.init else {
            // `let x;` (no initializer) is not given a value to wrap; leave it alone.
            continue;
        };
        if !is_simple_value_initializer(init) {
            continue;
        }

        signals.insert(name.to_string());
        let span = oxc_span::GetSpan::span(init);
        let start = span.start as usize;
        let end = span.end as usize;
        edits.push(Edit {
            start,
            end,
            text: format!("signal({})", &source[start..end]),
        });
    }
}

/// Whether `init` is a *simple value* initializer eligible for signal wrapping.
///
/// Eligible: literals (string/number/bool/null/bigint/regex), identifiers, `this`, member access,
/// call expressions (that are not a reactive primitive), object/array/template literals, sequence
/// expressions, and parenthesized / `as` / `!` / `satisfies` wrappers of any of the above.
///
/// NOT eligible (conservative skips): functions and arrow functions (behaviour, not state), and any
/// call that is already a reactive primitive (`signal`/`computed`/`input`/… — see
/// [`is_reactive_primitive_call`]).
fn is_simple_value_initializer(init: &Expression) -> bool {
    match init {
        // Behaviour, not state — never a signal.
        Expression::FunctionExpression(_) | Expression::ArrowFunctionExpression(_) => false,
        // A class expression is a definition, not reactive state.
        Expression::ClassExpression(_) => false,
        // Already a reactive primitive: do not double-wrap.
        Expression::CallExpression(_) if is_reactive_primitive_call(init) => false,
        // Unwrap transparent wrappers and re-test the inner expression.
        Expression::ParenthesizedExpression(p) => is_simple_value_initializer(&p.expression),
        Expression::TSAsExpression(e) => is_simple_value_initializer(&e.expression),
        Expression::TSSatisfiesExpression(e) => is_simple_value_initializer(&e.expression),
        Expression::TSNonNullExpression(e) => is_simple_value_initializer(&e.expression),
        Expression::TSTypeAssertion(e) => is_simple_value_initializer(&e.expression),
        // Everything else with a concrete value form is eligible.
        Expression::BooleanLiteral(_)
        | Expression::NullLiteral(_)
        | Expression::NumericLiteral(_)
        | Expression::BigIntLiteral(_)
        | Expression::RegExpLiteral(_)
        | Expression::StringLiteral(_)
        | Expression::TemplateLiteral(_)
        | Expression::TaggedTemplateExpression(_)
        | Expression::Identifier(_)
        | Expression::ThisExpression(_)
        | Expression::ArrayExpression(_)
        | Expression::ObjectExpression(_)
        | Expression::CallExpression(_)
        | Expression::NewExpression(_)
        | Expression::ComputedMemberExpression(_)
        | Expression::StaticMemberExpression(_)
        | Expression::PrivateFieldExpression(_)
        | Expression::BinaryExpression(_)
        | Expression::LogicalExpression(_)
        | Expression::UnaryExpression(_)
        | Expression::ConditionalExpression(_)
        | Expression::SequenceExpression(_)
        | Expression::AwaitExpression(_) => true,
        // Anything else (yield, import(), JSX, …) is left untouched — out of scope for state.
        _ => false,
    }
}

/// Whether `expr` is a call to an Angular reactive-primitive factory that already returns a reactive
/// value, so wrapping it in `signal(...)` would be wrong.
///
/// Recognizes `name(...)` and `name.required(...)` for the documented primitives. Mirrors the
/// `signal_call` recognition in [`crate::sfc`] but over a wider set (the full reactive surface).
fn is_reactive_primitive_call(expr: &Expression) -> bool {
    let Expression::CallExpression(call) = expr else {
        return false;
    };
    let base = match &call.callee {
        // `signal(...)`, `computed(...)`, `input(...)`, …
        Expression::Identifier(id) => id.name.as_str(),
        // `input.required(...)`, `viewChild.required(...)`, `model.required(...)`
        Expression::StaticMemberExpression(member) => {
            let Expression::Identifier(base) = &member.object else {
                return false;
            };
            base.name.as_str()
        }
        _ => return false,
    };
    matches!(
        base,
        "signal"
            | "computed"
            | "linkedSignal"
            | "input"
            | "model"
            | "output"
            | "inject"
            | "effect"
            | "viewChild"
            | "viewChildren"
            | "contentChild"
            | "contentChildren"
            | "toSignal"
            | "resource"
            | "rxResource"
    )
}

// ---------------------------------------------------------------------------
// Write lowering (pass 2).
// ---------------------------------------------------------------------------

/// Recurse through a statement collecting assignment/update edits for signal writes.
fn collect_write_edits_in_statement(
    stmt: &Statement,
    source: &str,
    signals: &HashSet<String>,
    edits: &mut Vec<Edit>,
) {
    match stmt {
        Statement::ExpressionStatement(s) => {
            collect_write_edits_in_expression(&s.expression, source, signals, edits)
        }
        Statement::BlockStatement(b) => {
            for s in &b.body {
                collect_write_edits_in_statement(s, source, signals, edits);
            }
        }
        Statement::IfStatement(s) => {
            collect_write_edits_in_expression(&s.test, source, signals, edits);
            collect_write_edits_in_statement(&s.consequent, source, signals, edits);
            if let Some(alt) = &s.alternate {
                collect_write_edits_in_statement(alt, source, signals, edits);
            }
        }
        Statement::ForStatement(s) => {
            if let Some(test) = &s.test {
                collect_write_edits_in_expression(test, source, signals, edits);
            }
            if let Some(update) = &s.update {
                collect_write_edits_in_expression(update, source, signals, edits);
            }
            collect_write_edits_in_statement(&s.body, source, signals, edits);
        }
        Statement::ForOfStatement(s) => {
            collect_write_edits_in_expression(&s.right, source, signals, edits);
            collect_write_edits_in_statement(&s.body, source, signals, edits);
        }
        Statement::ForInStatement(s) => {
            collect_write_edits_in_expression(&s.right, source, signals, edits);
            collect_write_edits_in_statement(&s.body, source, signals, edits);
        }
        Statement::WhileStatement(s) => {
            collect_write_edits_in_expression(&s.test, source, signals, edits);
            collect_write_edits_in_statement(&s.body, source, signals, edits);
        }
        Statement::DoWhileStatement(s) => {
            collect_write_edits_in_statement(&s.body, source, signals, edits);
            collect_write_edits_in_expression(&s.test, source, signals, edits);
        }
        Statement::ReturnStatement(s) => {
            if let Some(arg) = &s.argument {
                collect_write_edits_in_expression(arg, source, signals, edits);
            }
        }
        Statement::SwitchStatement(s) => {
            collect_write_edits_in_expression(&s.discriminant, source, signals, edits);
            for case in &s.cases {
                if let Some(test) = &case.test {
                    collect_write_edits_in_expression(test, source, signals, edits);
                }
                for s in &case.consequent {
                    collect_write_edits_in_statement(s, source, signals, edits);
                }
            }
        }
        Statement::TryStatement(s) => {
            for s in &s.block.body {
                collect_write_edits_in_statement(s, source, signals, edits);
            }
            if let Some(handler) = &s.handler {
                for s in &handler.body.body {
                    collect_write_edits_in_statement(s, source, signals, edits);
                }
            }
            if let Some(finalizer) = &s.finalizer {
                for s in &finalizer.body {
                    collect_write_edits_in_statement(s, source, signals, edits);
                }
            }
        }
        Statement::ThrowStatement(s) => {
            collect_write_edits_in_expression(&s.argument, source, signals, edits)
        }
        Statement::LabeledStatement(s) => {
            collect_write_edits_in_statement(&s.body, source, signals, edits)
        }
        // A nested function/arrow declaration is an event handler or helper: descend into its body
        // so `count++` inside it is rewritten.
        Statement::FunctionDeclaration(func) => {
            collect_write_edits_in_function(func, source, signals, edits)
        }
        // The component function in the JSX shape is wrapped in an export; descend into it so writes
        // in its body (and in handlers it declares) are rewritten.
        Statement::ExportDefaultDeclaration(export) => {
            use oxc_ast::ast::ExportDefaultDeclarationKind;
            match &export.declaration {
                ExportDefaultDeclarationKind::FunctionDeclaration(func) => {
                    collect_write_edits_in_function(func, source, signals, edits)
                }
                ExportDefaultDeclarationKind::ArrowFunctionExpression(arrow) => {
                    collect_write_edits_in_arrow(arrow, source, signals, edits)
                }
                _ => {}
            }
        }
        Statement::ExportNamedDeclaration(export) => {
            use oxc_ast::ast::Declaration;
            match &export.declaration {
                Some(Declaration::FunctionDeclaration(func)) => {
                    collect_write_edits_in_function(func, source, signals, edits)
                }
                Some(Declaration::VariableDeclaration(decl)) => {
                    for d in &decl.declarations {
                        if let Some(init) = &d.init {
                            collect_write_edits_in_expression(init, source, signals, edits);
                        }
                    }
                }
                _ => {}
            }
        }
        // Initializers of declarations can hold handlers (`const inc = () => count++`); descend.
        Statement::VariableDeclaration(decl) => {
            for d in &decl.declarations {
                if let Some(init) = &d.init {
                    collect_write_edits_in_expression(init, source, signals, edits);
                }
            }
        }
        _ => {}
    }
}

/// Recurse through an expression collecting assignment/update edits for signal writes.
fn collect_write_edits_in_expression(
    expr: &Expression,
    source: &str,
    signals: &HashSet<String>,
    edits: &mut Vec<Edit>,
) {
    match expr {
        Expression::AssignmentExpression(assign) => {
            // Rewrite this write if it targets a signal, then recurse into the RHS (which may hold
            // further writes, e.g. `a = (b = 1)`).
            rewrite_assignment(assign, source, signals, edits);
            collect_write_edits_in_expression(&assign.right, source, signals, edits);
        }
        Expression::UpdateExpression(update) => {
            rewrite_update(update, signals, edits);
        }
        Expression::ParenthesizedExpression(p) => {
            collect_write_edits_in_expression(&p.expression, source, signals, edits)
        }
        Expression::SequenceExpression(seq) => {
            for e in &seq.expressions {
                collect_write_edits_in_expression(e, source, signals, edits);
            }
        }
        Expression::CallExpression(call) => {
            collect_write_edits_in_expression(&call.callee, source, signals, edits);
            for arg in &call.arguments {
                if let Argument::SpreadElement(s) = arg {
                    collect_write_edits_in_expression(&s.argument, source, signals, edits);
                } else if let Some(e) = arg.as_expression() {
                    collect_write_edits_in_expression(e, source, signals, edits);
                }
            }
        }
        Expression::ConditionalExpression(cond) => {
            collect_write_edits_in_expression(&cond.test, source, signals, edits);
            collect_write_edits_in_expression(&cond.consequent, source, signals, edits);
            collect_write_edits_in_expression(&cond.alternate, source, signals, edits);
        }
        Expression::LogicalExpression(logical) => {
            collect_write_edits_in_expression(&logical.left, source, signals, edits);
            collect_write_edits_in_expression(&logical.right, source, signals, edits);
        }
        Expression::BinaryExpression(bin) => {
            collect_write_edits_in_expression(&bin.left, source, signals, edits);
            collect_write_edits_in_expression(&bin.right, source, signals, edits);
        }
        Expression::ArrowFunctionExpression(arrow) => {
            collect_write_edits_in_arrow(arrow, source, signals, edits)
        }
        Expression::FunctionExpression(func) => {
            if let Some(body) = &func.body {
                for s in &body.statements {
                    collect_write_edits_in_statement(s, source, signals, edits);
                }
            }
        }
        Expression::AwaitExpression(a) => {
            collect_write_edits_in_expression(&a.argument, source, signals, edits)
        }
        Expression::UnaryExpression(u) => {
            collect_write_edits_in_expression(&u.argument, source, signals, edits)
        }
        _ => {}
    }
}

/// Descend into a function declaration/expression body.
fn collect_write_edits_in_function(
    func: &Function,
    source: &str,
    signals: &HashSet<String>,
    edits: &mut Vec<Edit>,
) {
    if let Some(body) = &func.body {
        for s in &body.statements {
            collect_write_edits_in_statement(s, source, signals, edits);
        }
    }
}

/// Descend into an arrow body — either an expression body or a block of statements.
fn collect_write_edits_in_arrow(
    arrow: &ArrowFunctionExpression,
    source: &str,
    signals: &HashSet<String>,
    edits: &mut Vec<Edit>,
) {
    if arrow.expression {
        if let Some(Statement::ExpressionStatement(stmt)) = arrow.body.statements.first() {
            collect_write_edits_in_expression(&stmt.expression, source, signals, edits);
        }
        return;
    }
    for s in &arrow.body.statements {
        collect_write_edits_in_statement(s, source, signals, edits);
    }
}

/// Rewrite a signal-targeted assignment into the signal mutation API.
///
///   * `x = v`  → `x.set(v)`
///   * `x += n` → `x.update(prev => prev + (n))` (and the other compound operators)
fn rewrite_assignment(
    assign: &AssignmentExpression,
    source: &str,
    signals: &HashSet<String>,
    edits: &mut Vec<Edit>,
) {
    let Some(name) = simple_target_signal_name(&assign.left, signals) else {
        return;
    };

    let rhs_span = oxc_span::GetSpan::span(&assign.right);
    let rhs = source[rhs_span.start as usize..rhs_span.end as usize].trim();

    let span = oxc_span::GetSpan::span(assign);
    let replacement = match assign.operator {
        AssignmentOperator::Assign => format!("{name}.set({rhs})"),
        op => {
            // Compound assignment `x <op>= n` updates the previous value: `x.update(prev => prev <op> (n))`.
            // The binary operator is the compound operator minus the trailing `=`.
            let Some(bin_op) = compound_binary_operator(op) else {
                return;
            };
            format!("{name}.update(prev => prev {bin_op} ({rhs}))")
        }
    };

    edits.push(Edit {
        start: span.start as usize,
        end: span.end as usize,
        text: replacement,
    });
}

/// Rewrite a signal-targeted `++`/`--` (prefix or postfix) into `x.update(prev => prev ± 1)`.
fn rewrite_update(
    update: &UpdateExpression,
    signals: &HashSet<String>,
    edits: &mut Vec<Edit>,
) {
    let SimpleAssignmentTarget::AssignmentTargetIdentifier(id) = &update.argument else {
        return;
    };
    let name = id.name.as_str();
    if !signals.contains(name) {
        return;
    }
    let op = match update.operator {
        UpdateOperator::Increment => "+",
        UpdateOperator::Decrement => "-",
    };
    let span = oxc_span::GetSpan::span(update);
    edits.push(Edit {
        start: span.start as usize,
        end: span.end as usize,
        text: format!("{name}.update(prev => prev {op} 1)"),
    });
}

/// The signal name an assignment target writes to, if the target is a *bare identifier* that names a
/// signal. A member/destructuring target (`obj.x = …`, `x.prop = …`, `[a] = …`) is not a signal
/// write — those mutate something a signal points at, not the signal binding itself.
fn simple_target_signal_name<'a>(
    target: &AssignmentTarget,
    signals: &'a HashSet<String>,
) -> Option<&'a str> {
    let AssignmentTarget::AssignmentTargetIdentifier(id) = target else {
        return None;
    };
    signals.get(id.name.as_str()).map(|s| s.as_str())
}

/// The binary operator string for a compound assignment operator (`+=` → `+`), or `None` for `=`
/// and the logical-assignment operators (`&&=`, `||=`, `??=`) which have no plain-binary `update`
/// form.
fn compound_binary_operator(op: AssignmentOperator) -> Option<&'static str> {
    match op {
        AssignmentOperator::Addition => Some("+"),
        AssignmentOperator::Subtraction => Some("-"),
        AssignmentOperator::Multiplication => Some("*"),
        AssignmentOperator::Division => Some("/"),
        AssignmentOperator::Remainder => Some("%"),
        AssignmentOperator::Exponential => Some("**"),
        AssignmentOperator::ShiftLeft => Some("<<"),
        AssignmentOperator::ShiftRight => Some(">>"),
        AssignmentOperator::ShiftRightZeroFill => Some(">>>"),
        AssignmentOperator::BitwiseOR => Some("|"),
        AssignmentOperator::BitwiseXOR => Some("^"),
        AssignmentOperator::BitwiseAnd => Some("&"),
        // `=`, `&&=`, `||=`, `??=` are handled elsewhere / have no plain-binary update form.
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Edit application + import injection.
// ---------------------------------------------------------------------------

/// Apply byte-range `edits` to `source`, right-to-left so earlier edits never shift later spans.
/// Overlapping edits cannot occur here (declaration inits and statement-level writes are disjoint
/// regions), so a stable sort by start (descending) is sufficient.
fn apply_edits(source: &str, mut edits: Vec<Edit>) -> String {
    if edits.is_empty() {
        return source.to_string();
    }
    edits.sort_by(|a, b| b.start.cmp(&a.start));
    let mut out = source.to_string();
    for edit in edits {
        out.replace_range(edit.start..edit.end, &edit.text);
    }
    out
}

/// The Angular reactive primitives the JSX/React lowering may emit calls to and which must be
/// imported from `@angular/core`. Order is the emission order for a freshly-prepended import.
const CORE_PRIMITIVES: [&str; 5] = ["signal", "computed", "effect", "input", "inject"];

/// Ensure `body` imports every `@angular/core` reactive primitive it references, with EXACTLY ONE
/// merged `@angular/core` import and no duplicate specifier.
///
/// The signals pass wraps declarations into `signal(...)`, and the React pre-pass may already have
/// produced `signal(...)` / `computed(...)` / `effect(...)` / `input(...)` / `inject(...)` calls
/// before this pass runs. Either way, the needed import set is whatever primitives the body now
/// references. This:
///   * scans the body for each primitive used as a bare call (`signal(` — not `.signal(`),
///   * parses the body to find an existing `@angular/core` import and which names it already binds,
///   * MERGES any missing primitive into that existing import (a span edit appended before its `}`),
///     so a hand-written `import { signal } from '@angular/core'` is extended in place rather than
///     duplicated; OR prepends a fresh `import { … } from "@angular/core";` when none exists.
///
/// A primitive already imported (by the author, or by a prior pass) is never re-added. When the body
/// references no primitive, it is returned unchanged.
fn ensure_core_import(body: &str) -> String {
    // 1. Which primitives does the body reference as a bare call?
    let used: Vec<&str> = CORE_PRIMITIVES
        .iter()
        .copied()
        .filter(|name| references_call(body, name))
        .collect();
    if used.is_empty() {
        return body.to_string();
    }

    // 2. Find an existing `@angular/core` import, its already-bound names, and the byte position of
    //    its closing `}` (the merge point). Parsed off the AST, never a regex.
    let existing = find_core_import(body);
    let already: HashSet<&str> = existing
        .as_ref()
        .map(|e| e.bound_names.iter().map(|s| s.as_str()).collect())
        .unwrap_or_default();

    let missing: Vec<&str> = used
        .into_iter()
        .filter(|name| !already.contains(name))
        .collect();
    if missing.is_empty() {
        // Every referenced primitive is already imported — nothing to add.
        return body.to_string();
    }

    match existing {
        // Merge into the existing import: insert `, a, b` just before its closing `}`.
        Some(e) => {
            let insert = format!(", {}", missing.join(", "));
            let mut out = String::with_capacity(body.len() + insert.len());
            out.push_str(&body[..e.insert_at]);
            out.push_str(&insert);
            out.push_str(&body[e.insert_at..]);
            out
        }
        // No existing import: prepend a fresh one with the missing names in canonical order.
        None => {
            let ordered: Vec<&str> = CORE_PRIMITIVES
                .iter()
                .copied()
                .filter(|p| missing.contains(p))
                .collect();
            format!(
                "import {{ {} }} from \"@angular/core\";\n{body}",
                ordered.join(", ")
            )
        }
    }
}

/// A located `import { … } from '@angular/core'` declaration: the names it already binds and the
/// byte offset just before its closing `}` (where a merged specifier list is inserted).
struct CoreImport {
    bound_names: Vec<String>,
    insert_at: usize,
}

/// Locate an existing named `@angular/core` import in `body` via the parsed AST. Returns the bound
/// specifier locals and the byte offset of the position immediately before the import's closing `}`
/// (so a `, name` can be appended into the brace list). A namespace/default-only import, or no
/// `@angular/core` import at all, yields `None` (a fresh import is prepended instead).
fn find_core_import(body: &str) -> Option<CoreImport> {
    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true).with_module(true);
    let ret = JsParser::new(&allocator, body, source_type).parse();
    if !ret.errors.is_empty() {
        return None;
    }

    for stmt in &ret.program.body {
        let Statement::ImportDeclaration(import) = stmt else {
            continue;
        };
        if import.source.value.as_str() != "@angular/core" {
            continue;
        }
        let Some(specifiers) = &import.specifiers else {
            continue;
        };
        let mut bound_names = Vec::new();
        let mut last_named_end: Option<usize> = None;
        for spec in specifiers {
            use oxc_ast::ast::ImportDeclarationSpecifier::*;
            match spec {
                ImportSpecifier(s) => {
                    bound_names.push(s.local.name.to_string());
                    last_named_end = Some(oxc_span::GetSpan::span(s.as_ref()).end as usize);
                }
                ImportDefaultSpecifier(s) => bound_names.push(s.local.name.to_string()),
                ImportNamespaceSpecifier(s) => bound_names.push(s.local.name.to_string()),
            }
        }
        // Only a braced (named) import can be merged into. Insert right after the last named
        // specifier (before the closing `}`), preserving any trailing whitespace/comma the source had.
        if let Some(insert_at) = last_named_end {
            return Some(CoreImport { bound_names, insert_at });
        }
    }
    None
}

/// Whether `body` contains a call to the named function (`name(`), ignoring a member-access
/// `.name(`. Used to decide whether to import `computed` alongside `signal`.
fn references_call(body: &str, name: &str) -> bool {
    let needle = format!("{name}(");
    let mut search_from = 0;
    while let Some(rel) = body[search_from..].find(&needle) {
        let idx = search_from + rel;
        // Reject a member access (`x.computed(`) — only a bare `computed(` is the core helper.
        let preceded_by_dot = idx
            .checked_sub(1)
            .map(|p| body.as_bytes()[p] == b'.')
            .unwrap_or(false);
        // Reject an identifier-char prefix (`mycomputed(`).
        let preceded_by_ident = idx
            .checked_sub(1)
            .map(|p| is_ident_byte(body.as_bytes()[p]))
            .unwrap_or(false);
        if !preceded_by_dot && !preceded_by_ident {
            return true;
        }
        search_from = idx + needle.len();
    }
    false
}

// ---------------------------------------------------------------------------
// Template interpolation auto-call.
// ---------------------------------------------------------------------------

/// Find the byte index of the matching `}}` for an interpolation opened just before `from`, or
/// `None` if the region is unterminated. (Angular interpolations do not nest, so the first `}}`
/// closes the region.)
fn find_interpolation_close(html: &str, from: usize) -> Option<usize> {
    let bytes = html.as_bytes();
    let mut i = from;
    while i + 1 < bytes.len() {
        if bytes[i] == b'}' && bytes[i + 1] == b'}' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Rewrite signal identifier reads inside one interpolation expression to calls.
///
/// Tokenizes the expression into identifiers and everything else. An identifier that (a) names a
/// signal, (b) is not immediately preceded by a `.` (so it is a value, not a property name), and
/// (c) is not immediately followed by a `(` (already a call) gets a `()` appended. String literals
/// are passed through untouched so an identifier-looking word inside a string is never rewritten.
fn auto_call_expression(expr: &str, signals: &HashSet<String>) -> String {
    let bytes = expr.as_bytes();
    let mut out = String::with_capacity(expr.len() + 2);
    let mut i = 0usize;
    // Whether the previous non-whitespace significant char was a `.` (member access) — in which case
    // a following identifier is a property name, not a variable read.
    let mut prev_was_dot = false;

    while i < bytes.len() {
        let b = bytes[i];

        // Skip string literals verbatim so their contents are never treated as identifiers.
        if b == b'"' || b == b'\'' || b == b'`' {
            let quote = b;
            let start = i;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if bytes[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push_str(&expr[start..i.min(expr.len())]);
            prev_was_dot = false;
            continue;
        }

        if is_ident_start_byte(b) {
            let start = i;
            i += 1;
            while i < bytes.len() && is_ident_byte(bytes[i]) {
                i += 1;
            }
            let word = &expr[start..i];

            // Is the next significant char a `(`? Then it is already a call.
            let mut j = i;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            let followed_by_call = j < bytes.len() && bytes[j] == b'(';

            if !prev_was_dot && !followed_by_call && signals.contains(word) {
                out.push_str(word);
                out.push_str("()");
            } else {
                out.push_str(word);
            }
            prev_was_dot = false;
            continue;
        }

        if b.is_ascii_whitespace() {
            out.push(b as char);
            // Whitespace does not change member-access state.
            i += 1;
            continue;
        }

        // A `?.` optional-chaining access also makes a following identifier a property name.
        prev_was_dot = b == b'.';
        let ch_len = utf8_char_len(b);
        out.push_str(&expr[i..i + ch_len]);
        i += ch_len;
    }

    out
}

// ---------------------------------------------------------------------------
// Small byte helpers.
// ---------------------------------------------------------------------------

fn is_ident_start_byte(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b'$'
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// Length in bytes of the UTF-8 character whose leading byte is `b`.
fn utf8_char_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        1
    }
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sig_set(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    // --- Declaration lowering -------------------------------------------------

    #[test]
    fn simple_let_becomes_signal_with_import() {
        let out = transform("let count = 0;");
        assert!(out.signals.contains("count"), "count not recorded as signal");
        assert!(
            out.javascript.contains("count = signal(0)"),
            "init not wrapped; got: {}",
            out.javascript
        );
        assert!(
            out.javascript
                .contains("import { signal } from \"@angular/core\";"),
            "signal import missing; got: {}",
            out.javascript
        );
    }

    #[test]
    fn const_with_various_simple_initializers_wrapped() {
        let out = transform(
            "const a = 'x';\nconst b = [1, 2];\nconst c = { k: 1 };\nconst d = `t${x}`;\nconst e = foo();\nconst f = other;",
        );
        for (name, wrapped) in [
            ("a", "a = signal('x')"),
            ("b", "b = signal([1, 2])"),
            ("c", "c = signal({ k: 1 })"),
            ("d", "d = signal(`t${x}`)"),
            ("e", "e = signal(foo())"),
            ("f", "f = signal(other)"),
        ] {
            assert!(out.signals.contains(name), "{name} not a signal");
            assert!(
                out.javascript.contains(wrapped),
                "{name} not wrapped as {wrapped}; got: {}",
                out.javascript
            );
        }
    }

    #[test]
    fn arrow_and_function_declarations_not_wrapped() {
        let out = transform("const inc = () => {};\nfunction dec() {}\nconst f = function () {};");
        assert!(!out.signals.contains("inc"), "arrow wrongly wrapped");
        assert!(!out.signals.contains("f"), "fn expr wrongly wrapped");
        assert!(
            !out.javascript.contains("signal("),
            "behaviour wrongly wrapped; got: {}",
            out.javascript
        );
        // The arrow body is preserved verbatim.
        assert!(out.javascript.contains("const inc = () => {};"));
    }

    #[test]
    fn destructuring_not_wrapped() {
        let out = transform("const { a, b } = obj;\nconst [c] = xs;");
        assert!(out.signals.is_empty(), "destructuring wrongly wrapped");
        assert!(
            !out.javascript.contains("signal("),
            "destructuring wrongly wrapped; got: {}",
            out.javascript
        );
    }

    #[test]
    fn existing_reactive_primitives_not_double_wrapped() {
        let out = transform(
            "const a = signal(0);\nconst b = computed(() => 1);\nconst c = input();\nconst d = input.required();\nconst e = model('x');\nconst f = output();\nconst g = inject(Svc);\nconst h = viewChild('ref');",
        );
        // None are re-wrapped, so there is no `signal(signal(` etc.
        assert!(
            !out.javascript.contains("signal(signal("),
            "double-wrapped signal; got: {}",
            out.javascript
        );
        assert!(
            !out.javascript.contains("signal(computed("),
            "double-wrapped computed; got: {}",
            out.javascript
        );
        assert!(
            !out.javascript.contains("signal(input"),
            "double-wrapped input; got: {}",
            out.javascript
        );
        assert!(
            !out.javascript.contains("signal(model("),
            "double-wrapped model; got: {}",
            out.javascript
        );
        assert!(
            !out.javascript.contains("signal(output("),
            "double-wrapped output; got: {}",
            out.javascript
        );
        assert!(
            !out.javascript.contains("signal(inject("),
            "double-wrapped inject; got: {}",
            out.javascript
        );
        assert!(
            !out.javascript.contains("signal(viewChild("),
            "double-wrapped viewChild; got: {}",
            out.javascript
        );
        // And none of those names is registered as a (newly) wrapped signal.
        assert!(out.signals.is_empty(), "reactive primitive recorded as new signal");
    }

    #[test]
    fn computed_reference_adds_computed_import() {
        // `count` is a plain value (wrapped), and the body also uses `computed(...)` — so the import
        // pulls in both `signal` and `computed`.
        let out = transform("let count = 0;\nconst doubled = computed(() => count() * 2);");
        assert!(
            out.javascript
                .contains("import { signal, computed } from \"@angular/core\";"),
            "computed not added to import; got: {}",
            out.javascript
        );
        // `doubled` is a reactive primitive, so it is not re-wrapped.
        assert!(!out.signals.contains("doubled"), "computed wrongly wrapped");
    }

    #[test]
    fn no_import_when_nothing_wrapped() {
        let out = transform("const inc = () => {};");
        assert!(
            !out.javascript.contains("import { signal"),
            "import added with no signals; got: {}",
            out.javascript
        );
    }

    // --- Write lowering -------------------------------------------------------

    #[test]
    fn increment_in_handler_becomes_update() {
        let out = transform("let count = 0;\nfunction inc() { count++; }");
        assert!(
            out.javascript.contains("count.update(prev => prev + 1)"),
            "increment not lowered; got: {}",
            out.javascript
        );
    }

    #[test]
    fn prefix_decrement_becomes_update() {
        let out = transform("let count = 0;\nconst dec = () => { --count; };");
        assert!(
            out.javascript.contains("count.update(prev => prev - 1)"),
            "prefix decrement not lowered; got: {}",
            out.javascript
        );
    }

    #[test]
    fn plain_assignment_becomes_set() {
        let out = transform("let name = '';\nconst rename = () => { name = 'x'; };");
        assert!(
            out.javascript.contains("name.set('x')"),
            "assignment not lowered to set; got: {}",
            out.javascript
        );
    }

    #[test]
    fn compound_assignment_becomes_update() {
        let out = transform("let count = 0;\nconst add = (n) => { count += n; };");
        assert!(
            out.javascript.contains("count.update(prev => prev + (n))"),
            "compound assignment not lowered; got: {}",
            out.javascript
        );
    }

    #[test]
    fn write_to_non_signal_is_untouched() {
        // `other` is never declared as a signal in this body, so its write is left alone.
        let out = transform("let count = 0;\nfunction f() { other = 1; other++; }");
        assert!(
            out.javascript.contains("other = 1"),
            "non-signal write wrongly rewritten; got: {}",
            out.javascript
        );
        assert!(
            out.javascript.contains("other++"),
            "non-signal update wrongly rewritten; got: {}",
            out.javascript
        );
    }

    #[test]
    fn member_write_is_not_a_signal_set() {
        // `obj.count = 1` mutates a property, not the signal binding `count`.
        let out = transform("let count = 0;\nfunction f() { obj.count = 1; }");
        assert!(
            out.javascript.contains("obj.count = 1"),
            "member write wrongly rewritten; got: {}",
            out.javascript
        );
        assert!(
            !out.javascript.contains("obj.count.set"),
            "member write wrongly treated as signal; got: {}",
            out.javascript
        );
    }

    // --- Template auto-call ---------------------------------------------------

    #[test]
    fn bare_signal_read_is_auto_called() {
        let signals = sig_set(&["count"]);
        assert_eq!(
            auto_call_template("<div>{{ count }}</div>", &signals),
            "<div>{{ count() }}</div>"
        );
    }

    #[test]
    fn already_called_signal_is_not_double_called() {
        let signals = sig_set(&["count"]);
        assert_eq!(
            auto_call_template("<div>{{ count() }}</div>", &signals),
            "<div>{{ count() }}</div>"
        );
    }

    #[test]
    fn non_signal_identifier_untouched() {
        let signals = sig_set(&["count"]);
        assert_eq!(
            auto_call_template("<div>{{ other }}</div>", &signals),
            "<div>{{ other }}</div>"
        );
    }

    #[test]
    fn signal_as_member_object_is_called() {
        let signals = sig_set(&["user"]);
        assert_eq!(
            auto_call_template("<div>{{ user.name }}</div>", &signals),
            "<div>{{ user().name }}</div>"
        );
    }

    #[test]
    fn signal_as_property_name_untouched() {
        // `count` here is a property of `obj`, not the signal variable.
        let signals = sig_set(&["count"]);
        assert_eq!(
            auto_call_template("<div>{{ obj.count }}</div>", &signals),
            "<div>{{ obj.count }}</div>"
        );
    }

    #[test]
    fn signal_in_expression_is_called() {
        let signals = sig_set(&["count"]);
        assert_eq!(
            auto_call_template("<div>{{ count + 1 }}</div>", &signals),
            "<div>{{ count() + 1 }}</div>"
        );
    }

    #[test]
    fn signal_inside_string_literal_untouched() {
        let signals = sig_set(&["count"]);
        assert_eq!(
            auto_call_template("<div>{{ 'count' }}</div>", &signals),
            "<div>{{ 'count' }}</div>"
        );
    }

    #[test]
    fn markup_outside_interpolation_untouched() {
        let signals = sig_set(&["count"]);
        // `count` as an attribute name / tag is not an interpolation read.
        assert_eq!(
            auto_call_template("<div count=\"x\">text</div>", &signals),
            "<div count=\"x\">text</div>"
        );
    }
}
