//! Binding-expression AST -> output_ast converter.
//!
//! PORT TARGET: Angular's `compiler_util/expression_converter.ts`
//! (`convertPropertyBinding` / `convertActionBinding` / `convertUpdateArguments`).
//!
//! In Angular v22 the historic `compiler_util/expression_converter.ts` has been
//! retired; its job is now spread across the template *pipeline* — the
//! `e.AST -> o.Expression` lowering lives in `template/pipeline/src/ingest.ts`
//! (`convertAst`) and the operator table in `template/pipeline/src/conversion.ts`
//! (`BINARY_OPERATORS`). The pipeline lowers `PropertyRead(ImplicitReceiver)` to a
//! `LexicalReadExpr` placeholder that a *later* phase resolves against the view's
//! `ctx`. Since that view-`ctx` resolution phase is the view builder's job (not this
//! module's), this converter reproduces the **classic** `convertPropertyBinding`
//! behaviour directly: the implicit receiver is materialised as a caller-supplied
//! root expression (conventionally the `ctx` variable), so `{{a}}` lowers straight to
//! `ctx.a`. The mapping of every concrete node kind follows `convertAst` exactly (see
//! `ingest.ts:1097`), and the binary-operator table is a 1:1 copy of
//! `BINARY_OPERATORS` (`conversion.ts:12`). Instruction-level interpolation lowering
//! (`ɵɵinterpolateN`/`ɵɵinterpolateV`) is provided by
//! [`convert_interpolation_instruction`] (a port of `collateInterpolationArgs` +
//! `callVariadicInstructionExpr` from `template/pipeline/src/instruction.ts`).
//!
//! This converter is owned & arena-free: it consumes a borrowed
//! [`crate::expression::ast::AstNode`] and produces an owned
//! [`crate::output_ast::Expr`].

use crate::expression::ast::{
    self as e, AstNode, BinaryOperation, ExprKind as EK, LiteralMapKey, LiteralValue as ELit,
    UnaryOperator as EUnary,
};
use crate::identifiers::R3;
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

// ---------------------------------------------------------------------------
// Pipe-slot allocation abstraction.
// ---------------------------------------------------------------------------

/// The slot pair an [`EK::BindingPipe`] lowering needs: the **data slot** the
/// `ɵɵpipe(slot, "name")` creation instruction was allocated at, and the **var
/// offset** (binding/pure-function slot) the `ɵɵpipeBindN(slot, varOffset, …)`
/// update call reads for change detection. Mirrors the pipeline `PipeBindingExpr`
/// `{ targetSlot, varOffset }` (`reify.ts` → `ng.pipeBind(slot, varOffset, args)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipeSlots {
    /// Data slot of the `ɵɵpipe(slot, "name")` creation instruction.
    pub slot: usize,
    /// Var/change-detection offset passed as the second `ɵɵpipeBindN` argument.
    pub var_offset: usize,
}

/// `PipeSlotAllocator` — supplied by the view builder so the converter can turn a
/// `{{ x | name:args }}` ([`EK::BindingPipe`]) into a `ɵɵpipeBindN`/`ɵɵpipeBindV`
/// call against builder-allocated slots. The converter calls
/// [`Self::allocate_pipe`] once per pipe usage as it lowers the expression, in
/// source order; the builder records the `ɵɵpipe` creation instruction + slots and
/// reserves the matching var slots (`1 + total_args`, faithful to Angular
/// `varsUsedByOp` for `PipeBinding`/`PipeBindingVariadic`).
///
/// `total_args` is the full lowered argument count — the piped value plus the pipe
/// parameters (`x | slice:1:3` → 3) — i.e. exactly the `args.length` Angular feeds
/// `pipeBind`. Implementations use it for var-slot accounting and to pick the arity
/// instruction.
pub trait PipeSlotAllocator {
    /// Register one pipe usage (`name`, with `total_args` lowered arguments) and
    /// return its allocated [`PipeSlots`].
    fn allocate_pipe(&self, name: &str, total_args: usize) -> PipeSlots;

