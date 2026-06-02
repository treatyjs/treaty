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
    /// The discovered `useState` setters as `(setterName, signalName)` pairs (`setCount` → `count`).
    /// Exposed so the template's inline-arrow event handlers — lowered to Angular `(event)="…"`
    /// actions BEFORE this pass ran — can have their `setX(…)` calls rewritten to `x.set/x.update`
    /// with the SAME setter machinery the body uses (see [`rewrite_template_handlers`]).
    pub setters: Vec<(String, String)>,
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
        setters: Vec::new(),
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
        setters,
        diagnostics,
    }
}

/// Auto-call signal-typed name READS inside the React component **body** code.
///
/// In React-compat mode a `useState` var, a `useMemo` computed, a `useRef` signal, and a props-as-
/// `input()` name are all Angular signals — i.e. *functions*. A body expression that reads one of
/// them by bare name (`count * 2`, `console.log(count)`, `prev + step`) would otherwise operate on
/// the signal function itself (`count * 2` → `NaN`; an `effect` that reads `count` never re-runs
/// because it logs the function and tracks nothing). This rewrites each such read `count` → `count()`
/// so the body reads the VALUE and the surrounding `computed`/`effect`/callback/handler tracks it.
///
/// This is the body-code counterpart to [`super::signals::auto_call_template`] (which only auto-calls
/// template interpolations) and MUST run only in React mode — the plain `.tsx`/`.treaty` signals path
/// deliberately does NOT auto-call body reads, and the matchGolden corpus depends on that behaviour.
///
/// It is AST-aware (not a textual tokenizer like the template pass) because a component body is full
/// JS: it must never call a property key (`obj.count`), the LHS of a declaration, an already-called
/// `count()`, or the signal-mutation receiver in `count.set(…)` / `count.update(…)`. A read that is
/// the OBJECT of a plain member access IS called (`user.name` → `user().name`), mirroring the
/// template rule. Parse failures are non-fatal (the input is returned unchanged).
pub fn auto_call_body(javascript: &str, signals: &HashSet<String>) -> String {
    if signals.is_empty() || javascript.trim().is_empty() {
        return javascript.to_string();
    }
    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true);
    let ret = JsParser::new(&allocator, javascript, source_type).parse();
    if !ret.errors.is_empty() {
        return javascript.to_string();
    }

    let mut edits: Vec<Edit> = Vec::new();
    for stmt in &ret.program.body {
        autocall_statement(stmt, signals, &mut edits);
    }
    apply_edits(javascript, edits)
}

// ---------------------------------------------------------------------------
// Template event-handler rewriting (react mode).
// ---------------------------------------------------------------------------

/// Rewrite the React setter calls and signal reads inside every `(event)="ACTION"` binding of a
/// lowered template.
///
/// An inline-arrow JSX handler (`onClick={() => setCount(c => c + 1)}`) is UNWRAPPED to its body by
/// the template lowering ([`super::template`]) BEFORE this pass — so the template already carries
/// `(click)="setCount(c => c + 1)"`. But the body still references the React setter `setCount` and any
/// bare signal reads, which only this pass (which knows the discovered `setters` / `signals`) can
/// rewrite: `setCount(c => c + 1)` → `count.update(c => c + 1)`, a `setX(v)` → `x.set(v)`, and a bare
/// signal read inside the action → a call. The rewrite REUSES the exact body machinery
/// ([`rewrite_uses_in_statement`] + [`autocall_statement`]) so the template and the body lower setters
/// identically.
///
/// Only the `(name)="…"` event-binding VALUES emitted by the JSX template lowering are touched; all
/// other markup (interpolations, attributes, property bindings) is left untouched here — interpolation
/// auto-call is owned by [`super::signals::auto_call_template`]. The binding value is always
/// double-quoted and never contains a literal `"` (its handler body is TS-erased JS whose own string
/// literals use `'`/`` ` ``), so each value is delimited unambiguously by the surrounding `"`.
pub fn rewrite_template_handlers(
    template_html: &str,
    setters: &[(String, String)],
    signals: &HashSet<String>,
) -> String {
    if setters.is_empty() && signals.is_empty() {
        return template_html.to_string();
    }
    let bytes = template_html.as_bytes();
    let mut out = String::with_capacity(template_html.len());
    let mut i = 0usize;
    while i < bytes.len() {
        // Recognize an event binding `(name)="`. The `(` must open an event name (letters), close
        // with `)`, then be immediately followed by `="`.
        if bytes[i] == b'(' {
            if let Some((value_start, value_end)) = event_binding_value_span(template_html, i) {
                // Copy `(name)="` verbatim, rewrite the value, then re-emit the closing `"`.
                out.push_str(&template_html[i..value_start]);
                let value = &template_html[value_start..value_end];
                out.push_str(&rewrite_handler_action(value, setters, signals));
                out.push('"');
                i = value_end + 1; // skip past the closing quote
                continue;
            }
        }
        let ch_len = utf8_len_byte(bytes[i]);
        out.push_str(&template_html[i..i + ch_len]);
        i += ch_len;
    }
    out
}

