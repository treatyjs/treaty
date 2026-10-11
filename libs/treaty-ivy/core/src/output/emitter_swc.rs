//! The **SWC-backend emitter** — an engine-neutral printer over `output_ast`.
//!
//! # Why a neutral printer, not `swc_ecma_codegen`
//!
//! `migration/SWC-BACKEND-PLAN.md` §3.3 / §6 calls for the SWC backend to emit Ivy
//! **byte-identical** to the oxc backend, and offers two implementation strategies:
//! (a) lower `output_ast` → `swc_ecma_ast` and tune `swc_ecma_codegen`'s `Config` to match
//! `oxc_codegen`, or (b) a neutral hand-rolled printer over `output_ast` shared by both backends,
//! making byte-equality *structural*.
//!
//! Strategy (a) is not reachable here: `oxc_codegen`'s pretty form is **tab-indented**, prints
//! object literals multi-line iff they have `>1` property and arrays multi-line iff they have `>2`
//! elements, and keeps short arrays/objects compact (`[["app-btn"]]`, `goog.getMsg(..., { "k": "v" })`).
//! `swc_ecma_codegen`'s `Config` exposes only `minify` (collapse everything to one line) or its
//! default (4-**space** indent, every array element on its own line). Neither can be tuned to
//! `oxc_codegen`'s exact bytes, and reconciling them by text post-processing across nested
//! functions/objects/arrays is fragile. So this backend uses strategy (b): a printer that walks
//! the SAME owned `output_ast` the oxc emitter consumes and reproduces `oxc_codegen`'s pretty
//! algorithm directly.
//!
//! The two divergence classes the plan worries about (§3.4) both terminate in emitted text, so the
//! single byte-equality gate in `tools/backend-parity` is what proves this printer matches oxc.
//!
//! # What this reproduces from `oxc_codegen` 0.133
//!
//! - **Indent**: one `\t` per nesting level (`print_indent`).
//! - **Object literal**: empty `{}`; single-line `{ k: v }` for exactly one property; multi-line
//!   (one property per line, no trailing comma) for `len > 1` — matching `ObjectExpression::gen_expr`
//!   (`is_multi_line = len > 1`).
//! - **Array literal**: single-line for `len <= 2`, multi-line for `len > 2` — matching
//!   `ArrayExpression::gen` (`is_multi_line = elements.len() > 2`).
//! - **Statement-position wrap**: an object/function/arrow expression that begins an expression
//!   statement (or an arrow body) is wrapped in `( … )` — matching `start_of_stmt` /
//!   `start_of_arrow_expr` in `oxc_codegen`.
//! - **Precedence-based parenthesization**: binary/logical/conditional/assignment/unary operands are
//!   parenthesized exactly when their precedence requires it — matching `oxc_codegen`'s
//!   `GenExpr::print_expr(precedence, …)` dispatch.
//! - **Numeric literals**: the canonical JS `Number.prototype.toString()` decimal form (no oxc-style
//!   scientific collapse, so no post-pass needed).
//! - **Raw U+FFFD** in string/template text (the i18n placeholder magic chars) is emitted as the
//!   RAW code point — matching the oxc emitter and the `@angular/compiler` parity oracle (the
//!   `�` escaped spelling only ever appears in skipped compliance-macro goldens).
//!
//! Public surface mirrors `emitter.rs` exactly (the four functions + `build_definition_map`) so the
//! feature-gated dispatch in `emitter.rs` is a drop-in swap.

use std::cell::RefCell;

use crate::output::comments::{comment_lines as comment_lines_for, expr_comment_text, expr_has_identifier_head};
use crate::output::emit_options::{current_emit_options, EmitOptions};
use crate::output::source_map::{utf16_columns, utf16_offset_to_line_col, SourceMapBuilder};
use crate::output_ast::{
    self as o, ArrowBody, BinaryOperator, ExprKind, ImportUrl, LiteralMapEntry, LiteralValue,
    ParseSourceSpan, StmtKind, StmtModifier, UnaryOperator,
};

// ---------------------------------------------------------------------------
// Public API (mirrors emitter.rs)
// ---------------------------------------------------------------------------

/// Lower a slice of `output_ast` statements to JS (byte-identical to the oxc emitter).
pub fn emit_statements(stmts: &[o::Stmt]) -> String {
    emit_statements_with(stmts, &current_emit_options())
}

/// [`emit_statements`] with explicit [`EmitOptions`].
pub fn emit_statements_with(stmts: &[o::Stmt], opts: &EmitOptions) -> String {
    let mut p = Printer::with_options(*opts);
    p.print_program(stmts, None);
    p.finish()
}

/// Lower a single `output_ast` expression to JS (byte-identical to the oxc emitter).
pub fn emit_expression(expr: &o::Expr) -> String {
    emit_expression_with(expr, &current_emit_options())
}

/// [`emit_expression`] with explicit [`EmitOptions`].
pub fn emit_expression_with(expr: &o::Expr, opts: &EmitOptions) -> String {
    let mut p = Printer::with_options(*opts);
    p.print_program(&[], Some(expr));
    p.finish()
}

/// Map-aware [`emit_statements`]: returns the SAME bytes plus a v3 source map.
pub fn emit_statements_with_map(
    stmts: &[o::Stmt],
    file_name: &str,
    source_name: &str,
    source_content: &str,
) -> (String, String) {
    let mut p = Printer::with_anchor_tracking();
    p.print_program(stmts, None);
    let code = p.finish_keep_anchors();
    let anchors = p.take_anchors();
    let map = build_source_map(file_name, source_name, source_content, &code, &anchors);
    (code, map.to_json())
}

/// Map-aware [`emit_expression`]: returns the SAME bytes plus a v3 source map.
pub fn emit_expression_with_map(
    expr: &o::Expr,
    file_name: &str,
    source_name: &str,
    source_content: &str,
) -> (String, String) {
    let mut p = Printer::with_anchor_tracking();
    p.print_program(&[], Some(expr));
    let code = p.finish_keep_anchors();
    let anchors = p.take_anchors();
    let map = build_source_map(file_name, source_name, source_content, &code, &anchors);
    (code, map.to_json())
}

/// Build a v3 source-map JSON for `expr` already printed into `full_code` at `expr_offset`.
pub fn build_definition_map(
    file_name: &str,
    source_name: &str,
    source_content: &str,
    full_code: &str,
    expr_offset: usize,
    expr: &o::Expr,
) -> String {
    let mut p = Printer::with_anchor_tracking();
    p.print_program(&[], Some(expr));
    let _discarded = p.finish_keep_anchors();
    let anchors = p.take_anchors();

    let mut builder = SourceMapBuilder::new(file_name.to_string());
    let src_index = builder.add_source(source_name, Some(source_content.to_string()));
    let mut search_from = expr_offset.min(full_code.len());
    for anchor in &anchors {
        if anchor.token.is_empty() {
            continue;
        }
        let Some(rel) = full_code[search_from..].find(&anchor.token) else {
            continue;
        };
        let gen_byte = search_from + rel;
        search_from = gen_byte + anchor.token.len();
        let generated = generated_byte_to_line_col(full_code, gen_byte);
        let original = utf16_offset_to_line_col(source_content, anchor.span.start.offset);
        builder.add_segment(generated, src_index, original);
    }
    builder.to_json()
}

// ---------------------------------------------------------------------------
// Source-map anchors (identical model to emitter.rs)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct EmittedAnchor {
    span: ParseSourceSpan,
    token: String,
}

fn build_source_map(
    file_name: &str,
    source_name: &str,
    source_content: &str,
    code: &str,
    anchors: &[EmittedAnchor],
) -> SourceMapBuilder {
    let mut builder = SourceMapBuilder::new(file_name.to_string());
    let src_index = builder.add_source(source_name, Some(source_content.to_string()));
    let mut search_from = 0usize;
    for anchor in anchors {
        if anchor.token.is_empty() {
            continue;
        }
        let Some(rel) = code[search_from..].find(&anchor.token) else {
            continue;
        };
        let gen_byte = search_from + rel;
        search_from = gen_byte + anchor.token.len();
        let generated = generated_byte_to_line_col(code, gen_byte);
        let original = utf16_offset_to_line_col(source_content, anchor.span.start.offset);
        builder.add_segment(generated, src_index, original);
    }
    builder
}

