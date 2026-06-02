//! React-compat lowering for the JSX authoring front-end.
//!
//! Treaty's JSX front-end authors *Angular* components in a JSX-flavoured syntax, with
//! signals-by-default and the hooks-shaped reactive primitives Angular ships
//! (`signal`/`computed`/`effect`/`input`/`inject`). A plain **React** component — `useState`,
//! `useEffect`, `useMemo`, `useCallback`, plus a `props` parameter — is *almost* the same shape, so
//! this module lowers the React idioms to their Angular equivalents BEFORE the signals-by-default
//! pass ([`super::signals`]) runs:
//!
//!   * `import … from 'react' / 'react-dom'`        → deleted (Angular provides the primitives)
//!   * `const [x, setX] = useState(INIT)`           → `const x = signal(INIT)` (x recorded as a signal)
//!   * `setX(v)`                                     → `x.set(v)` (or `x.update(fn)` for a functional updater)
//!   * `useEffect(fn, deps?)`                        → `effect(fn)` (deps dropped — `effect` auto-tracks)
//!   * `const m = useMemo(fn, deps?)`               → `const m = computed(fn)` (m recorded as a signal)
//!   * `const c = useCallback(fn, deps?)`           → `const c = fn`
//!   * `const r = useRef(v)`                         → `const r = signal(v)` (`.current` reads → calls)
//!   * `const v = useContext(C)`                     → `const v = inject(C)`
//!
//! `useReducer` is not mechanically lowerable to a single Angular primitive, so it is left in place
//! with a diagnostic rather than mis-compiled (the "don't block, surface the gap" contract).
//!
//! Everything is a span-based edit collected over the OXC AST and applied right-to-left (the same
//! discipline as [`super::signals`]), so unrelated code is preserved byte-for-byte. A parse failure
//! is non-fatal: the body is returned unchanged. The discovered signal names are returned so
//! [`super`] can merge them into the template auto-call set, and the `@angular/core` import for the
//! emitted primitives is injected by [`super::signals`] (which runs next and owns the single merged
//! import), so this pass never writes the import itself.

use std::collections::HashSet;

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, Declaration, Expression, ExportDefaultDeclarationKind, Function, Program, Statement,
    VariableDeclarator,
};
use oxc_parser::Parser as JsParser;
use oxc_span::{GetSpan, SourceType};

/// The result of the React-compat lowering.
pub struct ReactTransform {
    /// The component-body JavaScript with React idioms lowered to Angular primitives.
    pub javascript: String,
    /// The names that became signals (a `useState` state var, a `useMemo`/`useRef` binding) — merged
    /// by [`super`] into the template auto-call candidate set so a bare `{{ x }}` read auto-calls.
    pub signals: HashSet<String>,
    /// Non-fatal diagnostics (an un-lowerable `useReducer`, an opaque `useState` destructure, …).
    pub diagnostics: Vec<String>,
}

/// A single byte-range replacement, applied right-to-left so earlier edits never shift later spans.
struct Edit {
    start: usize,
    end: usize,
    text: String,
}

/// The React hook identifiers whose presence (as a call) marks a body as React-mode.
const REACT_HOOKS: [&str; 7] = [
    "useState",
    "useEffect",
    "useMemo",
    "useRef",
    "useCallback",
    "useContext",
    "useReducer",
];

/// Whether the already-parsed `program` is a React component: it imports from `react`/`react-dom`,
/// OR it calls any of the recognized React hooks. Reuses the front-end's existing JSX-aware parse —
/// it never re-parses.
pub fn detect_react_mode(program: &Program) -> bool {
    for stmt in &program.body {
        if let Statement::ImportDeclaration(import) = stmt {
            let src = import.source.value.as_str();
            if src == "react" || src == "react-dom" {
                return true;
            }
        }
    }
    let mut found = false;
    for stmt in &program.body {
        if found {
            break;
        }
        scan_for_hook_call(stmt, &mut found);
    }
    found
}

