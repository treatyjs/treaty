//! `TemplateDefinitionBuilder` — the classic render3 template view compiler.
//!
//! Walks the render3 template AST ([`crate::template::r3_ast::Node`]), allocates data slots,
//! and emits the two ɵɵ-instruction streams — *creation* (`ɵɵelementStart`/`ɵɵelementEnd`/
//! `ɵɵelement`/`ɵɵtext`/`ɵɵtemplate`/`ɵɵlistener`/…) and *update* (`ɵɵproperty`/`ɵɵadvance`/
//! `ɵɵtextInterpolate1…8`/`ɵɵtextInterpolateV`/…) — as [`crate::output_ast`] statements, then
//! wraps them in the standard view function:
//!
//! ```text
//! function Tmpl(rf, ctx) {
//!   if (rf & 1) { /* creation instructions */ }
//!   if (rf & 2) { /* update instructions  */ }
//! }
//! ```
//!
//! Instruction references come from [`crate::identifiers::R3`]; the IR emitted is
//! [`crate::output_ast`] (`Expr`/`Stmt`), which a separate emitter lowers to OXC + JS.
//!
//! PORT NOTE (version): per `migration/render3-specs/12-view_template.md`, Angular 22 deleted
//! `TemplateDefinitionBuilder` and moved creation/update emission + slot allocation into the
//! `template/pipeline/**` phases. This module faithfully reproduces the *classic* TDB algorithm
//! (slot allocation, creation vs update split, advance bookkeeping, the `rf & 1`/`rf & 2`
//! function shape, text-interpolation arity selection) against the already-ported Rust
//! foundation, because that is the algorithm the brief targets. The pipeline rewrite is a
//! separate concern (`NOTE(port)`).
//!
//! Owned & arena-free (Box/Vec/String), matching the rest of the crate.

use crate::expression::ast::ExprKind as AstExprKind;
use crate::expression::ast::AstNode;
use crate::expression_converter::{
    convert_action_binding_with, convert_property_binding_with_pipes, LocalResolver,
    PipeSlotAllocator, PipeSlots,
};
use crate::identifiers::R3;
use crate::output_ast as o;
use crate::output_ast::{Expr, FnParam, Stmt, StmtKind, StmtModifier};
use crate::template::r3_ast::{
    BoundAttribute, BoundEvent, BoundText, Element, ForLoopBlock, IfBlock, Node, SwitchBlock,
    Template, Text, TextAttribute, Visitor,
};

// ---------------------------------------------------------------------------
// Constants (from `render3/view/util.ts`).
// ---------------------------------------------------------------------------

/// `RENDER_FLAGS = 'rf'` — the render-flags parameter name of every view function.
pub const RENDER_FLAGS: &str = "rf";
/// `CONTEXT_NAME = 'ctx'` — the component-context parameter name.
pub const CONTEXT_NAME: &str = "ctx";
/// `TEMPORARY_NAME = '_t'` — the temporary-variable name used by binding lowering.
pub const TEMPORARY_NAME: &str = "_t";
/// `EVENT_NAME = '$event'` — the implicit event parameter of a listener handler function.
pub const EVENT_NAME: &str = "$event";

/// `AttributeMarker.Classes` — marks the start of the class-name group in a static attrs array.
pub const ATTRIBUTE_MARKER_CLASSES: f64 = 1.0;
/// `AttributeMarker.Styles` — marks the start of the style key/value group in a static attrs array.
pub const ATTRIBUTE_MARKER_STYLES: f64 = 2.0;
/// `AttributeMarker.Bindings` — marks the start of the binding-name group in a static attrs array.
/// Each name following the marker was extracted from a property (`[name]`) input or an event
/// (`(name)`) output binding. See `core.ts` `AttributeMarker.Bindings = 3`.
pub const ATTRIBUTE_MARKER_BINDINGS: f64 = 3.0;

/// `core.RenderFlags` — the bitmask the view function branches on.
pub mod render_flags {
    /// Run the creation block (create elements/text/directives).
    pub const CREATE: f64 = 1.0;
    /// Run the update block (refresh bindings).
    pub const UPDATE: f64 = 2.0;
}

// ---------------------------------------------------------------------------
// Minimal owned metadata inputs.
//
// The classic TDB takes a `R3BoundTarget` (slot/var/ref resolution) and pipe/directive
// metadata. For this faithful-but-standalone port we allocate slots locally during the walk
// (exactly as TS `allocateDataSlot` does) and accept a tiny owned config struct. Wiring the
// real `crate::binder::R3BoundTarget` in is a `NOTE(port)` follow-up.
// ---------------------------------------------------------------------------

/// Minimal owned input describing the template to compile.
#[derive(Debug, Clone)]
pub struct TemplateCompilationInput {
    /// The generated view-function name (e.g. `AppComponent_Template`).
    pub name: String,
    /// The root template nodes to walk.
    pub nodes: Vec<Node>,
}

impl TemplateCompilationInput {
    pub fn new(name: impl Into<String>, nodes: Vec<Node>) -> Self {
        TemplateCompilationInput {
            name: name.into(),
            nodes,
        }
    }
}

// ---------------------------------------------------------------------------
// Constant pool (the subset the template builder collects).
// ---------------------------------------------------------------------------

/// The collected constants pool (the classic TDB `_constants.prepareStatements` / shared
/// `consts` array). Each entry is an `output_ast` expression that becomes one element of the
/// component definition's `consts:` array; the slot index recorded in instructions is the
/// entry's position. Only the attribute-array form is modelled here.
#[derive(Debug, Clone, Default)]
pub struct ConstantPool {
    entries: Vec<Expr>,
}

impl ConstantPool {
    pub fn new() -> Self {
        ConstantPool::default()
    }

    /// Intern an expression, returning its index. De-dupes structurally-equivalent entries
    /// (mirroring the classic pool's `getConstLiteral` sharing).
    pub fn intern(&mut self, value: Expr) -> usize {
        if let Some(i) = self.entries.iter().position(|e| e.is_equivalent(&value)) {
            return i;
        }
        self.entries.push(value);
        self.entries.len() - 1
    }

    pub fn entries(&self) -> &[Expr] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The `consts: [...]` literal-array expression, or `None` when empty.
    pub fn to_const_array(&self) -> Option<Expr> {
        if self.entries.is_empty() {
            None
        } else {
            Some(o::literal_arr(self.entries.clone(), None))
        }
    }
}

// ---------------------------------------------------------------------------
// Pipe slot allocation.
// ---------------------------------------------------------------------------

/// Sentinel base for a not-yet-finalised pipe data slot. During the view walk a
/// pipe's real data slot is unknown (pipe slots land at the *end* of the data
/// array, after every element/text/block slot), so [`BuilderPipes::allocate_pipe`]
/// returns `PIPE_SLOT_PLACEHOLDER + ordinal` and [`TemplateDefinitionBuilder::finalize_pipes`]
/// rewrites it to the real slot once the walk is complete. The base is far above any
/// realistic data-slot count so a placeholder is never mistaken for a real slot.
const PIPE_SLOT_PLACEHOLDER: usize = 1_000_000_000;

/// One registered pipe usage collected during the view walk. Mirrors Angular's
/// `PipeBindingExpr` bookkeeping (`pipe_creation.ts` records a `Pipe` create op per
/// usage). Each distinct *usage* (not name) reserves its own creation slot + var
/// slots, faithful to the classic TDB which allocates a fresh pipe slot per
/// occurrence.
#[derive(Debug, Clone)]
struct PendingPipe {
    /// The pipe name (`uppercase`, `slice`, …) — the `ɵɵpipe(slot, "name")` argument.
    name: String,
}

/// Per-view pipe registry. Lives behind a [`std::cell::RefCell`] on the builder so
/// the `&self` expression-lowering path ([`TemplateDefinitionBuilder::lower_expr`])
/// can register pipes and reserve their var slots while lowering.
#[derive(Debug, Default)]
struct PipeState {
    /// Registered usages, in source order; the index is the pipe's ordinal.
    pending: Vec<PendingPipe>,
    /// Running var-slot cursor, advanced by `1 + total_args` per pipe (Angular
    /// `varsUsedByOp` for `PipeBinding`/`PipeBindingVariadic`). Seeded from the
    /// builder's `binding_slots` when lowering begins and flushed back after.
    var_cursor: usize,
}

/// The [`PipeSlotAllocator`] the converter receives while a view's update
/// expressions are lowered. Borrows the builder's [`PipeState`] and allocates a
/// placeholder data slot + a real var offset per pipe usage.
struct BuilderPipes<'a> {
    state: &'a std::cell::RefCell<PipeState>,
}

impl PipeSlotAllocator for BuilderPipes<'_> {
    fn allocate_pipe(&self, name: &str, total_args: usize) -> PipeSlots {
        let mut state = self.state.borrow_mut();
        let ordinal = state.pending.len();
        state.pending.push(PendingPipe {
            name: name.to_string(),
        });
        // Var slots: one change-detection slot plus one per lowered argument
        // (`1 + args.length`), matching Angular `varsUsedByOp`.
        let var_offset = state.var_cursor;
        state.var_cursor += 1 + total_args;
        PipeSlots {
            slot: PIPE_SLOT_PLACEHOLDER + ordinal,
            var_offset,
        }
    }
}

// ---------------------------------------------------------------------------
// Embedded-view local variables (`@for` loop bindings).
// ---------------------------------------------------------------------------

/// A lexical local available inside an embedded view (e.g. a `@for` loop's item / `$index` /
/// `$count`). Mirrors Angular's `generate_variables` phase output: each becomes a
/// `const <name>_r<id> = ctx.<source>;` declaration at the top of the embedded view's update block,
/// and every implicit-receiver read of `<name>` inside the view lowers to the local `<name>_r<id>`
/// instead of `ctx.<name>` (`naming.ts` / `variable_optimization.ts`).
#[derive(Debug, Clone)]
struct LoopVar {
    /// The template-source name the author writes (`x`, `$index`, `$count`).
    source_name: String,
    /// The generated local identifier (`x_r1`, `$index_r2`, …).
    local_name: String,
}

/// A [`LocalResolver`] that roots the implicit receiver at `ctx` (like [`crate::expression_converter::CtxResolver`])
/// but additionally lowers reads of an embedded view's loop variables to their generated locals
/// (`x` → `x_r1`), faithful to Angular's `resolve_names` + `variable_optimization` phases.
struct LoopVarResolver<'a> {
    vars: &'a [LoopVar],
}

impl LocalResolver for LoopVarResolver<'_> {
    fn resolve_implicit_receiver(&self) -> Expr {
        o::variable(CONTEXT_NAME, None)
    }

    fn maybe_resolve_local(&self, name: &str) -> Option<Expr> {
        self.vars
            .iter()
            .find(|v| v.source_name == name)
            .map(|v| o::variable(v.local_name.clone(), None))
    }
}

/// A nesting-aware [`LocalResolver`] for an embedded (`view_level > 0`) view. Loop variables of the
/// current view resolve to their generated locals (`x` → `x_r1`); every other implicit read roots at
/// the ancestor context obtained via `ɵɵnextContext()` (`ctx` → `ctx_r<level>`), faithful to
/// Angular's `BindingScope` (`retrievalLevel` / `getOrCreateSharedContextVar` / `generateNextContextExpr`).
/// Reading the implicit receiver records that a `ɵɵnextContext()` declaration is needed (`needs`),
/// which [`TemplateDefinitionBuilder::build_template_function`] turns into the leading
/// `const ctx_r<level> = ɵɵnextContext();` of the update block.
struct NestedViewResolver<'a> {
    vars: &'a [LoopVar],
    /// The shared-context identifier for this view (`ctx_r<level>`).
    ctx_name: String,
    /// Set when the implicit receiver was consulted (an ancestor read occurred).
    needs: &'a std::cell::Cell<bool>,
}

impl LocalResolver for NestedViewResolver<'_> {
    fn resolve_implicit_receiver(&self) -> Expr {
        // An ancestor-context read: flag that this view needs a `ɵɵnextContext()` and root at the
        // shared context var.
        self.needs.set(true);
        o::variable(self.ctx_name.clone(), None)
    }

    fn maybe_resolve_local(&self, name: &str) -> Option<Expr> {
        self.vars
            .iter()
            .find(|v| v.source_name == name)
            .map(|v| o::variable(v.local_name.clone(), None))
    }
}

/// Resolver for an event-handler body: roots the implicit receiver at `ctx`, resolves any in-scope
/// `@for` loop variables to their generated locals, and resolves `$event` to the bare `$event`
/// handler parameter (Angular's `resolveDollarEvent` — `$event` reads must NOT become `ctx.$event`).
struct ListenerResolver<'a> {
    vars: &'a [LoopVar],
}

impl LocalResolver for ListenerResolver<'_> {
    fn resolve_implicit_receiver(&self) -> Expr {
        o::variable(CONTEXT_NAME, None)
    }

    fn maybe_resolve_local(&self, name: &str) -> Option<Expr> {
        if name == EVENT_NAME {
            return Some(o::variable(EVENT_NAME, None));
        }
        self.vars
            .iter()
            .find(|v| v.source_name == name)
            .map(|v| o::variable(v.local_name.clone(), None))
    }
}

// ---------------------------------------------------------------------------
// Helpers for building instruction calls.
// ---------------------------------------------------------------------------

fn num(n: f64) -> Expr {
    o::literal(o::LiteralValue::Number(n), None)
}

/// `sanitizeIdentifier(name)` (`parse_util.ts`): replace every non-word char (`/\W/g`, i.e.
/// anything outside `[A-Za-z0-9_]`) with `_`, so a tag like `ng-template` becomes `ng_template`
/// when used in a generated view-function name.
fn sanitize_identifier(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect()
}

/// Pop trailing `null` literal arguments off an instruction's parameter list, mirroring the
/// `while (args[args.length - 1].isEquivalent(o.NULL_EXPR)) args.pop()` tail in Angular's
/// `instruction.ts` builders (`templateBase` / `conditionalCreate` / …).
fn trim_trailing_nulls(params: &mut Vec<Expr>) {
    while params
        .last()
        .is_some_and(|p| p.is_equivalent(&o::null_expr()))
    {
        params.pop();
    }
}

fn str_lit(s: &str) -> Expr {
    o::literal(o::LiteralValue::String(s.to_string()), None)
}

/// `instruction(reference, params)` — `ɵɵfoo(...params)` as an expression statement, mirroring
/// the classic TDB `instruction()` helper (sans source-span span attachment).
fn instruction(reference: R3, params: Vec<Expr>) -> Stmt {
    o::import_expr(reference.reference(), None)
        .call_fn(params, false)
        .to_stmt()
}

/// Convert a [`o::fn_`] `FunctionExpr` into a top-level `function <name>(…) {…}` declaration
/// statement (`FunctionExpr.toDeclStmt(name)` in Angular's `output_ast`). Used to hoist a
/// branch/loop embedded-view function. Falls back to a no-op `const` if handed a non-function
/// expression (never happens for builder-produced view fns).
fn declare_function_from(name: &str, func: Expr) -> Stmt {
    match func.kind {
        o::ExprKind::Function {
            params, statements, ..
        } => Stmt::bare(StmtKind::DeclareFunction {
            name: name.to_string(),
            params,
            statements,
            ty: None,
        }),
        other => Stmt::bare(StmtKind::DeclareVar {
            name: name.to_string(),
            value: Some(Expr::bare(other)),
            ty: None,
        }),
    }
}

/// If `stmt` is an expression-statement wrapping a (non-optional, impure) call `Invoke`, return
/// `(callee, args)`; otherwise `None`. This is the shape produced by [`instruction`] — the only
/// thing the chaining pass folds.
fn as_call(stmt: &Stmt) -> Option<(&Expr, &[Expr])> {
    if let o::StmtKind::Expression(expr) = &stmt.kind {
        if let o::ExprKind::Invoke { callee, args, .. } = &expr.kind {
            return Some((callee, args.as_slice()));
        }
    }
    None
}

/// The `ɵɵ*` wire name of a callee expression when it is an `ExternalExpr` (`o::import_expr(R3)`).
/// This is what Angular's `chaining` phase keys its `CHAIN_COMPATIBILITY` map on (`fn.value`).
fn callee_wire_name(callee: &Expr) -> Option<&str> {
    if let o::ExprKind::External { value, .. } = &callee.kind {
        Some(value.name.as_str())
    } else {
        None
    }
}

/// Whether a call with callee `next` may be chained onto a run whose *first* call's callee is
/// `first`. Mirrors Angular's `CHAIN_COMPATIBILITY` map (`phases/chaining.ts`): the continuation
/// callee is keyed on the run's first instruction and is constant for the whole run. Almost every
/// chainable instruction continues with *itself* (so equivalent callees chain), with one
/// exception — `ɵɵconditionalCreate` continues with `ɵɵconditionalBranchCreate` (a `@if`/`@switch`
/// chain emits one `conditionalCreate` followed by `conditionalBranchCreate` branches).
fn chains_onto(first: &Expr, next: &Expr) -> bool {
    let cc = R3::ConditionalCreate.name();
    let cbc = R3::ConditionalBranchCreate.name();
    match (callee_wire_name(first), callee_wire_name(next)) {
        // `conditionalCreate` run absorbs subsequent `conditionalBranchCreate` calls.
        (Some(f), Some(n)) if f == cc => n == cbc,
        // Default: a run continues with the same instruction.
        _ => first.is_equivalent(next),
    }
}

