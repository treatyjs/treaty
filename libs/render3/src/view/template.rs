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
//! foundation, because that is the algorithm the brief targets. The v22 `template/pipeline/**`
//! rewrite is a wholesale, deliberately out-of-scope replacement of this whole file (not an
//! incremental in-file change), so it is intentionally not undertaken here.
//!
//! Owned & arena-free (Box/Vec/String), matching the rest of the crate.

use crate::expression::ast::ExprKind as AstExprKind;
use crate::expression::ast::AstNode;
use crate::expression::ast::ParsedEventType;
use crate::expression_converter::{
    convert_action_binding_with, convert_property_binding_with, convert_property_binding_with_pipes,
    LocalResolver, PipeSlotAllocator, PipeSlots,
};
use crate::identifiers::R3;
use crate::output_ast as o;
use crate::output_ast::{Expr, FnParam, Stmt, StmtKind, StmtModifier};
use crate::template::r3_ast::{
    BoundAttribute, BoundEvent, BoundText, Content, DeferredBlock, DeferredBlockTriggers,
    DeferredTriggerKind, Element, ForLoopBlock, IfBlock, LetDeclaration, Node,
    SwitchBlock, SwitchBlockCase, Template, Text, TextAttribute, Visitor,
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
// SLOT / VAR COUNTING. The classic `TemplateDefinitionBuilder` computes `decls` and `vars` with two
// running cursors threaded through the walk — `_dataIndex` (`allocateDataSlot`) for data slots and
// `_bindingSlots` (`allocateBindingSlots`, i.e. the v22 `varsUsedByOp` accounting) for binding/var
// slots — NOT from the `R3TargetBinder` result. This port reproduces exactly that: [`Self::data_index`]
// and [`Self::binding_slots`] are those two cursors, incremented per op as Angular does, so
// [`Self::data_index`] / [`Self::vars`] are the faithful `decls` / `vars` totals. (A `R3BoundTarget`
// is consulted by the *upstream* template-transform/binder for directive matching, reference-target
// resolution and pipe-usage detection — concerns that live outside this view-function emitter — and
// is therefore not an input to slot/var counting here.)
// ---------------------------------------------------------------------------

/// Minimal owned input describing the template to compile.
#[derive(Debug, Clone)]
pub struct TemplateCompilationInput {
    /// The generated view-function name (e.g. `AppComponent_Template`).
    pub name: String,
    /// The root template nodes to walk.
    pub nodes: Vec<Node>,
    /// Compilation mode: `true` selects Angular's `TemplateCompilationMode.DomOnly` instruction set
    /// (`ɵɵdomElement*`/`ɵɵdomListener`/`ɵɵdomProperty`), `false` the `Full` set
    /// (`ɵɵelement*`/`ɵɵlistener`/`ɵɵproperty`). Angular picks `DomOnly` iff the component
    /// `isStandalone && !hasDirectiveDependencies` (`render3/view/compiler.ts`), `Full` otherwise.
    ///
    /// Defaults to `true` (DomOnly) in [`Self::new`]: the selectorless `compile_component` entry point
    /// (and this module's unit fixtures) always describe a standalone, dependency-free component, for
    /// which Angular emits the DOM family. The source/decorator front-end overrides it via
    /// [`Self::with_dom_only`] from the parsed `standalone` flag + resolved directive dependencies.
    pub dom_only: bool,
}

impl TemplateCompilationInput {
    pub fn new(name: impl Into<String>, nodes: Vec<Node>) -> Self {
        TemplateCompilationInput {
            name: name.into(),
            nodes,
            dom_only: true,
        }
    }

    /// Set the DOM-only compilation mode (see [`Self::dom_only`]) and return `self`, so callers that
    /// know the component's `isStandalone && !hasDirectiveDependencies` status can select the matching
    /// instruction family.
    pub fn with_dom_only(mut self, dom_only: bool) -> Self {
        self.dom_only = dom_only;
        self
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
    /// i18n const-pool initializer statements (`let $i18n_0$; if (closureMode) { … } else { … }`)
    /// that must run before the `consts` array is built. Non-empty only when the template carries
    /// an i18n block, mirroring Angular's `ComponentCompilationJob.constsInitializers`. When
    /// present the definition emits `consts: () => { …initializers…; return [ … ]; }` instead of
    /// the inline `consts: [ … ]` literal array.
    initializers: Vec<Stmt>,
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

    /// Append an entry along with the initializer statements that produce its value (Angular's
    /// `job.addConst(mainVar, statements)`). Used for i18n messages whose const-array entry is a
    /// `$i18n_n$` read-var initialized lazily in the `consts: () => { … }` arrow body. Unlike
    /// [`intern`], this never de-dupes (each i18n message owns a distinct variable + statements).
    pub fn add_const_with_initializers(&mut self, value: Expr, initializers: Vec<Stmt>) -> usize {
        self.initializers.extend(initializers);
        self.entries.push(value);
        self.entries.len() - 1
    }

    pub fn entries(&self) -> &[Expr] {
        &self.entries
    }

    /// The i18n const-pool initializer statements (empty unless an i18n message was collected).
    pub fn initializers(&self) -> &[Stmt] {
        &self.initializers
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

/// Placeholder a template local-ref slot carries until the host element/template is reached during
/// the main walk and the real `ɵɵreference(slot)` data slot is known. A listener handler whose body
/// reads a `#ref` declared LATER in the template emits `ɵɵreference(PLACEHOLDER + ordinal)`; the slot
/// is patched in [`TemplateDefinitionBuilder::finalize_local_refs`] after the walk. Distinct, large,
/// and disjoint from the pipe placeholder so neither masks a genuine slot literal.
const LOCAL_REF_SLOT_PLACEHOLDER: usize = 2_000_000_000;

/// Placeholder a var (change-detection) offset carries during the walk until the view's two-pass
/// var-offset assignment runs. The var offsets of `ɵɵpipeBindN` / `ɵɵarrowFunction` /
/// `ɵɵpureFunctionN` cannot be assigned inline as each expression is lowered, because Angular
/// (`var_counting.ts`) assigns them in a deferred per-view pass: first every top-level op reserves
/// its var slots, THEN — in expression-traversal (post-order) order — pipes/arrows get offsets, and
/// only AFTER that do pure functions get theirs (the historic two-pass behaviour the TDB emulates).
/// So a var consumer reached during lowering emits `VAR_OFFSET_PLACEHOLDER + ordinal` for its offset
/// argument; [`TemplateDefinitionBuilder::finalize_var_offsets`] resolves each ordinal to its real
/// offset after the whole view is walked. Distinct, large, and disjoint from the slot placeholders so
/// the remap never touches a genuine slot/offset literal.
const VAR_OFFSET_PLACEHOLDER: usize = 3_000_000_000;

/// One registered pipe usage collected during the view walk. Mirrors Angular's
/// `PipeBindingExpr` bookkeeping (`pipe_creation.ts` records a `Pipe` create op per
/// usage). Each distinct *usage* (not name) reserves its own creation slot + var
/// slots, faithful to the classic TDB which allocates a fresh pipe slot per
/// occurrence.
#[derive(Debug, Clone)]
struct PendingPipe {
    /// The pipe name (`uppercase`, `slice`, …) — the `ɵɵpipe(slot, "name")` argument.
    name: String,
    /// The data slot of the creation op that *consumes* this pipe (the text/element/anchor the
    /// owning update binding targets). Angular `pipe_creation.ts` inserts the `Pipe` create op
    /// immediately after this op (skipping any pipe ops already there), so the `ɵɵpipe(...)` lands
    /// inside the consuming element's creation block — e.g. between `ɵɵtext` and the element's
    /// `ɵɵdomElementEnd` — rather than appended at the end of the creation buffer.
    target_slot: usize,
    /// A real data slot pre-assigned *positionally* during the walk, when the pipe's consuming op is
    /// NOT the last data op (e.g. a pipe in a `@let result = x | pipe` value, whose consuming
    /// `ɵɵdeclareLet` precedes later text/element slots). Angular allocates slots by walking the
    /// final create-op list in order, so such a pipe takes the slot immediately after its consuming
    /// op (shifting every later op up). `None` falls back to end-of-data-array allocation, which
    /// coincides with the positional slot for the common case of a pipe consumed by a leaf op.
    real_slot: Option<usize>,
}

/// One var (change-detection) slot consumer recorded during the walk, in expression-lowering
/// (post-order) order. The var offset each consumer's instruction references is assigned in a
/// deferred per-view pass ([`TemplateDefinitionBuilder::finalize_var_offsets`]) that mirrors Angular's
/// `var_counting.ts`: pipes & arrows are assigned first (in this recorded order), pure functions
/// second. `slots` is the number of var slots the consumer occupies (`varsUsedByOp` / `varsUsedByIrExpression`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VarConsumerKind {
    /// `ɵɵpipeBindN` / `ɵɵpipeBindV` — first-pass, `1 + total_args` slots.
    Pipe,
    /// `ɵɵarrowFunction` — first-pass, `1` slot.
    Arrow,
    /// `ɵɵpureFunctionN` / `ɵɵpureFunctionV` — second-pass, `1 + num_args` slots.
    Pure,
    /// A `ɵɵstoreLet(value)` expression (an external `@let`) — first-pass, `1` slot
    /// (`varsUsedByIrExpression` for `ExpressionKind.StoreLet` ⇒ `1`). It consumes a var slot for
    /// change-detection but emits NO offset argument, so it only advances the assignment cursor
    /// (nothing to patch). Recorded AFTER its value is lowered so — post-order, exactly like Angular —
    /// any arrow / pure function the stored value contains is assigned its offset *before* the
    /// enclosing `storeLet` takes its slot. This is what places `ɵɵarrowFunction` at the lower offset
    /// in `@let theFn = (a, b) => …` (the arrow precedes the storeLet in the var sequence).
    StoreLet,
}

/// A recorded var-slot consumer awaiting its deferred offset. `ordinal` is its position in the
/// recorded sequence (the `VAR_OFFSET_PLACEHOLDER + ordinal` literal emitted in its instruction).
#[derive(Debug, Clone, Copy)]
struct VarConsumer {
    kind: VarConsumerKind,
    /// Number of var slots this consumer occupies.
    slots: usize,
}

/// Per-view pipe + var-offset registry. Lives behind a [`std::cell::RefCell`] on the builder so
/// the `&self` expression-lowering path ([`TemplateDefinitionBuilder::lower_expr`])
/// can register pipes and var-slot consumers while lowering.
#[derive(Debug, Default)]
struct PipeState {
    /// Registered pipe usages, in source order; the index is the pipe's ordinal.
    pending: Vec<PendingPipe>,
    /// Every var-slot consumer (pipe / arrow / pure function) reached during the walk, in
    /// expression-lowering (post-order) order. Drives the deferred two-pass var-offset assignment
    /// (`finalize_var_offsets`); the consumer's index is the ordinal its `VAR_OFFSET_PLACEHOLDER + n`
    /// offset literal carries.
    var_consumers: Vec<VarConsumer>,
    /// View-global counter minting the shared-constant reference name for a hoisted pure-literal
    /// factory (`$c0$`, `$c1$`, …). Persists across every binding of the view (the per-binding
    /// expression converter is recreated each call, so the name counter must live here) so distinct
    /// factories get distinct names — Angular keeps `null`, `[]`, and `{foo: a}` factories separate
    /// (`getSharedConstant`). See [`BuilderPipes::intern_pure_function_factory`].
    next_const_name: usize,
    /// View-global counter minting the shared-function reference name for a hoisted arrow factory
    /// (`$arrowFn0$`, `$arrowFn1$`, …) — a namespace independent of the `$cN$` pure-literal names
    /// (Angular `getSharedFunctionReference`).
    next_arrow_name: usize,
    /// Interned factory bodies (kept for structural de-dup, mirroring `ConstantPool.getSharedConstant`
    /// / `getSharedFunctionReference`, which return the SAME reference for an equivalent factory). Each
    /// entry is `(factory_expr, reference_name)`; a structurally-equivalent factory reuses its name.
    interned_factories: Vec<(Expr, String)>,
    /// The data slot of the creation op currently being bound (the text/element/anchor the binding
    /// under lowering targets). Set by [`TemplateDefinitionBuilder::lower_expr`] before each
    /// conversion so [`BuilderPipes::allocate_pipe`] can record it on the [`PendingPipe`]; that slot
    /// later drives where the `ɵɵpipe(...)` creation op is inserted (Angular `addPipeToCreationBlock`).
    current_target_slot: usize,
}

/// The [`PipeSlotAllocator`] the converter receives while a view's update
/// expressions are lowered. Borrows the builder's [`PipeState`] and allocates a
/// placeholder data slot + a real var offset per pipe usage.
struct BuilderPipes<'a> {
    state: &'a std::cell::RefCell<PipeState>,
}

impl BuilderPipes<'_> {
    /// Record a var-slot consumer (pipe / arrow / pure function) reached during lowering and return
    /// the *placeholder* var offset (`VAR_OFFSET_PLACEHOLDER + ordinal`) its instruction should carry.
    /// The real offset is resolved later by [`TemplateDefinitionBuilder::finalize_var_offsets`], which
    /// runs Angular's deferred two-pass assignment over the recorded sequence.
    fn record_var_consumer(&self, kind: VarConsumerKind, slots: usize) -> usize {
        let mut state = self.state.borrow_mut();
        let ordinal = state.var_consumers.len();
        state.var_consumers.push(VarConsumer { kind, slots });
        VAR_OFFSET_PLACEHOLDER + ordinal
    }
}

impl PipeSlotAllocator for BuilderPipes<'_> {
    fn allocate_pipe(&self, name: &str, total_args: usize) -> PipeSlots {
        let (ordinal, target_slot) = {
            let mut state = self.state.borrow_mut();
            let ordinal = state.pending.len();
            let target_slot = state.current_target_slot;
            state.pending.push(PendingPipe {
                name: name.to_string(),
                target_slot,
                real_slot: None,
            });
            (ordinal, target_slot)
        };
        let _ = target_slot;
        // Var slots: one change-detection slot plus one per lowered argument (`1 + args.length`,
        // Angular `varsUsedByOp` for `PipeBinding`/`PipeBindingVariadic`). The offset is a deferred
        // placeholder — pipes are first-pass var consumers, assigned in lowering (post-order) order.
        let var_offset = self.record_var_consumer(VarConsumerKind::Pipe, 1 + total_args);
        PipeSlots {
            slot: PIPE_SLOT_PLACEHOLDER + ordinal,
            var_offset,
        }
    }

    /// Record the single var slot a hoisted `ɵɵarrowFunction` consumes (Angular `varsUsedByOp` ⇒ `1`
    /// for an `ArrowFunction` IR expression, `var_counting.ts:186`) and return its deferred placeholder
    /// offset. Arrows are first-pass var consumers, assigned alongside pipes in lowering order.
    fn allocate_arrow_slot(&self) -> Option<usize> {
        Some(self.record_var_consumer(VarConsumerKind::Arrow, 1))
    }

    /// Record the `1 + num_args` var slots a hoisted `ɵɵpureFunctionN` consumes (Angular `varsUsedByOp`
    /// for a `PureFunctionExpr`, `var_counting.ts:180`) and return its deferred placeholder offset. Pure
    /// functions are SECOND-pass var consumers: every pipe/arrow in the view is assigned an offset
    /// before any pure function (the TDB's lazy pure-function offset assignment).
    fn allocate_pure_function_slot(&self, num_args: usize) -> Option<usize> {
        Some(self.record_var_consumer(VarConsumerKind::Pure, 1 + num_args))
    }

    /// Mint the shared-constant reference name for a hoisted factory, de-duping structurally
    /// equivalent factories (Angular `ConstantPool.getSharedConstant` for pure-literal factories /
    /// `getSharedFunctionReference` for arrow factories — both return the SAME reference for an
    /// equivalent factory). Pure-literal factories get `$cN$` names, arrow factories `$arrowFnN$`,
    /// from independent view-global counters so a view mixing both keeps two sequences. The counter
    /// lives on the view's [`PipeState`] (not the per-binding converter, which is recreated each call),
    /// so distinct factories across distinct bindings get distinct names.
    fn intern_pure_function_factory(&self, factory: &Expr, is_arrow: bool) -> Option<String> {
        let mut state = self.state.borrow_mut();
        if let Some((_, name)) = state
            .interned_factories
            .iter()
            .find(|(f, _)| f.is_equivalent(factory))
        {
            return Some(name.clone());
        }
        let name = if is_arrow {
            let n = state.next_arrow_name;
            state.next_arrow_name = n + 1;
            format!("$arrowFn{n}$")
        } else {
            let n = state.next_const_name;
            state.next_const_name = n + 1;
            format!("$c{n}$")
        };
        state.interned_factories.push((factory.clone(), name.clone()));
        Some(name)
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

/// A `@let` declaration that reserved a `ɵɵdeclareLet` data slot in some view and is therefore
/// readable cross-view via `ɵɵreadContextLet(slot)`. Threaded from a view into every descendant
/// embedded view + listener handler so a read of `name` there can switch to the owner context
/// (`ɵɵnextContext`) and read the stored value (`generate_variables.ts` `letDeclarations` scope).
#[derive(Debug, Clone)]
struct ContextLet {
    /// The author-written `@let` name.
    name: String,
    /// The `ɵɵdeclareLet` data slot in the owner view.
    slot: usize,
    /// The owner view's nesting level (`view_level`); the number of `ɵɵnextContext()` hops a
    /// reader needs is `reader_level - owner_level`.
    owner_level: usize,
}

/// A template local reference (`#user`) declared on an element/template in THIS view. A read of the
/// name resolves to `const $name$ = ɵɵreference(slot)` materialised at the head of the consuming
/// update block (or inside a listener handler), faithful to Angular's `generate_variables.ts`
/// `Reference` lowering. The `slot` is the `ɵɵreference` data slot — `element_slot + 1 + ref_index`
/// (each `#ref` reserves one extra data slot after its host, `liftLocalRefs`).
#[derive(Debug, Clone)]
struct LocalRef {
    /// The author-written reference name (`user`).
    name: String,
    /// The `ɵɵreference(slot)` data slot.
    slot: usize,
    /// The pre-assigned generated local identifier (`user_r<id>`), minted when the host element is
    /// built so the `&self` expression resolver can hand it back without mutating the var counter.
    local_name: String,
}

/// A [`LocalResolver`] that roots the implicit receiver at `ctx` (like [`crate::expression_converter::CtxResolver`])
/// but additionally lowers reads of an embedded view's loop variables to their generated locals
/// (`x` → `x_r1`), faithful to Angular's `resolve_names` + `variable_optimization` phases.
struct LoopVarResolver<'a> {
    vars: &'a [LoopVar],
    /// Template local references (`#ref`) declared in this view: a read resolves to the ref's
    /// generated local and records the usage so a `const <local> = ɵɵreference(slot)` is emitted.
    local_refs: &'a [LocalRef],
    used_local_refs: &'a std::cell::RefCell<Vec<(String, String, usize)>>,
}

impl LocalResolver for LoopVarResolver<'_> {
    fn resolve_implicit_receiver(&self) -> Expr {
        o::variable(CONTEXT_NAME, None)
    }

    fn maybe_resolve_local(&self, name: &str) -> Option<Expr> {
        if let Some(v) = self.vars.iter().find(|v| v.source_name == name) {
            return Some(o::variable(v.local_name.clone(), None));
        }
        resolve_local_ref(name, self.local_refs, self.used_local_refs)
    }
}

/// Resolve a read of a template local reference (`#ref`) to its generated local, recording the
/// usage so the materialising `const <local> = ɵɵreference(slot)` is emitted exactly once.
fn resolve_local_ref(
    name: &str,
    local_refs: &[LocalRef],
    used: &std::cell::RefCell<Vec<(String, String, usize)>>,
) -> Option<Expr> {
    let r = local_refs.iter().find(|r| r.name == name)?;
    let mut used = used.borrow_mut();
    if !used.iter().any(|(n, _, _)| n == &r.name) {
        used.push((r.name.clone(), r.local_name.clone(), r.slot));
    }
    Some(o::variable(r.local_name.clone(), None))
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
    /// This view's own template local references (`#ref`).
    local_refs: &'a [LocalRef],
    used_local_refs: &'a std::cell::RefCell<Vec<(String, String, usize)>>,
}

impl LocalResolver for NestedViewResolver<'_> {
    fn resolve_implicit_receiver(&self) -> Expr {
        // An ancestor-context read: flag that this view needs a `ɵɵnextContext()` and root at the
        // shared context var.
        self.needs.set(true);
        o::variable(self.ctx_name.clone(), None)
    }

    fn maybe_resolve_local(&self, name: &str) -> Option<Expr> {
        if let Some(v) = self.vars.iter().find(|v| v.source_name == name) {
            return Some(o::variable(v.local_name.clone(), None));
        }
        resolve_local_ref(name, self.local_refs, self.used_local_refs)
    }
}

/// Resolver for an event-handler body: roots the implicit receiver at `ctx`, resolves any in-scope
/// `@for` loop variables to their generated locals, and resolves `$event` to the bare `$event`
/// handler parameter (Angular's `resolveDollarEvent` — `$event` reads must NOT become `ctx.$event`).
struct ListenerResolver<'a> {
    vars: &'a [LoopVar],
    /// `@let` source-name → generated readContextLet local, for lets the handler reads cross-view.
    context_let_locals: &'a [(String, String)],
}

impl LocalResolver for ListenerResolver<'_> {
    fn resolve_implicit_receiver(&self) -> Expr {
        o::variable(CONTEXT_NAME, None)
    }

    fn maybe_resolve_local(&self, name: &str) -> Option<Expr> {
        if name == EVENT_NAME {
            return Some(o::variable(EVENT_NAME, None));
        }
        // Inside a callback, EVERY in-scope `@let` is read through its `ɵɵreadContextLet` local —
        // even one declared in THIS view — rather than its in-view `ɵɵstoreLet` local
        // (`generate_variables.ts`: `scope.view !== view.xref || isCallback`). A `@let`'s in-view
        // const is also recorded in `vars` (as a semantic local), so the `context_let_locals` lookup
        // MUST take precedence over `vars` here, otherwise the handler would bind the storeLet temp
        // instead of the `ɵɵreadContextLet` const.
        if let Some((_, local)) = self.context_let_locals.iter().find(|(src, _)| src == name) {
            return Some(o::variable(local.clone(), None));
        }
        // Otherwise a `@for` loop item / `$index` etc. resolves to its generated loop local.
        if let Some(v) = self.vars.iter().find(|v| v.source_name == name) {
            return Some(o::variable(v.local_name.clone(), None));
        }
        None
    }
}

