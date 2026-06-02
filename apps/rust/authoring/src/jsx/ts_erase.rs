//! TS-type-erasing expression serializer for JSX authoring.
//!
//! JSX authoring (`.tsx` / `.tjsx`) handlers and bound-attribute values are *real TypeScript*
//! expressions, but the JSX plugin lowers them into Angular **template** binding expressions. The
//! shared render3 template-expression parser (`libs/render3` `expression/parser.rs`) implements the
//! Angular template grammar, which supports arrow functions, member / call expressions, optional
//! chaining, template literals, and so on — but it does **not** understand TypeScript-only syntax.
//! An inline handler such as
//!
//! ```text
//! onInput={(event) => name.set((event.target as HTMLInputElement).value)}
//! ```
//!
//! fails the template parser with `Missing closing parentheses` at the `as`.
//!
//! TypeScript type syntax is entirely *runtime-erased*: it carries no behaviour and the emitted
//! JavaScript simply drops it. This module produces JS-only expression text from an OXC
//! [`Expression`] by erasing exactly those runtime-erased TS constructs:
//!
//!   * [`Expression::TSAsExpression`]        `x as T`        → `x`
//!   * [`Expression::TSSatisfiesExpression`]  `x satisfies T` → `x`
//!   * [`Expression::TSNonNullExpression`]    `x!`            → `x`
//!   * [`Expression::TSTypeAssertion`]        `<T>x`          → `x`
//!   * [`Expression::TSInstantiationExpression`] `f<T>`       → `f`
//!   * call / `new` type arguments            `f<T>(a)`       → `f(a)`
//!   * arrow-function parameter type annotations + return-type annotation
//!     `(e: Event): void => …`                                → `(e) => …`
//!
//! Everything else is kept byte-faithful.
//!
//! ## Strategy
//!
//! The lowering visitor holds the parsed program by shared (`&`) reference, so the borrowed
//! [`Expression`] cannot be mutated in place. We therefore take the **hybrid** route:
//!
//!   1. A cheap immutable [`oxc_ast_visit::Visit`] scan ([`contains_ts_syntax`]) checks whether the
//!      expression contains *any* of the erased constructs. The overwhelmingly common case — a plain
//!      reference handler (`onClick={greet}`) or a plain binding (`[value]="name()"`) — contains
//!      none, so we return the **verbatim source slice** unchanged. This guarantees that
//!      non-TS expressions lower byte-identically to before (the hard "unchanged" gate).
//!   2. Only when TS syntax is present do we re-parse the expression's source slice into a fresh
//!      allocator (wrapped in parentheses to force expression context), run a
//!      [`oxc_ast_visit::VisitMut`] pass ([`TsEraser`]) that unwraps the TS wrapper nodes and nulls
//!      the type annotations / type arguments, and re-emit the cleaned expression via
//!      [`oxc_codegen`]. The result is JS-only text the template parser accepts.
//!
//! Re-parsing the slice is robust: every call site slices a real expression span, which is a valid
//! standalone expression; the surrounding parentheses are stripped back off when we extract the
//! inner expression for codegen.

use oxc_allocator::{Allocator, TakeIn};
use oxc_ast::ast::{
    ArrowFunctionExpression, CallExpression, Expression, FormalParameter, NewExpression, Program,
    Statement,
};
use oxc_ast_visit::{
    walk_mut::{
        walk_arrow_function_expression, walk_call_expression, walk_expression, walk_formal_parameter,
        walk_new_expression,
    },
    Visit, VisitMut,
};
use oxc_codegen::Codegen;
use oxc_parser::Parser as JsParser;
use oxc_span::{GetSpan, SourceType};

/// Erase TS-only syntax from `expression` and return JS-only template-expression text.
///
/// The single entry point for *every* JSX expression that is lowered into an Angular template
/// binding (event handlers, property / attribute bindings, `class` / `style` bindings, directive
/// input values, interpolation containers, and control-flow expressions). When `expression` carries
/// no TS-only syntax the verbatim source slice is returned unchanged (byte-faithful); otherwise the
/// expression is re-parsed, erased, and re-emitted.
pub(crate) fn erase_expression(expression: &Expression, source: &str) -> String {
    let span = GetSpan::span(expression);
    let verbatim = &source[span.start as usize..span.end as usize];

    if !contains_ts_syntax(expression) {
        // Fast path: no runtime-erased TS syntax, so the source text is already valid JS / template
        // expression text. Return it byte-for-byte (preserves the exact prior behaviour).
        return verbatim.to_string();
    }

    // Slow path: re-parse the slice in a fresh allocator, erase TS syntax, and re-emit. Parenthesize
    // to force expression context (e.g. an object-literal slice would otherwise parse as a block).
    erase_via_reparse(verbatim).unwrap_or_else(|| verbatim.to_string())
}