/// Angular instruction chaining (`chainedInstruction` / the pipeline chaining phase).
///
/// Walks `stmts` and collapses every maximal run of *consecutive* expression-statements whose
/// call callee is structurally equal (via [`Expr::is_equivalent`]) to the run's first member
/// into a single statement holding the left-folded chained `Invoke`: each subsequent call is
/// applied to the prior `Invoke` expression, i.e. `callee(args0)(args1)(args2)…`. Single-element
/// runs (and any non-call statement, e.g. an interleaved `ɵɵadvance`) pass through unchanged —
/// which is exactly what breaks a run, so distinct instructions are never chained together.
fn chain_statements(stmts: Vec<Stmt>) -> Vec<Stmt> {
    let mut out: Vec<Stmt> = Vec::with_capacity(stmts.len());
    let mut iter = stmts.into_iter().peekable();

    while let Some(stmt) = iter.next() {
        // Only call-statements can start a chainable run.
        let Some((callee, args)) = as_call(&stmt) else {
            out.push(stmt);
            continue;
        };

        // Seed the folded expression with the first call: `callee(args0)`.
        let chain_callee = callee.clone();
        let mut folded = chain_callee
            .clone()
            .call_fn(args.to_vec(), false);

        // Absorb following statements whose callee is the expected continuation of this run's
        // first callee (Angular `CHAIN_COMPATIBILITY`: usually the same instruction, but a
        // `conditionalCreate` run continues with `conditionalBranchCreate`).
        let mut chained = false;
        while let Some(next) = iter.peek() {
            let Some((next_callee, next_args)) = as_call(next) else {
                break;
            };
            if !chains_onto(&chain_callee, next_callee) {
                break;
            }
            let next_args = next_args.to_vec();
            // Apply the next call to the prior `Invoke` expression: `…(prevArgs)(nextArgs)`.
            folded = folded.call_fn(next_args, false);
            chained = true;
            iter.next();
        }

        if chained {
            out.push(folded.to_stmt());
        } else {
            // Single-element run: leave the original statement untouched.
            out.push(stmt);
        }
    }

    out
}

// ---------------------------------------------------------------------------
// The builder.
// ---------------------------------------------------------------------------

/// `TemplateDefinitionBuilder` — produces the creation + update instruction streams for one
/// template (view), and assembles them into an `output_ast` view-function expression.
///
/// Slot bookkeeping mirrors the classic TDB:
/// - `data_index` is the next free data slot (`allocateDataSlot`).
/// - `binding_slots` tracks slots reserved for bindings (advance bookkeeping).
/// - `_creation_code` / `_update_code` accumulate the two instruction buffers.
/// - `current_advance` tracks how far `ɵɵadvance` has stepped so the next update emits the
///   correct delta before binding into a later slot.
#[derive(Debug)]
pub struct TemplateDefinitionBuilder {
    name: String,
    data_index: usize,
    /// Slot the update block has currently advanced to.
    advance_cursor: usize,
    /// Number of binding (var) slots reserved so far (`_bindingSlots` / `allocateBindingSlots`).
    binding_slots: usize,
    creation_code: Vec<Stmt>,
    update_code: Vec<Stmt>,
    const_pool: ConstantPool,
    /// Loop variables in scope for this (embedded) view — `@for` item / `$index` / `$count`.
    /// Empty for the root view. Reads of these names lower to their generated locals.
    loop_vars: Vec<LoopVar>,
    /// `const <item>_r<id> = ctx.$implicit;` declarations to emit at the head of this view's update
    /// block (generated for the loop variables this view actually references).
    update_prelude: Vec<Stmt>,
    /// Hoisted, named `@if`/`@for`/`@switch` branch/loop template functions collected while walking
    /// this view (and its descendants). Emitted as leading `DeclareFunction` statements of the
    /// produced view function, mirroring how Angular hoists `ConditionalCreate`/`RepeaterCreate`
    /// template fns onto the const pool (`emit.ts` `emitChildViews` → `pool.statements`).
    hoisted_fns: Vec<Stmt>,
    /// The component base name (the parent's name with a trailing `_Template` stripped) used to
    /// derive child view function names (`<Base>_Conditional_<slot>`, `<Base>_For_<slot>`), faithful
    /// to `naming.ts` (which roots child names at `job.componentName`, not the parent fn name).
    base_name: String,
    /// Monotonic counter seeding loop-variable suffixes (`_r1`, `_r2`, …), shared across the whole
    /// component so every embedded-view variable gets a globally-unique name (`naming.ts`
    /// `state.index`). Threaded into nested builders via [`Self::build_embedded_view`].
    var_counter: usize,
    /// Whether this is the outermost (root) view. The root inlines ALL collected hoisted child-view
    /// functions as leading declarations of its body (so a single self-contained expression carries
    /// the whole component, like Angular hoisting onto `pool.statements`). Embedded views do NOT
    /// inline them — their hoisted fns bubble up to the root via [`Self::build_embedded_view`] — so
    /// each child view fn is emitted exactly once, at the top level (never nested inside a parent
    /// view body).
    is_root: bool,
    /// This view's nesting depth (`retrievalLevel` in Angular's `BindingScope`): the root view is
    /// `0`, each embedded view is `parent.view_level + 1`. Used to (a) name temporary-variable
    /// spills (`tmp_<level>_<index>`, faithful to the pipeline temporary allocator) and (b) compute
    /// how many `ɵɵnextContext()` hops an ancestor-resolved implicit read needs.
    view_level: usize,
    /// Per-update-block counter seeding temporary-variable spill names (`tmp_<level>_0`,
    /// `tmp_<level>_1`, …). Reset per view (a fresh builder starts at 0).
    temp_counter: usize,
    /// Set during expression lowering when an implicit read in THIS (nested) view resolved against
    /// an ancestor context (i.e. it is not satisfied by a local of the current view). When set, a
    /// single `const ctx_r<id> = ɵɵnextContext();` is emitted at the head of this view's update
    /// block and ancestor reads lower against `ctx_r<id>`. `Cell` because the resolver borrows the
    /// builder immutably during `lower_expr`. See Angular `BindingScope.getOrCreateSharedContextVar`
    /// + the `ɵɵnextContext` insertion in `generateNextContextExpr`.
    needs_next_context: std::cell::Cell<bool>,
    /// The generated shared-context identifier for this view (`ctx_r<id>`), minted lazily the first
    /// time an ancestor read is lowered (so the `_rN` id is allocated from the same component-global
    /// `var_counter` as loop-variable locals, matching Angular's `naming.ts` shared counter).
    next_context_name: std::cell::RefCell<Option<String>>,
    /// Pipe usages collected while lowering this view's update expressions. `RefCell` because the
    /// pipe allocator borrows the builder immutably during `lower_expr`. After the walk,
    /// [`Self::finalize_pipes`] allocates each pipe's data slot at the END of the data array, emits
    /// the `ɵɵpipe(slot, "name")` creation instructions, and patches the placeholder slots in the
    /// update block. See Angular `pipe_creation.ts` + `slot_allocation.ts`.
    pipes: std::cell::RefCell<PipeState>,
}

impl TemplateDefinitionBuilder {
    /// Construct a builder for a single view from minimal owned input.
    pub fn new(input: &TemplateCompilationInput) -> Self {
        let base_name = input
            .name
            .strip_suffix("_Template")
            .unwrap_or(&input.name)
            .to_string();
        TemplateDefinitionBuilder {
            name: input.name.clone(),
            data_index: 0,
            advance_cursor: 0,
            binding_slots: 0,
            creation_code: Vec::new(),
            update_code: Vec::new(),
            const_pool: ConstantPool::new(),
            loop_vars: Vec::new(),
            update_prelude: Vec::new(),
            hoisted_fns: Vec::new(),
            base_name,
            var_counter: 0,
            is_root: true,
            view_level: 0,
            temp_counter: 0,
            needs_next_context: std::cell::Cell::new(false),
            next_context_name: std::cell::RefCell::new(None),
            pipes: std::cell::RefCell::new(PipeState::default()),
        }
    }

    /// The hoisted, named branch/loop template functions collected during
    /// [`Self::build_template_function`] (`DeclareFunction` statements). These are emitted as
    /// leading statements of the produced view function; callers assembling the component
    /// definition may also surface them at the top level (`pool.statements`).
    pub fn hoisted_functions(&self) -> &[Stmt] {
        &self.hoisted_fns
    }

    /// Lower a binding expression against this view's scope (`ctx` + any in-scope loop variables),
    /// resolving any `{{ … | pipe }}` to a `ɵɵpipeBindN`/`ɵɵpipeBindV` call. Inside a `@for` body it
    /// also rewrites item / `$index` / `$count` reads to their generated locals.
    ///
    /// Pipe usages register against this view's [`PipeState`]: each reserves `1 + total_args` var
    /// slots (Angular `varsUsedByOp`) — taken from the shared `binding_slots` pool *as the pipe is
    /// reached*, so a pipe's var offset follows the host binding's own slots — and a placeholder data
    /// slot finalised later by [`Self::finalize_pipes`].
    fn lower_expr(&mut self, node: &AstNode) -> Expr {
        // Seed the pipe var cursor at the current binding-slot count: pipe change-detection slots are
        // drawn from the same pool, immediately after the host binding's slots (Angular var_counting
        // assigns offsets to bindings in op order).
        self.pipes.borrow_mut().var_cursor = self.binding_slots;

        // Embedded (nested) views resolve ancestor-context reads via `ɵɵnextContext()` (`ctx_r<level>`),
        // recording the need in `needs_next_context`; loop locals still resolve to their generated
        // names. The root view roots everything at `ctx`.
        let expr = if self.view_level > 0 {
            let ctx_name = self.next_context_var_name();
            let resolver = NestedViewResolver {
                vars: &self.loop_vars,
                ctx_name,
                needs: &self.needs_next_context,
            };
            convert_property_binding_with_pipes(node, &resolver, &BuilderPipes { state: &self.pipes })
                .expr
        } else {
            let resolver = LoopVarResolver {
                vars: &self.loop_vars,
            };
            convert_property_binding_with_pipes(node, &resolver, &BuilderPipes { state: &self.pipes })
                .expr
        };

        // Flush any var slots the pipes consumed back into the view-wide binding-slot total.
        self.binding_slots = self.pipes.borrow().var_cursor;
        expr
    }

    /// The shared-context identifier (`ctx_r<level>`) for this embedded view — the `BindingScope`
    /// shared-context variable obtained from `ɵɵnextContext()`. Suffixed with the view's nesting
    /// level (`retrievalLevel`), matching Angular's `ctx_r<n>` naming. Cached so every ancestor read
    /// in the view uses the same identifier.
    fn next_context_var_name(&self) -> String {
        let mut slot = self.next_context_name.borrow_mut();
        if let Some(name) = slot.as_ref() {
            return name.clone();
        }
        let name = format!("ctx_r{}", self.view_level);
        *slot = Some(name.clone());
        name
    }

    /// Borrow the constant pool collected during [`Self::build_template_function`].
    pub fn const_pool(&self) -> &ConstantPool {
        &self.const_pool
    }

    /// The number of allocated data slots after [`Self::build_template_function`] — this is the
    /// `decls` count fed into the component definition (`allocateDataSlot` calls).
    pub fn data_index(&self) -> usize {
        self.data_index
    }

    /// The number of binding (`vars`) slots reserved after [`Self::build_template_function`] —
    /// this is the `vars` count fed into the component / `ɵɵtemplate` definition. Mirrors the
    /// classic TDB `_bindingSlots` total (the sum of `allocateBindingSlots` over every property /
    /// interpolation binding in this view).
    pub fn vars(&self) -> usize {
        self.binding_slots
    }

    /// `allocateDataSlot()` — reserve and return the next data slot index.
    fn allocate_data_slot(&mut self) -> usize {
        let slot = self.data_index;
        self.data_index += 1;
        slot
    }

    /// `allocateBindingSlots(value)` — reserve `count` binding (var) slots for an upcoming
    /// property / interpolation binding. Mirrors the classic TDB `allocateBindingSlots`
    /// (`_bindingSlots += …`) and the v22 `varsUsedByOp` accounting:
    /// - a plain property binding reserves **1** slot;
    /// - an interpolation reserves **N** slots for its `N` expressions (a property interpolation
    ///   reserves `1 + N`; a `textInterpolate` reserves exactly `N`).
    fn allocate_binding_slots(&mut self, count: usize) {
        self.binding_slots += count;
    }

    /// Emit a `ɵɵadvance(delta)` into the update buffer if the cursor needs to move to reach
    /// `slot` before binding into it (classic TDB `instructionFn(this._updateCodeFns, …,
    /// R3.advance, …)` after computing the delta from `_bindingSlots`/`_currentIndex`).
    fn advance_to(&mut self, slot: usize) {
        if slot > self.advance_cursor {
            let delta = slot - self.advance_cursor;
            // Angular elides the delta argument when it is 1 (`ɵɵadvance()` — the runtime
            // defaults the step to 1) and omits the instruction entirely when the delta is 0
            // (handled by the `slot > cursor` guard above).
            let params = if delta == 1 {
                vec![]
            } else {
                vec![num(delta as f64)]
            };
            self.update_code.push(instruction(R3::Advance, params));
            self.advance_cursor = slot;
        }
    }

    /// Finalise the pipe usages collected during the walk (`pipe_creation.ts` + `slot_allocation.ts`):
    /// allocate each pipe a data slot at the END of the data array (after every element/text/block
    /// slot), emit its `ɵɵpipe(slot, "name")` creation instruction, and patch the placeholder slots
    /// (`PIPE_SLOT_PLACEHOLDER + ordinal`) in the update block to the real slot. Pipe slots are taken
    /// in registration (source) order, so ordinal `k` maps to data slot `pipe_base + k`.
    fn finalize_pipes(&mut self) {
        let pending = std::mem::take(&mut self.pipes.borrow_mut().pending);
        if pending.is_empty() {
            return;
        }
        // Pipe slots come after every other data slot in this view.
        let pipe_base = self.data_index;
        self.data_index += pending.len();

        // Emit `ɵɵpipe(slot, "name")` per usage (creation block). Appending here places them after
        // all element/text creation instructions; chaining will fold the run of `ɵɵpipe` calls.
        for (ordinal, pending_pipe) in pending.iter().enumerate() {
            let slot = pipe_base + ordinal;
            self.creation_code.push(instruction(
                R3::Pipe,
                vec![num(slot as f64), str_lit(&pending_pipe.name)],
            ));
        }

        // Patch placeholder slot literals in the update block to their real slots.
        let mut update = std::mem::take(&mut self.update_code);
        for stmt in &mut update {
            remap_pipe_slots_in_stmt(stmt, pipe_base);
        }
        self.update_code = update;
    }

    /// Walk the root nodes and assemble the `function Name(rf, ctx) { … }` view function.
    ///
    /// Returns the `output_ast` function expression. The collected constants are available via
    /// [`Self::const_pool`].
    pub fn build_template_function(&mut self, input: &TemplateCompilationInput) -> Expr {
        // Walk the nodes, accumulating creation + update instructions.
        let nodes = input.nodes.clone();
        self.visit_all(&nodes);

        // Allocate pipe data slots (at the END of the data array), emit their `ɵɵpipe(slot,"name")`
        // creation instructions, and patch the placeholder slots in the update block.
        self.finalize_pipes();

        let mut statements: Vec<Stmt> = Vec::new();

        // Instruction chaining (Angular `chainedInstruction` / the pipeline chaining phase):
        // collapse each maximal run of adjacent expression-statements whose call callee is the
        // *same* instruction into one left-folded chained call. Applied to both buffers before
        // they are wrapped in the `rf`-guarded ifs.
        let creation = chain_statements(std::mem::take(&mut self.creation_code));
        // Update-block prelude order (Angular `generateNextContextExpr` then `generate_variables`):
        //   1. `const ctx_r<level> = ɵɵnextContext();` — once, when any expression in this nested
        //      view read an ancestor context (`needs_next_context` was set during lowering).
        //   2. embedded-view loop-variable `const`s (`const x_r1 = ctx.$implicit;`).
        // Both precede the chained binding instructions.
        let mut update_body: Vec<Stmt> = Vec::new();
        if self.needs_next_context.get() {
            let ctx_name = self.next_context_var_name();
            // `ɵɵnextContext(n)` — the hop count is the number of view levels walked up. The bare
            // `ɵɵnextContext()` (Angular elides the `1` argument) covers the common single-level case.
            let hops = self.view_level; // each level resolves to the immediately-enclosing view here.
            let args = if hops <= 1 { vec![] } else { vec![num(hops as f64)] };
            update_body.push(Stmt::with_modifiers(
                StmtKind::DeclareVar {
                    name: ctx_name,
                    value: Some(
                        o::import_expr(R3::NextContext.reference(), None).call_fn(args, false),
                    ),
                    ty: None,
                },
                StmtModifier::FINAL,
            ));
        }
        update_body.extend(std::mem::take(&mut self.update_prelude));
        update_body.extend(chain_statements(std::mem::take(&mut self.update_code)));
        let update = update_body;

        // `if (rf & 1) { creation }`
        if !creation.is_empty() {
            let cond = o::variable(RENDER_FLAGS, None)
                .bitwise_and(num(render_flags::CREATE));
            statements.push(o::if_stmt(cond, creation, None));
        }

        // `if (rf & 2) { update }`
        if !update.is_empty() {
            let cond = o::variable(RENDER_FLAGS, None)
                .bitwise_and(num(render_flags::UPDATE));
            statements.push(o::if_stmt(cond, update, None));
        }

        // Hoisted `@if`/`@for`/`@switch` branch/loop template functions are emitted as leading
        // `function …_Conditional_n_Template(rf, ctx) {…}` / `…_For_n_Template(…)` declarations.
        // Angular hoists these onto the `ConstantPool.statements` (siblings of the definition); this
        // standalone builder, which returns a single self-contained view-function expression, hoists
        // them to the head of the enclosing view body instead. Either way they are top-level NAMED
        // functions referenced by name from `ɵɵconditionalCreate`/`ɵɵrepeaterCreate` — never inline
        // closures. They are also reachable via [`Self::hoisted_functions`] for callers that prefer
        // to surface them at the true top level.
        //
        // Only the ROOT view inlines them: a child view's hoisted fns bubble up to the root via
        // [`Self::build_embedded_view`], so inlining them here too would emit each nested fn twice
        // (once inside the parent view body and once at the top level). Angular always hoists every
        // child view fn to a single top-level scope (`pool.statements`).
        let mut body: Vec<Stmt> = if self.is_root {
            self.hoisted_fns.clone()
        } else {
            Vec::new()
        };
        body.append(&mut statements);

        o::fn_(
            vec![
                FnParam::new(RENDER_FLAGS, None),
                FnParam::new(CONTEXT_NAME, None),
            ],
            body,
            None,
            Some(self.name.clone()),
        )
    }