/// Resolver for a generated `@for` custom-trackBy arrow body (`generateTrackFn`). The loop item name
/// resolves to the arrow's first parameter and `$index` to its second; any other implicit-receiver
/// read roots at the component context and flags `used_component_instance` (so the runtime binds the
/// generated trackBy to `this`).
struct TrackFnResolver<'a> {
    item_name: String,
    used_component_instance: &'a std::cell::Cell<bool>,
}

impl LocalResolver for TrackFnResolver<'_> {
    fn resolve_implicit_receiver(&self) -> Expr {
        // A read that is not the item or `$index` came off the component instance.
        self.used_component_instance.set(true);
        o::variable(CONTEXT_NAME, None)
    }

    fn maybe_resolve_local(&self, name: &str) -> Option<Expr> {
        if name == self.item_name || name == "$index" {
            return Some(o::variable(name.to_string(), None));
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Helpers for building instruction calls.
// ---------------------------------------------------------------------------

fn num(n: f64) -> Expr {
    o::literal(o::LiteralValue::Number(n), None)
}

/// The `undefined` literal — the value Angular emits for a *valueless* legacy-animation property
/// binding (`@bar` / `[@baz]` → `ɵɵproperty("@bar", undefined)`).
fn undefined_expr() -> Expr {
    o::literal(o::LiteralValue::Undefined, None)
}

/// Whether a binding/attribute name targets a legacy animation trigger (Angular's synthetic,
/// `@`-prefixed properties — `prepareSyntheticProperty`). Such names reify to a property binding
/// (`ɵɵproperty("@name", …)`) and never enter the element's static-attribute const pool.
fn is_legacy_animation_name(name: &str) -> bool {
    name.starts_with('@')
}

/// Whether a binding expression is "empty" — the parser's representation of `[@baz]` with no
/// `="…"` value: an empty-string literal primitive or an empty/`EmptyExpr` AST. Such a binding
/// emits the `undefined` value.
fn is_empty_binding_value(value: &AstNode) -> bool {
    use crate::expression::ast::LiteralValue as LV;
    match &value.kind {
        AstExprKind::EmptyExpr => true,
        AstExprKind::LiteralPrimitive { value: LV::Str(s) } => s.is_empty(),
        AstExprKind::Interpolation { expressions, .. } => expressions.is_empty(),
        _ => false,
    }
}

/// `sanitizeIdentifier(name)` (`parse_util.ts`): replace every non-word char (`/\W/g`, i.e.
/// anything outside `[A-Za-z0-9_]`) with `_`, so a tag like `ng-template` becomes `ng_template`
/// when used in a generated view-function name.
fn sanitize_identifier(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect()
}

/// The generated local identifier for a template local reference (`#user` → `$user_1$`). Angular's
/// `generate_variables.ts` emits a `Reference` view variable; its goldens spell the binding with the
/// `$name$` expect-emit placeholder. Wrapping the source name in `$…$` (with a unique counter so
/// distinct refs never collide) keeps the identifier valid while matching that convention.
fn local_ref_var_name(name: &str, counter: usize) -> String {
    format!("${}_{}$", sanitize_identifier(name), counter)
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

/// The i18n placeholder name for the `n`-th interpolation in a message: `INTERPOLATION` for the
/// first, then `INTERPOLATION_1`, `INTERPOLATION_2`, … — mirroring Angular's `PlaceholderRegistry`
/// (`i18n_parser.ts`: base name `INTERPOLATION`, de-duped with a numeric suffix).
fn i18n_interpolation_name(index: usize) -> String {
    if index == 0 {
        "INTERPOLATION".to_string()
    } else {
        format!("INTERPOLATION_{index}")
    }
}

/// The `original_code` template fragment for an i18n interpolation placeholder: the authored
/// `{{ <expr> }}` source. `get_msg_utils.ts` records the verbatim template text; the parser keeps
/// only the structured AST (spans are offset-only with no retained source), so the expression is
/// re-serialized from the AST. For the common reads/literals/operators this reproduces the authored
/// form (`{{result}}`, `{{value}}`); cases the reconstructor does not model fall back to an empty
/// inner expression rather than a wrong one.
fn i18n_original_code(node: &AstNode) -> String {
    format!("{{{{{}}}}}", ast_to_source(node))
}

/// Re-serialize a binding-expression AST back to its authored source for the common cases used by
/// i18n interpolations (implicit-receiver property reads, keyed reads, literals, binary / unary
/// operators, parenthesised chains). Unmodelled shapes return `""`.
fn ast_to_source(node: &AstNode) -> String {
    match &node.kind {
        AstExprKind::ImplicitReceiver | AstExprKind::ThisReceiver => String::new(),
        AstExprKind::PropertyRead { receiver, name, .. }
        | AstExprKind::SafePropertyRead { receiver, name, .. } => {
            let safe = matches!(node.kind, AstExprKind::SafePropertyRead { .. });
            let recv = ast_to_source(receiver);
            if recv.is_empty() {
                name.clone()
            } else if safe {
                format!("{recv}?.{name}")
            } else {
                format!("{recv}.{name}")
            }
        }
        AstExprKind::KeyedRead { receiver, key } => {
            format!("{}[{}]", ast_to_source(receiver), ast_to_source(key))
        }
        AstExprKind::SafeKeyedRead { receiver, key } => {
            format!("{}?.[{}]", ast_to_source(receiver), ast_to_source(key))
        }
        AstExprKind::LiteralPrimitive { value } => {
            use crate::expression::ast::LiteralValue as LV;
            match value {
                LV::Str(s) => format!("'{s}'"),
                LV::Num(n) => {
                    if n.fract() == 0.0 && n.is_finite() {
                        format!("{}", *n as i64)
                    } else {
                        n.to_string()
                    }
                }
                LV::Bool(b) => b.to_string(),
                LV::Null => "null".to_string(),
                LV::Undefined => "undefined".to_string(),
            }
        }
        AstExprKind::Binary { operation, left, right } => {
            format!(
                "{} {} {}",
                ast_to_source(left),
                operation.as_str(),
                ast_to_source(right)
            )
        }
        _ => String::new(),
    }
}

fn str_lit(s: &str) -> Expr {
    o::literal(o::LiteralValue::String(s.to_string()), None)
}

/// Angular `SelectorFlags` (`core.ts`) — flags interleaved into an `R3CssSelector` array to mark
/// negative `:not(...)` sub-selectors and class-matching mode.
mod selector_flags {
    pub const NOT: u32 = 0b0001;
    pub const ATTRIBUTE: u32 = 0b0010;
    pub const ELEMENT: u32 = 0b0100;
    pub const CLASS: u32 = 0b1000;
}

/// A single parsed `CssSelector` (Angular `directive_matching.ts` `CssSelector`).
#[derive(Default)]
struct CssSelectorParts {
    element: Option<String>,
    attrs: Vec<String>,
    class_names: Vec<String>,
    not_selectors: Vec<CssSelectorParts>,
}

/// `CssSelector.parse(selector)` — split a selector string into one or more `CssSelectorParts`
/// (comma-separated alternates), recognising tag names, `.class`, `#id` (→ `id` attribute),
/// `[attr]`/`[attr=value]` and `:not(...)` groups. A faithful port of Angular's `_SELECTOR_REGEXP`
/// scanner restricted to the forms the compliance corpus uses.
fn css_selector_parse(selector: &str) -> Vec<CssSelectorParts> {
    let mut results: Vec<CssSelectorParts> = Vec::new();
    let mut current_top = CssSelectorParts::default();
    let mut in_not = false;
    let chars: Vec<char> = selector.chars().collect();
    let mut i = 0usize;

    fn add_result(results: &mut Vec<CssSelectorParts>, mut sel: CssSelectorParts) {
        if !sel.not_selectors.is_empty()
            && sel.element.is_none()
            && sel.class_names.is_empty()
            && sel.attrs.is_empty()
        {
            sel.element = Some("*".to_string());
        }
        results.push(sel);
    }

    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == ':' && selector[char_to_byte_index(&chars, i)..].starts_with(":not(") {
            in_not = true;
            current_top.not_selectors.push(CssSelectorParts::default());
            i += 5;
            continue;
        }
        if c == ')' {
            in_not = false;
            i += 1;
            continue;
        }
        if c == ',' {
            let done = std::mem::take(&mut current_top);
            add_result(&mut results, done);
            i += 1;
            continue;
        }
        if c == '[' {
            let mut j = i + 1;
            let mut name = String::new();
            while j < chars.len() && chars[j] != ']' && chars[j] != '=' {
                name.push(chars[j]);
                j += 1;
            }
            let mut value = String::new();
            if j < chars.len() && chars[j] == '=' {
                j += 1;
                let quote = if j < chars.len() && (chars[j] == '"' || chars[j] == '\'') {
                    let q = chars[j];
                    j += 1;
                    Some(q)
                } else {
                    None
                };
                while j < chars.len() {
                    if let Some(q) = quote {
                        if chars[j] == q {
                            j += 1;
                            break;
                        }
                    } else if chars[j] == ']' {
                        break;
                    }
                    value.push(chars[j]);
                    j += 1;
                }
            }
            while j < chars.len() && chars[j] != ']' {
                j += 1;
            }
            j += 1; // consume `]`
            let target = css_selector_target(&mut current_top, in_not);
            target.attrs.push(css_unescape_attribute(&name));
            target.attrs.push(value.to_lowercase());
            i = j;
            continue;
        }
        if c == '.' || c == '#' || c == '*' || c == '-' || c == '_' || c.is_alphanumeric() {
            let prefix = if c == '.' || c == '#' { Some(c) } else { None };
            let mut j = if prefix.is_some() { i + 1 } else { i };
            let mut tok = String::new();
            while j < chars.len() {
                let d = chars[j];
                if d == '-' || d == '_' || d == '*' || d.is_alphanumeric() {
                    tok.push(d);
                    j += 1;
                } else {
                    break;
                }
            }
            let target = css_selector_target(&mut current_top, in_not);
            match prefix {
                Some('#') => {
                    target.attrs.push("id".to_string());
                    target.attrs.push(tok.to_lowercase());
                }
                Some('.') => target.class_names.push(tok.to_lowercase()),
                _ => target.element = Some(tok),
            }
            i = j;
            continue;
        }
        i += 1;
    }
    add_result(&mut results, current_top);
    results
}

/// Map a char index to a byte index in the original string (for `starts_with` on the slice).
fn char_to_byte_index(chars: &[char], char_idx: usize) -> usize {
    chars[..char_idx].iter().map(|c| c.len_utf8()).sum()
}

/// Resolve the `CssSelectorParts` the current token applies to: the in-progress `:not(...)`
/// sub-selector when inside one, else the top-level selector.
fn css_selector_target(top: &mut CssSelectorParts, in_not: bool) -> &mut CssSelectorParts {
    if in_not {
        top.not_selectors
            .last_mut()
            .expect("`:not(` opened a sub-selector")
    } else {
        top
    }
}

/// Angular `CssSelector.unescapeAttribute` (the subset that strips `\` escapes).
fn css_unescape_attribute(attr: &str) -> String {
    let mut out = String::new();
    for ch in attr.chars() {
        if ch == '\\' {
            continue;
        }
        out.push(ch);
    }
    out
}

/// `parserSelectorToSimpleSelector` — the positive part of one selector: `[element, ...attrs,
/// (CLASS, ...classNames)?]`. An element of `"*"` or absent becomes `""`.
fn simple_selector_exprs(sel: &CssSelectorParts) -> Vec<Expr> {
    let mut parts: Vec<Expr> = Vec::new();
    let element = match &sel.element {
        Some(e) if e != "*" => e.clone(),
        _ => String::new(),
    };
    parts.push(str_lit(&element));
    for a in &sel.attrs {
        parts.push(str_lit(a));
    }
    if !sel.class_names.is_empty() {
        parts.push(num(selector_flags::CLASS as f64));
        for c in &sel.class_names {
            parts.push(str_lit(c));
        }
    }
    parts
}

/// `parserSelectorToNegativeSelector` — a `:not(...)` sub-selector, flag-prefixed by its mode.
fn negative_selector_exprs(sel: &CssSelectorParts) -> Vec<Expr> {
    let mut parts: Vec<Expr> = Vec::new();
    if let Some(element) = &sel.element {
        parts.push(num((selector_flags::NOT | selector_flags::ELEMENT) as f64));
        parts.push(str_lit(element));
        for a in &sel.attrs {
            parts.push(str_lit(a));
        }
        if !sel.class_names.is_empty() {
            parts.push(num(selector_flags::CLASS as f64));
            for c in &sel.class_names {
                parts.push(str_lit(c));
            }
        }
    } else if !sel.attrs.is_empty() {
        parts.push(num((selector_flags::NOT | selector_flags::ATTRIBUTE) as f64));
        for a in &sel.attrs {
            parts.push(str_lit(a));
        }
        if !sel.class_names.is_empty() {
            parts.push(num(selector_flags::CLASS as f64));
            for c in &sel.class_names {
                parts.push(str_lit(c));
            }
        }
    } else if !sel.class_names.is_empty() {
        parts.push(num((selector_flags::NOT | selector_flags::CLASS) as f64));
        for c in &sel.class_names {
            parts.push(str_lit(c));
        }
    }
    parts
}

/// `parseSelectorToR3Selector(selector)` — the `R3CssSelectorList` literal for one selector string:
/// an array of per-alternate `R3CssSelector` arrays. E.g. `[spacer]` → `[["", "spacer", ""]]`,
/// `basic` → `[["basic"]]`.
fn parse_selector_to_r3_selector(selector: &str) -> Expr {
    let parsed = css_selector_parse(selector);
    let lists: Vec<Expr> = parsed
        .iter()
        .map(|sel| {
            let mut parts = simple_selector_exprs(sel);
            for neg in &sel.not_selectors {
                parts.extend(negative_selector_exprs(neg));
            }
            o::literal_arr(parts, None)
        })
        .collect();
    o::literal_arr(lists, None)
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

/// The leading numeric `slot` argument of a creation instruction (`ɵɵtext(slot, …)`,
/// `ɵɵdomElement(slot, …)`, `ɵɵconditionalCreate(slot, …)`, `ɵɵrepeaterCreate(slot, …)`, …), if it
/// has one. Used by [`pipe_insertion_index`] to find the create op a pipe should be inserted after.
fn creation_op_slot(stmt: &Stmt) -> Option<usize> {
    let (_callee, args) = as_call(stmt)?;
    match args.first()?.kind {
        o::ExprKind::Literal(o::LiteralValue::Number(n)) if n >= 0.0 => Some(n as usize),
        _ => None,
    }
}

/// Where to insert a `ɵɵpipe(...)` create op into `creation` for a pipe consumed by the create op at
/// `target_slot`. Faithful to Angular `pipe_creation.ts::addPipeToCreationBlock`: locate the create
/// op whose slot is `target_slot`, then skip past any `ɵɵpipe` ops already sitting after it, and
/// return the index of the first op that is NOT one of those pipes — i.e. the new pipe lands
/// immediately after the consuming op (after any earlier pipes for the same op) and before the next
/// non-pipe op (e.g. the element's `ɵɵdomElementEnd`). Returns `None` when no matching op is found,
/// so the caller can fall back to appending.
fn pipe_insertion_index(creation: &[Stmt], target_slot: usize) -> Option<usize> {
    let consumer = creation
        .iter()
        .position(|stmt| creation_op_slot(stmt) == Some(target_slot))?;
    let mut idx = consumer + 1;
    while idx < creation.len()
        && as_call(&creation[idx]).and_then(|(callee, _)| callee_wire_name(callee))
            == Some(R3::Pipe.name())
    {
        idx += 1;
    }
    Some(idx)
}

/// DOM-property name remapping applied by `reifyDomProperty` (`reify.ts` `DOM_PROPERTY_REMAPPING`):
/// in DomOnly mode a handful of attribute-style binding names map to their DOM-property spelling
/// (`class` → `className`, `for` → `htmlFor`, …). Names not in the table pass through unchanged.
fn remap_dom_property(name: &str) -> &str {
    match name {
        "class" => "className",
        "for" => "htmlFor",
        "formaction" => "formAction",
        "innerHtml" => "innerHTML",
        "readonly" => "readOnly",
        "tabindex" => "tabIndex",
        other => other,
    }
}

/// Whether `name` is an ARIA attribute name (`isAriaAttribute`, `util/attributes.ts`): begins with
/// `aria-` and is longer than the prefix. In Full mode an `[aria-*]` property binding reifies to
/// `ɵɵariaProperty` instead of `ɵɵproperty`.
fn is_aria_attribute(name: &str) -> bool {
    const ARIA_PREFIX: &str = "aria-";
    name.starts_with(ARIA_PREFIX) && name.len() > ARIA_PREFIX.len()
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

/// The continuation callee a chained run keyed on `first`'s instruction expects, mirroring Angular's
/// `CHAIN_COMPATIBILITY` map (`phases/chaining.ts`). Returns the wire name (`ɵɵ…`) that may be
/// appended onto a run whose *first* call uses `first`, or `None` when `first` is NOT a chainable
/// instruction (absent from the map — e.g. `ɵɵprojection`, `ɵɵpipe`, `ɵɵtemplate`-anchor-less ops).
///
/// Only instructions in this map chain; every other call breaks the run and stays a standalone
/// statement. Almost every chainable instruction continues with *itself*; the two exceptions are the
/// conditional-create pair (`conditionalCreate` → `conditionalBranchCreate`, then
/// `conditionalBranchCreate` → itself).
fn chain_continuation(first: &Expr) -> Option<&'static str> {
    let name = callee_wire_name(first)?;
    // Self-continuing chainable instructions (the bulk of CHAIN_COMPATIBILITY).
    const SELF_CHAIN: &[R3] = &[
        R3::AriaProperty,
        R3::Attribute,
        R3::ClassProp,
        R3::Element,
        R3::ElementContainer,
        R3::ElementContainerEnd,
        R3::ElementContainerStart,
        R3::ElementEnd,
        R3::ElementStart,
        R3::DomProperty,
        R3::I18nExp,
        R3::Listener,
        R3::Property,
        R3::StyleProp,
        R3::SyntheticHostListener,
        R3::SyntheticHostProperty,
        R3::TemplateCreate,
        R3::TwoWayProperty,
        R3::TwoWayListener,
        R3::DeclareLet,
        R3::DomElement,
        R3::DomElementStart,
        R3::DomElementEnd,
        R3::DomElementContainer,
        R3::DomElementContainerStart,
        R3::DomElementContainerEnd,
        R3::DomListener,
        R3::DomTemplate,
        R3::AnimationEnter,
        R3::AnimationLeave,
        R3::AnimationEnterListener,
        R3::AnimationLeaveListener,
    ];
    if name == R3::ConditionalCreate.name() {
        // `conditionalCreate` run absorbs subsequent `conditionalBranchCreate` calls.
        return Some(R3::ConditionalBranchCreate.name());
    }
    if name == R3::ConditionalBranchCreate.name() {
        return Some(R3::ConditionalBranchCreate.name());
    }
    if let Some(r) = SELF_CHAIN.iter().find(|r| r.name() == name) {
        return Some(r.name());
    }
    None
}

/// Whether a call with callee `next` may be chained onto a run whose *first* call's callee is
/// `first`. Faithful to Angular's `chainOperationsInList`: `first` must be a chainable instruction
/// (present in `CHAIN_COMPATIBILITY`) and `next`'s callee must equal the map's continuation for
/// `first`.
fn chains_onto(first: &Expr, next: &Expr) -> bool {
    match (chain_continuation(first), callee_wire_name(next)) {
        (Some(cont), Some(next_name)) => cont == next_name,
        _ => false,
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
    /// `@let` declarations (this view's + every ancestor's) that reserved a `ɵɵdeclareLet` slot and
    /// are thus readable cross-view via `ɵɵreadContextLet`. Threaded into each embedded view +
    /// listener handler. When a binding in this view (or a callback) reads a name found here that is
    /// NOT satisfied by a local of the current view, the read lowers to a generated local fed by a
    /// prepended `const <name>_r = ɵɵreadContextLet(slot)` (with the appropriate `ɵɵnextContext`
    /// hops). Mirrors `generate_variables.ts` `Scope.letDeclarations`.
    context_lets: Vec<ContextLet>,
    /// The set of `@let` names declared at this view's top level that need cross-view storage
    /// (external or pipe-bearing). Populated by [`Self::analyze_let_declarations`] before the walk;
    /// consulted in [`Self::build_let_declaration`]. The boolean is `true` when the let is external
    /// (storeLet retained), `false` when it only kept its `ɵɵdeclareLet` because of a pipe.
    external_lets: std::collections::HashMap<String, bool>,
    /// This view's top-level node list, captured for the duration of the walk so a `@let`'s
    /// in-view-usage decision can consult its (shadowing-aware) sibling/descendant references.
    current_nodes: Option<Vec<Node>>,
    /// The shared `const _r<id> = ɵɵgetCurrentView();` identifier for this view, minted the first
    /// time a listener handler needs to restore a saved view (because it reads a cross-view `@let`).
    /// `generate_variables.ts` saves the current view once per view and reuses it across listeners.
    saved_view_name: Option<String>,
    /// `const <item>_r<id> = ctx.$implicit;` declarations to emit at the head of this view's update
    /// block (generated for the loop variables this view actually references).
    update_prelude: Vec<Stmt>,
    /// Hoisted, named `@if`/`@for`/`@switch` branch/loop template functions collected while walking
    /// this view (and its descendants). Emitted as leading `DeclareFunction` statements of the
    /// produced view function, mirroring how Angular hoists `ConditionalCreate`/`RepeaterCreate`
    /// template fns onto the const pool (`emit.ts` `emitChildViews` → `pool.statements`).
    hoisted_fns: Vec<Stmt>,
    /// Shared constant-pool entries hoisted to the top level as `const $cN$ = <value>;` declarations
    /// (Angular `ConstantPool.getConstLiteral` → `pool.statements`). Distinct from the per-template
    /// `consts:` array: these are the de-duped, view-global literals an instruction references BY NAME
    /// (e.g. `ɵɵprojectionDef($c0$)` parsed-selector arrays). Each entry is `(value_expr, name)`; a
    /// structurally-equivalent value reuses its existing name (Angular returns the SAME reference).
    /// Surfaced alongside [`Self::hoisted_functions`] so they print before the `ɵɵdefineComponent`.
    shared_consts: Vec<(Expr, String)>,
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
    /// Template local references (`#user`) declared on elements/templates in THIS view, in
    /// declaration order. A read of one of these names in a same-view update binding lowers to a
    /// generated `const $name$ = ɵɵreference(slot)` local; reads in a listener handler likewise read
    /// `ɵɵreference(slot)` (with view save/restore scaffolding). Mirrors `generate_variables.ts`
    /// `Reference`.
    local_refs: Vec<LocalRef>,
    /// The names of local refs THIS view's own update bindings actually read, recorded during
    /// expression lowering (the resolver borrows `&self`, so this is interior-mutable). After the
    /// walk, each is materialised once as a leading `const <name>_r<id> = ɵɵreference(slot)` in the
    /// update block (`optimizeVariables` keeps only referenced ones). Stored as
    /// `(source_name, local_name, slot)` in first-use order.
    used_local_refs: std::cell::RefCell<Vec<(String, String, usize)>>,
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
    /// The data slot of the creation op whose bindings are currently being lowered (the
    /// text/element/anchor a `lower_expr` call binds into). Pushed into [`PipeState`] by
    /// [`Self::lower_expr`] so a pipe usage records its consuming op (`addPipeToCreationBlock`).
    current_target_slot: usize,
    /// The *specific* (non-wildcard) selectors of every `<ng-content select="…">` slot reached in
    /// this view, in first-appearance order (Angular TDB `_ngContentReservedSlots`, minus the
    /// implicit wildcard). A selector's `projectionSlotIndex` is `1 + its position` here — index `0`
    /// is reserved for the wildcard catch-all. When this list (or [`Self::has_default_projection`])
    /// is non-empty, [`Self::build_template_function`] prepends a `ɵɵprojectionDef(...)` to the
    /// creation block.
    ng_content_selectors: Vec<String>,
    /// Every `<ng-content>` projection slot's raw selector in create-block order, INCLUDING
    /// duplicates and the wildcard `"*"` (Angular `generateProjectionDefs` `selectors.push`). Drives
    /// the `ɵɵprojectionDef(...)` argument: when `selectors.len() > 1 || selectors[0] != "*"`, each
    /// entry maps `"*"` → `"*"` else `parseSelectorToR3Selector(s)`, interned as one const.
    all_projection_selectors: Vec<String>,
    /// Whether a catch-all (`<ng-content>` / `select="*"`) projection slot was reached. Drives
    /// whether the prepended `ɵɵprojectionDef(...)` needs to be emitted in the no-specific-selector
    /// case.
    has_default_projection: bool,
    /// The number of `<ng-content>` projection slots reached so far in this view. Angular's
    /// `generateProjectionDefs` assigns every projection op a *unique ascending* `projectionSlotIndex`
    /// (`op.projectionSlotIndex = projectionSlotIndex++`) — independent of selector dedup — so the
    /// N-th `<ng-content>` (0-based) carries index N (`ɵɵprojection(slot, N)`, the `0` case elided).
    projection_count: usize,
    /// Compilation mode for THIS view: `true` → Angular's `DomOnly` instruction family
    /// (`ɵɵdomElement*`/`ɵɵdomListener`/`ɵɵdomProperty`), `false` → the `Full` family
    /// (`ɵɵelement*`/`ɵɵlistener`/`ɵɵproperty`). Set from [`TemplateCompilationInput::dom_only`] and
    /// inherited unchanged by every embedded view ([`Self::build_embedded_view`]), since the mode is a
    /// whole-component property (`render3/view/compiler.ts` selects it once per component).
    dom_only: bool,
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
            context_lets: Vec::new(),
            external_lets: std::collections::HashMap::new(),
            current_nodes: None,
            saved_view_name: None,
            update_prelude: Vec::new(),
            hoisted_fns: Vec::new(),
            shared_consts: Vec::new(),
            base_name,
            var_counter: 0,
            is_root: true,
            view_level: 0,
            temp_counter: 0,
            local_refs: Vec::new(),
            used_local_refs: std::cell::RefCell::new(Vec::new()),
            needs_next_context: std::cell::Cell::new(false),
            next_context_name: std::cell::RefCell::new(None),
            pipes: std::cell::RefCell::new(PipeState::default()),
            current_target_slot: 0,
            ng_content_selectors: Vec::new(),
            all_projection_selectors: Vec::new(),
            has_default_projection: false,
            projection_count: 0,
            dom_only: input.dom_only,
        }
    }

    /// The hoisted, named branch/loop template functions collected during
    /// [`Self::build_template_function`] (`DeclareFunction` statements). These are emitted as
    /// leading statements of the produced view function; callers assembling the component
    /// definition may also surface them at the top level (`pool.statements`).
    pub fn hoisted_functions(&self) -> &[Stmt] {
        &self.hoisted_fns
    }

    /// Intern a literal into the SHARED constant pool (Angular `ConstantPool.getConstLiteral`),
    /// returning the `$cN$` reference name an instruction should carry. De-dupes structurally
    /// equivalent values (returns the existing name) and, on first sight of a value, mints the next
    /// `$cN$` name and emits a top-level `const $cN$ = <value>;` declaration onto `hoisted_fns` so it
    /// prints before the `ɵɵdefineComponent` call (the shared-const namespace shares the view-global
    /// `next_const_name` counter with the pure-literal factories, exactly as Angular's single pool does).
    fn intern_shared_const(&mut self, value: Expr) -> String {
        if let Some((_, name)) = self.shared_consts.iter().find(|(v, _)| v.is_equivalent(&value)) {
            return name.clone();
        }
        let n = {
            let mut state = self.pipes.borrow_mut();
            let n = state.next_const_name;
            state.next_const_name = n + 1;
            n
        };
        let name = format!("$c{n}$");
        self.shared_consts.push((value.clone(), name.clone()));
        self.hoisted_fns.push(Stmt::with_modifiers(
            StmtKind::DeclareVar {
                name: name.clone(),
                value: Some(value),
                ty: None,
            },
            StmtModifier::FINAL,
        ));
        name
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
        // Publish the consuming op's data slot so any pipe reached here records it (Angular
        // `addPipeToCreationBlock` keys the create-op insertion point on the owning update op's
        // `target`). Var (change-detection) offsets for pipes / arrows / pure functions are NOT
        // assigned here — they are deferred to `finalize_var_offsets`, which runs Angular's two-pass
        // per-view assignment after every top-level op has reserved its own var slots.
        {
            let mut pipes = self.pipes.borrow_mut();
            pipes.current_target_slot = self.current_target_slot;
        }

        // Embedded (nested) views resolve ancestor-context reads via `ɵɵnextContext()` (`ctx_r<level>`),
        // recording the need in `needs_next_context`; loop locals still resolve to their generated
        // names. The root view roots everything at `ctx`.
        let converted = if self.view_level > 0 {
            let ctx_name = self.next_context_var_name();
            let resolver = NestedViewResolver {
                vars: &self.loop_vars,
                ctx_name,
                needs: &self.needs_next_context,
                local_refs: &self.local_refs,
                used_local_refs: &self.used_local_refs,
            };
            convert_property_binding_with_pipes(node, &resolver, &BuilderPipes { state: &self.pipes })
        } else {
            let resolver = LoopVarResolver {
                vars: &self.loop_vars,
                local_refs: &self.local_refs,
                used_local_refs: &self.used_local_refs,
            };
            convert_property_binding_with_pipes(node, &resolver, &BuilderPipes { state: &self.pipes })
        };

        // Binding-lowering corner case (Angular `convertPropertyBinding` `stmts` + `convertActionBinding`):
        // some expressions spill temporary `let`/guard statements that must run BEFORE the binding
        // instruction consumes the value (e.g. a safe-navigation chain whose lowering allocates a
        // temporary, or a chained sub-expression). Those statements are emitted into the update buffer
        // here, immediately ahead of the instruction the caller is about to push, so the temporary is
        // materialised first. (For the node kinds whose safe-navigation lowering is inlined as a
        // ternary, `stmts` is empty and nothing is emitted.)
        for stmt in converted.stmts {
            self.update_code.push(stmt);
        }
        converted.expr
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

    /// The shared saved-view identifier for this view, minted on first use. The matching
    /// `const $s_<id>$ = ɵɵgetCurrentView();` is prepended to the creation block during finalisation.
    /// Angular's `generate_variables.ts` emits a `SavedView` view variable; its goldens spell it with
    /// the `$name$` expect-emit placeholder, so the generated identifier wraps in `$…$` to match.
    fn saved_view_var_name(&mut self) -> String {
        if let Some(name) = &self.saved_view_name {
            return name.clone();
        }
        self.var_counter += 1;
        let name = format!("$s_{}$", self.var_counter);
        self.saved_view_name = Some(name.clone());
        name
    }

    /// Register a template local reference (`#name`) at its `ɵɵreference(slot)` data slot. The
    /// pre-pass (`preregister_local_ref`) already minted the generated local + a placeholder slot;
    /// fill in the real slot now that the host is reached (patch the first still-placeholder entry of
    /// this name). Falls back to pushing a fresh entry for a ref the pre-pass did not see.
    fn register_local_ref(&mut self, name: &str, slot: usize) {
        if let Some(entry) = self
            .local_refs
            .iter_mut()
            .find(|r| r.name == name && r.slot >= LOCAL_REF_SLOT_PLACEHOLDER)
        {
            entry.slot = slot;
        } else {
            self.var_counter += 1;
            let local_name = local_ref_var_name(name, self.var_counter);
            self.local_refs.push(LocalRef {
                name: name.to_string(),
                slot,
                local_name,
            });
        }
    }

    /// Pre-register a local ref by name during the const pre-pass, minting its generated local
    /// identifier and a placeholder slot. The placeholder lets a listener handler that reads a ref
    /// declared LATER emit `ɵɵreference(placeholder)`; the real slot is filled by
    /// [`Self::register_local_ref`] when the host is reached, and patched into handler bodies by
    /// [`Self::finalize_local_refs`].
    fn preregister_local_ref(&mut self, name: &str) {
        let ordinal = self.local_refs.len();
        self.var_counter += 1;
        let local_name = local_ref_var_name(name, self.var_counter);
        self.local_refs.push(LocalRef {
            name: name.to_string(),
            slot: LOCAL_REF_SLOT_PLACEHOLDER + ordinal,
            local_name,
        });
    }

    /// Patch every placeholder local-ref slot literal (`LOCAL_REF_SLOT_PLACEHOLDER + ordinal`) in the
    /// creation block (listener handler bodies) to the real `ɵɵreference(slot)` slot recorded for that
    /// ordinal in `self.local_refs`. Needed when a handler reads a `#ref` declared after it.
    fn finalize_local_refs(&mut self) {
        let slots: Vec<usize> = self.local_refs.iter().map(|r| r.slot).collect();
        if slots.is_empty() {
            return;
        }
        let resolve = move |ordinal: usize| slots.get(ordinal).copied().unwrap_or(ordinal);
        let mut creation = std::mem::take(&mut self.creation_code);
        for stmt in &mut creation {
            remap_placeholder_slots_in_stmt(stmt, LOCAL_REF_SLOT_PLACEHOLDER, &resolve);
        }
        self.creation_code = creation;
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

    /// Pre-assign real, positional data slots to any pipes registered for the consuming op at
    /// `target_slot` that have not yet been slotted, allocating a fresh data slot per pipe right now
    /// (so subsequent nodes' slots shift up). Used for a `@let … = x | pipe` value whose consuming
    /// `ɵɵdeclareLet` precedes later ops — Angular's in-order slot allocation places the `ɵɵpipe`
    /// immediately after the `ɵɵdeclareLet`. Pipes sharing the op are slotted in registration order.
    fn assign_positional_pipe_slots_for(&mut self, target_slot: usize) {
        // Collect the ordinals of not-yet-slotted pipes for this op (immutable borrow first).
        let ordinals: Vec<usize> = {
            let state = self.pipes.borrow();
            state
                .pending
                .iter()
                .enumerate()
                .filter(|(_, p)| p.target_slot == target_slot && p.real_slot.is_none())
                .map(|(i, _)| i)
                .collect()
        };
        for ordinal in ordinals {
            let real = self.allocate_data_slot();
            self.pipes.borrow_mut().pending[ordinal].real_slot = Some(real);
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
        // Resolve each pipe's real data slot. A pipe whose consuming op was NOT the last data op got
        // a slot pre-assigned *positionally* during the walk ([`PendingPipe::real_slot`], e.g. a pipe
        // in a `@let … = x | pipe` value — the `ɵɵpipe` lands right after the `ɵɵdeclareLet`, ahead of
        // later text/element slots). Every other pipe (the common leaf case) is allocated at the END
        // of the data array, in registration order, which coincides with its positional slot there.
        let mut slots = vec![0usize; pending.len()];
        let mut pipe_base = self.data_index;
        for (ordinal, pending_pipe) in pending.iter().enumerate() {
            if let Some(real) = pending_pipe.real_slot {
                slots[ordinal] = real;
            } else {
                slots[ordinal] = pipe_base;
                pipe_base += 1;
            }
        }
        self.data_index = pipe_base;

        // Insert each `ɵɵpipe(slot, "name")` create op into the creation block at the position
        // Angular's `addPipeToCreationBlock` picks: immediately after the create op that *consumes*
        // it (the text/element/anchor the owning update binding targets), skipping past any pipe ops
        // already inserted after that op. This places the `ɵɵpipe(...)` inside the consuming element's
        // creation block (e.g. between `ɵɵtext(1)` and the element's `ɵɵdomElementEnd()`) rather than
        // appended after the whole creation buffer. Pipes are processed in source/ordinal order, so a
        // later pipe sharing the same consuming op chains after an earlier one. The subsequent
        // `chain_statements` pass folds any resulting run of adjacent `ɵɵpipe` calls.
        let mut creation = std::mem::take(&mut self.creation_code);
        for (ordinal, pending_pipe) in pending.iter().enumerate() {
            let slot = slots[ordinal];
            let pipe_op = instruction(
                R3::Pipe,
                vec![num(slot as f64), str_lit(&pending_pipe.name)],
            );
            let insert_at =
                pipe_insertion_index(&creation, pending_pipe.target_slot).unwrap_or(creation.len());
            creation.insert(insert_at, pipe_op);
        }
        self.creation_code = creation;

        // Patch placeholder slot literals in the update block to their real slots (per-ordinal map).
        let mut update = std::mem::take(&mut self.update_code);
        for stmt in &mut update {
            remap_pipe_slots_in_stmt(stmt, &slots);
        }
        self.update_code = update;
    }

    /// Assign the deferred var (change-detection) offsets every pipe / arrow / pure-function consumer
    /// reserved during the walk, faithful to Angular's two-pass `countVariables` (`var_counting.ts`):
    ///
    /// 1. Every top-level op has already reserved its own var slots in [`Self::binding_slots`] (via
    ///    [`Self::allocate_binding_slots`], counted as the op was emitted) — this is the base offset.
    /// 2. FIRST pass: walk the recorded consumers in lowering (post-order) order and assign offsets to
    ///    every pipe and arrow (`hasUsesVarOffsetTrait`, skipping pure functions), advancing the cursor
    ///    by each consumer's slot count.
    /// 3. SECOND pass: walk the recorded consumers again, assigning offsets to the pure functions that
    ///    were skipped (the TDB assigns pure-function offsets lazily, *after* every other binding).
    ///
    /// The cursor's final value is the view's total var count ([`Self::binding_slots`] is updated to
    /// it, so [`Self::vars`] reports the full total). Each consumer's instruction was emitted with a
    /// `VAR_OFFSET_PLACEHOLDER + ordinal` offset literal; here we build the ordinal→offset map and
    /// patch the update block.
    fn finalize_var_offsets(&mut self) {
        let consumers = std::mem::take(&mut self.pipes.borrow_mut().var_consumers);
        if consumers.is_empty() {
            return;
        }
        // Base = every top-level op's reserved var slots (already in `binding_slots`).
        let mut cursor = self.binding_slots;
        let mut offsets = vec![0usize; consumers.len()];

        // First pass: pipes, arrows, and storeLets, in recorded (post-order) order. (StoreLet consumes
        // a slot but emits no offset argument, so its `offsets[ordinal]` is never read — it only
        // advances the cursor, after any arrow/pure its value contained.)
        for (ordinal, c) in consumers.iter().enumerate() {
            if matches!(
                c.kind,
                VarConsumerKind::Pipe | VarConsumerKind::Arrow | VarConsumerKind::StoreLet
            ) {
                offsets[ordinal] = cursor;
                cursor += c.slots;
            }
        }
        // Second pass: pure functions, in recorded order (assigned only after every pipe/arrow).
        for (ordinal, c) in consumers.iter().enumerate() {
            if matches!(c.kind, VarConsumerKind::Pure) {
                offsets[ordinal] = cursor;
                cursor += c.slots;
            }
        }

        // The view's full var total now includes every deferred consumer.
        self.binding_slots = cursor;

        // Patch every `VAR_OFFSET_PLACEHOLDER + ordinal` offset literal to its assigned offset.
        let mut update = std::mem::take(&mut self.update_code);
        for stmt in &mut update {
            remap_placeholder_slots_in_stmt(stmt, VAR_OFFSET_PLACEHOLDER, &|ordinal| offsets[ordinal]);
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

        // Capture the node list for in-view `@let`-usage queries during the walk.
        self.current_nodes = Some(nodes.clone());

        // Angular runs `liftLocalRefs` (which interns each element/template's `#ref` const) BEFORE
        // `collectElementConsts` (the attribute consts) — so EVERY local-ref const precedes every
        // attribute const in the component `consts` pool. Pre-intern them here, in create-op
        // (depth-first pre-order) order, so the attribute consts the main walk interns land after.
        // `intern` dedups, so the walk's own `local_refs_index` call returns the same index. This
        // also pre-registers each ref (name + generated local + placeholder slot) so a listener whose
        // handler reads a ref declared LATER can resolve it.
        self.prepass_local_ref_consts(&nodes);

        // Classify every top-level `@let` in this view (external / pipe-bearing) so the inline walk
        // can decide whether each reserves a `ɵɵdeclareLet` slot + `ɵɵstoreLet`, or inlines as a
        // plain `const`/bare statement (`optimizeStoreLet` / `optimizeVariables`).
        self.analyze_let_declarations(&nodes);

        // Bring any ancestor `@let`s that THIS view's own update bindings read into scope as
        // generated locals, fed by prepended `ɵɵnextContext()` + `const … = ɵɵreadContextLet(slot)`
        // declarations (`generate_variables.ts`). Must run before the walk so reads of those names
        // resolve to the generated local rather than `ctx.<name>`.
        self.bring_ancestor_lets_into_scope(&nodes);

        self.visit_all(&nodes);

        // Assign the deferred var (change-detection) offsets for every pipe / arrow / pure-function
        // consumer — Angular's two-pass `var_counting`. Runs after the whole view is walked (so every
        // op has reserved its var slots). Resolves the `VAR_OFFSET_PLACEHOLDER` (3e9) offset literals
        // to real small offsets BEFORE `finalize_pipes` runs, so the pipe-data-slot remap (which maps
        // every literal `>= PIPE_SLOT_PLACEHOLDER` = 1e9) never mistakes a var-offset placeholder for a
        // pipe data slot. Updates `binding_slots` to the full view var total.
        self.finalize_var_offsets();

        // Allocate pipe data slots (at the END of the data array), emit their `ɵɵpipe(slot,"name")`
        // creation instructions, and patch the placeholder slots in the update block.
        self.finalize_pipes();

        // Patch any placeholder `ɵɵreference(LOCAL_REF_SLOT_PLACEHOLDER + ordinal)` a listener
        // handler emitted for a `#ref` declared LATER in the template (the host's real slot is now
        // known). Same-view update reads were materialised against the real slot already.
        self.finalize_local_refs();

        // When a listener handler in this view restored a saved view (to read a cross-view `@let`),
        // the creation block opens with `const _r<id> = ɵɵgetCurrentView();` (`generate_variables.ts`
        // saves the view once per view, ahead of every listener that consumes it).
        if let Some(saved) = self.saved_view_name.clone() {
            self.creation_code.insert(
                0,
                Stmt::with_modifiers(
                    StmtKind::DeclareVar {
                        name: saved,
                        value: Some(
                            o::import_expr(R3::GetCurrentView.reference(), None)
                                .call_fn(vec![], false),
                        ),
                        ty: None,
                    },
                    StmtModifier::FINAL,
                ),
            );
        }

        let mut statements: Vec<Stmt> = Vec::new();

        // `<ng-content>` projection: when any projection slot was reached, the creation block opens
        // with a single `ɵɵprojectionDef(...)` (Angular TDB `buildTemplateFunction` prepends it from
        // `_ngContentReservedSlots`). The common single default-slot case (`<ng-content>` with no
        // `select`) elides the argument entirely (`ɵɵprojectionDef()`). When specific selectors are
        // present they are interned as a literal string array in the const pool and that const index
        // is passed (the precise `parseSelectorToR3Selector` encoding is a larger subsystem — see the
        // module's i18n/selector scope notes).
        if !self.all_projection_selectors.is_empty() {
            // Angular `generateProjectionDefs`: the argument is elided when there is exactly one slot
            // and it is the wildcard (`selectors.length === 1 && selectors[0] === "*"`). Otherwise the
            // selectors array is built in slot order — `"*"` stays `"*"`, every specific selector is
            // mapped through `parseSelectorToR3Selector(s)` — and interned as one const.
            let selectors = std::mem::take(&mut self.all_projection_selectors);
            let elide_arg = selectors.len() == 1 && selectors[0] == "*";
            let params = if elide_arg {
                vec![]
            } else {
                let entries: Vec<Expr> = selectors
                    .iter()
                    .map(|s| {
                        if s == "*" {
                            str_lit("*")
                        } else {
                            parse_selector_to_r3_selector(s)
                        }
                    })
                    .collect();
                // Angular `generateProjectionDefs` hoists the parsed selector array into the SHARED
                // constant pool (`constantPool.getConstLiteral(asLiteral(parsed), true)` → a top-level
                // `const $cN$ = [...]`) and passes THAT reference — never the per-template `consts:`
                // index. Mint a `$cN$` shared-const name, emit the top-level declaration as a hoisted
                // pool statement, and reference it by name.
                let arr = o::literal_arr(entries, None);
                let name = self.intern_shared_const(arr);
                vec![o::variable(name, None)]
            };
            self.creation_code
                .insert(0, instruction(R3::ProjectionDef, params));
        }

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
        // Template local references read by this view's own update bindings materialise as
        // `const <name>_r<id> = ɵɵreference(slot)` at the head of the variable block, before the
        // bindings consume them (`generate_variables.ts` `Reference`). Emitted in first-use order.
        let used_refs = std::mem::take(&mut *self.used_local_refs.borrow_mut());
        for (name, local_name, slot) in used_refs {
            // A `#ref` read by an update binding declared BEFORE the ref's host (a forward
            // reference — e.g. `@let m = name.value` above `<input #name>`) was recorded with the
            // pre-pass placeholder slot. By now the host has been reached and
            // [`Self::register_local_ref`] has filled the real `ɵɵreference(slot)` slot into
            // `self.local_refs`; re-resolve from there so the materialised const uses the real slot.
            let slot = self
                .local_refs
                .iter()
                .find(|r| r.name == name && r.slot < LOCAL_REF_SLOT_PLACEHOLDER)
                .map(|r| r.slot)
                .unwrap_or(slot);
            update_body.push(Stmt::with_modifiers(
                StmtKind::DeclareVar {
                    name: local_name,
                    value: Some(
                        o::import_expr(R3::Reference.reference(), None)
                            .call_fn(vec![num(slot as f64)], false),
                    ),
                    ty: None,
                },
                StmtModifier::FINAL,
            ));
        }
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

        // Angular hoists every nested branch/loop/template view function onto
        // `ConstantPool.statements` — emitted as top-level sibling `function …_Template(…){…}`
        // declarations OUTSIDE (and before) the `ɵɵdefineComponent({…})` call, NEVER inside the
        // root view body. The component-definition harness (and Angular's own goldens) extract only
        // the `ɵɵdefineComponent` block, so the root template function must begin directly with
        // `if (rf & 1) {…}` and close immediately after `if (rf & 2) {…}` — no leading or trailing
        // function declarations inside it. The collected hoisted functions are therefore left OUT
        // of the returned view-function body entirely; callers surface them at the true top level
        // via [`Self::hoisted_functions`] (`ConstantPool.statements`). A child view's hoisted fns
        // bubble up to the root via [`Self::build_embedded_view`], so the whole nested-view tree is
        // collected once on the root builder's `hoisted_fns`.
        let body: Vec<Stmt> = statements;

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

    /// Lower an `<ng-content>` projection slot (`Content`). Faithful to the classic TDB
    /// `visitContent`:
    ///
    /// - allocates one data slot (the projection anchor TNode);
    /// - records the slot's selector — index `0` is reserved for the wildcard catch-all and specific
    ///   selectors are numbered 1-based in first-appearance order (Angular `getProjectionSlotIndex`);
    /// - emits `ɵɵprojection(slot[, projectionSlotIndex[, attrsIndex]])` into the creation block,
    ///   trimming a trailing `null` attrs arg and a default projectionSlotIndex of `0`.
    ///
    /// The matching `ɵɵprojectionDef(...)` is prepended once to the creation block by
    /// [`Self::build_template_function`] when any projection slot was reached.
    fn build_content(&mut self, content: &Content) {
        let slot = self.allocate_data_slot();

        let selector = if content.selector.is_empty() {
            "*".to_string()
        } else {
            content.selector.clone()
        };
        let is_default = selector == "*";
        // Angular's `generateProjectionDefs` assigns each `<ng-content>` op a *unique ascending*
        // `projectionSlotIndex` (0-based, in create-block order), regardless of selector dedup.
        let projection_index = self.projection_count;
        self.projection_count += 1;
        // Record EVERY slot's selector in create order (with dups + wildcards) for the
        // `ɵɵprojectionDef` argument (Angular `generateProjectionDefs`).
        self.all_projection_selectors.push(selector.clone());
        if is_default {
            self.has_default_projection = true;
        } else if !self.ng_content_selectors.iter().any(|s| s == &selector) {
            // The projectionDef/ngContentSelectors selector list (a separate const-pool subsystem)
            // records each specific selector once, in first-appearance order.
            self.ng_content_selectors.push(selector.clone());
        }

        // Static attributes on the `<ng-content>` (e.g. `class="x"`) are interned like an element's.
        let attrs_index = self.element_attrs_index(&content.attributes, &[]);

        // `ɵɵprojection(slot, projectionSlotIndex, attrs)` — trailing `null` attrs are trimmed, and a
        // default projectionSlotIndex of `0` is elided.
        let mut params = vec![
            num(slot as f64),
            num(projection_index as f64),
            attrs_index.map(|i| num(i as f64)).unwrap_or_else(o::null_expr),
        ];
        trim_trailing_nulls(&mut params);
        if params.len() == 2 && params[1].is_equivalent(&num(0.0)) {
            params.pop();
        }
        self.creation_code.push(instruction(R3::Projection, params));
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
        // The consuming op for any pipe in this interpolation is the text node itself; the
        // `ɵɵpipe(...)` create op is inserted right after this text op (Angular `addPipeToCreationBlock`).
        self.current_target_slot = slot;
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

        // Any pipe consumed by this text node takes the data slot immediately after it (Angular
        // `slot_allocation.ts` walks the final create-op list in order, and `ɵɵpipe` ops sit right
        // after their consuming op). Assign those slots NOW — before the next sibling allocates its
        // slot — so the layout is `text(N), pipe(N+1), pipe(N+2), <next op>(N+3)` and the
        // `ɵɵadvance` counts that follow are correct.
        self.assign_positional_pipe_slots_for(slot);
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
        self.build_element_inner(element)
    }

    /// The value expression for a `[style.x]`/`[class.x]` binding. A literal interpolation value
    /// (`"a{{exp}}b"`) is wrapped in the matching `ɵɵinterpolateN(...)` (Angular `interpolate.ts`
    /// reifies `StyleProp`/`ClassProp` interpolation through the value-interpolation family); a
    /// non-interpolation value is passed through as already lowered.
    fn style_class_binding_value(&mut self, value: &AstNode, already_lowered: Expr) -> Expr {
        match &value.kind {
            AstExprKind::Interpolation { strings, expressions } => {
                // Lower each interpolation expression against this view's scope, in order, then
                // collate them with the literal string affixes into the `ɵɵinterpolateN` arg list.
                let lowered: Vec<Expr> = expressions.iter().map(|e| self.lower_expr(e)).collect();
                let (reference, args) =
                    value_interpolation_call(strings, &lowered);
                o::import_expr(reference.reference(), None).call_fn(args, false)
            }
            _ => already_lowered,
        }
    }

    /// The property-binding instruction for this view's compilation mode: `ɵɵdomProperty` in
    /// DomOnly mode, `ɵɵproperty` in Full mode (`reify.ts` `reifyDomProperty`/`reifyProperty`).
    fn property_reference(&self) -> R3 {
        if self.dom_only {
            R3::DomProperty
        } else {
            R3::Property
        }
    }

    fn build_element_inner(&mut self, element: &Element) {
        let slot = self.allocate_data_slot();
        // Each `#ref` on the element reserves one extra data slot (Angular `liftLocalRefs`:
        // `numSlotsUsed += localRefs.length`). The `ɵɵreference(slot)` slot for ref `k` is
        // `element_slot + 1 + k`. Register each ref so same-view / handler reads resolve to it.
        for r in &element.references {
            let ref_slot = self.allocate_data_slot();
            self.register_local_ref(&r.name, ref_slot);
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
            // A modern animation listener (`(animate.enter)`/`(animate.leave)`) reifies to a
            // create-block `ɵɵanimateEnterListener`/`ɵɵanimateLeaveListener` with NO const-pool
            // entry (Angular keeps it out of `AttributeMarker.Bindings`), so skip its name.
            if matches!(output.kind, ParsedEventType::Animation) && output.name.starts_with("animate.")
            {
                continue;
            }
            // A legacy animation listener (`(@myAnimation.start)`) is a synthetic `@`-prefixed event
            // reified as a `ɵɵlistener("@trigger.phase", …)` with NO const-pool entry — Angular keeps
            // synthetic listeners out of the `AttributeMarker.Bindings` group. The front-end leaves it
            // a `Regular` event whose name keeps the `@` prefix, so detect it by that prefix.
            if output.name.starts_with('@') {
                continue;
            }
            binding_names.push(output.name.clone());
        }
        for input in &element.inputs {
            if matches!(
                input.kind,
                BindingType::Class
                    | BindingType::Style
                    | BindingType::Attribute
                    | BindingType::Animation
            ) {
                // A modern animation binding (`animate.enter`/`[animate.enter]`) reifies to a
                // create-block `ɵɵanimateEnter` op with NO const-pool entry (Angular's
                // `convert_animations` removes the binding op; it never reaches
                // `attribute_extraction`), so its name is not extracted under
                // `AttributeMarker.Bindings`.
                continue;
            }
            // A whole-element `[class]="exp"` / `[style]="exp"` binding becomes a `ClassMap`/
            // `StyleMap` op (`style_binding_specialization.ts`), which — like ClassProp/StyleProp —
            // is NOT a property and never extracts an `AttributeMarker.Bindings` name.
            if matches!(input.kind, BindingType::Property | BindingType::TwoWay)
                && (input.name == "class" || input.name == "style")
            {
                continue;
            }
            // A legacy-animation property binding (`[@trigger]`) is a synthetic `@`-prefixed
            // property that reifies to `ɵɵproperty("@trigger", …)` and is NOT extracted into the
            // element's const attrs (Angular keeps synthetic properties out of `consts`).
            if is_legacy_animation_name(&input.name) {
                continue;
            }
            binding_names.push(input.name.clone());
        }

        // A valueless `@`-prefixed static attribute (`<div @bar>`) is a legacy-animation property
        // binding with no expression: it reifies to `ɵɵproperty("@bar", undefined)` and, like a
        // bound `[@trigger]`, is kept OUT of the element's static-attribute const pool. Split those
        // synthetic names off so the rest of `element.attributes` interns normally.
        let mut synthetic_static_props: Vec<String> = Vec::new();
        let static_attrs: Vec<TextAttribute> = element
            .attributes
            .iter()
            .filter(|a| {
                if is_legacy_animation_name(&a.name) {
                    synthetic_static_props.push(a.name.clone());
                    false
                } else {
                    true
                }
            })
            .cloned()
            .collect();

        let attrs_index = self.element_attrs_index(&static_attrs, &binding_names);
        let local_refs_index = self.local_refs_index(&element.references);

        let has_children = !element.children.is_empty();
        // `collapseEmptyInstructions` (`phases/empty_elements.ts`) only merges an `elementStart` +
        // `elementEnd` into a single `element` when the `End` immediately follows the `Start` (only
        // `Pipe` ops are ignored in between). A **Listener** (a creation-block `ɵɵdomListener`) is
        // emitted between the start and end, so it blocks the merge — an element carrying any
        // `(event)` output stays in `elementStart … elementEnd` form even with no children.
        // An i18n-marked element always wraps an `ɵɵi18nStart … ɵɵi18nEnd` pair around its content,
        // so it stays in `elementStart … elementEnd` form even when it has no element children.
        let needs_end = has_children || !element.outputs.is_empty() || element.i18n.is_some();

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

        // Creation: the element itself, then its listeners. Angular's `reify` phase picks the
        // DOM-only element family (`ɵɵdomElementStart`/`ɵɵdomElementEnd`/`ɵɵdomElement`) when the
        // component compiles in `DomOnly` mode (standalone, no directive dependencies), and the
        // classic `Full` family (`ɵɵelementStart`/`ɵɵelementEnd`/`ɵɵelement`) otherwise.
        let (start_ref, single_ref) = if self.dom_only {
            (R3::DomElementStart, R3::DomElement)
        } else {
            (R3::ElementStart, R3::Element)
        };
        if needs_end {
            self.creation_code.push(instruction(start_ref, params));
        } else {
            self.creation_code.push(instruction(single_ref, params));
        }

        // `(event)="handler"` → creation-block listener (`ɵɵdomListener` in DomOnly mode,
        // `ɵɵlistener` in Full mode).
        for output in &element.outputs {
            self.build_listener(slot, &element.name, output);
        }

        // Modern animation bindings (`animate.enter`/`[animate.leave]`) reify to a CREATE-block
        // `ɵɵanimateEnter`/`ɵɵanimateLeave` op (Angular `convert_animations` inserts it right after
        // the element op, removing the update binding), emitted here after the listeners and before
        // the element's children. They reserve NO var slots and NO const-pool entry.
        for input in &element.inputs {
            if matches!(input.kind, BindingType::Animation) {
                self.build_animation(input);
            }
        }

        // Update: `[name]="expr"` → binding instructions (`ɵɵdomProperty`/`ɵɵclassProp`/
        // `ɵɵstyleProp`/`ɵɵattribute`), each advancing to this slot first. The bindings are
        // re-ordered into Angular's fixed `UPDATE_ORDERING` groups (`phases/ordering.ts`):
        // style props, then class props, then (non-interpolation) properties, then attributes —
        // a stable sort, so within a group source order is preserved. Animation bindings are
        // create-block ops (handled above) and are excluded from the update pass.
        let mut ordered: Vec<&BoundAttribute> = element
            .inputs
            .iter()
            .filter(|input| !matches!(input.kind, BindingType::Animation))
            .collect();
        ordered.sort_by_key(|input| update_order_rank(input));
        for input in ordered {
            self.build_property(slot, input);
        }

        // Valueless `@`-prefixed static attributes (`<div @bar>`) → `ɵɵproperty("@bar", undefined)`.
        // These synthetic legacy-animation properties carry no expression, so they reserve one var
        // slot each (like any property binding) and advance to the host slot before emitting.
        for name in &synthetic_static_props {
            self.allocate_binding_slots(1);
            self.advance_to(slot);
            self.update_code.push(instruction(
                self.property_reference(),
                vec![str_lit(name), undefined_expr()],
            ));
        }

        // A pipe consumed by one of this element's bindings takes the data slot immediately after
        // the element op (its `ɵɵpipe` create op is inserted right after `elementStart`, before the
        // children). Assign those slots before visiting children so the children get the slots that
        // follow (Angular `slot_allocation.ts` order).
        self.assign_positional_pipe_slots_for(slot);

        if element.i18n.is_some() {
            // The element is marked for translation: its content is lowered as an i18n block
            // (`ɵɵi18nStart`/`ɵɵi18nEnd` + `ɵɵi18nExp`/`ɵɵi18nApply`) rather than the normal
            // text / bound-text path.
            self.build_i18n_block(&element.children);
        } else if has_children {
            let children = element.children.clone();
            self.visit_all(&children);
        }
        if needs_end {
            let end_ref = if self.dom_only {
                R3::DomElementEnd
            } else {
                R3::ElementEnd
            };
            self.creation_code.push(instruction(end_ref, vec![]));
        }
    }

    /// Lower the content of an `i18n`-marked element. Builds the [`crate::i18n::Message`] from the
    /// element's text/interpolation children, allocates a data slot for the i18n block, interns the
    /// `$localize` message expression into the const pool, and emits:
    ///
    ///   - creation: `ɵɵi18nStart(slot, constIndex)` … `ɵɵi18nEnd()`.
    ///   - update (one per interpolation, in order): `ɵɵi18nExp(<expr>)`, then a single
    ///     `ɵɵi18nApply(slot)` after an `ɵɵadvance` to the block slot.
    ///
    /// SCOPE (common case): static text and `{{ … }}` interpolations only. ICU expansions, nested
    /// element placeholders, `goog.getMsg` legacy ids, and custom message meaning/description/id are
    /// out of scope here because they depend on subsystems that are not part of this view-function
    /// emitter (see the inline scope comments at the unhandled node arm and the message-meta
    /// construction below): the i18n placeholder-registry / ICU-context lowering, and a parsed
    /// `I18nMeta` carrying the message metadata. The text + interpolation path is fully handled.
    fn build_i18n_block(&mut self, children: &[Node]) {
        use crate::i18n;

        let slot = self.allocate_data_slot();

        // Build the i18n Message AST + collect the interpolation expressions (in source order) that
        // become `ɵɵi18nExp` operands.
        let mut nodes: Vec<i18n::Node> = Vec::new();
        let mut exprs: Vec<AstNode> = Vec::new();
        let mut interp_index = 0usize;
        for child in children {
            match child {
                Node::Text(t) => {
                    nodes.push(i18n::Node::Text(i18n::Text {
                        value: t.value.clone(),
                    }));
                }
                Node::BoundText(bt) => {
                    // A `BoundText` is an interpolation: `["a", "b", …]` strings interleaved with
                    // `[expr, …]`. Each literal-string segment becomes an i18n Text node, each
                    // expression an INTERPOLATION placeholder (numbered after the first, matching
                    // Angular's `PlaceholderRegistry`).
                    match &bt.value.kind {
                        AstExprKind::Interpolation { strings, expressions } => {
                            for (i, expr) in expressions.iter().enumerate() {
                                if let Some(s) = strings.get(i) {
                                    if !s.is_empty() {
                                        nodes.push(i18n::Node::Text(i18n::Text { value: s.clone() }));
                                    }
                                }
                                let name = i18n_interpolation_name(interp_index);
                                interp_index += 1;
                                nodes.push(i18n::Node::Placeholder(i18n::Placeholder {
                                    // `value` is only used by the UID/digest serializer; the raw
                                    // interpolation source is the faithful choice when available.
                                    value: String::new(),
                                    name,
                                }));
                                exprs.push(expr.clone());
                            }
                            if let Some(last) = strings.last() {
                                if !last.is_empty() {
                                    nodes.push(i18n::Node::Text(i18n::Text { value: last.clone() }));
                                }
                            }
                        }
                        // A bare `{{ expr }}` (no surrounding text).
                        _ => {
                            let name = i18n_interpolation_name(interp_index);
                            interp_index += 1;
                            nodes.push(i18n::Node::Placeholder(i18n::Placeholder {
                                value: String::new(),
                                name,
                            }));
                            exprs.push(bt.value.clone());
                        }
                    }
                }
                // SCOPE BOUNDARY: nested elements (which become `TagPlaceholder` start/close pairs),
                // ICU expansions (`Icu`/`IcuPlaceholder`) and control-flow blocks inside an i18n block
                // require the i18n placeholder-registry + ICU-context lowering subsystem (Angular
                // `i18n/context.ts`/`i18n/meta.ts`), which lives outside this view-function emitter.
                // Their content is intentionally skipped here; the common text + interpolation case is
                // fully handled.
                _ => {}
            }
        }

        // SCOPE BOUNDARY: the `i18n` attribute value (`meaning|description@@id`) cannot be threaded
        // here because the r3_ast `I18nMeta` marker is an opaque zero-field struct (`pub struct
        // I18nMeta;`) — it carries no parsed metadata. So meaning/description/customId are empty. When
        // the upstream marker is fleshed out to carry the parsed meta, it would feed `Message::new`
        // and `compute_msg_id`; until then the metadata is structurally unavailable.
        let message = i18n::Message::new(nodes, "", "", "");

        // Build the placeholder params (`I18nMessageOp.params`): each interpolation maps its public
        // placeholder name to the runtime magic string `\u{FFFD}<index>\u{FFFD}` plus the authored
        // template source (`original_code`, reconstructed as `{{ <expr-source> }}`). These feed both
        // the `goog.getMsg` placeholder/options maps and the `$localize` substitution expressions.
        let params: Vec<i18n::I18nPlaceholderParam> = (0..exprs.len())
            .map(|i| i18n::I18nPlaceholderParam {
                name: i18n_interpolation_name(i),
                value: format!("\u{FFFD}{i}\u{FFFD}"),
                original_code: i18n_original_code(&exprs[i]),
            })
            .collect();

        let const_index = self.intern_i18n_message(&message, &params);

        // Creation block: `ɵɵi18nStart(slot, constIndex)` … `ɵɵi18nEnd()`.
        self.creation_code.push(instruction(
            R3::I18nStart,
            vec![num(slot as f64), num(const_index as f64)],
        ));
        self.creation_code.push(instruction(R3::I18nEnd, vec![]));

        // Update block: one `ɵɵi18nExp(<expr>)` per interpolation, then a single `ɵɵi18nApply(slot)`.
        // Each interpolation reserves one binding (var) slot (Angular `i18nExp` → one var).
        if !exprs.is_empty() {
            self.allocate_binding_slots(exprs.len());
            self.advance_to(slot);
            self.current_target_slot = slot;
            for expr in &exprs {
                let lowered = self.lower_expr(expr);
                self.update_code
                    .push(instruction(R3::I18nExp, vec![lowered]));
            }
            self.update_code
                .push(instruction(R3::I18nApply, vec![num(slot as f64)]));
        }
    }

    /// Intern an i18n [`crate::i18n::Message`] as a closure-mode const-pool entry and return its
    /// const-pool index (used as the `constIndex` argument of `ɵɵi18nStart`/`ɵɵi18n`).
    ///
    /// Mirrors Angular's `i18n_const_collection.ts`: the const-array entry is a `$i18n_n$` read-var
    /// whose value is assigned lazily in the `consts: () => { … }` arrow body via
    /// `let $i18n_n$; if (typeof ngI18nClosureMode … && ngI18nClosureMode) { const $MSG_…$ =
    /// goog.getMsg(…); $i18n_n$ = $MSG_…$; } else { $i18n_n$ = $localize`…`; }`. The `$localize`
    /// branch's `LocalizedString` is built here (mirroring `createLocalizeStatements`); the
    /// `goog.getMsg` branch + the closure guard are assembled by [`crate::i18n::build_i18n_const`].
    ///
    /// `params` carry, per interpolation placeholder, the runtime magic string and authored source;
    /// the `$localize` substitution expressions are exactly those magic-string literals (Angular's
    /// `placeHolders.map(ph => params[ph.text])`).
    fn intern_i18n_message(
        &mut self,
        message: &crate::i18n::Message,
        params: &[crate::i18n::I18nPlaceholderParam],
    ) -> usize {
        use crate::i18n;

        // The `$localize` meta block carries only the AUTHORED description / meaning / custom id
        // (`serializeI18nHead`); the auto-computed decimal digest is the runtime/linker message id
        // and is NOT embedded in the tagged-template head. With no explicit `@@id`/meaning/desc on
        // the template, the head is empty (` The result is ${…}:INTERPOLATION: `), matching Angular.
        let meta = o::I18nMeta {
            description: (!message.description.is_empty()).then(|| message.description.clone()),
            meaning: (!message.meaning.is_empty()).then(|| message.meaning.clone()),
            custom_id: (!message.custom_id.is_empty()).then(|| message.custom_id.clone()),
            legacy_ids: Vec::new(),
        };

        let mut message_parts: Vec<o::LiteralPiece> = Vec::new();
        let mut placeholders: Vec<o::PlaceholderPiece> = Vec::new();
        // The `$localize` substitution expressions: the placeholder magic strings, in placeholder
        // order. Without these the emitted tagged template drops the `${…}` substitution entirely.
        let mut expressions: Vec<o::Expr> = Vec::new();
        let no_span = o::ParseSourceSpan::new(0, 0);
        let mut param_by_name = std::collections::HashMap::new();
        for p in params {
            param_by_name.insert(p.name.clone(), p.value.clone());
        }

        // Walk the (flat, common-case) message nodes: Text → a literal part, Placeholder → a
        // placeholder piece. Two consecutive literals are merged so `messageParts` and
        // `placeholders` interleave as `$localize` expects (one more part than placeholder).
        let mut pending = String::new();
        for node in &message.nodes {
            match node {
                i18n::Node::Text(t) => pending.push_str(&t.value),
                i18n::Node::Placeholder(ph) => {
                    message_parts.push(o::LiteralPiece {
                        text: std::mem::take(&mut pending),
                        source_span: no_span.clone(),
                    });
                    placeholders.push(o::PlaceholderPiece {
                        // `$localize` uses the NON-camel public placeholder name (`:INTERPOLATION:`).
                        text: i18n::format_i18n_placeholder_name(&ph.name, false),
                        source_span: no_span.clone(),
                        associated_message: None,
                    });
                    let value = param_by_name
                        .get(&ph.name)
                        .cloned()
                        .unwrap_or_default();
                    expressions.push(o::literal(o::LiteralValue::String(value), None));
                }
                _ => {}
            }
        }
        // Trailing (or sole) literal part. `$localize` always has one more part than placeholder.
        message_parts.push(o::LiteralPiece {
            text: pending,
            source_span: no_span.clone(),
        });

        let localize_expr = o::localized_string(meta, message_parts, placeholders, expressions);

        // The message's const ordinal — Angular numbers `$i18n_n$` from the const-array position.
        let index = self.const_pool.entries().len();
        let i18n_const = i18n::build_i18n_const(message, index, params, localize_expr);
        self.const_pool
            .add_const_with_initializers(i18n_const.const_entry, i18n_const.initializers)
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

        // A whole-element `[class]="exp"` / `[style]="exp"` binding (a `Property` whose name is
        // exactly `class`/`style`) is specialized to a `ClassMap`/`StyleMap` op
        // (`style_binding_specialization.ts`).
        let is_style_or_class_map = matches!(input.kind, BindingType::Property | BindingType::TwoWay)
            && (input.name == "class" || input.name == "style");

        // Reserve var slots per Angular `varsUsedByOp`: property/attribute = 1 (+N), class/style
        // (ClassProp/StyleProp AND ClassMap/StyleMap) = 2 (+N).
        let base_vars = match input.kind {
            BindingType::Class | BindingType::Style => 2,
            _ if is_style_or_class_map => 2,
            _ => 1,
        };
        self.allocate_binding_slots(base_vars + interp_extra);

        self.advance_to(slot);

        // The consuming op for any pipe in this binding is the host element; the `ɵɵpipe(...)` create
        // op is inserted right after the element's create op (Angular `addPipeToCreationBlock`).
        self.current_target_slot = slot;

        // A `[@trigger]` legacy-animation property binding (a `Property`/`TwoWay` whose name begins
        // with `@`) reifies to `ɵɵproperty("@trigger", …)` — NOT through the DOM-property remapping
        // path (which would mangle the `@` name) and never as an ARIA/class/style binding. A bound
        // form with an empty value (`[@baz]`) carries the `undefined` value (Angular emits
        // `ɵɵproperty("@baz", undefined)`).
        if matches!(input.kind, BindingType::Property | BindingType::TwoWay)
            && is_legacy_animation_name(&input.name)
        {
            let value = if is_empty_binding_value(&input.value) {
                undefined_expr()
            } else {
                self.lower_expr(&input.value)
            };
            self.update_code.push(instruction(
                self.property_reference(),
                vec![str_lit(&input.name), value],
            ));
            return;
        }

        // Lower against this view's scope (`ctx` + any `@for` loop locals). `lower_expr` already
        // spilled any temporary statements the expression needs (safe-navigation guards, chained
        // sub-expressions) into the update buffer ahead of the instruction we push below.
        let lowered = self.lower_expr(&input.value);
        match input.kind {
            // Modern animation bindings (`animate.enter`/`[animate.enter]`) are CREATE-block ops
            // handled by `build_animation` (Angular `convert_animations` removes the update binding),
            // so they never reach this update-pass lowering.
            BindingType::Animation => {}
            // Legacy animation bindings (`[@trigger]="exp"`) reify to a DOM property whose name is the
            // synthetic, `@`-prefixed trigger name (`ɵɵdomProperty("@trigger", <exp>)`) — Angular's
            // `prepareSyntheticProperty` prefixes the trigger with `@`.
            BindingType::LegacyAnimation => {
                let name = format!("@{}", input.name);
                self.update_code
                    .push(instruction(R3::DomProperty, vec![str_lit(&name), lowered]));
            }
            BindingType::Property | BindingType::TwoWay if is_style_or_class_map => {
                // Whole-element `[class]="exp"` / `[style]="exp"` reify to `ɵɵclassMap(exp)` /
                // `ɵɵstyleMap(exp)` (`style_binding_specialization.ts` + `reify.ts`), with NO
                // `AttributeMarker.Bindings` const entry and the value as a single argument.
                let reference = if input.name == "style" {
                    R3::StyleMap
                } else {
                    R3::ClassMap
                };
                self.update_code.push(instruction(reference, vec![lowered]));
            }
            BindingType::Property | BindingType::TwoWay => {
                // A plain `[name]="expr"` property binding reifies to `ɵɵdomProperty` in DomOnly mode
                // and `ɵɵproperty` in Full mode (`reify.ts` `reifyDomProperty`/`reifyProperty`). In
                // DomOnly mode the property name is run through `DOM_PROPERTY_REMAPPING` (e.g.
                // `class` → `className`); in Full mode an ARIA-attribute name (`aria-*`) selects
                // `ɵɵariaProperty` instead and the DOM remapping is not applied.
                if self.dom_only {
                    let name = remap_dom_property(&input.name);
                    self.update_code
                        .push(instruction(R3::DomProperty, vec![str_lit(name), lowered]));
                } else if is_aria_attribute(&input.name) {
                    self.update_code
                        .push(instruction(R3::AriaProperty, vec![str_lit(&input.name), lowered]));
                } else {
                    self.update_code
                        .push(instruction(R3::Property, vec![str_lit(&input.name), lowered]));
                }
            }
            BindingType::Class => {
                // A `[class.x]="a{{exp}}b"` interpolation reifies its value through
                // `ɵɵinterpolateN("a", exp, "b")` (Angular `interpolate.ts` `ClassProp` interpolation),
                // not a raw string concat. A non-interpolation value passes through unchanged.
                let value = self.style_class_binding_value(&input.value, lowered);
                self.update_code
                    .push(instruction(R3::ClassProp, vec![str_lit(&input.name), value]));
            }
            BindingType::Style => {
                // As `ClassProp` above: a `[style.x]="a{{exp}}b"` value is wrapped in
                // `ɵɵinterpolateN(...)`. A `style.x.unit` carries the unit as a trailing argument.
                let value = self.style_class_binding_value(&input.value, lowered);
                let mut params = vec![str_lit(&input.name), value];
                if let Some(unit) = &input.unit {
                    params.push(str_lit(unit));
                }
                self.update_code.push(instruction(R3::StyleProp, params));
            }
            BindingType::Attribute => {
                // `ɵɵattribute(name, value[, sanitizer])` — `resolve_sanitizers.ts` appends the
                // security-context sanitizer when one applies. `[attr.style]` carries the STYLE
                // context (→ `ɵɵsanitizeStyle`); URL-ish attributes carry URL/RESOURCE_URL. The
                // attribute name drives the context the way Angular's schema does for the common
                // cases (the upstream transform leaves `attr.*` context at `None`, so derive it here).
                let mut params = vec![str_lit(&input.name), lowered];
                if let Some(sanitizer) = attribute_sanitizer(&input.security_context, &input.name) {
                    params.push(o::import_expr(sanitizer.reference(), None));
                }
                self.update_code.push(instruction(R3::Attribute, params));
            }
        }
    }

    /// Lower a modern animation binding (`animate.enter`/`[animate.enter]`,
    /// `animate.leave`/`[animate.leave]`) into a CREATE-block `ɵɵanimateEnter`/`ɵɵanimateLeave` op
    /// (Angular `convert_animations` inserts it right after the element op). Two shapes, per
    /// `AnimationBindingKind`:
    ///
    /// - **STRING** (static `animate.enter="slide"`, the value is a string literal): the literal is
    ///   passed directly — `ɵɵanimateEnter("slide")`.
    /// - **VALUE** (`[animate.enter]="exp"`, a bound expression): the expression is wrapped in a
    ///   zero-arg callback named `<viewFn>_<name-without-dot>_cb` (`naming.ts`) whose body
    ///   `return`s the lowered expression — `ɵɵanimateEnter(function MyApp_Template_animateenter_cb() { return ctx.exp(); })`.
    ///
    /// Reserves NO var slots and emits NO const-pool entry (the binding name is not extracted under
    /// `AttributeMarker.Bindings`).
    fn build_animation(&mut self, input: &BoundAttribute) {
        let reference = if input.name.ends_with("leave") {
            R3::AnimationLeave
        } else {
            R3::AnimationEnter
        };

        // STRING form: the parsed value is a bare string literal (Angular's
        // `AnimationBindingKind.STRING`, lifted from a static `animate.*="..."` attribute).
        let arg = if let AstExprKind::LiteralPrimitive {
            value: crate::expression::ast::LiteralValue::Str(s),
        } = &input.value.kind
        {
            str_lit(s)
        } else {
            // VALUE form: wrap the lowered expression in a zero-arg `_cb` callback returning it.
            // The expression lowers against this view's scope (`ctx` + any `@for` loop locals).
            let lowered = self.lower_expr(&input.value);
            // `naming.ts`: `${unit.fnName}_${name.replace('.', '')}_cb`.
            let cb_name = format!("{}_{}_cb", self.name, input.name.replace('.', ""));
            let body = vec![o::Stmt::bare(o::StmtKind::Return(lowered))];
            o::fn_(vec![], body, None, Some(cb_name))
        };

        self.creation_code.push(instruction(reference, vec![arg]));
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
        // A `@let` (this view's or an ancestor's) read in the handler is a cross-view read: it
        // resolves to a `const <name>_r = ɵɵreadContextLet(slot)` prepended to the handler body
        // (`generate_variables.ts`, `isCallback` ⇒ even this view's own lets are read this way), and
        // forces the view to be saved/restored (`ɵɵgetCurrentView`/`ɵɵrestoreView`/`ɵɵresetView`).
        // `context_lets` holds exactly the slot-bearing `@let`s; in a callback EVERY such let in
        // scope (including this view's own) is read via `ɵɵreadContextLet` rather than its in-view
        // `ɵɵstoreLet` local (`generate_variables.ts`: `scope.view !== view.xref || isCallback`).
        let referenced_lets: Vec<ContextLet> = self
            .context_lets
            .iter()
            .filter(|cl| expr_references_implicit(&output.handler, &cl.name))
            .cloned()
            .collect();

        let mut context_let_locals: Vec<(String, String)> = Vec::new();
        let mut let_reads: Vec<Stmt> = Vec::new();
        for cl in &referenced_lets {
            self.var_counter += 1;
            // Angular's `BindingScope` mints the `ɵɵreadContextLet` read local with the `$name$`
            // expect-emit form the goldens spell as `$one_1$` (see `local_ref_var_name`), distinct
            // from the `<name>_r<id>` view-ref scheme used for `ɵɵreference` locals.
            let local_name = local_ref_var_name(&cl.name, self.var_counter);
            let read = o::import_expr(R3::ReadContextLet.reference(), None)
                .call_fn(vec![num(cl.slot as f64)], false);
            let_reads.push(Stmt::with_modifiers(
                StmtKind::DeclareVar {
                    name: local_name.clone(),
                    value: Some(read),
                    ty: None,
                },
                StmtModifier::FINAL,
            ));
            context_let_locals.push((cl.name.clone(), local_name));
        }

        // A template local reference (`#ref`) read in the handler likewise resolves to a
        // `const $ref$ = ɵɵreference(slot)` prepended to the handler body, and forces the view to be
        // saved/restored (`generate_variables.ts` `Reference` in a callback scope). The ref's host
        // may be declared LATER in the template (the listener's create op precedes it); the slot was
        // reserved at pre-registration and is the real slot once the host is reached (placeholder is
        // patched by `finalize_local_refs` for not-yet-reached hosts).
        let referenced_refs: Vec<LocalRef> = self
            .local_refs
            .iter()
            .filter(|r| expr_references_implicit(&output.handler, &r.name))
            .cloned()
            .collect();
        for r in &referenced_refs {
            let read = o::import_expr(R3::Reference.reference(), None)
                .call_fn(vec![num(r.slot as f64)], false);
            let_reads.push(Stmt::with_modifiers(
                StmtKind::DeclareVar {
                    name: r.local_name.clone(),
                    value: Some(read),
                    ty: None,
                },
                StmtModifier::FINAL,
            ));
            context_let_locals.push((r.name.clone(), r.local_name.clone()));
        }

        let needs_view_restore = !referenced_lets.is_empty() || !referenced_refs.is_empty();

        // Lower the handler against a resolver that keeps `$event` a bare parameter read (Angular
        // `resolveDollarEvent`), rewrites any in-scope `@for` loop vars to their locals, and resolves
        // cross-view `@let` reads to their `ɵɵreadContextLet` locals.
        let resolver = ListenerResolver {
            vars: &self.loop_vars,
            context_let_locals: &context_let_locals,
        };
        let converted = convert_action_binding_with(&output.handler, &resolver);

        // Handler body, in order: `ɵɵrestoreView(savedView)` (when reading cross-view state), the
        // `ɵɵreadContextLet` `const`s, any spilled statements, then the `return`. The final value is
        // wrapped in `ɵɵresetView(...)` when the view was restored.
        let mut body: Vec<Stmt> = Vec::new();
        if needs_view_restore {
            let saved = self.saved_view_var_name();
            body.push(
                o::import_expr(R3::RestoreView.reference(), None)
                    .call_fn(vec![o::variable(saved, None)], false)
                    .to_stmt(),
            );
        }
        body.extend(let_reads);
        body.extend(converted.stmts);
        let ret = if needs_view_restore {
            o::import_expr(R3::ResetView.reference(), None).call_fn(vec![converted.expr], false)
        } else {
            converted.expr
        };
        body.push(o::Stmt::bare(o::StmtKind::Return(ret)));

        // A modern animation listener (`(animate.enter)`/`(animate.leave)`) reifies to
        // `ɵɵanimateEnterListener`/`ɵɵanimateLeaveListener` (Angular `reify.ts`), and its handler is
        // named with the SANITIZED event (`sanitizeIdentifier`: the `.` is dropped, so
        // `animate.enter` → `animateenter`). It carries no `(name, …)` event-name argument and never
        // extracts a const attr.
        let is_animate_listener =
            matches!(output.kind, ParsedEventType::Animation) && output.name.starts_with("animate.");
        // A LEGACY animation listener (`(@myAnimation.start)`) — Angular's synthetic, `@`-prefixed
        // output. The front-end parser classifies it as a `Regular` event whose NAME carries the
        // whole `@trigger.phase` (it does not split the phase), so detect it here by the `@` prefix.
        // Its `ɵɵlistener` event-name argument is the `prepareSyntheticListenerName` form
        // `@${trigger}.${phase}` (the raw name, already in that form) and its handler function is
        // named with the `prepareSyntheticListenerFunctionName` form `animation_${trigger}_${phase}`
        // (an `animation` prefix; `.`/`@` sanitized to `_`), NOT the raw `@x.y` (invalid JS).
        let is_legacy_animation_listener = !is_animate_listener && output.name.starts_with('@');
        // Angular `naming.ts`: `${unit.fnName}_${tag.replace('-', '_')}_${event}_${slot}_listener`,
        // with `event` run through `sanitizeIdentifier` (non-word chars → `_`); an animate event's
        // `.` is removed entirely, giving `animateenter`; a legacy animation event becomes
        // `animation_${trigger}_${phase}`.
        let event_in_name = if is_animate_listener {
            output.name.replace('.', "")
        } else if is_legacy_animation_listener {
            // `@myAnimation.start` → `animation_myAnimation_start` (`prepareSyntheticListenerFunctionName`:
            // strip the leading `@`, prefix `animation_`, map the `.` phase separator to `_`).
            let trigger_phase = output.name.trim_start_matches('@').replace('.', "_");
            format!("animation_{trigger_phase}")
        } else {
            output.name.clone()
        };
        let handler_name = format!(
            "{}_{}_{}_{}_listener",
            self.name,
            tag.replace('-', "_"),
            event_in_name,
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

        // A modern animation listener reifies to `ɵɵanimateEnterListener`/`ɵɵanimateLeaveListener`
        // and takes ONLY the handler function (no event-name argument). A regular template listener
        // reifies to `ɵɵdomListener` in DomOnly mode and `ɵɵlistener` in Full mode (`reify.ts`:
        // `domListener` iff `mode === DomOnly && !hostListener && !isLegacyAnimationListener`); both
        // take the `(name, handlerFn)` argument shape.
        if is_animate_listener {
            let reference = if output.name.ends_with("leave") {
                R3::AnimationLeaveListener
            } else {
                R3::AnimationEnterListener
            };
            self.creation_code
                .push(instruction(reference, vec![handler_fn]));
            return;
        }
        // A legacy animation listener reifies to `ɵɵlistener` (Full family) REGARDLESS of the
        // component's DomOnly mode (`reify.ts`: `domListener` requires `!isLegacyAnimationListener`),
        // and its event-name argument is the synthetic `@${name}.${phase}` form
        // (`prepareSyntheticListenerName`). A regular listener reifies to `ɵɵdomListener` in DomOnly
        // mode / `ɵɵlistener` otherwise, with the raw event name.
        let listener_ref = if self.dom_only && !is_legacy_animation_listener {
            R3::DomListener
        } else {
            R3::Listener
        };
        // The event-name argument is the raw event name. For a legacy animation listener the parser
        // already delivered it in the `@${trigger}.${phase}` synthetic form, so it is used verbatim.
        self.creation_code.push(instruction(
            listener_ref,
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
        for r in &template.references {
            let ref_slot = self.allocate_data_slot();
            self.register_local_ref(&r.name, ref_slot);
        }
        // A `<ng-template>`'s own `[prop]` inputs are collected — like an element's — under the
        // `AttributeMarker.Bindings` group of its const attrs (Angular `serializeAttributes`), so
        // `<ng-template [id]="">` interns `[AttributeMarker.Bindings, "id"]`. Legacy-animation
        // synthetic (`@`-prefixed) inputs stay out of the const pool, mirroring the element path.
        let template_binding_names: Vec<String> = template
            .inputs
            .iter()
            .filter(|i| !is_legacy_animation_name(&i.name))
            .map(|i| i.name.clone())
            .collect();
        let attrs_index = self.element_attrs_index(&template.attributes, &template_binding_names);
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
        // An `<ng-template>` reifies to `ɵɵdomTemplate` in DomOnly mode and `ɵɵtemplate` in Full mode
        // (`reify.ts`: `domTemplate` iff `templateKind === Block || mode === DomOnly`). A plain
        // `<ng-template>` is an `NgTemplate`, so the choice follows the component's compilation mode;
        // control-flow block bodies (`@if`/`@for`) are built via [`Self::build_embedded_view`] and
        // referenced by `ɵɵconditionalCreate`/`ɵɵrepeaterCreate`, not this `<ng-template>` path.
        let template_ref = if self.dom_only {
            R3::DomTemplate
        } else {
            R3::TemplateCreate
        };
        self.creation_code.push(instruction(template_ref, params));

        // A `<ng-template>`'s `[prop]="expr"` inputs bind in the PARENT update block against the
        // template's data slot (Angular lowers them like an element's property bindings). An
        // empty-value binding (`[id]=""`) has no expression, so Angular reserves its const-attr
        // Bindings entry (collected above) but emits NO `ɵɵproperty` update and NO var slot — the
        // remaining inputs each reserve one var and emit `ɵɵproperty(name, <expr>)` after advancing
        // to the template slot. Legacy-animation synthetic inputs are handled like elements'.
        let bound_inputs: Vec<&BoundAttribute> = template
            .inputs
            .iter()
            .filter(|i| !is_empty_binding_value(&i.value))
            .collect();
        for input in bound_inputs {
            self.build_property(slot, input);
        }
    }

    /// Pre-intern every local-reference const in THIS view, in create-op (depth-first pre-order)
    /// order, mirroring Angular's `liftLocalRefs` phase (which runs before `collectElementConsts`),
    /// and pre-register each ref (name + generated local + placeholder slot). Only same-view
    /// element/template hosts contribute; control-flow / `<ng-template>` bodies are separate child
    /// views whose refs are interned when that view is built. The element's own children ARE in the
    /// same view, so we recurse into them.
    fn prepass_local_ref_consts(&mut self, nodes: &[Node]) {
        for node in nodes {
            match node {
                Node::Element(el) => {
                    self.local_refs_index(&el.references);
                    for r in &el.references {
                        self.preregister_local_ref(&r.name);
                    }
                    self.prepass_local_ref_consts(&el.children);
                }
                Node::Template(tmpl) => {
                    // The `<ng-template>` host's own `#ref`s belong to THIS (outer) view; its body
                    // is a separate child view, so we do NOT recurse into its children here.
                    self.local_refs_index(&tmpl.references);
                    for r in &tmpl.references {
                        self.preregister_local_ref(&r.name);
                    }
                }
                _ => {}
            }
        }
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
        let nested_input =
            TemplateCompilationInput::new(fn_name.clone(), children).with_dom_only(self.dom_only);
        let mut nested = TemplateDefinitionBuilder::new(&nested_input);
        // An embedded view is never the root: its hoisted descendant fns bubble up to the root
        // (below) rather than being inlined into this view body, so each is emitted exactly once.
        nested.is_root = false;
        // This embedded view sits one nesting level below the current view (Angular `retrievalLevel`).
        nested.view_level = self.view_level + 1;
        // Thread the component-global variable counter so nested loop vars get unique `_rN` names.
        nested.var_counter = self.var_counter;
        // Thread the in-scope cross-view `@let`s so a read in this embedded view can resolve to
        // `ɵɵreadContextLet(slot)` against the owning ancestor view (`generate_variables.ts`).
        nested.context_lets = self.context_lets.clone();
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
            // `instruction.ts` `conditionalCreate`/`conditionalBranchCreate` trim trailing `null`
            // arguments, so a text-only branch (no element tag) drops the final `null` tag arg.
            let mut params = vec![num(slot as f64), fn_ref, num(decls as f64), num(vars as f64), tag];
            trim_trailing_nulls(&mut params);
            self.creation_code.push(instruction(reference, params));
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

        // Any pipe in a branch condition is consumed by the conditional anchor op (`addPipeToCreationBlock`).
        self.current_target_slot = anchor_slot;
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
        // Flatten groups -> one `CaseSlot` per `@case`/`@default` LABEL, all labels of a group sharing
        // that group's body slot. A group like `@case 1 @case 2 { … }` therefore contributes two
        // comparisons (`tmp === 1` and `tmp === 2`) both selecting the same slot, so any of the
        // labels matches the shared body (faithful to `createSwitch`, which emits one comparison per
        // case expression). One template is still emitted per group.
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
            // Emit one `CaseSlot` per label so every `@case` label of the group gets its own
            // comparison against the shared body slot. A group with no labels (defensive) or a sole
            // `@default` still records a single slot-selecting entry.
            let labels: Vec<&SwitchBlockCase> = group.cases.iter().collect();
            if labels.is_empty() {
                cases.push(CaseSlot { expression: None, slot });
            } else {
                for case in labels {
                    cases.push(CaseSlot {
                        expression: case.expression.clone(),
                        slot,
                    });
                }
            }
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
        // Any pipe in the discriminant or a case expression is consumed by the conditional anchor op.
        self.current_target_slot = anchor_slot;
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
    ///   → `ɵɵrepeaterTrackByIdentity`; otherwise a custom `track` expression (`track item.id`,
    ///   `track trackFn($index, item)`) lowers to a generated pure arrow `(<item>, $index) => <expr>`
    ///   passed directly as the trackBy argument (Angular `optimizeTrackFns`/`generateTrackFn`);
    /// - `ɵɵrepeaterCreate(slot, ForFn, decls, vars, tag, attrs?, trackByFn[, usesComponentInstance,
    ///   EmptyFn, emptyDecls, emptyVars])`, then `ɵɵrepeater(<collection>)` in update.
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

        // The track-by function expression (optimized helper reference, or a generated arrow for a
        // custom `track` expression) plus whether it reads the component instance.
        let (track_fn, track_uses_component_instance) = self.build_track_fn(block, slot);

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
            track_fn,
        ];

        // The `trackByUsesComponentInstance` flag (Angular `ɵɵrepeaterCreate`'s 8th argument): a
        // custom `track` that reads the component context is bound to `this`, so the runtime is told
        // to pass the component instance. It is also (re)emitted whenever an `@empty` view follows, as
        // its positional slot must be filled before the empty-view args.
        if let Some(empty) = &block.empty {
            // `@empty { … }` → trailing empty-view args after the trackByUsesComponentInstance flag.
            let empty_fn_name = format!("{}_ForEmpty_{}_Template", self.base_name, slot + 2);
            let (empty_fn, empty_decls, empty_vars) =
                self.build_embedded_view(empty_fn_name, empty.children.clone(), Vec::new(), Vec::new());
            params.push(o::literal(
                o::LiteralValue::Bool(track_uses_component_instance),
                None,
            ));
            params.push(empty_fn);
            params.push(num(empty_decls as f64));
            params.push(num(empty_vars as f64));
            params.push(single_root_tag(&empty.children));
        } else if track_uses_component_instance {
            // No `@empty`, but the custom trackBy needs the component instance: emit the flag so the
            // runtime binds `this`. (When false and there is no empty view, the trailing arg is
            // elided, matching Angular's trimmed argument list.)
            params.push(o::literal(o::LiteralValue::Bool(true), None));
        }

        self.creation_code
            .push(instruction(R3::RepeaterCreate, params));

        // Update: `ɵɵrepeater(<collection>)`. The collection lowers against this view's scope.
        // Any pipe in the collection is consumed by the repeater create op at `slot`.
        self.current_target_slot = slot;
        let collection = self.lower_expr(&block.expression.ast);
        self.advance_to(slot);
        self.update_code
            .push(instruction(R3::Repeater, vec![collection]));
    }

    /// Build the trackBy argument of `ɵɵrepeaterCreate` for an `@for` block, returning
    /// `(trackByExpr, usesComponentInstance)`.
    ///
    /// Faithful to Angular `optimizeTrackFns` / `generateTrackFn`:
    /// - `track $index` (a bare `$index` read) → the shared `ɵɵrepeaterTrackByIndex` helper;
    /// - `track <item>` (a bare read of the loop item variable) → `ɵɵrepeaterTrackByIdentity`;
    /// - any other expression → a generated arrow `(<itemName>, $index) => <expr>` where the loop
    ///   item name resolves to the first parameter and `$index` to the second. If the expression
    ///   reads anything off the component context (a non-item, non-`$index` implicit read), the
    ///   arrow needs the component instance, so `usesComponentInstance` is `true` (Angular binds the
    ///   generated trackBy to `this`).
    fn build_track_fn(&mut self, block: &ForLoopBlock, _slot: usize) -> (Expr, bool) {
        let Some(track) = &block.track_by else {
            return (
                o::import_expr(R3::RepeaterTrackByIdentity.reference(), None),
                false,
            );
        };

        // Bare `$index` / item reads collapse to the shared optimized helpers (no generated fn).
        if let Some(name) = bare_read_name(&track.ast) {
            if name == "$index" {
                return (
                    o::import_expr(R3::RepeaterTrackByIndex.reference(), None),
                    false,
                );
            }
            if name == block.item.name {
                return (
                    o::import_expr(R3::RepeaterTrackByIdentity.reference(), None),
                    false,
                );
            }
        }

        // Custom track expression → generate `(<item>, $index) => <expr>`. Reads of the item or
        // `$index` resolve to the arrow parameters; everything else roots at the component context
        // (flagging `usesComponentInstance`).
        let item_name = block.item.name.clone();
        let uses_ctx = std::cell::Cell::new(false);
        let resolver = TrackFnResolver {
            item_name: item_name.clone(),
            used_component_instance: &uses_ctx,
        };
        let body = convert_property_binding_with(&track.ast, &resolver).expr;
        let track_arrow = o::arrow_fn(
            vec![FnParam::new(item_name, None), FnParam::new("$index", None)],
            o::ArrowBody::Expr(Box::new(body)),
            None,
        );
        (track_arrow, uses_ctx.get())
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

    /// Classify every top-level `@let` of this view, populating [`Self::external_lets`].
    ///
    /// A `@let` needs cross-view storage (`ɵɵdeclareLet` slot + `ɵɵstoreLet`) only when its value is
    /// read from a *different* view — a listener handler in this view, or any descendant embedded
    /// view (`optimizeStoreLet`: an `external` let). A non-external let whose value uses a pipe keeps
    /// its `ɵɵdeclareLet` slot (the pipe needs the TNode for DI) but NOT the `ɵɵstoreLet`. Everything
    /// else inlines as a plain `const` / bare statement with no slot or var. The map value is `true`
    /// for an external let (storeLet retained) and `false` for a pipe-only-slot let.
    fn analyze_let_declarations(&mut self, nodes: &[Node]) {
        for node in nodes {
            if let Node::LetDeclaration(decl) = node {
                let external = let_used_externally(nodes, &decl.name);
                if external || let_value_has_pipe(&decl.value) {
                    self.external_lets.insert(decl.name.clone(), external);
                }
            }
        }
    }

    /// Prepend, for each ancestor `@let` whose value THIS view's own update bindings read, a
    /// `const <name>_r<id> = ɵɵreadContextLet(slot)` declaration plus the `ɵɵnextContext()` hop that
    /// switches into the owner view, and register the generated local so reads of the name resolve
    /// to it (`generate_variables.ts` `letDeclarations`). Only ancestor lets actually referenced in
    /// this view are materialised (`optimizeVariables` drops the unused ones). The current view's own
    /// lets are read directly via their `ɵɵstoreLet` local, so they are excluded here.
    fn bring_ancestor_lets_into_scope(&mut self, nodes: &[Node]) {
        let ancestor_lets: Vec<ContextLet> = self
            .context_lets
            .iter()
            .filter(|cl| cl.owner_level < self.view_level)
            .cloned()
            .collect();
        if ancestor_lets.is_empty() {
            return;
        }

        // A name is referenced in this view's update block if any same-view binding reads it (a
        // bound text, element/template/component input, or control-flow condition). Skip names this
        // view re-declares (a local let / loop var shadows the ancestor).
        let mut prelude: Vec<Stmt> = Vec::new();
        let mut emitted_next_context = false;
        for cl in &ancestor_lets {
            if declares_let_at_top(nodes, &cl.name) {
                continue;
            }
            if self.loop_vars.iter().any(|v| v.source_name == cl.name) {
                continue;
            }
            let referenced = nodes
                .iter()
                .any(|n| match n {
                    Node::LetDeclaration(l) => expr_references_implicit(&l.value, &cl.name),
                    _ => same_view_node_references(n, &cl.name),
                });
            if !referenced {
                continue;
            }

            // A single `ɵɵnextContext()` switches into the immediately-enclosing view (the common
            // single-level case shared by all the referenced ancestor lets here).
            if !emitted_next_context {
                prelude.push(
                    o::import_expr(R3::NextContext.reference(), None)
                        .call_fn(vec![], false)
                        .to_stmt(),
                );
                emitted_next_context = true;
            }

            self.var_counter += 1;
            // The cross-view `ɵɵreadContextLet` read local takes the `$name$` expect-emit form the
            // goldens spell as `$two_0$` (see `local_ref_var_name`), like the listener-scope read.
            let local_name = local_ref_var_name(&cl.name, self.var_counter);
            let read = o::import_expr(R3::ReadContextLet.reference(), None)
                .call_fn(vec![num(cl.slot as f64)], false);
            prelude.push(Stmt::with_modifiers(
                StmtKind::DeclareVar {
                    name: local_name.clone(),
                    value: Some(read),
                    ty: None,
                },
                StmtModifier::FINAL,
            ));
            self.loop_vars.push(LoopVar {
                source_name: cl.name.clone(),
                local_name,
            });
        }

        // These declarations lead the update block, before the loop-variable prelude and bindings.
        let mut combined = prelude;
        combined.append(&mut self.update_prelude);
        self.update_prelude = combined;
    }

    /// Lower a `@let x = <expr>;` declaration (`ingest.ts` `ingestLetDeclaration` +
    /// `declareLet`/`storeLet`/`readContextLet` lowering, post `optimizeStoreLet` /
    /// `optimizeVariables`).
    ///
    /// Three shapes, selected from [`Self::external_lets`]:
    ///   * **external** (read cross-view) — reserve a `ɵɵdeclareLet(slot)` data slot + one
    ///     `ɵɵstoreLet` var slot; the update emits `ɵɵstoreLet(<value>)`, captured in a `const` when
    ///     the value is also read in this view (so in-view reads reuse it) or left bare otherwise.
    ///     Cross-view reads resolve via `ɵɵreadContextLet(slot)` (see
    ///     [`Self::bring_ancestor_lets_into_scope`]).
    ///   * **pipe-only slot** (non-external, value uses a pipe) — reserve the `ɵɵdeclareLet(slot)`
    ///     for the pipe's DI TNode, but inline the value (no `ɵɵstoreLet`, no var slot).
    ///   * **inlined** (non-external, no pipe) — no slot, no var; the value lowers to a plain
    ///     `const <name> = <value>;` when read in this view, or a bare side-effectful `<value>;`
    ///     statement when unused.
    fn build_let_declaration(&mut self, decl: &LetDeclaration) {
        let external = self.external_lets.get(&decl.name).copied();
        let needs_slot = external.is_some(); // external OR pipe-only both reserve a declareLet slot.
        let is_external = external == Some(true);

        let slot = if needs_slot {
            let slot = self.allocate_data_slot();
            self.creation_code
                .push(instruction(R3::DeclareLet, vec![num(slot as f64)]));
            Some(slot)
        } else {
            None
        };

        // An external let's `ɵɵstoreLet` reserves one var slot (`var_counting` `StoreLet` => 1). This
        // slot is NOT counted as a top-level op var here: in Angular it is a first-pass *expression*
        // consumer (`ExpressionKind.StoreLet`), assigned its slot in expression-traversal order —
        // crucially AFTER any arrow / pure function nested in the stored value (post-order). It is
        // therefore recorded as a deferred [`VarConsumerKind::StoreLet`] consumer right after the value
        // is lowered (below), so the deferred two-pass assignment orders it correctly.

        // Advance to the let's slot before its update binding when it reserved one.
        if let Some(slot) = slot {
            self.advance_to(slot);
            self.current_target_slot = slot;
        }

        // Whether the value is read by THIS view's own update bindings: drives whether a local
        // `const` is generated (a used let keeps its declaration; an unused one degrades to a bare
        // statement that still runs the value for its side effects, the `ɵɵstoreLet` for an external
        // let, the raw expression otherwise).
        let used_in_view = self.let_used_in_view_now(&decl.name);

        // If this external let is visible to descendant views, register it as a context-let so their
        // reads can resolve to `ɵɵreadContextLet(slot)`. (Pipe-only-slot lets are not cross-view.)
        if is_external {
            if let Some(slot) = slot {
                self.context_lets.push(ContextLet {
                    name: decl.name.clone(),
                    slot,
                    owner_level: self.view_level,
                });
            }
        }

        let value = self.lower_expr(&decl.value);
        let value = if is_external {
            // Record the `ɵɵstoreLet` var-slot consumer AFTER the value was lowered, so any arrow /
            // pure function inside the value is recorded (and thus assigned an offset) first — Angular's
            // post-order var assignment. StoreLet emits no offset argument; it only advances the cursor.
            self.pipes
                .borrow_mut()
                .var_consumers
                .push(VarConsumer {
                    kind: VarConsumerKind::StoreLet,
                    slots: 1,
                });
            o::import_expr(R3::StoreLet.reference(), None).call_fn(vec![value], false)
        } else {
            value
        };

        // A pipe in the let's value (`@let result = one | double`) is consumed by this let's
        // `ɵɵdeclareLet` op, which precedes any later text/element ops. Angular allocates data slots
        // by walking the final create-op list in order, so the `ɵɵpipe` op takes the slot immediately
        // after the `ɵɵdeclareLet` — shifting every later op up. Pre-assign those pipe slots
        // positionally NOW (before the next node allocates its slot) so the order is
        // `declareLet(N), pipe(N+1), <next op>(N+2)` and the subsequent `ɵɵadvance` counts are right.
        if let Some(slot) = slot {
            self.assign_positional_pipe_slots_for(slot);
        }

        if used_in_view {
            // `const $<name>_<id>$ = <value-or-storeLet>;` — the in-view local for reads of this let.
            //
            // A `@let`'s in-view local is an *identifier* semantic variable (Angular `generate_
            // variables.ts`), NOT a loop variable. Angular's compliance goldens spell these with the
            // renamable `$<name>_<index>$` expect-emit placeholder (`$result_0$`, `$one_0$`/`$two_1$`/
            // `$result_2$`), the same `$…$` convention this builder already uses for the saved-view
            // (`$s_<id>$`) and local-ref (`$<name>_<id>$`, [`local_ref_var_name`]) view variables —
            // distinct from the loop-variable `_r<n>` view-suffix form. The single generated name is
            // used both for the `const … =` binding and for every read of the let inside this view
            // (resolved through `loop_vars`).
            self.var_counter += 1;
            // Angular's compliance goldens spell EVERY `@let`'s in-view binding const with the
            // renamable `$<name>_<index>$` expect-emit placeholder (`$result_0$`, `$one_0$`,
            // `$result_1$`) — including a let whose value is a hoisted `ɵɵpureFunctionN`/pipe-bind
            // result temp (`let_with_pipe`: `const $result_1$ = ɵɵpipeBind1(1, 1, $one_0$)`). Always
            // use the `$name$` (`local_ref_var_name`) form; the `_r<id>` view-ref scheme is reserved
            // for loop variables, not `@let` identifiers.
            let local_name = local_ref_var_name(&decl.name, self.var_counter);
            self.update_code.push(Stmt::with_modifiers(
                StmtKind::DeclareVar {
                    name: local_name.clone(),
                    value: Some(value),
                    ty: None,
                },
                StmtModifier::FINAL,
            ));
            self.loop_vars.push(LoopVar {
                source_name: decl.name.clone(),
                local_name,
            });
        } else {
            // Unused in this view: the value runs as a bare statement (for its side effects / the
            // cross-view `ɵɵstoreLet`), with no `const` binding (`optimizeVariables`).
            self.update_code.push(value.to_stmt());
        }
    }

    /// Lower a `@defer` block (`ingest.ts` `ingestDeferBlock` + `reify.ts` Defer/DeferOn/Template).
    ///
    /// Slot layout (Angular `slot_allocation`): the **main** deferred view's `ɵɵdomTemplate` op
    /// takes one slot (`mainSlot`), then any `@placeholder`/`@loading`/`@error` sub-block is its own
    /// one-slot `ɵɵdomTemplate`, then the `ɵɵdefer` op takes **two** (`numSlotsUsed: 2`).
    ///
    /// Emits, in create order: the main `ɵɵdomTemplate(mainSlot, fn, decls, vars)`, then each
    /// secondary `ɵɵdomTemplate`, then `ɵɵdefer(deferSlot, mainSlot[, resolverFn, loadingSlot,
    /// placeholderSlot, errorSlot])` with trailing `null`s trimmed, then one trigger instruction per
    /// `on` trigger (defaulting to `ɵɵdeferOnIdle()` when no concrete trigger is given). Deferred
    /// block bodies always compile DOM-only (`reify.ts`: block templates emit `ɵɵdomTemplate`).
    fn build_deferred_block(&mut self, deferred: &DeferredBlock) {
        // Defer timing config arrays (`defer_configs.ts`): a `@placeholder (minimum Nms)` collects a
        // `[minimumTime]` const, a `@loading (minimum / after)` a `[minimumTime, afterTime]` const.
        // These are `ConstCollectedExpr`s gathered by `collectConstExpressions`, which runs BEFORE
        // `collectElementConsts` — so a defer config const precedes every element-attrs const in the
        // pool. Intern them HERE, before building the secondary views (whose element attrs would
        // otherwise be interned first), so the pool order matches Angular
        // (e.g. `consts: [[2000], ["src", "placeholder.gif"]]`).
        let placeholder_config_index = deferred
            .placeholder
            .as_ref()
            .and_then(|ph| ph.minimum_time)
            .map(|t| {
                let arr = o::literal_arr(vec![num(t)], None);
                self.const_pool.intern(arr)
            });
        let loading_config_index = deferred.loading.as_ref().and_then(|ld| {
            if ld.minimum_time.is_some() || ld.after_time.is_some() {
                // `[minimumTime, afterTime]` — absent entries are `null` (kept; the array is 2-wide).
                let arr = o::literal_arr(
                    vec![
                        ld.minimum_time.map(num).unwrap_or_else(o::null_expr),
                        ld.after_time.map(num).unwrap_or_else(o::null_expr),
                    ],
                    None,
                );
                Some(self.const_pool.intern(arr))
            } else {
                None
            }
        });
        // `enableTimerScheduling` is set whenever any timing config exists (`reify.ts`): the runtime
        // needs the timer scheduler function passed as the final `ɵɵdefer` argument.
        let enable_timer_scheduling = placeholder_config_index.is_some()
            || loading_config_index.is_some();

        // Main deferred view — one data slot, named `<Base>_Defer_<mainSlot>_Template` (`naming.ts`).
        let main_slot = self.allocate_data_slot();
        let main_fn = format!("{}_Defer_{}_Template", self.base_name, main_slot);
        let (main_ref, main_decls, main_vars) =
            self.build_deferred_view(main_fn, deferred.children.clone());

        // Secondary views (`@placeholder`/`@loading`/`@error`), each a one-slot `ɵɵdomTemplate`,
        // allocated before the `ɵɵdefer` op (Angular ingests their views ahead of the defer op).
        let mut placeholder: Option<(usize, Expr, usize, usize)> = None;
        let mut loading: Option<(usize, Expr, usize, usize)> = None;
        let mut error: Option<(usize, Expr, usize, usize)> = None;
        // Slot/view allocation order is LOADING, then PLACEHOLDER, then ERROR (Angular
        // `ingestDeferBlock` ingests the loading view before the placeholder view), so a block with
        // all three lays out as `Defer(N), DeferLoading(N+1), DeferPlaceholder(N+2), DeferError(N+3)`.
        if let Some(ld) = &deferred.loading {
            let s = self.allocate_data_slot();
            let f = format!("{}_DeferLoading_{}_Template", self.base_name, s);
            let (r, d, v) = self.build_deferred_view(f, ld.children.clone());
            loading = Some((s, r, d, v));
        }
        if let Some(ph) = &deferred.placeholder {
            let s = self.allocate_data_slot();
            let f = format!("{}_DeferPlaceholder_{}_Template", self.base_name, s);
            let (r, d, v) = self.build_deferred_view(f, ph.children.clone());
            placeholder = Some((s, r, d, v));
        }
        if let Some(er) = &deferred.error {
            let s = self.allocate_data_slot();
            let f = format!("{}_DeferError_{}_Template", self.base_name, s);
            let (r, d, v) = self.build_deferred_view(f, er.children.clone());
            error = Some((s, r, d, v));
        }

        // The defer op itself reserves two slots (`numSlotsUsed: 2`).
        let defer_slot = self.allocate_data_slot();
        let _defer_slot_2 = self.allocate_data_slot();

        let emit_template = |this: &mut Self, slot: usize, r: Expr, d: usize, v: usize| {
            let params = vec![num(slot as f64), r, num(d as f64), num(v as f64)];
            this.creation_code.push(instruction(R3::DomTemplate, params));
        };
        emit_template(self, main_slot, main_ref, main_decls, main_vars);
        if let Some((s, r, d, v)) = &loading {
            emit_template(self, *s, r.clone(), *d, *v);
        }
        if let Some((s, r, d, v)) = &placeholder {
            emit_template(self, *s, r.clone(), *d, *v);
        }
        if let Some((s, r, d, v)) = &error {
            emit_template(self, *s, r.clone(), *d, *v);
        }

        // `ɵɵdefer(selfSlot, primarySlot, dependencyResolverFn, loadingSlot, placeholderSlot,
        // errorSlot, loadingConfig, placeholderConfig, enableTimerScheduling, flags)` with trailing
        // `null`s trimmed (`instruction.ts` `defer`). `loadingConfig`/`placeholderConfig` are the
        // interned timing-config const indices; `enableTimerScheduling` becomes the
        // `ɵɵdeferEnableTimerScheduling` import when any timing config is present (else `null`); the
        // trailing `flags` arg is always `null` here (basic block).
        let mut defer_params = vec![
            num(defer_slot as f64),
            num(main_slot as f64),
            o::null_expr(), // dependencyResolverFn (per-block resolver not modelled here)
            loading
                .as_ref()
                .map(|(s, ..)| num(*s as f64))
                .unwrap_or_else(o::null_expr),
            placeholder
                .as_ref()
                .map(|(s, ..)| num(*s as f64))
                .unwrap_or_else(o::null_expr),
            error
                .as_ref()
                .map(|(s, ..)| num(*s as f64))
                .unwrap_or_else(o::null_expr),
            loading_config_index
                .map(|i| num(i as f64))
                .unwrap_or_else(o::null_expr),
            placeholder_config_index
                .map(|i| num(i as f64))
                .unwrap_or_else(o::null_expr),
            if enable_timer_scheduling {
                o::import_expr(R3::DeferEnableTimerScheduling.reference(), None)
            } else {
                o::null_expr()
            },
            o::null_expr(), // flags (TDeferDetailsFlags) — always null for a basic defer block.
        ];
        trim_trailing_nulls(&mut defer_params);
        self.creation_code.push(instruction(R3::Defer, defer_params));

        // Trigger instructions (regular `on` triggers only; prefetch/hydrate are a separate
        // subsystem). Default to `ɵɵdeferOnIdle()` when no concrete trigger is given.
        self.emit_defer_triggers(&deferred.triggers);
    }

    /// Emit the create-block trigger instructions for a defer block's regular trigger set, defaulting
    /// to `ɵɵdeferOnIdle()` when none is present (`ingestDeferBlock` / `reify.ts` `DeferOn`).
    fn emit_defer_triggers(&mut self, triggers: &DeferredBlockTriggers) {
        let mut emitted_concrete = false;
        for trigger in triggers.defined_in_order() {
            let (reference, args): (R3, Vec<Expr>) = match &trigger.kind {
                DeferredTriggerKind::Idle { timeout } => (
                    R3::DeferOnIdle,
                    timeout.map(|t| vec![num(t)]).unwrap_or_default(),
                ),
                DeferredTriggerKind::Immediate => (R3::DeferOnImmediate, vec![]),
                DeferredTriggerKind::Timer { delay } => (R3::DeferOnTimer, vec![num(*delay)]),
                DeferredTriggerKind::Hover { .. } => (R3::DeferOnHover, vec![]),
                DeferredTriggerKind::Interaction { .. } => (R3::DeferOnInteraction, vec![]),
                DeferredTriggerKind::Viewport { .. } => (R3::DeferOnViewport, vec![]),
                // `when` is a `ɵɵdeferWhen` update op; `never` is hydrate-only — skip here.
                DeferredTriggerKind::When { .. } | DeferredTriggerKind::Never => continue,
            };
            emitted_concrete = true;
            self.creation_code.push(instruction(reference, args));
        }
        if !emitted_concrete {
            self.creation_code.push(instruction(R3::DeferOnIdle, vec![]));
        }
    }

    /// Build a `@defer` secondary/main view as a hoisted, named DOM-only embedded view, returning
    /// `(fnNameRef, decls, vars)`. Like [`Self::build_embedded_view`] but the body always compiles
    /// DOM-only (`reify.ts`: block templates are `ɵɵdomTemplate`).
    fn build_deferred_view(&mut self, fn_name: String, children: Vec<Node>) -> (Expr, usize, usize) {
        let nested_input =
            TemplateCompilationInput::new(fn_name.clone(), children).with_dom_only(true);
        let mut nested = TemplateDefinitionBuilder::new(&nested_input);
        nested.is_root = false;
        nested.view_level = self.view_level + 1;
        nested.var_counter = self.var_counter;
        nested.context_lets = self.context_lets.clone();
        nested.base_name = self.base_name.clone();
        let tmpl_fn = nested.build_template_function(&nested_input);
        let decls = nested.data_index;
        let vars = nested.binding_slots;
        self.var_counter = nested.var_counter;
        for entry in nested.const_pool.entries() {
            self.const_pool.intern(entry.clone());
        }
        self.hoisted_fns.append(&mut nested.hoisted_fns);
        self.hoisted_fns
            .push(declare_function_from(&fn_name, tmpl_fn));
        (o::variable(fn_name, None), decls, vars)
    }

    /// Whether a top-level `@let` named `name` is referenced by this view's own update bindings,
    /// using the source node tree the walk is processing. Captured at declaration time from the
    /// current input nodes (stored on `self`); falls back to `false` when unavailable.
    fn let_used_in_view_now(&self, name: &str) -> bool {
        if let Some((idx, nodes)) = self
            .current_nodes
            .as_ref()
            .and_then(|nodes| nodes.iter().position(|n| matches!(n, Node::LetDeclaration(l) if l.name == name)).map(|i| (i, nodes)))
        {
            let_used_in_view(nodes, idx, name)
        } else {
            false
        }
    }
}

impl Visitor for TemplateDefinitionBuilder {
    // Text, bound text, elements, `<ng-content>` projection, `@if`/`@switch`/`@for` control flow,
    // `<ng-template>`/structural templates and `@let` declarations are lowered directly here. The
    // remaining node kinds — `@defer` blocks and their sub-blocks, ICU expansions and `UnknownBlock`
    // — fall back to the default recursive traversal, which visits their children (so any plain
    // text/element/binding inside still lowers) without emitting the kind's own dedicated
    // instructions. Those dedicated lowerings (`ɵɵdefer*`, ICU `ɵɵi18n*`) are separate subsystems.
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

    fn visit_let_declaration(&mut self, decl: &LetDeclaration) {
        self.build_let_declaration(decl);
    }

    fn visit_content(&mut self, content: &Content) {
        self.build_content(content);
    }

    fn visit_deferred_block(&mut self, deferred: &DeferredBlock) {
        self.build_deferred_block(deferred);
    }
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
/// The `ɵɵattribute(...)` sanitizer for a bound `[attr.name]` binding, per Angular's
/// `resolve_sanitizers.ts` (the security context selects the sanitizer function). Returns `None`
/// when no sanitization applies. The security context normally comes from the schema; the upstream
/// transform records `None` for `attr.*`, so the well-known sanitized attribute names are recovered
/// here (`style` → STYLE, URL/resource-URL attributes → URL) to match Angular's emitted output.
fn attribute_sanitizer(
    security_context: &crate::expression::ast::SecurityContext,
    name: &str,
) -> Option<R3> {
    use crate::expression::ast::SecurityContext;
    let ctx = match security_context {
        SecurityContext::None => match name {
            "style" => SecurityContext::Style,
            _ => SecurityContext::None,
        },
        other => other.clone(),
    };
    match ctx {
        SecurityContext::Style => Some(R3::SanitizeStyle),
        SecurityContext::Html => Some(R3::SanitizeHtml),
        SecurityContext::Script => Some(R3::SanitizeScript),
        SecurityContext::Url => Some(R3::SanitizeUrl),
        SecurityContext::ResourceUrl => Some(R3::SanitizeResourceUrl),
        SecurityContext::None => None,
    }
}

fn update_order_rank(input: &BoundAttribute) -> u8 {
    use crate::expression::ast::BindingType;
    let is_interp = matches!(input.value.kind, AstExprKind::Interpolation { .. });
    match input.kind {
        // A whole-element `[style]="exp"` / `[class]="exp"` binding (a `Property` whose name is
        // exactly `style`/`class`) is a StyleMap / ClassMap — Angular's `UPDATE_ORDERING` groups 0/1,
        // ahead of the per-key StyleProp/ClassProp groups.
        BindingType::Property | BindingType::TwoWay if input.name == "style" => 0,
        BindingType::Property | BindingType::TwoWay if input.name == "class" => 1,
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
// `@let` declaration analysis (Angular `optimizeStoreLet` / `optimizeVariables`).
//
// For every `@let` declared at a view's top level we must decide, faithfully:
//   * `external`  — the value is read from a *different* view (a listener handler in
//                   this view, or any descendant embedded view) and therefore must be
//                   stored cross-view via `ɵɵdeclareLet` + `ɵɵstoreLet`, read back with
//                   `ɵɵreadContextLet`.
//   * `has_pipe`  — the value uses a pipe, which forces the `ɵɵdeclareLet` TNode to be
//                   retained even when the let is not external (the pipe needs the slot
//                   for DI).
//   * `in_view_uses` — whether the let is referenced anywhere in its OWN view's update
//                   bindings (subsequent let initializers, bound text, element/template
//                   inputs, control-flow conditions). Drives whether a local `const` is
//                   generated for it (`generateLocalLetReferences` + `optimizeVariables`
//                   keep the declaration only when something reads it locally; an unused
//                   let is reduced to a bare side-effectful statement).
// Shadowing: a descendant or sibling view that re-declares the same name shadows this
// one, so references inside the shadowing subtree do not count toward this let.
// ---------------------------------------------------------------------------

/// Whether `name` is read from a *cross-view* context relative to the view whose top-level
/// `nodes` are given: inside any element/template/component/directive output handler (a
/// listener callback) or inside any descendant embedded view (`@if`/`@for`/`@switch`/`@defer`
/// bodies and `<ng-template>` children). References in the same view's own update bindings do
/// NOT count. Stops descending into a subtree that re-declares (shadows) `name`.
fn let_used_externally(nodes: &[Node], name: &str) -> bool {
    nodes.iter().any(|n| cross_view_node_uses(n, name))
}

/// Scan a list of children that constitute a SEPARATE (embedded) view: any read of `name` there
/// (own bindings, listeners, or deeper embedded views) is a cross-view use — unless the embedded
/// view re-declares the name (shadowing), in which case the outer let is not consumed by it.
fn embedded_view_uses(children: &[Node], name: &str) -> bool {
    if declares_let_at_top(children, name) {
        return false;
    }
    children
        .iter()
        .any(|n| same_view_node_references(n, name) || cross_view_node_uses(n, name))
}

/// Whether `node` reaches a cross-view read of `name`: through a listener handler attached at or
/// beneath it (same view, but the handler is a separate callback) or through an embedded view
/// beneath it. Plain element/component children remain in the same view, so recurse into them to
/// reach their listeners and nested blocks.
fn cross_view_node_uses(node: &Node, name: &str) -> bool {
    match node {
        Node::Element(el) => {
            el.outputs.iter().any(|o| expr_references_implicit(&o.handler, name))
                || el
                    .directives
                    .iter()
                    .any(|d| d.outputs.iter().any(|o| expr_references_implicit(&o.handler, name)))
                || el.children.iter().any(|c| cross_view_node_uses(c, name))
        }
        Node::Component(c) => {
            c.outputs.iter().any(|o| expr_references_implicit(&o.handler, name))
                || c.children.iter().any(|ch| cross_view_node_uses(ch, name))
        }
        // `<ng-template>` children form an embedded view; its own outputs are listeners.
        Node::Template(t) => {
            t.outputs.iter().any(|o| expr_references_implicit(&o.handler, name))
                || embedded_view_uses(&t.children, name)
        }
        Node::IfBlock(b) => b.branches.iter().any(|br| embedded_view_uses(&br.children, name)),
        Node::SwitchBlock(b) => b.groups.iter().any(|g| embedded_view_uses(&g.children, name)),
        Node::ForLoopBlock(b) => {
            embedded_view_uses(&b.children, name)
                || b.empty.as_ref().is_some_and(|e| embedded_view_uses(&e.children, name))
        }
        Node::DeferredBlock(b) => embedded_view_uses(&b.children, name),
        Node::DeferredBlockPlaceholder(b) => embedded_view_uses(&b.children, name),
        Node::DeferredBlockLoading(b) => embedded_view_uses(&b.children, name),
        Node::DeferredBlockError(b) => embedded_view_uses(&b.children, name),
        _ => false,
    }
}

/// Whether `name` is referenced in the SAME view's own update bindings: the initializers of
/// later top-level `@let`s, bound text, element/template/component input bindings, and the
/// expressions of control-flow blocks (`@if` conditions, `@switch`/`@for` expressions). Reads
/// inside listeners or embedded views are NOT counted (those are cross-view). Used to decide
/// whether to emit a local `const` for the let.
fn let_used_in_view(nodes: &[Node], decl_index: usize, name: &str) -> bool {
    for (i, n) in nodes.iter().enumerate() {
        match n {
            Node::LetDeclaration(l) => {
                // A later sibling let whose initializer reads `name` is an in-view use; but a
                // later let that *re-declares* `name` shadows this one for everything after it.
                if i > decl_index {
                    if expr_references_implicit(&l.value, name) {
                        return true;
                    }
                    if l.name == name {
                        // Re-declaration shadows the rest of this view.
                        return false;
                    }
                }
            }
            _ => {
                if same_view_node_references(n, name) {
                    return true;
                }
            }
        }
    }
    false
}

/// Whether a node references `name` through THIS view's own update bindings only (bound text,
/// inputs, control-flow condition/collection expressions) — NOT through listeners or embedded
/// views. Plain element children stay in the same view, so recurse into them.
fn same_view_node_references(node: &Node, name: &str) -> bool {
    match node {
        Node::BoundText(bt) => expr_references_implicit(&bt.value, name),
        // A `@let other = … name …;` initializer evaluates in this view and reads `name`. Without
        // this arm an `@let` consumed only by another `@let` in a descendant view (e.g.
        // `@let two = one + 1;` inside an `@if`, reading the parent's `@let one`) would be missed by
        // the cross-view analysis, so the parent let would not be hoisted (`ɵɵdeclareLet`/`storeLet`
        // dropped) and a stray bare `1;` statement would remain.
        Node::LetDeclaration(l) => expr_references_implicit(&l.value, name),
        Node::Element(el) => {
            el.inputs.iter().any(|i| expr_references_implicit(&i.value, name))
                || el.directives.iter().any(|d| {
                    d.inputs.iter().any(|i| expr_references_implicit(&i.value, name))
                })
                || el.children.iter().any(|c| same_view_node_references(c, name))
        }
        Node::Component(c) => {
            c.inputs.iter().any(|i| expr_references_implicit(&i.value, name))
                || c.children.iter().any(|ch| same_view_node_references(ch, name))
        }
        // A `<ng-template>` / control-flow block: only its *own* binding inputs / condition
        // expressions evaluate in this view; its children are a separate view (handled by
        // `let_used_externally`).
        Node::Template(t) => t.inputs.iter().any(|i| expr_references_implicit(&i.value, name)),
        Node::IfBlock(b) => b.branches.iter().any(|br| {
            br.expression
                .as_ref()
                .is_some_and(|e| expr_references_implicit(e, name))
        }),
        Node::SwitchBlock(b) => {
            expr_references_implicit(&b.expression, name)
                || b.groups.iter().any(|g| {
                    g.cases.iter().any(|c| {
                        c.expression
                            .as_ref()
                            .is_some_and(|e| expr_references_implicit(e, name))
                    })
                })
        }
        Node::ForLoopBlock(b) => {
            expr_references_implicit(&b.expression.ast, name)
                || b.track_by.as_ref().is_some_and(|t| expr_references_implicit(&t.ast, name))
        }
        _ => false,
    }
}

/// Whether the top level of `children` declares a `@let` named `name` (used for shadowing
/// detection: a view that re-declares the name does not consume the outer one).
fn declares_let_at_top(children: &[Node], name: &str) -> bool {
    children
        .iter()
        .any(|n| matches!(n, Node::LetDeclaration(l) if l.name == name))
}

/// Whether a `@let` value expression uses a pipe (`exp | name`). Angular keeps the `ɵɵdeclareLet`
/// TNode for a non-external let when its value contains a pipe (the pipe needs the slot for DI).
fn let_value_has_pipe(node: &AstNode) -> bool {
    use AstExprKind as EK;
    match &node.kind {
        EK::BindingPipe { .. } => true,
        EK::EmptyExpr
        | EK::ImplicitReceiver
        | EK::ThisReceiver
        | EK::LiteralPrimitive { .. }
        | EK::TemplateLiteralElement { .. }
        | EK::RegularExpressionLiteral { .. } => false,
        EK::Chain { expressions }
        | EK::LiteralArray { expressions }
        | EK::Interpolation { expressions, .. } => expressions.iter().any(let_value_has_pipe),
        EK::Conditional { condition, true_exp, false_exp } => {
            let_value_has_pipe(condition) || let_value_has_pipe(true_exp) || let_value_has_pipe(false_exp)
        }
        EK::PropertyRead { receiver, .. } | EK::SafePropertyRead { receiver, .. } => {
            let_value_has_pipe(receiver)
        }
        EK::KeyedRead { receiver, key } | EK::SafeKeyedRead { receiver, key } => {
            let_value_has_pipe(receiver) || let_value_has_pipe(key)
        }
        EK::SpreadElement { expression }
        | EK::PrefixNot { expression }
        | EK::TypeofExpression { expression }
        | EK::VoidExpression { expression }
        | EK::NonNullAssert { expression }
        | EK::ParenthesizedExpression { expression } => let_value_has_pipe(expression),
        EK::LiteralMap { values, .. } => values.iter().any(let_value_has_pipe),
        EK::Binary { left, right, .. } => let_value_has_pipe(left) || let_value_has_pipe(right),
        EK::Unary { expr, .. } => let_value_has_pipe(expr),
        EK::Call { receiver, args, .. } | EK::SafeCall { receiver, args, .. } => {
            let_value_has_pipe(receiver) || args.iter().any(let_value_has_pipe)
        }
        EK::TaggedTemplateLiteral { tag, template } => {
            let_value_has_pipe(tag) || let_value_has_pipe(template)
        }
        EK::TemplateLiteral { expressions, .. } => expressions.iter().any(let_value_has_pipe),
        EK::ArrowFunction { body, .. } => let_value_has_pipe(body),
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

/// Build the value-interpolation call for a `[style.x]`/`[class.x]` (or any non-text)
/// interpolation: `ɵɵinterpolate{N}(s0, e0, s1, …, sN)` (Angular `interpolate.ts`, the *value*
/// family — `ɵɵinterpolate1` etc.). `strings` are the literal affixes, `lowered` the already-lowered
/// expressions; collation mirrors `text_interpolation_call` (single empty-affix collapse, trailing
/// empty-string trim, arity-selected instruction, `…V` variadic for >8 expressions).
fn value_interpolation_call(strings: &[String], lowered: &[Expr]) -> (R3, Vec<Expr>) {
    let mut args: Vec<Expr> =
        if lowered.len() == 1 && strings.len() == 2 && strings[0].is_empty() && strings[1].is_empty()
        {
            vec![lowered[0].clone()]
        } else {
            let mut out = Vec::with_capacity(strings.len() + lowered.len());
            for (idx, expr) in lowered.iter().enumerate() {
                out.push(str_lit(strings.get(idx).map(String::as_str).unwrap_or("")));
                out.push(expr.clone());
            }
            out.push(str_lit(strings.last().map(String::as_str).unwrap_or("")));
            out
        };

    let count = if args.len() == 1 { 0 } else { (args.len() - 1) / 2 };

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
        0 => (R3::Interpolate, args),
        1 => (R3::Interpolate1, args),
        2 => (R3::Interpolate2, args),
        3 => (R3::Interpolate3, args),
        4 => (R3::Interpolate4, args),
        5 => (R3::Interpolate5, args),
        6 => (R3::Interpolate6, args),
        7 => (R3::Interpolate7, args),
        8 => (R3::Interpolate8, args),
        _ => (R3::InterpolateV, vec![o::literal_arr(args, None)]),
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
fn remap_pipe_slots_in_stmt(stmt: &mut Stmt, slots: &[usize]) {
    remap_placeholder_slots_in_stmt(stmt, PIPE_SLOT_PLACEHOLDER, &|ordinal| slots[ordinal]);
}

/// Rewrite every placeholder slot literal `>= placeholder` reachable from `stmt`, mapping its
/// ordinal (`literal - placeholder`) through `resolve`. Shared by pipe-slot finalisation and
/// local-ref-slot finalisation (which differ only in their placeholder base + resolver).
fn remap_placeholder_slots_in_stmt(
    stmt: &mut Stmt,
    placeholder: usize,
    resolve: &dyn Fn(usize) -> usize,
) {
    match &mut stmt.kind {
        StmtKind::DeclareVar { value, .. } => {
            if let Some(v) = value {
                remap_placeholder_slots_in_expr(v, placeholder, resolve);
            }
        }
        StmtKind::DeclareFunction { statements, .. } => {
            for s in statements {
                remap_placeholder_slots_in_stmt(s, placeholder, resolve);
            }
        }
        StmtKind::Expression(e) | StmtKind::Return(e) => {
            remap_placeholder_slots_in_expr(e, placeholder, resolve);
        }
        StmtKind::If {
            condition,
            true_case,
            false_case,
        } => {
            remap_placeholder_slots_in_expr(condition, placeholder, resolve);
            for s in true_case {
                remap_placeholder_slots_in_stmt(s, placeholder, resolve);
            }
            for s in false_case {
                remap_placeholder_slots_in_stmt(s, placeholder, resolve);
            }
        }
    }
}

/// Recursively rewrite placeholder slot literals in `expr`. A pipe's / local-ref's data slot is
/// emitted as `placeholder + ordinal` during the walk (the real slot is unknown until every other
/// data slot is allocated); here we map its ordinal through `resolve`. The sentinel base is far
/// above any real slot count, so the `>= placeholder` test never matches a genuine slot literal.
fn remap_placeholder_slots_in_expr(
    expr: &mut Expr,
    placeholder: usize,
    resolve: &dyn Fn(usize) -> usize,
) {
    use o::ExprKind as K;
    let recur = |e: &mut Expr| remap_placeholder_slots_in_expr(e, placeholder, resolve);
    match &mut expr.kind {
        K::Literal(o::LiteralValue::Number(n)) => {
            let v = *n as usize;
            if *n >= placeholder as f64 && v >= placeholder {
                let ordinal = v - placeholder;
                *n = resolve(ordinal) as f64;
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
            recur(e);
        }
        K::Invoke { callee, args, .. } => {
            recur(callee);
            for a in args {
                recur(a);
            }
        }
        K::TaggedTemplate { tag, template } => {
            recur(tag);
            recur(template);
        }
        K::New { class_expr, args } => {
            recur(class_expr);
            for a in args {
                recur(a);
            }
        }
        K::TemplateLiteral { expressions, .. } | K::LocalizedString { expressions, .. } => {
            for e in expressions {
                recur(e);
            }
        }
        K::Conditional {
            condition,
            true_case,
            false_case,
        } => {
            recur(condition);
            recur(true_case);
            if let Some(f) = false_case {
                recur(f);
            }
        }
        K::DynamicImport { url, .. } => {
            if let o::ImportUrl::Expr(e) = url {
                recur(e);
            }
        }
        K::Function { statements, .. } => {
            for s in statements {
                remap_placeholder_slots_in_stmt(s, placeholder, resolve);
            }
        }
        K::Arrow { body, .. } => match body {
            o::ArrowBody::Expr(e) => recur(e),
            o::ArrowBody::Block(stmts) => {
                for s in stmts {
                    remap_placeholder_slots_in_stmt(s, placeholder, resolve);
                }
            }
        },
        K::Unary { expr: inner, .. } => recur(inner),
        K::Binary { lhs, rhs, .. } => {
            recur(lhs);
            recur(rhs);
        }
        K::ReadProp { receiver, .. } => recur(receiver),
        K::ReadKey { receiver, index, .. } => {
            recur(receiver);
            recur(index);
        }
        K::LiteralArray(entries) | K::Comma(entries) => {
            for e in entries {
                recur(e);
            }
        }
        K::LiteralMap { entries, .. } => {
            for entry in entries {
                match entry {
                    o::LiteralMapEntry::Property { value, .. } => recur(value),
                    o::LiteralMapEntry::Spread { expression } => recur(expression),
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

    /// Emit the root view function PLUS its hoisted nested-view functions concatenated, mirroring
    /// the real component definition where Angular prints every `function …_Template(rf, ctx) {…}`
    /// on `ConstantPool.statements` as a sibling of (and before) the `ɵɵdefineComponent` call. The
    /// root view body itself no longer inlines them, so tests that assert on nested-fn shape look at
    /// this combined text.
    fn emit_with_hoisted(builder: &TemplateDefinitionBuilder, func: &Expr) -> String {
        let mut out = emit_expression(func);
        for stmt in builder.hoisted_functions() {
            out.push('\n');
            out.push_str(&crate::output::emitter::emit_statements(std::slice::from_ref(stmt)));
        }
        out
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

    #[test]
    fn full_mode_selects_classic_element_family() {
        // Angular compiles a non-standalone component (or one with directive dependencies) in `Full`
        // mode (`render3/view/compiler.ts`), where `reify` selects the classic
        // `ɵɵelementStart`/`ɵɵelementEnd` family instead of the DomOnly `ɵɵdomElement*` family.
        let input =
            TemplateCompilationInput::new("Test_Template", div_with_interpolation()).with_dom_only(false);
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let func = builder.build_template_function(&input);
        let out = emit_expression(&func);

        assert!(out.contains("\u{0275}\u{0275}elementStart(0, \"div\")"), "got: {out}");
        assert!(out.contains("\u{0275}\u{0275}elementEnd()"), "got: {out}");
        // The DomOnly family must NOT appear in Full mode.
        assert!(!out.contains("\u{0275}\u{0275}domElement"), "got: {out}");
    }

    #[test]
    fn full_mode_property_uses_classic_property_instruction() {
        // A `[id]="x"` property binding on a Full-mode element reifies to the classic `ɵɵproperty`
        // instruction (not the DomOnly `ɵɵdomProperty`).
        let nodes = element_with_input("div", "id", BindingType::Property, prop_read("x"));
        let input = TemplateCompilationInput::new("Test_Template", nodes).with_dom_only(false);
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let func = builder.build_template_function(&input);
        let out = emit_expression(&func);

        assert!(out.contains("\u{0275}\u{0275}property(\"id\""), "got: {out}");
        assert!(!out.contains("\u{0275}\u{0275}domProperty"), "got: {out}");
    }

    #[test]
    fn full_mode_aria_property_uses_aria_property_instruction() {
        // In Full mode an `[aria-*]` property binding reifies to `ɵɵariaProperty` (`reify.ts`
        // `reifyProperty` → `isAriaAttribute`), unlike DomOnly which keeps `ɵɵdomProperty`.
        let nodes = element_with_input("div", "aria-label", BindingType::Property, prop_read("x"));
        let input = TemplateCompilationInput::new("Test_Template", nodes).with_dom_only(false);
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let func = builder.build_template_function(&input);
        let out = emit_expression(&func);

        assert!(out.contains("\u{0275}\u{0275}ariaProperty(\"aria-label\""), "got: {out}");
    }

    #[test]
    fn whole_element_class_binding_lowers_to_class_map() {
        // A whole-element `[class]="x"` binding (a `Property` whose name is exactly `class`) is
        // specialized to a `ɵɵclassMap(x)` op by `style_binding_specialization.ts` — which runs
        // unconditionally, BEFORE the DomOnly `class`→`className` DOM-property remapping — so it is
        // a ClassMap (NOT a `ɵɵdomProperty("className")`) even in DomOnly mode. (The `class_binding`
        // compliance golden confirms: `[class]="myClassExp"` → `ɵɵclassMap(ctx.myClassExp)`.)
        let nodes = element_with_input("div", "class", BindingType::Property, prop_read("x"));
        let input = TemplateCompilationInput::new("Test_Template", nodes); // dom_only defaults to true
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let func = builder.build_template_function(&input);
        let out = emit_expression(&func);

        assert!(out.contains("\u{0275}\u{0275}classMap(ctx.x)"), "got: {out}");
        assert!(!out.contains("\u{0275}\u{0275}domProperty"), "got: {out}");
    }

    #[test]
    fn in_view_let_declaration_inlines_as_const_without_slot() {
        use crate::expression::ast::LiteralValue as ELit;
        use crate::template::r3_ast::LetDeclaration;

        // `@let x = 1; {{ x }}` — the let declaration followed by an interpolation reading it.
        let value = AstNode::new(
            ParseSpan::new(0, 0),
            AbsoluteSourceSpan::new(0, 0),
            AstExprKind::LiteralPrimitive {
                value: ELit::Num(1.0),
            },
        );
        let nodes = vec![
            Node::LetDeclaration(LetDeclaration {
                name: "x".to_string(),
                value,
                source_span: t_span(),
                name_span: t_span(),
                value_span: t_span(),
            }),
            Node::BoundText(BoundText {
                value: prop_read_x(),
                source_span: t_span(),
                i18n: None,
            }),
        ];

        let input = TemplateCompilationInput::new("Test_Template", nodes);
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let func = builder.build_template_function(&input);
        let out = emit_expression(&func);

        // A `@let` read only within its own view is NOT external: `optimizeStoreLet` drops the
        // `ɵɵstoreLet` wrapper and `ɵɵdeclareLet` op entirely, so the value inlines as a plain
        // `const $x_1$ = 1;` in the update block (Angular `simple_let` golden, whose in-view let
        // local is the renamable `$<name>_<index>$` identifier placeholder).
        assert!(!out.contains("\u{0275}\u{0275}declareLet"), "got: {out}");
        assert!(!out.contains("\u{0275}\u{0275}storeLet"), "got: {out}");
        assert!(out.contains("const $x_1$ = 1"), "got: {out}");
        // The interpolation reads the inlined let local, NOT `ctx.x`.
        assert!(out.contains("\u{0275}\u{0275}textInterpolate"), "got: {out}");
        assert!(out.contains("$x_1$"), "got: {out}");
        assert!(!out.contains("ctx.x"), "got: {out}");
        // No `ɵɵdeclareLet` slot and no `ɵɵstoreLet` var: only the text node (1 decl) and the
        // interpolation (1 var) remain.
        assert_eq!(builder.data_index(), 1, "decls: {out}");
        assert_eq!(builder.vars(), 1, "vars: {out}");
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
        //   creation: ɵɵdomElementStart(0,"div"); ɵɵtext(1); ɵɵpipe(2,"uppercase"); ɵɵdomElementEnd();
        //             — the pipe create op is sequenced right after the text it feeds, INSIDE the
        //               host element's creation block (before its `ɵɵdomElementEnd`), matching Angular
        //               `pipe_creation.ts::addPipeToCreationBlock`.
        //   update:   ɵɵpipeBind1(N, varOffset, ctx.x)
        let input =
            TemplateCompilationInput::new("Test_Template", interpolation_with_pipe("uppercase", vec![]));
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let func = builder.build_template_function(&input);
        let out = emit_expression(&func);

        // The pipe creation instruction with the pipe name.
        assert!(out.contains("\u{0275}\u{0275}pipe("), "missing ɵɵpipe, got: {out}");
        assert!(out.contains("\"uppercase\""), "missing pipe name, got: {out}");

        // Creation-block ORDERING: domElementStart, then text, then pipe, then domElementEnd. The
        // `ɵɵpipe(...)` must land INSIDE the element block (before `ɵɵdomElementEnd`), not appended
        // after it — the bug this fix targets.
        let pos = |needle: &str| out.find(needle).unwrap_or_else(|| panic!("missing {needle}, got: {out}"));
        let start = pos("\u{0275}\u{0275}domElementStart");
        let text = pos("\u{0275}\u{0275}text(");
        let pipe = pos("\u{0275}\u{0275}pipe(");
        let end = pos("\u{0275}\u{0275}domElementEnd");
        assert!(
            start < text && text < pipe && pipe < end,
            "expected order domElementStart < text < pipe < domElementEnd, got: {out}"
        );
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
        let out = emit_with_hoisted(&builder, &func);

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
        let out = emit_with_hoisted(&builder, &func);

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
        let out = emit_with_hoisted(&builder, &func);

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
        let out = emit_with_hoisted(&builder, &func);

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
        let func = builder.build_template_function(&input);
        let out = emit_with_hoisted(&builder, &func);

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
        let func = builder.build_template_function(&input);
        let out = emit_with_hoisted(&builder, &func);

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

    // -----------------------------------------------------------------------
    // i18n wiring.
    // -----------------------------------------------------------------------

    /// `<div i18n>Hello</div>` — a static i18n element (text only, no interpolation).
    fn i18n_static_div() -> Vec<Node> {
        use crate::template::r3_ast::I18nMeta;
        vec![Node::Element(Element {
            name: "div".to_string(),
            attributes: vec![],
            inputs: vec![],
            outputs: vec![],
            directives: vec![],
            children: vec![Node::Text(Text {
                value: "Hello".to_string(),
                source_span: t_span(),
            })],
            references: vec![],
            is_self_closing: false,
            source_span: t_span(),
            start_source_span: t_span(),
            end_source_span: None,
            is_void: false,
            i18n: Some(I18nMeta),
        })]
    }

    /// `<div i18n>Hello {{name}}</div>` — an i18n element with one interpolation.
    fn i18n_interp_div() -> Vec<Node> {
        use crate::expression::ast::ExprKind as EK;
        use crate::template::r3_ast::I18nMeta;
        let ab = || AbsoluteSourceSpan::new(0, 0);
        let sp = || ParseSpan::new(0, 0);
        // `name` implicit-receiver read.
        let implicit = AstNode::new(sp(), ab(), EK::ImplicitReceiver);
        let name_read = AstNode::new(
            sp(),
            ab(),
            EK::PropertyRead {
                name_span: ab(),
                receiver: Box::new(implicit),
                name: "name".to_string(),
            },
        );
        let interp = AstNode::new(
            sp(),
            ab(),
            EK::Interpolation {
                strings: vec!["Hello ".to_string(), "".to_string()],
                expressions: vec![name_read],
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
            i18n: Some(I18nMeta),
        })]
    }

    #[test]
    fn i18n_static_emits_i18n_start_end() {
        let input = TemplateCompilationInput::new("Test_Template", i18n_static_div());
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let func = builder.build_template_function(&input);
        let out = emit_expression(&func);

        // The element wrapper plus the i18n block creation instructions.
        assert!(out.contains("\u{0275}\u{0275}domElementStart"), "got: {out}");
        assert!(out.contains("\u{0275}\u{0275}i18nStart("), "missing ɵɵi18nStart, got: {out}");
        assert!(out.contains("\u{0275}\u{0275}i18nEnd("), "missing ɵɵi18nEnd, got: {out}");
        assert!(out.contains("\u{0275}\u{0275}domElementEnd"), "got: {out}");
        // No interpolation → no i18nExp / i18nApply.
        assert!(!out.contains("\u{0275}\u{0275}i18nExp"), "unexpected ɵɵi18nExp, got: {out}");
        // i18nStart/i18nEnd must land INSIDE the element block.
        let pos = |n: &str| out.find(n).unwrap_or_else(|| panic!("missing {n}, got: {out}"));
        assert!(
            pos("\u{0275}\u{0275}domElementStart")
                < pos("\u{0275}\u{0275}i18nStart(")
                && pos("\u{0275}\u{0275}i18nStart(") < pos("\u{0275}\u{0275}i18nEnd(")
                && pos("\u{0275}\u{0275}i18nEnd(") < pos("\u{0275}\u{0275}domElementEnd"),
            "expected domElementStart < i18nStart < i18nEnd < domElementEnd, got: {out}"
        );
        // Slots: div (0) + i18n block (1). The message is interned into the const pool.
        assert_eq!(builder.data_index(), 2, "expected element + i18n block slots");
        assert_eq!(builder.const_pool().entries().len(), 1, "message const");
        // The const-index argument of i18nStart is the message's const-pool slot (0).
        assert!(out.contains("\u{0275}\u{0275}i18nStart(1, 0)"), "got: {out}");
    }

    #[test]
    fn i18n_interpolation_emits_i18n_exp_and_apply() {
        let input = TemplateCompilationInput::new("Test_Template", i18n_interp_div());
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let func = builder.build_template_function(&input);
        let out = emit_expression(&func);

        // Creation: i18nStart/i18nEnd around the element content.
        assert!(out.contains("\u{0275}\u{0275}i18nStart("), "missing ɵɵi18nStart, got: {out}");
        assert!(out.contains("\u{0275}\u{0275}i18nEnd("), "missing ɵɵi18nEnd, got: {out}");
        // Update: i18nExp for the interpolation operand, then i18nApply, with the bound expr on ctx.
        assert!(out.contains("\u{0275}\u{0275}i18nExp("), "missing ɵɵi18nExp, got: {out}");
        assert!(out.contains("\u{0275}\u{0275}i18nApply("), "missing ɵɵi18nApply, got: {out}");
        assert!(out.contains("ctx.name"), "interpolation operand should resolve to ctx, got: {out}");
        // The normal text-interpolation path must NOT be used for i18n content.
        assert!(!out.contains("\u{0275}\u{0275}textInterpolate"), "unexpected textInterpolate, got: {out}");
        // i18nExp precedes i18nApply in the update block.
        let pos = |n: &str| out.find(n).unwrap_or_else(|| panic!("missing {n}, got: {out}"));
        assert!(
            pos("\u{0275}\u{0275}i18nExp(") < pos("\u{0275}\u{0275}i18nApply("),
            "expected i18nExp before i18nApply, got: {out}"
        );
        // One interpolation reserves one var slot.
        assert_eq!(builder.vars(), 1, "expected one i18nExp var slot");
    }

    // -- ng-content projection, custom trackBy, multi-label switch, animation bindings. --

    use crate::template::r3_ast::Content;

    fn content(selector: &str) -> Content {
        Content {
            selector: selector.to_string(),
            attributes: vec![],
            children: vec![],
            is_self_closing: true,
            source_span: t_span(),
            start_source_span: t_span(),
            end_source_span: None,
            i18n: None,
        }
    }

    #[test]
    fn default_ng_content_emits_projection_def_and_projection() {
        // `<ng-content></ng-content>` (default catch-all slot).
        let input = TemplateCompilationInput::new(
            "Test_Template",
            vec![Node::Content(content("*"))],
        );
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let func = builder.build_template_function(&input);
        let out = emit_expression(&func);

        // A single default slot: bare `ɵɵprojectionDef()` (no selector arg) then `ɵɵprojection(0)`
        // (the default projectionSlotIndex 0 is elided).
        assert!(out.contains("\u{0275}\u{0275}projectionDef()"), "got: {out}");
        assert!(out.contains("\u{0275}\u{0275}projection(0)"), "got: {out}");
        // The projection slot consumes one decl slot.
        assert_eq!(builder.data_index(), 1, "got: {out}");
    }

    #[test]
    fn named_ng_content_selectors_index_one_based_and_passes_const() {
        // `<ng-content></ng-content> <ng-content select="header"></ng-content>` — default at index 0,
        // the named selector at index 1, and `ɵɵprojectionDef` receives the selector-list const.
        let input = TemplateCompilationInput::new(
            "Test_Template",
            vec![
                Node::Content(content("*")),
                Node::Content(content("header")),
            ],
        );
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let func = builder.build_template_function(&input);
        let out = emit_expression(&func);

        // `projectionDef($c0$)` references the SHARED-pool selector array (Angular
        // `generateProjectionDefs` → `getConstLiteral(asLiteral(parsed), true)`), NOT a `consts:`
        // index. The default slot emits `ɵɵprojection(0)` (index elided) and the named slot
        // `ɵɵprojection(1, 1)`. `ɵɵprojection` is NOT in Angular's `CHAIN_COMPATIBILITY` map, so the
        // two adjacent projection create ops stay as SEPARATE statements (faithful to Angular's
        // `content_projection` goldens, which emit `ɵɵprojection(0); ɵɵprojection(1, 1);` un-chained).
        assert!(out.contains("\u{0275}\u{0275}projectionDef($c0$)"), "got: {out}");
        assert!(out.contains("\u{0275}\u{0275}projection(0)"), "got: {out}");
        assert!(out.contains("\u{0275}\u{0275}projection(1, 1)"), "got: {out}");
        assert!(!out.contains("\u{0275}\u{0275}projection(0)(1, 1)"), "got: {out}");
        // The parsed selector array is hoisted as a top-level shared `const $c0$ = [...]`, NOT pushed
        // into the per-template `consts:` pool.
        assert!(
            builder.const_pool().is_empty(),
            "selectors must not be in consts:, got: {:?}",
            builder.const_pool().entries()
        );
        let hoisted = crate::output::emitter::emit_statements(builder.hoisted_functions());
        assert!(hoisted.contains("$c0$"), "missing shared const decl, got: {hoisted}");
        assert!(hoisted.contains("\"header\""), "got hoisted: {hoisted}");
    }

    #[test]
    fn custom_track_generates_arrow_and_flags_component_instance() {
        // `@for (x of xs; track helper(x))` where `helper` is on the component context → a generated
        // trackBy arrow that reads `ctx` and so flags trackByUsesComponentInstance.
        let item = Variable {
            name: "x".to_string(),
            value: "$implicit".to_string(),
            source_span: t_span(),
            key_span: t_span(),
            value_span: None,
        };
        // `track foo` (a bare read that is neither the item nor `$index`) roots at the component.
        let block = ForLoopBlock {
            item,
            expression: AstWithSource::new(prop_read("xs"), None, String::new(), 0, vec![]),
            track_by: Some(AstWithSource::new(prop_read("foo"), None, String::new(), 0, vec![])),
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

        // The trackBy is a generated arrow `(x, $index) => ctx.foo`, NOT a shared helper reference.
        assert!(out.contains("(x, $index) =>"), "got: {out}");
        assert!(out.contains("ctx.foo"), "got: {out}");
        assert!(
            !out.contains("\u{0275}\u{0275}repeaterTrackByIdentity")
                && !out.contains("\u{0275}\u{0275}repeaterTrackByIndex"),
            "custom track should not use a shared helper, got: {out}"
        );
        // No `@empty`, but the trackBy reads the component instance, so the trailing `true` flag is
        // emitted.
        assert!(out.contains("true"), "expected usesComponentInstance flag, got: {out}");
    }

    #[test]
    fn switch_group_with_multiple_case_labels_emits_one_comparison_each() {
        // `@switch (v) { @case (a) @case (b) { <div>a</div> } }` — two labels share one body, so two
        // comparisons select the same slot.
        let group = SwitchBlockCaseGroup {
            cases: vec![
                SwitchBlockCase { expression: Some(prop_read("a")), spans: block_spans() },
                SwitchBlockCase { expression: Some(prop_read("b")), spans: block_spans() },
            ],
            children: div_a(),
            spans: block_spans(),
            i18n: None,
        };
        let block = SwitchBlock {
            expression: prop_read("v"),
            groups: vec![group],
            unknown_blocks: vec![],
            exhaustive_check: None,
            spans: block_spans(),
        };
        let input = TemplateCompilationInput::new("Test_Template", vec![Node::SwitchBlock(block)]);
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let func = builder.build_template_function(&input);
        let out = emit_expression(&func);

        // Both labels compare against the discriminant and select the SAME slot (0); only the first
        // comparison spills the discriminant into the temp.
        assert!(out.contains("(tmp_0_0 = ctx.v) === ctx.a ? 0"), "got: {out}");
        assert!(out.contains("tmp_0_0 === ctx.b ? 0"), "got: {out}");
        // Exactly one template/body is emitted for the group.
        assert_eq!(builder.hoisted_functions().len(), 1, "got: {out}");
    }

    #[test]
    fn animation_and_legacy_animation_bindings_lower() {
        // `<div [animate.leave]="exp" [@trig]="exp2"></div>`.
        let el = Element {
            name: "div".to_string(),
            attributes: vec![],
            inputs: vec![
                BoundAttribute {
                    name: "animate.leave".to_string(),
                    kind: BindingType::Animation,
                    security_context: SecurityContext::None,
                    value: prop_read("exp"),
                    unit: None,
                    source_span: t_span(),
                    key_span: t_span(),
                    value_span: None,
                    i18n: None,
                },
                BoundAttribute {
                    name: "trig".to_string(),
                    kind: BindingType::LegacyAnimation,
                    security_context: SecurityContext::None,
                    value: prop_read("exp2"),
                    unit: None,
                    source_span: t_span(),
                    key_span: t_span(),
                    value_span: None,
                    i18n: None,
                },
            ],
            outputs: vec![],
            directives: vec![],
            children: vec![],
            references: vec![],
            is_self_closing: true,
            source_span: t_span(),
            start_source_span: t_span(),
            end_source_span: None,
            is_void: false,
            i18n: None,
        };
        let input = TemplateCompilationInput::new("Test_Template", vec![Node::Element(el)]);
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let func = builder.build_template_function(&input);
        let out = emit_expression(&func);

        // Modern `[animate.leave]="exp"` (a bound expression) → a CREATE-block `ɵɵanimateLeave`
        // taking a zero-arg `_cb` callback returning the lowered expression (`convert_animations.ts`
        // `AnimationBindingKind.VALUE`). It reserves no var slot and no const entry.
        assert!(
            out.contains("\u{0275}\u{0275}animateLeave(function Test_Template_animateleave_cb() {"),
            "got: {out}"
        );
        assert!(out.contains("return ctx.exp;"), "got: {out}");
        // Legacy `[@trig]` → update-block `ɵɵdomProperty("@trig", ctx.exp2)`.
        assert!(out.contains("\u{0275}\u{0275}domProperty(\"@trig\", ctx.exp2)"), "got: {out}");
    }
}
