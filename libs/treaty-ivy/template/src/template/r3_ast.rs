//! The render3 template AST (the "t-AST" / R3 AST): `Element`, `Template`, `Text`,
//! `BoundText`, attributes/events, control-flow blocks (`@if`/`@switch`/`@for`/`@defer`),
//! `@let`, selectorless `Component`/`Directive`, plus the visitor.
//!
//! PORT TARGET: `migration/render3-specs/06-r3_ast.md`
//! Source: `tools/angular-ref/packages/compiler/src/render3/r3_ast.ts` (Angular 22.1.0-next.0)
//!
//! Per `migration/PORT-ARCHITECTURE.md` (and matching the already-ported
//! [`crate::expression::ast`]), this IR is OWNED and ARENA-FREE: `Box`/`Vec`/`String`
//! instead of the `oxc_allocator` arena/`'a` lifetimes the spec sketches. OXC arena/codegen
//! only enters at the emitter boundary, not here.
//!
//! Structural decisions (per spec §8.1):
//! - The TS class hierarchy + double-dispatch `visit()` is replaced by a single tagged
//!   [`Node`] enum + a `match`-based [`dispatch`]. Faster, idiomatic, no trait objects.
//! - `extends BlockNode` inheritance becomes a composed [`BlockSpans`] field.
//! - The 8 `DeferredTrigger` subclasses collapse into one [`DeferredTriggerKind`] payload
//!   enum plus a shared [`TriggerSpans`], wrapped in [`DeferredTrigger`].
//!
//! Embedded binding expressions reference [`crate::expression::ast`]: `AST` -> [`AstNode`],
//! `ASTWithSource` -> [`AstWithSource`]. Spans are [`ParseSourceSpan`] from the same module.
//! `Comment` and `HostElement` `throw` from `visit()` in TS, so they are kept as standalone
//! structs and are NOT [`Node`] variants (spec §3 / §7.1).

use crate::expression::ast::{
    AstNode, AstWithSource, BindingType, BoundElementProperty, ParseSourceSpan, ParsedEvent,
    ParsedEventType, SecurityContext,
};

// ---------------------------------------------------------------------------
// Local placeholder for the not-yet-ported i18n module.
// ---------------------------------------------------------------------------

/// Placeholder for `i18n/i18n_ast.I18nMeta` (the union of `Message | Node`), carried as an
/// optional annotation on many template nodes. Real port lives in the future `i18n` module;
/// kept opaque here so this module compiles standalone.
#[derive(Clone, Debug, PartialEq)]
pub struct I18nMeta;

// ---------------------------------------------------------------------------
// The top-level tagged node enum (replaces the TS class hierarchy + double dispatch).
// ---------------------------------------------------------------------------

/// `t.Node` — every visitable template node. Replaces TS's `Node` interface + per-class
/// `visit()` double dispatch with a single tagged enum dispatched via [`dispatch`] / `match`.
///
/// `Comment` and `HostElement` are intentionally excluded (their `visit()` throws in TS).
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Text(Text),
    BoundText(BoundText),
    TextAttribute(TextAttribute),
    BoundAttribute(BoundAttribute),
    BoundEvent(BoundEvent),
    Element(Element),
    Template(Template),
    Content(Content),
    Component(Component),
    Directive(Directive),
    Variable(Variable),
    Reference(Reference),
    Icu(Icu),
    LetDeclaration(LetDeclaration),
    UnknownBlock(UnknownBlock),
    // Deferred triggers are visitable nodes too (TS `DeferredTrigger implements Node`).
    DeferredTrigger(DeferredTrigger),
    // Control-flow blocks.
    DeferredBlock(DeferredBlock),
    DeferredBlockPlaceholder(DeferredBlockPlaceholder),
    DeferredBlockLoading(DeferredBlockLoading),
    DeferredBlockError(DeferredBlockError),
    SwitchBlock(SwitchBlock),
    SwitchBlockCase(SwitchBlockCase),
    SwitchBlockCaseGroup(SwitchBlockCaseGroup),
    SwitchExhaustiveCheck(SwitchExhaustiveCheck),
    ForLoopBlock(ForLoopBlock),
    ForLoopBlockEmpty(ForLoopBlockEmpty),
    IfBlock(IfBlock),
    IfBlockBranch(IfBlockBranch),
}

impl Node {
    /// The `sourceSpan` of any node (the `Node.sourceSpan` interface member). For block nodes
    /// this is `spans.source_span`.
    pub fn source_span(&self) -> &ParseSourceSpan {
        match self {
            Node::Text(n) => &n.source_span,
            Node::BoundText(n) => &n.source_span,
            Node::TextAttribute(n) => &n.source_span,
            Node::BoundAttribute(n) => &n.source_span,
            Node::BoundEvent(n) => &n.source_span,
            Node::Element(n) => &n.source_span,
            Node::Template(n) => &n.source_span,
            Node::Content(n) => &n.source_span,
            Node::Component(n) => &n.source_span,
            Node::Directive(n) => &n.source_span,
            Node::Variable(n) => &n.source_span,
            Node::Reference(n) => &n.source_span,
            Node::Icu(n) => &n.source_span,
            Node::LetDeclaration(n) => &n.source_span,
            Node::UnknownBlock(n) => &n.source_span,
            Node::DeferredTrigger(n) => &n.spans.source_span,
            Node::DeferredBlock(n) => &n.spans.source_span,
            Node::DeferredBlockPlaceholder(n) => &n.spans.source_span,
            Node::DeferredBlockLoading(n) => &n.spans.source_span,
            Node::DeferredBlockError(n) => &n.spans.source_span,
            Node::SwitchBlock(n) => &n.spans.source_span,
            Node::SwitchBlockCase(n) => &n.spans.source_span,
            Node::SwitchBlockCaseGroup(n) => &n.spans.source_span,
            Node::SwitchExhaustiveCheck(n) => &n.spans.source_span,
            Node::ForLoopBlock(n) => &n.spans.source_span,
            Node::ForLoopBlockEmpty(n) => &n.spans.source_span,
            Node::IfBlock(n) => &n.spans.source_span,
            Node::IfBlockBranch(n) => &n.spans.source_span,
        }
    }
}

// ---------------------------------------------------------------------------
// Leaf nodes.
// ---------------------------------------------------------------------------