fn generated_byte_to_line_col(
    code: &str,
    byte_offset: usize,
) -> crate::output::source_map::LineCol {
    let offset = byte_offset.min(code.len());
    let mut line: u32 = 0;
    let mut line_start = 0usize;
    for (i, b) in code.as_bytes().iter().enumerate() {
        if i >= offset {
            break;
        }
        if *b == b'\n' {
            line += 1;
            line_start = i + 1;
        }
    }
    let column = utf16_columns(&code[line_start..offset]);
    crate::output::source_map::LineCol::new(line, column)
}

// ---------------------------------------------------------------------------
// Precedence model (mirrors oxc_codegen's `Precedence` ladder, JS standard).
// ---------------------------------------------------------------------------

/// JS expression precedence, lowest → highest. Values are ordered so a stronger binding is a
/// larger number; an operand needs parens when its own precedence is **lower** than the context
/// it appears in (with the usual left/right associativity handling done at the call site).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Prec {
    /// `,` sequence — lowest.
    Comma,
    /// `... =>` arrow / `yield` / assignment.
    Assign,
    /// `?:`
    Conditional,
    /// `??`
    NullishCoalesce,
    /// `||`
    LogicalOr,
    /// `&&`
    LogicalAnd,
    /// `|`
    BitOr,
    /// `^`
    BitXor,
    /// `&`
    BitAnd,
    /// `== != === !==`
    Equality,
    /// `< > <= >= in instanceof`
    Relational,
    /// `<< >> >>>`
    Shift,
    /// `+ -`
    Additive,
    /// `* / %`
    Multiplicative,
    /// `**`
    Exponentiation,
    /// `! - + typeof void ~` (prefix unary)
    Unary,
    /// `f()` `new x()` `a.b` `a[b]` — call / member / new-with-args.
    Postfix,
    /// atoms: identifiers, literals, parenthesized, arrays, objects.
    Atom,
}

/// Precedence of a binary/logical operator (the precedence of the *expression* it forms).
fn binary_prec(op: BinaryOperator) -> Prec {
    use BinaryOperator::*;
    match op {
        Or => Prec::LogicalOr,
        And => Prec::LogicalAnd,
        NullishCoalesce => Prec::NullishCoalesce,
        BitwiseOr => Prec::BitOr,
        BitwiseAnd => Prec::BitAnd,
        Equals | NotEquals | Identical | NotIdentical => Prec::Equality,
        Lower | LowerEquals | Bigger | BiggerEquals | In | InstanceOf => Prec::Relational,
        Plus | Minus => Prec::Additive,
        Multiply | Divide | Modulo => Prec::Multiplicative,
        Exponentiation => Prec::Exponentiation,
        // Assignment operators form an assignment-precedence expression.
        _ => Prec::Assign,
    }
}

/// The logical operator class of an expression: `??`, or `&&`/`||` grouped together. Used to detect
/// the `??`-with-`&&`/`||` mix the grammar forbids without parentheses. Returns `None` for anything
/// that is not a top-level logical-binary expression.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LogicalClass {
    /// `??`
    Coalesce,
    /// `&&` or `||`
    AndOr,
}

fn logical_class(expr: &o::Expr) -> Option<LogicalClass> {
    match &expr.kind {
        ExprKind::Binary { op, .. } => match op {
            BinaryOperator::NullishCoalesce => Some(LogicalClass::Coalesce),
            BinaryOperator::And | BinaryOperator::Or => Some(LogicalClass::AndOr),
            _ => None,
        },
        // The printer strips an explicit `Parenthesized`, so a parenthesized logical operand still
        // forms the forbidden mix and must be re-parenthesized — look through it.
        ExprKind::Parenthesized(inner) => logical_class(inner),
        _ => None,
    }
}

/// Whether an operand of the binary operator `parent_op` must be FORCE-parenthesized because it would
/// otherwise form the grammar-forbidden `??`-with-`&&`/`||` mix (`a && b ?? c` is a SyntaxError; it
/// must be `(a && b) ?? c`). True iff the parent is one logical class and the operand is the OTHER.
fn nullish_mix_needs_parens(parent_op: BinaryOperator, operand: &o::Expr) -> bool {
    let parent = match parent_op {
        BinaryOperator::NullishCoalesce => LogicalClass::Coalesce,
        BinaryOperator::And | BinaryOperator::Or => LogicalClass::AndOr,
        _ => return false,
    };
    matches!(logical_class(operand), Some(child) if child != parent)
}

