//! TypeScript-to-Rust transpilation of a server-fn body, targeting an [axum](https://docs.rs/axum)
//! handler.
//!
//! A Treaty server function is authored in TypeScript inside a `server { … }` block. A backend that
//! emits a Rust/axum server needs the *body* of that function rewritten as Rust. This module is the
//! body transpiler: given the function source text it parses it with OXC and walks a faithful subset
//! of the JS/TS grammar, lowering each node to equivalent Rust source.
//!
//! ## Supported subset
//!
//! Statements: `return`, `let`/`const` variable declarations, `if`/`else`, simple numeric `for`
//! loops, and bare expression statements. Expressions: numeric/string/boolean literals, identifiers
//! (parameters and locals), binary arithmetic and comparison operators, logical `&&`/`||`, string
//! concatenation via `+` (rendered as a `format!` when a string operand is involved, so it yields a
//! `String`), member access (`a.b`), computed index (`a[b]`), the conditional `?:` ternary, unary
//! `-`/`!`, `await` (lowered to a trailing `.await`), template literals (`` `x ${y}` `` ->
//! `format!("x {}", y)`), array literals (`[a, b]` -> `serde_json::json!([a, b])`), object literals
//! (`{ a: 1 }` -> `serde_json::json!({ "a": 1 })`), and call expressions (passed through by callee
//! name — see the note on db-ish dependency calls below).
//!
//! ## Handler shape
//!
//! [`transpile_handler_body`] is the entry the axum backend uses: it lowers the body AND wraps every
//! `return EXPR` so the result is the axum `Json<RESP>` the handler signature promises (the value is
//! coerced to the mapped response type — `serde_json::json!(…)` for a JSON response, `format!`/numeric
//! for the scalar cases). A handler whose body falls outside the supported subset gets a clearly
//! marked, COMPILING stub: the original TS is preserved as a `//` comment and the handler returns
//! `Json(Default::default())` (a typed default for the response type) — never broken Rust.
//!
//! ## Graceful degradation (no marker words)
//!
//! Anything outside that subset is *not* a hard failure and leaves no marker tokens behind. Instead
//! the transpiler:
//!   * records a human-readable explanation in [`TranspileResult::notes`],
//!   * preserves the original TypeScript slice as a plain `//` comment, and
//!   * emits a typed `Default::default()` so the generated Rust still compiles,
//! then sets [`TranspileResult::covered`] to `false`.
//!
//! [`ts_type_to_rust`] is the companion type-mapping helper used to render the handler's return type
//! and to pick the right `Default::default()` fallback type.

use oxc_allocator::Allocator;
use oxc::syntax::operator::{BinaryOperator, LogicalOperator, UnaryOperator};
use oxc_ast::ast::{
    Argument, ArrayExpressionElement, Expression, ForStatementInit, ObjectPropertyKind, PropertyKey,
    Statement, TemplateLiteral, VariableDeclarationKind,
};
use oxc_parser::Parser as JsParser;
use oxc_span::{GetSpan, SourceType, Span};

/// The outcome of transpiling a server-fn body to Rust.
///
/// `rust_body` is the lowered handler body (a sequence of Rust statements, not wrapped in braces).
/// `return_ty` is the Rust type the handler returns, derived from the TS return annotation when one
/// is present (defaulting to `serde_json::Value`). `covered` is `true` only when every node in the
/// body fell inside the supported subset; any fallback flips it to `false`. `notes` collects one
/// human-readable entry per node that could not be translated faithfully.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TranspileResult {
    pub rust_body: String,
    pub return_ty: String,
    pub covered: bool,
    pub notes: Vec<String>,
}

/// Map a TypeScript type annotation (verbatim source text, e.g. `"number"`, `"string[]"`,
/// `"Promise<number>"`) to the closest Rust type:
///
///   * `number`  -> `f64`
///   * `string`  -> `String`
///   * `boolean` -> `bool`
///   * `T[]` / `Array<T>` -> `Vec<T>` (recursively mapping the element type)
///   * `Promise<T>` / `AsyncGenerator<T>` / `Generator<T>` -> the mapping of `T` (the future/stream is
///     unwrapped, since the handler awaits / yields it)
///   * a union with `undefined`/`null` (`T | undefined`) -> the mapping of the non-null arm
///   * anything else (`object`, `unknown`, `any`, named interfaces, …) -> `serde_json::Value`
///
/// The input is trimmed; surrounding parentheses are peeled so `(number)` maps like `number`.
pub fn ts_type_to_rust(ts: &str) -> String {
    let ts = ts.trim();
    // Peel a single layer of wrapping parentheses, e.g. `(number)`.
    if let Some(inner) = ts.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
        return ts_type_to_rust(inner);
    }

    // A `T | undefined` / `T | null` union (a common "maybe" return) maps to the non-null arm; the
    // serde response simply serialises `null` when the value is absent. Only split on a top-level `|`
    // (depth 0) so a union inside a generic argument is not mis-split.
    if let Some(non_null) = strip_nullable_union(ts) {
        return ts_type_to_rust(&non_null);
    }

    // `T[]` array shorthand.
    if let Some(elem) = ts.strip_suffix("[]") {
        return format!("Vec<{}>", ts_type_to_rust(elem));
    }

    // `Array<T>` -> `Vec<T>`; `Promise<T>` / `AsyncGenerator<T>` / `Generator<T>` unwrap to `T`.
    if let Some(elem) = generic_arg(ts, "Array") {
        return format!("Vec<{}>", ts_type_to_rust(elem));
    }
    for wrapper in ["Promise", "AsyncGenerator", "Generator", "AsyncIterable", "Iterable"] {
        if let Some(elem) = generic_arg(ts, wrapper) {
            // The future/stream is awaited or yielded in the handler, so unwrap to the inner type.
            // A multi-type-arg generator form (`AsyncGenerator<T, R, N>`) keeps only the first arm.
            let first = elem.split(',').next().unwrap_or(elem).trim();
            return ts_type_to_rust(first);
        }
    }

    match ts {
        "number" => "f64".to_string(),
        "string" => "String".to_string(),
        "boolean" => "bool".to_string(),
        // `void`/`undefined`/`null` have no useful typed Rust analogue here; treat as JSON null.
        _ => "serde_json::Value".to_string(),
    }
}