    /// Reserve the single binding (var) slot a hoisted template arrow function
    /// consumes (`ɵɵarrowFunction`'s `varOffset`; Angular `varsUsedByOp` returns `1`
    /// for an `ArrowFunction` IR expression — `var_counting.ts:186`), returning the
    /// var offset it was assigned.
    ///
    /// Defaulted so the only out-of-tree implementor (the view builder's pipe
    /// registry) need not be changed: the default returns `None`, signalling that no
    /// host var-slot pool is available, in which case [`Converter`] falls back to a
    /// self-contained slot counter rooted at the conventional first nested-binding
    /// offset (1). A builder that owns the var pool overrides this to draw the slot
    /// from the shared binding-slot cursor (so `vars` grows by one per hoisted arrow,
    /// matching Angular).
    fn allocate_arrow_slot(&self) -> Option<usize> {
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

/// Whether a lowered expression reads an embedded-view local — an ancestor shared-context
/// variable (`ctx_r<level>`) or a generated `@for` loop local (`item_r1`, `$index_r2`, …),
/// both of which carry the `_r<digits>` suffix Angular's `BindingScope` /
/// `variable_optimization` naming produces. Such reads mean the (would-be hoisted) arrow
/// depends on the embedded view's restored context, which the const-pool factory can only
/// reach via the deeper `ɵɵrestoreView`/`ɵɵnextContext` relocation the view builder owns;
/// until that lands we keep those arrows inline (value-correct) rather than emit a factory
/// that captures only the top-level `ctx`. Safe-navigation temporaries (`tmp_0`, `tmp_1`, …)
/// carry a `_<digit>` suffix but never `_r<digit>`, so they are not mistaken for view locals.
fn references_view_local(expr: &Expr) -> bool {
    fn name_is_view_local(name: &str) -> bool {
        // Match a trailing `_r<digits>` segment (e.g. `ctx_r3`, `item_r1`, `$index_r2`).
        if let Some(idx) = name.rfind("_r") {
            let tail = &name[idx + 2..];
            return !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit());
        }
        false
    }
    let mut found = false;
    visit_read_var_names(expr, &mut |name| {
        if name_is_view_local(name) {
            found = true;
        }
    });
    found
}

/// Walk every `ReadVar` name reachable from `expr`, invoking `f` for each. Covers the
/// expression shapes the binding converter produces (the arrow factory guard only needs to
/// see variable reads; statements inside function bodies are not produced on this path).
fn visit_read_var_names(expr: &Expr, f: &mut impl FnMut(&str)) {
    match &expr.kind {
        ExprKind::ReadVar { name } => f(name),
        ExprKind::Unary { expr, .. } => visit_read_var_names(expr, f),
        ExprKind::Not(inner)
        | ExprKind::Typeof(inner)
        | ExprKind::Void(inner)
        | ExprKind::Parenthesized(inner)
        | ExprKind::Spread(inner) => visit_read_var_names(inner, f),
        ExprKind::Binary { lhs, rhs, .. } => {
            visit_read_var_names(lhs, f);
            visit_read_var_names(rhs, f);
        }
        ExprKind::ReadProp { receiver, .. } => visit_read_var_names(receiver, f),
        ExprKind::ReadKey { receiver, index, .. } => {
            visit_read_var_names(receiver, f);
            visit_read_var_names(index, f);
        }
        ExprKind::Conditional {
            condition,
            true_case,
            false_case,
        } => {
            visit_read_var_names(condition, f);
            visit_read_var_names(true_case, f);
            if let Some(fc) = false_case {
                visit_read_var_names(fc, f);
            }
        }
        ExprKind::Invoke { callee, args, .. } => {
            visit_read_var_names(callee, f);
            for a in args {
                visit_read_var_names(a, f);
            }
        }
        ExprKind::New { class_expr, args } => {
            visit_read_var_names(class_expr, f);
            for a in args {
                visit_read_var_names(a, f);
            }
        }
        ExprKind::LiteralArray(entries) => {
            for e in entries {
                visit_read_var_names(e, f);
            }
        }
        ExprKind::LiteralMap { entries, .. } => {
            for entry in entries {
                match entry {
                    o::LiteralMapEntry::Property { value, .. } => visit_read_var_names(value, f),
                    o::LiteralMapEntry::Spread { expression } => visit_read_var_names(expression, f),
                }
            }
        }
        ExprKind::Comma(parts) => {
            for p in parts {
                visit_read_var_names(p, f);
            }
        }
        ExprKind::Arrow { body, .. } => match body {
            ArrowBody::Expr(e) => visit_read_var_names(e, f),
            ArrowBody::Block(_) => {}
        },
        ExprKind::TemplateLiteral { expressions, .. } => {
            for e in expressions {
                visit_read_var_names(e, f);
            }
        }
        ExprKind::TaggedTemplate { tag, template } => {
            visit_read_var_names(tag, f);
            visit_read_var_names(template, f);
        }
        // Leaves / shapes with no nested binding-expression children to inspect.
        _ => {}
    }
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

/// Like [`convert_property_binding_with`] but additionally lowers any
/// `{{ … | name:args }}` ([`EK::BindingPipe`]) to a `ɵɵpipeBindN`/`ɵɵpipeBindV`
/// call, allocating the pipe's data + var slots through `pipes`. Without an
/// allocator (the other entry points) a `BindingPipe` falls back to the
/// `__pipe_<name>(…)` placeholder, so non-pipe callers are unaffected.
pub fn convert_property_binding_with_pipes<R: LocalResolver, P: PipeSlotAllocator>(
    expr: &AstNode,
    resolver: &R,
    pipes: &P,
) -> ConvertedBinding {
    let mut cx = Converter::new(resolver).with_pipes(pipes);
    // This is the property/interpolation binding path (the only one Angular's
    // `generateArrowFunctions` rewrites), so a top-level user arrow here is hoisted
    // into a const-pool factory and emitted as `ɵɵarrowFunction(slot, factory, ctx)`.
    cx.hoist_arrows = true;
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

/// Which family of interpolation instruction to lower an interpolation to. Mirrors
/// the pipeline's `TEXT_INTERPOLATE_CONFIG` vs `VALUE_INTERPOLATE_CONFIG`
/// (`template/pipeline/src/instruction.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterpolationKind {
    /// `ɵɵtextInterpolate{N}` / `ɵɵtextInterpolateV` — bound text nodes.
    Text,
    /// `ɵɵinterpolate{N}` / `ɵɵinterpolateV` — value interpolations (property/attr).
    Value,
}

/// `collateInterpolationArgs` + `callVariadicInstructionExpr` from the render3
/// pipeline (`template/pipeline/src/instruction.ts`): lower an interpolation
/// (`strings`/`expressions`, `strings.len() == expressions.len() + 1`) to the real
/// `ɵɵinterpolate{N}` / `ɵɵinterpolateV` (or text-flavoured) call expression.
///
/// The collated argument list interleaves `[s0, e0, s1, e1, …, sN]`; a lone
/// `{{ e }}` (both surrounding strings empty) collapses to just `[e]`. A trailing
/// empty string is dropped (the runtime re-adds it). ≤8 expressions select the
/// arity-specialised `ɵɵinterpolate{N}`; more use `ɵɵinterpolateV(slot?, [args])`
/// — here the variadic form passes the collated args as a single array, matching
/// the pipeline (callers prepend any base/slot args).
///
/// This is the faithful instruction-level lowering the historic
/// `convertPropertyBinding` deferred to a later pipeline phase.
pub fn convert_interpolation_instruction<R: LocalResolver>(
    strings: &[String],
    expressions: &[AstNode],
    kind: InterpolationKind,
    resolver: &R,
) -> Expr {
    assert!(
        !strings.is_empty() && expressions.len() + 1 == strings.len(),
        "interpolation invariant: strings.len() == expressions.len() + 1"
    );
    let mut cx = Converter::new(resolver);

    // collateInterpolationArgs.
    let mut args: Vec<Expr> = Vec::new();
    if expressions.len() == 1 && strings[0].is_empty() && strings[1].is_empty() {
        args.push(cx.convert(&expressions[0]));
    } else {
        for (idx, ex) in expressions.iter().enumerate() {
            args.push(literal(OLit::String(strings[idx].clone()), None));
            args.push(cx.convert(ex));
        }
        // The last string.
        args.push(literal(OLit::String(strings[expressions.len()].clone()), None));
    }

    // Arity selection mirrors `callVariadicInstructionExpr`: `mapping` is `(n-1)/2`
    // computed BEFORE possibly dropping a trailing empty string. `n` is the number of
    // interpolation expressions.
    let n = expressions.len();

    // Drop a trailing empty-string literal (the runtime supplies it).
    if args.len() > 1 {
        if let Some(last) = args.last() {
            if matches!(&last.kind, ExprKind::Literal(OLit::String(s)) if s.is_empty()) {
                args.pop();
            }
        }
    }

    let (constant, variadic) = interpolation_refs(kind);
    if n < constant.len() {
        o::import_expr(constant[n].reference(), None).call_fn(args, false)
    } else {
        let arr = literal_arr(args, None);
        o::import_expr(variadic.reference(), None).call_fn(vec![arr], false)
    }
}

/// The `(constant[], variadic)` instruction table for an [`InterpolationKind`],
/// indexed by the expression count (`Interpolate{N}` for `N` in `0..=8`).
fn interpolation_refs(kind: InterpolationKind) -> ([R3; 9], R3) {
    match kind {
        InterpolationKind::Text => (
            [
                R3::TextInterpolate,
                R3::TextInterpolate1,
                R3::TextInterpolate2,
                R3::TextInterpolate3,
                R3::TextInterpolate4,
                R3::TextInterpolate5,
                R3::TextInterpolate6,
                R3::TextInterpolate7,
                R3::TextInterpolate8,
            ],
            R3::TextInterpolateV,
        ),
        InterpolationKind::Value => (
            [
                R3::Interpolate,
                R3::Interpolate1,
                R3::Interpolate2,
                R3::Interpolate3,
                R3::Interpolate4,
                R3::Interpolate5,
                R3::Interpolate6,
                R3::Interpolate7,
                R3::Interpolate8,
            ],
            R3::InterpolateV,
        ),
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
    /// Optional pipe-slot allocator. When `Some`, a `BindingPipe` lowers to a
    /// `ɵɵpipeBindN`/`ɵɵpipeBindV` call; when `None`, it falls back to the
    /// `__pipe_<name>(…)` placeholder (the historic behaviour of the non-pipe
    /// entry points).
    pipes: Option<&'r dyn PipeSlotAllocator>,
    /// Lexical scope of in-scope arrow-function parameter names. When converting
    /// the body of an `(params) => body` arrow, the parameter names shadow the
    /// implicit receiver, so a read of `value` inside `value => value + 1`
    /// resolves to the local `value` (a `ReadVarExpr`) rather than `ctx.value`.
    /// This mirrors Angular's `updateParameterReferences` (`ingest.ts:1993`),
    /// which rewrites any `LexicalReadExpr` whose name matches an enclosing
    /// arrow parameter into a plain `o.variable(name)` before the later
    /// implicit-receiver-resolution phase runs. Names accumulate across nested
    /// arrows (an inner arrow's params join the set), exactly as the pipeline does.
    arrow_params: Vec<String>,
    /// Whether a *user* arrow function written in this binding should be hoisted into
    /// a const-pool `(ctx, view) => …` factory and emitted as
    /// `ɵɵarrowFunction(slot, factory, ctx)` (Angular `generateArrowFunctions` +
    /// `reify.ts` `ArrowFunction`). Only the binding/interpolation entry point
    /// ([`convert_property_binding_with_pipes`]) enables this; event-handler actions
    /// keep arrows inline (Angular skips `Listener`/`TwoWayListener` ops because the
    /// handler needs `$event` and is never stored — `generate_arrow_functions.ts:19`),
    /// and the plain (non-pipe) entry points keep the historic inline form so their
    /// callers (e.g. `@for` trackBy lowering) are unaffected.
    hoist_arrows: bool,
    /// Running fallback counter for a hoisted arrow's `varOffset` when the host does
    /// not expose its binding-slot pool (`PipeSlotAllocator::allocate_arrow_slot`
    /// returns `None`). Seeded at `1` — the offset of the first *nested* binding when
    /// the consuming interpolation/property op has already reserved slot `0` — and
    /// advanced once per hoisted arrow so multiple arrows in one binding get distinct
    /// offsets. When the host *does* expose the pool the real offset is used instead.
    arrow_slot_fallback: std::cell::Cell<usize>,
}

impl<'r, R: LocalResolver> Converter<'r, R> {
    fn new(resolver: &'r R) -> Self {
        Converter {
            resolver,
            next_temp: 0,
            pipes: None,
            arrow_params: Vec::new(),
            hoist_arrows: false,
            arrow_slot_fallback: std::cell::Cell::new(1),
        }
    }

    /// Whether `name` is bound by an enclosing arrow-function parameter and thus
    /// shadows the implicit receiver. Mirrors the `parameterNames.has(expr.name)`
    /// test in `updateParameterReferences`.
    fn is_arrow_param(&self, name: &str) -> bool {
        self.arrow_params.iter().any(|p| p == name)
    }

    /// Attach a [`PipeSlotAllocator`] so `BindingPipe` nodes lower to real
    /// `ɵɵpipeBindN` calls.
    fn with_pipes(mut self, pipes: &'r dyn PipeSlotAllocator) -> Self {
        self.pipes = Some(pipes);
        self
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
                    // An enclosing arrow parameter shadows the implicit receiver:
                    // `value => value + 1` reads the local `value`, not `ctx.value`.
                    if self.is_arrow_param(name) {
                        o::variable(name.clone(), None)
                    } else if let Some(local) = self.resolver.maybe_resolve_local(name) {
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

            // Interpolation appearing as a *sub-expression* (rare; an interpolation is
            // normally a top-level binding value). There is no single render3 instruction
            // for a nested interpolation, so we fold the parts into string concatenation
            // `"s0" + e0 + "s1" + ...`, which is value-equivalent. The faithful
            // instruction-level lowering (`ɵɵinterpolateN`/`ɵɵinterpolateV`) for a
            // *top-level* interpolation is [`convert_interpolation_instruction`].
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
                let params: Vec<FnParam> = parameters
                    .iter()
                    .map(|p| match p {
                        e::ArrowFunctionParameter::Identifier(id) => {
                            FnParam::new(id.name.clone(), Some(dynamic_type()))
                        }
                        // Rest parameter `(...rest) => …`. `output_ast::FnParam` has
                        // no dedicated rest flag, so the `...` spread is carried in the
                        // emitted parameter name; the emitter's param lowering prints it
                        // verbatim, producing `(...rest)`.
                        e::ArrowFunctionParameter::Rest(rest) => {
                            FnParam::new(format!("...{}", rest.name), Some(dynamic_type()))
                        }
                    })
                    .collect();
                // Register the bound parameter names so reads of them inside the
                // body shadow the implicit receiver (Angular's
                // `updateParameterReferences`). Names accumulate across nested
                // arrows; we restore the prior scope length when this arrow's body
                // is done so a sibling arrow does not see these params.
                let scope_base = self.arrow_params.len();
                // An *outermost* arrow (no enclosing arrow params yet) in a binding
                // context is the one Angular hoists; a *nested* arrow stays inside the
                // factory body verbatim (the `InChildOperation` guard in
                // `generate_arrow_functions.ts`). We capture this before pushing this
                // arrow's params so the inner `self.convert` recursion sees `arrow_params`
                // non-empty and therefore never tries to hoist a nested arrow.
                let is_outermost = scope_base == 0;
                for p in parameters {
                    let bound = match p {
                        e::ArrowFunctionParameter::Identifier(id) => id.name.clone(),
                        // The rest parameter is bound by its bare name in the body
                        // (`(...rest) => rest[0]`), not the spread-prefixed form.
                        e::ArrowFunctionParameter::Rest(rest) => rest.name.clone(),
                    };
                    self.arrow_params.push(bound);
                }
                let body = self.convert(body);
                self.arrow_params.truncate(scope_base);
                let inline_arrow = o::arrow_fn(params, ArrowBody::Expr(Box::new(body)), None);

                // Hoist into a const-pool `ɵɵarrowFunction` factory when this is the
                // outermost arrow of a property/interpolation binding and the arrow only
                // captures the top-level component context (no ancestor `ɵɵnextContext`
                // context or embedded-view local — those need the deeper
                // restoreView/nextContext relocation the view builder performs, which we
                // leave inline rather than emit incorrectly).
                if self.hoist_arrows && is_outermost && !references_view_local(&inline_arrow) {
                    self.hoist_arrow_function(inline_arrow)
                } else {
                    inline_arrow
                }
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

            // BindingPipe — `exp | name:arg0:arg1`. With a `PipeSlotAllocator` this
            // lowers exactly like Angular's pipeline reify (`reify.ts` →
            // `ng.pipeBind`/`ng.pipeBindV`): the piped value plus the pipe arguments
            // form the lowered arg list `[value, ...args]`; ≤4 args use the
            // arity-specialised `ɵɵpipeBind{N}(slot, varOffset, ...args)`, >4 args use
            // `ɵɵpipeBindV(slot, varOffset, [args])` (`pipe_variadic.ts`). The slot /
            // var offset come from the builder's allocator. Without an allocator we keep
            // the historic `__pipe_<name>(…)` placeholder so non-pipe callers are
            // unaffected.
            EK::BindingPipe {
                exp, name, args, ..
            } => {
                let mut lowered_args = Vec::with_capacity(args.len() + 1);
                lowered_args.push(self.convert(exp));
                for a in args {
                    lowered_args.push(self.convert(a));
                }
                match self.pipes {
                    Some(pipes) => {
                        let slots = pipes.allocate_pipe(name, lowered_args.len());
                        let slot = literal(OLit::Number(slots.slot as f64), None);
                        let var_offset = literal(OLit::Number(slots.var_offset as f64), None);
                        // `args.length` here is the full lowered arg count (value + pipe
                        // params), matching the pipeline's variadic threshold (>4).
                        if lowered_args.len() <= 4 {
                            let reference = match lowered_args.len() {
                                1 => R3::PipeBind1,
                                2 => R3::PipeBind2,
                                3 => R3::PipeBind3,
                                // `lowered_args` always has ≥1 entry (the piped value);
                                // 4 is the only remaining case.
                                _ => R3::PipeBind4,
                            };
                            let mut call_args = Vec::with_capacity(lowered_args.len() + 2);
                            call_args.push(slot);
                            call_args.push(var_offset);
                            call_args.extend(lowered_args);
                            o::import_expr(reference.reference(), None).call_fn(call_args, false)
                        } else {
                            // `ɵɵpipeBindV(slot, varOffset, [value, ...args])`.
                            let arr = literal_arr(lowered_args, None);
                            o::import_expr(R3::PipeBindV.reference(), None)
                                .call_fn(vec![slot, var_offset, arr], false)
                        }
                    }
                    None => o::variable(format!("__pipe_{name}"), None)
                        .call_fn(lowered_args, false),
                }
            }
        }
    }

    /// Hoist a (already-lowered) user arrow into a const-pool factory and return the
    /// `ɵɵarrowFunction(slot, factory, ctx)` call that references it. Mirrors Angular's
    /// `generateArrowFunctions` (which moves the arrow into `unit.functions`) +
    /// `reify.ts`'s `ArrowFunction` case + `getArrowFunctionFactory`:
    ///
    /// - the factory is `(ctx, view) => <userArrow>` (`getArrowFunctionFactory` wraps the
    ///   arrow in a two-parameter `contextName`/`currentViewName` outer arrow);
    /// - the call is `ɵɵarrowFunction(varOffset, factory, ctx)` — the var offset is the
    ///   single binding slot the arrow consumes (`varsUsedByOp` ⇒ `1`), `factory` is the
    ///   hoisted reference, and the lone captured context argument is the component `ctx`
    ///   (`reify.ts:845` passes `o.variable(CONTEXT_NAME)`).
    ///
    /// The factory body already roots its implicit-receiver reads at `ctx` (this path is
    /// only taken when the arrow captures the top-level context — see the
    /// [`references_view_local`] guard), and the factory's first parameter is named `ctx`,
    /// so the captured `ctx` argument flows straight through with no rewriting needed.
    fn hoist_arrow_function(&mut self, inline_arrow: Expr) -> Expr {
        // The factory's binding (var) slot: drawn from the host's binding-slot pool when
        // the allocator exposes it, else a self-contained counter (see field docs).
        let slot = match self.pipes.and_then(|p| p.allocate_arrow_slot()) {
            Some(offset) => offset,
            None => {
                let next = self.arrow_slot_fallback.get();
                self.arrow_slot_fallback.set(next + 1);
                next
            }
        };

        // factory = (ctx, view) => <userArrow>
        let factory = o::arrow_fn(
            vec![
                FnParam::new("ctx", Some(dynamic_type())),
                FnParam::new("view", Some(dynamic_type())),
            ],
            ArrowBody::Expr(Box::new(inline_arrow)),
            None,
        );

        // ɵɵarrowFunction(slot, factory, ctx)
        let ctx_capture = o::variable("ctx", None);
        o::import_expr(R3::ArrowFunction.reference(), None).call_fn(
            vec![literal(OLit::Number(slot as f64), None), factory, ctx_capture],
            false,
        )
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

    /// `x` for use as the piped value, and helpers to build a `BindingPipe` node.
    fn binding_pipe(exp: AstNode, name: &str, args: Vec<AstNode>) -> AstNode {
        use crate::expression::ast::BindingPipeType;
        node(EK::BindingPipe {
            name_span: ab(),
            exp: Box::new(exp),
            name: name.to_string(),
            args,
            pipe_type: BindingPipeType::ReferencedByName,
        })
    }

    /// A test [`PipeSlotAllocator`] that hands out slot `10 + n` and var offset
    /// `100 + n` for the n-th registered pipe, recording `(name, total_args)`.
    struct MockPipes {
        log: std::cell::RefCell<Vec<(String, usize)>>,
    }
    impl MockPipes {
        fn new() -> Self {
            MockPipes {
                log: std::cell::RefCell::new(Vec::new()),
            }
        }
    }
    impl PipeSlotAllocator for MockPipes {
        fn allocate_pipe(&self, name: &str, total_args: usize) -> PipeSlots {
            let mut log = self.log.borrow_mut();
            let n = log.len();
            log.push((name.to_string(), total_args));
            PipeSlots {
                slot: 10 + n,
                var_offset: 100 + n,
            }
        }
    }

    /// A [`PipeSlotAllocator`] that additionally hands out a fixed arrow var slot, so the
    /// hoisting path can be exercised with a host-provided offset.
    struct ArrowPipes {
        arrow_slot: usize,
    }
    impl PipeSlotAllocator for ArrowPipes {
        fn allocate_pipe(&self, _name: &str, _total_args: usize) -> PipeSlots {
            PipeSlots {
                slot: 0,
                var_offset: 0,
            }
        }
        fn allocate_arrow_slot(&self) -> Option<usize> {
            Some(self.arrow_slot)
        }
    }

    #[test]
    fn arrow_hoisted_to_factory_in_binding_context() {
        // (param) => param + value + 1, lowered through the binding (pipes) path, hoists into a
        // `(ctx, view) => …` factory and emits `ɵɵarrowFunction(slot, factory, ctx)`.
        let body = node(EK::Binary {
            operation: BinaryOperation::Add,
            left: Box::new(node(EK::Binary {
                operation: BinaryOperation::Add,
                left: Box::new(prop("param")),
                right: Box::new(prop("value")),
            })),
            right: Box::new(num(1.0)),
        });
        let arrow = node(EK::ArrowFunction {
            parameters: vec![arrow_id_param("param")],
            body: Box::new(body),
        });
        let pipes = ArrowPipes { arrow_slot: 1 };
        let r = convert_property_binding_with_pipes(&arrow, &CtxResolver::ctx(), &pipes);
        let out = emit_expression(&r.expr);
        assert!(out.contains("\u{0275}\u{0275}arrowFunction("), "no arrowFunction, got: {out}");
        // The single binding slot (varOffset) the host allocated.
        assert!(out.contains("arrowFunction(1,"), "wrong slot, got: {out}");
        // Factory wraps the user arrow in a `(ctx, view) => …` outer arrow.
        assert!(out.contains("(ctx, view) =>"), "no factory wrapper, got: {out}");
        // Captured context argument is `ctx`; the body's `value` read becomes `ctx.value`.
        assert!(out.contains("ctx.value"), "body not ctx-rooted, got: {out}");
        // The bound parameter is NOT rewritten to `ctx.param`.
        assert!(out.contains("param + ctx.value + 1"), "param shadowing broke, got: {out}");
    }

    #[test]
    fn arrow_hoist_falls_back_to_local_slot_without_host_offset() {
        // Without a host-provided arrow slot (`allocate_arrow_slot` → None, the default), the
        // factory still emits with a self-contained slot counter rooted at 1.
        let arrow = node(EK::ArrowFunction {
            parameters: vec![arrow_id_param("a")],
            body: Box::new(prop("a")),
        });
        let pipes = MockPipes::new();
        let r = convert_property_binding_with_pipes(&arrow, &CtxResolver::ctx(), &pipes);
        let out = emit_expression(&r.expr);
        assert!(out.contains("arrowFunction(1,"), "fallback slot wrong, got: {out}");
        // No pipe was registered for the arrow.
        assert!(pipes.log.borrow().is_empty(), "arrow registered a pipe: {:?}", pipes.log.borrow());
    }

    #[test]
    fn arrow_referencing_view_local_stays_inline() {
        // An arrow whose body reads an ancestor shared-context var (`ctx_r3.x`) is NOT hoisted —
        // it needs the deeper restoreView/nextContext relocation, so it stays inline.
        struct NestedRes;
        impl LocalResolver for NestedRes {
            fn resolve_implicit_receiver(&self) -> Expr {
                o::variable("ctx_r3", None)
            }
        }
        let arrow = node(EK::ArrowFunction {
            parameters: vec![],
            body: Box::new(prop("x")),
        });
        let pipes = ArrowPipes { arrow_slot: 1 };
        let r = convert_property_binding_with_pipes(&arrow, &NestedRes, &pipes);
        let out = emit_expression(&r.expr);
        assert!(!out.contains("arrowFunction"), "should stay inline, got: {out}");
        assert!(out.contains("ctx_r3.x"), "got: {out}");
    }

    #[test]
    fn arrow_in_action_context_stays_inline() {
        // Event-handler actions keep arrows inline (Angular skips Listener ops in
        // generateArrowFunctions), so the action path must not hoist.
        let arrow = node(EK::ArrowFunction {
            parameters: vec![arrow_id_param("value")],
            body: Box::new(node(EK::Binary {
                operation: BinaryOperation::Add,
                left: Box::new(prop("value")),
                right: Box::new(num(1.0)),
            })),
        });
        let r = convert_action_binding(&arrow, ctx(), "0");
        let out = emit_expression(&r.expr);
        assert!(!out.contains("arrowFunction"), "action arrow should stay inline, got: {out}");
        assert!(out.contains("(value) => value + 1"), "got: {out}");
    }

    #[test]
    fn nested_arrow_inside_hoisted_arrow_stays_inline() {
        // a => b => a + b through the binding path: only the OUTER arrow is hoisted; the inner
        // arrow remains a plain arrow inside the factory body (Angular's InChildOperation guard).
        let inner = node(EK::ArrowFunction {
            parameters: vec![arrow_id_param("b")],
            body: Box::new(node(EK::Binary {
                operation: BinaryOperation::Add,
                left: Box::new(prop("a")),
                right: Box::new(prop("b")),
            })),
        });
        let outer = node(EK::ArrowFunction {
            parameters: vec![arrow_id_param("a")],
            body: Box::new(inner),
        });
        let pipes = ArrowPipes { arrow_slot: 2 };
        let r = convert_property_binding_with_pipes(&outer, &CtxResolver::ctx(), &pipes);
        let out = emit_expression(&r.expr);
        // Exactly one arrowFunction instruction (the outer), inner stays a plain arrow.
        assert_eq!(out.matches("\u{0275}\u{0275}arrowFunction(").count(), 1, "got: {out}");
        assert!(out.contains("(a) => (b) => a + b"), "nested arrow body wrong, got: {out}");
    }

    #[test]
    fn binding_pipe_without_allocator_is_placeholder() {
        // x | uppercase  ->  __pipe_uppercase(ctx.x)
        let n = binding_pipe(prop("x"), "uppercase", vec![]);
        assert_eq!(emit(&n), "__pipe_uppercase(ctx.x);\n");
    }

    #[test]
    fn binding_pipe_one_arg_lowers_to_pipe_bind1() {
        // x | uppercase  ->  ɵɵpipeBind1(slot, varOffset, ctx.x)
        let n = binding_pipe(prop("x"), "uppercase", vec![]);
        let pipes = MockPipes::new();
        let r = convert_property_binding_with_pipes(&n, &CtxResolver::ctx(), &pipes);
        let out = emit_expression(&r.expr);
        assert!(out.contains("\u{0275}\u{0275}pipeBind1("), "got: {out}");
        assert!(out.contains("10"), "slot missing, got: {out}");
        assert!(out.contains("100"), "varOffset missing, got: {out}");
        assert!(out.contains("ctx.x"), "value missing, got: {out}");
        // total_args = 1 (just the piped value).
        assert_eq!(pipes.log.borrow().as_slice(), &[("uppercase".to_string(), 1)]);
    }

    #[test]
    fn binding_pipe_two_args_lowers_to_pipe_bind3() {
        // x | slice:1:3  ->  ɵɵpipeBind3(slot, varOffset, ctx.x, 1, 3)
        let n = binding_pipe(prop("x"), "slice", vec![num(1.0), num(3.0)]);
        let pipes = MockPipes::new();
        let r = convert_property_binding_with_pipes(&n, &CtxResolver::ctx(), &pipes);
        let out = emit_expression(&r.expr);
        assert!(out.contains("\u{0275}\u{0275}pipeBind3("), "got: {out}");
        assert!(out.contains("ctx.x"), "value missing, got: {out}");
        assert!(out.contains('1') && out.contains('3'), "args missing, got: {out}");
        // total_args = 3 (value + two pipe args).
        assert_eq!(pipes.log.borrow().as_slice(), &[("slice".to_string(), 3)]);
    }

    #[test]
    fn binding_pipe_variadic_over_four_args() {
        // x | p:1:2:3:4:5  ->  ɵɵpipeBindV(slot, varOffset, [ctx.x, 1, 2, 3, 4, 5])
        let n = binding_pipe(
            prop("x"),
            "p",
            vec![num(1.0), num(2.0), num(3.0), num(4.0), num(5.0)],
        );
        let pipes = MockPipes::new();
        let r = convert_property_binding_with_pipes(&n, &CtxResolver::ctx(), &pipes);
        let out = emit_expression(&r.expr);
        assert!(out.contains("\u{0275}\u{0275}pipeBindV("), "got: {out}");
        // total_args = 6 (value + five pipe args).
        assert_eq!(pipes.log.borrow().as_slice(), &[("p".to_string(), 6)]);
    }

    #[test]
    fn interpolation_instruction_single_expr_collapses() {
        // {{a}} (strings ["",""]) -> ɵɵinterpolate1(ctx.a)  (lone-expr collapse).
        let r = convert_interpolation_instruction(
            &[String::new(), String::new()],
            &[prop("a")],
            InterpolationKind::Value,
            &CtxResolver::ctx(),
        );
        let out = emit_expression(&r);
        assert!(out.contains("\u{0275}\u{0275}interpolate1("), "got: {out}");
        assert!(out.contains("ctx.a"), "got: {out}");
        // Collapsed to a single arg (no empty-string literals).
        assert!(!out.contains("\"\""), "got: {out}");
    }

    #[test]
    fn interpolation_instruction_with_text_keeps_strings() {
        // "Hi {{name}}!" -> ɵɵinterpolate1("Hi ", ctx.name, "!")
        let r = convert_interpolation_instruction(
            &["Hi ".to_string(), "!".to_string()],
            &[prop("name")],
            InterpolationKind::Value,
            &CtxResolver::ctx(),
        );
        let out = emit_expression(&r);
        assert!(out.contains("\u{0275}\u{0275}interpolate1("), "got: {out}");
        assert!(out.contains("\"Hi \""), "got: {out}");
        assert!(out.contains("ctx.name"), "got: {out}");
        assert!(out.contains("\"!\""), "got: {out}");
    }

    #[test]
    fn interpolation_instruction_drops_trailing_empty_string() {
        // "x {{a}}" -> ɵɵinterpolate1("x ", ctx.a)  (trailing "" dropped).
        let r = convert_interpolation_instruction(
            &["x ".to_string(), String::new()],
            &[prop("a")],
            InterpolationKind::Value,
            &CtxResolver::ctx(),
        );
        let out = emit_expression(&r);
        assert!(out.contains("\u{0275}\u{0275}interpolate1("), "got: {out}");
        assert!(out.contains("\"x \""), "got: {out}");
        // The trailing empty string after ctx.a is dropped.
        assert!(!out.contains(", \"\")"), "trailing empty not dropped, got: {out}");
    }

    #[test]
    fn interpolation_instruction_text_flavor_and_variadic() {
        // 9 expressions -> ɵɵtextInterpolateV([...]) (over the 8-arity threshold).
        let strings: Vec<String> = (0..10).map(|i| format!("s{i}")).collect();
        let exprs: Vec<AstNode> = (0..9).map(|_| prop("a")).collect();
        let r = convert_interpolation_instruction(
            &strings,
            &exprs,
            InterpolationKind::Text,
            &CtxResolver::ctx(),
        );
        let out = emit_expression(&r);
        assert!(out.contains("\u{0275}\u{0275}textInterpolateV("), "got: {out}");
        assert!(out.contains('['), "variadic array missing, got: {out}");
    }

    /// Build an identifier arrow parameter node.
    fn arrow_id_param(name: &str) -> e::ArrowFunctionParameter {
        e::ArrowFunctionParameter::Identifier(ArrowFunctionIdentifierParameter {
            name: name.to_string(),
            span: sp(),
            source_span: ab(),
        })
    }

    #[test]
    fn arrow_function() {
        // (p) => x  -> a body read of a NON-parameter resolves to `ctx.x`.
        let body = prop("x");
        let n = node(EK::ArrowFunction {
            parameters: vec![arrow_id_param("p")],
            body: Box::new(body),
        });
        assert_eq!(emit(&n), "(p) => ctx.x;\n");
    }

    #[test]
    fn arrow_function_parameter_shadows_implicit_receiver() {
        // value => value + 1  -> the param `value` resolves to the local, NOT `ctx.value`.
        let body = node(EK::Binary {
            operation: BinaryOperation::Add,
            left: Box::new(prop("value")),
            right: Box::new(num(1.0)),
        });
        let n = node(EK::ArrowFunction {
            parameters: vec![arrow_id_param("value")],
            body: Box::new(body),
        });
        assert_eq!(emit(&n), "(value) => value + 1;\n");
    }

    #[test]
    fn arrow_function_non_parameter_still_reads_ctx() {
        // a => a + 1 + componentProp  -> `a` is local, `componentProp` resolves to `ctx`.
        let inner = node(EK::Binary {
            operation: BinaryOperation::Add,
            left: Box::new(prop("a")),
            right: Box::new(num(1.0)),
        });
        let body = node(EK::Binary {
            operation: BinaryOperation::Add,
            left: Box::new(inner),
            right: Box::new(prop("componentProp")),
        });
        let n = node(EK::ArrowFunction {
            parameters: vec![arrow_id_param("a")],
            body: Box::new(body),
        });
        assert_eq!(emit(&n), "(a) => a + 1 + ctx.componentProp;\n");
    }

    #[test]
    fn arrow_function_nested_accumulates_parameters() {
        // a => b => a + b  -> both `a` (outer) and `b` (inner) resolve as locals.
        let inner_body = node(EK::Binary {
            operation: BinaryOperation::Add,
            left: Box::new(prop("a")),
            right: Box::new(prop("b")),
        });
        let inner = node(EK::ArrowFunction {
            parameters: vec![arrow_id_param("b")],
            body: Box::new(inner_body),
        });
        let outer = node(EK::ArrowFunction {
            parameters: vec![arrow_id_param("a")],
            body: Box::new(inner),
        });
        assert_eq!(emit(&outer), "(a) => (b) => a + b;\n");
    }

    #[test]
    fn arrow_function_sibling_parameter_scope_is_restored() {
        // (a => a) + b  -> after the arrow body, `b` is NOT shadowed and reads `ctx.b`.
        let arrow = node(EK::ArrowFunction {
            parameters: vec![arrow_id_param("a")],
            body: Box::new(prop("a")),
        });
        let n = node(EK::Binary {
            operation: BinaryOperation::Add,
            left: Box::new(arrow),
            right: Box::new(prop("a")),
        });
        // The second `a` is outside the arrow's scope, so it resolves to `ctx.a`.
        assert_eq!(emit(&n), "((a) => a) + ctx.a;\n");
    }

    #[test]
    fn arrow_function_rest_parameter() {
        // (...rest) => rest[0]  -> the rest param lowers with a `...rest` name and the
        // body read of `rest` resolves to the local (not `ctx.rest`).
        use crate::expression::ast::ArrowFunctionRestParameter;
        let body = node(EK::KeyedRead {
            receiver: Box::new(prop("rest")),
            key: Box::new(num(0.0)),
        });
        let n = node(EK::ArrowFunction {
            parameters: vec![e::ArrowFunctionParameter::Rest(ArrowFunctionRestParameter {
                name: "rest".to_string(),
                span: sp(),
                source_span: ab(),
            })],
            body: Box::new(body),
        });
        assert_eq!(emit(&n), "(...rest) => rest[0];\n");
    }
}