/// Whether the LEFT operand (base) of `**` must be force-parenthesized: a unary-prefixed base
/// (`-x`, `!x`, `typeof x`, `void x`, `+x`, `~x`) is a SyntaxError as a bare `**` base. Looks through
/// an explicit `Parenthesized` (the printer strips it) so the wrap is re-derived.
fn exponent_base_needs_parens(parent_op: BinaryOperator, base: &o::Expr) -> bool {
    if !matches!(parent_op, BinaryOperator::Exponentiation) {
        return false;
    }
    match &base.kind {
        // `++x ** 2` and `x++ ** 2` are UpdateExpressions, which the grammar allows as a `**` base.
        ExprKind::Unary { op, .. } if op.is_update() => false,
        ExprKind::Not(_) | ExprKind::Unary { .. } | ExprKind::Typeof(_) | ExprKind::Void(_) => true,
        ExprKind::Parenthesized(inner) => exponent_base_needs_parens(parent_op, inner),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Printer
// ---------------------------------------------------------------------------

struct Printer {
    out: String,
    indent: usize,
    imports: RefCell<ImportManager>,
    anchors: Option<RefCell<Vec<EmittedAnchor>>>,
    /// [`EmitOptions::escape_non_ascii`]: print string literals the TypeScript-printer way.
    escape_non_ascii: bool,
    /// An expression comment waiting for its leading identifier (see [`Printer::print_expr_inner`]).
    pending_head: Option<String>,
}

#[derive(Default)]
struct ImportManager {
    modules: Vec<(String, String)>,
}

impl ImportManager {
    fn alias_for(&mut self, module: &str) -> String {
        if let Some((_, alias)) = self.modules.iter().find(|(m, _)| m == module) {
            return alias.clone();
        }
        let alias = format!("i{}", self.modules.len());
        self.modules.push((module.to_string(), alias.clone()));
        alias
    }
}

impl Printer {
    fn with_options(opts: EmitOptions) -> Self {
        Printer {
            out: String::new(),
            indent: 0,
            imports: RefCell::new(ImportManager::default()),
            anchors: None,
            escape_non_ascii: opts.escape_non_ascii,
            pending_head: None,
        }
    }

    fn with_anchor_tracking() -> Self {
        Printer {
            out: String::new(),
            indent: 0,
            imports: RefCell::new(ImportManager::default()),
            anchors: Some(RefCell::new(Vec::new())),
            escape_non_ascii: current_emit_options().escape_non_ascii,
            pending_head: None,
        }
    }

    fn take_anchors(&self) -> Vec<EmittedAnchor> {
        self.anchors
            .as_ref()
            .map(|a| a.borrow().clone())
            .unwrap_or_default()
    }

    fn record_anchor(&self, span: &Option<ParseSourceSpan>, token: &str) {
        if let (Some(anchors), Some(span)) = (self.anchors.as_ref(), span) {
            anchors.borrow_mut().push(EmittedAnchor {
                span: span.clone(),
                token: token.to_string(),
            });
        }
    }

    // -- low-level emit ---------------------------------------------------

    fn push(&mut self, s: &str) {
        self.out.push_str(s);
    }
    fn push_ch(&mut self, c: char) {
        self.out.push(c);
    }
    fn newline(&mut self) {
        self.out.push('\n');
    }
    fn print_indent(&mut self) {
        for _ in 0..self.indent {
            self.out.push('\t');
        }
    }

    // -- program ----------------------------------------------------------

    /// Print a statement list followed (optionally) by a single trailing expression-statement.
    /// Both `emit_statements` (stmts, no expr) and `emit_expression` ([], expr) route here so the
    /// leading `import * as iN …` lines are emitted in first-seen order AFTER lowering everything.
    fn print_program(&mut self, stmts: &[o::Stmt], trailing_expr: Option<&o::Expr>) {
        // First render the body into a scratch buffer so the import manager has seen every
        // External reference, then prepend the import lines (mirrors the oxc emitter's two-phase
        // `namespace_import_stmts()` ordering).
        let mut body = String::new();
        std::mem::swap(&mut self.out, &mut body);

        // The first top-level item, when it is a bare string-literal expression statement, would
        // read as a directive (`"use strict";`), so oxc_codegen parenthesises it (`("svg");`,
        // codegen `Program` "Ensure first string literal is not a directive"). That only applies
        // when it is the first statement of the program, i.e. when no import line precedes it.
        let first_is_string = match (stmts.first(), trailing_expr) {
            (Some(stmt), _) => match &stmt.kind {
                StmtKind::Expression(expr) => is_string_literal_expr(expr),
                _ => false,
            },
            (None, Some(expr)) => is_string_literal_expr(expr),
            (None, None) => false,
        };
        let mut head_len = 0;
        let total = stmts.len() + usize::from(trailing_expr.is_some());
        for index in 0..total {
            self.print_indent();
            match stmts.get(index) {
                Some(stmt) => self.print_stmt(stmt),
                None => self.print_expr_stmt(trailing_expr.expect("index < total")),
            }
            self.newline();
            if index == 0 {
                head_len = self.out.len();
            }
        }

        std::mem::swap(&mut self.out, &mut body); // self.out is empty again; `body` holds the code
        // Emit imports first.
        let modules = self.imports.borrow().modules.clone();
        if first_is_string && modules.is_empty() && head_len > 0 {
            // The head is `"lit";\n` (print_expr_stmt + newline): rewrite it as `("lit");\n`.
            debug_assert!(body[..head_len].ends_with(";\n"));
            let (head, rest) = body.split_at(head_len);
            let literal = head.strip_suffix(";\n").unwrap_or(head);
            let mut wrapped = format!("({literal});\n");
            wrapped.push_str(rest);
            body = wrapped;
        }
        for (module, alias) in &modules {
            self.push("import * as ");
            self.push(alias);
            self.push(" from ");
            self.print_string_literal(module);
            self.push(";");
            self.newline();
        }
        self.push(&body);
    }

    fn finish(self) -> String {
        self.out
    }

    /// Like [`finish`] but keeps `self` alive so anchors can be drained afterwards.
    fn finish_keep_anchors(&self) -> String {
        self.out.clone()
    }

    // -- statements -------------------------------------------------------

    fn print_stmt(&mut self, stmt: &o::Stmt) {
        // Leading comments: each on its own line(s), then the statement at the same indentation. The
        // oxc emitter gets the same bytes by replacing one marker statement per comment.
        if !stmt.meta.leading_comments.is_empty() {
            let indent = "\t".repeat(self.indent);
            for comment in stmt.meta.leading_comments.iter().map(comment_lines_for) {
                self.push(&comment.join(&format!("\n{indent}")));
                self.newline();
                self.print_indent();
            }
        }
        match &stmt.kind {
            StmtKind::Expression(expr) => self.print_expr_stmt(expr),
            StmtKind::Return(expr) => {
                self.push("return ");
                self.print_expr(expr, Prec::Comma);
                self.push(";");
            }
            StmtKind::DeclareVar { name, value, .. } => {
                let kw = if stmt.meta.modifiers.has_modifier(StmtModifier::FINAL) {
                    "const"
                } else {
                    "let"
                };
                self.push(kw);
                self.push(" ");
                self.record_anchor(&stmt.meta.span, name);
                self.push(name);
                if let Some(v) = value {
                    self.push(" = ");
                    self.print_expr(v, Prec::Assign);
                }
                self.push(";");
            }
            StmtKind::DeclareFunction {
                name,
                params,
                statements,
                ..
            } => {
                self.record_anchor(&stmt.meta.span, name);
                self.push("function ");
                self.push(name);
                self.print_params(params);
                self.push(" ");
                self.print_block(statements);
            }
            StmtKind::If {
                condition,
                true_case,
                false_case,
            } => {
                self.push("if (");
                self.print_expr(condition, Prec::Comma);
                self.push(") ");
                self.print_block(true_case);
                if !false_case.is_empty() {
                    self.push(" else ");
                    self.print_block(false_case);
                }
            }
        }
    }

    /// Print an expression statement, wrapping the expression in `()` when it begins with a token
    /// that would otherwise be parsed as a statement (`{` object literal, `function`) — matching
    /// oxc_codegen's `start_of_stmt` wrap.
    fn print_expr_stmt(&mut self, expr: &o::Expr) {
        let needs_wrap = expr_starts_with_brace_or_function(expr);
        if needs_wrap {
            self.push("(");
            self.print_expr(expr, Prec::Comma);
            self.push(")");
        } else {
            self.print_expr(expr, Prec::Comma);
        }
        self.push(";");
    }

    fn print_block(&mut self, stmts: &[o::Stmt]) {
        if stmts.is_empty() {
            self.push("{}");
            return;
        }
        self.push("{");
        self.newline();
        self.indent += 1;
        for s in stmts {
            self.print_indent();
            self.print_stmt(s);
            self.newline();
        }
        self.indent -= 1;
        self.print_indent();
        self.push("}");
    }

    fn print_params(&mut self, params: &[o::FnParam]) {
        self.push("(");
        for (i, p) in params.iter().enumerate() {
            if i != 0 {
                self.push(", ");
            }
            self.push(&p.name);
        }
        self.push(")");
    }

    // -- expressions ------------------------------------------------------

    /// Print `expr`, parenthesizing iff its own precedence is below `ctx` (the minimum precedence
    /// allowed in this position without parens).
    fn print_expr(&mut self, expr: &o::Expr, ctx: Prec) {
        let prec = expr_prec(expr);
        let need = prec < ctx;
        if need {
            self.push("(");
            self.print_expr_inner(expr);
            self.push(")");
        } else {
            self.print_expr_inner(expr);
        }
    }

    /// Emit the parked expression comment (with its separating space), if any.
    fn print_pending_head(&mut self) {
        if let Some(text) = self.pending_head.take() {
            self.push(&text);
            self.push(" ");
        }
    }

    fn print_expr_inner(&mut self, expr: &o::Expr) {
        // An expression comment is printed directly before the expression's leading identifier, so it is
        // parked here and emitted when that identifier is printed (after any opening parenthesis).
        if !expr.meta.leading_comments.is_empty() && expr_has_identifier_head(expr) {
            self.pending_head = Some(expr_comment_text(&expr.meta.leading_comments));
        }
        match &expr.kind {
            ExprKind::ReadVar { name } => {
                self.print_pending_head();
                self.record_anchor(&expr.meta.span, name);
                self.push(name);
            }
            ExprKind::Literal(v) => self.print_literal(v),
            ExprKind::External { value, .. } => match &value.module_name {
                Some(module) if !module.is_empty() => {
                    let alias = self.imports.borrow_mut().alias_for(module);
                    self.print_pending_head();
                    self.push(&alias);
                    self.push(".");
                    self.push(&value.name);
                }
                _ => {
                    self.print_pending_head();
                    self.push(&value.name);
                }
            },
            ExprKind::Invoke {
                callee,
                args,
                optional,
                ..
            } => {
                // A function / arrow callee must be parenthesized — `function(){}()` would parse as a
                // function DECLARATION followed by `()`, so the IIFE form is `(function(){})()`.
                // `oxc_codegen` wraps the callee (not the whole call), which also means the enclosing
                // statement no longer begins with the `function` keyword. Mirror it byte-for-byte.
                if expr_is_callee_needing_parens(callee) {
                    self.print_expr_forced(callee);
                } else {
                    self.print_expr(callee, Prec::Postfix);
                }
                if *optional {
                    self.push("?.");
                }
                self.print_call_args(args);
            }
            ExprKind::New { class_expr, args } => {
                self.push("new ");
                self.print_expr(class_expr, Prec::Postfix);
                self.print_call_args(args);
            }
            ExprKind::ReadProp {
                receiver,
                name,
                optional,
            } => {
                self.print_expr(receiver, Prec::Postfix);
                self.push(if *optional { "?." } else { "." });
                self.push(name);
            }
            ExprKind::ReadKey {
                receiver,
                index,
                optional,
            } => {
                self.print_expr(receiver, Prec::Postfix);
                if *optional {
                    self.push("?.[");
                } else {
                    self.push("[");
                }
                self.print_expr(index, Prec::Comma);
                self.push("]");
            }
            ExprKind::Conditional {
                condition,
                true_case,
                false_case,
            } => {
                // test: needs precedence ABOVE Conditional (test binds tighter than ?:).
                self.print_expr(condition, Prec::NullishCoalesce);
                self.push(" ? ");
                // branches are assignment-precedence.
                self.print_expr(true_case, Prec::Assign);
                self.push(" : ");
                match false_case {
                    Some(f) => self.print_expr(f, Prec::Assign),
                    None => self.push("undefined"),
                }
            }
            ExprKind::Not(inner) => {
                self.push("!");
                self.print_expr(inner, Prec::Unary);
            }
            ExprKind::Unary { op, expr, is_prefix, .. } => match op {
                UnaryOperator::Plus => {
                    self.push("+");
                    self.print_expr(expr, Prec::Unary);
                }
                UnaryOperator::Minus => {
                    self.push("-");
                    self.print_expr(expr, Prec::Unary);
                }
                // `++x` / `--x` bind like other prefix unaries; `x++` / `x--` need a left operand
                // that binds at least as tightly as a member access (ESTree UpdateExpression).
                UnaryOperator::Increment | UnaryOperator::Decrement => {
                    let text = if *op == UnaryOperator::Increment { "++" } else { "--" };
                    if *is_prefix {
                        self.push(text);
                        self.print_expr(expr, Prec::Unary);
                    } else {
                        self.print_expr(expr, Prec::Postfix);
                        self.push(text);
                    }
                }
            },
            ExprKind::Typeof(inner) => {
                self.push("typeof ");
                self.print_expr(inner, Prec::Unary);
            }
            ExprKind::Void(inner) => {
                self.push("void ");
                self.print_expr(inner, Prec::Unary);
            }
            ExprKind::Binary { op, lhs, rhs } => self.print_binary(*op, lhs, rhs),
            ExprKind::LiteralArray(entries) => self.print_array(entries),
            ExprKind::LiteralMap { entries, .. } => self.print_object(entries),
            ExprKind::Comma(parts) => {
                for (i, p) in parts.iter().enumerate() {
                    if i != 0 {
                        self.push(", ");
                    }
                    self.print_expr(p, Prec::Assign);
                }
            }
            ExprKind::Parenthesized(inner) => {
                // The precedence machinery already inserts parens where the grammar needs them; an
                // explicit ParenthesizedExpr just prints its inner expression (oxc_codegen also
                // re-derives parens by precedence rather than honoring an explicit paren node).
                self.print_expr_inner(inner);
            }
            ExprKind::Spread(inner) => {
                self.push("...");
                self.print_expr(inner, Prec::Assign);
            }
            ExprKind::Function {
                params,
                statements,
                name,
            } => {
                self.push("function");
                if let Some(n) = name {
                    self.push(" ");
                    self.push(n);
                }
                if name.is_none() {
                    // anonymous: `function(...)` has no space before params in oxc.
                }
                self.print_params(params);
                self.push(" ");
                self.print_block(statements);
            }
            ExprKind::Arrow { params, body } => self.print_arrow(params, body),
            ExprKind::TemplateLiteral {
                elements,
                expressions,
            } => self.print_template_literal(elements, expressions),
            ExprKind::TemplateLiteralElement(el) => {
                self.print_template_literal(std::slice::from_ref(el), &[])
            }
            ExprKind::TaggedTemplate { tag, template } => {
                self.print_expr(tag, Prec::Postfix);
                if let ExprKind::TemplateLiteral {
                    elements,
                    expressions,
                } = &template.kind
                {
                    self.print_template_literal(elements, expressions);
                } else {
                    self.print_template_literal(&[], &[]);
                }
            }
            ExprKind::RegExpLiteral { body, flags } => {
                self.push("/");
                self.push(body);
                self.push("/");
                if let Some(f) = flags {
                    self.push(f);
                }
            }
            ExprKind::LocalizedString {
                meta,
                message_parts,
                placeholders,
                expressions,
            } => self.print_localized_string(meta, message_parts, placeholders, expressions),
            ExprKind::WrappedNode(_) => self.push("__unsupported_WrappedNode"),
            ExprKind::DynamicImport { url, .. } => {
                self.push("import(");
                match url {
                    ImportUrl::Str(s) => self.print_string_literal(s),
                    ImportUrl::Expr(e) => self.print_expr(e, Prec::Assign),
                }
                self.push(")");
            }
        }
    }

    fn print_call_args(&mut self, args: &[o::Expr]) {
        self.push("(");
        for (i, a) in args.iter().enumerate() {
            if i != 0 {
                self.push(", ");
            }
            if let ExprKind::Spread(inner) = &a.kind {
                self.push("...");
                self.print_expr(inner, Prec::Assign);
            } else {
                self.print_expr(a, Prec::Assign);
            }
        }
        self.push(")");
    }

    fn print_binary(&mut self, op: BinaryOperator, lhs: &o::Expr, rhs: &o::Expr) {
        if op.is_assignment() {
            // Assignment is right-associative; LHS binds at Postfix (it's a reference target),
            // RHS at Assign precedence.
            self.print_expr(lhs, Prec::Postfix);
            self.push(" ");
            self.push(assignment_op_str(op));
            self.push(" ");
            self.print_expr(rhs, Prec::Assign);
            return;
        }
        let prec = binary_prec(op);
        // Left-associative for all standard ops except `**` (right-assoc). Left operand may bind at
        // the operator's own precedence; right operand must bind strictly tighter (one step up),
        // except `**` which flips.
        let (left_ctx, right_ctx) = if matches!(op, BinaryOperator::Exponentiation) {
            (prec_step_up(prec), prec)
        } else {
            (prec, prec_step_up(prec))
        };
        // The `??` operator may NOT be combined with `&&` or `||` without explicit parentheses — the
        // grammar forbids the un-parenthesized mix outright (it is a SyntaxError), so the operand must
        // be wrapped regardless of numeric precedence. `oxc_codegen` parenthesizes this case; mirror it
        // so `(a && b) ?? c` / `a ?? (b || c)` survive the neutral printer byte-for-byte.
        // The base of `**` may NOT be an un-parenthesized unary expression — `-1 ** 3` is a
        // SyntaxError, so it must read `(-1) ** 3`. `oxc_codegen` parenthesizes a unary `**` base;
        // mirror it (the precedence model alone leaves them at equal `Unary` precedence → no wrap).
        if nullish_mix_needs_parens(op, lhs) || exponent_base_needs_parens(op, lhs) {
            self.print_expr_forced(lhs);
        } else {
            self.print_expr(lhs, left_ctx);
        }
        self.push(" ");
        self.push(binary_op_str(op));
        self.push(" ");
        if nullish_mix_needs_parens(op, rhs) {
            self.print_expr_forced(rhs);
        } else {
            self.print_expr(rhs, right_ctx);
        }
    }

    /// Print `expr` ALWAYS wrapped in parentheses — used where the grammar forbids the bare form
    /// regardless of precedence (the `??`/`&&`/`||` mix).
    fn print_expr_forced(&mut self, expr: &o::Expr) {
        self.push("(");
        self.print_expr_inner(expr);
        self.push(")");
    }

    fn print_arrow(&mut self, params: &[o::FnParam], body: &ArrowBody) {
        // A single bare-identifier parameter prints WITHOUT parens (`x => …`), matching the oxc
        // emitter's `drop_single_param_arrow_parens` post-pass; everything else (zero params,
        // multiple params, or a non-identifier single param such as a `...rest` spread, whose
        // `FnParam.name` is the literal `"...rest"`) keeps its parens.
        if params.len() == 1 && is_bare_identifier(&params[0].name) {
            self.push(&params[0].name);
        } else {
            self.print_params(params);
        }
        self.push(" => ");
        match body {
            ArrowBody::Block(stmts) => self.print_block(stmts),
            ArrowBody::Expr(e) => {
                // An object-literal arrow body must be wrapped in `()` so `=> {` is not parsed as a
                // block (oxc_codegen's `start_of_arrow_expr` wrap).
                if expr_starts_with_brace_or_function(e) {
                    self.push("(");
                    self.print_expr(e, Prec::Assign);
                    self.push(")");
                } else {
                    self.print_expr(e, Prec::Assign);
                }
            }
        }
    }

    // -- literals ---------------------------------------------------------

    fn print_literal(&mut self, v: &LiteralValue) {
        match v {
            LiteralValue::String(s) => self.print_string_literal(s),
            LiteralValue::Number(n) => {
                let s = format_number(*n);
                self.push(&s);
            }
            LiteralValue::Bool(b) => self.push(if *b { "true" } else { "false" }),
            LiteralValue::Null => self.push("null"),
            LiteralValue::Undefined => self.push("undefined"),
        }
    }

    /// Print a double-quoted string literal with JS escapes, matching oxc_codegen's string output
    /// (which prefers double quotes) plus the emitter's `�` escape for i18n placeholders.
    fn print_string_literal(&mut self, s: &str) {
        if self.escape_non_ascii {
            self.print_string_literal_escaped(s);
            return;
        }
        self.push_ch('"');
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '"' => self.push("\\\""),
                '\\' => self.push("\\\\"),
                '\n' => self.push("\\n"),
                '\r' => self.push("\\r"),
                '\t' => self.push("\\t"),
                '\u{08}' => self.push("\\b"),
                '\u{0C}' => self.push("\\f"),
                '\u{0B}' => self.push("\\v"),
                '\u{07}' => self.push("\\x07"),
                '\u{1B}' => self.push("\\x1B"),
                // oxc_codegen: a NUL emits `\x00` when the NEXT char is an ASCII digit (so it does not
                // merge into a longer octal escape), else the short `\0`.
                '\u{00}' => {
                    if chars.peek().is_some_and(|n| n.is_ascii_digit()) {
                        self.push("\\x00");
                    } else {
                        self.push("\\0");
                    }
                }
                '\u{2028}' => self.push("\\u2028"),
                '\u{2029}' => self.push("\\u2029"),
                // NON-BREAKING SPACE (U+00A0): oxc_codegen escapes it as `\xA0` (its
                // `print_non_breaking_space`), NOT the raw byte. Mirror it for byte-identity.
                '\u{A0}' => self.push("\\xA0"),
                // U+FFFD (the i18n placeholder magic char) is emitted as the RAW code point, exactly
                // as `@angular/compiler`'s real emitter and oxc_codegen do — NOT the `�` escape
                // (that form only appears in the skipped `String.raw` compliance-macro goldens). This
                // keeps the swc backend byte-identical to the oxc backend and the parity oracle.
                _ => self.push_ch(c),
            }
        }
        self.push_ch('"');
    }

    /// [`EmitOptions::escape_non_ascii`] form: TypeScript's `escapeNonAsciiString(escapeString(s))`.
    /// Control characters use TypeScript's table (`\0`/`\x00` for NUL, the named escapes, then
    /// `\uXXXX`), and every non-ASCII UTF-16 code unit is `\uXXXX`.
    fn print_string_literal_escaped(&mut self, s: &str) {
        self.push_ch('"');
        let mut chars = s.char_indices();
        let mut out = String::new();
        while let Some((i, c)) = chars.next() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{08}' => out.push_str("\\b"),
                '\u{0C}' => out.push_str("\\f"),
                '\u{0B}' => out.push_str("\\v"),
                '\u{00}' => {
                    // TypeScript: NUL is `\0`, but `\x00` when a decimal digit follows it.
                    let next_is_digit = s[i + 1..].chars().next().is_some_and(|n| n.is_ascii_digit());
                    out.push_str(if next_is_digit { "\\x00" } else { "\\0" });
                }
                c if (c as u32) < 0x20 => {
                    out.push_str(&crate::output::emit_options::utf16_escape(c as u16));
                }
                c if !c.is_ascii() => {
                    let mut buf = [0u16; 2];
                    for unit in c.encode_utf16(&mut buf) {
                        out.push_str(&crate::output::emit_options::utf16_escape(*unit));
                    }
                }
                c => out.push(c),
            }
        }
        self.push(&out);
        self.push_ch('"');
    }

    fn print_array(&mut self, entries: &[o::Expr]) {
        // oxc_codegen: is_multi_line = elements.len() > 2.
        let multi = entries.len() > 2;
        self.push("[");
        if multi {
            self.indent += 1;
            for (i, e) in entries.iter().enumerate() {
                if i != 0 {
                    self.push(",");
                }
                self.newline();
                self.print_indent();
                self.print_array_element(e);
            }
            self.newline();
            self.indent -= 1;
            self.print_indent();
        } else {
            for (i, e) in entries.iter().enumerate() {
                if i != 0 {
                    self.push(", ");
                }
                self.print_array_element(e);
            }
        }
        self.push("]");
    }

    fn print_array_element(&mut self, e: &o::Expr) {
        if let ExprKind::Spread(inner) = &e.kind {
            self.push("...");
            self.print_expr(inner, Prec::Assign);
        } else {
            self.print_expr(e, Prec::Assign);
        }
    }

    fn print_object(&mut self, entries: &[LiteralMapEntry]) {
        if entries.is_empty() {
            self.push("{}");
            return;
        }
        // oxc_codegen: is_multi_line = properties.len() > 1.
        let multi = entries.len() > 1;
        self.push("{");
        if multi {
            self.indent += 1;
            for (i, e) in entries.iter().enumerate() {
                if i != 0 {
                    self.push(",");
                }
                self.newline();
                self.print_indent();
                self.print_object_entry(e);
            }
            self.newline();
            self.indent -= 1;
            self.print_indent();
        } else {
            self.push(" ");
            self.print_object_entry(&entries[0]);
            self.push(" ");
        }
        self.push("}");
    }

    fn print_object_entry(&mut self, entry: &LiteralMapEntry) {
        match entry {
            LiteralMapEntry::Property { key, value, quoted, .. } => {
                let ident_key = !*quoted && is_valid_ident_name(key);
                // SHORTHAND: `{ child }` when an unquoted identifier key equals a same-named variable
                // value. `oxc_codegen` emits this regardless of the AST `shorthand` flag (it compares
                // key vs value), so mirror it for byte-identity.
                if ident_key {
                    if let ExprKind::ReadVar { name } = &value.kind {
                        if name == key {
                            self.record_anchor(&value.meta.span, name);
                            self.push(key);
                            return;
                        }
                    }
                }
                if ident_key {
                    self.push(key);
                } else {
                    self.print_string_literal(key);
                }
                self.push(": ");
                self.print_expr(value, Prec::Assign);
            }
            LiteralMapEntry::Spread { expression } => {
                self.push("...");
                self.print_expr(expression, Prec::Assign);
            }
        }
    }

    // -- template / $localize --------------------------------------------

    fn print_template_literal(
        &mut self,
        elements: &[o::TemplateLiteralElement],
        expressions: &[o::Expr],
    ) {
        self.push_ch('`');
        if elements.is_empty() {
            self.push_ch('`');
            return;
        }
        let last = elements.len() - 1;
        for (i, el) in elements.iter().enumerate() {
            self.push(&template_raw_escape(&el.raw_text));
            if i != last {
                if let Some(e) = expressions.get(i) {
                    self.push("${");
                    self.print_expr(e, Prec::Comma);
                    self.push("}");
                }
            }
        }
        self.push_ch('`');
    }

    fn print_localized_string(
        &mut self,
        meta: &o::I18nMeta,
        message_parts: &[o::LiteralPiece],
        placeholders: &[o::PlaceholderPiece],
        expressions: &[o::Expr],
    ) {
        self.push("$localize");
        self.push_ch('`');
        let n = message_parts.len();
        if n == 0 {
            self.push_ch('`');
            return;
        }
        let head = serialize_i18n_head(meta, &message_parts[0].text);
        self.push(&template_raw_escape(&head.1));
        for i in 1..n {
            if let Some(e) = expressions.get(i - 1) {
                self.push("${");
                self.print_expr(e, Prec::Comma);
                self.push("}");
            }
            let part = serialize_i18n_template_part(&placeholders[i - 1], &message_parts[i].text);
            self.push(&template_raw_escape(&part.1));
        }
        self.push_ch('`');
    }
}