/// Re-parse `verbatim` (a standalone expression's source text), run the TS-erasing [`VisitMut`]
/// pass, and re-emit JS-only text. Returns `None` if the slice does not re-parse cleanly as a single
/// expression, in which case the caller falls back to the verbatim slice.
pub(crate) fn erase_via_reparse(verbatim: &str) -> Option<String> {
    let wrapped = format!("({verbatim});");
    let allocator = Allocator::default();
    // The slice is TypeScript (it reached the slow path *because* it carries TS syntax), so parse as
    // TS. `.tsx` keeps JSX available too, matching the front-end's own parse mode.
    let ret = JsParser::new(&allocator, &wrapped, SourceType::tsx()).parse();
    if !ret.errors.is_empty() {
        return None;
    }

    let mut program = ret.program;
    let mut eraser = TsEraser { allocator: &allocator };
    eraser.visit_program(&mut program);

    let expression = extract_single_expression(&mut program, &allocator)?;
    let mut codegen = Codegen::new().with_source_type(SourceType::tsx());
    codegen.print_expression(&expression);
    Some(codegen.into_source_text().trim().to_string())
}

/// Pull the single expression back out of the re-parsed `( expr );` program. The parenthesized
/// expression statement is the only statement; we move its inner expression out (unwrapping the
/// [`Expression::ParenthesizedExpression`] wrapper the parser created for the `( … )`).
fn extract_single_expression<'a>(
    program: &mut Program<'a>,
    allocator: &'a Allocator,
) -> Option<Expression<'a>> {
    let stmt = program.body.first_mut()?;
    let Statement::ExpressionStatement(expr_stmt) = stmt else {
        return None;
    };
    let mut expr = expr_stmt.expression.take_in(allocator);
    // Strip the synthetic wrapper parentheses we added so codegen does not emit redundant `( … )`.
    while let Expression::ParenthesizedExpression(paren) = &mut expr {
        expr = paren.expression.take_in(allocator);
    }
    Some(expr)
}

// ---------------------------------------------------------------------------
// Immutable detection pass.
// ---------------------------------------------------------------------------

/// Whether `expression` (recursively) contains any runtime-erased TS-only syntax that the Angular
/// template-expression grammar cannot parse.
fn contains_ts_syntax(expression: &Expression) -> bool {
    let mut detector = TsDetector { found: false };
    detector.visit_expression(expression);
    detector.found
}

/// Immutable visitor that sets `found` as soon as any erased TS construct is seen.
struct TsDetector {
    found: bool,
}

impl<'a> Visit<'a> for TsDetector {
    fn visit_expression(&mut self, it: &Expression<'a>) {
        match it {
            Expression::TSAsExpression(_)
            | Expression::TSSatisfiesExpression(_)
            | Expression::TSNonNullExpression(_)
            | Expression::TSTypeAssertion(_)
            | Expression::TSInstantiationExpression(_) => {
                self.found = true;
            }
            // A call / `new` with explicit type arguments (`f<T>(a)`) is also TS-only.
            Expression::CallExpression(call) if call.type_arguments.is_some() => {
                self.found = true;
            }
            Expression::NewExpression(new) if new.type_arguments.is_some() => {
                self.found = true;
            }
            _ => {}
        }
        if self.found {
            return;
        }
        oxc_ast_visit::walk::walk_expression(self, it);
    }

    fn visit_arrow_function_expression(&mut self, it: &ArrowFunctionExpression<'a>) {
        // Type parameters (`<T>`), parameter type annotations, and a return-type annotation are all
        // TS-only syntax on an arrow.
        if it.type_parameters.is_some() || it.return_type.is_some() {
            self.found = true;
        }
        if self.found {
            return;
        }
        oxc_ast_visit::walk::walk_arrow_function_expression(self, it);
    }

    fn visit_formal_parameter(&mut self, it: &FormalParameter<'a>) {
        if it.type_annotation.is_some() {
            self.found = true;
        }
        if self.found {
            return;
        }
        oxc_ast_visit::walk::walk_formal_parameter(self, it);
    }
}

// ---------------------------------------------------------------------------
// Mutable erasure pass.
// ---------------------------------------------------------------------------

/// Mutating visitor that erases runtime-erased TS syntax from an owned expression tree:
///   * unwraps `as` / `satisfies` / non-null / type-assertion / instantiation wrapper expressions,
///   * nulls call / `new` type arguments,
///   * nulls arrow type parameters + return type + parameter type annotations.
///
/// Children are visited *first* (post-order) so an unwrap exposes any TS syntax nested in the inner
/// expression, which is then handled when the visit re-descends — for the wrapper case we visit the
/// replacement explicitly to guarantee full recursion through stacked wrappers (`(x as A)!`).
struct TsEraser<'a> {
    /// Arena the re-parsed expression lives in; used to allocate the dummy placeholders that
    /// [`TakeIn`] swaps in while we move a wrapper's inner expression out.
    allocator: &'a Allocator,
}