/// Given `template_html.as_bytes()[at] == b'('`, recognize an Angular event binding `(name)="value"`
/// and return `(value_start, value_end)` — the byte offsets of the binding value (between the quotes).
/// Returns `None` if `at` is not the start of an `(eventname)="…"` binding.
fn event_binding_value_span(template_html: &str, at: usize) -> Option<(usize, usize)> {
    let bytes = template_html.as_bytes();
    let mut j = at + 1;
    // Event name: identifier chars (Angular event names are letters; `.` allowed for `keydown.enter`).
    let name_start = j;
    while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'.' || bytes[j] == b'_') {
        j += 1;
    }
    if j == name_start || j >= bytes.len() || bytes[j] != b')' {
        return None;
    }
    j += 1; // past ')'
    // Must be immediately followed by `="`.
    if j + 1 >= bytes.len() || bytes[j] != b'=' || bytes[j + 1] != b'"' {
        return None;
    }
    let value_start = j + 2;
    // The value runs to the next `"` (handler bodies never contain a literal `"`).
    let mut k = value_start;
    while k < bytes.len() && bytes[k] != b'"' {
        k += 1;
    }
    if k >= bytes.len() {
        return None;
    }
    Some((value_start, k))
}

/// Rewrite the setter calls and signal reads in one event-handler ACTION string (`setCount(c => c + 1)`,
/// `count++`, `doThing(); count.set(0)`) using the body machinery. Parsed as a statement list (the
/// action may be a `;`-separated chain); on a parse failure the action is returned unchanged.
fn rewrite_handler_action(
    action: &str,
    setters: &[(String, String)],
    signals: &HashSet<String>,
) -> String {
    if action.trim().is_empty() {
        return action.to_string();
    }
    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true);
    let ret = JsParser::new(&allocator, action, source_type).parse();
    if !ret.errors.is_empty() {
        return action.to_string();
    }

    let mut edits: Vec<Edit> = Vec::new();
    // 1. Setter calls (`setX(…)` → `x.set/x.update`) and `ref.current` reads — the body's pass 2.
    let no_refs: HashSet<String> = HashSet::new();
    for stmt in &ret.program.body {
        rewrite_uses_in_statement(stmt, action, setters, &no_refs, &mut edits);
    }
    // 2. Bare signal reads (`count` → `count()`) — the body auto-call. A read that is the ARGUMENT of
    //    a setter (`setCount(count + 1)`) is itself auto-called, mirroring the body behaviour.
    for stmt in &ret.program.body {
        autocall_statement(stmt, signals, &mut edits);
    }
    apply_edits(action, edits)
}