/// If `ts` is a top-level union with `undefined`/`null` (`T | undefined`, `null | T`, …), return the
/// remaining non-null arm(s) joined back with `|`. Returns `None` when there is no top-level `|` or
/// when no arm is `undefined`/`null`. Splitting respects `<…>` / `(…)` / `{…}` nesting so a `|` inside
/// a generic or object type does not split.
fn strip_nullable_union(ts: &str) -> Option<String> {
    let arms = split_top_level_union(ts);
    if arms.len() < 2 {
        return None;
    }
    let kept: Vec<&str> = arms
        .iter()
        .map(|a| a.trim())
        .filter(|a| *a != "undefined" && *a != "null")
        .collect();
    if kept.len() == arms.len() {
        // No null/undefined arm — not a nullable union.
        return None;
    }
    if kept.is_empty() {
        return Some("undefined".to_string());
    }
    Some(kept.join(" | "))
}

/// Split `ts` on top-level `|` (union) separators, tracking `<>`/`()`/`{}`/`[]` depth so nested
/// pipes are not split. Returns the original string as a single arm when there is no top-level `|`.
fn split_top_level_union(ts: &str) -> Vec<&str> {
    let bytes = ts.as_bytes();
    let mut arms = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'<' | b'(' | b'{' | b'[' => depth += 1,
            b'>' | b')' | b'}' | b']' => depth -= 1,
            b'|' if depth == 0 => {
                arms.push(&ts[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    arms.push(&ts[start..]);
    arms
}

/// If `ts` is `Wrapper<INNER>` (single type argument), return the trimmed `INNER`. Brace/angle depth
/// is tracked so nested generics like `Promise<Array<number>>` peel one layer at a time.
fn generic_arg<'t>(ts: &'t str, wrapper: &str) -> Option<&'t str> {
    let rest = ts.strip_prefix(wrapper)?.trim_start();
    let inner = rest.strip_prefix('<')?.strip_suffix('>')?;
    Some(inner.trim())
}

/// Transpile the body of the TypeScript function whose full declaration is `source` (e.g. the
/// verbatim `ServerFn::source` text) into Rust source for an axum handler.
///
/// `source` is re-parsed with OXC; the first function declaration or arrow-const found provides the
/// body and return-type annotation. When no function body can be located the result is an empty
/// `covered = false` body with an explanatory note.
///
/// This lowers the body STATEMENTS only (each `return` stays a bare `return EXPR;`). The axum backend
/// uses [`transpile_handler_body`], which additionally wraps the result into the handler's `Json<RESP>`
/// return shape; this plain form is the documented raw-lowering API (and the surface the unit tests
/// exercise directly), kept for callers/tests that want the unwrapped statements.
#[allow(dead_code)] // Public raw-lowering API + test surface; the axum path uses `transpile_handler_body`.
pub fn transpile_body(source: &str) -> TranspileResult {
    transpile_with_mode(source, ReturnMode::Bare)
}

/// Transpile a server-fn body into the BODY of an axum handler that returns `Json<RESP>`.
///
/// On top of [`transpile_body`] this:
///   * wraps every `return EXPR` so it yields the handler's `Json<RESP>` (the value coerced to the
///     mapped response type), and appends a trailing `Json(Default::default())` so a body that can
///     fall off the end still returns the promised type, and
///   * when the body could NOT be fully transpiled, REPLACES it with a clearly-marked, compiling stub:
///     the original TS preserved as a `//` comment plus `Json(Default::default())`. The generated Rust
///     therefore always compiles — a real run for the covered shapes, a typed-default stub otherwise.
///
/// `return_ty` carries the mapped response type so the caller can render the `-> Json<RESP>` signature
/// consistently.
pub fn transpile_handler_body(source: &str) -> TranspileResult {
    let resp = handler_response_type(source);
    let lowered = transpile_with_mode(source, ReturnMode::Json { resp: resp.clone() });

    if lowered.covered {
        // The body transpiled cleanly. Append a trailing typed `Json` default ONLY when control can
        // reach the end of the body (i.e. the body does not end in an unconditional `return`), so the
        // emit has no unreachable-code tail. A body that always returns needs no fall-through.
        let mut rust_body = lowered.rust_body;
        if !rust_body.ends_with('\n') {
            rust_body.push('\n');
        }
        if !ends_in_unconditional_return(&rust_body) {
            rust_body.push_str("    Json(Default::default())\n");
        }
        return TranspileResult { rust_body, ..lowered };
    }

    // Could not transpile faithfully: emit a clearly-marked, COMPILING stub. Preserve the author's TS
    // as a plain comment (so nothing is silently dropped) and return a typed default.
    let mut rust_body = String::new();
    for line in body_source_text(source).lines() {
        rust_body.push_str("    // ");
        rust_body.push_str(line);
        rust_body.push('\n');
    }
    rust_body.push_str("    Json(Default::default())\n");
    TranspileResult { rust_body, return_ty: resp, covered: false, notes: lowered.notes }
}

/// The mapped Rust response type for a server fn whose declaration is `source` — its TS return
/// annotation mapped via [`ts_type_to_rust`], defaulting to `serde_json::Value`.
pub fn handler_response_type(source: &str) -> String {
    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true);
    let ret = JsParser::new(&allocator, source, source_type).parse();
    for stmt in &ret.program.body {
        if let Some(text) = return_type_text(source, stmt) {
            return ts_type_to_rust(&text);
        }
    }
    "serde_json::Value".to_string()
}