    // -- Per-node lowering (the meat of the TDB walk). --

    /// Intern an element's static attributes + binding names into the const pool and return the
    /// attrs slot index, or `None` when the serialized array is empty (matching TS, which passes
    /// the const index only when an attrs array exists).
    ///
    /// Faithful to Angular's `serializeAttributes` (`const_collection.ts`): plain `name, value`
    /// pairs come first, then the `AttributeMarker`-prefixed groups in this fixed order — Classes
    /// (`1`), Styles (`2`), Bindings (`3`). A `class="box"` attribute becomes `[…, 1, "box"]`,
    /// `style="…"` becomes `[…, 2, "k", "v", …]`, and the names of `[prop]` / `(event)` bindings
    /// are collected under the Bindings marker (`[…, 3, "prop", "event"]`).
    fn element_attrs_index(
        &mut self,
        attributes: &[TextAttribute],
        binding_names: &[String],
    ) -> Option<usize> {
        // Angular's `serializeAttributes` encoding: plain `name, value` pairs come first, then the
        // marker-prefixed groups (Classes `1`, Styles `2`, Bindings `3`) in that fixed order.
        let mut plain: Vec<Expr> = Vec::new();
        let mut classes: Vec<Expr> = Vec::new();
        let mut styles: Vec<Expr> = Vec::new();

        for attr in attributes {
            match attr.name.as_str() {
                "class" => {
                    // `class="a b"` -> each class name as a bare string entry under the marker.
                    for cls in attr.value.split_whitespace() {
                        classes.push(str_lit(cls));
                    }
                }
                "style" => {
                    // `style="k: v; k2: v2"` -> alternating `"k", "v"` entries under the marker.
                    for decl in attr.value.split(';') {
                        let decl = decl.trim();
                        if decl.is_empty() {
                            continue;
                        }
                        let (k, v) = match decl.split_once(':') {
                            Some((k, v)) => (k.trim(), v.trim()),
                            None => (decl, ""),
                        };
                        styles.push(str_lit(k));
                        styles.push(str_lit(v));
                    }
                }
                _ => {
                    plain.push(str_lit(&attr.name));
                    plain.push(str_lit(&attr.value));
                }
            }
        }

        let mut entries: Vec<Expr> = plain;
        if !classes.is_empty() {
            entries.push(num(ATTRIBUTE_MARKER_CLASSES));
            entries.extend(classes);
        }
        if !styles.is_empty() {
            entries.push(num(ATTRIBUTE_MARKER_STYLES));
            entries.extend(styles);
        }
        if !binding_names.is_empty() {
            entries.push(num(ATTRIBUTE_MARKER_BINDINGS));
            entries.extend(binding_names.iter().map(|n| str_lit(n)));
        }
        if entries.is_empty() {
            return None;
        }
        Some(self.const_pool.intern(o::literal_arr(entries, None)))
    }

    /// Lower a static `Text` node: `ɵɵtext(slot, "value")`.
    fn build_text(&mut self, text: &Text) {
        let slot = self.allocate_data_slot();
        self.creation_code
            .push(instruction(R3::Text, vec![num(slot as f64), str_lit(&text.value)]));
    }

    /// Lower an interpolated `BoundText` node: `ɵɵtext(slot)` in creation, and the matching
    /// `ɵɵtextInterpolate{N}` (with a leading `ɵɵadvance`) in update.
    ///
    /// Reserves binding (var) slots equal to the number of interpolation expressions
    /// (`varsUsedByOp` for `InterpolateText`), matching the classic TDB `allocateBindingSlots`.
    fn build_bound_text(&mut self, bound: &BoundText) {
        let slot = self.allocate_data_slot();
        // Creation: a bare text node placeholder.
        self.creation_code
            .push(instruction(R3::Text, vec![num(slot as f64)]));

        // Reserve one var slot per interpolation expression.
        self.allocate_binding_slots(interpolation_expression_count(&bound.value));

        // Update: advance to the slot, then the arity-selected interpolation instruction. Each
        // interpolation expression lowers against this view's scope (`ctx` + any `@for` loop locals),
        // so `{{x}}` inside a loop body emits `x_r1`, not `ctx.x`; pipes (`{{ x | upper }}`) lower to
        // `ɵɵpipeBindN` here too. We pre-lower the expressions through `lower_expr` (which needs
        // `&mut self` for pipe registration), then feed them positionally to `text_interpolation_call`.
        self.advance_to(slot);
        let lowered: Vec<Expr> = match &bound.value.kind {
            AstExprKind::Interpolation { expressions, .. } => {
                expressions.iter().map(|e| self.lower_expr(e)).collect()
            }
            // Bare expression: a single `{{ expr }}`.
            _ => vec![self.lower_expr(&bound.value)],
        };
        let iter = std::cell::RefCell::new(lowered.into_iter());
        let lower = |_node: &AstNode| -> Expr {
            iter.borrow_mut()
                .next()
                .expect("text_interpolation_call requests one lowered expr per source expr")
        };
        let (reference, params) = text_interpolation_call(&bound.value, &lower);
        self.update_code.push(instruction(reference, params));
    }

    /// Lower an `Element`. Childless elements collapse to a single
    /// `ɵɵelement(slot, tag[, attrsIndex])`; otherwise `ɵɵelementStart … ɵɵelementEnd` wrap the
    /// recursively-built children.
    ///
    /// Bound outputs (`(event)="…"`) emit a creation-block `ɵɵdomListener` right after the element
    /// is created; bound inputs (`[name]="…"`) emit an update-block binding instruction
    /// (`ɵɵdomProperty`/`ɵɵclassProp`/`ɵɵstyleProp`/`ɵɵattribute`, preceded by an `ɵɵadvance` to the
    /// element's slot). This mirrors the classic TDB ordering exactly: listeners belong to the
    /// creation pass, binding refreshes to the update pass. The names of all bound inputs/outputs are
    /// also collected into the element's const attrs array under `AttributeMarker.Bindings` (`3`).
    fn build_element(&mut self, element: &Element) {
        let slot = self.allocate_data_slot();
        // Each `#ref` on the element reserves one extra data slot (Angular `liftLocalRefs`:
        // `numSlotsUsed += localRefs.length`).
        for _ in &element.references {
            self.allocate_data_slot();
        }

        // Collect the names that go under `AttributeMarker.Bindings` (`3`). Faithful to Angular's
        // `extractAttributes` (`phases/attribute_extraction.ts`): only **Property** inputs and
        // **Listener** (event) outputs extract a name into the element's const attrs array. A
        // *bound* `[class.x]`/`[style.x]` binding does NOT extract a name (the `StyleProp`/
        // `ClassProp` cases only emit an `ExtractedAttribute` when the expression is *empty*), and a
        // `[attr.x]` binding is likewise not extracted (only text attributes are).
        //
        // Binding-name ORDER (`attribute_extraction.ts`): every extracted name is
        // `insertBefore(..., elementOp)`, and the phase visits create-ops (listeners) before
        // update-ops (properties), so each later insertion lands immediately before the element —
        // i.e. event/output names precede property/input names.
        use crate::expression::ast::BindingType;
        let mut binding_names: Vec<String> = Vec::new();
        for output in &element.outputs {
            binding_names.push(output.name.clone());
        }
        for input in &element.inputs {
            if matches!(
                input.kind,
                BindingType::Class | BindingType::Style | BindingType::Attribute
            ) {
                continue;
            }
            binding_names.push(input.name.clone());
        }

        let attrs_index = self.element_attrs_index(&element.attributes, &binding_names);
        let local_refs_index = self.local_refs_index(&element.references);

        let has_children = !element.children.is_empty();
        // `collapseEmptyInstructions` (`phases/empty_elements.ts`) only merges an `elementStart` +
        // `elementEnd` into a single `element` when the `End` immediately follows the `Start` (only
        // `Pipe` ops are ignored in between). A **Listener** (a creation-block `ɵɵdomListener`) is
        // emitted between the start and end, so it blocks the merge — an element carrying any
        // `(event)` output stays in `elementStart … elementEnd` form even with no children.
        let needs_end = has_children || !element.outputs.is_empty();

        // `element`/`elementStart` reify to `(slot, tag, attributes, localRefs, span)`; the
        // trailing `null` attributes / localRefs args are trimmed (Angular `instruction.ts`).
        let mut params = vec![
            num(slot as f64),
            str_lit(&element.name),
            attrs_index.map(|i| num(i as f64)).unwrap_or_else(o::null_expr),
            local_refs_index
                .map(|i| num(i as f64))
                .unwrap_or_else(o::null_expr),
        ];
        trim_trailing_nulls(&mut params);

        // Creation: the element itself, then its listeners. Angular 21 emits the DOM-element
        // family (`ɵɵdomElementStart`/`ɵɵdomElementEnd`/`ɵɵdomElement`) for plain elements.
        if needs_end {
            self.creation_code
                .push(instruction(R3::DomElementStart, params));
        } else {
            self.creation_code.push(instruction(R3::DomElement, params));
        }

        // `(event)="handler"` → creation-block `ɵɵdomListener(...)`.
        for output in &element.outputs {
            self.build_listener(slot, &element.name, output);
        }

        // Update: `[name]="expr"` → binding instructions (`ɵɵdomProperty`/`ɵɵclassProp`/
        // `ɵɵstyleProp`/`ɵɵattribute`), each advancing to this slot first. The bindings are
        // re-ordered into Angular's fixed `UPDATE_ORDERING` groups (`phases/ordering.ts`):
        // style props, then class props, then (non-interpolation) properties, then attributes —
        // a stable sort, so within a group source order is preserved.
        let mut ordered: Vec<&BoundAttribute> = element.inputs.iter().collect();
        ordered.sort_by_key(|input| update_order_rank(input));
        for input in ordered {
            self.build_property(slot, input);
        }

        if has_children {
            let children = element.children.clone();
            self.visit_all(&children);
        }
        if needs_end {
            self.creation_code
                .push(instruction(R3::DomElementEnd, vec![]));
        }
    }

    /// Lower a bound input (`[name]="value"`) into the matching update-block binding instruction,
    /// advancing the update cursor to the host element's `slot` first. The instruction is selected
    /// off [`BoundAttribute::kind`], faithful to Angular's pipeline reify/var-counting phases:
    ///
    /// - [`BindingType::Property`] → `ɵɵdomProperty("name", <expr>)` (the DOM-only reification of a
    ///   property binding; reserves **1** var slot, `+N` for an interpolation).
    /// - [`BindingType::Class`] → `ɵɵclassProp("name", <expr>)` (reserves **2** var slots, `+N`).
    /// - [`BindingType::Style`] → `ɵɵstyleProp("name", <expr>)` (reserves **2** var slots, `+N`).
    /// - [`BindingType::Attribute`] → `ɵɵattribute("name", <expr>)` (reserves **1** var slot, `+N`).
    ///
    /// The binding expression is lowered via [`convert_property_binding`] against the `ctx`
    /// receiver, so `[id]="x"` → `ɵɵdomProperty("id", ctx.x)`.
    fn build_property(&mut self, slot: usize, input: &BoundAttribute) {
        use crate::expression::ast::BindingType;

        // Number of interpolation expressions contributing extra var slots.
        let interp_extra = match &input.value.kind {
            AstExprKind::Interpolation { expressions, .. } => expressions.len(),
            _ => 0,
        };

        // Reserve var slots per Angular `varsUsedByOp`: property/attribute = 1 (+N), class/style
        // = 2 (+N).
        let base_vars = match input.kind {
            BindingType::Class | BindingType::Style => 2,
            _ => 1,
        };
        self.allocate_binding_slots(base_vars + interp_extra);

        self.advance_to(slot);

        // Lower against this view's scope (`ctx` + any `@for` loop locals).
        let lowered = self.lower_expr(&input.value);
        // NOTE(port): safe-navigation / pipe temporaries are empty for the ported node kinds; once
        // they are produced they will be spilled before the instruction.
        let reference = match input.kind {
            BindingType::Property | BindingType::TwoWay => R3::DomProperty,
            BindingType::Class => R3::ClassProp,
            BindingType::Style => R3::StyleProp,
            BindingType::Attribute => R3::Attribute,
            // LegacyAnimation / Animation are not lowered here (NOTE(port)); fall back to a DOM
            // property so the binding still emits rather than panicking.
            BindingType::LegacyAnimation | BindingType::Animation => R3::DomProperty,
        };
        let params = vec![str_lit(&input.name), lowered];
        self.update_code.push(instruction(reference, params));
    }

    /// Lower a bound output (`(event)="handler"`) into a creation-block
    /// `ɵɵdomListener("event", function <fnName>($event) { return <action>; })`.
    ///
    /// Faithful to Angular 21: a regular listener reifies to `ɵɵdomListener` (the DOM-only listener
    /// instruction), and the handler function is named with the canonical
    /// `<viewFn>_<tag>_<event>_<slot>_listener` scheme (`naming.ts`; hyphens in the tag become
    /// underscores). The handler body is lowered via [`convert_action_binding`] (a possible
    /// statement [`crate::expression::ast::ExprKind::Chain`]); the final expression is `return`ed so
    /// the handler propagates its value.
    ///
    /// The `$event` parameter is emitted **only when the handler references it** — mirroring
    /// Angular's `resolveDollarEvent` (which sets `consumesDollarEvent` when a `$event`
    /// `LexicalReadExpr` is seen) + `reifyListenerHandler` (which pushes the `$event` `FnParam`
    /// only when `consumesDollarEvent` is set). So `(click)="f()"` emits a no-param handler.
    fn build_listener(&mut self, slot: usize, tag: &str, output: &BoundEvent) {
        // Lower the handler against a resolver that keeps `$event` a bare parameter read (Angular
        // `resolveDollarEvent`) and rewrites any in-scope `@for` loop vars to their locals.
        let resolver = ListenerResolver {
            vars: &self.loop_vars,
        };
        let converted = convert_action_binding_with(&output.handler, &resolver);

        // Handler body: any leading statements, then `return <final expr>;`.
        let mut body: Vec<Stmt> = converted.stmts;
        body.push(o::Stmt::bare(o::StmtKind::Return(converted.expr)));

        // Angular `naming.ts`: `${unit.fnName}_${tag.replace('-', '_')}_${event}_${slot}_listener`.
        let handler_name = format!(
            "{}_{}_{}_{}_listener",
            self.name,
            tag.replace('-', "_"),
            output.name,
            slot,
        );
        // Only add the `$event` parameter when the handler AST actually references `$event`.
        let params = if handler_references_dollar_event(&output.handler) {
            vec![FnParam::new(EVENT_NAME, None)]
        } else {
            vec![]
        };
        let handler_fn = o::fn_(
            params,
            body,
            None,
            Some(handler_name),
        );

        self.creation_code.push(instruction(
            R3::DomListener,
            vec![str_lit(&output.name), handler_fn],
        ));
    }

    /// Lower a `Template` (`<ng-template>` / desugared structural directive) to Angular 21's
    /// DOM-only form `ɵɵdomTemplate(slot, tmplFn, decls, vars, tagName, attrsIndex, localRefsIndex,
    /// ɵɵtemplateRefExtractor)` with trailing-`null` arguments trimmed (`instruction.ts`
    /// `templateBase`).
    ///
    /// The embedded view function is built by a nested [`TemplateDefinitionBuilder`]; its `decls`
    /// (data slots) and `vars` (binding slots) feed the instruction. Template-reference variables
    /// (`#tpl`) are lowered like Angular's `liftLocalRefs`: each `#ref` reserves one extra **decl**
    /// slot on the host element, the flattened `["ref", "target"]` pairs are interned as a single
    /// const, that const's index is passed as the `localRefs` argument, and
    /// [`R3::TemplateRefExtractor`] is appended as the `localRefExtractor`.
    fn build_template(&mut self, template: &Template) {
        let slot = self.allocate_data_slot();
        // Each `#ref` on the host reserves one extra data slot (`liftLocalRefs`:
        // `numSlotsUsed += localRefs.length`).
        for _ in &template.references {
            self.allocate_data_slot();
        }
        let attrs_index = self.element_attrs_index(&template.attributes, &[]);
        let local_refs_index = self.local_refs_index(&template.references);

        let tag_name = template
            .tag_name
            .clone()
            .unwrap_or_else(|| "ng-template".to_string());

        // Embedded-view fn name (Angular `naming.ts` `Template` case): `<base>_<suffix>_<slot>`,
        // where `suffix` is the tag name sanitized to an identifier (`ng-template` → `ng_template`),
        // then suffixed with `_Template`. The view is HOISTED as a named top-level function and
        // referenced by name (like control-flow blocks), not embedded inline.
        let suffix = sanitize_identifier(&tag_name);
        let fn_name = format!("{}_{}_{}_Template", self.base_name, suffix, slot);
        let (fn_ref, decls, vars) =
            self.build_embedded_view(fn_name, template.children.clone(), Vec::new(), Vec::new());

        // `templateBase` arg list: [slot, fn, decls, vars, tag, constIndex, localRefsIndex,
        // templateRefExtractor]. constIndex / localRefsIndex are `null` when absent; trailing
        // `null`s are trimmed.
        let mut params = vec![
            num(slot as f64),
            fn_ref,
            num(decls as f64),
            num(vars as f64),
            str_lit(&tag_name),
            attrs_index.map(|i| num(i as f64)).unwrap_or_else(o::null_expr),
        ];
        if let Some(refs_idx) = local_refs_index {
            params.push(num(refs_idx as f64));
            params.push(o::import_expr(R3::TemplateRefExtractor.reference(), None));
        }
        trim_trailing_nulls(&mut params);
        self.creation_code.push(instruction(R3::DomTemplate, params));
    }

