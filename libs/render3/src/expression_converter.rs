//! Binding-expression AST -> output_ast converter.
//!
//! PORT TARGET: Angular's `compiler_util/expression_converter.ts`
//! (`convertPropertyBinding` / `convertActionBinding` / `convertUpdateArguments`).
//!
//! NOTE(port): In Angular v22 the historic `compiler_util/expression_converter.ts`
//! has been retired; its job is now spread across the template *pipeline* — the
//! `e.AST -> o.Expression` lowering lives in `template/pipeline/src/ingest.ts`
//! (`convertAst`) and the operator table in `template/pipeline/src/conversion.ts`
//! (`BINARY_OPERATORS`). The pipeline lowers `PropertyRead(ImplicitReceiver)` to a
//! `LexicalReadExpr` placeholder that a *later* phase resolves against the view's
//! `ctx`. Since those resolution phases are not yet ported, this module reproduces
//! the **classic** `convertPropertyBinding` behaviour directly: the implicit
//! receiver is materialised as a caller-supplied root expression (conventionally the
//! `ctx` variable), so `{{a}}` lowers straight to `ctx.a`. The mapping of every
//! concrete node kind follows `convertAst` exactly (see `ingest.ts:1097`), and the
//! binary-operator table is a 1:1 copy of `BINARY_OPERATORS` (`conversion.ts:12`).
//!
//! This converter is owned & arena-free: it consumes a borrowed
//! [`crate::expression::ast::AstNode`] and produces an owned
//! [`crate::output_ast::Expr`].

use crate::expression::ast::{
    self as e, AstNode, BinaryOperation, ExprKind as EK, LiteralMapKey, LiteralValue as ELit,
    UnaryOperator as EUnary,
};
use crate::output_ast::{
    self as o, ArrowBody, BinaryOperator, Expr, ExprKind, FnParam, LiteralValue as OLit,
    UnaryOperator as OUnary, dynamic_type, literal, literal_arr, not, typeof_expr,
};

// ---------------------------------------------------------------------------
// Result of a binding conversion.
// ---------------------------------------------------------------------------

/// Result of converting a binding expression: the lowered root [`Expr`] plus any
/// temporary [`o::Stmt`]s that must be evaluated first. Mirrors the
/// `ConvertPropertyBindingResult` / `ConvertActionBindingResult` shape from the
/// classic `expression_converter.ts` (`{ stmts, currValExpr }` /
/// `{ stmts, allowDefault }`).
///
/// For the node kinds ported here no temporaries are required, so `stmts` is
/// always empty today; it is part of the public shape so safe-navigation
/// (`a?.b`) and pipe lowering — which *do* spill temporaries — can be added later
/// without a breaking change.
#[derive(Debug, Clone, PartialEq)]
pub struct ConvertedBinding {
    /// Statements that must run before `expr` (temporary `let`s, guards, ...).
    pub stmts: Vec<o::Stmt>,
    /// The lowered expression.
    pub expr: Expr,
}

impl ConvertedBinding {
    fn pure(expr: Expr) -> Self {
        ConvertedBinding {
            stmts: Vec::new(),
            expr,
        }
    }
}

// ---------------------------------------------------------------------------
// Local resolver abstraction.
// ---------------------------------------------------------------------------

/// `LocalResolver` — abstracts how an [`EK::ImplicitReceiver`] / [`EK::ThisReceiver`]
/// and named local reads resolve to an [`Expr`]. Mirrors the `LocalResolver`
/// interface in the classic `expression_converter.ts`.
///
/// The default implementation ([`CtxResolver`]) resolves the implicit receiver to
/// a caller-supplied root expression (conventionally the `ctx` `ReadVarExpr`), so
/// `PropertyRead(ImplicitReceiver, "a")` lowers to `ctx.a`.
pub trait LocalResolver {
    /// Resolve the implicit/this receiver root (the component context).
    fn resolve_implicit_receiver(&self) -> Expr;

    /// Resolve a named local variable, or `None` to fall back to a property read
    /// off the implicit receiver. Default: no locals.
    fn maybe_resolve_local(&self, _name: &str) -> Option<Expr> {
        None
    }
}