impl<'a> VisitMut<'a> for TsEraser<'a> {
    fn visit_expression(&mut self, it: &mut Expression<'a>) {
        // Walk children first so inner TS syntax is erased before we (possibly) unwrap this node.
        walk_expression(self, it);

        // Unwrap a TS wrapper expression to its inner expression. After unwrapping, re-visit the new
        // node: stacked wrappers (`x as A as B`, `(x as A)!`) collapse fully, and a wrapper whose
        // inner is itself a wrapper is handled without relying on a second top-level pass. The
        // discarded wrapper takes the swapped-in dummy with it, so the dummy never reaches codegen.
        let unwrapped = match it {
            Expression::TSAsExpression(expr) => Some(expr.expression.take_in(self.allocator)),
            Expression::TSSatisfiesExpression(expr) => Some(expr.expression.take_in(self.allocator)),
            Expression::TSNonNullExpression(expr) => Some(expr.expression.take_in(self.allocator)),
            Expression::TSTypeAssertion(expr) => Some(expr.expression.take_in(self.allocator)),
            Expression::TSInstantiationExpression(expr) => {
                Some(expr.expression.take_in(self.allocator))
            }
            _ => None,
        };
        if let Some(inner) = unwrapped {
            *it = inner;
            self.visit_expression(it);
        }
    }

    fn visit_call_expression(&mut self, it: &mut CallExpression<'a>) {
        // `f<T>(a)` → `f(a)`: drop explicit type arguments.
        it.type_arguments = None;
        walk_call_expression(self, it);
    }

    fn visit_new_expression(&mut self, it: &mut NewExpression<'a>) {
        // `new Foo<T>(a)` → `new Foo(a)`.
        it.type_arguments = None;
        walk_new_expression(self, it);
    }

    fn visit_arrow_function_expression(&mut self, it: &mut ArrowFunctionExpression<'a>) {
        // `<T>(…): R => …` → `(…) => …`: drop type parameters and the return-type annotation.
        it.type_parameters = None;
        it.return_type = None;
        walk_arrow_function_expression(self, it);
    }

    fn visit_formal_parameter(&mut self, it: &mut FormalParameter<'a>) {
        // `(e: Event)` → `(e)`: drop the parameter type annotation.
        it.type_annotation = None;
        walk_formal_parameter(self, it);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxc_ast::ast::Statement;

    /// Parse `src` as a single TS *expression statement* and erase it via [`erase_expression`].
    ///
    /// Parses as plain TS (not TSX) so the angle-bracket type-assertion (`<T>x`) and
    /// instantiation-expression forms — which are ambiguous with JSX in `.tsx` — are available and
    /// can be exercised directly. The lowering pipeline itself parses TSX; this harness targets the
    /// erasure serializer in isolation.
    fn erase(src: &str) -> String {
        let wrapped = format!("({src});");
        let allocator = Allocator::default();
        let ret = JsParser::new(&allocator, &wrapped, SourceType::ts()).parse();
        assert!(ret.errors.is_empty(), "parse errors for {src:?}: {:?}", ret.errors);
        for stmt in &ret.program.body {
            if let Statement::ExpressionStatement(expr_stmt) = stmt {
                // Unwrap the wrapper parens to reach the real expression node.
                let mut expr = &expr_stmt.expression;
                while let Expression::ParenthesizedExpression(p) = expr {
                    expr = &p.expression;
                }
                return erase_expression(expr, &wrapped);
            }
        }
        panic!("no expression statement in {src:?}");
    }

    // NOTE on the slow-path assertions below: when TS syntax is present, the expression is re-parsed
    // and re-emitted by `oxc_codegen`, which normalises insignificant formatting (it drops redundant
    // parentheses and may wrap the whole emitted expression in parens). The results are
    // *semantically identical* JS that the Angular template-expression parser accepts; the
    // assertions therefore check the codegen-normalised form. The key invariant is that no TS-only
    // token survives.

    #[test]
    fn as_expression_erases() {
        assert_eq!(erase("x as number"), "x");
        // The redundant cast parens are dropped by codegen; the result is plain `x + 1`.
        assert_eq!(erase("(x as number) + 1"), "x + 1");
    }

    #[test]
    fn satisfies_expression_erases() {
        assert_eq!(erase("x satisfies Foo"), "x");
    }

    #[test]
    fn non_null_expression_erases() {
        assert_eq!(erase("x!"), "x");
        assert_eq!(erase("x!.y!.z"), "x.y.z");
    }

    #[test]
    fn type_assertion_angle_form_erases() {
        // The `<T>x` prefix type-assertion form (the `TSTypeAssertion` variant) is only valid in
        // plain `.ts` — it is ambiguous with JSX in `.tsx`, so TypeScript itself forbids it there and
        // the JSX authoring front-end (always TSX) never sees it. We still exercise the erase logic
        // by detecting the assertion in the (TS) AST. The slow-path re-parse uses TSX (matching the
        // real pipeline), under which `<number>x` is JSX, so we assert only that detection fires.
        let allocator = Allocator::default();
        let src = "(<number>x);";
        let ret = JsParser::new(&allocator, src, SourceType::ts()).parse();
        let mut found = false;
        for stmt in &ret.program.body {
            if let Statement::ExpressionStatement(s) = stmt {
                let mut e = &s.expression;
                while let Expression::ParenthesizedExpression(p) = e {
                    e = &p.expression;
                }
                found = super::contains_ts_syntax(e);
            }
        }
        assert!(found, "TSTypeAssertion `<number>x` not detected as TS syntax");
    }

    #[test]
    fn call_and_new_type_arguments_erase() {
        assert_eq!(erase("foo<string>(a)"), "foo(a)");
        assert_eq!(erase("new Foo<string>(a)"), "new Foo(a)");
    }

    #[test]
    fn instantiation_expression_erases() {
        // A bare `f<T>` instantiation expression unwraps to `f`.
        assert_eq!(erase("foo<string>"), "foo");
    }

    #[test]
    fn arrow_param_and_return_type_annotations_drop() {
        // Codegen wraps a re-emitted arrow in parens; no `: Event` / `: void` annotation survives.
        // (A generic `<T>(…)` arrow is not representable in `.tsx` — it is ambiguous with JSX — so
        // the authoring pipeline never sees one; the `type_parameters` field is still nulled by the
        // eraser, covered indirectly by the `<T>(x: T): T => x` reachability note in the angle-form
        // test.)
        assert_eq!(erase("(e: Event) => handle(e)"), "((e) => handle(e))");
        assert_eq!(erase("(): void => go()"), "(() => go())");
    }

    #[test]
    fn generic_arrow_type_parameters_erase_in_plain_ts() {
        // A generic arrow is only valid in plain `.ts`. Parse + erase it there to prove the
        // `type_parameters` branch of the eraser drops `<T>` (and the param annotation) — even though
        // the TSX authoring path can never produce this form.
        let allocator = Allocator::default();
        let src = "(<T>(x: T): T => x);";
        let ret = JsParser::new(&allocator, src, SourceType::ts()).parse();
        assert!(ret.errors.is_empty(), "parse errors: {:?}", ret.errors);
        let mut program = ret.program;
        let mut eraser = TsEraser { allocator: &allocator };
        eraser.visit_program(&mut program);
        let expr = extract_single_expression(&mut program, &allocator).expect("expression");
        let mut codegen = Codegen::new().with_source_type(SourceType::ts());
        codegen.print_expression(&expr);
        let out = codegen.into_source_text();
        assert!(!out.contains('<') && !out.contains(": T") && !out.contains(": T =>"), "type params/annotations not erased; got {out}");
        assert!(out.contains("(x) => x") || out.contains("(x)=>x"), "arrow body garbled; got {out}");
    }

    #[test]
    fn stacked_wrappers_collapse_fully() {
        // `x as A as B` and `(x as A)!` both collapse to `x`.
        assert_eq!(erase("x as A as B"), "x");
        assert_eq!(erase("(x as A)!"), "x");
    }

    #[test]
    fn deeply_nested_assertion_in_arrow_body_erases() {
        // The headline handler shape: an `as` cast deep inside an arrow body member/call chain. The
        // cast's now-redundant parens are dropped by codegen; the cast itself is gone.
        assert_eq!(
            erase("(event) => name.set((event.target as HTMLInputElement).value)"),
            "((event) => name.set(event.target.value))"
        );
    }

    #[test]
    fn assertions_inside_arrays_objects_and_optional_chains_erase() {
        assert_eq!(erase("[a as number, b]"), "[a, b]");
        assert_eq!(erase("({ k: v as T })"), "({ k: v })");
        assert_eq!(erase("(a as number[])?.[0]"), "a?.[0]");
    }

    #[test]
    fn non_ts_expression_is_returned_verbatim() {
        // The fast path returns the exact source slice unchanged for expressions with no TS syntax —
        // formatting is preserved byte-for-byte (no codegen reformat), so spacing is kept.
        assert_eq!(erase("name()"), "name()");
        assert_eq!(erase("user?.profile?.name"), "user?.profile?.name");
        assert_eq!(erase("a +  b"), "a +  b"); // verbatim keeps the double space; codegen would not.
        assert_eq!(erase("(e) => handle(e)"), "(e) => handle(e)");
    }
}