    /// Lower a list of template-reference variables (`#ref` / `#ref="exportAs"`) into a single
    /// interned const holding the flattened `[name, target, …]` pairs, returning its const-pool
    /// index (or `None` when there are no refs). Mirrors Angular's `serializeLocalRefs`
    /// (`phases/local_refs.ts`): `#tpl` → `["tpl", ""]`.
    fn local_refs_index(&mut self, references: &[crate::template::r3_ast::Reference]) -> Option<usize> {
        if references.is_empty() {
            return None;
        }
        let mut entries: Vec<Expr> = Vec::with_capacity(references.len() * 2);
        for r in references {
            entries.push(str_lit(&r.name));
            entries.push(str_lit(&r.value));
        }
        Some(self.const_pool.intern(o::literal_arr(entries, None)))
    }

    /// Build a nested embedded-view function for a control-flow branch / loop body, HOISTING it as a
    /// named top-level `DeclareFunction` and returning `(fnNameRef, decls, vars)` — where `fnNameRef`
    /// is `o::variable(fn_name)`, the by-name reference the creation instruction
    /// (`ɵɵconditionalCreate`/`ɵɵrepeaterCreate`) passes (`reify.ts` uses `o.variable(childView.fnName)`).
    ///
    /// `loop_vars` are the embedded-view locals in scope (the `@for` item / `$index` / `$count`);
    /// `update_prelude` are their `const x_r1 = ctx.$implicit;` declarations to head the view's
    /// update block. The nested view's constants are merged into the parent pool (re-interned), and
    /// its own hoisted functions bubble up to this builder so the whole component's branch/loop fns
    /// end up hoisted together (Angular `emitChildViews` depth-first push onto `pool.statements`).
    fn build_embedded_view(
        &mut self,
        fn_name: String,
        children: Vec<Node>,
        loop_vars: Vec<LoopVar>,
        update_prelude: Vec<Stmt>,
    ) -> (Expr, usize, usize) {
        let nested_input = TemplateCompilationInput::new(fn_name.clone(), children);
        let mut nested = TemplateDefinitionBuilder::new(&nested_input);
        // An embedded view is never the root: its hoisted descendant fns bubble up to the root
        // (below) rather than being inlined into this view body, so each is emitted exactly once.
        nested.is_root = false;
        // This embedded view sits one nesting level below the current view (Angular `retrievalLevel`).
        nested.view_level = self.view_level + 1;
        // Thread the component-global variable counter so nested loop vars get unique `_rN` names.
        nested.var_counter = self.var_counter;
        nested.loop_vars = loop_vars;
        nested.update_prelude = update_prelude;
        let tmpl_fn = nested.build_template_function(&nested_input);
        let decls = nested.data_index;
        let vars = nested.binding_slots;
        self.var_counter = nested.var_counter;
        for entry in nested.const_pool.entries() {
            self.const_pool.intern(entry.clone());
        }
        // Hoist the nested view function itself, plus any functions it hoisted from deeper blocks.
        // Depth-first order (descendants first) matches Angular's `emitChildViews`.
        self.hoisted_fns.append(&mut nested.hoisted_fns);
        self.hoisted_fns.push(declare_function_from(&fn_name, tmpl_fn));
        (o::variable(fn_name, None), decls, vars)
    }

    /// Lower an `@if`/`@else if`/`@else` chain (`IfBlock`) to Angular 21's hoisted-template scheme:
    ///
    /// - the *first* branch becomes `ɵɵconditionalCreate(slot, <Name>_Conditional_<slot>_Template,
    ///   decls, vars, <tag>)` and each *subsequent* branch `ɵɵconditionalBranchCreate(slot, …)`,
    ///   where the branch body is a HOISTED, NAMED top-level function (not an inline closure) and
    ///   `<tag>` is the branch root's element tag (`'span'`) when the branch has a single element
    ///   root, else `null` (`ingestControlFlowInsertionPoint`);
    /// - the update block emits a single `ɵɵconditional(<test>)` whose test is the nested ternary
    ///   `cond_i ? slot_i : (… : (cond_0 ? slot_0 : -1))`, built back-to-front exactly like
    ///   `generateConditionalExpressions`. A branch with no condition (`@else`) becomes the
    ///   default result `slot` instead of `-1`.
    fn build_if_block(&mut self, block: &IfBlock) {
        // First branch's slot is the "conditional anchor" the update instruction advances to.
        let mut branch_slots: Vec<usize> = Vec::with_capacity(block.branches.len());
        let anchor_slot = self.data_index;

        for (i, branch) in block.branches.iter().enumerate() {
            let slot = self.allocate_data_slot();
            branch_slots.push(slot);
            let fn_name = format!("{}_Conditional_{}_Template", self.base_name, slot);
            let (fn_ref, decls, vars) =
                self.build_embedded_view(fn_name, branch.children.clone(), Vec::new(), Vec::new());
            // First branch -> ɵɵconditionalCreate, the rest -> ɵɵconditionalBranchCreate. The trailing
            // arg is the branch root element tag (or `null`).
            let reference = if i == 0 {
                R3::ConditionalCreate
            } else {
                R3::ConditionalBranchCreate
            };
            let tag = single_root_tag(&branch.children);
            self.creation_code.push(instruction(
                reference,
                vec![num(slot as f64), fn_ref, num(decls as f64), num(vars as f64), tag],
            ));
        }

        // Build the selecting test expression (back-to-front, mirroring `generateConditionalExpressions`).
        // Default: a `@else` (no condition) selects its slot; otherwise `-1` (no branch shown).
        let mut test: Expr = num(-1.0);
        if let Some(default_idx) = block
            .branches
            .iter()
            .position(|b| b.expression.is_none())
        {
            test = num(branch_slots[default_idx] as f64);
        }

        // The whole `@if` chain reserves exactly ONE var slot — the single `ɵɵconditional` binding —
        // regardless of how many branches it has (Angular `varsUsedByOp` for the conditional op is 1).
        if block.branches.iter().any(|b| b.expression.is_some()) {
            self.allocate_binding_slots(1);
        }

        for (idx, branch) in block.branches.iter().enumerate().rev() {
            let Some(cond) = &branch.expression else {
                continue; // default handled above.
            };
            let cond_expr = self.lower_expr(cond);
            let slot_lit = num(branch_slots[idx] as f64);
            test = cond_expr.conditional(slot_lit, Some(test));
        }

        // Update: advance to the conditional anchor, then `ɵɵconditional(<test>)`.
        self.advance_to(anchor_slot);
        self.update_code
            .push(instruction(R3::Conditional, vec![test]));
    }

    /// Lower a `@switch` (`SwitchBlock`) onto the same `ɵɵconditional` mechanism.
    ///
    /// Each `@case`/`@default` group body becomes a nested `ɵɵtemplate` in the creation block. The
    /// update block emits `ɵɵconditional(<test>)` where the test compares the (converted) switch
    /// expression against each case expression via strict equality — `switchExpr === caseExpr ?
    /// slot : …` — with `@default` as the fallback result (mirroring `generateConditionalExpressions`,
    /// whose `tmp === caseExpr` form is the switch lowering).
    fn build_switch_block(&mut self, block: &SwitchBlock) {
        let anchor_slot = self.data_index;
        // Flatten groups -> (case_expression_option, slot) while emitting one template per group.
        struct CaseSlot {
            expression: Option<AstNode>,
            slot: usize,
        }
        let mut cases: Vec<CaseSlot> = Vec::new();

        for (i, group) in block.groups.iter().enumerate() {
            let slot = self.allocate_data_slot();
            let fn_name = format!("{}_Case_{}_Template", self.base_name, slot);
            let (fn_ref, decls, vars) =
                self.build_embedded_view(fn_name, group.children.clone(), Vec::new(), Vec::new());
            // `@switch` shares the conditional-create mechanism: first group -> ɵɵconditionalCreate,
            // the rest -> ɵɵconditionalBranchCreate, each with the case root's element tag (or null).
            let reference = if i == 0 {
                R3::ConditionalCreate
            } else {
                R3::ConditionalBranchCreate
            };
            let tag = single_root_tag(&group.children);
            self.creation_code.push(instruction(
                reference,
                vec![num(slot as f64), fn_ref, num(decls as f64), num(vars as f64), tag],
            ));
            // A group may carry several `@case` labels sharing one body; the first non-default label
            // selects the slot (additional labels collapse to the same body in this port — NOTE(port)).
            let expression = group
                .cases
                .iter()
                .find_map(|c| c.expression.clone());
            cases.push(CaseSlot { expression, slot });
        }

        self.allocate_binding_slots(1);

        // Default result: a `@default` group (no case expression) selects its slot, else `-1`.
        let mut test: Expr = num(-1.0);
        if let Some(default) = cases.iter().find(|c| c.expression.is_none()) {
            test = num(default.slot as f64);
        }

        // The index of the first case carrying a test expression — that comparison's left-hand side
        // is the ASSIGNMENT `(tmp = <disc>)`; all later ones reuse the bare `tmp`.
        let first_test_idx = cases.iter().position(|c| c.expression.is_some());

        // Angular binds the `@switch` discriminant to a TEMPORARY local declared at the top of the
        // update block (`let tmp_<level>_<idx>;`) and assigns it on first use, reusing the temp for
        // every subsequent case comparison — `(tmp = <disc>) === case0 ? 0 : tmp === case1 ? 1 : …`
        // (`createSwitch` + the pipeline temporary-variable allocator). The temp name is
        // `tmp_<viewLevel>_<index>`; declare it as a leading `let` of this view's update block. A
        // switch with no case tests (only `@default`) reads the discriminant zero times, so neither
        // the temp nor the discriminant is materialized.
        let temp_name = first_test_idx.map(|_| {
            let name = format!("tmp_{}_{}", self.view_level, self.temp_counter);
            self.temp_counter += 1;
            self.update_prelude.push(Stmt::bare(StmtKind::DeclareVar {
                name: name.clone(),
                value: None,
                ty: None,
            }));
            name
        });
        // Lower the discriminant only when a case actually compares against it.
        let discriminant = first_test_idx.map(|_| self.lower_expr(&block.expression));

        // Build `tmp === caseExpr ? slot : …` back-to-front; the first (outermost, lowest-index)
        // comparison spills the discriminant into the temp via `(tmp = <disc>)`.
        for (idx, case) in cases.iter().enumerate().rev() {
            let Some(case_expr) = &case.expression else {
                continue;
            };
            // Safe: a case with a test expression exists, so `first_test_idx`, `temp_name` and
            // `discriminant` are all `Some`.
            let temp = temp_name.clone().expect("temp allocated when a case test exists");
            let case_val = self.lower_expr(case_expr);
            let lhs = if Some(idx) == first_test_idx {
                o::variable(temp, None)
                    .set(discriminant.clone().expect("discriminant lowered when a case test exists"))
            } else {
                o::variable(temp, None)
            };
            let cond = lhs.identical(case_val);
            test = cond.conditional(num(case.slot as f64), Some(test));
        }

        self.advance_to(anchor_slot);
        self.update_code
            .push(instruction(R3::Conditional, vec![test]));
    }

    /// Lower an `@for` loop (`ForLoopBlock`) to `ɵɵrepeaterCreate(...)` (creation) + `ɵɵrepeater(<collection>)`
    /// (update).
    ///
    /// Faithful to the classic repeater lowering (`render3/view/template.ts` / pipeline
    /// `ingestForBlock` + `optimizeTrackFns`):
    /// - the loop body becomes a nested embedded-view function (`For_Template`);
    /// - the `@empty` block, when present, becomes a second nested view;
    /// - the `track` expression is optimized: `track $index` → `ɵɵrepeaterTrackByIndex`, `track <item>`
    ///   → `ɵɵrepeaterTrackByIdentity`, otherwise a custom trackBy is a `NOTE(port)` (falls back to the
    ///   identity helper);
    /// - `ɵɵrepeaterCreate(slot, ForFn, decls, vars, tag, attrs?, trackByFn[, false, EmptyFn, emptyDecls,
    ///   emptyVars])`, then `ɵɵrepeater(<collection>)` in update.
    fn build_for_block(&mut self, block: &ForLoopBlock) {
        // Main repeater slot, then a second (hidden) slot the runtime uses internally for the view
        // container — the repeater always allocates 2 slots (the primary view fn is at slot+1, the
        // empty view fn at slot+2, per `naming.ts`).
        let slot = self.allocate_data_slot();
        let _container_slot = self.allocate_data_slot();

        // Collect the loop variables this body references, in Angular's fixed order
        // (item, $index, $count, …), assigning each a globally-unique `_rN` local and a
        // `const <name>_r<n> = ctx.<source>;` declaration to head the body's update block.
        let (loop_vars, prelude) = self.collect_loop_vars(block);

        // Faithful naming: the primary view fn is `<Base>_For_<slot+1>_Template` (`naming.ts`).
        let fn_name = format!("{}_For_{}_Template", self.base_name, slot + 1);
        let (for_fn, decls, vars) =
            self.build_embedded_view(fn_name, block.children.clone(), loop_vars, prelude);

        // The track-by function reference (optimized form).
        let track_ref = optimized_track_ref(block);

        // The item element tag (`'li'`) when the body has a single element root, else null.
        let tag = single_root_tag(&block.children);
        let attrs = o::null_expr();

        let mut params = vec![
            num(slot as f64),
            for_fn,
            num(decls as f64),
            num(vars as f64),
            tag,
            attrs,
            o::import_expr(track_ref.reference(), None),
        ];

        // `@empty { … }` → trailing empty-view args (`false` for trackByUsesComponentInstance, the
        // empty view fn, its decls/vars, and the empty root element tag).
        if let Some(empty) = &block.empty {
            let empty_fn_name = format!("{}_ForEmpty_{}_Template", self.base_name, slot + 2);
            let (empty_fn, empty_decls, empty_vars) =
                self.build_embedded_view(empty_fn_name, empty.children.clone(), Vec::new(), Vec::new());
            params.push(o::literal(o::LiteralValue::Bool(false), None));
            params.push(empty_fn);
            params.push(num(empty_decls as f64));
            params.push(num(empty_vars as f64));
            params.push(single_root_tag(&empty.children));
        }

        self.creation_code
            .push(instruction(R3::RepeaterCreate, params));

        // Update: `ɵɵrepeater(<collection>)`. The collection lowers against this view's scope.
        let collection = self.lower_expr(&block.expression.ast);
        self.advance_to(slot);
        self.update_code
            .push(instruction(R3::Repeater, vec![collection]));
    }

    /// Determine which `@for` loop variables the body references and build their generated locals
    /// (`x_r1`) + `const x_r1 = ctx.$implicit;` declarations, in Angular's fixed order
    /// (item, `$index`, `$count`). Unreferenced variables are skipped (and consume no `_rN` index),
    /// matching the `variable_optimization` phase which drops unused embedded-view variables.
    fn collect_loop_vars(&mut self, block: &ForLoopBlock) -> (Vec<LoopVar>, Vec<Stmt>) {
        // (source name the author writes, ctx property it reads).
        let mut candidates: Vec<(String, String)> =
            vec![(block.item.name.clone(), "$implicit".to_string())];
        for implicit in ["$index", "$count", "$first", "$last", "$even", "$odd"] {
            candidates.push((implicit.to_string(), implicit.to_string()));
        }

        let mut vars = Vec::new();
        let mut prelude = Vec::new();
        for (source_name, ctx_property) in candidates {
            if !for_body_references(&block.children, &source_name) {
                continue;
            }
            self.var_counter += 1;
            let local_name = format!("{}_r{}", source_name, self.var_counter);
            // `const <local> = ctx.<property>;`
            prelude.push(Stmt::with_modifiers(
                StmtKind::DeclareVar {
                    name: local_name.clone(),
                    value: Some(o::variable(CONTEXT_NAME, None).prop(ctx_property.clone())),
                    ty: None,
                },
                StmtModifier::FINAL,
            ));
            vars.push(LoopVar {
                source_name,
                local_name,
            });
        }
        (vars, prelude)
    }
}

impl Visitor for TemplateDefinitionBuilder {
    // Only the node kinds the classic TDB lowers directly are overridden; the rest fall back to
    // the default recursive traversal (a `NOTE(port)` for control-flow/defer/i18n lowering).
    fn visit_text(&mut self, text: &Text) {
        self.build_text(text);
    }

    fn visit_bound_text(&mut self, text: &BoundText) {
        self.build_bound_text(text);
    }

    fn visit_element(&mut self, element: &Element) {
        self.build_element(element);
    }

    fn visit_template(&mut self, template: &Template) {
        self.build_template(template);
    }

    fn visit_if_block(&mut self, block: &IfBlock) {
        self.build_if_block(block);
    }

    fn visit_switch_block(&mut self, block: &SwitchBlock) {
        self.build_switch_block(block);
    }

    fn visit_for_loop_block(&mut self, block: &ForLoopBlock) {
        self.build_for_block(block);
    }
}

/// Choose the optimized `track`-by helper reference for a `@for` block, mirroring
/// `optimizeTrackFns`:
/// - `track $index` (a bare `$index` read) → [`R3::RepeaterTrackByIndex`];
/// - `track <item>` (a bare read of the loop item variable) → [`R3::RepeaterTrackByIdentity`];
/// - anything else → [`R3::RepeaterTrackByIdentity`] as a placeholder. NOTE(port): a custom
///   `trackBy` should lower the expression into a shared `_forTrack` function reference; that
///   const-pool sharing is a follow-up, so we fall back to identity here.
fn optimized_track_ref(block: &ForLoopBlock) -> R3 {
    let Some(track) = &block.track_by else {
        return R3::RepeaterTrackByIdentity;
    };
    // Resolve a bare implicit-receiver property read name (`$index` / item var), if any.
    if let Some(name) = bare_read_name(&track.ast) {
        if name == "$index" {
            return R3::RepeaterTrackByIndex;
        }
        if name == block.item.name {
            return R3::RepeaterTrackByIdentity;
        }
    }
    // NOTE(port): custom trackBy function not yet lowered to a shared reference.
    R3::RepeaterTrackByIdentity
}