/// Default [`LocalResolver`]: every implicit-receiver read roots at a single
/// context expression (e.g. `o::variable("ctx", None)`).
#[derive(Debug, Clone)]
pub struct CtxResolver {
    ctx: Expr,
}

impl CtxResolver {
    /// Build a resolver rooted at `ctx`.
    pub fn new(ctx: Expr) -> Self {
        CtxResolver { ctx }
    }

    /// Convenience: root at the `ctx` variable (`o::variable("ctx", None)`), the
    /// conventional name used by render3 update/create functions.
    pub fn ctx() -> Self {
        CtxResolver::new(o::variable("ctx", None))
    }
}

impl LocalResolver for CtxResolver {
    fn resolve_implicit_receiver(&self) -> Expr {
        self.ctx.clone()
    }
}

// ---------------------------------------------------------------------------
// Operator table (1:1 with conversion.ts BINARY_OPERATORS).
// ---------------------------------------------------------------------------

/// Map an [`e::BinaryOperation`] to an [`o::BinaryOperator`]. Mirrors
/// `BINARY_OPERATORS` (`template/pipeline/src/conversion.ts`). Returns `None` for
/// operators with no `output_ast` equivalent (none, in practice — every entry maps).
fn binary_operator(op: BinaryOperation) -> Option<BinaryOperator> {
    use crate::expression::ast::AssignmentOperation as A;
    Some(match op {
        BinaryOperation::And => BinaryOperator::And,
        BinaryOperation::Or => BinaryOperator::Or,
        BinaryOperation::Nullish => BinaryOperator::NullishCoalesce,
        BinaryOperation::Eq => BinaryOperator::Equals,
        BinaryOperation::Neq => BinaryOperator::NotEquals,
        BinaryOperation::Identity => BinaryOperator::Identical,
        BinaryOperation::NotIdentity => BinaryOperator::NotIdentical,
        BinaryOperation::Lt => BinaryOperator::Lower,
        BinaryOperation::Gt => BinaryOperator::Bigger,
        BinaryOperation::Le => BinaryOperator::LowerEquals,
        BinaryOperation::Ge => BinaryOperator::BiggerEquals,
        BinaryOperation::In => BinaryOperator::In,
        BinaryOperation::Instanceof => BinaryOperator::InstanceOf,
        BinaryOperation::Add => BinaryOperator::Plus,
        BinaryOperation::Sub => BinaryOperator::Minus,
        BinaryOperation::Mul => BinaryOperator::Multiply,
        BinaryOperation::Mod => BinaryOperator::Modulo,
        BinaryOperation::Div => BinaryOperator::Divide,
        BinaryOperation::Pow => BinaryOperator::Exponentiation,
        BinaryOperation::Assignment(A::Assign) => BinaryOperator::Assign,
        BinaryOperation::Assignment(A::AddAssign) => BinaryOperator::AdditionAssignment,
        BinaryOperation::Assignment(A::SubAssign) => BinaryOperator::SubtractionAssignment,
        BinaryOperation::Assignment(A::MulAssign) => BinaryOperator::MultiplicationAssignment,
        BinaryOperation::Assignment(A::DivAssign) => BinaryOperator::DivisionAssignment,
        BinaryOperation::Assignment(A::ModAssign) => BinaryOperator::RemainderAssignment,
        BinaryOperation::Assignment(A::PowAssign) => BinaryOperator::ExponentiationAssignment,
        BinaryOperation::Assignment(A::AndAssign) => BinaryOperator::AndAssignment,
        BinaryOperation::Assignment(A::OrAssign) => BinaryOperator::OrAssignment,
        BinaryOperation::Assignment(A::NullishAssign) => BinaryOperator::NullishCoalesceAssignment,
    })
}

fn binary_expr(op: BinaryOperator, lhs: Expr, rhs: Expr) -> Expr {
    Expr::bare(ExprKind::Binary {
        op,
        lhs: Box::new(lhs),
        rhs: Box::new(rhs),
    })
}