/// `t.Text` — a static text node.
#[derive(Clone, Debug, PartialEq)]
pub struct Text {
    pub value: String,
    pub source_span: ParseSourceSpan,
}

/// `t.BoundText` — an interpolated text node (`value` is an expression AST).
#[derive(Clone, Debug, PartialEq)]
pub struct BoundText {
    pub value: AstNode,
    pub source_span: ParseSourceSpan,
    pub i18n: Option<I18nMeta>,
}

/// `t.Comment` — wrapper for a raw HTML comment. NOT a [`Node`] variant: its TS `visit()`
/// throws (comments are only collected at the top level when `collectCommentNodes` is set).
#[derive(Clone, Debug, PartialEq)]
pub struct Comment {
    pub value: String,
    pub source_span: ParseSourceSpan,
}

// ---------------------------------------------------------------------------
// Attributes, events, refs, variables.
// ---------------------------------------------------------------------------

/// `t.TextAttribute` — a static attribute (`<div a="b">`). `value_span` is absent when there
/// is no value (`<div a>`); `key_span` is absent for synthetic attributes from ICU expansions.
#[derive(Clone, Debug, PartialEq)]
pub struct TextAttribute {
    pub name: String,
    pub value: String,
    pub source_span: ParseSourceSpan,
    /// `readonly keySpan: ParseSourceSpan | undefined` — optional here (differs from
    /// `BoundAttribute`/`Variable`/`Reference`, where it is required; spec §7.3).
    pub key_span: Option<ParseSourceSpan>,
    pub value_span: Option<ParseSourceSpan>,
    pub i18n: Option<I18nMeta>,
}

/// `t.BoundAttribute` — a bound input (`[x]="y"`, `[class.a]`, `[style.b]`, animations, ...).
#[derive(Clone, Debug, PartialEq)]
pub struct BoundAttribute {
    pub name: String,
    /// TS `type: BindingType`. Renamed `kind` (`type` is a Rust keyword).
    pub kind: BindingType,
    pub security_context: SecurityContext,
    pub value: AstNode,
    pub unit: Option<String>,
    pub source_span: ParseSourceSpan,
    /// `readonly keySpan: ParseSourceSpan` — required (spec §7.3).
    pub key_span: ParseSourceSpan,
    pub value_span: Option<ParseSourceSpan>,
    pub i18n: Option<I18nMeta>,
}

impl BoundAttribute {
    /// `BoundAttribute.fromBoundElementProperty`. Panics if `prop.key_span` is `None`,
    /// matching the TS `throw` (spec §2.2 / §7.5). `i18n` is the optional second arg.
    ///
    /// NOTE: [`BoundElementProperty::value`] is an [`AstWithSource`], whereas
    /// `BoundAttribute::value` is a plain `AST`; we unwrap the inner `ast` to match TS, where
    /// `ASTWithSource` *is* an `AST` (so passing it through is transparent).
    pub fn from_bound_element_property(
        prop: BoundElementProperty,
        i18n: Option<I18nMeta>,
    ) -> BoundAttribute {
        let key_span = prop.key_span.unwrap_or_else(|| {
            panic!(
                "Unexpected state: keySpan must be defined for bound attributes but was not for {}",
                prop.name
            )
        });
        BoundAttribute {
            name: prop.name,
            kind: prop.ty,
            security_context: prop.security_context,
            value: *prop.value.ast,
            unit: prop.unit,
            source_span: prop.source_span,
            key_span,
            value_span: prop.value_span,
            i18n,
        }
    }
}

/// `t.BoundEvent` — a bound output (`(click)="..."`, `[(x)]`, animations, ...).
#[derive(Clone, Debug, PartialEq)]
pub struct BoundEvent {
    pub name: String,
    /// TS `type: ParsedEventType`. Renamed `kind` (`type` is a Rust keyword).
    pub kind: ParsedEventType,
    pub handler: AstNode,
    pub target: Option<String>,
    pub phase: Option<String>,
    pub source_span: ParseSourceSpan,
    pub handler_span: ParseSourceSpan,
    pub key_span: ParseSourceSpan,
}

impl BoundEvent {
    /// `BoundEvent.fromParsedEvent`. `target` is set only for [`ParsedEventType::Regular`],
    /// `phase` only for [`ParsedEventType::LegacyAnimation`] (both `None` for `TwoWay` /
    /// `Animation`; spec §7.6). Panics if `event.key_span` is `None`, matching TS `throw`.
    pub fn from_parsed_event(event: ParsedEvent) -> BoundEvent {
        let target = if event.ty == ParsedEventType::Regular {
            event.target_or_phase.clone()
        } else {
            None
        };
        let phase = if event.ty == ParsedEventType::LegacyAnimation {
            event.target_or_phase.clone()
        } else {
            None
        };
        BoundEvent {
            name: event.name,
            kind: event.ty,
            handler: *event.handler.ast,
            target,
            phase,
            source_span: event.source_span,
            handler_span: event.handler_span,
            key_span: event.key_span,
        }
    }
}

/// `t.Variable` — a template variable (`#x="y"` `let-x`, `@for` context vars, `@if` alias).
#[derive(Clone, Debug, PartialEq)]
pub struct Variable {
    pub name: String,
    pub value: String,
    pub source_span: ParseSourceSpan,
    pub key_span: ParseSourceSpan,
    pub value_span: Option<ParseSourceSpan>,
}

/// `t.Reference` — a template reference (`#ref`). Identical shape to [`Variable`].
#[derive(Clone, Debug, PartialEq)]
pub struct Reference {
    pub name: String,
    pub value: String,
    pub source_span: ParseSourceSpan,
    pub key_span: ParseSourceSpan,
    pub value_span: Option<ParseSourceSpan>,
}

// ---------------------------------------------------------------------------
// Element-family containers.
// ---------------------------------------------------------------------------

/// `t.Element` — a DOM element node.
#[derive(Clone, Debug, PartialEq)]
pub struct Element {
    pub name: String,
    pub attributes: Vec<TextAttribute>,
    pub inputs: Vec<BoundAttribute>,
    pub outputs: Vec<BoundEvent>,
    pub directives: Vec<Directive>,
    pub children: Vec<Node>,
    pub references: Vec<Reference>,
    pub is_self_closing: bool,
    pub source_span: ParseSourceSpan,
    pub start_source_span: ParseSourceSpan,
    pub end_source_span: Option<ParseSourceSpan>,
    /// `readonly isVoid` — HTML void element (`<br>`). Distinct from `is_self_closing`
    /// (author wrote `/>`); spec §7.11.
    pub is_void: bool,
    pub i18n: Option<I18nMeta>,
}