// ---------------------------------------------------------------------------
// Expression precedence + structural helpers.
// ---------------------------------------------------------------------------

/// The precedence of an `output_ast` expression (what it would bind as in a parent context).
fn expr_prec(expr: &o::Expr) -> Prec {
    match &expr.kind {
        ExprKind::ReadVar { .. }
        | ExprKind::Literal(_)
        | ExprKind::External { .. }
        | ExprKind::LiteralArray(_)
        | ExprKind::LiteralMap { .. }
        | ExprKind::TemplateLiteral { .. }
        | ExprKind::TemplateLiteralElement(_)
        | ExprKind::RegExpLiteral { .. }
        | ExprKind::LocalizedString { .. }
        | ExprKind::WrappedNode(_)
        | ExprKind::DynamicImport { .. } => Prec::Atom,
        // The printer STRIPS an explicit `Parenthesized` and re-derives parens by precedence (matching
        // `oxc_codegen`, which ignores the explicit paren node). So a parenthesized operand binds as
        // its INNER expression for the purpose of deciding whether the position needs parens — NOT as
        // an atom (treating it as an atom would wrongly suppress every re-derived paren, e.g. dropping
        // the required `(a && b) ?? c` / `"x" + (a ?? b)` wraps).
        ExprKind::Parenthesized(inner) => expr_prec(inner),
        ExprKind::Invoke { .. }
        | ExprKind::ReadProp { .. }
        | ExprKind::ReadKey { .. }
        | ExprKind::TaggedTemplate { .. } => Prec::Postfix,
        // `new X(args)` binds like a member/call; `new X` without args is also Postfix here since
        // our IR always carries an args vec.
        ExprKind::New { .. } => Prec::Postfix,
        // A postfix update (`x++` / `x--`) binds as a postfix expression; every other unary is prefix.
        ExprKind::Unary { is_prefix: false, op, .. } if op.is_update() => Prec::Postfix,
        ExprKind::Not(_) | ExprKind::Unary { .. } | ExprKind::Typeof(_) | ExprKind::Void(_) => {
            Prec::Unary
        }
        ExprKind::Binary { op, .. } => {
            if op.is_assignment() {
                Prec::Assign
            } else {
                binary_prec(*op)
            }
        }
        ExprKind::Conditional { .. } => Prec::Conditional,
        ExprKind::Comma(_) => Prec::Comma,
        ExprKind::Spread(_) => Prec::Assign,
        ExprKind::Function { .. } => Prec::Atom,
        ExprKind::Arrow { .. } => Prec::Assign,
    }
}