/// Recurse a statement looking for any `useX(...)` hook call.
fn scan_for_hook_call(stmt: &Statement, found: &mut bool) {
    if *found {
        return;
    }
    // A cheap structural walk over the common nesting (declarations / function bodies / expression
    // statements) is enough to spot a hook call in a component body; we only need a boolean.
    match stmt {
        Statement::VariableDeclaration(decl) => {
            for d in &decl.declarations {
                if let Some(init) = &d.init {
                    scan_expr_for_hook(init, found);
                }
            }
        }
        Statement::ExpressionStatement(s) => scan_expr_for_hook(&s.expression, found),
        Statement::FunctionDeclaration(func) => scan_fn_for_hook(func, found),
        Statement::ReturnStatement(s) => {
            if let Some(arg) = &s.argument {
                scan_expr_for_hook(arg, found);
            }
        }
        Statement::ExportDefaultDeclaration(export) => match &export.declaration {
            ExportDefaultDeclarationKind::FunctionDeclaration(func) => scan_fn_for_hook(func, found),
            ExportDefaultDeclarationKind::ArrowFunctionExpression(arrow) => {
                for s in &arrow.body.statements {
                    scan_for_hook_call(s, found);
                }
            }
            _ => {}
        },
        Statement::ExportNamedDeclaration(export) => {
            if let Some(decl) = &export.declaration {
                match decl {
                    Declaration::FunctionDeclaration(func) => scan_fn_for_hook(func, found),
                    Declaration::VariableDeclaration(var) => {
                        for d in &var.declarations {
                            if let Some(init) = &d.init {
                                scan_expr_for_hook(init, found);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        Statement::BlockStatement(b) => {
            for s in &b.body {
                scan_for_hook_call(s, found);
            }
        }
        _ => {}
    }
}

fn scan_fn_for_hook(func: &Function, found: &mut bool) {
    if let Some(body) = &func.body {
        for s in &body.statements {
            scan_for_hook_call(s, found);
        }
    }
}

fn scan_expr_for_hook(expr: &Expression, found: &mut bool) {
    if *found {
        return;
    }
    match expr {
        Expression::CallExpression(call) => {
            if let Expression::Identifier(id) = &call.callee {
                if REACT_HOOKS.contains(&id.name.as_str()) {
                    *found = true;
                    return;
                }
            }
            // Descend into a hook used as a call argument / member object.
            scan_expr_for_hook(&call.callee, found);
            for arg in &call.arguments {
                if let Some(e) = arg.as_expression() {
                    scan_expr_for_hook(e, found);
                }
            }
        }
        Expression::ArrowFunctionExpression(arrow) => {
            for s in &arrow.body.statements {
                scan_for_hook_call(s, found);
            }
        }
        Expression::ParenthesizedExpression(p) => scan_expr_for_hook(&p.expression, found),
        _ => {}
    }
}

/// Lower the React idioms in a component-body JS chunk to Angular primitives.
///
/// Parse failures are non-fatal: an unparseable chunk is returned unchanged with no signals and no
/// diagnostics (the body is the author's free-form code, and the backend reports any real syntax
/// errors). The caller is expected to have established React-mode via [`detect_react_mode`] over the
/// *whole* program; this routine only performs the rewrites and is safe to run unconditionally (it is
/// a no-op on a body with no React idioms).
pub fn transform(javascript: &str) -> ReactTransform {
    let empty = || ReactTransform {
        javascript: javascript.to_string(),
        signals: HashSet::new(),
        diagnostics: Vec::new(),
    };
    if javascript.trim().is_empty() {
        return empty();
    }

    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true);
    let ret = JsParser::new(&allocator, javascript, source_type).parse();
    if !ret.errors.is_empty() {
        return empty();
    }

    let mut signals: HashSet<String> = HashSet::new();
    let mut setters: Vec<(String, String)> = Vec::new(); // (setterName, signalName)
    let mut refs: HashSet<String> = HashSet::new(); // `useRef` locals whose `.current` reads → calls
    let mut diagnostics: Vec<String> = Vec::new();
    let mut edits: Vec<Edit> = Vec::new();

    // Pass 1: delete react imports + lower the hook declarations / `useEffect` statements. The
    // component body may be at the module top level (the `.treaty`-shaped flat body the JSX assembler
    // produces) — so we walk the top-level statements directly.
    for stmt in &ret.program.body {
        lower_statement(
            stmt,
            javascript,
            &mut signals,
            &mut setters,
            &mut refs,
            &mut diagnostics,
            &mut edits,
        );
    }

    // Pass 2: rewrite every `setX(arg)` call to the signal mutation API, and every `ref.current`
    // read to a call. The setter/ref sets must be complete before this pass (a setter may be called
    // textually before its `useState` declaration inside a handler), so it runs after pass 1.
    for stmt in &ret.program.body {
        rewrite_uses_in_statement(stmt, javascript, &setters, &refs, &mut edits);
    }

    let javascript = apply_edits(javascript, edits);
    ReactTransform {
        javascript,
        signals,
        diagnostics,
    }
}

// ---------------------------------------------------------------------------
// Pass 1: declaration / statement lowering.
// ---------------------------------------------------------------------------

/// Lower a single component-scope statement. Handles the module-top-level flat body (the JSX
/// assembler's shape) plus the still-wrapped function/arrow component forms (so the pass is robust if
/// run before assembly).
fn lower_statement(
    stmt: &Statement,
    source: &str,
    signals: &mut HashSet<String>,
    setters: &mut Vec<(String, String)>,
    refs: &mut HashSet<String>,
    diagnostics: &mut Vec<String>,
    edits: &mut Vec<Edit>,
) {
    match stmt {
        // `import … from 'react' / 'react-dom'` → deleted.
        Statement::ImportDeclaration(import) => {
            let src = import.source.value.as_str();
            if src == "react" || src == "react-dom" {
                let span = import.span;
                edits.push(Edit {
                    start: span.start as usize,
                    end: span.end as usize,
                    text: String::new(),
                });
            }
        }
        Statement::VariableDeclaration(decl) => {
            for declarator in &decl.declarations {
                lower_declarator(declarator, source, signals, setters, refs, diagnostics, edits);
            }
        }
        // A bare `useEffect(fn, deps)` statement → `effect(fn)`.
        Statement::ExpressionStatement(s) => {
            lower_effect_call(&s.expression, source, edits);
        }
        // Descend into a still-wrapped component function/arrow so the pass also works pre-assembly.
        Statement::FunctionDeclaration(func) => {
            descend_fn(func, source, signals, setters, refs, diagnostics, edits);
        }
        Statement::ExportDefaultDeclaration(export) => match &export.declaration {
            ExportDefaultDeclarationKind::FunctionDeclaration(func) => {
                descend_fn(func, source, signals, setters, refs, diagnostics, edits);
            }
            ExportDefaultDeclarationKind::ArrowFunctionExpression(arrow) => {
                for s in &arrow.body.statements {
                    lower_statement(s, source, signals, setters, refs, diagnostics, edits);
                }
            }
            _ => {}
        },
        Statement::ExportNamedDeclaration(export) => {
            if let Some(decl) = &export.declaration {
                match decl {
                    Declaration::FunctionDeclaration(func) => {
                        descend_fn(func, source, signals, setters, refs, diagnostics, edits);
                    }
                    Declaration::VariableDeclaration(var) => {
                        for d in &var.declarations {
                            lower_declarator(d, source, signals, setters, refs, diagnostics, edits);
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn descend_fn(
    func: &Function,
    source: &str,
    signals: &mut HashSet<String>,
    setters: &mut Vec<(String, String)>,
    refs: &mut HashSet<String>,
    diagnostics: &mut Vec<String>,
    edits: &mut Vec<Edit>,
) {
    if let Some(body) = &func.body {
        for s in &body.statements {
            lower_statement(s, source, signals, setters, refs, diagnostics, edits);
        }
    }
}

/// Lower a single declarator if it is a recognized React hook declaration.
fn lower_declarator(
    declarator: &VariableDeclarator,
    source: &str,
    signals: &mut HashSet<String>,
    setters: &mut Vec<(String, String)>,
    refs: &mut HashSet<String>,
    diagnostics: &mut Vec<String>,
    edits: &mut Vec<Edit>,
) {
    let Some(init) = &declarator.init else {
        return;
    };
    let Some((hook, call)) = hook_call(init) else {
        return;
    };

    match hook {
        // `const [x, setX] = useState(INIT)` → `const x = signal(INIT)`.
        "useState" => {
            // The binding must be a 2-element array pattern `[state, setter]`.
            let pat = &declarator.id;
            let oxc_ast::ast::BindingPattern::ArrayPattern(arr) = pat else {
                diagnostics.push(
                    "react: `useState` must destructure to `[state, setState]`; left unchanged"
                        .to_string(),
                );
                return;
            };
            let state_name = arr
                .elements
                .first()
                .and_then(|e| e.as_ref())
                .and_then(binding_ident_name);
            let setter_name = arr
                .elements
                .get(1)
                .and_then(|e| e.as_ref())
                .and_then(binding_ident_name);
            let (Some(state_name), Some(setter_name)) = (state_name, setter_name) else {
                diagnostics.push(
                    "react: `useState` destructure is not a simple `[state, setState]` pair; left unchanged"
                        .to_string(),
                );
                return;
            };

            // The initial value is the first arg of `useState(INIT)` (absent → `undefined`).
            let init_text = first_arg_text(call, source).unwrap_or("undefined");
            // Replace the WHOLE declarator (`[x, setX] = useState(0)`) with `x = signal(0)`.
            let span = declarator.span;
            edits.push(Edit {
                start: span.start as usize,
                end: span.end as usize,
                text: format!("{state_name} = signal({init_text})"),
            });
            signals.insert(state_name.clone());
            setters.push((setter_name, state_name));
        }
        // `const m = useMemo(fn, deps?)` → `const m = computed(fn)`.
        "useMemo" => {
            if let Some(name) = binding_ident_name(&declarator.id) {
                if let Some(fn_text) = first_arg_text(call, source) {
                    replace_init(init, &format!("computed({fn_text})"), edits);
                    signals.insert(name);
                }
            }
        }
        // `const c = useCallback(fn, deps?)` → `const c = fn` (behaviour, not state).
        "useCallback" => {
            if let Some(fn_text) = first_arg_text(call, source) {
                replace_init(init, fn_text, edits);
            }
        }
        // `const r = useRef(v)` → `const r = signal(v)`; `.current` reads are rewritten to calls.
        "useRef" => {
            if let Some(name) = binding_ident_name(&declarator.id) {
                let v = first_arg_text(call, source).unwrap_or("undefined");
                replace_init(init, &format!("signal({v})"), edits);
                signals.insert(name.clone());
                refs.insert(name);
            }
        }
        // `const v = useContext(C)` → `const v = inject(C)`.
        "useContext" => {
            if let Some(arg) = first_arg_text(call, source) {
                replace_init(init, &format!("inject({arg})"), edits);
            }
        }
        // `useReducer` has no single-primitive Angular equivalent — surface a diagnostic, leave it.
        "useReducer" => {
            diagnostics.push(
                "react: `useReducer` has no direct Angular primitive; left unchanged. Model it as a \
                 `signal` plus a reducer function, or a small store. (TODO: mechanical lowering.)"
                    .to_string(),
            );
        }
        // A `useEffect` written as a declaration init is unusual; the statement form is handled
        // separately. Ignore here.
        _ => {}
    }
}

/// If `expr` is a `useEffect(fn, deps?)` call, rewrite it to `effect(fn)` (the deps array is dropped:
/// Angular's `effect` auto-tracks the signals its `fn` reads).
fn lower_effect_call(expr: &Expression, source: &str, edits: &mut Vec<Edit>) {
    let Some((hook, call)) = hook_call(expr) else {
        return;
    };
    if hook != "useEffect" {
        return;
    }
    let Some(fn_text) = first_arg_text(call, source) else {
        return;
    };
    let span = expr.span();
    edits.push(Edit {
        start: span.start as usize,
        end: span.end as usize,
        text: format!("effect({fn_text})"),
    });
}

// ---------------------------------------------------------------------------
// Pass 2: setter-call + ref-read rewriting.
// ---------------------------------------------------------------------------

/// Recurse a statement rewriting `setX(arg)` setter calls and `ref.current` reads.
fn rewrite_uses_in_statement(
    stmt: &Statement,
    source: &str,
    setters: &[(String, String)],
    refs: &HashSet<String>,
    edits: &mut Vec<Edit>,
) {
    match stmt {
        Statement::ExpressionStatement(s) => {
            rewrite_uses_in_expression(&s.expression, source, setters, refs, edits)
        }
        Statement::VariableDeclaration(decl) => {
            for d in &decl.declarations {
                if let Some(init) = &d.init {
                    rewrite_uses_in_expression(init, source, setters, refs, edits);
                }
            }
        }
        Statement::ReturnStatement(s) => {
            if let Some(arg) = &s.argument {
                rewrite_uses_in_expression(arg, source, setters, refs, edits);
            }
        }
        Statement::BlockStatement(b) => {
            for s in &b.body {
                rewrite_uses_in_statement(s, source, setters, refs, edits);
            }
        }
        Statement::IfStatement(s) => {
            rewrite_uses_in_expression(&s.test, source, setters, refs, edits);
            rewrite_uses_in_statement(&s.consequent, source, setters, refs, edits);
            if let Some(alt) = &s.alternate {
                rewrite_uses_in_statement(alt, source, setters, refs, edits);
            }
        }
        Statement::ForStatement(s) => {
            if let Some(test) = &s.test {
                rewrite_uses_in_expression(test, source, setters, refs, edits);
            }
            if let Some(update) = &s.update {
                rewrite_uses_in_expression(update, source, setters, refs, edits);
            }
            rewrite_uses_in_statement(&s.body, source, setters, refs, edits);
        }
        Statement::ForOfStatement(s) => {
            rewrite_uses_in_expression(&s.right, source, setters, refs, edits);
            rewrite_uses_in_statement(&s.body, source, setters, refs, edits);
        }
        Statement::WhileStatement(s) => {
            rewrite_uses_in_expression(&s.test, source, setters, refs, edits);
            rewrite_uses_in_statement(&s.body, source, setters, refs, edits);
        }
        Statement::SwitchStatement(s) => {
            rewrite_uses_in_expression(&s.discriminant, source, setters, refs, edits);
            for case in &s.cases {
                for s in &case.consequent {
                    rewrite_uses_in_statement(s, source, setters, refs, edits);
                }
            }
        }
        Statement::TryStatement(s) => {
            for s in &s.block.body {
                rewrite_uses_in_statement(s, source, setters, refs, edits);
            }
            if let Some(handler) = &s.handler {
                for s in &handler.body.body {
                    rewrite_uses_in_statement(s, source, setters, refs, edits);
                }
            }
            if let Some(finalizer) = &s.finalizer {
                for s in &finalizer.body {
                    rewrite_uses_in_statement(s, source, setters, refs, edits);
                }
            }
        }
        Statement::FunctionDeclaration(func) => {
            if let Some(body) = &func.body {
                for s in &body.statements {
                    rewrite_uses_in_statement(s, source, setters, refs, edits);
                }
            }
        }
        Statement::ExportDefaultDeclaration(export) => match &export.declaration {
            ExportDefaultDeclarationKind::FunctionDeclaration(func) => {
                if let Some(body) = &func.body {
                    for s in &body.statements {
                        rewrite_uses_in_statement(s, source, setters, refs, edits);
                    }
                }
            }
            ExportDefaultDeclarationKind::ArrowFunctionExpression(arrow) => {
                for s in &arrow.body.statements {
                    rewrite_uses_in_statement(s, source, setters, refs, edits);
                }
            }
            _ => {}
        },
        Statement::ExportNamedDeclaration(export) => {
            if let Some(decl) = &export.declaration {
                match decl {
                    Declaration::FunctionDeclaration(func) => {
                        if let Some(body) = &func.body {
                            for s in &body.statements {
                                rewrite_uses_in_statement(s, source, setters, refs, edits);
                            }
                        }
                    }
                    Declaration::VariableDeclaration(var) => {
                        for d in &var.declarations {
                            if let Some(init) = &d.init {
                                rewrite_uses_in_expression(init, source, setters, refs, edits);
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

/// Recurse an expression rewriting `setX(arg)` setter calls and `ref.current` reads.
fn rewrite_uses_in_expression(
    expr: &Expression,
    source: &str,
    setters: &[(String, String)],
    refs: &HashSet<String>,
    edits: &mut Vec<Edit>,
) {
    match expr {
        Expression::CallExpression(call) => {
            // Is this a `setX(arg)` call on a recognized setter?
            if let Expression::Identifier(id) = &call.callee {
                if let Some((_, signal_name)) =
                    setters.iter().find(|(setter, _)| setter == id.name.as_str())
                {
                    rewrite_setter_call(call, signal_name, source, edits);
                    // Still descend into the argument (it may hold further setter calls / ref reads).
                }
            }
            rewrite_uses_in_expression(&call.callee, source, setters, refs, edits);
            for arg in &call.arguments {
                if let Argument::SpreadElement(s) = arg {
                    rewrite_uses_in_expression(&s.argument, source, setters, refs, edits);
                } else if let Some(e) = arg.as_expression() {
                    rewrite_uses_in_expression(e, source, setters, refs, edits);
                }
            }
        }
        // `ref.current` → `ref()` (the signal read). Only the exact `<ref>.current` member access.
        Expression::StaticMemberExpression(member) => {
            if member.property.name.as_str() == "current" {
                if let Expression::Identifier(obj) = &member.object {
                    if refs.contains(obj.name.as_str()) {
                        let span = member.span;
                        edits.push(Edit {
                            start: span.start as usize,
                            end: span.end as usize,
                            text: format!("{}()", obj.name),
                        });
                        return;
                    }
                }
            }
            rewrite_uses_in_expression(&member.object, source, setters, refs, edits);
        }
        Expression::ComputedMemberExpression(member) => {
            rewrite_uses_in_expression(&member.object, source, setters, refs, edits);
            rewrite_uses_in_expression(&member.expression, source, setters, refs, edits);
        }
        Expression::ParenthesizedExpression(p) => {
            rewrite_uses_in_expression(&p.expression, source, setters, refs, edits)
        }
        Expression::SequenceExpression(seq) => {
            for e in &seq.expressions {
                rewrite_uses_in_expression(e, source, setters, refs, edits);
            }
        }
        Expression::AssignmentExpression(assign) => {
            rewrite_uses_in_expression(&assign.right, source, setters, refs, edits)
        }
        Expression::ConditionalExpression(cond) => {
            rewrite_uses_in_expression(&cond.test, source, setters, refs, edits);
            rewrite_uses_in_expression(&cond.consequent, source, setters, refs, edits);
            rewrite_uses_in_expression(&cond.alternate, source, setters, refs, edits);
        }
        Expression::LogicalExpression(logical) => {
            rewrite_uses_in_expression(&logical.left, source, setters, refs, edits);
            rewrite_uses_in_expression(&logical.right, source, setters, refs, edits);
        }
        Expression::BinaryExpression(bin) => {
            rewrite_uses_in_expression(&bin.left, source, setters, refs, edits);
            rewrite_uses_in_expression(&bin.right, source, setters, refs, edits);
        }
        Expression::UnaryExpression(u) => {
            rewrite_uses_in_expression(&u.argument, source, setters, refs, edits)
        }
        Expression::AwaitExpression(a) => {
            rewrite_uses_in_expression(&a.argument, source, setters, refs, edits)
        }
        Expression::ArrowFunctionExpression(arrow) => {
            for s in &arrow.body.statements {
                rewrite_uses_in_statement(s, source, setters, refs, edits);
            }
        }
        Expression::FunctionExpression(func) => {
            if let Some(body) = &func.body {
                for s in &body.statements {
                    rewrite_uses_in_statement(s, source, setters, refs, edits);
                }
            }
        }
        Expression::ObjectExpression(obj) => {
            for prop in &obj.properties {
                if let oxc_ast::ast::ObjectPropertyKind::ObjectProperty(p) = prop {
                    rewrite_uses_in_expression(&p.value, source, setters, refs, edits);
                }
            }
        }
        Expression::ArrayExpression(arr) => {
            for el in &arr.elements {
                if let Some(e) = el.as_expression() {
                    rewrite_uses_in_expression(e, source, setters, refs, edits);
                }
            }
        }
        Expression::TemplateLiteral(t) => {
            for e in &t.expressions {
                rewrite_uses_in_expression(e, source, setters, refs, edits);
            }
        }
        _ => {}
    }
}

/// Rewrite one setter call `setX(arg)` to the signal mutation API:
///   * a functional updater (`setX(prev => …)` / `setX(function (prev) {…})`) → `x.update(arg)`
///   * any other argument                                                     → `x.set(arg)`
///
/// A zero-arg `setX()` resets to `undefined` via `x.set(undefined)` (a faithful React semantic).
fn rewrite_setter_call(
    call: &oxc_ast::ast::CallExpression,
    signal_name: &str,
    source: &str,
    edits: &mut Vec<Edit>,
) {
    let span = call.span;
    let (method, arg_text) = match call.arguments.first().and_then(|a| a.as_expression()) {
        Some(arg) => {
            let is_updater = matches!(
                arg,
                Expression::ArrowFunctionExpression(_) | Expression::FunctionExpression(_)
            );
            let arg_span = arg.span();
            let text = source[arg_span.start as usize..arg_span.end as usize].trim().to_string();
            (if is_updater { "update" } else { "set" }, text)
        }
        None => ("set", "undefined".to_string()),
    };
    edits.push(Edit {
        start: span.start as usize,
        end: span.end as usize,
        text: format!("{signal_name}.{method}({arg_text})"),
    });
}

// ---------------------------------------------------------------------------
// Small helpers.
// ---------------------------------------------------------------------------

/// If `expr` is a direct call `useX(...)` to one of the recognized React hooks, return the hook name
/// and the call expression. (`React.useState(...)` member-call form is intentionally not matched —
/// the `react` import is removed, so the namespace form would not resolve anyway; the bare-call form
/// is what the front-end authors.)
fn hook_call<'a>(expr: &'a Expression<'a>) -> Option<(&'a str, &'a oxc_ast::ast::CallExpression<'a>)> {
    let Expression::CallExpression(call) = expr else {
        return None;
    };
    let Expression::Identifier(id) = &call.callee else {
        return None;
    };
    let name = id.name.as_str();
    if REACT_HOOKS.contains(&name) {
        Some((name, call))
    } else {
        None
    }
}

/// The verbatim source text of a call's first argument, trimmed, or `None` when the call has no
/// arguments (or the first is a spread).
fn first_arg_text<'a>(call: &oxc_ast::ast::CallExpression<'a>, source: &'a str) -> Option<&'a str> {
    let arg = call.arguments.first()?.as_expression()?;
    let span = arg.span();
    Some(source[span.start as usize..span.end as usize].trim())
}

/// The single identifier name bound by a binding pattern (`x`), or `None` for a destructuring /
/// defaulted / rest pattern. Uses OXC's own `get_identifier_name`, which returns `Some` only for a
/// plain `BindingIdentifier`.
fn binding_ident_name(pat: &oxc_ast::ast::BindingPattern) -> Option<String> {
    pat.get_identifier_name().map(|n| n.to_string())
}

/// Queue an edit replacing a declarator's initializer expression with `text`.
fn replace_init(init: &Expression, text: &str, edits: &mut Vec<Edit>) {
    let span = init.span();
    edits.push(Edit {
        start: span.start as usize,
        end: span.end as usize,
        text: text.to_string(),
    });
}

/// Apply byte-range `edits` to `source`, right-to-left so earlier edits never shift later spans.
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

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse `body` as a React component program and return whether it is detected as React-mode.
    fn detect(body: &str) -> bool {
        let allocator = Allocator::default();
        let ret = JsParser::new(&allocator, body, SourceType::tsx()).parse();
        assert!(ret.errors.is_empty(), "test source did not parse: {body}");
        detect_react_mode(&ret.program)
    }

    // --- detection ------------------------------------------------------------

    #[test]
    fn detects_react_import() {
        assert!(detect("import React from 'react';\nconst x = 1;"));
        assert!(detect("import { useState } from 'react';\nconst x = 1;"));
        assert!(detect("import { createRoot } from 'react-dom';\nconst x = 1;"));
    }

    #[test]
    fn detects_hook_call_without_import() {
        assert!(detect("function App() { const [n, setN] = useState(0); return <i/>; }"));
        assert!(detect("function App() { useEffect(() => {}, []); return <i/>; }"));
    }

    #[test]
    fn non_react_body_is_not_react_mode() {
        assert!(!detect("import { signal } from '@angular/core';\nconst n = signal(0);"));
        assert!(!detect("function App() { const n = 0; return <i/>; }"));
    }

    // --- useState -------------------------------------------------------------

    #[test]
    fn use_state_becomes_signal_and_records_setter() {
        let out = transform("const [count, setCount] = useState(0);");
        assert_eq!(out.javascript, "const count = signal(0);");
        assert!(out.signals.contains("count"), "count not recorded as a signal");
    }

    #[test]
    fn use_state_setter_call_becomes_set() {
        let out = transform(
            "const [count, setCount] = useState(0);\nfunction inc() { setCount(count + 1); }",
        );
        assert!(
            out.javascript.contains("count.set(count + 1)"),
            "setter call not lowered to .set; got: {}",
            out.javascript
        );
    }

    #[test]
    fn use_state_functional_updater_becomes_update() {
        let out = transform(
            "const [count, setCount] = useState(0);\nfunction inc() { setCount(prev => prev + 1); }",
        );
        assert!(
            out.javascript.contains("count.update(prev => prev + 1)"),
            "functional updater not lowered to .update; got: {}",
            out.javascript
        );
    }

    #[test]
    fn use_state_with_object_initial_value() {
        let out = transform("const [user, setUser] = useState({ name: 'a' });");
        assert!(
            out.javascript.contains("const user = signal({ name: 'a' })"),
            "object init not preserved; got: {}",
            out.javascript
        );
    }

    // --- useEffect ------------------------------------------------------------

    #[test]
    fn use_effect_drops_deps_and_becomes_effect() {
        let out = transform("useEffect(() => { doThing(); }, [dep]);");
        assert_eq!(out.javascript, "effect(() => { doThing(); });");
    }

    #[test]
    fn use_effect_without_deps_becomes_effect() {
        let out = transform("useEffect(() => { doThing(); });");
        assert_eq!(out.javascript, "effect(() => { doThing(); });");
    }

    // --- useMemo / useCallback ------------------------------------------------

    #[test]
    fn use_memo_becomes_computed_and_is_a_signal() {
        let out = transform("const doubled = useMemo(() => count() * 2, [count]);");
        assert!(
            out.javascript.contains("const doubled = computed(() => count() * 2)"),
            "useMemo not lowered to computed; got: {}",
            out.javascript
        );
        assert!(out.signals.contains("doubled"), "computed memo not recorded as a signal");
    }

    #[test]
    fn use_callback_unwraps_to_its_function() {
        let out = transform("const onClick = useCallback(() => fire(), [fire]);");
        assert_eq!(out.javascript, "const onClick = () => fire();");
        assert!(
            !out.signals.contains("onClick"),
            "a callback is behaviour, not a signal"
        );
    }

    // --- useRef / useContext --------------------------------------------------

    #[test]
    fn use_ref_becomes_signal_and_current_reads_become_calls() {
        let out = transform(
            "const r = useRef(0);\nfunction read() { return r.current; }\nfunction bump() { r.current = 1; }",
        );
        assert!(
            out.javascript.contains("const r = signal(0)"),
            "useRef not lowered to signal; got: {}",
            out.javascript
        );
        // `.current` reads become signal calls.
        assert!(
            out.javascript.contains("return r()"),
            "`.current` read not rewritten to a call; got: {}",
            out.javascript
        );
        assert!(out.signals.contains("r"), "useRef binding not recorded as signal");
    }

    #[test]
    fn use_context_becomes_inject() {
        let out = transform("const theme = useContext(ThemeContext);");
        assert_eq!(out.javascript, "const theme = inject(ThemeContext);");
    }

    // --- react imports + useReducer ------------------------------------------

    #[test]
    fn react_imports_are_deleted() {
        let out = transform("import React, { useState } from 'react';\nconst [n, setN] = useState(1);");
        assert!(
            !out.javascript.contains("from 'react'"),
            "react import not deleted; got: {}",
            out.javascript
        );
        assert!(out.javascript.contains("const n = signal(1)"));
    }

    #[test]
    fn react_dom_import_is_deleted() {
        let out = transform("import { render } from 'react-dom';\nconst x = 1;");
        assert!(
            !out.javascript.contains("react-dom"),
            "react-dom import not deleted; got: {}",
            out.javascript
        );
    }

    #[test]
    fn use_reducer_is_left_with_a_diagnostic() {
        let out = transform("const [state, dispatch] = useReducer(reducer, initial);");
        // Not mechanically lowered (still references useReducer), but a diagnostic is surfaced.
        assert!(
            out.diagnostics.iter().any(|d| d.contains("useReducer")),
            "no useReducer diagnostic; got: {:?}",
            out.diagnostics
        );
    }

    #[test]
    fn parse_failure_leaves_body_unchanged() {
        let broken = "const [x, = useState(";
        let out = transform(broken);
        assert_eq!(out.javascript, broken, "broken body must be returned unchanged");
        assert!(out.signals.is_empty());
    }

    #[test]
    fn non_react_body_is_a_noop() {
        // A body with no React idiom is returned byte-identically (the pass is safe to run always).
        let body = "const n = signal(0);\nconst inc = () => n.update(p => p + 1);";
        let out = transform(body);
        assert_eq!(out.javascript, body);
    }

    #[test]
    fn multiple_use_state_all_lower() {
        let out = transform(
            "const [a, setA] = useState(1);\nconst [b, setB] = useState('x');\nfunction f() { setA(2); setB('y'); }",
        );
        assert!(out.javascript.contains("const a = signal(1)"));
        assert!(out.javascript.contains("const b = signal('x')"));
        assert!(out.javascript.contains("a.set(2)"));
        assert!(out.javascript.contains("b.set('y')"));
        assert!(out.signals.contains("a") && out.signals.contains("b"));
    }
}