fn map_literal_value(v: &ELit) -> OLit {
    match v {
        ELit::Str(s) => OLit::String(s.clone()),
        ELit::Num(n) => OLit::Number(*n),
        ELit::Bool(b) => OLit::Bool(*b),
        ELit::Null => OLit::Null,
        ELit::Undefined => OLit::Undefined,
    }
}

fn is_implicit_receiver(node: &AstNode) -> bool {
    matches!(node.kind, EK::ImplicitReceiver | EK::ThisReceiver)
}

// ---------------------------------------------------------------------------
// Public entry points.
// ---------------------------------------------------------------------------

/// `convertPropertyBinding(ast, implicitReceiver, bindingId)` — lower a property /
/// interpolation binding expression to an [`Expr`] (+ any temporaries).
///
/// `implicit_receiver` is the root the implicit context resolves to (e.g.
/// `o::variable("ctx", None)`); `binding_id` is accepted for parity with the
/// classic signature (it seeds temporary-variable names once safe-navigation
/// lowering lands) but is currently unused.
pub fn convert_property_binding(
    expr: &AstNode,
    implicit_receiver: Expr,
    _binding_id: &str,
) -> ConvertedBinding {
    let resolver = CtxResolver::new(implicit_receiver);
    convert_property_binding_with(expr, &resolver)
}

/// Like [`convert_property_binding`] but with a caller-supplied [`LocalResolver`].
pub fn convert_property_binding_with<R: LocalResolver>(
    expr: &AstNode,
    resolver: &R,
) -> ConvertedBinding {
    let mut cx = Converter::new(resolver);
    ConvertedBinding::pure(cx.convert(expr))
}

/// `convertActionBinding(ast, ...)` — lower an event-handler expression. Event
/// handlers are an implicit [`EK::Chain`] of statements; each chained expression
/// becomes an [`o::Stmt::Expression`], and the *last* expression's value is what a
/// handler returns. Here we emit each as a statement and return the final
/// expression as `expr` (callers wanting `return` semantics wrap it themselves).
pub fn convert_action_binding(
    expr: &AstNode,
    implicit_receiver: Expr,
    _binding_id: &str,
) -> ConvertedBinding {
    let resolver = CtxResolver::new(implicit_receiver);
    convert_action_binding_with(expr, &resolver)
}

/// Like [`convert_action_binding`] but with a caller-supplied [`LocalResolver`].
pub fn convert_action_binding_with<R: LocalResolver>(
    expr: &AstNode,
    resolver: &R,
) -> ConvertedBinding {
    let mut cx = Converter::new(resolver);
    match &expr.kind {
        EK::Chain { expressions } => {
            if expressions.is_empty() {
                return ConvertedBinding::pure(literal(OLit::Null, None));
            }
            let mut stmts = Vec::new();
            let last = expressions.len() - 1;
            let mut tail = literal(OLit::Null, None);
            for (i, sub) in expressions.iter().enumerate() {
                let lowered = cx.convert(sub);
                if i == last {
                    tail = lowered;
                } else {
                    stmts.push(lowered.to_stmt());
                }
            }
            ConvertedBinding { stmts, expr: tail }
        }
        _ => ConvertedBinding::pure(cx.convert(expr)),
    }
}

// ---------------------------------------------------------------------------
// The converter.
// ---------------------------------------------------------------------------

struct Converter<'r, R: LocalResolver> {
    resolver: &'r R,
    /// Monotonic counter seeding temporary-variable names (`tmp_0`, `tmp_1`, ...)
    /// for safe-navigation guard expansion. Mirrors `allocateTemporary()` /
    /// `_currentTemporary` in the classic `_AstToIrVisitor`.
    next_temp: usize,
}

impl<'r, R: LocalResolver> Converter<'r, R> {
    fn new(resolver: &'r R) -> Self {
        Converter {
            resolver,
            next_temp: 0,
        }
    }

    /// `allocateTemporary()` — mint a fresh temporary-variable [`Expr`]
    /// (`ReadVarExpr`). The classic converter names these `tmp` (`pf` for pure
    /// functions); we suffix with a counter so nested safe chains never collide.
    fn allocate_temporary(&mut self) -> Expr {
        let name = format!("tmp_{}", self.next_temp);
        self.next_temp += 1;
        o::variable(name, None)
    }
}