/// One precedence level above `p` (used for the tighter operand of a left-associative binary op).
fn prec_step_up(p: Prec) -> Prec {
    use Prec::*;
    match p {
        Comma => Assign,
        Assign => Conditional,
        Conditional => NullishCoalesce,
        NullishCoalesce => LogicalOr,
        LogicalOr => LogicalAnd,
        LogicalAnd => BitOr,
        BitOr => BitXor,
        BitXor => BitAnd,
        BitAnd => Equality,
        Equality => Relational,
        Relational => Shift,
        Shift => Additive,
        Additive => Multiplicative,
        Multiplicative => Exponentiation,
        Exponentiation => Unary,
        Unary => Postfix,
        Postfix => Atom,
        Atom => Atom,
    }
}

/// Whether a CALL/NEW callee must be parenthesized to read as a call target: a function or arrow
/// expression (`(function(){})()` / `(() => …)()`). `oxc_codegen` wraps exactly these callee shapes.
fn expr_is_callee_needing_parens(callee: &o::Expr) -> bool {
    matches!(&callee.kind, ExprKind::Function { .. } | ExprKind::Arrow { .. })
}

/// Does the expression begin with `{` (object literal) or the `function` keyword, requiring a
/// statement-position / arrow-body wrap?
/// Is `expr` a string literal once explicit parentheses are looked through? (oxc's
/// `Expression::without_parentheses` check in its `Program` codegen.)
fn is_string_literal_expr(expr: &o::Expr) -> bool {
    match &expr.kind {
        ExprKind::Literal(LiteralValue::String(_)) => true,
        ExprKind::Parenthesized(inner) => is_string_literal_expr(inner),
        _ => false,
    }
}

