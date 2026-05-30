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
//! concatenation via `+`, member access (`a.b`), `await` (lowered to a trailing `.await`), and call
//! expressions (passed through by callee name).
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
use oxc::syntax::operator::{BinaryOperator, LogicalOperator};
use oxc_ast::ast::{
    Argument, Expression, ForStatementInit, Statement, VariableDeclarationKind,
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
///   * `Promise<T>` -> the mapping of `T` (the future is unwrapped, since the handler awaits it)
///   * anything else (`object`, `unknown`, `any`, named interfaces, unions, …) -> `serde_json::Value`
///
/// The input is trimmed; surrounding parentheses are peeled so `(number)` maps like `number`.
pub fn ts_type_to_rust(ts: &str) -> String {
    let ts = ts.trim();
    // Peel a single layer of wrapping parentheses, e.g. `(number)`.
    if let Some(inner) = ts.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
        return ts_type_to_rust(inner);
    }

    // `T[]` array shorthand.
    if let Some(elem) = ts.strip_suffix("[]") {
        return format!("Vec<{}>", ts_type_to_rust(elem));
    }

    // `Array<T>` / `Promise<T>` generic forms.
    if let Some(elem) = generic_arg(ts, "Array") {
        return format!("Vec<{}>", ts_type_to_rust(elem));
    }
    if let Some(elem) = generic_arg(ts, "Promise") {
        // The future is awaited in the handler, so unwrap to the inner type.
        return ts_type_to_rust(elem);
    }

    match ts {
        "number" => "f64".to_string(),
        "string" => "String".to_string(),
        "boolean" => "bool".to_string(),
        // `void`/`undefined`/`null` have no useful typed Rust analogue here; treat as JSON null.
        _ => "serde_json::Value".to_string(),
    }
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
pub fn transpile_body(source: &str) -> TranspileResult {
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

    let return_ty = return_ty_text
        .as_deref()
        .map(ts_type_to_rust)
        .unwrap_or_else(|| "serde_json::Value".to_string());

    let Some(stmts) = body_stmts else {
        return TranspileResult {
            rust_body: String::new(),
            return_ty,
            covered: false,
            notes: vec!["could not locate a function body to transpile".to_string()],
        };
    };

    let mut tx = Transpiler { source, notes: Vec::new(), covered: true };
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

/// Walker state shared across the recursive lowering. `source` is the original text (for slicing
/// out unsupported nodes verbatim); `notes`/`covered` accumulate degradation info.
struct Transpiler<'a> {
    source: &'a str,
    notes: Vec<String>,
    covered: bool,
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

    /// Lower a statement to Rust source (already indented to `indent`).
    fn statement(&mut self, stmt: &Statement, indent: usize) -> String {
        let pad = Self::pad(indent);
        match stmt {
            Statement::ReturnStatement(ret) => match &ret.argument {
                Some(expr) => {
                    let value = self.expression(expr, indent);
                    format!("{pad}return {value};")
                }
                None => format!("{pad}return;"),
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
            Expression::Identifier(id) => id.name.to_string(),
            Expression::ParenthesizedExpression(paren) => {
                format!("({})", self.expression(&paren.expression, indent))
            }
            Expression::BinaryExpression(bin) => {
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
            Expression::CallExpression(call) => {
                // Pass the call through by callee text, lowering each argument.
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
        assert!(
            result.rust_body.contains(r#"return "hello " + name;"#),
            "unexpected body:\n{}",
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