/// Length in bytes of the UTF-8 character whose lead byte is `b`.
fn utf8_len_byte(b: u8) -> usize {
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
        //
        // MINIMAL-SPAN: rename the callee `useMemo` → `computed` and delete the trailing `, deps`
        // argument, leaving the `fn` body's bytes untouched so pass 2's setter/`.current`/body
        // auto-call rewrites INSIDE the body compose without overlapping this edit (the
        // overlapping-whole-init edit was the `apply_edits` panic — see the module note on edit
        // composition).
        "useMemo" => {
            if let Some(name) = binding_ident_name(&declarator.id) {
                if rename_callee_and_drop_deps(call, "computed", edits) {
                    signals.insert(name);
                }
            }
        }
        // `const c = useCallback(fn, deps?)` → `const c = fn` (behaviour, not state).
        //
        // MINIMAL-SPAN: delete only the `useCallback(` prefix and the trailing `, deps)` suffix,
        // leaving the function expression itself byte-for-byte (so a `setX(...)` / body read inside
        // it is still rewritten by pass 2 without an overlapping edit). The function's own
        // surrounding parens (e.g. `(e) => …`) are part of its span and are preserved — no paren is
        // stripped or left dangling.
        "useCallback" => {
            unwrap_callback(call, edits);
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
///
/// MINIMAL-SPAN (see [`rename_callee_and_drop_deps`]): rename the callee `useEffect` → `effect` and
/// delete the trailing `, deps` argument, leaving the `fn` body's bytes untouched so pass 2's
/// rewrites inside the body compose without overlap.
fn lower_effect_call(expr: &Expression, _source: &str, edits: &mut Vec<Edit>) {
    let Some((hook, call)) = hook_call(expr) else {
        return;
    };
    if hook != "useEffect" {
        return;
    }
    rename_callee_and_drop_deps(call, "effect", edits);
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
    // `setX(v)` → `x.set(v)`, `setX(fn)` → `x.update(fn)`. A functional-updater argument picks `update`.
    let method = match call.arguments.first().and_then(|a| a.as_expression()) {
        Some(arg)
            if matches!(
                arg,
                Expression::ArrowFunctionExpression(_) | Expression::FunctionExpression(_)
            ) =>
        {
            "update"
        }
        _ => "set",
    };
    // MINIMAL-SPAN edit: rename ONLY the callee identifier (`setX` → `x.set`/`x.update`), leaving the
    // argument list and parens untouched. Replacing the WHOLE call span would OVERLAP any sub-edit
    // another pass queues inside the argument — a `r.current` → `r()` ref read, or a body signal-read
    // auto-call — which panics `apply_edits` (the bug class behind the useRef-in-updater repro). A
    // callee rename is disjoint from those inner spans, so every rewrite composes; the argument's own
    // contents are rewritten in place by the body passes.
    let _ = source;
    let callee_span = call.callee.span();
    edits.push(Edit {
        start: callee_span.start as usize,
        end: callee_span.end as usize,
        text: format!("{signal_name}.{method}"),
    });
}

// ---------------------------------------------------------------------------
// Body auto-call (react mode): rewrite signal reads `count` → `count()`.
// ---------------------------------------------------------------------------

/// Walk a statement collecting body auto-call edits for signal reads.
fn autocall_statement(stmt: &Statement, signals: &HashSet<String>, edits: &mut Vec<Edit>) {
    match stmt {
        Statement::ExpressionStatement(s) => autocall_expression(&s.expression, signals, edits),
        Statement::VariableDeclaration(decl) => {
            // Only the INIT expression is a read site — the declared name (the `id` pattern) is a
            // binding, never auto-called. So a `const count = signal(0)` LHS is left alone, while a
            // `const doubled = computed(() => count * 2)` init has its inner `count` read called.
            for d in &decl.declarations {
                if let Some(init) = &d.init {
                    autocall_expression(init, signals, edits);
                }
            }
        }
        Statement::ReturnStatement(s) => {
            if let Some(arg) = &s.argument {
                autocall_expression(arg, signals, edits);
            }
        }
        Statement::BlockStatement(b) => {
            for s in &b.body {
                autocall_statement(s, signals, edits);
            }
        }
        Statement::IfStatement(s) => {
            autocall_expression(&s.test, signals, edits);
            autocall_statement(&s.consequent, signals, edits);
            if let Some(alt) = &s.alternate {
                autocall_statement(alt, signals, edits);
            }
        }
        Statement::ForStatement(s) => {
            if let Some(test) = &s.test {
                autocall_expression(test, signals, edits);
            }
            if let Some(update) = &s.update {
                autocall_expression(update, signals, edits);
            }
            autocall_statement(&s.body, signals, edits);
        }
        Statement::ForOfStatement(s) => {
            autocall_expression(&s.right, signals, edits);
            autocall_statement(&s.body, signals, edits);
        }
        Statement::ForInStatement(s) => {
            autocall_expression(&s.right, signals, edits);
            autocall_statement(&s.body, signals, edits);
        }
        Statement::WhileStatement(s) => {
            autocall_expression(&s.test, signals, edits);
            autocall_statement(&s.body, signals, edits);
        }
        Statement::DoWhileStatement(s) => {
            autocall_statement(&s.body, signals, edits);
            autocall_expression(&s.test, signals, edits);
        }
        Statement::SwitchStatement(s) => {
            autocall_expression(&s.discriminant, signals, edits);
            for case in &s.cases {
                if let Some(test) = &case.test {
                    autocall_expression(test, signals, edits);
                }
                for s in &case.consequent {
                    autocall_statement(s, signals, edits);
                }
            }
        }
        Statement::ThrowStatement(s) => autocall_expression(&s.argument, signals, edits),
        Statement::TryStatement(s) => {
            for s in &s.block.body {
                autocall_statement(s, signals, edits);
            }
            if let Some(handler) = &s.handler {
                for s in &handler.body.body {
                    autocall_statement(s, signals, edits);
                }
            }
            if let Some(finalizer) = &s.finalizer {
                for s in &finalizer.body {
                    autocall_statement(s, signals, edits);
                }
            }
        }
        Statement::LabeledStatement(s) => autocall_statement(&s.body, signals, edits),
        Statement::FunctionDeclaration(func) => {
            if let Some(body) = &func.body {
                for s in &body.statements {
                    autocall_statement(s, signals, edits);
                }
            }
        }
        Statement::ExportDefaultDeclaration(export) => match &export.declaration {
            ExportDefaultDeclarationKind::FunctionDeclaration(func) => {
                if let Some(body) = &func.body {
                    for s in &body.statements {
                        autocall_statement(s, signals, edits);
                    }
                }
            }
            ExportDefaultDeclarationKind::ArrowFunctionExpression(arrow) => {
                for s in &arrow.body.statements {
                    autocall_statement(s, signals, edits);
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
                                autocall_statement(s, signals, edits);
                            }
                        }
                    }
                    Declaration::VariableDeclaration(var) => {
                        for d in &var.declarations {
                            if let Some(init) = &d.init {
                                autocall_expression(init, signals, edits);
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

/// Walk an expression collecting body auto-call edits. The single READ site that gets a `()` appended
/// is a bare [`Expression::Identifier`] naming a signal that is NOT in an excluded position; every
/// other arm just recurses into the sub-expressions that can hold reads. The excluded positions are
/// handled by NOT routing the identifier through [`autocall_read`]:
///   * a CALL callee — `count(...)` is already a call (handled in the `CallExpression` arm);
///   * a member PROPERTY — `obj.count` keys are never visited (only the member `object` is);
///   * a `.set` / `.update` mutation RECEIVER — `count.set(…)` must stay (handled in the member arm);
///   * a declaration LHS — the `id` pattern is never visited (see [`autocall_statement`]).
fn autocall_expression(expr: &Expression, signals: &HashSet<String>, edits: &mut Vec<Edit>) {
    match expr {
        // A bare identifier read: this is the one place a `()` is appended.
        Expression::Identifier(_) => autocall_read(expr, signals, edits),
        Expression::CallExpression(call) => {
            // The callee is NOT a read-to-call site: a bare `count(...)` is already calling. Recurse
            // into the callee only when it is a member/other expression (so `a.b().c` inner reads are
            // still covered), but never append `()` to a bare-identifier callee.
            if !matches!(&call.callee, Expression::Identifier(_)) {
                autocall_expression(&call.callee, signals, edits);
            }
            for arg in &call.arguments {
                if let Argument::SpreadElement(s) = arg {
                    autocall_expression(&s.argument, signals, edits);
                } else if let Some(e) = arg.as_expression() {
                    autocall_expression(e, signals, edits);
                }
            }
        }
        Expression::StaticMemberExpression(member) => {
            // `signal.set(…)` / `signal.update(…)` is the mutation API — the receiver must NOT be
            // called (it is not a value read). Any other property access on a signal reads its value
            // and IS called: `user.name` → `user().name`. Non-signal objects recurse normally so a
            // deeper read is still found.
            let prop = member.property.name.as_str();
            let is_mutation = prop == "set" || prop == "update";
            if is_mutation {
                if let Expression::Identifier(id) = &member.object {
                    if signals.contains(id.name.as_str()) {
                        // Leave `count.set(...)` / `count.update(...)` receiver untouched.
                        return;
                    }
                }
            }
            autocall_expression(&member.object, signals, edits);
        }
        Expression::ComputedMemberExpression(member) => {
            autocall_expression(&member.object, signals, edits);
            autocall_expression(&member.expression, signals, edits);
        }
        Expression::ParenthesizedExpression(p) => {
            autocall_expression(&p.expression, signals, edits)
        }
        Expression::SequenceExpression(seq) => {
            for e in &seq.expressions {
                autocall_expression(e, signals, edits);
            }
        }
        Expression::AssignmentExpression(assign) => {
            // The LHS target is a write; only the RHS is a read site here.
            autocall_expression(&assign.right, signals, edits)
        }
        Expression::ConditionalExpression(cond) => {
            autocall_expression(&cond.test, signals, edits);
            autocall_expression(&cond.consequent, signals, edits);
            autocall_expression(&cond.alternate, signals, edits);
        }
        Expression::LogicalExpression(logical) => {
            autocall_expression(&logical.left, signals, edits);
            autocall_expression(&logical.right, signals, edits);
        }
        Expression::BinaryExpression(bin) => {
            autocall_expression(&bin.left, signals, edits);
            autocall_expression(&bin.right, signals, edits);
        }
        Expression::UnaryExpression(u) => autocall_expression(&u.argument, signals, edits),
        Expression::AwaitExpression(a) => autocall_expression(&a.argument, signals, edits),
        Expression::ArrowFunctionExpression(arrow) => {
            for s in &arrow.body.statements {
                autocall_statement(s, signals, edits);
            }
        }
        Expression::FunctionExpression(func) => {
            if let Some(body) = &func.body {
                for s in &body.statements {
                    autocall_statement(s, signals, edits);
                }
            }
        }
        Expression::ObjectExpression(obj) => {
            for prop in &obj.properties {
                if let oxc_ast::ast::ObjectPropertyKind::ObjectProperty(p) = prop {
                    // The property VALUE is a read; the key is not. A computed key `[count]` is a read.
                    autocall_expression(&p.value, signals, edits);
                    if p.computed {
                        if let Some(k) = p.key.as_expression() {
                            autocall_expression(k, signals, edits);
                        }
                    }
                }
            }
        }
        Expression::ArrayExpression(arr) => {
            for el in &arr.elements {
                if let Some(e) = el.as_expression() {
                    autocall_expression(e, signals, edits);
                }
            }
        }
        Expression::TemplateLiteral(t) => {
            for e in &t.expressions {
                autocall_expression(e, signals, edits);
            }
        }
        Expression::TSAsExpression(e) => autocall_expression(&e.expression, signals, edits),
        Expression::TSNonNullExpression(e) => autocall_expression(&e.expression, signals, edits),
        Expression::TSSatisfiesExpression(e) => autocall_expression(&e.expression, signals, edits),
        _ => {}
    }
}

/// Append `()` to a bare identifier read when it names a signal. The identifier must be in a READ
/// position (the caller guarantees it is not a callee / property key / mutation receiver / LHS).
fn autocall_read(expr: &Expression, signals: &HashSet<String>, edits: &mut Vec<Edit>) {
    let Expression::Identifier(id) = expr else {
        return;
    };
    if !signals.contains(id.name.as_str()) {
        return;
    }
    let end = id.span.end as usize;
    edits.push(Edit {
        start: end,
        end,
        text: "()".to_string(),
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

/// Rewrite a hook call `hookName(fn, deps?)` to `newName(fn)` with MINIMAL-SPAN edits:
///   1. replace just the **callee identifier** span with `new_name` (`useMemo` → `computed`,
///      `useEffect` → `effect`), and
///   2. delete the **trailing `, deps` argument(s)** — the byte range from the end of the first
///      argument to just before the call's closing `)`.
///
/// The first argument (the hook's function body) is left byte-for-byte, so a `setX(...)` /
/// `ref.current` / body-read auto-call edit that pass 2 queues *inside* that body never overlaps
/// these edits — the two passes compose. (The previous whole-init replacement produced an edit that
/// fully contained pass 2's inner edits, which `apply_edits` then applied with a stale end offset →
/// the `range end index out of range` panic.)
///
/// Returns `true` when the rewrite was applied (the call had a first argument), so the caller can
/// record the binding as a signal only on success.
fn rename_callee_and_drop_deps(
    call: &oxc_ast::ast::CallExpression,
    new_name: &str,
    edits: &mut Vec<Edit>,
) -> bool {
    let Some(first) = call.arguments.first().and_then(|a| a.as_expression()) else {
        return false;
    };

    // 1. Rename the callee identifier in place.
    let callee_span = call.callee.span();
    edits.push(Edit {
        start: callee_span.start as usize,
        end: callee_span.end as usize,
        text: new_name.to_string(),
    });

    // 2. Delete the trailing `, deps` (everything after the first argument up to the closing `)`).
    //    `call.span.end` is one past the closing `)`, so the deps span is `[first.end, end - 1)`.
    let first_end = first.span().end as usize;
    let call_end = call.span.end as usize;
    let drop_end = call_end.saturating_sub(1); // before the `)`
    if drop_end > first_end {
        edits.push(Edit {
            start: first_end,
            end: drop_end,
            text: String::new(),
        });
    }
    true
}

/// Unwrap `useCallback(fn, deps?)` to just `fn` with MINIMAL-SPAN edits: delete the `useCallback(`
/// prefix (the callee through the call's opening `(`) and the trailing `, deps)` suffix (everything
/// after the first argument through the closing `)`). The function expression's own bytes — including
/// any parentheses that are part of its span (e.g. arrow params `(e) => …`) — are left untouched, so
/// no paren is stripped or left dangling and pass 2's inner rewrites compose without overlap.
fn unwrap_callback(call: &oxc_ast::ast::CallExpression, edits: &mut Vec<Edit>) {
    let Some(first) = call.arguments.first().and_then(|a| a.as_expression()) else {
        return;
    };
    let call_start = call.span.start as usize;
    let call_end = call.span.end as usize;
    let first_start = first.span().start as usize;
    let first_end = first.span().end as usize;

    // Delete the `useCallback(` prefix: from the start of the call to the start of the first argument.
    if first_start > call_start {
        edits.push(Edit {
            start: call_start,
            end: first_start,
            text: String::new(),
        });
    }
    // Delete the trailing `, deps)` suffix: from the end of the first argument to the end of the call.
    if call_end > first_end {
        edits.push(Edit {
            start: first_end,
            end: call_end,
            text: String::new(),
        });
    }
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
        let _ = out;
    }

    #[test]
    fn setter_updater_reading_a_ref_composes_without_panic() {
        // Regression (4th overlapping-edit case): a functional setter updater whose body reads a
        // `useRef` `.current` must NOT panic — the setter rewrite is minimal-span (callee rename
        // only), so the inner `.current` → `r()` ref read composes instead of overlapping the whole
        // call span. `setN(prev => prev + r.current)` → `n.update(prev => prev + r())`.
        let out = transform(
            "const [n, setN] = useState(0);\nconst r = useRef(0);\nfunction bump() { setN(prev => prev + r.current); }",
        );
        assert!(
            out.javascript.contains("n.update(prev => prev + r())"),
            "setter updater with a ref read not lowered correctly; got: {}",
            out.javascript
        );
        assert!(
            !out.javascript.contains("r.current"),
            "useRef `.current` read not auto-called; got: {}",
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

    // --- adversarial regression repros ---------------------------------------

    /// Parse `js` as a TS body and assert it has no syntax errors (the lowered body must be valid JS,
    /// not a corrupt fragment with a stray paren / out-of-range edit).
    fn assert_parses(js: &str) {
        let allocator = Allocator::default();
        let ret = JsParser::new(&allocator, js, SourceType::default().with_typescript(true)).parse();
        assert!(ret.errors.is_empty(), "lowered body did not parse: {:?}\n--- js ---\n{js}", ret.errors);
    }

    #[test]
    fn repro1_use_ref_in_use_memo_does_not_panic_and_composes() {
        // BUG 1: pass 1 replaced the WHOLE useMemo init span while pass 2 queued an OVERLAPPING edit
        // rewriting `r.current` INSIDE it → `apply_edits` panicked `range end index out of range`.
        // Minimal-span hook edits now compose: callee `useMemo`→`computed`, deps dropped, and the
        // `r.current`→`r()` read rewritten inside the untouched body.
        let out = transform("const r = useRef(0);\nconst m = useMemo(() => r.current + 1, []);");
        assert_eq!(
            out.javascript,
            "const r = signal(0);\nconst m = computed(() => r() + 1);",
            "useRef-in-useMemo did not compose; got: {}",
            out.javascript
        );
        assert_parses(&out.javascript);
        // No leaked hook callees, and the deps array was dropped.
        assert!(!out.javascript.contains("useMemo") && !out.javascript.contains(", [])"));
    }

    #[test]
    fn repro2_use_callback_setter_unwraps_cleanly_and_rewrites_setter() {
        // BUG 2: useCallback unwrap replaced the whole init AND pass 2's inner `setVal(...)` overlapped
        // → emitted `const onInput = (e) => setVal(e.target.value));` (stray trailing `)`, and the
        // inner setter never rewritten). Minimal-span unwrap (delete `useCallback(` prefix + `, deps)`
        // suffix) leaves the function body for pass 2, which rewrites the setter.
        let out = transform(
            "const [val, setVal] = useState('');\nconst onInput = useCallback((e) => setVal(e.target.value), []);",
        );
        assert_eq!(
            out.javascript,
            "const val = signal('');\nconst onInput = (e) => val.set(e.target.value);",
            "useCallback+setter not lowered cleanly; got: {}",
            out.javascript
        );
        assert_parses(&out.javascript);
        // No stray trailing paren, no leaked callee, the inner setter became `.set`.
        assert!(!out.javascript.contains("useCallback"));
        assert!(!out.javascript.contains("value));"), "stray trailing paren; got: {}", out.javascript);
        assert!(out.javascript.contains("val.set(e.target.value)"));
    }

    #[test]
    fn repro3_body_reads_of_signals_are_auto_called() {
        // BUG 3: a useState/useMemo signal read INSIDE a hook body is a read of a signal *function* —
        // `count * 2` was `NaN`, `console.log(count)` logged the function and the effect never re-ran.
        // `auto_call_body` (react mode) rewrites the body reads to calls.
        let rt = transform(
            "const [count, setCount] = useState(0);\nconst doubled = useMemo(() => count * 2, [count]);\nuseEffect(() => console.log(count));",
        );
        let body = auto_call_body(&rt.javascript, &rt.signals);
        assert_eq!(
            body,
            "const count = signal(0);\nconst doubled = computed(() => count() * 2);\neffect(() => console.log(count()));",
            "body reads not auto-called; got: {body}"
        );
        assert_parses(&body);
        // Reads inside BOTH the computed and the effect are called.
        assert!(body.contains("count() * 2"), "computed body read not called; got: {body}");
        assert!(body.contains("console.log(count())"), "effect body read not called; got: {body}");
    }

    #[test]
    fn body_autocall_does_not_touch_mutation_receiver_or_property_key() {
        // The signal-mutation receiver `count.set` / `count.update` must NOT be called, a property key
        // `obj.count` must NOT be called, and an already-called `count()` must NOT be double-called;
        // a plain member-access object IS called (`user.name` → `user().name`).
        let signals: HashSet<String> =
            ["count", "user"].into_iter().map(String::from).collect();
        let body = auto_call_body(
            "function f() {\n  count.set(1);\n  count.update(p => p + 1);\n  const a = obj.count;\n  const b = count();\n  const c = user.name;\n  const d = count + 1;\n}",
            &signals,
        );
        assert!(body.contains("count.set(1)"), "mutation receiver wrongly called; got: {body}");
        assert!(body.contains("count.update(p => p + 1)"), "update receiver wrongly called; got: {body}");
        assert!(body.contains("const a = obj.count;"), "property key wrongly called; got: {body}");
        assert!(body.contains("const b = count();"), "already-called read double-called; got: {body}");
        assert!(body.contains("const c = user().name;"), "member object not called; got: {body}");
        assert!(body.contains("const d = count() + 1;"), "plain read not called; got: {body}");
        assert_parses(&body);
    }

    #[test]
    fn body_autocall_is_a_noop_with_no_signals() {
        // No signals → the body is returned byte-identically (so a non-react / signal-free body is
        // never disturbed).
        let body = "const x = compute(1) + go();";
        assert_eq!(auto_call_body(body, &HashSet::new()), body);
    }

    // --- template event-handler rewrite (GAP 2) -------------------------------

    fn setters(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    fn sig(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn template_handler_functional_setter_becomes_update() {
        // GAP 2 verification: an UNWRAPPED inline-arrow handler `(click)="setCount(c => c + 1)"` has
        // its functional-updater setter rewritten to `count.update(c => c + 1)`.
        let out = rewrite_template_handlers(
            "<button (click)=\"setCount(c => c + 1)\">x</button>",
            &setters(&[("setCount", "count")]),
            &sig(&["count"]),
        );
        assert_eq!(
            out,
            "<button (click)=\"count.update(c => c + 1)\">x</button>",
            "functional setter not lowered to update; got: {out}"
        );
    }

    #[test]
    fn template_handler_value_setter_becomes_set_and_arg_auto_calls() {
        // A value setter `setCount(count + 1)` → `count.set(count() + 1)`: the setter becomes `.set`
        // and the bare signal read in the argument auto-calls.
        let out = rewrite_template_handlers(
            "<button (click)=\"setCount(count + 1)\">x</button>",
            &setters(&[("setCount", "count")]),
            &sig(&["count"]),
        );
        assert_eq!(
            out,
            "<button (click)=\"count.set(count() + 1)\">x</button>",
            "value setter / arg auto-call wrong; got: {out}"
        );
    }

    #[test]
    fn template_handler_plain_setter_set_value() {
        // `setDismissed(true)` (alert.tsx) → `dismissed.set(true)`.
        let out = rewrite_template_handlers(
            "<button (click)=\"setDismissed(true)\">x</button>",
            &setters(&[("setDismissed", "dismissed")]),
            &sig(&["dismissed"]),
        );
        assert_eq!(
            out,
            "<button (click)=\"dismissed.set(true)\">x</button>",
            "plain setter not lowered; got: {out}"
        );
    }

    #[test]
    fn template_handler_leaves_non_event_markup_untouched() {
        // Interpolations, attributes, and property bindings are NOT event bindings and are untouched
        // (interpolation auto-call is owned by the signals pass). A `(click)` next to them still
        // rewrites.
        let out = rewrite_template_handlers(
            "<div [id]=\"setCount\">{{ count }}</div><button (click)=\"setCount(1)\">x</button>",
            &setters(&[("setCount", "count")]),
            &sig(&["count"]),
        );
        assert_eq!(
            out,
            "<div [id]=\"setCount\">{{ count }}</div><button (click)=\"count.set(1)\">x</button>",
            "non-event markup wrongly touched; got: {out}"
        );
    }

    #[test]
    fn template_handler_noop_without_setters_or_signals() {
        let html = "<button (click)=\"doThing()\">x</button>";
        assert_eq!(rewrite_template_handlers(html, &[], &HashSet::new()), html);
    }
}