fn expr_starts_with_brace_or_function(expr: &o::Expr) -> bool {
    match &expr.kind {
        ExprKind::LiteralMap { .. } => true,
        ExprKind::Function { .. } => true,
        // A binary/member/call expression whose left-most atom is an object/function also starts
        // with that token. Walk the left spine.
        ExprKind::Binary { lhs, .. } => expr_starts_with_brace_or_function(lhs),
        ExprKind::ReadProp { receiver, .. } => expr_starts_with_brace_or_function(receiver),
        ExprKind::ReadKey { receiver, .. } => expr_starts_with_brace_or_function(receiver),
        // A function/arrow callee is wrapped in parens by the `Invoke` printer (the IIFE
        // `(function(){})()` form), so such a call begins with `(`, NOT the `function` keyword — it
        // therefore needs no extra statement-position wrap. Any other callee still propagates.
        ExprKind::Invoke { callee, .. } if expr_is_callee_needing_parens(callee) => false,
        ExprKind::Invoke { callee, .. } => expr_starts_with_brace_or_function(callee),
        ExprKind::Conditional { condition, .. } => expr_starts_with_brace_or_function(condition),
        ExprKind::TaggedTemplate { tag, .. } => expr_starts_with_brace_or_function(tag),
        // oxc_codegen re-derives parens by precedence and ignores an explicit ParenthesizedExpr, so
        // a parenthesized object/function at statement start is still wrapped by `start_of_stmt`.
        ExprKind::Parenthesized(inner) => expr_starts_with_brace_or_function(inner),
        _ => false,
    }
}

/// Is `name` a single bare JS identifier (so a lone arrow parameter need not be parenthesized)?
/// Mirrors the oxc emitter's `drop_single_param_arrow_parens` recognition: a `...rest` spread name
/// (which starts with `.`) is NOT bare and therefore keeps its parens, as does any name containing
/// a separator (commas, `=`, `:`).
fn is_bare_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c == '_' || c == '$' || c.is_ascii_alphabetic() || (c as u32) >= 0x80 => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c == '$' || c.is_ascii_alphanumeric() || (c as u32) >= 0x80)
}