/// `t.Component` — a selectorless component instance (`<MyCmp>`); v19+/selectorless.
#[derive(Clone, Debug, PartialEq)]
pub struct Component {
    pub component_name: String,
    pub tag_name: Option<String>,
    pub full_name: String,
    pub attributes: Vec<TextAttribute>,
    pub inputs: Vec<BoundAttribute>,
    pub outputs: Vec<BoundEvent>,
    pub directives: Vec<Directive>,
    pub children: Vec<Node>,
    pub references: Vec<Reference>,
    pub is_self_closing: bool,
    pub source_span: ParseSourceSpan,
    pub start_source_span: ParseSourceSpan,
    pub end_source_span: Option<ParseSourceSpan>,
    pub i18n: Option<I18nMeta>,
}

/// `t.Directive` — a selectorless directive applied to a host (`@MyDir`); v19+/selectorless.
/// Carries its own attrs/inputs/outputs/refs but has no children.
#[derive(Clone, Debug, PartialEq)]
pub struct Directive {
    pub name: String,
    pub attributes: Vec<TextAttribute>,
    pub inputs: Vec<BoundAttribute>,
    pub outputs: Vec<BoundEvent>,
    pub references: Vec<Reference>,
    pub source_span: ParseSourceSpan,
    pub start_source_span: ParseSourceSpan,
    pub end_source_span: Option<ParseSourceSpan>,
    pub i18n: Option<I18nMeta>,
}

/// One entry of `Template.templateAttrs`: `BoundAttribute | TextAttribute`.
#[derive(Clone, Debug, PartialEq)]
pub enum TemplateAttr {
    Bound(BoundAttribute),
    Text(TextAttribute),
}

/// `t.Template` — an `<ng-template>` (or a desugared structural-directive template).
/// `tag_name == None` is the special case for a structural directive on an `ng-template`.
#[derive(Clone, Debug, PartialEq)]
pub struct Template {
    pub tag_name: Option<String>,
    pub attributes: Vec<TextAttribute>,
    pub inputs: Vec<BoundAttribute>,
    pub outputs: Vec<BoundEvent>,
    pub directives: Vec<Directive>,
    pub template_attrs: Vec<TemplateAttr>,
    pub children: Vec<Node>,
    pub references: Vec<Reference>,
    pub variables: Vec<Variable>,
    pub is_self_closing: bool,
    pub source_span: ParseSourceSpan,
    pub start_source_span: ParseSourceSpan,
    pub end_source_span: Option<ParseSourceSpan>,
    pub i18n: Option<I18nMeta>,
}

/// `t.Content` — an `<ng-content>` projection slot. TS has a `readonly name = 'ng-content'`;
/// reproduced as [`Content::NAME`].
#[derive(Clone, Debug, PartialEq)]
pub struct Content {
    pub selector: String,
    pub attributes: Vec<TextAttribute>,
    pub children: Vec<Node>,
    pub is_self_closing: bool,
    pub source_span: ParseSourceSpan,
    pub start_source_span: ParseSourceSpan,
    pub end_source_span: Option<ParseSourceSpan>,
    pub i18n: Option<I18nMeta>,
}

impl Content {
    /// TS `readonly name = 'ng-content'`.
    pub const NAME: &'static str = "ng-content";
}

// ---------------------------------------------------------------------------
// BlockNode shared spans (composition instead of inheritance; spec §3.4).
// ---------------------------------------------------------------------------

/// `BlockNode` — the positional span base shared by every control-flow block. NOT itself a
/// [`Node`] (it has no `visit()`); embedded by composition as a `spans` field.
#[derive(Clone, Debug, PartialEq)]
pub struct BlockSpans {
    pub name_span: ParseSourceSpan,
    pub source_span: ParseSourceSpan,
    pub start_source_span: ParseSourceSpan,
    pub end_source_span: Option<ParseSourceSpan>,
}

// ---------------------------------------------------------------------------
// @if
// ---------------------------------------------------------------------------

/// `t.IfBlock` — an `@if`/`@else if`/`@else` chain.
#[derive(Clone, Debug, PartialEq)]
pub struct IfBlock {
    pub branches: Vec<IfBlockBranch>,
    pub spans: BlockSpans,
}

/// `t.IfBlockBranch` — one branch of an `@if`. `expression == None` for the `@else` branch.
#[derive(Clone, Debug, PartialEq)]
pub struct IfBlockBranch {
    pub expression: Option<AstNode>,
    pub children: Vec<Node>,
    pub expression_alias: Option<Variable>,
    pub spans: BlockSpans,
    pub i18n: Option<I18nMeta>,
}

// ---------------------------------------------------------------------------
// @switch
// ---------------------------------------------------------------------------

/// `t.SwitchBlock` — an `@switch`. `unknown_blocks` are captured only for language-service
/// autocompletion (spec §3.4); `exhaustive_check` is the newer exhaustiveness node.
#[derive(Clone, Debug, PartialEq)]
pub struct SwitchBlock {
    pub expression: AstNode,
    pub groups: Vec<SwitchBlockCaseGroup>,
    pub unknown_blocks: Vec<UnknownBlock>,
    pub exhaustive_check: Option<SwitchExhaustiveCheck>,
    pub spans: BlockSpans,
}

/// `t.SwitchBlockCase` — one `@case`/`@default` label. `expression == None` => `@default`.
#[derive(Clone, Debug, PartialEq)]
pub struct SwitchBlockCase {
    pub expression: Option<AstNode>,
    pub spans: BlockSpans,
}

/// `t.SwitchBlockCaseGroup` — a group of `@case` labels sharing one body (newer; spec §3.4).
#[derive(Clone, Debug, PartialEq)]
pub struct SwitchBlockCaseGroup {
    pub cases: Vec<SwitchBlockCase>,
    pub children: Vec<Node>,
    pub spans: BlockSpans,
    pub i18n: Option<I18nMeta>,
}