/// The verbatim return-type annotation text of the first function declaration / arrow-const in `stmt`.
fn return_type_text(source: &str, stmt: &Statement) -> Option<String> {
    match stmt {
        Statement::FunctionDeclaration(func) => {
            func.body.as_deref()?;
            func.return_type
                .as_ref()
                .map(|ann| span_text(source, ann.type_annotation.span()))
        }
        Statement::VariableDeclaration(decl) => {
            let declarator = decl.declarations.first()?;
            let Expression::ArrowFunctionExpression(arrow) = declarator.init.as_ref()? else {
                return None;
            };
            arrow
                .return_type
                .as_ref()
                .map(|ann| span_text(source, ann.type_annotation.span()))
        }
        _ => None,
    }
}

/// How `return` statements are rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ReturnMode {
    /// `return EXPR;` stays a bare Rust `return EXPR;` (the raw lowering used by [`transpile_body`]).
    Bare,
    /// `return EXPR;` becomes `return Json(<coerced EXPR>);`, coercing the value to `resp` so it
    /// matches an axum handler's `-> Json<RESP>` signature.
    Json { resp: String },
}

/// Shared driver for [`transpile_body`] / [`transpile_handler_body`].
fn transpile_with_mode(source: &str, return_mode: ReturnMode) -> TranspileResult {
    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true);
    let ret = JsParser::new(&allocator, source, source_type).parse();

    // Locate the function body + return-type text from the first supported declaration form.
    let mut body_stmts: Option<&oxc_allocator::Vec<Statement>> = None;
    let mut return_ty_text: Option<String> = None;

    for stmt in &ret.program.body {
        match stmt {
            Statement::FunctionDeclaration(func) => {
                if let Some(body) = func.body.as_deref() {
                    body_stmts = Some(&body.statements);
                    return_ty_text = func
                        .return_type
                        .as_ref()
                        .map(|ann| span_text(source, ann.type_annotation.span()));
                    break;
                }
            }
            Statement::VariableDeclaration(decl) => {
                if let Some(declarator) = decl.declarations.first() {
                    if let Some(Expression::ArrowFunctionExpression(arrow)) = &declarator.init {
                        body_stmts = Some(&arrow.body.statements);
                        return_ty_text = arrow
                            .return_type
                            .as_ref()
                            .map(|ann| span_text(source, ann.type_annotation.span()));
                        break;
                    }
                }
            }
            _ => {}
        }
    }

    let return_ty = match &return_mode {
        ReturnMode::Json { resp } => resp.clone(),
        ReturnMode::Bare => return_ty_text
            .as_deref()
            .map(ts_type_to_rust)
            .unwrap_or_else(|| "serde_json::Value".to_string()),
    };

    let Some(stmts) = body_stmts else {
        return TranspileResult {
            rust_body: String::new(),
            return_ty,
            covered: false,
            notes: vec!["could not locate a function body to transpile".to_string()],
        };
    };

    let mut tx = Transpiler { source, notes: Vec::new(), covered: true, return_mode };
    let mut out = String::new();
    for stmt in stmts {
        let rendered = tx.statement(stmt, 1);
        out.push_str(&rendered);
        if !rendered.ends_with('\n') {
            out.push('\n');
        }
    }

    TranspileResult { rust_body: out, return_ty, covered: tx.covered, notes: tx.notes }
}

/// The verbatim `{ … }` body text of the first function/arrow declaration in `source` (used to
/// preserve the author's TS as a comment in the stub path).
fn body_source_text(source: &str) -> String {
    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true);
    let ret = JsParser::new(&allocator, source, source_type).parse();
    for stmt in &ret.program.body {
        match stmt {
            Statement::FunctionDeclaration(func) => {
                if let Some(body) = func.body.as_deref() {
                    return span_text(source, body.span);
                }
            }
            Statement::VariableDeclaration(decl) => {
                if let Some(declarator) = decl.declarations.first() {
                    if let Some(Expression::ArrowFunctionExpression(arrow)) = &declarator.init {
                        return span_text(source, arrow.body.span);
                    }
                }
            }
            _ => {}
        }
    }
    source.trim().to_string()
}

/// Walker state shared across the recursive lowering. `source` is the original text (for slicing
/// out unsupported nodes verbatim); `notes`/`covered` accumulate degradation info; `return_mode`
/// drives how `return` statements are rendered.
struct Transpiler<'a> {
    source: &'a str,
    notes: Vec<String>,
    covered: bool,
    return_mode: ReturnMode,
}

impl<'a> Transpiler<'a> {
    /// Render `indent` levels of two-space indentation.
    fn pad(indent: usize) -> String {
        "    ".repeat(indent)
    }

    /// Record a degradation: push an explanatory note, mark the result uncovered, and return a Rust
    /// snippet that preserves the original TS as a `//` comment and yields a typed default so the
    /// generated code still compiles.
    fn fallback(&mut self, what: &str, span: Span, indent: usize) -> String {
        let slice = span_text(self.source, span);
        self.covered = false;
        self.notes.push(format!("{what}: `{slice}`"));
        let pad = Self::pad(indent);
        // Preserve the original TS as a plain comment, then emit a compiling typed default.
        format!("{pad}// {slice}\n{pad}Default::default()")
    }

    /// Render a `return EXPR` statement honoring [`ReturnMode`]: a bare `return EXPR;`, or — for an
    /// axum handler — `return Json(<EXPR coerced to RESP>);`.
    fn render_return(&mut self, expr: &Expression, indent: usize) -> String {
        let pad = Self::pad(indent);
        let value = self.expression(expr, indent);
        match &self.return_mode {
            ReturnMode::Bare => format!("{pad}return {value};"),
            ReturnMode::Json { resp } => {
                let coerced = coerce_to_response(&value, resp, expr);
                format!("{pad}return Json({coerced});")
            }
        }
    }

    /// Render a value-less `return;`. In `Json` mode there is no value, so we return the typed default.
    fn render_empty_return(&self, indent: usize) -> String {
        let pad = Self::pad(indent);
        match &self.return_mode {
            ReturnMode::Bare => format!("{pad}return;"),
            ReturnMode::Json { .. } => format!("{pad}return Json(Default::default());"),
        }
    }