/// Is `name` a valid bare object-property identifier (so it need not be quoted)?
fn is_valid_ident_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c == '_' || c == '$' || c.is_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c == '$' || c.is_alphanumeric())
}

// ---------------------------------------------------------------------------
// Operator spellings.
// ---------------------------------------------------------------------------

fn binary_op_str(op: BinaryOperator) -> &'static str {
    use BinaryOperator::*;
    match op {
        Equals => "==",
        NotEquals => "!=",
        Identical => "===",
        NotIdentical => "!==",
        Minus => "-",
        Plus => "+",
        Divide => "/",
        Multiply => "*",
        Modulo => "%",
        And => "&&",
        Or => "||",
        BitwiseOr => "|",
        BitwiseAnd => "&",
        Lower => "<",
        LowerEquals => "<=",
        Bigger => ">",
        BiggerEquals => ">=",
        NullishCoalesce => "??",
        Exponentiation => "**",
        In => "in",
        InstanceOf => "instanceof",
        // assignment operators handled by assignment_op_str
        _ => "==",
    }
}

fn assignment_op_str(op: BinaryOperator) -> &'static str {
    use BinaryOperator::*;
    match op {
        Assign => "=",
        AdditionAssignment => "+=",
        SubtractionAssignment => "-=",
        MultiplicationAssignment => "*=",
        DivisionAssignment => "/=",
        RemainderAssignment => "%=",
        ExponentiationAssignment => "**=",
        AndAssignment => "&&=",
        OrAssignment => "||=",
        NullishCoalesceAssignment => "??=",
        _ => "=",
    }
}

// ---------------------------------------------------------------------------
// Numeric literal formatting (JS Number.prototype.toString, matching the oxc emitter).
// ---------------------------------------------------------------------------

fn format_number(n: f64) -> String {
    if !n.is_finite() {
        if n.is_nan() {
            return "NaN".to_string();
        }
        return if n < 0.0 { "-Infinity".to_string() } else { "Infinity".to_string() };
    }
    if n == 0.0 {
        return "0".to_string();
    }
    let abs = n.abs();
    if abs >= 1e21 || abs < 1e-6 {
        return format!("{n:e}");
    }
    format!("{n}")
}

// ---------------------------------------------------------------------------
// Template-literal raw text. The output_ast already stores `raw_text` pre-escaped for backtick /
// `${`; oxc_codegen emits that raw text verbatim. A raw U+FFFD (the i18n placeholder magic char)
// is emitted as the RAW code point — matching `@angular/compiler`'s real emitter, oxc_codegen, and
// the parity oracle (the `�` escaped form only appears in skipped compliance-macro goldens).
// ---------------------------------------------------------------------------

fn template_raw_escape(raw: &str) -> String {
    raw.to_string()
}

// ---------------------------------------------------------------------------
// `$localize` cooked/raw serialization (mirrors emitter.rs exactly).
// ---------------------------------------------------------------------------

const MEANING_SEPARATOR: &str = "|";
const ID_SEPARATOR: &str = "@@";
const LEGACY_ID_INDICATOR: &str = "\u{241f}";

fn escape_slashes(s: &str) -> String {
    s.replace('\\', "\\\\")
}
fn escape_starting_colon(s: &str) -> String {
    if let Some(rest) = s.strip_prefix(':') {
        format!("\\:{rest}")
    } else {
        s.to_string()
    }
}
fn escape_colons(s: &str) -> String {
    s.replace(':', "\\:")
}
fn escape_for_template_literal(s: &str) -> String {
    s.replace('`', "\\`").replace("${", "$\\{")
}

fn create_cooked_raw_string(meta_block: &str, message_part: &str) -> (String, String) {
    if meta_block.is_empty() {
        let cooked = message_part.to_string();
        let raw = escape_for_template_literal(&escape_starting_colon(&escape_slashes(message_part)));
        (cooked, raw)
    } else {
        let cooked = format!(":{meta_block}:{message_part}");
        let raw = escape_for_template_literal(&format!(
            ":{}:{}",
            escape_colons(&escape_slashes(meta_block)),
            escape_slashes(message_part)
        ));
        (cooked, raw)
    }
}

fn serialize_i18n_head(meta: &o::I18nMeta, first_part: &str) -> (String, String) {
    let mut meta_block = meta.description.clone().unwrap_or_default();
    if let Some(meaning) = meta.meaning.as_deref().filter(|m| !m.is_empty()) {
        meta_block = format!("{meaning}{MEANING_SEPARATOR}{meta_block}");
    }
    if let Some(id) = meta.custom_id.as_deref().filter(|i| !i.is_empty()) {
        meta_block = format!("{meta_block}{ID_SEPARATOR}{id}");
    }
    for legacy_id in &meta.legacy_ids {
        meta_block = format!("{meta_block}{LEGACY_ID_INDICATOR}{legacy_id}");
    }
    create_cooked_raw_string(&meta_block, first_part)
}

fn serialize_i18n_template_part(
    placeholder: &o::PlaceholderPiece,
    message_part: &str,
) -> (String, String) {
    let mut meta_block = placeholder.text.clone();
    if let Some(assoc) = &placeholder.associated_message {
        if assoc.legacy_ids.is_empty() {
            let id = crate::digest::compute_msg_id(
                &assoc.message_string,
                assoc.meaning.as_deref().unwrap_or(""),
            );
            meta_block = format!("{meta_block}{ID_SEPARATOR}{id}");
        }
    }
    create_cooked_raw_string(&meta_block, message_part)
}

#[cfg(test)]
mod update_expression_tests {
    use super::*;
    use crate::output::emitter as oxc_emitter;

    fn var(name: &str) -> o::Expr {
        o::variable(name, None)
    }

    /// The expression statement text without its trailing `;\n`.
    fn js_text(e: &o::Expr) -> String {
        emit_expression(e).trim_end_matches(";\n").to_string()
    }

    /// The swc printer and the oxc emitter must agree byte-for-byte on `++` / `--` (prefix and
    /// postfix), including member and keyed targets.
    #[test]
    fn increment_and_decrement_match_oxc_for_prefix_and_postfix() {
        let cases: Vec<(o::Expr, &str)> = vec![
            (o::unary_with_prefix(UnaryOperator::Increment, var("x"), None, true), "++x"),
            (o::unary_with_prefix(UnaryOperator::Increment, var("x"), None, false), "x++"),
            (o::unary_with_prefix(UnaryOperator::Decrement, var("x"), None, true), "--x"),
            (o::unary_with_prefix(UnaryOperator::Decrement, var("x"), None, false), "x--"),
            (
                o::unary_with_prefix(UnaryOperator::Increment, var("obj").prop("n"), None, false),
                "obj.n++",
            ),
            (
                o::unary_with_prefix(UnaryOperator::Decrement, var("arr").key(o::literal(
                    LiteralValue::Number(0.0),
                    None,
                )), None, true),
                "--arr[0]",
            ),
        ];
        for (expr, expected) in cases {
            assert_eq!(js_text(&expr), expected);
            assert_eq!(oxc_emitter::emit_expression(&expr).trim_end_matches(";\n"), expected, "oxc disagrees for {expected}");
        }
    }

    #[test]
    fn postfix_update_binds_tighter_than_binary_operators() {
        // `x++ + 1` needs no parens; `(-x)++` is not a valid target so never reaches the printer.
        let sum = o::Expr::bare(ExprKind::Binary {
            op: BinaryOperator::Plus,
            lhs: Box::new(o::unary_with_prefix(UnaryOperator::Increment, var("x"), None, false)),
            rhs: Box::new(o::literal(LiteralValue::Number(1.0), None)),
        });
        assert_eq!(js_text(&sum), "x++ + 1");
    }