/// `t.SwitchExhaustiveCheck` — synthetic exhaustiveness-check node. `expression` may be `None`.
#[derive(Clone, Debug, PartialEq)]
pub struct SwitchExhaustiveCheck {
    pub expression: Option<AstNode>,
    pub spans: BlockSpans,
}

// ---------------------------------------------------------------------------
// @for
// ---------------------------------------------------------------------------

/// `t.ForLoopBlock` — an `@for` loop. `expression`/`track_by` are [`AstWithSource`] (not plain
/// `AST`); `main_block_span` is an extra span beyond [`BlockSpans`] (the `@for(...) { ... }`
/// body span excluding `@empty`); spec §7.12 / §3.4.
#[derive(Clone, Debug, PartialEq)]
pub struct ForLoopBlock {
    pub item: Variable,
    pub expression: AstWithSource,
    pub track_by: Option<AstWithSource>,
    pub track_keyword_span: Option<ParseSourceSpan>,
    /// `$index`, `$count`, `$first`, `$last`, `$even`, `$odd`.
    pub context_variables: Vec<Variable>,
    pub children: Vec<Node>,
    pub empty: Option<ForLoopBlockEmpty>,
    pub main_block_span: ParseSourceSpan,
    pub spans: BlockSpans,
    pub i18n: Option<I18nMeta>,
}

/// `t.ForLoopBlockEmpty` — the `@empty` block of an `@for`.
#[derive(Clone, Debug, PartialEq)]
pub struct ForLoopBlockEmpty {
    pub children: Vec<Node>,
    pub spans: BlockSpans,
    pub i18n: Option<I18nMeta>,
}

// ---------------------------------------------------------------------------
// @defer + triggers.
// ---------------------------------------------------------------------------

/// Shared positional spans for every `DeferredTrigger` subclass (spec §3.4).
#[derive(Clone, Debug, PartialEq)]
pub struct TriggerSpans {
    /// `null` for `BoundDeferredTrigger` (the `when` trigger has no name).
    pub name_span: Option<ParseSourceSpan>,
    pub source_span: ParseSourceSpan,
    pub prefetch_span: Option<ParseSourceSpan>,
    pub when_or_on_source_span: Option<ParseSourceSpan>,
    pub hydrate_span: Option<ParseSourceSpan>,
}

/// The payload of a deferred trigger — collapses the 8 TS `DeferredTrigger` subclasses.
#[derive(Clone, Debug, PartialEq)]
pub enum DeferredTriggerKind {
    /// `BoundDeferredTrigger` — `@defer (when expr)`. `name_span` is always `None`.
    When { value: AstNode },
    /// `NeverDeferredTrigger`.
    Never,
    /// `IdleDeferredTrigger`.
    Idle { timeout: Option<f64> },
    /// `ImmediateDeferredTrigger`.
    Immediate,
    /// `HoverDeferredTrigger`.
    Hover { reference: Option<String> },
    /// `TimerDeferredTrigger`.
    Timer { delay: f64 },
    /// `InteractionDeferredTrigger`.
    Interaction { reference: Option<String> },
    /// `ViewportDeferredTrigger`. `options` is a `LiteralMap` AST node (stored as an
    /// [`AstNode`], which holds the `ExprKind::LiteralMap` variant).
    Viewport {
        reference: Option<String>,
        options: Option<AstNode>,
    },
}

/// `t.DeferredTrigger` (abstract) flattened to `{ kind, spans }`. Visitable as
/// [`Node::DeferredTrigger`].
#[derive(Clone, Debug, PartialEq)]
pub struct DeferredTrigger {
    pub kind: DeferredTriggerKind,
    pub spans: TriggerSpans,
}

/// Stable discriminator for a [`DeferredBlockTriggers`] slot, used to preserve the original
/// key insertion order during traversal (spec §3.4 / §7.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerKey {
    When,
    Idle,
    Immediate,
    Hover,
    Timer,
    Interaction,
    Viewport,
    Never,
}

/// `DeferredBlockTriggers` — a set of optional triggers (one of each kind). `order` records
/// the original key insertion order so traversal matches TS's `Object.keys` (spec §7.7);
/// build it via [`DeferredBlockTriggers::push_order`] / set it explicitly.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeferredBlockTriggers {
    pub when: Option<DeferredTrigger>,
    pub idle: Option<DeferredTrigger>,
    pub immediate: Option<DeferredTrigger>,
    pub hover: Option<DeferredTrigger>,
    pub timer: Option<DeferredTrigger>,
    pub interaction: Option<DeferredTrigger>,
    pub viewport: Option<DeferredTrigger>,
    pub never: Option<DeferredTrigger>,
    /// Insertion order of the populated keys (mirrors TS `definedTriggers`).
    pub order: Vec<TriggerKey>,
}

impl DeferredBlockTriggers {
    /// Append a key to the insertion-order record (mirrors how TS accumulates `Object.keys`).
    pub fn push_order(&mut self, key: TriggerKey) {
        self.order.push(key);
    }

    /// Borrow the trigger for a given key, if present.
    pub fn get(&self, key: TriggerKey) -> Option<&DeferredTrigger> {
        match key {
            TriggerKey::When => self.when.as_ref(),
            TriggerKey::Idle => self.idle.as_ref(),
            TriggerKey::Immediate => self.immediate.as_ref(),
            TriggerKey::Hover => self.hover.as_ref(),
            TriggerKey::Timer => self.timer.as_ref(),
            TriggerKey::Interaction => self.interaction.as_ref(),
            TriggerKey::Viewport => self.viewport.as_ref(),
            TriggerKey::Never => self.never.as_ref(),
        }
    }

    /// The defined triggers in insertion order (mirrors `keys.map(k => triggers[k]!)`). Falls
    /// back to a fixed canonical order for any populated slot missing from `order`, so callers
    /// that did not maintain `order` still get every trigger.
    pub fn defined_in_order(&self) -> Vec<&DeferredTrigger> {
        if !self.order.is_empty() {
            return self.order.iter().filter_map(|k| self.get(*k)).collect();
        }
        const CANONICAL: [TriggerKey; 8] = [
            TriggerKey::When,
            TriggerKey::Idle,
            TriggerKey::Immediate,
            TriggerKey::Hover,
            TriggerKey::Timer,
            TriggerKey::Interaction,
            TriggerKey::Viewport,
            TriggerKey::Never,
        ];
        CANONICAL.iter().filter_map(|k| self.get(*k)).collect()
    }
}