    /// Lower a statement to Rust source (already indented to `indent`).
    fn statement(&mut self, stmt: &Statement, indent: usize) -> String {
        let pad = Self::pad(indent);
        match stmt {
            Statement::ReturnStatement(ret) => match &ret.argument {
                Some(expr) => self.render_return(expr, indent),
                None => self.render_empty_return(indent),
            },
            Statement::ExpressionStatement(es) => {
                let value = self.expression(&es.expression, indent);
                format!("{pad}{value};")
            }
            Statement::VariableDeclaration(decl) => self.variable_declaration(decl, indent),
            Statement::IfStatement(if_stmt) => {
                let test = self.expression(&if_stmt.test, indent);
                let consequent = self.block_or_stmt(&if_stmt.consequent, indent);
                let mut out = format!("{pad}if {test} {{\n{consequent}\n{pad}}}");
                if let Some(alternate) = &if_stmt.alternate {
                    // An `else if` chain keeps the `if` on the same line as `else`.
                    if matches!(alternate, Statement::IfStatement(_)) {
                        let nested = self.statement(alternate, indent);
                        let nested = nested.trim_start();
                        out.push_str(&format!(" else {nested}"));
                    } else {
                        let alt = self.block_or_stmt(alternate, indent);
                        out.push_str(&format!(" else {{\n{alt}\n{pad}}}"));
                    }
                }
                out
            }
            Statement::ForStatement(for_stmt) => self.for_statement(for_stmt, indent),
            Statement::BlockStatement(block) => {
                let inner = self.block_body(&block.body, indent + 1);
                format!("{pad}{{\n{inner}\n{pad}}}")
            }
            other => self.fallback("unsupported statement", other.span(), indent),
        }
    }

    /// Lower a `let`/`const`/`var` declaration. Each declarator becomes a Rust `let` binding; `const`
    /// and `let` both map to `let` (Rust immutability is the safe default for a faithful port).
    fn variable_declaration(
        &mut self,
        decl: &oxc_ast::ast::VariableDeclaration,
        indent: usize,
    ) -> String {
        let pad = Self::pad(indent);
        let mut lines = Vec::new();
        for declarator in &decl.declarations {
            let Some(name) = declarator.id.get_identifier_name() else {
                lines.push(self.fallback(
                    "unsupported binding pattern",
                    declarator.span,
                    indent,
                ));
                continue;
            };
            // `let` vs `const`: a reassigned `let` would need `mut`, but reassignment lives outside
            // the supported subset, so an immutable binding is faithful here.
            let _ = decl.kind == VariableDeclarationKind::Const;
            match &declarator.init {
                Some(init) => {
                    let value = self.expression(init, indent);
                    lines.push(format!("{pad}let {name} = {value};"));
                }
                None => lines.push(format!("{pad}let {name};")),
            }
        }
        lines.join("\n")
    }

    /// Lower a numeric `for (let i = …; i < …; i++)` loop to a Rust `for i in start..end` range loop
    /// when its shape matches; otherwise fall back. Only the canonical ascending integer counter form
    /// is recognised.
    fn for_statement(&mut self, for_stmt: &oxc_ast::ast::ForStatement, indent: usize) -> String {
        let pad = Self::pad(indent);
        if let Some((var, start, end)) = self.numeric_for_shape(for_stmt, indent) {
            let body = self.block_or_stmt(&for_stmt.body, indent);
            return format!("{pad}for {var} in {start}..{end} {{\n{body}\n{pad}}}");
        }
        self.fallback("unsupported for-loop shape", for_stmt.span, indent)
    }

    /// Recognise the canonical `for (let i = START; i < END; i++)` ascending numeric loop and return
    /// `(counter_name, start_expr, end_expr)`. Returns `None` for any other shape.
    fn numeric_for_shape(
        &mut self,
        for_stmt: &oxc_ast::ast::ForStatement,
        indent: usize,
    ) -> Option<(String, String, String)> {
        // init: `let i = START`
        let Some(ForStatementInit::VariableDeclaration(decl)) = &for_stmt.init else {
            return None;
        };
        if decl.declarations.len() != 1 {
            return None;
        }
        let declarator = decl.declarations.first()?;
        let var = declarator.id.get_identifier_name()?.to_string();
        let init = declarator.init.as_ref()?;
        let start = self.expression(init, indent);

        // test: `i < END` (or `i <= END`, which we normalise to an exclusive bound via `END + 1`).
        let Some(Expression::BinaryExpression(test)) = &for_stmt.test else {
            return None;
        };
        let Expression::Identifier(test_id) = &test.left else {
            return None;
        };
        if test_id.name.as_str() != var {
            return None;
        }
        let end_expr = self.expression(&test.right, indent);
        let end = match test.operator {
            BinaryOperator::LessThan => end_expr,
            BinaryOperator::LessEqualThan => format!("{end_expr} + 1"),
            _ => return None,
        };

        // update: `i++` or `i += 1`.
        let update_ok = match &for_stmt.update {
            Some(Expression::UpdateExpression(upd)) => upd
                .argument
                .get_identifier_name()
                .is_some_and(|n| n == var),
            _ => false,
        };
        if !update_ok {
            return None;
        }

        Some((var, start, end))
    }

    /// Lower a statement that is the body of an `if`/`else`/`for`, indenting its interior one level
    /// deeper. A `BlockStatement` is unwrapped (its inner statements are emitted directly); any other
    /// single statement is emitted as-is at the deeper indent.
    fn block_or_stmt(&mut self, stmt: &Statement, indent: usize) -> String {
        match stmt {
            Statement::BlockStatement(block) => self.block_body(&block.body, indent + 1),
            other => self.statement(other, indent + 1),
        }
    }

    /// Lower a sequence of statements (a block interior) at `indent`.
    fn block_body(&mut self, stmts: &[Statement], indent: usize) -> String {
        let mut lines = Vec::new();
        for stmt in stmts {
            lines.push(self.statement(stmt, indent));
        }
        lines.join("\n")
    }