/// Whether an event-handler expression AST references `$event`.
///
/// Mirrors Angular's `resolveDollarEvent` (`phases/resolve_dollar_event.ts`): a listener handler
/// only consumes `$event` (and therefore only declares the `$event` parameter) when a `$event`
/// lexical read appears anywhere in the handler. Here a `$event` reference is an implicit/`this`
/// `PropertyRead`/`SafePropertyRead` named `$event` (e.g. `f($event)` → `ctx.$event` after
/// lowering). The walk recurses through every sub-expression so nested uses (`g(h($event))`,
/// `cond ? $event : 0`, chains, …) are all detected.
fn handler_references_dollar_event(node: &AstNode) -> bool {
    use AstExprKind as EK;

    // A bare implicit/this read named `$event` is the reference we look for.
    if let EK::PropertyRead { receiver, name, .. } | EK::SafePropertyRead { receiver, name, .. } =
        &node.kind
    {
        if name == EVENT_NAME
            && matches!(
                receiver.kind,
                EK::ImplicitReceiver | EK::ThisReceiver
            )
        {
            return true;
        }
    }

    // Otherwise recurse into every child expression.
    match &node.kind {
        EK::EmptyExpr
        | EK::ImplicitReceiver
        | EK::ThisReceiver
        | EK::LiteralPrimitive { .. }
        | EK::TemplateLiteralElement { .. }
        | EK::RegularExpressionLiteral { .. } => false,
        EK::Chain { expressions }
        | EK::LiteralArray { expressions }
        | EK::Interpolation { expressions, .. } => {
            expressions.iter().any(handler_references_dollar_event)
        }
        EK::Conditional {
            condition,
            true_exp,
            false_exp,
        } => {
            handler_references_dollar_event(condition)
                || handler_references_dollar_event(true_exp)
                || handler_references_dollar_event(false_exp)
        }
        EK::PropertyRead { receiver, .. } | EK::SafePropertyRead { receiver, .. } => {
            handler_references_dollar_event(receiver)
        }
        EK::KeyedRead { receiver, key } | EK::SafeKeyedRead { receiver, key } => {
            handler_references_dollar_event(receiver) || handler_references_dollar_event(key)
        }
        EK::BindingPipe { exp, args, .. } => {
            handler_references_dollar_event(exp)
                || args.iter().any(handler_references_dollar_event)
        }
        EK::SpreadElement { expression }
        | EK::PrefixNot { expression }
        | EK::TypeofExpression { expression }
        | EK::VoidExpression { expression }
        | EK::NonNullAssert { expression }
        | EK::ParenthesizedExpression { expression } => {
            handler_references_dollar_event(expression)
        }
        EK::LiteralMap { values, .. } => values.iter().any(handler_references_dollar_event),
        EK::Binary { left, right, .. } => {
            handler_references_dollar_event(left) || handler_references_dollar_event(right)
        }
        EK::Unary { expr, .. } => handler_references_dollar_event(expr),
        EK::Call {
            receiver, args, ..
        }
        | EK::SafeCall {
            receiver, args, ..
        } => {
            handler_references_dollar_event(receiver)
                || args.iter().any(handler_references_dollar_event)
        }
        EK::TaggedTemplateLiteral { tag, template } => {
            handler_references_dollar_event(tag) || handler_references_dollar_event(template)
        }
        EK::TemplateLiteral { expressions, .. } => {
            expressions.iter().any(handler_references_dollar_event)
        }
        EK::ArrowFunction { body, .. } => handler_references_dollar_event(body),
    }
}

/// If `node` is a bare property read off the implicit receiver (`x`, `$index`), return its name.
fn bare_read_name(node: &AstNode) -> Option<&str> {
    if let AstExprKind::PropertyRead { receiver, name, .. } = &node.kind {
        if matches!(receiver.kind, AstExprKind::ImplicitReceiver) {
            return Some(name);
        }
    }
    None
}

/// The element tag name to pass as the trailing `ɵɵconditionalCreate`/`ɵɵconditionalBranchCreate`/
/// `ɵɵrepeaterCreate` argument: the single root element/template tag of a control-flow block body,
/// or `null` when the body has zero or multiple non-trivial roots, or a root that is not an element
/// (a single text node, etc.). Mirrors `ingestControlFlowInsertionPoint` (`ingest.ts`): comment /
/// `@let` nodes are skipped; an `ng-template` tag is not passed (it would enable directive matching).
fn single_root_tag(children: &[Node]) -> Expr {
    let mut root: Option<&str> = None;
    for child in children {
        match child {
            // Skipped: they don't anchor a DOM insertion point.
            Node::LetDeclaration(_) => continue,
            Node::Element(el) => {
                if root.is_some() {
                    return o::null_expr();
                }
                root = Some(&el.name);
            }
            Node::Template(tmpl) => {
                let Some(tag) = tmpl.tag_name.as_deref() else {
                    return o::null_expr();
                };
                if root.is_some() {
                    return o::null_expr();
                }
                root = Some(tag);
            }
            // Any other node kind (text, bound text, another block, content, …) means the body has
            // no single element root.
            _ => return o::null_expr(),
        }
    }
    match root {
        Some(tag) if tag != "ng-template" => str_lit(tag),
        _ => o::null_expr(),
    }
}

/// The `UPDATE_ORDERING` group rank of a bound input (`phases/ordering.ts`, non-host order). Update
/// binding instructions for a single element are stable-sorted by this rank so style props precede
/// class props precede (non-interpolation) properties precede attributes — matching Angular exactly.
/// Interpolated `Attribute`/`Property` bindings sort into the earlier interpolation groups.
fn update_order_rank(input: &BoundAttribute) -> u8 {
    use crate::expression::ast::BindingType;
    let is_interp = matches!(input.value.kind, AstExprKind::Interpolation { .. });
    match input.kind {
        BindingType::Style => 2,
        BindingType::Class => 3,
        BindingType::Attribute => {
            if is_interp {
                4
            } else {
                7
            }
        }
        // Property / TwoWay (and the animation fall-throughs, which lower as DOM properties):
        // interpolation → group 5, non-interpolation → group 6.
        _ => {
            if is_interp {
                5
            } else {
                6
            }
        }
    }
}

/// Whether a `@for` body references the implicit-receiver variable named `name` (the loop item or a
/// `$`-prefixed magic var). Drives `variable_optimization`-style elision of unused loop locals.
/// Walks every binding expression in the body (bound text, inputs, outputs) and recurses into
/// nested control-flow blocks, since a nested block's bindings still resolve against this scope.
fn for_body_references(children: &[Node], name: &str) -> bool {
    children.iter().any(|node| node_references(node, name))
}

fn node_references(node: &Node, name: &str) -> bool {
    match node {
        Node::BoundText(bt) => expr_references_implicit(&bt.value, name),
        Node::Element(el) => {
            el.inputs.iter().any(|i| expr_references_implicit(&i.value, name))
                || el.outputs.iter().any(|o| expr_references_implicit(&o.handler, name))
                || for_body_references(&el.children, name)
        }
        Node::Template(t) => {
            t.inputs.iter().any(|i| expr_references_implicit(&i.value, name))
                || t.outputs.iter().any(|o| expr_references_implicit(&o.handler, name))
                || for_body_references(&t.children, name)
        }
        Node::IfBlock(b) => b.branches.iter().any(|br| {
            br.expression
                .as_ref()
                .is_some_and(|e| expr_references_implicit(e, name))
                || for_body_references(&br.children, name)
        }),
        Node::SwitchBlock(b) => {
            expr_references_implicit(&b.expression, name)
                || b.groups.iter().any(|g| {
                    g.cases.iter().any(|c| {
                        c.expression
                            .as_ref()
                            .is_some_and(|e| expr_references_implicit(e, name))
                    }) || for_body_references(&g.children, name)
                })
        }
        Node::ForLoopBlock(b) => {
            // A nested `@for` re-binds its own item; references to *this* `name` still resolve to the
            // outer scope through the inner view's collection/track expressions and body.
            expr_references_implicit(&b.expression.ast, name)
                || b.track_by
                    .as_ref()
                    .is_some_and(|t| expr_references_implicit(&t.ast, name))
                || for_body_references(&b.children, name)
                || b.empty
                    .as_ref()
                    .is_some_and(|e| for_body_references(&e.children, name))
        }
        _ => false,
    }
}

/// Whether an expression AST contains a read of `name` off the implicit/this receiver (`x`,
/// `$index`), recursing through every sub-expression.
fn expr_references_implicit(node: &AstNode, name: &str) -> bool {
    use AstExprKind as EK;

    if let EK::PropertyRead { receiver, name: n, .. } | EK::SafePropertyRead { receiver, name: n, .. } =
        &node.kind
    {
        if n == name && matches!(receiver.kind, EK::ImplicitReceiver | EK::ThisReceiver) {
            return true;
        }
    }

    match &node.kind {
        EK::EmptyExpr
        | EK::ImplicitReceiver
        | EK::ThisReceiver
        | EK::LiteralPrimitive { .. }
        | EK::TemplateLiteralElement { .. }
        | EK::RegularExpressionLiteral { .. } => false,
        EK::Chain { expressions }
        | EK::LiteralArray { expressions }
        | EK::Interpolation { expressions, .. } => {
            expressions.iter().any(|e| expr_references_implicit(e, name))
        }
        EK::Conditional {
            condition,
            true_exp,
            false_exp,
        } => {
            expr_references_implicit(condition, name)
                || expr_references_implicit(true_exp, name)
                || expr_references_implicit(false_exp, name)
        }
        EK::PropertyRead { receiver, .. } | EK::SafePropertyRead { receiver, .. } => {
            expr_references_implicit(receiver, name)
        }
        EK::KeyedRead { receiver, key } | EK::SafeKeyedRead { receiver, key } => {
            expr_references_implicit(receiver, name) || expr_references_implicit(key, name)
        }
        EK::BindingPipe { exp, args, .. } => {
            expr_references_implicit(exp, name) || args.iter().any(|a| expr_references_implicit(a, name))
        }
        EK::SpreadElement { expression }
        | EK::PrefixNot { expression }
        | EK::TypeofExpression { expression }
        | EK::VoidExpression { expression }
        | EK::NonNullAssert { expression }
        | EK::ParenthesizedExpression { expression } => expr_references_implicit(expression, name),
        EK::LiteralMap { values, .. } => values.iter().any(|v| expr_references_implicit(v, name)),
        EK::Binary { left, right, .. } => {
            expr_references_implicit(left, name) || expr_references_implicit(right, name)
        }
        EK::Unary { expr, .. } => expr_references_implicit(expr, name),
        EK::Call { receiver, args, .. } | EK::SafeCall { receiver, args, .. } => {
            expr_references_implicit(receiver, name)
                || args.iter().any(|a| expr_references_implicit(a, name))
        }
        EK::TaggedTemplateLiteral { tag, template } => {
            expr_references_implicit(tag, name) || expr_references_implicit(template, name)
        }
        EK::TemplateLiteral { expressions, .. } => {
            expressions.iter().any(|e| expr_references_implicit(e, name))
        }
        EK::ArrowFunction { body, .. } => expr_references_implicit(body, name),
    }
}

// ---------------------------------------------------------------------------
// Text-interpolation arity selection (`ɵɵtextInterpolate{N}` / `…V`).
// ---------------------------------------------------------------------------

/// Choose the arity-specialized `ɵɵtextInterpolate{N}` instruction and build its argument list
/// from a `BoundText` expression AST.
///
/// Faithful to Angular's pipeline `callVariadicInstructionExpr(TEXT_INTERPOLATE_CONFIG, …)` +
/// `collateInterpolationArgs` (`template/pipeline/src/instruction.ts`):
/// - `BoundText.value` is normally an [`AstExprKind::Interpolation`] (`strings = [s0..sN]`,
///   `expressions = [e0..eN-1]`).
/// - args are collated as `[s0, e0, s1, e1, …, sN]`, except a single expression with empty
///   surrounding strings collapses to just `[e0]`.
/// - a trailing empty-string arg is dropped when there is more than one arg.
/// - the instruction is `textInterpolate{count}` for `count` expressions (0 ⇒ the bare
///   `textInterpolate`), or `textInterpolateV([…])` for >8.
///
/// A bare (non-interpolation) expression is treated as a single `{{ expr }}` with empty affixes,
/// i.e. `textInterpolate(expr)`.
fn text_interpolation_call(value: &AstNode, lower: &dyn Fn(&AstNode) -> Expr) -> (R3, Vec<Expr>) {
    // Collate interpolation args (mirrors `collateInterpolationArgs`).
    let mut args: Vec<Expr> = match &value.kind {
        AstExprKind::Interpolation { strings, expressions }
            if expressions.len() == 1
                && strings.len() == 2
                && strings[0].is_empty()
                && strings[1].is_empty() =>
        {
            vec![lower(&expressions[0])]
        }
        AstExprKind::Interpolation { strings, expressions } => {
            let mut out = Vec::with_capacity(strings.len() + expressions.len());
            for (idx, expr) in expressions.iter().enumerate() {
                out.push(str_lit(strings.get(idx).map(String::as_str).unwrap_or("")));
                out.push(lower(expr));
            }
            out.push(str_lit(strings.last().map(String::as_str).unwrap_or("")));
            out
        }
        // Bare expression (not a desugared interpolation node) ⇒ treat as a single `{{ expr }}`
        // with explicit empty affixes (`textInterpolate1("", expr, "")`). This is the shape the
        // direct-AST unit tests construct.
        _ => {
            return (
                R3::TextInterpolate1,
                vec![str_lit(""), lower(value), str_lit("")],
            );
        }
    };

    // `mapping(n) = (n - 1) / 2` for the odd-length collated form; the single-collapsed form has
    // length 1 ⇒ count 0.
    let count = if args.len() == 1 { 0 } else { (args.len() - 1) / 2 };

    // Drop a trailing empty-string arg (the runtime fills it in) when more than one arg remains.
    if args.len() > 1 {
        if let Some(last) = args.last() {
            if matches!(
                &last.kind,
                o::ExprKind::Literal(o::LiteralValue::String(s)) if s.is_empty()
            ) {
                args.pop();
            }
        }
    }

    match count {
        0 => (R3::TextInterpolate, args),
        1 => (R3::TextInterpolate1, args),
        2 => (R3::TextInterpolate2, args),
        3 => (R3::TextInterpolate3, args),
        4 => (R3::TextInterpolate4, args),
        5 => (R3::TextInterpolate5, args),
        6 => (R3::TextInterpolate6, args),
        7 => (R3::TextInterpolate7, args),
        8 => (R3::TextInterpolate8, args),
        // `textInterpolateV([s0, e0, s1, …, sN])` — variadic for >8 expressions.
        _ => (R3::TextInterpolateV, vec![o::literal_arr(args, None)]),
    }
}

/// The number of binding (var) slots an interpolation reserves: the count of its dynamic
/// expressions. A non-interpolation (bare) expression counts as a single expression. Mirrors the
/// v22 `varsUsedByOp` accounting (`op.interpolation.expressions.length`).
fn interpolation_expression_count(value: &AstNode) -> usize {
    match &value.kind {
        AstExprKind::Interpolation { expressions, .. } => expressions.len(),
        _ => 1,
    }
}

// ---------------------------------------------------------------------------
// Pipe-slot finalisation: rewrite placeholder slot literals to real slots.
// ---------------------------------------------------------------------------

/// Rewrite every placeholder pipe-slot literal (`PIPE_SLOT_PLACEHOLDER + ordinal`) reachable from
/// `stmt` to its real data slot (`pipe_base + ordinal`). Covers every [`StmtKind`] so a pipe binding
/// is patched wherever it sits (it only appears in update-block expression statements in practice).
fn remap_pipe_slots_in_stmt(stmt: &mut Stmt, pipe_base: usize) {
    match &mut stmt.kind {
        StmtKind::DeclareVar { value, .. } => {
            if let Some(v) = value {
                remap_pipe_slots_in_expr(v, pipe_base);
            }
        }
        StmtKind::DeclareFunction { statements, .. } => {
            for s in statements {
                remap_pipe_slots_in_stmt(s, pipe_base);
            }
        }
        StmtKind::Expression(e) | StmtKind::Return(e) => {
            remap_pipe_slots_in_expr(e, pipe_base);
        }
        StmtKind::If {
            condition,
            true_case,
            false_case,
        } => {
            remap_pipe_slots_in_expr(condition, pipe_base);
            for s in true_case {
                remap_pipe_slots_in_stmt(s, pipe_base);
            }
            for s in false_case {
                remap_pipe_slots_in_stmt(s, pipe_base);
            }
        }
    }
}