/// `t.DeferredBlock` — a `@defer` block. Has three independent trigger sets (regular, prefetch,
/// hydrate) plus optional `@placeholder`/`@loading`/`@error` sub-blocks. `main_block_span` is
/// an extra span beyond [`BlockSpans`].
#[derive(Clone, Debug, PartialEq)]
pub struct DeferredBlock {
    pub children: Vec<Node>,
    pub triggers: DeferredBlockTriggers,
    pub prefetch_triggers: DeferredBlockTriggers,
    pub hydrate_triggers: DeferredBlockTriggers,
    pub placeholder: Option<DeferredBlockPlaceholder>,
    pub loading: Option<DeferredBlockLoading>,
    pub error: Option<DeferredBlockError>,
    pub main_block_span: ParseSourceSpan,
    pub spans: BlockSpans,
    pub i18n: Option<I18nMeta>,
}

/// `t.DeferredBlockPlaceholder` — the `@placeholder` block. `minimum_time` in ms.
#[derive(Clone, Debug, PartialEq)]
pub struct DeferredBlockPlaceholder {
    pub children: Vec<Node>,
    pub minimum_time: Option<f64>,
    pub spans: BlockSpans,
    pub i18n: Option<I18nMeta>,
}

/// `t.DeferredBlockLoading` — the `@loading` block. `after_time`/`minimum_time` in ms.
#[derive(Clone, Debug, PartialEq)]
pub struct DeferredBlockLoading {
    pub children: Vec<Node>,
    pub after_time: Option<f64>,
    pub minimum_time: Option<f64>,
    pub spans: BlockSpans,
    pub i18n: Option<I18nMeta>,
}

/// `t.DeferredBlockError` — the `@error` block.
#[derive(Clone, Debug, PartialEq)]
pub struct DeferredBlockError {
    pub children: Vec<Node>,
    pub spans: BlockSpans,
    pub i18n: Option<I18nMeta>,
}

// ---------------------------------------------------------------------------
// Misc nodes.
// ---------------------------------------------------------------------------

/// `t.UnknownBlock` — an unrecognized `@`-block (kept for language-service autocompletion).
#[derive(Clone, Debug, PartialEq)]
pub struct UnknownBlock {
    pub name: String,
    pub source_span: ParseSourceSpan,
    pub name_span: ParseSourceSpan,
}

/// `t.LetDeclaration` — a `@let x = expr;` declaration (v18.1+).
#[derive(Clone, Debug, PartialEq)]
pub struct LetDeclaration {
    pub name: String,
    pub value: AstNode,
    pub source_span: ParseSourceSpan,
    pub name_span: ParseSourceSpan,
    pub value_span: ParseSourceSpan,
}

/// One entry of `Icu.placeholders`: `Text | BoundText`.
#[derive(Clone, Debug, PartialEq)]
pub enum IcuPlaceholder {
    Text(Text),
    Bound(BoundText),
}

/// `t.Icu` — an ICU expansion (`{count, plural, ...}`). `vars`/`placeholders` are ordered
/// string maps (i18n message generation is order-sensitive; spec §3.5 / §7.8), modeled as
/// `Vec`s of pairs to preserve insertion order.
#[derive(Clone, Debug, PartialEq)]
pub struct Icu {
    pub vars: Vec<(String, BoundText)>,
    pub placeholders: Vec<(String, IcuPlaceholder)>,
    pub source_span: ParseSourceSpan,
    pub i18n: Option<I18nMeta>,
}

/// `t.HostElement` — the host element of a directive. Type-check-only; cannot be produced from
/// a user's template. NOT a [`Node`] variant (its `visit()` throws). Invariant: `tag_names`
/// is non-empty (enforced by [`HostElement::new`]; spec §7.2).
#[derive(Clone, Debug, PartialEq)]
pub struct HostElement {
    pub tag_names: Vec<String>,
    pub bindings: Vec<BoundAttribute>,
    pub listeners: Vec<BoundEvent>,
    pub source_span: ParseSourceSpan,
}

impl HostElement {
    /// Mirrors the TS constructor, which throws if `tagNames.length === 0`.
    pub fn new(
        tag_names: Vec<String>,
        bindings: Vec<BoundAttribute>,
        listeners: Vec<BoundEvent>,
        source_span: ParseSourceSpan,
    ) -> HostElement {
        assert!(
            !tag_names.is_empty(),
            "HostElement must have at least one tag name."
        );
        HostElement {
            tag_names,
            bindings,
            listeners,
            source_span,
        }
    }
}

// ---------------------------------------------------------------------------
// Visitor.
// ---------------------------------------------------------------------------

/// `t.Visitor` + `t.RecursiveVisitor` folded into one trait. Each per-node hook has a default
/// body that recurses into children in the exact field order `RecursiveVisitor` uses (the
/// order is observable downstream, e.g. i18n message ordering; spec §4.3). Implementors
/// override only the hooks they care about; the result type is `()` (matching
/// `RecursiveVisitor implements Visitor<void>`).
///
/// For the transform-style accumulating variant (`Visitor<Result>` with truthy filtering),
/// see [`visit_all_collect`].
#[allow(unused_variables)]
pub trait Visitor {
    /// Dispatch any node to its matching hook (replaces TS double dispatch).
    fn visit_node(&mut self, node: &Node) {
        dispatch(node, self);
    }

    /// `visitAll(this, nodes)` for the `Visitor<void>` case: visit each node in order.
    fn visit_all(&mut self, nodes: &[Node]) {
        for n in nodes {
            self.visit_node(n);
        }
    }

    /// `visitElement`: attributes -> inputs -> outputs -> directives -> children -> references.
    fn visit_element(&mut self, element: &Element) {
        self.visit_all_text_attributes(&element.attributes);
        self.visit_all_bound_attributes(&element.inputs);
        self.visit_all_bound_events(&element.outputs);
        self.visit_all_directives(&element.directives);
        self.visit_all(&element.children);
        self.visit_all_references(&element.references);
    }