impl<R: LocalResolver> Converter<'_, R> {
    /// Core recursive lowering — mirrors `convertAst` (`ingest.ts:1097`).
    fn convert(&mut self, node: &AstNode) -> Expr {
        match &node.kind {
            // Empty expression -> `null` (the pipeline mints a dedicated EmptyExpr;
            // we have no runtime placeholder, so `null` is the safe stand-in).
            EK::EmptyExpr => literal(OLit::Null, None),

            // The implicit/this receiver resolve via the resolver.
            EK::ImplicitReceiver | EK::ThisReceiver => self.resolver.resolve_implicit_receiver(),

            // PropertyRead: `a` off the implicit receiver -> resolver lookup then
            // `.prop`, matching the classic convertPropertyBinding (`ctx.a`). For a
            // non-implicit receiver, recurse and `.prop`.
            EK::PropertyRead { receiver, name, .. } => {
                if is_implicit_receiver(receiver) {
                    if let Some(local) = self.resolver.maybe_resolve_local(name) {
                        local
                    } else {
                        self.resolver.resolve_implicit_receiver().prop(name.clone())
                    }
                } else {
                    self.convert(receiver).prop(name.clone())
                }
            }

            // SafePropertyRead: `a?.b`. Expanded to a guarded temporary —
            // `(tmp = a) == null ? null : tmp.b` — see `convert_safe`.
            EK::SafePropertyRead { receiver, name, .. } => {
                let name = name.clone();
                self.convert_safe(receiver, move |recv| recv.prop(name))
            }

            // KeyedRead: `a[k]`.
            EK::KeyedRead { receiver, key } => {
                let recv = self.convert(receiver);
                let idx = self.convert(key);
                recv.key(idx)
            }

            // SafeKeyedRead: `a?.[k]` -> `(tmp = a) == null ? null : tmp[k]`.
            EK::SafeKeyedRead { receiver, key } => {
                let idx = self.convert(key);
                self.convert_safe(receiver, move |recv| recv.key(idx))
            }

            // Call: `f(args)`.
            EK::Call { receiver, args, .. } => {
                let callee = self.convert(receiver);
                let args = args.iter().map(|a| self.convert(a)).collect();
                callee.call_fn(args, false)
            }

            // SafeCall: `f?.(args)` -> `(tmp = f) == null ? null : tmp(args)`.
            EK::SafeCall { receiver, args, .. } => {
                let args: Vec<Expr> = args.iter().map(|a| self.convert(a)).collect();
                self.convert_safe(receiver, move |recv| recv.call_fn(args, false))
            }

            // LiteralPrimitive.
            EK::LiteralPrimitive { value } => literal(map_literal_value(value), None),

            // LiteralArray.
            EK::LiteralArray { expressions } => {
                let entries = expressions.iter().map(|x| self.convert(x)).collect();
                literal_arr(entries, None)
            }

            // SpreadElement.
            EK::SpreadElement { expression } => {
                Expr::bare(ExprKind::Spread(Box::new(self.convert(expression))))
            }

            // LiteralMap — `keys`/`values` are parallel; spread keys carry no value
            // slot of their own but the AST keeps `values` aligned by index.
            EK::LiteralMap { keys, values } => {
                let entries = keys
                    .iter()
                    .enumerate()
                    .map(|(idx, key)| {
                        let value = self.convert(&values[idx]);
                        match key {
                            LiteralMapKey::Property {
                                key, quoted, ..
                            } => o::LiteralMapEntry::Property {
                                key: key.clone(),
                                value,
                                quoted: *quoted,
                            },
                            LiteralMapKey::Spread { .. } => {
                                o::LiteralMapEntry::Spread { expression: value }
                            }
                        }
                    })
                    .collect();
                Expr::bare(ExprKind::LiteralMap {
                    entries,
                    value_type: None,
                })
            }

            // Interpolation — there is no faithful single-expression lowering: the
            // render3 pipeline emits a dedicated `ɵɵtextInterpolateN` / `pureFunctionN`
            // instruction per interpolation. As a self-contained stand-in we fold the
            // parts into string concatenation `"s0" + e0 + "s1" + ...`, which is
            // value-equivalent for property bindings.
            // NOTE(port): instruction-level interpolation lowering is deferred.
            EK::Interpolation {
                strings,
                expressions,
            } => self.convert_interpolation(strings, expressions),

            // Binary.
            EK::Binary {
                operation,
                left,
                right,
            } => {
                let op = binary_operator(*operation)
                    .expect("every BinaryOperation maps to a BinaryOperator");
                let lhs = self.convert(left);
                let rhs = self.convert(right);
                binary_expr(op, lhs, rhs)
            }

            // Unary `+x` / `-x`.
            EK::Unary { operator, expr } => {
                let inner = self.convert(expr);
                let op = match operator {
                    EUnary::Plus => OUnary::Plus,
                    EUnary::Minus => OUnary::Minus,
                };
                o::unary(op, inner, None)
            }

            // PrefixNot `!x`.
            EK::PrefixNot { expression } => not(self.convert(expression)),

            // typeof x.
            EK::TypeofExpression { expression } => typeof_expr(self.convert(expression)),

            // void x.
            EK::VoidExpression { expression } => {
                Expr::bare(ExprKind::Void(Box::new(self.convert(expression))))
            }

            // NonNullAssert `x!` — a no-op at codegen time (drop the assertion).
            EK::NonNullAssert { expression } => self.convert(expression),

            // Conditional `c ? t : f`.
            EK::Conditional {
                condition,
                true_exp,
                false_exp,
            } => {
                let cond = self.convert(condition);
                let t = self.convert(true_exp);
                let f = self.convert(false_exp);
                cond.conditional(t, Some(f))
            }

            // Chain in a property-binding context is unexpected (it is only valid in
            // actions); fold to a comma expression so we never panic.
            EK::Chain { expressions } => {
                let parts = expressions.iter().map(|x| self.convert(x)).collect();
                Expr::bare(ExprKind::Comma(parts))
            }

            // ParenthesizedExpression.
            EK::ParenthesizedExpression { expression } => {
                Expr::bare(ExprKind::Parenthesized(Box::new(self.convert(expression))))
            }

            // ArrowFunction `(params) => body`.
            EK::ArrowFunction { parameters, body } => {
                let params = parameters
                    .iter()
                    .map(|p| match p {
                        e::ArrowFunctionParameter::Identifier(id) => {
                            FnParam::new(id.name.clone(), Some(dynamic_type()))
                        }
                    })
                    .collect();
                let body = self.convert(body);
                o::arrow_fn(params, ArrowBody::Expr(Box::new(body)), None)
            }

            // TemplateLiteral `` `a${x}b` ``.
            EK::TemplateLiteral {
                elements,
                expressions,
            } => {
                let els = elements
                    .iter()
                    .map(|el| o::TemplateLiteralElement::new(el.text.clone(), None))
                    .collect();
                let exprs = expressions.iter().map(|x| self.convert(x)).collect();
                Expr::bare(ExprKind::TemplateLiteral {
                    elements: els,
                    expressions: exprs,
                })
            }

            // A standalone TemplateLiteralElement node.
            EK::TemplateLiteralElement { text } => Expr::bare(ExprKind::TemplateLiteralElement(
                o::TemplateLiteralElement::new(text.clone(), None),
            )),

            // TaggedTemplateLiteral `tag`...``.
            EK::TaggedTemplateLiteral { tag, template } => {
                let tag = self.convert(tag);
                let template = self.convert(template);
                o::tagged_template(tag, template, None)
            }

            // RegularExpressionLiteral `/body/flags`.
            EK::RegularExpressionLiteral { body, flags } => Expr::bare(ExprKind::RegExpLiteral {
                body: body.clone(),
                flags: flags.clone(),
            }),

            // BindingPipe — needs pure-function / pipeBind slot allocation that is
            // not yet ported. Emit a visible placeholder rather than wrong code.
            // NOTE(port): pipe lowering requires slot allocation (PipeBindN / pure
            // function slots); fall back to a marker call `__pipe(name, exp, ...args)`.
            EK::BindingPipe {
                exp, name, args, ..
            } => {
                let mut call_args = Vec::with_capacity(args.len() + 1);
                call_args.push(self.convert(exp));
                for a in args {
                    call_args.push(self.convert(a));
                }
                o::variable(format!("__pipe_{name}"), None).call_fn(call_args, false)
            }
        }
    }

    /// Expand a safe-navigation access (`a?.b`, `a?.[k]`, `f?.(args)`) into a
    /// guarded temporary, mirroring the classic `_AstToIrVisitor` safe-navigation
    /// lowering (`compiler_util/expression_converter.ts`).
    ///
    /// `a?.b` becomes `(tmp = a) == null ? null : tmp.b`: a temporary captures the
    /// receiver so it is evaluated exactly once, the guard short-circuits to `null`
    /// when the receiver is nullish, and `build` applies the actual access
    /// (`.prop` / `.key` / `.call_fn`) to the temporary on the safe branch.
    ///
    /// Nested safe chains compose naturally: for `a?.b?.c` the receiver of the
    /// outer read is itself a `SafePropertyRead`, so `self.convert(receiver)`
    /// already yields the inner ternary, which we then wrap in a fresh guard —
    /// producing `(tmp_1 = (tmp_0 = ctx.a) == null ? null : tmp_0.b) == null ? null : tmp_1.c`,
    /// exactly as the classic converter does.
    fn convert_safe(
        &mut self,
        receiver: &AstNode,
        build: impl FnOnce(Expr) -> Expr,
    ) -> Expr {
        let recv = self.convert(receiver);
        let tmp = self.allocate_temporary();
        // guard = (tmp = <receiver>)
        let guard = tmp.clone().set(recv);
        // access = tmp.<...>
        let access = build(tmp);
        // (tmp = recv) == null ? null : access
        guard
            .equals(literal(OLit::Null, None))
            .conditional(literal(OLit::Null, None), Some(access))
    }

    /// Fold an interpolation into string concatenation (see the `Interpolation`
    /// arm). With `strings = [s0, s1, ... sN]` and `exprs = [e0 ... e(N-1)]` the
    /// result is `s0 + e0 + s1 + ... + e(N-1) + sN`, skipping empty string parts
    /// except when the whole interpolation is a single literal.
    fn convert_interpolation(&mut self, strings: &[String], exprs: &[AstNode]) -> Expr {
        // Build the ordered list of pieces, dropping empty leading/trailing/middle
        // string literals (they contribute nothing to a `+` chain).
        let mut pieces: Vec<Expr> = Vec::new();
        for (i, s) in strings.iter().enumerate() {
            if !s.is_empty() {
                pieces.push(literal(OLit::String(s.clone()), None));
            }
            if let Some(ex) = exprs.get(i) {
                pieces.push(self.convert(ex));
            }
        }
        if pieces.is_empty() {
            return literal(OLit::String(String::new()), None);
        }
        let mut iter = pieces.into_iter();
        let mut acc = iter.next().unwrap();
        for piece in iter {
            acc = binary_expr(BinaryOperator::Plus, acc, piece);
        }
        acc
    }
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expression::ast::{
        AbsoluteSourceSpan, ArrowFunctionIdentifierParameter, AstNode, ExprKind as EK,
        LiteralValue as ELit, ParseSpan,
    };
    use crate::output::emitter::emit_expression;

    fn sp() -> ParseSpan {
        ParseSpan::new(0, 0)
    }
    fn ab() -> AbsoluteSourceSpan {
        AbsoluteSourceSpan::new(0, 0)
    }
    fn node(kind: EK) -> AstNode {
        AstNode::new(sp(), ab(), kind)
    }
    fn implicit() -> AstNode {
        node(EK::ImplicitReceiver)
    }
    /// `a` off the implicit receiver.
    fn prop(name: &str) -> AstNode {
        node(EK::PropertyRead {
            name_span: ab(),
            receiver: Box::new(implicit()),
            name: name.to_string(),
        })
    }
    fn num(n: f64) -> AstNode {
        node(EK::LiteralPrimitive {
            value: ELit::Num(n),
        })
    }
    fn ctx() -> Expr {
        o::variable("ctx", None)
    }
    fn emit(n: &AstNode) -> String {
        let r = convert_property_binding(n, ctx(), "0");
        assert!(r.stmts.is_empty());
        emit_expression(&r.expr)
    }

    #[test]
    fn interpolation_single_prop() {
        // {{a}} -> ctx.a   (strings = ["", ""], exprs = [a])
        let n = node(EK::Interpolation {
            strings: vec![String::new(), String::new()],
            expressions: vec![prop("a")],
        });
        assert_eq!(emit(&n), "ctx.a;\n");
    }

    #[test]
    fn property_read_implicit() {
        // a -> ctx.a
        assert_eq!(emit(&prop("a")), "ctx.a;\n");
    }

    #[test]
    fn nested_property_read() {
        // a.b.c -> ctx.a.b.c
        let a = prop("a");
        let ab_ = node(EK::PropertyRead {
            name_span: ab(),
            receiver: Box::new(a),
            name: "b".to_string(),
        });
        let abc = node(EK::PropertyRead {
            name_span: ab(),
            receiver: Box::new(ab_),
            name: "c".to_string(),
        });
        assert_eq!(emit(&abc), "ctx.a.b.c;\n");
    }

    #[test]
    fn binary_add() {
        // a + b -> ctx.a + ctx.b
        let n = node(EK::Binary {
            operation: BinaryOperation::Add,
            left: Box::new(prop("a")),
            right: Box::new(prop("b")),
        });
        assert_eq!(emit(&n), "ctx.a + ctx.b;\n");
    }

    #[test]
    fn conditional() {
        // x ? y : z -> ctx.x ? ctx.y : ctx.z
        let n = node(EK::Conditional {
            condition: Box::new(prop("x")),
            true_exp: Box::new(prop("y")),
            false_exp: Box::new(prop("z")),
        });
        assert_eq!(emit(&n), "ctx.x ? ctx.y : ctx.z;\n");
    }

    #[test]
    fn call_with_arg() {
        // fn(a) -> ctx.fn(ctx.a)
        let n = node(EK::Call {
            receiver: Box::new(prop("fn")),
            args: vec![prop("a")],
            argument_span: ab(),
        });
        assert_eq!(emit(&n), "ctx.fn(ctx.a);\n");
    }

    #[test]
    fn literal_array() {
        // [1, 2] -> [1, 2]
        let n = node(EK::LiteralArray {
            expressions: vec![num(1.0), num(2.0)],
        });
        assert_eq!(emit(&n), "[1, 2];\n");
    }

    #[test]
    fn keyed_read() {
        // a[b] -> ctx.a[ctx.b]
        let n = node(EK::KeyedRead {
            receiver: Box::new(prop("a")),
            key: Box::new(prop("b")),
        });
        assert_eq!(emit(&n), "ctx.a[ctx.b];\n");
    }

    #[test]
    fn prefix_not() {
        // !a -> !ctx.a
        let n = node(EK::PrefixNot {
            expression: Box::new(prop("a")),
        });
        assert_eq!(emit(&n), "!ctx.a;\n");
    }

    #[test]
    fn non_null_assert_passthrough() {
        // a! -> ctx.a  (assertion dropped)
        let n = node(EK::NonNullAssert {
            expression: Box::new(prop("a")),
        });
        assert_eq!(emit(&n), "ctx.a;\n");
    }

    #[test]
    fn literal_map() {
        // {k: a} -> { k: ctx.a }
        let n = node(EK::LiteralMap {
            keys: vec![LiteralMapKey::Property {
                key: "k".to_string(),
                quoted: false,
                span: sp(),
                source_span: ab(),
                is_shorthand_initialized: false,
            }],
            values: vec![prop("a")],
        });
        // emitter prints object literal; just assert it contains the entry.
        let out = emit(&n);
        assert!(out.contains("k:"), "got {out}");
        assert!(out.contains("ctx.a"), "got {out}");
    }

    #[test]
    fn unary_minus() {
        // -a -> -ctx.a
        let n = AstNode::create_minus(sp(), ab(), prop("a"));
        let out = emit(&n);
        assert!(out.contains("ctx.a"), "got {out}");
        assert!(out.contains('-'), "got {out}");
    }

    fn safe_prop(receiver: AstNode, name: &str) -> AstNode {
        node(EK::SafePropertyRead {
            name_span: ab(),
            receiver: Box::new(receiver),
            name: name.to_string(),
        })
    }

    #[test]
    fn safe_property_read_guard_expansion() {
        // a?.b -> (tmp_0 = ctx.a) == null ? null : tmp_0.b
        let n = safe_prop(prop("a"), "b");
        assert_eq!(
            emit(&n),
            "(tmp_0 = ctx.a) == null ? null : tmp_0.b;\n"
        );
    }

    #[test]
    fn safe_property_read_chained() {
        // a?.b?.c -> nested guard: the inner safe read is the receiver of the outer.
        // (tmp_1 = (tmp_0 = ctx.a) == null ? null : tmp_0.b) == null ? null : tmp_1.c
        let n = safe_prop(safe_prop(prop("a"), "b"), "c");
        assert_eq!(
            emit(&n),
            "(tmp_1 = (tmp_0 = ctx.a) == null ? null : tmp_0.b) == null ? null : tmp_1.c;\n"
        );
    }

    #[test]
    fn safe_call_guard_expansion() {
        // a?.m() -> the receiver `a?.m` is itself a SafePropertyRead, then SafeCall.
        // SafeCall guards the resolved method: (tmp = <a?.m>) == null ? null : tmp()
        // a.m?.() form: build SafeCall directly on `a.m` (plain prop) for clarity.
        let callee = node(EK::PropertyRead {
            name_span: ab(),
            receiver: Box::new(prop("a")),
            name: "m".to_string(),
        });
        let n = node(EK::SafeCall {
            receiver: Box::new(callee),
            args: vec![prop("x")],
            argument_span: ab(),
        });
        assert_eq!(
            emit(&n),
            "(tmp_0 = ctx.a.m) == null ? null : tmp_0(ctx.x);\n"
        );
    }

    #[test]
    fn interpolation_with_text() {
        // strings = ["Hi ", "!"], exprs = [name] -> "Hi " + ctx.name + "!"
        let n = node(EK::Interpolation {
            strings: vec!["Hi ".to_string(), "!".to_string()],
            expressions: vec![prop("name")],
        });
        assert_eq!(emit(&n), "\"Hi \" + ctx.name + \"!\";\n");
    }

    #[test]
    fn action_binding_chain() {
        // a; b  -> stmt: ctx.a;   expr: ctx.b
        let n = node(EK::Chain {
            expressions: vec![prop("a"), prop("b")],
        });
        let r = convert_action_binding(&n, ctx(), "0");
        assert_eq!(r.stmts.len(), 1);
        assert_eq!(emit_expression(&r.expr), "ctx.b;\n");
    }

    #[test]
    fn local_resolver_overrides_implicit_read() {
        // With a resolver that maps `a` -> a local var `tmp`, `a` lowers to `tmp`.
        struct R;
        impl LocalResolver for R {
            fn resolve_implicit_receiver(&self) -> Expr {
                o::variable("ctx", None)
            }
            fn maybe_resolve_local(&self, name: &str) -> Option<Expr> {
                if name == "a" {
                    Some(o::variable("tmp", None))
                } else {
                    None
                }
            }
        }
        let r = convert_property_binding_with(&prop("a"), &R);
        assert_eq!(emit_expression(&r.expr), "tmp;\n");
    }

    #[test]
    fn arrow_function() {
        // (x) => x  -> (x) => x
        let body = node(EK::PropertyRead {
            name_span: ab(),
            receiver: Box::new(implicit()),
            name: "x".to_string(),
        });
        let n = node(EK::ArrowFunction {
            parameters: vec![e::ArrowFunctionParameter::Identifier(
                ArrowFunctionIdentifierParameter {
                    name: "p".to_string(),
                    span: sp(),
                    source_span: ab(),
                },
            )],
            body: Box::new(body),
        });
        let out = emit(&n);
        assert!(out.contains("=>"), "got {out}");
    }
}