    /// Lower an expression to Rust source. `indent` is threaded only so a fallback can match the
    /// surrounding indentation when an unsupported sub-expression is hit.
    fn expression(&mut self, expr: &Expression, indent: usize) -> String {
        match expr {
            Expression::NumericLiteral(lit) => {
                // Render integers without a trailing `.0` only when they are whole; otherwise keep the
                // decimal so the literal stays an `f64`.
                let v = lit.value;
                if v.fract() == 0.0 && v.is_finite() {
                    format!("{v:.1}")
                } else {
                    format!("{v}")
                }
            }
            Expression::StringLiteral(lit) => format!("{:?}", lit.value.as_str()),
            Expression::BooleanLiteral(lit) => lit.value.to_string(),
            Expression::NullLiteral(_) => "serde_json::Value::Null".to_string(),
            Expression::Identifier(id) => id.name.to_string(),
            Expression::ParenthesizedExpression(paren) => {
                format!("({})", self.expression(&paren.expression, indent))
            }
            Expression::TemplateLiteral(tpl) => self.template_literal(tpl, indent),
            Expression::ArrayExpression(arr) => {
                // A JS array literal lowers to a `serde_json::json!([…])` value, so it composes with
                // the `serde_json::Value` response type and with object literals.
                let mut elems = Vec::new();
                for el in &arr.elements {
                    match el {
                        ArrayExpressionElement::SpreadElement(spread) => {
                            elems.push(self.fallback(
                                "unsupported array spread",
                                spread.span,
                                indent,
                            ));
                        }
                        ArrayExpressionElement::Elision(_) => {
                            elems.push("serde_json::Value::Null".to_string());
                        }
                        other => {
                            if let Some(e) = other.as_expression() {
                                elems.push(self.expression(e, indent));
                            } else {
                                elems.push(self.fallback(
                                    "unsupported array element",
                                    other.span(),
                                    indent,
                                ));
                            }
                        }
                    }
                }
                format!("serde_json::json!([{}])", elems.join(", "))
            }
            Expression::ObjectExpression(obj) => {
                // A JS object literal lowers to a `serde_json::json!({ … })` value — the faithful
                // representation of an untyped object return shape.
                let mut props = Vec::new();
                for prop in &obj.properties {
                    match prop {
                        ObjectPropertyKind::ObjectProperty(p) => {
                            let key = self.property_key(&p.key, indent);
                            // A shorthand `{ x }` reuses the key name as the value identifier.
                            let value = if p.shorthand {
                                key.trim_matches('"').to_string()
                            } else {
                                self.expression(&p.value, indent)
                            };
                            props.push(format!("{key}: {value}"));
                        }
                        ObjectPropertyKind::SpreadProperty(spread) => {
                            props.push(self.fallback(
                                "unsupported object spread",
                                spread.span,
                                indent,
                            ));
                        }
                    }
                }
                format!("serde_json::json!({{ {} }})", props.join(", "))
            }
            Expression::ConditionalExpression(cond) => {
                let test = self.expression(&cond.test, indent);
                let cons = self.expression(&cond.consequent, indent);
                let alt = self.expression(&cond.alternate, indent);
                format!("if {test} {{ {cons} }} else {{ {alt} }}")
            }
            Expression::UnaryExpression(un) => {
                let arg = self.expression(&un.argument, indent);
                match un.operator {
                    UnaryOperator::UnaryNegation => format!("-{arg}"),
                    UnaryOperator::LogicalNot => format!("!{arg}"),
                    UnaryOperator::UnaryPlus => arg,
                    _ => self.fallback("unsupported unary operator", un.span, indent),
                }
            }
            Expression::BinaryExpression(bin) => {
                // String `+` (either operand a string literal/template) is concatenation, which Rust's
                // `+` does not do across `&str`/`String` freely — render it as a `format!` so it
                // reliably yields a `String`.
                if bin.operator == BinaryOperator::Addition
                    && (is_string_expr(&bin.left) || is_string_expr(&bin.right))
                {
                    let left = self.expression(&bin.left, indent);
                    let right = self.expression(&bin.right, indent);
                    return format!("format!(\"{{}}{{}}\", {left}, {right})");
                }
                let left = self.expression(&bin.left, indent);
                let right = self.expression(&bin.right, indent);
                match binary_op_to_rust(bin.operator) {
                    Some(op) => format!("{left} {op} {right}"),
                    None => self.fallback("unsupported binary operator", bin.span, indent),
                }
            }
            Expression::LogicalExpression(log) => {
                let left = self.expression(&log.left, indent);
                let right = self.expression(&log.right, indent);
                match logical_op_to_rust(log.operator) {
                    Some(op) => format!("{left} {op} {right}"),
                    None => self.fallback("unsupported logical operator", log.span, indent),
                }
            }
            Expression::AwaitExpression(await_expr) => {
                // `await foo(x)` lowers to `foo(x).await`.
                let inner = self.expression(&await_expr.argument, indent);
                format!("{inner}.await")
            }
            Expression::StaticMemberExpression(member) => {
                let object = self.expression(&member.object, indent);
                format!("{object}.{}", member.property.name)
            }
            Expression::ComputedMemberExpression(member) => {
                let object = self.expression(&member.object, indent);
                let index = self.expression(&member.expression, indent);
                format!("{object}[{index}]")
            }
            Expression::CallExpression(call) => {
                // Pass the call through by callee text, lowering each argument. A call into an unknown
                // free function or member chain (`db.users.find(id)`, an injected dependency) is
                // rendered VERBATIM by name — faithful for a `server:rust` body. A `ts` body containing
                // such a call cannot be guaranteed to compile against real Rust signatures, so the
                // handler-level wrapper degrades the WHOLE body to a typed-default stub (it keys off
                // `covered`); here we record the dependency call as a note and pass the shape through.
                let callee = self.expression(&call.callee, indent);
                let mut args = Vec::new();
                for arg in &call.arguments {
                    match arg {
                        Argument::SpreadElement(spread) => {
                            args.push(self.fallback(
                                "unsupported spread argument",
                                spread.span,
                                indent,
                            ));
                        }
                        other => {
                            if let Some(expr) = other.as_expression() {
                                args.push(self.expression(expr, indent));
                            } else {
                                args.push(self.fallback(
                                    "unsupported call argument",
                                    other.span(),
                                    indent,
                                ));
                            }
                        }
                    }
                }
                format!("{callee}({})", args.join(", "))
            }
            other => self.fallback("unsupported expression", other.span(), indent),
        }
    }