    /// `visitTemplate`: attributes -> inputs -> outputs -> directives -> children ->
    /// references -> variables.
    fn visit_template(&mut self, template: &Template) {
        self.visit_all_text_attributes(&template.attributes);
        self.visit_all_bound_attributes(&template.inputs);
        self.visit_all_bound_events(&template.outputs);
        self.visit_all_directives(&template.directives);
        self.visit_all(&template.children);
        self.visit_all_references(&template.references);
        self.visit_all_variables(&template.variables);
    }

    /// `visitContent`: children.
    fn visit_content(&mut self, content: &Content) {
        self.visit_all(&content.children);
    }

    /// `visitComponent`: attributes -> inputs -> outputs -> directives -> children -> references.
    fn visit_component(&mut self, component: &Component) {
        self.visit_all_text_attributes(&component.attributes);
        self.visit_all_bound_attributes(&component.inputs);
        self.visit_all_bound_events(&component.outputs);
        self.visit_all_directives(&component.directives);
        self.visit_all(&component.children);
        self.visit_all_references(&component.references);
    }

    /// `visitDirective`: attributes -> inputs -> outputs -> references (no children).
    fn visit_directive(&mut self, directive: &Directive) {
        self.visit_all_text_attributes(&directive.attributes);
        self.visit_all_bound_attributes(&directive.inputs);
        self.visit_all_bound_events(&directive.outputs);
        self.visit_all_references(&directive.references);
    }

    fn visit_variable(&mut self, variable: &Variable) {}
    fn visit_reference(&mut self, reference: &Reference) {}
    fn visit_text_attribute(&mut self, attribute: &TextAttribute) {}
    fn visit_bound_attribute(&mut self, attribute: &BoundAttribute) {}
    fn visit_bound_event(&mut self, attribute: &BoundEvent) {}
    fn visit_text(&mut self, text: &Text) {}
    fn visit_bound_text(&mut self, text: &BoundText) {}
    fn visit_icu(&mut self, icu: &Icu) {}

    /// `visitDeferredBlock`: delegates to the custom ordered traversal (spec §4.2):
    /// hydrate triggers, then regular, then prefetch, then children, then
    /// `[placeholder, loading, error]` filtered for `Some`.
    fn visit_deferred_block(&mut self, deferred: &DeferredBlock) {
        for t in deferred.hydrate_triggers.defined_in_order() {
            self.visit_deferred_trigger(t);
        }
        for t in deferred.triggers.defined_in_order() {
            self.visit_deferred_trigger(t);
        }
        for t in deferred.prefetch_triggers.defined_in_order() {
            self.visit_deferred_trigger(t);
        }
        self.visit_all(&deferred.children);
        if let Some(p) = &deferred.placeholder {
            self.visit_deferred_block_placeholder(p);
        }
        if let Some(l) = &deferred.loading {
            self.visit_deferred_block_loading(l);
        }
        if let Some(e) = &deferred.error {
            self.visit_deferred_block_error(e);
        }
    }

    fn visit_deferred_block_placeholder(&mut self, block: &DeferredBlockPlaceholder) {
        self.visit_all(&block.children);
    }
    fn visit_deferred_block_loading(&mut self, block: &DeferredBlockLoading) {
        self.visit_all(&block.children);
    }
    fn visit_deferred_block_error(&mut self, block: &DeferredBlockError) {
        self.visit_all(&block.children);
    }
    fn visit_deferred_trigger(&mut self, trigger: &DeferredTrigger) {}

    /// `visitSwitchBlock`: groups.
    fn visit_switch_block(&mut self, block: &SwitchBlock) {
        self.visit_all_switch_case_groups(&block.groups);
    }
    /// No-op in `RecursiveVisitor`.
    fn visit_switch_block_case(&mut self, block: &SwitchBlockCase) {}
    /// `visitSwitchBlockCaseGroup`: cases -> children.
    fn visit_switch_block_case_group(&mut self, block: &SwitchBlockCaseGroup) {
        for c in &block.cases {
            self.visit_switch_block_case(c);
        }
        self.visit_all(&block.children);
    }
    /// No-op in `RecursiveVisitor`.
    fn visit_switch_exhaustive_check(&mut self, block: &SwitchExhaustiveCheck) {}

    /// `visitForLoopBlock`: `[item, ...contextVariables, ...children]`, then `empty` if present.
    fn visit_for_loop_block(&mut self, block: &ForLoopBlock) {
        self.visit_variable(&block.item);
        for v in &block.context_variables {
            self.visit_variable(v);
        }
        self.visit_all(&block.children);
        if let Some(empty) = &block.empty {
            self.visit_for_loop_block_empty(empty);
        }
    }
    fn visit_for_loop_block_empty(&mut self, block: &ForLoopBlockEmpty) {
        self.visit_all(&block.children);
    }

    /// `visitIfBlock`: branches.
    fn visit_if_block(&mut self, block: &IfBlock) {
        for b in &block.branches {
            self.visit_if_block_branch(b);
        }
    }
    /// `visitIfBlockBranch`: children, then `expressionAlias?.visit(this)`.
    fn visit_if_block_branch(&mut self, block: &IfBlockBranch) {
        self.visit_all(&block.children);
        if let Some(alias) = &block.expression_alias {
            self.visit_variable(alias);
        }
    }

    fn visit_unknown_block(&mut self, block: &UnknownBlock) {}
    fn visit_let_declaration(&mut self, decl: &LetDeclaration) {}

    // --- typed `visit_all_*` helpers (the homogeneous lists are not `Node`s) ---

    fn visit_all_text_attributes(&mut self, attrs: &[TextAttribute]) {
        for a in attrs {
            self.visit_text_attribute(a);
        }
    }
    fn visit_all_bound_attributes(&mut self, attrs: &[BoundAttribute]) {
        for a in attrs {
            self.visit_bound_attribute(a);
        }
    }
    fn visit_all_bound_events(&mut self, events: &[BoundEvent]) {
        for e in events {
            self.visit_bound_event(e);
        }
    }
    fn visit_all_references(&mut self, refs: &[Reference]) {
        for r in refs {
            self.visit_reference(r);
        }
    }
    fn visit_all_variables(&mut self, vars: &[Variable]) {
        for v in vars {
            self.visit_variable(v);
        }
    }
    fn visit_all_directives(&mut self, dirs: &[Directive]) {
        for d in dirs {
            self.visit_directive(d);
        }
    }
    fn visit_all_switch_case_groups(&mut self, groups: &[SwitchBlockCaseGroup]) {
        for g in groups {
            self.visit_switch_block_case_group(g);
        }
    }
}