/// Recursively rewrite placeholder pipe-slot literals in `expr`. A pipe's data slot is emitted as
/// `PIPE_SLOT_PLACEHOLDER + ordinal` during the walk (the real slot is unknown until every other data
/// slot is allocated); here we map it to `pipe_base + ordinal`. The sentinel base is far above any
/// real slot count, so the `>= PIPE_SLOT_PLACEHOLDER` test never matches a genuine slot/index literal.
fn remap_pipe_slots_in_expr(expr: &mut Expr, pipe_base: usize) {
    use o::ExprKind as K;
    match &mut expr.kind {
        K::Literal(o::LiteralValue::Number(n)) => {
            let v = *n as usize;
            if *n >= PIPE_SLOT_PLACEHOLDER as f64 && v >= PIPE_SLOT_PLACEHOLDER {
                let ordinal = v - PIPE_SLOT_PLACEHOLDER;
                *n = (pipe_base + ordinal) as f64;
            }
        }
        // Leaves with no child expressions.
        K::ReadVar { .. }
        | K::WrappedNode(_)
        | K::RegExpLiteral { .. }
        | K::Literal(_)
        | K::TemplateLiteralElement(_)
        | K::External { .. } => {}
        K::Typeof(e) | K::Void(e) | K::Not(e) | K::Parenthesized(e) | K::Spread(e) => {
            remap_pipe_slots_in_expr(e, pipe_base);
        }
        K::Invoke { callee, args, .. } => {
            remap_pipe_slots_in_expr(callee, pipe_base);
            for a in args {
                remap_pipe_slots_in_expr(a, pipe_base);
            }
        }
        K::TaggedTemplate { tag, template } => {
            remap_pipe_slots_in_expr(tag, pipe_base);
            remap_pipe_slots_in_expr(template, pipe_base);
        }
        K::New { class_expr, args } => {
            remap_pipe_slots_in_expr(class_expr, pipe_base);
            for a in args {
                remap_pipe_slots_in_expr(a, pipe_base);
            }
        }
        K::TemplateLiteral { expressions, .. } | K::LocalizedString { expressions, .. } => {
            for e in expressions {
                remap_pipe_slots_in_expr(e, pipe_base);
            }
        }
        K::Conditional {
            condition,
            true_case,
            false_case,
        } => {
            remap_pipe_slots_in_expr(condition, pipe_base);
            remap_pipe_slots_in_expr(true_case, pipe_base);
            if let Some(f) = false_case {
                remap_pipe_slots_in_expr(f, pipe_base);
            }
        }
        K::DynamicImport { url, .. } => {
            if let o::ImportUrl::Expr(e) = url {
                remap_pipe_slots_in_expr(e, pipe_base);
            }
        }
        K::Function { statements, .. } => {
            for s in statements {
                remap_pipe_slots_in_stmt(s, pipe_base);
            }
        }
        K::Arrow { body, .. } => match body {
            o::ArrowBody::Expr(e) => remap_pipe_slots_in_expr(e, pipe_base),
            o::ArrowBody::Block(stmts) => {
                for s in stmts {
                    remap_pipe_slots_in_stmt(s, pipe_base);
                }
            }
        },
        K::Unary { expr: inner, .. } => remap_pipe_slots_in_expr(inner, pipe_base),
        K::Binary { lhs, rhs, .. } => {
            remap_pipe_slots_in_expr(lhs, pipe_base);
            remap_pipe_slots_in_expr(rhs, pipe_base);
        }
        K::ReadProp { receiver, .. } => remap_pipe_slots_in_expr(receiver, pipe_base),
        K::ReadKey { receiver, index, .. } => {
            remap_pipe_slots_in_expr(receiver, pipe_base);
            remap_pipe_slots_in_expr(index, pipe_base);
        }
        K::LiteralArray(entries) | K::Comma(entries) => {
            for e in entries {
                remap_pipe_slots_in_expr(e, pipe_base);
            }
        }
        K::LiteralMap { entries, .. } => {
            for entry in entries {
                match entry {
                    o::LiteralMapEntry::Property { value, .. } => {
                        remap_pipe_slots_in_expr(value, pipe_base)
                    }
                    o::LiteralMapEntry::Spread { expression } => {
                        remap_pipe_slots_in_expr(expression, pipe_base)
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expression::ast::{
        AbsoluteSourceSpan, AstNode, ExprKind as AstExprKind, ParseSourceSpan, ParseSpan,
    };
    use crate::output::emitter::emit_expression;
    use crate::template::r3_ast::{BoundText, Element, Node, Text};

    fn t_span() -> ParseSourceSpan {
        ParseSourceSpan { start: 0, end: 0 }
    }

    /// `{{ x }}` — implicit-receiver property read of `x`.
    fn prop_read_x() -> AstNode {
        let implicit = AstNode::new(
            ParseSpan::new(0, 0),
            AbsoluteSourceSpan::new(0, 0),
            AstExprKind::ImplicitReceiver,
        );
        AstNode::new(
            ParseSpan::new(0, 0),
            AbsoluteSourceSpan::new(0, 0),
            AstExprKind::PropertyRead {
                name_span: AbsoluteSourceSpan::new(0, 0),
                receiver: Box::new(implicit),
                name: "x".to_string(),
            },
        )
    }

    /// Build `<div>{{x}}</div>` as the root t-AST.
    fn div_with_interpolation() -> Vec<Node> {
        vec![Node::Element(Element {
            name: "div".to_string(),
            attributes: vec![],
            inputs: vec![],
            outputs: vec![],
            directives: vec![],
            children: vec![Node::BoundText(BoundText {
                value: prop_read_x(),
                source_span: t_span(),
                i18n: None,
            })],
            references: vec![],
            is_self_closing: false,
            source_span: t_span(),
            start_source_span: t_span(),
            end_source_span: None,
            is_void: false,
            i18n: None,
        })]
    }

    #[test]
    fn builds_div_with_interpolation() {
        let input = TemplateCompilationInput::new("Test_Template", div_with_interpolation());
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let func = builder.build_template_function(&input);
        let out = emit_expression(&func);

        // Creation: domElementStart/domElementEnd (div has a child) + text placeholder.
        assert!(out.contains("\u{0275}\u{0275}domElementStart"), "got: {out}");
        assert!(out.contains("\u{0275}\u{0275}domElementEnd"), "got: {out}");
        assert!(out.contains("\u{0275}\u{0275}text"), "got: {out}");
        // Update: textInterpolate1 + an advance to the text slot.
        assert!(out.contains("\u{0275}\u{0275}textInterpolate1"), "got: {out}");
        assert!(out.contains("\u{0275}\u{0275}advance"), "got: {out}");
        // The bound expression resolves against `ctx`.
        assert!(out.contains("ctx.x"), "got: {out}");
        // The render-flags branching shape.
        assert!(out.contains("rf"), "got: {out}");
    }

    /// `{{ x | name:args }}` as a `BoundText` interpolation node (strings `["",""]`, one expression
    /// that is the `BindingPipe`). `args` are numeric pipe arguments.
    fn interpolation_with_pipe(name: &str, args: Vec<f64>) -> Vec<Node> {
        use crate::expression::ast::BindingPipeType;
        let ab = || AbsoluteSourceSpan::new(0, 0);
        let sp = || ParseSpan::new(0, 0);
        let arg_nodes: Vec<AstNode> = args
            .into_iter()
            .map(|n| {
                AstNode::new(
                    sp(),
                    ab(),
                    AstExprKind::LiteralPrimitive {
                        value: crate::expression::ast::LiteralValue::Num(n),
                    },
                )
            })
            .collect();
        let pipe = AstNode::new(
            sp(),
            ab(),
            AstExprKind::BindingPipe {
                name_span: ab(),
                exp: Box::new(prop_read_x()),
                name: name.to_string(),
                args: arg_nodes,
                pipe_type: BindingPipeType::ReferencedByName,
            },
        );
        let interp = AstNode::new(
            sp(),
            ab(),
            AstExprKind::Interpolation {
                strings: vec![String::new(), String::new()],
                expressions: vec![pipe],
            },
        );
        vec![Node::Element(Element {
            name: "div".to_string(),
            attributes: vec![],
            inputs: vec![],
            outputs: vec![],
            directives: vec![],
            children: vec![Node::BoundText(BoundText {
                value: interp,
                source_span: t_span(),
                i18n: None,
            })],
            references: vec![],
            is_self_closing: false,
            source_span: t_span(),
            start_source_span: t_span(),
            end_source_span: None,
            is_void: false,
            i18n: None,
        })]
    }

    #[test]
    fn interpolation_pipe_emits_pipe_creation_and_pipe_bind1() {
        // `<div>{{ x | uppercase }}</div>`:
        //   creation: ɵɵpipe(N, "uppercase") after the text/element slots
        //   update:   ɵɵpipeBind1(N, varOffset, ctx.x)
        let input =
            TemplateCompilationInput::new("Test_Template", interpolation_with_pipe("uppercase", vec![]));
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let func = builder.build_template_function(&input);
        let out = emit_expression(&func);

        // The pipe creation instruction with the pipe name.
        assert!(out.contains("\u{0275}\u{0275}pipe("), "missing ɵɵpipe, got: {out}");
        assert!(out.contains("\"uppercase\""), "missing pipe name, got: {out}");
        // The update-block pipeBind1 with the piped value.
        assert!(out.contains("\u{0275}\u{0275}pipeBind1("), "missing ɵɵpipeBind1, got: {out}");
        assert!(out.contains("ctx.x"), "missing piped value, got: {out}");
        // No leftover placeholder slot literal.
        assert!(!out.contains("1000000000"), "placeholder slot not patched, got: {out}");

        // Slot accounting: div (0), text (1), pipe (2). One pipe → +1 decl.
        assert_eq!(builder.data_index(), 3, "expected element+text+pipe data slots");
        // The pipe creation slot is the last data slot (2).
        assert!(
            out.contains("\u{0275}\u{0275}pipe(2"),
            "pipe slot should be at the end of the data array, got: {out}"
        );
        // Var slots: text interpolation (1) + pipe (1 + 1 arg = 2) = 3.
        assert_eq!(builder.vars(), 3, "expected interpolation + pipe var slots");
    }

    #[test]
    fn interpolation_pipe_with_two_args_emits_pipe_bind3() {
        // `<div>{{ x | slice:1:3 }}</div>` → ɵɵpipeBind3(N, varOffset, ctx.x, 1, 3).
        let input = TemplateCompilationInput::new(
            "Test_Template",
            interpolation_with_pipe("slice", vec![1.0, 3.0]),
        );
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let func = builder.build_template_function(&input);
        let out = emit_expression(&func);

        assert!(out.contains("\u{0275}\u{0275}pipe("), "missing ɵɵpipe, got: {out}");
        assert!(out.contains("\"slice\""), "missing pipe name, got: {out}");
        assert!(out.contains("\u{0275}\u{0275}pipeBind3("), "missing ɵɵpipeBind3, got: {out}");
        assert!(out.contains("ctx.x"), "missing piped value, got: {out}");
        assert!(!out.contains("1000000000"), "placeholder slot not patched, got: {out}");

        // Var slots: interpolation (1) + pipe (1 + 3 args = 4) = 5.
        assert_eq!(builder.vars(), 5, "expected interpolation + pipe var slots");
    }

    #[test]
    fn childless_element_collapses_to_element() {
        // `<span></span>` (no children) ⇒ a single `ɵɵdomElement`.
        let nodes = vec![Node::Element(Element {
            name: "span".to_string(),
            attributes: vec![],
            inputs: vec![],
            outputs: vec![],
            directives: vec![],
            children: vec![],
            references: vec![],
            is_self_closing: false,
            source_span: t_span(),
            start_source_span: t_span(),
            end_source_span: None,
            is_void: false,
            i18n: None,
        })];
        let input = TemplateCompilationInput::new("Test_Template", nodes);
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let func = builder.build_template_function(&input);
        let out = emit_expression(&func);

        assert!(out.contains("\u{0275}\u{0275}domElement("), "got: {out}");
        assert!(!out.contains("\u{0275}\u{0275}domElementStart"), "got: {out}");
        // No bindings ⇒ no update block.
        assert!(!out.contains("\u{0275}\u{0275}advance"), "got: {out}");
    }

    #[test]
    fn nested_elements_chain_dom_element_start() {
        // `<div><span>x</span></div>` ⇒ the two consecutive `ɵɵdomElementStart` create-mode calls
        // (div at slot 0, span at slot 1 — both childed, so neither collapses to `ɵɵdomElement`)
        // fold into one chained statement (`ɵɵdomElementStart(0, "div")(1, "span")`), matching
        // Angular `chainedInstruction`. The two `ɵɵdomElementEnd()` calls chain the same way.
        let span = Node::Element(Element {
            name: "span".to_string(),
            attributes: vec![],
            inputs: vec![],
            outputs: vec![],
            directives: vec![],
            children: vec![Node::Text(Text {
                value: "x".to_string(),
                source_span: t_span(),
            })],
            references: vec![],
            is_self_closing: false,
            source_span: t_span(),
            start_source_span: t_span(),
            end_source_span: None,
            is_void: false,
            i18n: None,
        });
        let nodes = vec![Node::Element(Element {
            name: "div".to_string(),
            attributes: vec![],
            inputs: vec![],
            outputs: vec![],
            directives: vec![],
            children: vec![span],
            references: vec![],
            is_self_closing: false,
            source_span: t_span(),
            start_source_span: t_span(),
            end_source_span: None,
            is_void: false,
            i18n: None,
        })];
        let input = TemplateCompilationInput::new("Test_Template", nodes);
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let func = builder.build_template_function(&input);
        let out = emit_expression(&func);

        // Both `domElementStart` calls fold into a single chained statement: the instruction
        // name appears exactly once, followed by the chained `(0, "div")(1, "span")` form.
        assert_eq!(
            out.matches("\u{0275}\u{0275}domElementStart").count(),
            1,
            "got: {out}"
        );
        assert!(
            out.contains("\u{0275}\u{0275}domElementStart(0, \"div\")(1, \"span\")"),
            "got: {out}"
        );
        // The two `domElementEnd` calls also chain into one statement.
        assert_eq!(
            out.matches("\u{0275}\u{0275}domElementEnd").count(),
            1,
            "got: {out}"
        );
    }

    #[test]
    fn static_text_node() {
        let nodes = vec![Node::Text(Text {
            value: "hello".to_string(),
            source_span: t_span(),
        })];
        let input = TemplateCompilationInput::new("Test_Template", nodes);
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let func = builder.build_template_function(&input);
        let out = emit_expression(&func);
        assert!(out.contains("\u{0275}\u{0275}text"), "got: {out}");
        assert!(out.contains("\"hello\""), "got: {out}");
    }

    #[test]
    fn element_with_static_attrs_interns_const() {
        let nodes = vec![Node::Element(Element {
            name: "div".to_string(),
            attributes: vec![TextAttribute {
                name: "id".to_string(),
                value: "a".to_string(),
                source_span: t_span(),
                key_span: None,
                value_span: None,
                i18n: None,
            }],
            inputs: vec![],
            outputs: vec![],
            directives: vec![],
            children: vec![],
            references: vec![],
            is_self_closing: false,
            source_span: t_span(),
            start_source_span: t_span(),
            end_source_span: None,
            is_void: false,
            i18n: None,
        })];
        let input = TemplateCompilationInput::new("Test_Template", nodes);
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let _ = builder.build_template_function(&input);
        // One attrs array was interned; `ɵɵelement(0, "div", 0)` references it.
        assert_eq!(builder.const_pool().entries().len(), 1);
        let consts = builder.const_pool().to_const_array().unwrap();
        let out = emit_expression(&consts);
        assert!(out.contains("\"id\""), "got: {out}");
        assert!(out.contains("\"a\""), "got: {out}");
    }

    // -- Binding-instruction tests (Task D). --

    use crate::expression::ast::{
        BindingType, ParsedEventType, SecurityContext,
    };
    use crate::template::r3_ast::{BoundAttribute, BoundEvent};

    /// `{{ name }}` — implicit-receiver property read of `name`.
    fn prop_read(name: &str) -> AstNode {
        let implicit = AstNode::new(
            ParseSpan::new(0, 0),
            AbsoluteSourceSpan::new(0, 0),
            AstExprKind::ImplicitReceiver,
        );
        AstNode::new(
            ParseSpan::new(0, 0),
            AbsoluteSourceSpan::new(0, 0),
            AstExprKind::PropertyRead {
                name_span: AbsoluteSourceSpan::new(0, 0),
                receiver: Box::new(implicit),
                name: name.to_string(),
            },
        )
    }

    /// `f()` — a call of implicit-receiver `f` with no args.
    fn call_f() -> AstNode {
        AstNode::new(
            ParseSpan::new(0, 0),
            AbsoluteSourceSpan::new(0, 0),
            AstExprKind::Call {
                receiver: Box::new(prop_read("f")),
                args: vec![],
                argument_span: AbsoluteSourceSpan::new(0, 0),
            },
        )
    }

    /// `g($event)` — a call of implicit-receiver `g` passing the implicit `$event` read.
    fn call_g_with_event() -> AstNode {
        AstNode::new(
            ParseSpan::new(0, 0),
            AbsoluteSourceSpan::new(0, 0),
            AstExprKind::Call {
                receiver: Box::new(prop_read("g")),
                args: vec![prop_read("$event")],
                argument_span: AbsoluteSourceSpan::new(0, 0),
            },
        )
    }

    /// A two-part interpolation `{{ a }} {{ b }}` (strings `["", " ", ""]`).
    fn interpolation_ab() -> AstNode {
        AstNode::new(
            ParseSpan::new(0, 0),
            AbsoluteSourceSpan::new(0, 0),
            AstExprKind::Interpolation {
                strings: vec![String::new(), " ".to_string(), String::new()],
                expressions: vec![prop_read("a"), prop_read("b")],
            },
        )
    }

    #[test]
    fn property_binding_emits_property_instruction() {
        // `<div [id]="x"></div>` ⇒ update-block `ɵɵdomProperty("id", ctx.x)`, vars >= 1, and a
        // `[3, "id"]` binding const referenced by `ɵɵdomElement(0, "div", 0)`.
        let nodes = vec![Node::Element(Element {
            name: "div".to_string(),
            attributes: vec![],
            inputs: vec![BoundAttribute {
                name: "id".to_string(),
                kind: BindingType::Property,
                security_context: SecurityContext::None,
                value: prop_read("x"),
                unit: None,
                source_span: t_span(),
                key_span: t_span(),
                value_span: None,
                i18n: None,
            }],
            outputs: vec![],
            directives: vec![],
            children: vec![],
            references: vec![],
            is_self_closing: false,
            source_span: t_span(),
            start_source_span: t_span(),
            end_source_span: None,
            is_void: false,
            i18n: None,
        })];
        let input = TemplateCompilationInput::new("Test_Template", nodes);
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let func = builder.build_template_function(&input);
        let out = emit_expression(&func);

        // DOM-only property binding → `ɵɵdomProperty` (not `ɵɵproperty`).
        assert!(out.contains("\u{0275}\u{0275}domProperty"), "got: {out}");
        assert!(!out.contains("\u{0275}\u{0275}property("), "got: {out}");
        assert!(out.contains("\"id\""), "got: {out}");
        assert!(out.contains("ctx.x"), "got: {out}");
        // The element references the binding-const slot: `ɵɵdomElement(0, "div", 0)`.
        assert!(out.contains("\u{0275}\u{0275}domElement(0, \"div\", 0)"), "got: {out}");
        // One property binding ⇒ one var slot.
        assert_eq!(builder.vars(), 1, "vars = {}", builder.vars());
        // A `[3, "id"]` binding const (AttributeMarker.Bindings = 3) was interned.
        assert_eq!(builder.const_pool().entries().len(), 1);
        let consts = emit_expression(&builder.const_pool().to_const_array().unwrap());
        assert!(consts.contains("3"), "got: {consts}");
        assert!(consts.contains("\"id\""), "got: {consts}");
    }

    #[test]
    fn event_binding_emits_listener_instruction() {
        // `<button (click)="f()"></button>` ⇒ creation-block `ɵɵdomListener("click", function
        // Test_Template_button_click_0_listener($event) { … })`, plus a `[3, "click"]` binding const.
        let nodes = vec![Node::Element(Element {
            name: "button".to_string(),
            attributes: vec![],
            inputs: vec![],
            outputs: vec![BoundEvent {
                name: "click".to_string(),
                kind: ParsedEventType::Regular,
                handler: call_f(),
                target: None,
                phase: None,
                source_span: t_span(),
                handler_span: t_span(),
                key_span: t_span(),
            }],
            directives: vec![],
            children: vec![],
            references: vec![],
            is_self_closing: false,
            source_span: t_span(),
            start_source_span: t_span(),
            end_source_span: None,
            is_void: false,
            i18n: None,
        })];
        let input = TemplateCompilationInput::new("Test_Template", nodes);
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let func = builder.build_template_function(&input);
        let out = emit_expression(&func);

        // DOM-only listener → `ɵɵdomListener` (not `ɵɵlistener`).
        assert!(out.contains("\u{0275}\u{0275}domListener"), "got: {out}");
        assert!(!out.contains("\u{0275}\u{0275}listener("), "got: {out}");
        assert!(out.contains("\"click\""), "got: {out}");
        // The handler fn carries Angular's canonical listener name.
        assert!(
            out.contains("Test_Template_button_click_0_listener"),
            "got: {out}"
        );
        // The handler body invokes the action and returns it.
        assert!(out.contains("ctx.f()"), "got: {out}");
        // `(click)="f()"` does NOT reference `$event`, so the handler takes no `$event` parameter
        // (Angular `resolveDollarEvent` / `reifyListenerHandler`).
        assert!(!out.contains("$event"), "got: {out}");
        assert!(out.contains("return"), "got: {out}");
        // The listener (a creation-block op) sits between start and end, so the element does NOT
        // collapse to a single `ɵɵdomElement` (Angular `collapseEmptyInstructions`): it stays
        // `ɵɵdomElementStart(0, "button", 0) … ɵɵdomElementEnd()`.
        assert!(out.contains("\u{0275}\u{0275}domElementStart(0, \"button\", 0)"), "got: {out}");
        assert!(out.contains("\u{0275}\u{0275}domElementEnd()"), "got: {out}");
        assert!(!out.contains("\u{0275}\u{0275}domElement(0,"), "got: {out}");
        // A regular listener consumes no var slots.
        assert_eq!(builder.vars(), 0);
        // A `[3, "click"]` binding const was interned.
        assert_eq!(builder.const_pool().entries().len(), 1);
        let consts = emit_expression(&builder.const_pool().to_const_array().unwrap());
        assert!(consts.contains("3"), "got: {consts}");
        assert!(consts.contains("\"click\""), "got: {consts}");
    }

    #[test]
    fn event_handler_referencing_dollar_event_emits_param() {
        // `<button (click)="g($event)"></button>` ⇒ the handler references `$event`, so its
        // function declares the `$event` parameter (Angular `resolveDollarEvent` /
        // `reifyListenerHandler`).
        let nodes = vec![Node::Element(Element {
            name: "button".to_string(),
            attributes: vec![],
            inputs: vec![],
            outputs: vec![BoundEvent {
                name: "click".to_string(),
                kind: ParsedEventType::Regular,
                handler: call_g_with_event(),
                target: None,
                phase: None,
                source_span: t_span(),
                handler_span: t_span(),
                key_span: t_span(),
            }],
            directives: vec![],
            children: vec![],
            references: vec![],
            is_self_closing: false,
            source_span: t_span(),
            start_source_span: t_span(),
            end_source_span: None,
            is_void: false,
            i18n: None,
        })];
        let input = TemplateCompilationInput::new("Test_Template", nodes);
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let func = builder.build_template_function(&input);
        let out = emit_expression(&func);

        assert!(out.contains("\u{0275}\u{0275}domListener"), "got: {out}");
        // The handler function declares the `$event` parameter because it references it.
        assert!(
            out.contains("function Test_Template_button_click_0_listener($event)"),
            "got: {out}"
        );
    }

    #[test]
    fn two_part_interpolation_emits_text_interpolate2() {
        // `<p>{{a}} {{b}}</p>` ⇒ `ɵɵtextInterpolate2("", ctx.a, " ", ctx.b, "")`, vars = 2.
        let nodes = vec![Node::Element(Element {
            name: "p".to_string(),
            attributes: vec![],
            inputs: vec![],
            outputs: vec![],
            directives: vec![],
            children: vec![Node::BoundText(BoundText {
                value: interpolation_ab(),
                source_span: t_span(),
                i18n: None,
            })],
            references: vec![],
            is_self_closing: false,
            source_span: t_span(),
            start_source_span: t_span(),
            end_source_span: None,
            is_void: false,
            i18n: None,
        })];
        let input = TemplateCompilationInput::new("Test_Template", nodes);
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let func = builder.build_template_function(&input);
        let out = emit_expression(&func);

        assert!(out.contains("\u{0275}\u{0275}textInterpolate2"), "got: {out}");
        assert!(out.contains("ctx.a"), "got: {out}");
        assert!(out.contains("ctx.b"), "got: {out}");
        // Two interpolation expressions ⇒ two var slots.
        assert_eq!(builder.vars(), 2);
    }

    /// Build a single childless element carrying one bound input, with the given tag and binding.
    fn element_with_input(tag: &str, name: &str, kind: BindingType, value: AstNode) -> Vec<Node> {
        vec![Node::Element(Element {
            name: tag.to_string(),
            attributes: vec![],
            inputs: vec![BoundAttribute {
                name: name.to_string(),
                kind,
                security_context: SecurityContext::None,
                value,
                unit: None,
                source_span: t_span(),
                key_span: t_span(),
                value_span: None,
                i18n: None,
            }],
            outputs: vec![],
            directives: vec![],
            children: vec![],
            references: vec![],
            is_self_closing: false,
            source_span: t_span(),
            start_source_span: t_span(),
            end_source_span: None,
            is_void: false,
            i18n: None,
        })]
    }

    #[test]
    fn class_binding_emits_class_prop_and_reserves_two_vars() {
        // `<div [class.active]="x"></div>` ⇒ `ɵɵclassProp("active", ctx.x)`, vars = 2, and NO
        // binding const (bound class names are not extracted unless the expression is empty).
        let nodes = element_with_input("div", "active", BindingType::Class, prop_read("x"));
        let input = TemplateCompilationInput::new("Test_Template", nodes);
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let out = emit_expression(&builder.build_template_function(&input));

        assert!(out.contains("\u{0275}\u{0275}classProp"), "got: {out}");
        assert!(out.contains("\"active\""), "got: {out}");
        assert!(out.contains("ctx.x"), "got: {out}");
        // A single style/class binding reserves two var slots (Angular `varsUsedByOp`).
        assert_eq!(builder.vars(), 2, "vars = {}", builder.vars());
        // A pure (non-empty) bound class binding does NOT extract a name into the const attrs
        // array (Angular `extractAttributes`: `ClassProp` only extracts when the expression is
        // empty). So no const is interned and the element creation instruction carries no const
        // index — `ɵɵdomElement(0, "div")` with two params only.
        assert!(builder.const_pool().is_empty(), "const pool not empty");
        assert!(out.contains("\u{0275}\u{0275}domElement(0, \"div\")"), "got: {out}");
        assert!(!out.contains("\u{0275}\u{0275}domElement(0, \"div\", 0)"), "got: {out}");
    }

    #[test]
    fn style_binding_emits_style_prop_and_reserves_two_vars() {
        // `<div [style.width]="w"></div>` ⇒ `ɵɵstyleProp("width", ctx.w)`, vars = 2.
        let nodes = element_with_input("div", "width", BindingType::Style, prop_read("w"));
        let input = TemplateCompilationInput::new("Test_Template", nodes);
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let out = emit_expression(&builder.build_template_function(&input));

        assert!(out.contains("\u{0275}\u{0275}styleProp"), "got: {out}");
        assert!(out.contains("\"width\""), "got: {out}");
        assert!(out.contains("ctx.w"), "got: {out}");
        assert_eq!(builder.vars(), 2, "vars = {}", builder.vars());
        // A pure (non-empty) bound style binding does NOT extract a name into the const attrs
        // array, so no const is interned and the element creation instruction carries no const
        // index — `ɵɵdomElement(0, "div")` with two params only.
        assert!(builder.const_pool().is_empty(), "const pool not empty");
        assert!(out.contains("\u{0275}\u{0275}domElement(0, \"div\")"), "got: {out}");
        assert!(!out.contains("\u{0275}\u{0275}domElement(0, \"div\", 0)"), "got: {out}");
    }

    #[test]
    fn attr_binding_emits_attribute_and_reserves_one_var() {
        // `<div [attr.role]="r"></div>` ⇒ `ɵɵattribute("role", ctx.r)`, vars = 1, decls = 1, and NO
        // const (Angular `extractAttributes` only extracts *text* attributes — an `[attr.x]` binding
        // is never lifted into the binding-const group, so the element carries no const index).
        let nodes = element_with_input("div", "role", BindingType::Attribute, prop_read("r"));
        let input = TemplateCompilationInput::new("Test_Template", nodes);
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let out = emit_expression(&builder.build_template_function(&input));

        assert!(out.contains("\u{0275}\u{0275}attribute"), "got: {out}");
        assert!(!out.contains("\u{0275}\u{0275}domProperty"), "got: {out}");
        assert!(out.contains("\"role\""), "got: {out}");
        assert!(out.contains("ctx.r"), "got: {out}");
        assert_eq!(builder.vars(), 1, "vars = {}", builder.vars());
        assert_eq!(builder.data_index(), 1, "decls = {}", builder.data_index());
        // No binding-const group is interned, so the element has only `(slot, tag)` params.
        assert!(builder.const_pool().is_empty(), "const pool not empty");
        assert!(out.contains("\u{0275}\u{0275}domElement(0, \"div\")"), "got: {out}");
    }

    #[test]
    fn mixed_prop_event_class_binding_order_and_const() {
        // `<input [value]="v" (input)="o($event)" [class.err]="e">` ⇒ Angular's ordering:
        //   consts: [[3, "input", "value"]]  (event name BEFORE property name; class extracts none)
        //   update: ɵɵclassProp("err", …) THEN ɵɵdomProperty("value", …)  (UPDATE_ORDERING)
        let nodes = vec![Node::Element(Element {
            name: "input".to_string(),
            attributes: vec![],
            inputs: vec![
                BoundAttribute {
                    name: "value".to_string(),
                    kind: BindingType::Property,
                    security_context: SecurityContext::None,
                    value: prop_read("v"),
                    unit: None,
                    source_span: t_span(),
                    key_span: t_span(),
                    value_span: None,
                    i18n: None,
                },
                BoundAttribute {
                    name: "err".to_string(),
                    kind: BindingType::Class,
                    security_context: SecurityContext::None,
                    value: prop_read("e"),
                    unit: None,
                    source_span: t_span(),
                    key_span: t_span(),
                    value_span: None,
                    i18n: None,
                },
            ],
            outputs: vec![BoundEvent {
                name: "input".to_string(),
                kind: ParsedEventType::Regular,
                handler: call_g_with_event(),
                target: None,
                phase: None,
                source_span: t_span(),
                handler_span: t_span(),
                key_span: t_span(),
            }],
            directives: vec![],
            children: vec![],
            references: vec![],
            is_self_closing: false,
            source_span: t_span(),
            start_source_span: t_span(),
            end_source_span: None,
            is_void: false,
            i18n: None,
        })];
        let input = TemplateCompilationInput::new("Test_Template", nodes);
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let out = emit_expression(&builder.build_template_function(&input));

        // Const binding-name order: event ("input") before property ("value"); class extracts none.
        assert_eq!(builder.const_pool().entries().len(), 1);
        let consts = emit_expression(&builder.const_pool().to_const_array().unwrap());
        let flat: String = consts.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(flat.contains("[[3,\"input\",\"value\"]]"), "got: {consts}");

        // Update-instruction order: classProp before domProperty (Angular UPDATE_ORDERING).
        let class_at = out.find("\u{0275}\u{0275}classProp").expect("classProp");
        let prop_at = out.find("\u{0275}\u{0275}domProperty").expect("domProperty");
        assert!(class_at < prop_at, "classProp must precede domProperty, got: {out}");
    }

    #[test]
    fn static_class_plus_bound_property_merges_const_groups() {
        // `<div class="box" [id]="x"></div>` ⇒ const `[1, "box", 3, "id"]` (Classes then Bindings),
        // `ɵɵdomElement(0, "div", 0)`, update `ɵɵdomProperty("id", ctx.x)`.
        let nodes = vec![Node::Element(Element {
            name: "div".to_string(),
            attributes: vec![TextAttribute {
                name: "class".to_string(),
                value: "box".to_string(),
                source_span: t_span(),
                key_span: None,
                value_span: None,
                i18n: None,
            }],
            inputs: vec![BoundAttribute {
                name: "id".to_string(),
                kind: BindingType::Property,
                security_context: SecurityContext::None,
                value: prop_read("x"),
                unit: None,
                source_span: t_span(),
                key_span: t_span(),
                value_span: None,
                i18n: None,
            }],
            outputs: vec![],
            directives: vec![],
            children: vec![],
            references: vec![],
            is_self_closing: false,
            source_span: t_span(),
            start_source_span: t_span(),
            end_source_span: None,
            is_void: false,
            i18n: None,
        })];
        let input = TemplateCompilationInput::new("Test_Template", nodes);
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let out = emit_expression(&builder.build_template_function(&input));

        assert!(out.contains("\u{0275}\u{0275}domProperty"), "got: {out}");
        assert!(out.contains("\u{0275}\u{0275}domElement(0, \"div\", 0)"), "got: {out}");
        // The single const merges the static Classes group (`1, "box"`) with the Bindings group
        // (`3, "id"`) in that fixed order. The emitter may pretty-print the array across lines, so
        // assert on the order of the (whitespace-stripped) tokens rather than exact formatting.
        assert_eq!(builder.const_pool().entries().len(), 1);
        let consts = emit_expression(&builder.const_pool().to_const_array().unwrap());
        let flat: String = consts.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(flat.contains("[[1,\"box\",3,\"id\"]]"), "got: {consts}");
    }

    // -- Control-flow instruction tests (Task A). --

    use crate::expression::ast::AstWithSource;
    use crate::template::r3_ast::{
        BlockSpans, ForLoopBlock, IfBlock, IfBlockBranch, SwitchBlock, SwitchBlockCase,
        SwitchBlockCaseGroup, Variable,
    };

    fn block_spans() -> BlockSpans {
        BlockSpans {
            name_span: t_span(),
            source_span: t_span(),
            start_source_span: t_span(),
            end_source_span: None,
        }
    }

    /// `<div>a</div>` — a childed element whose body is a single static text.
    fn div_a() -> Vec<Node> {
        vec![Node::Element(Element {
            name: "div".to_string(),
            attributes: vec![],
            inputs: vec![],
            outputs: vec![],
            directives: vec![],
            children: vec![Node::Text(Text {
                value: "a".to_string(),
                source_span: t_span(),
            })],
            references: vec![],
            is_self_closing: false,
            source_span: t_span(),
            start_source_span: t_span(),
            end_source_span: None,
            is_void: false,
            i18n: None,
        })]
    }

    #[test]
    fn if_block_emits_template_and_conditional() {
        // `@if (cond) { <div>a</div> }` ⇒ creation `ɵɵtemplate(...)`, update `ɵɵconditional(...)`.
        let block = IfBlock {
            branches: vec![IfBlockBranch {
                expression: Some(prop_read("cond")),
                children: div_a(),
                expression_alias: None,
                spans: block_spans(),
                i18n: None,
            }],
            spans: block_spans(),
        };
        let input = TemplateCompilationInput::new("Test_Template", vec![Node::IfBlock(block)]);
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let func = builder.build_template_function(&input);
        let out = emit_expression(&func);

        // Angular 21: a single `@if` branch lowers to `ɵɵconditionalCreate` referencing a HOISTED,
        // NAMED branch template fn, with the branch root element tag (`'div'`) as the trailing arg.
        assert!(out.contains("\u{0275}\u{0275}conditionalCreate"), "got: {out}");
        // The branch template is hoisted as a top-level named function (not an inline closure).
        assert!(
            out.contains("function Test_Conditional_0_Template(rf, ctx)"),
            "got: {out}"
        );
        // The create instruction references it by name and passes the root tag.
        assert!(
            out.contains("\u{0275}\u{0275}conditionalCreate(0, Test_Conditional_0_Template, 2, 0, \"div\")"),
            "got: {out}"
        );
        // The update block keeps the `ɵɵconditional(test)` selector.
        assert!(out.contains("\u{0275}\u{0275}conditional("), "got: {out}");
        // The branch condition resolves against `ctx` and selects slot 0 (else -1).
        assert!(out.contains("ctx.cond ? 0 : -1"), "got: {out}");
        // One branch condition reserves one var slot.
        assert_eq!(builder.vars(), 1);
        // The hoisted fn is also surfaced via the accessor.
        assert_eq!(builder.hoisted_functions().len(), 1);
    }

    #[test]
    fn if_else_emits_two_templates_and_default_branch() {
        // `@if (cond) { <div>a</div> } @else { <div>a</div> }`.
        let block = IfBlock {
            branches: vec![
                IfBlockBranch {
                    expression: Some(prop_read("cond")),
                    children: div_a(),
                    expression_alias: None,
                    spans: block_spans(),
                    i18n: None,
                },
                IfBlockBranch {
                    expression: None, // @else
                    children: div_a(),
                    expression_alias: None,
                    spans: block_spans(),
                    i18n: None,
                },
            ],
            spans: block_spans(),
        };
        let input = TemplateCompilationInput::new("Test_Template", vec![Node::IfBlock(block)]);
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let func = builder.build_template_function(&input);
        let out = emit_expression(&func);

        // The first branch lowers to `ɵɵconditionalCreate` and the `@else` branch's
        // `ɵɵconditionalBranchCreate` is CHAINED onto it as a call operand (Angular 21
        // `CHAIN_COMPATIBILITY`: `conditionalCreate → conditionalBranchCreate`), so the whole chain
        // is a single statement and `conditionalBranchCreate` never appears as its own instruction.
        assert!(
            out.contains(
                "\u{0275}\u{0275}conditionalCreate(0, Test_Conditional_0_Template, 2, 0, \"div\")(1, Test_Conditional_1_Template, 2, 0, \"div\")"
            ),
            "got: {out}"
        );
        assert!(
            !out.contains("\u{0275}\u{0275}conditionalBranchCreate"),
            "branch should be chained onto conditionalCreate, got: {out}"
        );
        // Both branch fns are hoisted as named top-level functions.
        assert!(out.contains("function Test_Conditional_0_Template(rf, ctx)"), "got: {out}");
        assert!(out.contains("function Test_Conditional_1_Template(rf, ctx)"), "got: {out}");
        assert_eq!(builder.hoisted_functions().len(), 2);
        // The `@else` default selects slot 1 rather than -1.
        assert!(out.contains("\u{0275}\u{0275}conditional(ctx.cond ? 0 : 1)"), "got: {out}");
    }

    #[test]
    fn for_block_emits_repeater_create_and_repeater() {
        // `@for (x of xs; track x) { <li>{{x}}</li> }`.
        let item = Variable {
            name: "x".to_string(),
            value: "$implicit".to_string(),
            source_span: t_span(),
            key_span: t_span(),
            value_span: None,
        };
        let li_body = vec![Node::Element(Element {
            name: "li".to_string(),
            attributes: vec![],
            inputs: vec![],
            outputs: vec![],
            directives: vec![],
            children: vec![Node::BoundText(BoundText {
                value: prop_read("x"),
                source_span: t_span(),
                i18n: None,
            })],
            references: vec![],
            is_self_closing: false,
            source_span: t_span(),
            start_source_span: t_span(),
            end_source_span: None,
            is_void: false,
            i18n: None,
        })];
        let block = ForLoopBlock {
            item,
            expression: AstWithSource::new(prop_read("xs"), None, String::new(), 0, vec![]),
            track_by: Some(AstWithSource::new(prop_read("x"), None, String::new(), 0, vec![])),
            track_keyword_span: None,
            context_variables: vec![],
            children: li_body,
            empty: None,
            main_block_span: t_span(),
            spans: block_spans(),
            i18n: None,
        };
        let input = TemplateCompilationInput::new("Test_Template", vec![Node::ForLoopBlock(block)]);
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let func = builder.build_template_function(&input);
        let out = emit_expression(&func);

        // Angular 21: `ɵɵrepeaterCreate(slot, <Fn>, decls, vars, "li", null, trackBy)` referencing a
        // HOISTED, NAMED loop body fn (the primary view fn is at slot+1 — here the `@for` is the root
        // node so slot 0 → `Test_For_1_Template`), with the item element tag (`"li"`) instead of null.
        assert!(
            out.contains("\u{0275}\u{0275}repeaterCreate(0, Test_For_1_Template, 2, 1, \"li\", null, "),
            "got: {out}"
        );
        // The loop body is hoisted as a top-level named function.
        assert!(out.contains("function Test_For_1_Template(rf, ctx)"), "got: {out}");
        assert_eq!(builder.hoisted_functions().len(), 1);
        // Inside the body the loop variable is bound as a `const` off `ctx.$implicit`, and the `x`
        // read lowers to that local (`x_r1`), NOT `ctx.x`.
        assert!(out.contains("const x_r1 = ctx.$implicit"), "got: {out}");
        // The body interpolation reads the local `x_r1`, not `ctx.x` (the collection is `ctx.xs`).
        assert!(out.contains("textInterpolate1(\"\", x_r1, \"\")"), "got: {out}");
        // `track x` (the item) optimizes to the identity helper.
        assert!(out.contains("\u{0275}\u{0275}repeaterTrackByIdentity"), "got: {out}");
        // The update block emits `ɵɵrepeater(<collection>)`; the collection resolves against `ctx`.
        assert!(out.contains("\u{0275}\u{0275}repeater(ctx.xs)"), "got: {out}");
    }

    #[test]
    fn for_block_track_index_uses_track_by_index() {
        let item = Variable {
            name: "x".to_string(),
            value: "$implicit".to_string(),
            source_span: t_span(),
            key_span: t_span(),
            value_span: None,
        };
        let block = ForLoopBlock {
            item,
            expression: AstWithSource::new(prop_read("xs"), None, String::new(), 0, vec![]),
            track_by: Some(AstWithSource::new(
                prop_read("$index"),
                None,
                String::new(),
                0,
                vec![],
            )),
            track_keyword_span: None,
            context_variables: vec![],
            children: div_a(),
            empty: None,
            main_block_span: t_span(),
            spans: block_spans(),
            i18n: None,
        };
        let input = TemplateCompilationInput::new("Test_Template", vec![Node::ForLoopBlock(block)]);
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let func = builder.build_template_function(&input);
        let out = emit_expression(&func);
        assert!(
            out.contains("\u{0275}\u{0275}repeaterTrackByIndex"),
            "got: {out}"
        );
    }

    #[test]
    fn for_block_with_empty_emits_empty_view() {
        let item = Variable {
            name: "x".to_string(),
            value: "$implicit".to_string(),
            source_span: t_span(),
            key_span: t_span(),
            value_span: None,
        };
        let block = ForLoopBlock {
            item,
            expression: AstWithSource::new(prop_read("xs"), None, String::new(), 0, vec![]),
            track_by: Some(AstWithSource::new(prop_read("x"), None, String::new(), 0, vec![])),
            track_keyword_span: None,
            context_variables: vec![],
            children: div_a(),
            empty: Some(crate::template::r3_ast::ForLoopBlockEmpty {
                children: div_a(),
                spans: block_spans(),
                i18n: None,
            }),
            main_block_span: t_span(),
            spans: block_spans(),
            i18n: None,
        };
        let input = TemplateCompilationInput::new("Test_Template", vec![Node::ForLoopBlock(block)]);
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let func = builder.build_template_function(&input);
        let out = emit_expression(&func);
        assert!(out.contains("\u{0275}\u{0275}repeaterCreate"), "got: {out}");
        // The empty-view boolean (`false`) marks the trailing empty-view args.
        assert!(out.contains("false"), "got: {out}");
    }

    #[test]
    fn switch_block_emits_templates_and_conditional() {
        // `@switch (v) { @case (1) { <div>a</div> } @default { <div>a</div> } }`.
        let case1 = SwitchBlockCaseGroup {
            cases: vec![SwitchBlockCase {
                expression: Some(prop_read("one")),
                spans: block_spans(),
            }],
            children: div_a(),
            spans: block_spans(),
            i18n: None,
        };
        let default = SwitchBlockCaseGroup {
            cases: vec![SwitchBlockCase {
                expression: None,
                spans: block_spans(),
            }],
            children: div_a(),
            spans: block_spans(),
            i18n: None,
        };
        let block = SwitchBlock {
            expression: prop_read("v"),
            groups: vec![case1, default],
            unknown_blocks: vec![],
            exhaustive_check: None,
            spans: block_spans(),
        };
        let input = TemplateCompilationInput::new("Test_Template", vec![Node::SwitchBlock(block)]);
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let func = builder.build_template_function(&input);
        let out = emit_expression(&func);

        // `@switch` shares the conditional-create mechanism: the first case group lowers to
        // `ɵɵconditionalCreate` (with the case root tag `'div'`) and the remaining cases' branch
        // creates are CHAINED onto it as call operands (Angular 21), so the whole chain is one
        // statement and `conditionalBranchCreate` never appears as its own instruction.
        assert!(
            out.contains(
                "\u{0275}\u{0275}conditionalCreate(0, Test_Case_0_Template, 2, 0, \"div\")(1, Test_Case_1_Template, 2, 0, \"div\")"
            ),
            "got: {out}"
        );
        assert!(
            !out.contains("\u{0275}\u{0275}conditionalBranchCreate"),
            "case branch should be chained onto conditionalCreate, got: {out}"
        );
        assert!(out.contains("function Test_Case_0_Template(rf, ctx)"), "got: {out}");
        assert!(out.contains("function Test_Case_1_Template(rf, ctx)"), "got: {out}");
        assert_eq!(builder.hoisted_functions().len(), 2);
        // The update block keeps the `ɵɵconditional(test)` selector.
        assert!(out.contains("\u{0275}\u{0275}conditional("), "got: {out}");
        // Angular binds the discriminant to a TEMPORARY local declared at the head of the update
        // block (`let tmp_0_0;`), assigns it on first use (`(tmp_0_0 = ctx.v)`), and reuses it for
        // each case comparison. Here there is a single `@case` test, so the temp is assigned in that
        // one comparison; the `@default` is the fallback result (slot 1).
        assert!(out.contains("let tmp_0_0;"), "got: {out}");
        assert!(
            out.contains("\u{0275}\u{0275}conditional((tmp_0_0 = ctx.v) === ctx.one ? 0 : 1)"),
            "got: {out}"
        );
    }

    #[test]
    fn nested_for_inside_if_walks_up_via_next_context() {
        // `<div>@if (cond) { <ul>@for (x of xs; track x) { <li>{{x}}</li> }</ul> }</div>`.
        //
        // Inside the `@if` embedded view (nesting level 1), the `@for` collection read `xs` resolves
        // to the ANCESTOR (component) context, so Angular declares `const ctx_r1 = ɵɵnextContext();`
        // once at the head of that view's update block and rewrites the read to `ctx_r1.xs`. The
        // loop variable `x` of the (deeper) `@for` body stays a LOCAL (`x_rN`), never routed through
        // `ɵɵnextContext`, and the `@if` condition `cond` stays on the ROOT `ctx` (level 0).
        let item = Variable {
            name: "x".to_string(),
            value: "$implicit".to_string(),
            source_span: t_span(),
            key_span: t_span(),
            value_span: None,
        };
        let li_body = vec![Node::Element(Element {
            name: "li".to_string(),
            attributes: vec![],
            inputs: vec![],
            outputs: vec![],
            directives: vec![],
            children: vec![Node::BoundText(BoundText {
                value: prop_read("x"),
                source_span: t_span(),
                i18n: None,
            })],
            references: vec![],
            is_self_closing: false,
            source_span: t_span(),
            start_source_span: t_span(),
            end_source_span: None,
            is_void: false,
            i18n: None,
        })];
        let for_block = ForLoopBlock {
            item,
            expression: AstWithSource::new(prop_read("xs"), None, String::new(), 0, vec![]),
            track_by: Some(AstWithSource::new(prop_read("x"), None, String::new(), 0, vec![])),
            track_keyword_span: None,
            context_variables: vec![],
            children: li_body,
            empty: None,
            main_block_span: t_span(),
            spans: block_spans(),
            i18n: None,
        };
        // `<ul>` wrapping the `@for`.
        let ul = Node::Element(Element {
            name: "ul".to_string(),
            attributes: vec![],
            inputs: vec![],
            outputs: vec![],
            directives: vec![],
            children: vec![Node::ForLoopBlock(for_block)],
            references: vec![],
            is_self_closing: false,
            source_span: t_span(),
            start_source_span: t_span(),
            end_source_span: None,
            is_void: false,
            i18n: None,
        });
        let if_block = IfBlock {
            branches: vec![IfBlockBranch {
                expression: Some(prop_read("cond")),
                children: vec![ul],
                expression_alias: None,
                spans: block_spans(),
                i18n: None,
            }],
            spans: block_spans(),
        };
        let div = Node::Element(Element {
            name: "div".to_string(),
            attributes: vec![],
            inputs: vec![],
            outputs: vec![],
            directives: vec![],
            children: vec![Node::IfBlock(if_block)],
            references: vec![],
            is_self_closing: false,
            source_span: t_span(),
            start_source_span: t_span(),
            end_source_span: None,
            is_void: false,
            i18n: None,
        });
        let input = TemplateCompilationInput::new("Test_Template", vec![div]);
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let out = emit_expression(&builder.build_template_function(&input));

        // The `@if` branch view declares its shared ancestor context once via `ɵɵnextContext()` and
        // reads the collection through it: `const ctx_r1 = ɵɵnextContext(); … ɵɵrepeater(ctx_r1.xs);`.
        // (External `ɵɵ*` refs are emitted under the `i0` namespace import, hence the `i0.` prefix.)
        assert!(
            out.contains("const ctx_r1 = i0.\u{0275}\u{0275}nextContext();"),
            "expected a single nextContext decl in the @if view, got: {out}"
        );
        // Exactly one nextContext declaration in the whole output.
        assert_eq!(
            out.matches("\u{0275}\u{0275}nextContext").count(),
            1,
            "expected a single nextContext call, got: {out}"
        );
        assert!(
            out.contains("\u{0275}\u{0275}repeater(ctx_r1.xs)"),
            "the collection should resolve against ctx_r1, got: {out}"
        );
        // The collection must NOT be read off the embedded view's own `ctx`.
        assert!(
            !out.contains("\u{0275}\u{0275}repeater(ctx.xs)"),
            "collection should walk up via nextContext, not read local ctx, got: {out}"
        );
        // The loop variable stays a local of the @for body (bound off `ctx.$implicit`).
        assert!(out.contains("= ctx.$implicit"), "loop var should bind off ctx.$implicit, got: {out}");
        // The `@if` view's component read (`cond`) stays on the ROOT `ctx` (level 0). The branch
        // template occupies slot 1 (the `<div>` host is slot 0), so the selector is
        // `ctx.cond ? 1 : -1`.
        assert!(out.contains("\u{0275}\u{0275}conditional(ctx.cond ? 1 : -1)"), "got: {out}");
    }

    #[test]
    fn ng_template_with_reference_var_lowers_to_dom_template_with_extractor() {
        // `<ng-template #tpl><span>tpl</span></ng-template>` ⇒ Angular 21:
        //   host decls: 2 (one for the template slot, one reserved for the `#tpl` ref),
        //   consts: [["tpl", ""]],
        //   the embedded view is HOISTED as `Test_ng_template_0_Template` (`<base>_<sanitizedTag>_
        //   <slot>_Template`) and referenced by name,
        //   ɵɵdomTemplate(0, Test_ng_template_0_Template, <nestedDecls>, 0, "ng-template", null, 0,
        //                 ɵɵtemplateRefExtractor)
        use crate::template::r3_ast::{Reference, Template};
        let tpl = Template {
            tag_name: Some("ng-template".to_string()),
            attributes: vec![],
            inputs: vec![],
            outputs: vec![],
            directives: vec![],
            template_attrs: vec![],
            children: vec![Node::Element(Element {
                name: "span".to_string(),
                attributes: vec![],
                inputs: vec![],
                outputs: vec![],
                directives: vec![],
                children: vec![Node::Text(Text {
                    value: "tpl".to_string(),
                    source_span: t_span(),
                })],
                references: vec![],
                is_self_closing: false,
                source_span: t_span(),
                start_source_span: t_span(),
                end_source_span: None,
                is_void: false,
                i18n: None,
            })],
            references: vec![Reference {
                name: "tpl".to_string(),
                value: String::new(),
                source_span: t_span(),
                key_span: t_span(),
                value_span: None,
            }],
            variables: vec![],
            is_self_closing: false,
            source_span: t_span(),
            start_source_span: t_span(),
            end_source_span: None,
            i18n: None,
        };
        let input = TemplateCompilationInput::new("Test_Template", vec![Node::Template(tpl)]);
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let out = emit_expression(&builder.build_template_function(&input));

        // DOM-only template instruction with the local-ref extractor lowering. The embedded view is
        // hoisted under a tag-derived name and referenced by name; the trailing args are
        // `<nestedDecls>, 0, "ng-template", null, 0, ɵɵtemplateRefExtractor` — constIndex `null`
        // (no static attrs) and localRefs index `0`.
        assert!(
            out.contains("\u{0275}\u{0275}domTemplate(0, Test_ng_template_0_Template, "),
            "should emit domTemplate referencing the hoisted view fn, got: {out}"
        );
        assert!(
            out.contains("function Test_ng_template_0_Template(rf, ctx)"),
            "embedded view should be hoisted as a named fn, got: {out}"
        );
        assert!(
            out.contains(", \"ng-template\", null, 0, "),
            "got: {out}"
        );
        assert!(out.contains("\u{0275}\u{0275}templateRefExtractor)"), "got: {out}");
        assert!(!out.contains("\u{0275}\u{0275}template("), "should be domTemplate, got: {out}");
        // The `#tpl` ref reserves an extra decl slot (2 = template + ref).
        assert_eq!(builder.data_index(), 2, "decls = {}", builder.data_index());
        // The flattened `["tpl", ""]` local-ref const is interned at index 0.
        assert_eq!(builder.const_pool().entries().len(), 1);
        let consts = emit_expression(&builder.const_pool().to_const_array().unwrap());
        let flat: String = consts.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(flat.contains("[[\"tpl\",\"\"]]"), "got: {consts}");
    }
}