    #[test]
    fn update_expression_is_allowed_as_exponent_base() {
        let pow = o::Expr::bare(ExprKind::Binary {
            op: BinaryOperator::Exponentiation,
            lhs: Box::new(o::unary_with_prefix(UnaryOperator::Increment, var("a"), None, false)),
            rhs: Box::new(o::literal(LiteralValue::Number(2.0), None)),
        });
        assert_eq!(js_text(&pow), "a++ ** 2");
    }
}

#[cfg(test)]
mod leading_string_directive_tests {
    use super::*;

    fn str_lit(s: &str) -> o::Expr {
        o::literal(LiteralValue::String(s.to_string()), None)
    }

    /// oxc_codegen parenthesises a string-literal expression statement that is the FIRST statement of
    /// the program, so it cannot read as a directive (`"use strict";`). The swc printer must match.
    #[test]
    fn leading_string_expression_is_parenthesised_like_oxc() {
        assert_eq!(emit_expression(&str_lit("svg")), "(\"svg\");\n");
        let stmts = vec![o::Stmt::bare(StmtKind::Expression(str_lit("use strict")))];
        assert_eq!(emit_statements(&stmts), "(\"use strict\");\n");
    }

    /// Only the first statement is affected; a later string statement keeps its bare form.
    #[test]
    fn later_string_expression_is_not_parenthesised() {
        let stmts = vec![
            o::Stmt::bare(StmtKind::Expression(o::variable("a", None))),
            o::Stmt::bare(StmtKind::Expression(str_lit("x"))),
        ];
        assert_eq!(emit_statements(&stmts), "a;\n\"x\";\n");
    }

    /// A leading string followed by an import line is not the program's first statement, so the
    /// import (not the string) is first and the string is printed bare.
    #[test]
    fn leading_string_after_import_is_not_parenthesised() {
        let ext = o::import_expr(
            crate::output_ast::ExternalReference::new(Some("@angular/core".to_string()), "ɵɵdefineComponent"),
            None,
        );
        let stmts = vec![
            o::Stmt::bare(StmtKind::Expression(str_lit("x"))),
            o::Stmt::bare(StmtKind::Expression(ext)),
        ];
        let out = emit_statements(&stmts);
        assert!(out.starts_with("import * as i0 from \"@angular/core\";\n"), "got: {out}");
        assert!(out.contains("\n\"x\";\n"), "got: {out}");
        assert!(!out.contains("(\"x\")"), "got: {out}");
    }
}

#[cfg(test)]
mod escape_non_ascii_tests {
    use super::*;
    use crate::output::emit_options::EmitOptions;

    const ON: EmitOptions = EmitOptions { escape_non_ascii: true };

    fn str_lit(s: &str) -> o::Expr {
        o::literal(LiteralValue::String(s.to_string()), None)
    }

    /// `f(<arg>);` so the literal is never the first statement (the directive rule is tested elsewhere).
    fn call_with(arg: o::Expr) -> o::Expr {
        o::variable("f", None).call_fn(vec![arg], false)
    }

    fn print_on(arg: &str) -> String {
        emit_expression_with(&call_with(str_lit(arg)), &ON)
    }

    #[test]
    fn bmp_non_ascii_is_escaped_as_utf16_units() {
        assert_eq!(print_on("é"), "f(\"\\u00E9\");\n");
        assert_eq!(print_on("\u{FFFD}0\u{FFFD}"), "f(\"\\uFFFD0\\uFFFD\");\n");
        assert_eq!(print_on("\u{00A0}"), "f(\"\\u00A0\");\n");
    }

    #[test]
    fn astral_characters_become_surrogate_pairs() {
        assert_eq!(print_on("🚀"), "f(\"\\uD83D\\uDE80\");\n");
    }

    #[test]
    fn existing_backslash_u_text_is_escaped_backslash_not_reinterpreted() {
        // The VALUE is backslash, u, F, F, F, D: printed with the backslash escaped, never as U+FFFD.
        assert_eq!(print_on("\\uFFFD"), "f(\"\\\\uFFFD\");\n");
    }

    #[test]
    fn control_characters_follow_typescript_table() {
        assert_eq!(print_on("\u{07}"), "f(\"\\u0007\");\n");
        assert_eq!(print_on("\u{1B}"), "f(\"\\u001B\");\n");
        assert_eq!(print_on("\t\n\r"), "f(\"\\t\\n\\r\");\n");
        assert_eq!(print_on("\u{0}"), "f(\"\\0\");\n");
        assert_eq!(print_on("\u{0}1"), "f(\"\\x001\");\n");
    }

    #[test]
    fn ascii_and_quotes_are_unchanged() {
        assert_eq!(print_on("say \"hi\""), "f(\"say \\\"hi\\\"\");\n");
        assert_eq!(print_on("plain"), "f(\"plain\");\n");
    }

    #[test]
    fn identifiers_with_non_ascii_letters_stay_raw() {
        let expr = o::variable("ɵɵdefine", None).call_fn(vec![], false);
        assert_eq!(emit_expression_with(&expr, &ON), "ɵɵdefine();\n");
    }

    #[test]
    fn template_raw_text_stays_raw_but_substitutions_are_escaped() {
        let tpl = o::Expr::bare(ExprKind::TemplateLiteral {
            elements: vec![
                o::TemplateLiteralElement { text: "é ".into(), raw_text: "é ".into() },
                o::TemplateLiteralElement { text: String::new(), raw_text: String::new() },
            ],
            expressions: vec![str_lit("ü")],
        });
        assert_eq!(emit_expression_with(&tpl, &ON), "`é ${\"\\u00FC\"}`;\n");
    }

    #[test]
    fn default_option_keeps_raw_characters() {
        let off = EmitOptions::default();
        assert_eq!(emit_expression_with(&call_with(str_lit("é")), &off), "f(\"é\");\n");
    }
}

#[cfg(test)]
mod leading_comment_tests {
    //! The swc printer's leading comments: pinned to the oxc emitter's bytes (see `emitter::leading_comment_tests`).
    use super::*;
    use crate::output_ast::{LiteralValue, StmtKind, StmtModifier};

    /// `const MSG = /* @ts-ignore */ goog.getMsg("x");` with a JSDoc above the statement.
    fn commented_program() -> Vec<o::Stmt> {
        let mut call = o::variable("goog", None).prop("getMsg").call_fn(
            vec![o::literal(LiteralValue::String("x".into()), None)],
            false,
        );
        call.meta.leading_comments = vec![o::leading_comment("@ts-ignore", true, true)];
        let mut decl = o::Stmt::with_modifiers(
            StmtKind::DeclareVar { name: "MSG".into(), value: Some(call), ty: None },
            StmtModifier::FINAL,
        );
        decl.meta.leading_comments = vec![o::js_doc_comment(vec![o::JsDocTag {
            tag_name: Some("desc".into()),
            text: Some("Hello".into()),
        }])];
        vec![decl]
    }

    const COMMENTED_PROGRAM_JS: &str =
        "/**\n * @desc Hello\n */\nconst MSG = /* @ts-ignore */ goog.getMsg(\"x\");\n";

    #[test]
    fn statement_jsdoc_and_inline_expression_comment_print_like_angular() {
        assert_eq!(emit_statements(&commented_program()), COMMENTED_PROGRAM_JS);
    }

    #[test]
    fn expression_comment_is_dropped_only_when_it_has_no_identifier_head() {
        let mut lit = o::literal(LiteralValue::Number(1.0), None);
        lit.meta.leading_comments = vec![o::leading_comment("note", true, true)];
        let out = emit_expression(&lit);
        assert!(!out.contains("note"), "got: {out}");
    }

    #[test]
    fn plain_multiline_and_single_line_statement_comments() {
        let mut stmt = o::Stmt::bare(StmtKind::Expression(o::variable("a", None)));
        stmt.meta.leading_comments = vec![
            o::leading_comment("one", true, true),
            o::leading_comment("two\nthree", false, true),
        ];
        assert_eq!(emit_statements(&[stmt]), "/* one */\n// two\n// three\na;\n");
    }
}