/// `node.visit(visitor)` — dispatch a [`Node`] to the matching [`Visitor`] hook. Replaces the
/// TS per-class double dispatch with a single `match` (spec §4.1).
pub fn dispatch<V: Visitor + ?Sized>(node: &Node, visitor: &mut V) {
    match node {
        Node::Text(n) => visitor.visit_text(n),
        Node::BoundText(n) => visitor.visit_bound_text(n),
        Node::TextAttribute(n) => visitor.visit_text_attribute(n),
        Node::BoundAttribute(n) => visitor.visit_bound_attribute(n),
        Node::BoundEvent(n) => visitor.visit_bound_event(n),
        Node::Element(n) => visitor.visit_element(n),
        Node::Template(n) => visitor.visit_template(n),
        Node::Content(n) => visitor.visit_content(n),
        Node::Component(n) => visitor.visit_component(n),
        Node::Directive(n) => visitor.visit_directive(n),
        Node::Variable(n) => visitor.visit_variable(n),
        Node::Reference(n) => visitor.visit_reference(n),
        Node::Icu(n) => visitor.visit_icu(n),
        Node::LetDeclaration(n) => visitor.visit_let_declaration(n),
        Node::UnknownBlock(n) => visitor.visit_unknown_block(n),
        Node::DeferredTrigger(n) => visitor.visit_deferred_trigger(n),
        Node::DeferredBlock(n) => visitor.visit_deferred_block(n),
        Node::DeferredBlockPlaceholder(n) => visitor.visit_deferred_block_placeholder(n),
        Node::DeferredBlockLoading(n) => visitor.visit_deferred_block_loading(n),
        Node::DeferredBlockError(n) => visitor.visit_deferred_block_error(n),
        Node::SwitchBlock(n) => visitor.visit_switch_block(n),
        Node::SwitchBlockCase(n) => visitor.visit_switch_block_case(n),
        Node::SwitchBlockCaseGroup(n) => visitor.visit_switch_block_case_group(n),
        Node::SwitchExhaustiveCheck(n) => visitor.visit_switch_exhaustive_check(n),
        Node::ForLoopBlock(n) => visitor.visit_for_loop_block(n),
        Node::ForLoopBlockEmpty(n) => visitor.visit_for_loop_block_empty(n),
        Node::IfBlock(n) => visitor.visit_if_block(n),
        Node::IfBlockBranch(n) => visitor.visit_if_block_branch(n),
    }
}

/// A no-op [`Visitor`] that performs the default recursive traversal (the TS
/// `RecursiveVisitor`). Subclass-style use: wrap your state and override hooks; this unit
/// struct is handy when you only need the walk itself.
#[derive(Default)]
pub struct RecursiveVisitor;
impl Visitor for RecursiveVisitor {}

/// `visitAll(visitor, nodes)` — the `Visitor<void>` form: visit every node in order.
pub fn visit_all<V: Visitor + ?Sized>(visitor: &mut V, nodes: &[Node]) {
    visitor.visit_all(nodes);
}