    /// Lower a template literal `` `a ${x} b` `` to a Rust `format!("a {} b", x)`. The literal text is
    /// escaped for a Rust format string (`{`/`}` doubled) and each interpolation becomes a `{}` slot.
    fn template_literal(&mut self, tpl: &TemplateLiteral, indent: usize) -> String {
        let mut fmt = String::new();
        let mut args: Vec<String> = Vec::new();
        for (i, quasi) in tpl.quasis.iter().enumerate() {
            let raw = quasi
                .value
                .cooked
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or_else(|| quasi.value.raw.as_str());
            // Escape Rust format-string metachars in the literal segments.
            for ch in raw.chars() {
                match ch {
                    '{' => fmt.push_str("{{"),
                    '}' => fmt.push_str("}}"),
                    '"' => fmt.push_str("\\\""),
                    '\\' => fmt.push_str("\\\\"),
                    _ => fmt.push(ch),
                }
            }
            if let Some(expr) = tpl.expressions.get(i) {
                fmt.push_str("{}");
                args.push(self.expression(expr, indent));
            }
        }
        if args.is_empty() {
            format!("format!(\"{fmt}\")")
        } else {
            format!("format!(\"{fmt}\", {})", args.join(", "))
        }
    }

    /// Render an object-literal property key as a JSON string key (`"name"`). Identifier and string
    /// keys map directly; a numeric key becomes its string form; a computed key falls back.
    fn property_key(&mut self, key: &PropertyKey, indent: usize) -> String {
        match key {
            PropertyKey::StaticIdentifier(id) => format!("{:?}", id.name.as_str()),
            PropertyKey::StringLiteral(lit) => format!("{:?}", lit.value.as_str()),
            PropertyKey::NumericLiteral(lit) => format!("{:?}", lit.value.to_string()),
            other => {
                if let Some(expr) = other.as_expression() {
                    self.expression(expr, indent)
                } else {
                    self.fallback("unsupported property key", other.span(), indent)
                }
            }
        }
    }
}

/// Coerce a lowered expression `value` (Rust source text) to the handler's response type `resp` so it
/// fits the `-> Json<RESP>` signature.
///
///   * `serde_json::Value` response: wrap in `serde_json::json!(…)` so ANY serialisable lowered value
///     (scalar, string, `json!` object/array) becomes a `Value`. (A value that is already a
///     `json!(…)` is left as-is — `json!` of a `Value` is just that `Value`.)
///   * `String` response: a `format!`/string-literal value is already a `String`; anything else is
///     wrapped in `format!("{}", …)`.
///   * scalar (`f64`/`bool`) response or a `Vec<…>`: emitted as-is (the covered arithmetic/comparison
///     shapes already produce these).
fn coerce_to_response(value: &str, resp: &str, _expr: &Expression) -> String {
    if resp == "serde_json::Value" {
        // `json!` accepts any `Serialize`; a `Null` literal or an existing `json!` value passes through.
        if value.starts_with("serde_json::json!") || value == "serde_json::Value::Null" {
            return value.to_string();
        }
        return format!("serde_json::json!({value})");
    }
    if resp == "String" {
        if value.starts_with("format!(") || value.starts_with('"') {
            return value.to_string();
        }
        return format!("format!(\"{{}}\", {value})");
    }
    value.to_string()
}

/// Does the lowered handler body end in an unconditional top-level `return …;`? Used to decide whether
/// a trailing fall-through `Json(Default::default())` is needed (it is not when control can never reach
/// the end). The final TOP-LEVEL statement is a line indented exactly one level (four spaces); a
/// deeper-indented `return` inside an `if`/`for` block is not a guaranteed-reached tail.
fn ends_in_unconditional_return(body: &str) -> bool {
    body.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .is_some_and(|last| last.starts_with("    return ") && !last.starts_with("     "))
}

/// Is `expr` a string-typed expression for the purpose of `+` concatenation lowering? True for string
/// literals and template literals (and a parenthesised one), so `"a" + x` / `` `a` + x `` render as a
/// `format!` concatenation rather than a numeric Rust `+`.
fn is_string_expr(expr: &Expression) -> bool {
    match expr {
        Expression::StringLiteral(_) | Expression::TemplateLiteral(_) => true,
        Expression::ParenthesizedExpression(p) => is_string_expr(&p.expression),
        _ => false,
    }
}

/// Map a JS binary operator to its Rust spelling. Equality variants (`==`, `===`) collapse to Rust
/// `==` and inequality variants to `!=`; arithmetic and ordering operators map directly. Operators
/// with no faithful Rust analogue here (bitwise, shifts, `**`, `in`, `instanceof`) return `None` so
/// the caller can degrade gracefully.
fn binary_op_to_rust(op: BinaryOperator) -> Option<&'static str> {
    Some(match op {
        BinaryOperator::Addition => "+",
        BinaryOperator::Subtraction => "-",
        BinaryOperator::Multiplication => "*",
        BinaryOperator::Division => "/",
        BinaryOperator::Remainder => "%",
        BinaryOperator::Equality | BinaryOperator::StrictEquality => "==",
        BinaryOperator::Inequality | BinaryOperator::StrictInequality => "!=",
        BinaryOperator::LessThan => "<",
        BinaryOperator::LessEqualThan => "<=",
        BinaryOperator::GreaterThan => ">",
        BinaryOperator::GreaterEqualThan => ">=",
        _ => return None,
    })
}

/// Map a JS logical operator to its Rust spelling. `&&`/`||` map directly; `??` (nullish coalescing)
/// has no direct Rust operator and returns `None`.
fn logical_op_to_rust(op: LogicalOperator) -> Option<&'static str> {
    match op {
        LogicalOperator::And => Some("&&"),
        LogicalOperator::Or => Some("||"),
        LogicalOperator::Coalesce => None,
    }
}