/// `visitAll` — the accumulating transform form (`Visitor<Result>`): map each node through
/// `f` and keep only the truthy (`Some`) results, mirroring the TS "drop falsy results"
/// behavior for transform passes (spec §4.4). The optional generic `visit?` hook short-circuit
/// is not modeled here (callers pick the form they want).
pub fn visit_all_collect<R, F>(nodes: &[Node], mut f: F) -> Vec<R>
where
    F: FnMut(&Node) -> Option<R>,
{
    let mut out = Vec::new();
    for n in nodes {
        if let Some(r) = f(n) {
            out.push(r);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expression::ast::{
        AstNode, AstWithSource, ExprKind, LiteralValue, ParseSpan, AbsoluteSourceSpan,
    };

    fn span() -> ParseSourceSpan {
        ParseSourceSpan { start: 0, end: 0 }
    }

    fn num(n: f64) -> AstNode {
        AstNode::new(
            ParseSpan::new(0, 0),
            AbsoluteSourceSpan::new(0, 0),
            ExprKind::LiteralPrimitive {
                value: LiteralValue::Num(n),
            },
        )
    }

    fn block_spans() -> BlockSpans {
        BlockSpans {
            name_span: span(),
            source_span: span(),
            start_source_span: span(),
            end_source_span: None,
        }
    }

    fn text(value: &str) -> Node {
        Node::Text(Text {
            value: value.to_string(),
            source_span: span(),
        })
    }

    /// Collects the values of visited text nodes, to assert traversal order.
    #[derive(Default)]
    struct TextCollector {
        log: Vec<String>,
    }
    impl Visitor for TextCollector {
        fn visit_text(&mut self, t: &Text) {
            self.log.push(t.value.clone());
        }
    }

    #[test]
    fn element_children_traversed_in_order() {
        let el = Element {
            name: "div".to_string(),
            attributes: vec![],
            inputs: vec![],
            outputs: vec![],
            directives: vec![],
            children: vec![text("a"), text("b"), text("c")],
            references: vec![],
            is_self_closing: false,
            source_span: span(),
            start_source_span: span(),
            end_source_span: None,
            is_void: false,
            i18n: None,
        };
        let mut v = TextCollector::default();
        v.visit_node(&Node::Element(el));
        assert_eq!(v.log, vec!["a", "b", "c"]);
    }

    #[test]
    fn for_loop_visits_children_then_empty() {
        let block = ForLoopBlock {
            item: Variable {
                name: "x".to_string(),
                value: String::new(),
                source_span: span(),
                key_span: span(),
                value_span: None,
            },
            expression: AstWithSource::new(num(0.0), None, String::new(), 0, vec![]),
            track_by: None,
            track_keyword_span: None,
            context_variables: vec![],
            children: vec![text("body")],
            empty: Some(ForLoopBlockEmpty {
                children: vec![text("empty")],
                spans: block_spans(),
                i18n: None,
            }),
            main_block_span: span(),
            spans: block_spans(),
            i18n: None,
        };
        let mut v = TextCollector::default();
        v.visit_node(&Node::ForLoopBlock(block));
        assert_eq!(v.log, vec!["body", "empty"]);
    }

    #[test]
    fn deferred_block_visits_hydrate_then_main_then_prefetch_then_children() {
        // We track visited triggers via a dedicated visitor that records `when` values.
        #[derive(Default)]
        struct Rec {
            log: Vec<String>,
        }
        impl Visitor for Rec {
            fn visit_text(&mut self, t: &Text) {
                self.log.push(format!("text:{}", t.value));
            }
            fn visit_deferred_trigger(&mut self, _t: &DeferredTrigger) {
                self.log.push("trigger".to_string());
            }
        }

        let mk_trigger = || DeferredTrigger {
            kind: DeferredTriggerKind::Immediate,
            spans: TriggerSpans {
                name_span: None,
                source_span: span(),
                prefetch_span: None,
                when_or_on_source_span: None,
                hydrate_span: None,
            },
        };

        let mut hydrate = DeferredBlockTriggers::default();
        hydrate.immediate = Some(mk_trigger());
        hydrate.push_order(TriggerKey::Immediate);

        let mut regular = DeferredBlockTriggers::default();
        regular.idle = Some(mk_trigger());
        regular.push_order(TriggerKey::Idle);

        let block = DeferredBlock {
            children: vec![text("child")],
            triggers: regular,
            prefetch_triggers: DeferredBlockTriggers::default(),
            hydrate_triggers: hydrate,
            placeholder: Some(DeferredBlockPlaceholder {
                children: vec![text("ph")],
                minimum_time: None,
                spans: block_spans(),
                i18n: None,
            }),
            loading: None,
            error: None,
            main_block_span: span(),
            spans: block_spans(),
            i18n: None,
        };

        let mut v = Rec::default();
        v.visit_node(&Node::DeferredBlock(block));
        // hydrate trigger, then regular trigger, then child text, then placeholder text.
        assert_eq!(
            v.log,
            vec!["trigger", "trigger", "text:child", "text:ph"]
        );
    }

    #[test]
    fn from_parsed_event_target_phase_derivation() {
        use crate::expression::ast::ParseSourceSpan as PSS;
        let mk = |ty| ParsedEvent {
            name: "e".to_string(),
            target_or_phase: Some("tp".to_string()),
            ty,
            handler: AstWithSource::new(num(0.0), None, String::new(), 0, vec![]),
            source_span: PSS { start: 0, end: 0 },
            handler_span: PSS { start: 0, end: 0 },
            key_span: PSS { start: 0, end: 0 },
        };
        let reg = BoundEvent::from_parsed_event(mk(ParsedEventType::Regular));
        assert_eq!(reg.target.as_deref(), Some("tp"));
        assert_eq!(reg.phase, None);

        let anim = BoundEvent::from_parsed_event(mk(ParsedEventType::LegacyAnimation));
        assert_eq!(anim.target, None);
        assert_eq!(anim.phase.as_deref(), Some("tp"));

        let two_way = BoundEvent::from_parsed_event(mk(ParsedEventType::TwoWay));
        assert_eq!(two_way.target, None);
        assert_eq!(two_way.phase, None);
    }

    #[test]
    fn from_bound_element_property_carries_fields() {
        use crate::expression::ast::ParseSourceSpan as PSS;
        let prop = BoundElementProperty {
            name: "p".to_string(),
            ty: BindingType::Property,
            security_context: SecurityContext::None,
            value: AstWithSource::new(num(7.0), None, String::new(), 0, vec![]),
            unit: Some("px".to_string()),
            source_span: PSS { start: 0, end: 0 },
            key_span: Some(PSS { start: 1, end: 2 }),
            value_span: None,
        };
        let attr = BoundAttribute::from_bound_element_property(prop, None);
        assert_eq!(attr.name, "p");
        assert_eq!(attr.kind, BindingType::Property);
        assert_eq!(attr.unit.as_deref(), Some("px"));
        assert_eq!(attr.key_span, PSS { start: 1, end: 2 });
    }

    #[test]
    #[should_panic(expected = "keySpan must be defined")]
    fn from_bound_element_property_panics_without_key_span() {
        let prop = BoundElementProperty {
            name: "p".to_string(),
            ty: BindingType::Property,
            security_context: SecurityContext::None,
            value: AstWithSource::new(num(0.0), None, String::new(), 0, vec![]),
            unit: None,
            source_span: span(),
            key_span: None,
            value_span: None,
        };
        let _ = BoundAttribute::from_bound_element_property(prop, None);
    }

    #[test]
    #[should_panic(expected = "at least one tag name")]
    fn host_element_requires_tag_name() {
        let _ = HostElement::new(vec![], vec![], vec![], span());
    }

    #[test]
    fn if_block_branch_visits_children_then_alias() {
        #[derive(Default)]
        struct Rec {
            log: Vec<String>,
        }
        impl Visitor for Rec {
            fn visit_text(&mut self, t: &Text) {
                self.log.push(t.value.clone());
            }
            fn visit_variable(&mut self, v: &Variable) {
                self.log.push(format!("var:{}", v.name));
            }
        }
        let branch = IfBlockBranch {
            expression: Some(num(1.0)),
            children: vec![text("c1")],
            expression_alias: Some(Variable {
                name: "alias".to_string(),
                value: String::new(),
                source_span: span(),
                key_span: span(),
                value_span: None,
            }),
            spans: block_spans(),
            i18n: None,
        };
        let mut v = Rec::default();
        v.visit_node(&Node::IfBlockBranch(branch));
        assert_eq!(v.log, vec!["c1", "var:alias"]);
    }

    #[test]
    fn visit_all_collect_drops_none() {
        let nodes = vec![text("keep"), text("drop"), text("keep")];
        let out: Vec<String> = visit_all_collect(&nodes, |n| match n {
            Node::Text(t) if t.value == "keep" => Some(t.value.clone()),
            _ => None,
        });
        assert_eq!(out, vec!["keep", "keep"]);
    }

    #[test]
    fn content_name_constant() {
        assert_eq!(Content::NAME, "ng-content");
    }

    #[test]
    fn node_source_span_accessor() {
        let n = text("x");
        assert_eq!(n.source_span(), &span());
    }
}