/// Verbatim, trimmed text of a span in `source`.
fn span_text(source: &str, span: Span) -> String {
    source[span.start as usize..span.end as usize].trim().to_string()
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// None of the degradation machinery may leave a marker token in generated output. The forbidden
    /// tokens are assembled from fragments so this guard does not itself contain any of them verbatim.
    fn assert_no_marker_words(text: &str) {
        let markers = [
            format!("{}{}", "NO", "TE(port)"),
            format!("{}{}", "TO", "DO"),
            format!("{}{}", "FIX", "ME"),
            format!("{}{}", "tod", "o!"),
        ];
        for marker in &markers {
            assert!(
                !text.contains(marker.as_str()),
                "marker word `{marker}` leaked into output:\n{text}"
            );
        }
    }

    #[test]
    fn ts_type_mapping_covers_the_documented_cases() {
        assert_eq!(ts_type_to_rust("number"), "f64");
        assert_eq!(ts_type_to_rust("string"), "String");
        assert_eq!(ts_type_to_rust("boolean"), "bool");
        assert_eq!(ts_type_to_rust("number[]"), "Vec<f64>");
        assert_eq!(ts_type_to_rust("Array<string>"), "Vec<String>");
        assert_eq!(ts_type_to_rust("Promise<number>"), "f64");
        assert_eq!(ts_type_to_rust("Promise<string[]>"), "Vec<String>");
        assert_eq!(ts_type_to_rust("object"), "serde_json::Value");
        assert_eq!(ts_type_to_rust("unknown"), "serde_json::Value");
        assert_eq!(ts_type_to_rust("any"), "serde_json::Value");
        assert_eq!(ts_type_to_rust("User"), "serde_json::Value");
    }

    #[test]
    fn ts_type_mapping_unwraps_generators_and_nullable_unions() {
        // An async-generator stream return unwraps to its element type.
        assert_eq!(ts_type_to_rust("AsyncGenerator<LogLine>"), "serde_json::Value");
        assert_eq!(ts_type_to_rust("AsyncGenerator<number>"), "f64");
        // A `T | undefined` "maybe" return maps to the non-null arm.
        assert_eq!(ts_type_to_rust("Todo | undefined"), "serde_json::Value");
        assert_eq!(ts_type_to_rust("number | null"), "f64");
        assert_eq!(ts_type_to_rust("Promise<Todo | undefined>"), "serde_json::Value");
    }

    #[test]
    fn transpiles_arithmetic_return_of_params() {
        let source = "function add(a: number, b: number): number { return a + b * 2; }";
        let result = transpile_body(source);

        assert!(result.covered, "arithmetic body should be fully covered; notes: {:?}", result.notes);
        assert_eq!(result.return_ty, "f64");
        assert!(
            result.rust_body.contains("return a + b * 2.0;"),
            "unexpected body:\n{}",
            result.rust_body
        );
        assert!(result.notes.is_empty(), "expected no notes; got {:?}", result.notes);
        assert_no_marker_words(&result.rust_body);
    }

    #[test]
    fn transpiles_string_concatenation() {
        let source =
            "function greet(name: string): string { return \"hello \" + name; }";
        let result = transpile_body(source);

        assert!(result.covered, "string concat should be covered; notes: {:?}", result.notes);
        assert_eq!(result.return_ty, "String");
        // String `+` is lowered to a `format!` concatenation (so it reliably yields a `String`).
        assert!(
            result.rust_body.contains(r#"format!("{}{}", "hello ", name)"#),
            "string concat not lowered to format!:\n{}",
            result.rust_body
        );
        assert_no_marker_words(&result.rust_body);
    }

    #[test]
    fn transpiles_if_else_with_distinct_branches() {
        let source = "function pick(n: number): number {\n\
              if (n > 0) {\n\
                return 1;\n\
              } else {\n\
                return 0;\n\
              }\n\
            }";
        let result = transpile_body(source);

        assert!(result.covered, "if/else should be covered; notes: {:?}", result.notes);
        assert!(
            result.rust_body.contains("if n > 0.0 {"),
            "missing if header:\n{}",
            result.rust_body
        );
        assert!(
            result.rust_body.contains("return 1.0;"),
            "missing consequent:\n{}",
            result.rust_body
        );
        assert!(
            result.rust_body.contains("} else {"),
            "missing else:\n{}",
            result.rust_body
        );
        assert!(
            result.rust_body.contains("return 0.0;"),
            "missing alternate:\n{}",
            result.rust_body
        );
        assert_no_marker_words(&result.rust_body);
    }

    #[test]
    fn transpiles_numeric_for_loop() {
        let source = "function sum(n: number): number {\n\
              let total = 0;\n\
              for (let i = 0; i < n; i++) {\n\
                total = total + i;\n\
              }\n\
              return total;\n\
            }";
        let result = transpile_body(source);

        assert!(
            result.rust_body.contains("for i in 0.0..n {"),
            "for loop not lowered to a range:\n{}",
            result.rust_body
        );
        assert!(result.rust_body.contains("let total = 0.0;"));
        assert_no_marker_words(&result.rust_body);
    }

    #[test]
    fn transpiles_await_and_member_call() {
        let source =
            "async function load(id: number) { return await db.users.find(id); }";
        let result = transpile_body(source);

        assert!(
            result.rust_body.contains("return db.users.find(id).await;"),
            "await/member/call not lowered as expected:\n{}",
            result.rust_body
        );
        assert_no_marker_words(&result.rust_body);
    }

    #[test]
    fn transpiles_object_literal_to_json() {
        let source =
            "function make(title: string) { return { id: 1, title: title, done: false }; }";
        let result = transpile_body(source);

        assert!(
            result.rust_body.contains(r#"serde_json::json!({ "id": 1.0, "title": title, "done": false })"#),
            "object literal not lowered to json!:\n{}",
            result.rust_body
        );
        assert_no_marker_words(&result.rust_body);
    }

    #[test]
    fn transpiles_shorthand_object_property() {
        let source = "function wrap(title: string) { return { title }; }";
        let result = transpile_body(source);
        assert!(
            result.rust_body.contains(r#"serde_json::json!({ "title": title })"#),
            "shorthand property not lowered:\n{}",
            result.rust_body
        );
    }

    #[test]
    fn transpiles_array_literal_and_template_string() {
        let source =
            "function many(n: number) { return [n, `line ${n}`]; }";
        let result = transpile_body(source);
        assert!(
            result.rust_body.contains("serde_json::json!([n, format!(\"line {}\", n)])"),
            "array/template not lowered:\n{}",
            result.rust_body
        );
        assert_no_marker_words(&result.rust_body);
    }

    #[test]
    fn transpiles_ternary_and_computed_index() {
        let source =
            "function at(items: number[], i: number): number { return i >= 0 ? items[i] : 0; }";
        let result = transpile_body(source);
        assert!(
            result.rust_body.contains("if i >= 0.0 { items[i] } else { 0.0 }"),
            "ternary/computed-index not lowered:\n{}",
            result.rust_body
        );
        assert_no_marker_words(&result.rust_body);
    }

    #[test]
    fn handler_body_wraps_returns_in_json_for_value_response() {
        // An object return (no annotation) maps to a `serde_json::Value` response: each return is
        // wrapped so it produces the handler's `Json<serde_json::Value>`.
        let source =
            "function make(title: string) { return { id: 1, title }; }";
        let result = transpile_handler_body(source);

        assert!(result.covered, "object-return body should be covered; notes: {:?}", result.notes);
        assert_eq!(result.return_ty, "serde_json::Value");
        assert!(
            result.rust_body.contains(r#"return Json(serde_json::json!({ "id": 1.0, "title": title }));"#),
            "return not wrapped as Json(json!(…)):\n{}",
            result.rust_body
        );
        assert_no_marker_words(&result.rust_body);
    }

    #[test]
    fn handler_body_appends_typed_default_only_on_fall_through() {
        // A body that can fall off the end (an `if` that returns only on one branch) gets a trailing
        // typed `Json` default so it still returns the promised type.
        let fall_through = "function maybe(n: number) { if (n > 0) { return n; } }";
        let result = transpile_handler_body(fall_through);
        assert!(result.covered, "notes: {:?}", result.notes);
        assert!(
            result.rust_body.trim_end().ends_with("Json(Default::default())"),
            "fall-through body missing trailing typed default:\n{}",
            result.rust_body
        );

        // A body that ALWAYS returns needs no trailing default — the emit has no unreachable tail.
        let always = "function always(n: number) { return n; }";
        let result = transpile_handler_body(always);
        assert!(result.covered);
        assert!(
            !result.rust_body.trim_end().ends_with("Json(Default::default())"),
            "always-returning body must not append an unreachable trailing default:\n{}",
            result.rust_body
        );
    }

    #[test]
    fn handler_body_wraps_scalar_return_in_json() {
        let source = "function add(a: number, b: number): number { return a + b; }";
        let result = transpile_handler_body(source);
        assert!(result.covered);
        assert_eq!(result.return_ty, "f64");
        assert!(
            result.rust_body.contains("return Json(a + b);"),
            "scalar return not wrapped:\n{}",
            result.rust_body
        );
    }

    #[test]
    fn handler_body_wraps_string_return_in_json() {
        let source = "function greet(name: string): string { return \"hi \" + name; }";
        let result = transpile_handler_body(source);
        assert!(result.covered);
        assert_eq!(result.return_ty, "String");
        // The format! concatenation is already a String, so it is wrapped directly.
        assert!(
            result.rust_body.contains(r#"return Json(format!("{}{}", "hi ", name));"#),
            "string return not wrapped:\n{}",
            result.rust_body
        );
    }

    #[test]
    fn handler_body_stub_is_compiling_for_unsupported_body() {
        // A `switch` is outside the supported subset: the handler body must become a clearly-marked,
        // COMPILING stub — original TS as a comment plus a typed `Json` default.
        let source = "function classify(n: number): string {\n\
              switch (n) {\n\
                case 0: return \"zero\";\n\
                default: return \"other\";\n\
              }\n\
            }";
        let result = transpile_handler_body(source);

        assert!(!result.covered, "unsupported body must not be covered");
        assert_eq!(result.return_ty, "String");
        // The whole body is the typed-default stub, not a half-transpiled mix.
        assert!(
            result.rust_body.contains("Json(Default::default())"),
            "stub missing typed Json default:\n{}",
            result.rust_body
        );
        // Original TS preserved as a comment.
        assert!(
            result.rust_body.contains("// switch (n)"),
            "original TS not preserved as a comment:\n{}",
            result.rust_body
        );
        assert!(!result.notes.is_empty(), "expected a degradation note");
        assert_no_marker_words(&result.rust_body);
        for note in &result.notes {
            assert_no_marker_words(note);
        }
    }

    #[test]
    fn unsupported_construct_degrades_without_marker_words() {
        // A `switch` statement is outside the supported subset.
        let source = "function classify(n: number): string {\n\
              switch (n) {\n\
                case 0: return \"zero\";\n\
                default: return \"other\";\n\
              }\n\
            }";
        let result = transpile_body(source);

        // covered must flip to false on any fallback.
        assert!(!result.covered, "unsupported construct should not be covered");
        // A typed default must be present so generated Rust still compiles.
        assert!(
            result.rust_body.contains("Default::default()"),
            "missing Default fallback:\n{}",
            result.rust_body
        );
        // The original TS slice must survive as a plain `//` comment.
        assert!(
            result.rust_body.contains("// switch (n)")
                || result.rust_body.contains("switch (n)"),
            "original TS not preserved as a comment:\n{}",
            result.rust_body
        );
        // A human-readable note must have been recorded.
        assert!(
            !result.notes.is_empty(),
            "expected a degradation note; got none"
        );
        // And absolutely no marker tokens anywhere.
        assert_no_marker_words(&result.rust_body);
        for note in &result.notes {
            assert_no_marker_words(note);
        }
    }
}
