//! t2 binder — resolves what a template references (elements -> directives/components,
//! variables, references, expression roots). FOUNDATION for selectorless + auto-import.
//!
//! PORT TARGET: `migration/render3-specs/10-t2_binder.md`
//! Source: `tools/angular-ref/packages/compiler/src/render3/view/t2_binder.ts` (+ `t2_api.ts`),
//! Angular 22.1.0-next.0.
//!
//! Ports the binder over [`crate::template::r3_ast`]: the `Scope`/template-variable resolution,
//! reference resolution, directive/component matching, and — above all — the Angular 22
//! SELECTORLESS path. In selectorless mode `Component`/`Directive` template nodes are matched to
//! imported symbols *by class name* (`Component.component_name` / `Directive.name`); names that
//! fail to match are recorded in `missing_directives`, and [`R3BoundTarget::referenced_directive_exists`]
//! is the lookup that drives auto-import with no duplicate declarations.
//!
//! # Identity model
//!
//! TS keys every map on parsed-node object identity. This crate's r3_ast is an OWNED, arena-free
//! IR (`Box`/`Vec`/`String`), so there is no GC identity and no node-id arena. We therefore bind
//! over a *borrowed* template (`&'t [Node]`) and key all maps on the **pointer address** of the
//! borrowed node (`*const T as usize`, see [`addr`]). This is faithful to TS object identity as
//! long as the bound template outlives the [`R3BoundTarget`] (enforced by the `'t` lifetime) and
//! is not moved while bound (it is borrowed immutably, so it cannot be). When the spec's node-id
//! arena lands in `r3_ast`, these `usize` keys become the obvious typed `NodeId`s.
//!
//! # Not-yet-ported siblings (minimal local placeholders, see notes inline)
//!
//! - `directive_matching` (`CssSelector` / `SelectorMatcher` / `SelectorlessMatcher`): a minimal
//!   owned matcher is defined here. The selectorless matcher (the keystone) is faithful — exact
//!   class-name lookup. The selector matcher is a *reduced* engine (element-name + attribute-name
//!   membership) sufficient for the binder's algorithm and tests; full CSS-selector semantics are
//!   deferred to the real `directive_matching` port.
//! - `property_mapping` (`ClassPropertyMapping`): a minimal owned mapping is defined here, exposing
//!   the only operations the binder uses (`has_binding_property_name`, iteration of
//!   `InputOrOutput`, `from_mapped_object`).
//! - `DirectiveMeta`: the spec notes this is supplied by the caller (the bridge from the metadata
//!   stage). We define a concrete owned [`DirectiveMeta`] with exactly the fields the binder reads.

use std::collections::{HashMap, HashSet};

use crate::expression::ast::{AstNode, AstVisitor, ExprKind};
use crate::template::r3_ast::{
    self as t, BoundAttribute, BoundEvent, Component, Content, DeferredBlock, DeferredBlockError,
    DeferredBlockLoading, DeferredBlockPlaceholder, Directive, Element, ForLoopBlock,
    ForLoopBlockEmpty, IfBlock, IfBlockBranch, LetDeclaration, Node, Reference, SwitchBlock,
    SwitchBlockCaseGroup, Template, TemplateAttr, TextAttribute, Variable, Visitor,
};

// ===========================================================================
// Identity helper.
// ===========================================================================

/// Stable identity of a borrowed node: its address. See the module-level "Identity model" note.
#[inline]
fn addr<T>(node: &T) -> usize {
    node as *const T as usize
}

// ===========================================================================
// Minimal placeholders for not-yet-ported `property_mapping`.
// ===========================================================================

/// Placeholder for `property_mapping.InputOrOutput`. One input/output binding of a directive.
///
/// NOTE: minimal owned stand-in for the not-yet-ported `property_mapping` module. Real port should
/// replace this with the full `InputOrOutput` (transform flags, declared type, etc.).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputOrOutput {
    /// The public (template-facing) binding name, e.g. `[myInput]`.
    pub binding_property_name: String,
    /// The class field name backing the binding.
    pub class_property_name: String,
    /// Whether the input/output is signal-based (`input()` / `output()`).
    pub is_signal: bool,
}

/// Placeholder for `property_mapping.ClassPropertyMapping`. Maps class property names to the
/// binding metadata, and supports membership queries by *binding* (public) name.
///
/// NOTE: minimal owned stand-in for the not-yet-ported `property_mapping` module. Only the methods
/// the binder actually uses are provided.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassPropertyMapping {
    /// Insertion-ordered list of bindings (order preserved for `from_mapped_object` round-trips).
    bindings: Vec<InputOrOutput>,
}

impl ClassPropertyMapping {
    pub fn new(bindings: Vec<InputOrOutput>) -> Self {
        ClassPropertyMapping { bindings }
    }

    /// Convenience constructor from `(binding_property_name, class_property_name)` pairs.
    pub fn from_pairs<I, S>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (S, S)>,
        S: Into<String>,
    {
        ClassPropertyMapping {
            bindings: pairs
                .into_iter()
                .map(|(b, c)| InputOrOutput {
                    binding_property_name: b.into(),
                    class_property_name: c.into(),
                    is_signal: false,
                })
                .collect(),
        }
    }

    /// `ClassPropertyMapping.fromMappedObject`: rebuild from a `class_property_name -> binding`
    /// map. Iteration order of the source map is preserved by the caller passing a `Vec`.
    pub fn from_mapped_object(entries: Vec<InputOrOutput>) -> Self {
        ClassPropertyMapping { bindings: entries }
    }

    /// `hasBindingPropertyName(name)` — does any binding claim this public name?
    pub fn has_binding_property_name(&self, name: &str) -> bool {
        self.bindings
            .iter()
            .any(|b| b.binding_property_name == name)
    }

    /// Iterate bindings (mirrors TS `for (const binding of mapping)`).
    pub fn iter(&self) -> impl Iterator<Item = &InputOrOutput> {
        self.bindings.iter()
    }
}

// ===========================================================================
// `t2_api` types.
// ===========================================================================

/// `MatchSource` — how a directive came to match a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchSource {
    /// Matched by selector (or, in selectorless mode, by class name).
    Selector,
    /// Applied as a host directive of another matched directive.
    HostDirective,
}

/// `t2_api.DirectiveMeta` — the metadata bridge the caller supplies. Only the fields the binder
/// reads are modeled (see spec §3). `ref_key` is the dedup/identity key (TS `ref.key`).
///
/// NOTE: the TS code makes this generic (`DirectiveT extends DirectiveMeta`); per spec §3 we use a
/// concrete struct plus the `ref_key` identity string rather than reproducing the genericity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectiveMeta {
    pub name: String,
    /// `ref.key` — dedup / identity key.
    pub ref_key: String,
    pub selector: Option<String>,
    pub is_component: bool,
    pub inputs: ClassPropertyMapping,
    pub outputs: ClassPropertyMapping,
    pub export_as: Option<Vec<String>>,
    pub is_structural: bool,
    pub match_source: MatchSource,
}

impl DirectiveMeta {
    /// Minimal constructor for the common case (selector match source, no host-directive merging).
    pub fn new(name: impl Into<String>, selector: Option<String>, is_component: bool) -> Self {
        let name = name.into();
        DirectiveMeta {
            ref_key: name.clone(),
            name,
            selector,
            is_component,
            inputs: ClassPropertyMapping::default(),
            outputs: ClassPropertyMapping::default(),
            export_as: None,
            is_structural: false,
            match_source: MatchSource::Selector,
        }
    }
}

/// `t2_api.ForeignComponentMeta` — a non-Angular (foreign) component matched by element name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForeignComponentMeta {
    pub name: String,
    pub ref_key: String,
}

/// `t2_api.DirectiveOwner` — `Element | Template | Component | Directive | HostElement`.
/// Stored as the node's identity address plus a kind discriminator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DirectiveOwner {
    Element(usize),
    Template(usize),
    Component(usize),
    Directive(usize),
    HostElement(usize),
}

impl DirectiveOwner {
    fn id(self) -> usize {
        match self {
            DirectiveOwner::Element(id)
            | DirectiveOwner::Template(id)
            | DirectiveOwner::Component(id)
            | DirectiveOwner::Directive(id)
            | DirectiveOwner::HostElement(id) => id,
        }
    }
}

/// `t2_api.ScopedNode` — the closed union of nodes that introduce a binding scope. Keyed by node
/// identity (`None` == the root scope). `HostElement` is included for the host-binding pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScopedNode {
    Template(usize),
    SwitchBlockCaseGroup(usize),
    IfBlockBranch(usize),
    ForLoopBlock(usize),
    ForLoopBlockEmpty(usize),
    DeferredBlock(usize),
    DeferredBlockError(usize),
    DeferredBlockLoading(usize),
    DeferredBlockPlaceholder(usize),
    Content(usize),
    HostElement(usize),
}

/// `t2_api.TemplateEntity` — `Reference | Variable | LetDeclaration`. Keyed by node identity; the
/// `name` is duplicated for the by-name scope lookups the binder performs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TemplateEntity {
    Reference { id: usize, name: String },
    Variable { id: usize, name: String },
    LetDeclaration { id: usize, name: String },
}

impl TemplateEntity {
    /// The declared name of the entity.
    pub fn name(&self) -> &str {
        match self {
            TemplateEntity::Reference { name, .. }
            | TemplateEntity::Variable { name, .. }
            | TemplateEntity::LetDeclaration { name, .. } => name,
        }
    }

    fn id(&self) -> usize {
        match self {
            TemplateEntity::Reference { id, .. }
            | TemplateEntity::Variable { id, .. }
            | TemplateEntity::LetDeclaration { id, .. } => *id,
        }
    }

    /// Whether this entity is specifically a `Reference`.
    pub fn is_reference(&self) -> bool {
        matches!(self, TemplateEntity::Reference { .. })
    }
}

/// `t2_api.ReferenceTarget` — what a `#ref` points at: an element, a template, or a specific
/// directive on a node.
#[derive(Clone, Debug, PartialEq)]
pub enum ReferenceTarget {
    Element(usize),
    Template(usize),
    Directive {
        directive: DirectiveMeta,
        /// The owning node (an `Element | Template | Component | Directive`, never `HostElement`).
        node: DirectiveOwner,
    },
}

/// Binding-consumer: who owns a particular `BoundAttribute`/`BoundEvent`/`TextAttribute`.
#[derive(Clone, Debug, PartialEq)]
pub enum BindingConsumer {
    Directive(DirectiveMeta),
    Element(usize),
    Template(usize),
}

/// `'input' | 'output'` discriminator for conflict reporting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingKind {
    Input,
    Output,
}

/// `t2_api.ConflictingHostDirectiveBinding`.
#[derive(Clone, Debug, PartialEq)]
pub struct ConflictingHostDirectiveBinding {
    pub directive: DirectiveMeta,
    pub class_property_name: String,
    pub conflicting_aliases: HashSet<String>,
    pub kind: BindingKind,
}

/// `t2_api.Target` — the thing being bound: a template node array and/or a host element with its
/// directives. At least one must be present (enforced by [`R3TargetBinder::bind`]).
#[derive(Clone, Debug, Default)]
pub struct Target<'t> {
    pub template: Option<&'t [Node]>,
    pub host: Option<HostTarget<'t>>,
}

/// The host arm of a [`Target`].
#[derive(Clone, Debug)]
pub struct HostTarget<'t> {
    pub node: &'t t::HostElement,
    pub directives: Vec<DirectiveMeta>,
}

// ===========================================================================
// Minimal `directive_matching` placeholders.
// ===========================================================================

/// The matcher kind the binder accepts. This is the selectorless/selector switch the whole
/// `DirectiveBinder` branches on (spec §7: "Selectorless is the matcher-kind switch").
///
/// NOTE: minimal owned stand-in for the not-yet-ported `directive_matching` module.
pub enum DirectiveMatcher {
    /// Selector-based matching (reduced engine; see [`SelectorMatcher`]).
    Selector(SelectorMatcher),
    /// Selectorless matching by class name (the Angular 22 keystone path; faithful).
    Selectorless(SelectorlessMatcher<DirectiveMeta>),
}

/// Reduced `SelectorMatcher`. Each registered selector is decomposed into an optional element name
/// plus a set of required attribute names. A node matches a selector when the element name agrees
/// (or the selector has none) and every required attribute is present on the node.
///
/// NOTE: this is a *reduced* selector engine (no `:not`, `,` groups, classes, attribute *values*).
/// It is sufficient for the binder's algorithm and the tests. Full CSS-selector semantics are
/// deferred to the real `directive_matching` port.
#[derive(Default)]
pub struct SelectorMatcher {
    entries: Vec<(ParsedSelector, Vec<DirectiveMeta>)>,
}

/// A parsed reduced selector: `element[attr1][attr2]...`.
struct ParsedSelector {
    element: Option<String>,
    attributes: Vec<String>,
}

impl SelectorMatcher {
    pub fn new() -> Self {
        SelectorMatcher::default()
    }

    /// `addSelectables(CssSelector.parse(selector), payloads)`.
    pub fn add_selectables(&mut self, selector: &str, payloads: Vec<DirectiveMeta>) {
        self.entries.push((parse_selector(selector), payloads));
    }

    /// `match(cssSelector, cb)` — collect every payload whose selector matches the node.
    fn match_node(&self, css: &CssSelector, out: &mut Vec<DirectiveMeta>) {
        for (sel, payloads) in &self.entries {
            let element_ok = match &sel.element {
                None => true,
                Some(name) => css.element.as_deref() == Some(name.as_str()),
            };
            let attrs_ok = sel
                .attributes
                .iter()
                .all(|a| css.attributes.iter().any(|x| x == a));
            if element_ok && attrs_ok {
                out.extend(payloads.iter().cloned());
            }
        }
    }
}

/// `directive_matching.SelectorlessMatcher` — keyed purely on class name. This is the keystone of
/// the Angular 22 selectorless / auto-import path: a `Component`/`Directive` template node matches
/// an imported symbol iff their class names are equal.
pub struct SelectorlessMatcher<T> {
    registry: HashMap<String, Vec<T>>,
}

impl<T: Clone> SelectorlessMatcher<T> {
    pub fn new() -> Self {
        SelectorlessMatcher {
            registry: HashMap::new(),
        }
    }

    /// Register a payload under a class name.
    pub fn add(&mut self, name: impl Into<String>, payload: T) {
        self.registry.entry(name.into()).or_default().push(payload);
    }

    /// `match(name)` — every payload registered under the exact class name.
    pub fn match_name(&self, name: &str) -> Vec<T> {
        self.registry.get(name).cloned().unwrap_or_default()
    }
}

impl<T: Clone> Default for SelectorlessMatcher<T> {
    fn default() -> Self {
        SelectorlessMatcher::new()
    }
}

/// Reduced `CssSelector` extracted from a node (element name + attribute names). See
/// `createCssSelectorFromNode` in `util.ts`.
struct CssSelector {
    element: Option<String>,
    attributes: Vec<String>,
}

/// Parse a reduced selector string of the form `element[a][b]` / `[a]` / `element`.
fn parse_selector(selector: &str) -> ParsedSelector {
    let mut element: Option<String> = None;
    let mut attributes = Vec::new();
    let mut chars = selector.chars().peekable();
    let mut current = String::new();
    while let Some(&c) = chars.peek() {
        match c {
            '[' => {
                chars.next();
                let mut attr = String::new();
                while let Some(&d) = chars.peek() {
                    if d == ']' {
                        chars.next();
                        break;
                    }
                    // Stop at '=' to drop any attribute value (reduced engine ignores values).
                    if d == '=' {
                        // consume up to ']'
                        while let Some(&e) = chars.peek() {
                            if e == ']' {
                                chars.next();
                                break;
                            }
                            chars.next();
                        }
                        break;
                    }
                    attr.push(d);
                    chars.next();
                }
                if !attr.is_empty() {
                    attributes.push(attr);
                }
            }
            _ => {
                current.push(c);
                chars.next();
            }
        }
    }
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        element = Some(trimmed.to_string());
    }
    ParsedSelector {
        element,
        attributes,
    }
}

/// `createCssSelectorFromNode(node)` — build the reduced selector for an element or template.
/// The element name for a `Template` is `"ng-template"`. Attribute names are gathered from text
/// attributes, bound-attribute (input) names, output names, and (for templates) template-attr
/// names — matching what the real `util.ts` feeds the matcher (names only, in our reduced engine).
fn css_selector_from_element(el: &Element) -> CssSelector {
    let mut attributes = Vec::new();
    for a in &el.attributes {
        attributes.push(a.name.clone());
    }
    for i in &el.inputs {
        attributes.push(i.name.clone());
    }
    for o in &el.outputs {
        attributes.push(o.name.clone());
    }
    CssSelector {
        element: Some(el.name.clone()),
        attributes,
    }
}

fn css_selector_from_template(tpl: &Template) -> CssSelector {
    let mut attributes = Vec::new();
    for a in &tpl.attributes {
        attributes.push(a.name.clone());
    }
    for i in &tpl.inputs {
        attributes.push(i.name.clone());
    }
    for o in &tpl.outputs {
        attributes.push(o.name.clone());
    }
    for ta in &tpl.template_attrs {
        match ta {
            TemplateAttr::Bound(b) => attributes.push(b.name.clone()),
            TemplateAttr::Text(tx) => attributes.push(tx.name.clone()),
        }
    }
    CssSelector {
        element: tpl.tag_name.clone(),
        attributes,
    }
}

// ===========================================================================
// `R3TargetBinder` — the public entry point.
// ===========================================================================

/// `R3TargetBinder` — performs a binding operation on a [`Target`] and returns a [`R3BoundTarget`]
/// (the template analogue of `ts.TypeChecker`).
pub struct R3TargetBinder {
    directive_matcher: Option<DirectiveMatcher>,
    foreign_component_matcher: Option<SelectorlessMatcher<ForeignComponentMeta>>,
}

impl R3TargetBinder {
    pub fn new(directive_matcher: Option<DirectiveMatcher>) -> Self {
        R3TargetBinder {
            directive_matcher,
            foreign_component_matcher: None,
        }
    }

    /// Full constructor including the foreign-component matcher (defaults to `None`).
    pub fn with_foreign_matcher(
        directive_matcher: Option<DirectiveMatcher>,
        foreign_component_matcher: Option<SelectorlessMatcher<ForeignComponentMeta>>,
    ) -> Self {
        R3TargetBinder {
            directive_matcher,
            foreign_component_matcher,
        }
    }

    /// `bind(target)` — see spec §4.1. Panics (TS `throw`) on an empty target.
    pub fn bind<'t>(&self, target: Target<'t>) -> R3BoundTarget<'t> {
        if target.template.is_none() && target.host.is_none() {
            panic!("Empty bound targets are not supported");
        }

        let mut directives: HashMap<DirectiveOwner, Vec<DirectiveMeta>> = HashMap::new();
        let mut foreign_components: HashMap<usize, ForeignComponentMeta> = HashMap::new();
        let mut eager_directives: Vec<DirectiveMeta> = Vec::new();
        let mut missing_directives: HashSet<String> = HashSet::new();
        let mut bindings: HashMap<usize, BindingConsumer> = HashMap::new();
        let mut references: HashMap<usize, ReferenceTarget> = HashMap::new();
        let mut scoped_node_entities: HashMap<Option<ScopedNode>, Vec<TemplateEntity>> =
            HashMap::new();
        let mut expressions: HashMap<usize, TemplateEntity> = HashMap::new();
        let mut symbols: HashMap<usize, ScopedNode> = HashMap::new();
        let mut nesting_level: HashMap<ScopedNode, usize> = HashMap::new();
        let mut used_pipes: HashSet<String> = HashSet::new();
        let mut eager_pipes: HashSet<String> = HashSet::new();
        let mut defer_blocks: Vec<(usize, usize)> = Vec::new(); // (DeferredBlock id, ScopeId)
        let mut conflicting: HashMap<DirectiveOwner, Vec<ConflictingHostDirectiveBinding>> =
            HashMap::new();

        // The scope arena lives for the whole bind; the BoundTarget keeps it for query methods.
        let mut scopes = ScopeArena::new();

        if let Some(template) = target.template {
            // 1. Build the lexical scope tree.
            let root = ScopeBuilder::apply(&mut scopes, ScopeInput::Nodes(template));

            // 2. Flatten the scope tree into the per-node visible-entity sets.
            extract_scoped_node_entities(&scopes, &mut scoped_node_entities);

            // 3. Directive matching, binding ownership, reference resolution, foreign components.
            DirectiveBinder::apply(
                template,
                self.directive_matcher.as_ref(),
                self.foreign_component_matcher.as_ref(),
                &mut directives,
                &mut foreign_components,
                &mut eager_directives,
                &mut missing_directives,
                &mut bindings,
                &mut references,
                &mut conflicting,
            );

            // 4. Expression/symbol/pipe/nesting/defer binding.
            TemplateBinder::apply_with_scope(
                TemplateBinderInput::Nodes(template),
                &scopes,
                root,
                &mut expressions,
                &mut symbols,
                &mut nesting_level,
                &mut used_pipes,
                &mut eager_pipes,
                &mut defer_blocks,
            );
        }

        // Host element: directives registered directly, only TemplateBinder runs (spec §4.1.4).
        if let Some(host) = &target.host {
            let owner = DirectiveOwner::HostElement(addr(host.node));
            directives.insert(owner, host.directives.clone());

            let host_root = ScopeBuilder::apply(&mut scopes, ScopeInput::Host(host.node));
            TemplateBinder::apply_with_scope(
                TemplateBinderInput::Host(host.node),
                &scopes,
                host_root,
                &mut expressions,
                &mut symbols,
                &mut nesting_level,
                &mut used_pipes,
                &mut eager_pipes,
                &mut defer_blocks,
            );
        }

        R3BoundTarget {
            target,
            scopes,
            directives,
            foreign_components,
            eager_directives,
            missing_directives,
            bindings,
            references,
            expressions,
            symbols,
            nesting_level,
            scoped_node_entities,
            used_pipes,
            eager_pipes,
            defer_block_scopes: defer_blocks,
            conflicting,
        }
    }
}

// ===========================================================================
// Scope arena + Scope (lexical scope tree).
// ===========================================================================

type ScopeId = usize;

/// `Scope` — a binding scope within a template (spec §4.2). Stored in a [`ScopeArena`]; parent and
/// child links are arena ids (avoids `Rc`/back-pointer aliasing).
pub struct Scope {
    /// Named members of the scope (`Reference`/`Variable`/`LetDeclaration`), first-declaration-wins.
    named_entities: HashMap<String, TemplateEntity>,
    /// `Element | Component` node ids that belong to this scope (used by `is_deferred`).
    element_like_in_scope: HashSet<usize>,
    /// Child scopes keyed by the scoped node that introduces them.
    child_scopes: HashMap<ScopedNode, ScopeId>,
    /// True if this scope or any ancestor is deferred.
    is_deferred: bool,
    parent_scope: Option<ScopeId>,
    /// Root node of this scope; `None` for the root scope.
    root_node: Option<ScopedNode>,
}

/// Arena holding all [`Scope`]s for one bind.
pub struct ScopeArena {
    scopes: Vec<Scope>,
}

impl ScopeArena {
    fn new() -> Self {
        ScopeArena { scopes: Vec::new() }
    }

    fn alloc(&mut self, parent: Option<ScopeId>, root_node: Option<ScopedNode>) -> ScopeId {
        let is_deferred = match parent {
            Some(p) if self.scopes[p].is_deferred => true,
            _ => matches!(root_node, Some(ScopedNode::DeferredBlock(_))),
        };
        let id = self.scopes.len();
        self.scopes.push(Scope {
            named_entities: HashMap::new(),
            element_like_in_scope: HashSet::new(),
            child_scopes: HashMap::new(),
            is_deferred,
            parent_scope: parent,
            root_node,
        });
        id
    }

    fn get(&self, id: ScopeId) -> &Scope {
        &self.scopes[id]
    }

    /// `lookup(name)` — search this scope then walk up parents.
    fn lookup(&self, id: ScopeId, name: &str) -> Option<TemplateEntity> {
        let scope = &self.scopes[id];
        if let Some(e) = scope.named_entities.get(name) {
            return Some(e.clone());
        }
        match scope.parent_scope {
            Some(p) => self.lookup(p, name),
            None => None,
        }
    }
}

/// Top-level input to `ScopeBuilder::apply` — mirrors the two entry shapes of TS `Scope.apply`
/// (`Node[]` for a whole template, or a `HostElement`). Scoped *child* nodes are descended into
/// directly by the `*_dispatch` helpers (which thread the `'t` borrow), so they are not modeled as
/// `ScopeInput` variants.
enum ScopeInput<'t> {
    Nodes(&'t [Node]),
    /// The host element is intentionally ignored during scope ingestion (spec §4.2), so its
    /// payload is never read — it exists only to document/distinguish the host entry shape.
    #[allow(dead_code)]
    Host(&'t t::HostElement),
}

/// The scope-construction visitor (spec §4.2). Walks template nodes building the [`ScopeArena`].
/// (TS's `Scope` class merged the data and the visitor; here the data lives in [`Scope`] and the
/// construction pass is this `ScopeBuilder`.)
struct ScopeBuilder<'a, 't> {
    arena: &'a mut ScopeArena,
    /// The scope being populated.
    current: ScopeId,
    _marker: std::marker::PhantomData<&'t ()>,
}

impl<'a, 't> ScopeBuilder<'a, 't> {
    /// `Scope.apply` — build the scope tree, returning the root scope id.
    fn apply(arena: &'a mut ScopeArena, input: ScopeInput<'t>) -> ScopeId {
        let root = arena.alloc(None, None);
        let mut s = ScopeBuilder {
            arena,
            current: root,
            _marker: std::marker::PhantomData,
        };
        s.ingest(input);
        root
    }

    fn ingest(&mut self, input: ScopeInput<'t>) {
        match input {
            ScopeInput::Nodes(nodes) => {
                for n in nodes {
                    self.visit_node(n);
                }
            }
            // HostElement is explicitly ignored during scope ingestion (spec §4.2).
            ScopeInput::Host(_) => {}
        }
    }

    fn maybe_declare(&mut self, entity: TemplateEntity) {
        let scope = &mut self.arena.scopes[self.current];
        scope
            .named_entities
            .entry(entity.name().to_string())
            .or_insert(entity);
    }
}

impl<'a, 't> Visitor for ScopeBuilder<'a, 't> {
    // NOTE: the r3_ast `Visitor` trait borrows nodes as `&Element` etc. with an anonymous
    // lifetime, but our scope construction needs `'t`-lifetime references to store ids. We only
    // store the *address* (a `usize`) and recurse, so the anonymous-lifetime borrow is sufficient
    // — addresses are lifetime-free. Child recursion goes back through `self.visit_node`.

    fn visit_element(&mut self, element: &Element) {
        // SAFETY of identity: we key on the address; recursion uses the same borrowed subtree.
        let id = addr(element);
        // Re-borrow children with the trait's anonymous lifetime is fine; we transmute nothing.
        self.visit_element_like_dispatch(id, &element.directives, &element.references, &element.children);
    }

    fn visit_template(&mut self, template: &Template) {
        for d in &template.directives {
            self.visit_directive(d);
        }
        // References on a <ng-template> are defined in the OUTER scope.
        for r in &template.references {
            self.visit_reference(r);
        }
        // Inner scope for the template body.
        self.ingest_scoped_node_dispatch_template(template);
    }

    fn visit_component(&mut self, component: &Component) {
        let id = addr(component);
        self.visit_element_like_dispatch(id, &component.directives, &component.references, &component.children);
    }

    fn visit_directive(&mut self, directive: &Directive) {
        for r in &directive.references {
            self.visit_reference(r);
        }
    }

    fn visit_variable(&mut self, variable: &Variable) {
        self.maybe_declare(TemplateEntity::Variable {
            id: addr(variable),
            name: variable.name.clone(),
        });
    }

    fn visit_reference(&mut self, reference: &Reference) {
        self.maybe_declare(TemplateEntity::Reference {
            id: addr(reference),
            name: reference.name.clone(),
        });
    }

    fn visit_let_declaration(&mut self, decl: &LetDeclaration) {
        self.maybe_declare(TemplateEntity::LetDeclaration {
            id: addr(decl),
            name: decl.name.clone(),
        });
    }

    fn visit_deferred_block(&mut self, deferred: &DeferredBlock) {
        self.ingest_scoped_node_dispatch_defer(deferred);
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
        self.ingest_scoped_node_dispatch_placeholder(block);
    }
    fn visit_deferred_block_error(&mut self, block: &DeferredBlockError) {
        self.ingest_scoped_node_dispatch_error(block);
    }
    fn visit_deferred_block_loading(&mut self, block: &DeferredBlockLoading) {
        self.ingest_scoped_node_dispatch_loading(block);
    }

    fn visit_switch_block(&mut self, block: &SwitchBlock) {
        for g in &block.groups {
            self.visit_switch_block_case_group(g);
        }
    }
    fn visit_switch_block_case_group(&mut self, block: &SwitchBlockCaseGroup) {
        self.ingest_scoped_node_dispatch_case_group(block);
    }

    fn visit_for_loop_block(&mut self, block: &ForLoopBlock) {
        self.ingest_scoped_node_dispatch_for(block);
        if let Some(empty) = &block.empty {
            self.visit_for_loop_block_empty(empty);
        }
    }
    fn visit_for_loop_block_empty(&mut self, block: &ForLoopBlockEmpty) {
        self.ingest_scoped_node_dispatch_for_empty(block);
    }

    fn visit_if_block(&mut self, block: &IfBlock) {
        for b in &block.branches {
            self.visit_if_block_branch(b);
        }
    }
    fn visit_if_block_branch(&mut self, block: &IfBlockBranch) {
        self.ingest_scoped_node_dispatch_if_branch(block);
    }

    fn visit_content(&mut self, content: &Content) {
        self.ingest_scoped_node_dispatch_content(content);
    }
}

// The r3_ast `Visitor` trait hands node references with an anonymous lifetime, but the dispatch
// helpers below need to thread the captured borrow as `ScopeInput<'t>`. Because we only ever store
// addresses (and recurse through the same borrowed tree), we provide thin wrappers that re-tie the
// lifetime. These transmute-free helpers reborrow through raw structural recursion.
impl<'a, 't> ScopeBuilder<'a, 't> {
    fn visit_element_like_dispatch(
        &mut self,
        id: usize,
        directives: &[Directive],
        references: &[Reference],
        children: &[Node],
    ) {
        for d in directives {
            <Self as Visitor>::visit_directive(self, d);
        }
        for r in references {
            <Self as Visitor>::visit_reference(self, r);
        }
        for c in children {
            self.visit_node(c);
        }
        self.arena.scopes[self.current].element_like_in_scope.insert(id);
    }

    fn ingest_scoped_node_dispatch_template(&mut self, template: &Template) {
        let node = ScopedNode::Template(addr(template));
        let child = self.arena.alloc(Some(self.current), Some(node));
        let parent = self.current;
        self.current = child;
        // Inline of `ingest(ScopeInput::Template)` against the anonymous borrow.
        for v in &template.variables {
            <Self as Visitor>::visit_variable(self, v);
        }
        for c in &template.children {
            self.visit_node(c);
        }
        self.current = parent;
        self.arena.scopes[parent].child_scopes.insert(node, child);
    }

    fn ingest_scoped_node_dispatch_if_branch(&mut self, branch: &IfBlockBranch) {
        let node = ScopedNode::IfBlockBranch(addr(branch));
        let child = self.arena.alloc(Some(self.current), Some(node));
        let parent = self.current;
        self.current = child;
        if let Some(alias) = &branch.expression_alias {
            <Self as Visitor>::visit_variable(self, alias);
        }
        for c in &branch.children {
            self.visit_node(c);
        }
        self.current = parent;
        self.arena.scopes[parent].child_scopes.insert(node, child);
    }

    fn ingest_scoped_node_dispatch_for(&mut self, block: &ForLoopBlock) {
        let node = ScopedNode::ForLoopBlock(addr(block));
        let child = self.arena.alloc(Some(self.current), Some(node));
        let parent = self.current;
        self.current = child;
        <Self as Visitor>::visit_variable(self, &block.item);
        for v in &block.context_variables {
            <Self as Visitor>::visit_variable(self, v);
        }
        for c in &block.children {
            self.visit_node(c);
        }
        self.current = parent;
        self.arena.scopes[parent].child_scopes.insert(node, child);
    }

    fn ingest_scoped_node_dispatch_for_empty(&mut self, block: &ForLoopBlockEmpty) {
        self.ingest_simple(ScopedNode::ForLoopBlockEmpty(addr(block)), &block.children);
    }
    fn ingest_scoped_node_dispatch_case_group(&mut self, block: &SwitchBlockCaseGroup) {
        self.ingest_simple(ScopedNode::SwitchBlockCaseGroup(addr(block)), &block.children);
    }
    fn ingest_scoped_node_dispatch_defer(&mut self, block: &DeferredBlock) {
        self.ingest_simple(ScopedNode::DeferredBlock(addr(block)), &block.children);
    }
    fn ingest_scoped_node_dispatch_error(&mut self, block: &DeferredBlockError) {
        self.ingest_simple(ScopedNode::DeferredBlockError(addr(block)), &block.children);
    }
    fn ingest_scoped_node_dispatch_placeholder(&mut self, block: &DeferredBlockPlaceholder) {
        self.ingest_simple(
            ScopedNode::DeferredBlockPlaceholder(addr(block)),
            &block.children,
        );
    }
    fn ingest_scoped_node_dispatch_loading(&mut self, block: &DeferredBlockLoading) {
        self.ingest_simple(ScopedNode::DeferredBlockLoading(addr(block)), &block.children);
    }
    fn ingest_scoped_node_dispatch_content(&mut self, content: &Content) {
        self.ingest_simple(ScopedNode::Content(addr(content)), &content.children);
    }

    /// Ingest a scoped node whose ingestion is just "recurse over children".
    fn ingest_simple(&mut self, node: ScopedNode, children: &[Node]) {
        let child = self.arena.alloc(Some(self.current), Some(node));
        let parent = self.current;
        self.current = child;
        for c in children {
            self.visit_node(c);
        }
        self.current = parent;
        self.arena.scopes[parent].child_scopes.insert(node, child);
    }
}

// ===========================================================================
// extractScopedNodeEntities (spec §4.5).
// ===========================================================================

fn extract_scoped_node_entities(
    arena: &ScopeArena,
    out: &mut HashMap<Option<ScopedNode>, Vec<TemplateEntity>>,
) {
    // Memoize the merged (own + inherited) named entities per scope id.
    let mut merged: HashMap<ScopeId, HashMap<String, TemplateEntity>> = HashMap::new();

    fn extract(
        arena: &ScopeArena,
        id: ScopeId,
        merged: &mut HashMap<ScopeId, HashMap<String, TemplateEntity>>,
    ) -> HashMap<String, TemplateEntity> {
        if let Some(m) = merged.get(&id) {
            return m.clone();
        }
        let scope = arena.get(id);
        let mut entities = match scope.parent_scope {
            Some(p) => extract(arena, p, merged),
            None => HashMap::new(),
        };
        // Own entities override / add (TS spreads `currentEntities` last).
        for (k, v) in &scope.named_entities {
            entities.insert(k.clone(), v.clone());
        }
        merged.insert(id, entities.clone());
        entities
    }

    // Walk every scope (stack-based, matching TS).
    let mut stack: Vec<ScopeId> = Vec::new();
    if !arena.scopes.is_empty() {
        stack.push(0);
    }
    let mut seen: HashSet<ScopeId> = HashSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        for &child in arena.get(id).child_scopes.values() {
            stack.push(child);
        }
        let _ = extract(arena, id, &mut merged);
    }

    for (id, entities) in &merged {
        let key = arena.get(*id).root_node;
        out.insert(key, entities.values().cloned().collect());
    }
}

// ===========================================================================
// DirectiveBinder (spec §4.3).
// ===========================================================================

struct DirectiveBinder<'a, 't> {
    directive_matcher: Option<&'a DirectiveMatcher>,
    foreign_matcher: Option<&'a SelectorlessMatcher<ForeignComponentMeta>>,
    directives: &'a mut HashMap<DirectiveOwner, Vec<DirectiveMeta>>,
    foreign_components: &'a mut HashMap<usize, ForeignComponentMeta>,
    eager_directives: &'a mut Vec<DirectiveMeta>,
    missing_directives: &'a mut HashSet<String>,
    bindings: &'a mut HashMap<usize, BindingConsumer>,
    references: &'a mut HashMap<usize, ReferenceTarget>,
    conflicting: &'a mut HashMap<DirectiveOwner, Vec<ConflictingHostDirectiveBinding>>,
    is_in_defer_block: bool,
    _marker: std::marker::PhantomData<&'t ()>,
}

impl<'a, 't> DirectiveBinder<'a, 't> {
    #[allow(clippy::too_many_arguments)]
    fn apply(
        template: &'t [Node],
        directive_matcher: Option<&'a DirectiveMatcher>,
        foreign_matcher: Option<&'a SelectorlessMatcher<ForeignComponentMeta>>,
        directives: &'a mut HashMap<DirectiveOwner, Vec<DirectiveMeta>>,
        foreign_components: &'a mut HashMap<usize, ForeignComponentMeta>,
        eager_directives: &'a mut Vec<DirectiveMeta>,
        missing_directives: &'a mut HashSet<String>,
        bindings: &'a mut HashMap<usize, BindingConsumer>,
        references: &'a mut HashMap<usize, ReferenceTarget>,
        conflicting: &'a mut HashMap<DirectiveOwner, Vec<ConflictingHostDirectiveBinding>>,
    ) {
        let mut binder = DirectiveBinder {
            directive_matcher,
            foreign_matcher,
            directives,
            foreign_components,
            eager_directives,
            missing_directives,
            bindings,
            references,
            conflicting,
            is_in_defer_block: false,
            _marker: std::marker::PhantomData,
        };
        for n in template {
            binder.visit_node(n);
        }
    }

    fn track_matched_directives(&mut self, owner: DirectiveOwner, matched: Vec<DirectiveMeta>) {
        if matched.is_empty() {
            return;
        }
        let deduped = self.dedupe_and_merge_directives(owner, matched);
        if !self.is_in_defer_block {
            self.eager_directives.extend(deduped.iter().cloned());
        }
        self.directives.insert(owner, deduped);
    }

    /// Host-directive dedup & merge (spec §4.3.1).
    fn dedupe_and_merge_directives(
        &mut self,
        owner: DirectiveOwner,
        matches: Vec<DirectiveMeta>,
    ) -> Vec<DirectiveMeta> {
        if matches.is_empty()
            || matches
                .iter()
                .all(|d| d.match_source == MatchSource::Selector)
        {
            return matches;
        }

        let mut selector_matches: HashSet<String> = HashSet::new();
        // Preserve discovery order of host-directive keys.
        let mut host_keys: Vec<String> = Vec::new();
        let mut host_directives: HashMap<String, Vec<DirectiveMeta>> = HashMap::new();

        for dir in &matches {
            if dir.match_source == MatchSource::Selector {
                selector_matches.insert(dir.ref_key.clone());
            } else {
                if !host_directives.contains_key(&dir.ref_key) {
                    host_keys.push(dir.ref_key.clone());
                    host_directives.insert(dir.ref_key.clone(), Vec::new());
                }
                host_directives
                    .get_mut(&dir.ref_key)
                    .unwrap()
                    .push(dir.clone());
            }
        }

        let mut merged_host: HashMap<String, DirectiveMeta> = HashMap::new();
        for key in &host_keys {
            if selector_matches.contains(key) {
                continue;
            }
            let dirs = &host_directives[key];
            if dirs.len() == 1 {
                merged_host.insert(key.clone(), dirs[0].clone());
                continue;
            }

            let mut inputs: Vec<InputOrOutput> = Vec::new();
            let mut outputs: Vec<InputOrOutput> = Vec::new();
            for dir in dirs {
                self.merge_mapping(owner, dir, BindingKind::Input, &mut inputs, &dir.inputs);
                self.merge_mapping(owner, dir, BindingKind::Output, &mut outputs, &dir.outputs);
            }
            let mut merged = dirs[0].clone();
            merged.inputs = ClassPropertyMapping::from_mapped_object(inputs);
            merged.outputs = ClassPropertyMapping::from_mapped_object(outputs);
            merged_host.insert(key.clone(), merged);
        }

        // Reassemble preserving original order.
        let mut result: Vec<DirectiveMeta> = Vec::new();
        for dir in matches {
            if dir.match_source == MatchSource::Selector {
                result.push(dir);
            } else if let Some(m) = merged_host.remove(&dir.ref_key) {
                result.push(m);
            }
        }
        result
    }

    fn merge_mapping(
        &mut self,
        owner: DirectiveOwner,
        directive: &DirectiveMeta,
        kind: BindingKind,
        accumulator: &mut Vec<InputOrOutput>,
        bindings: &ClassPropertyMapping,
    ) {
        for binding in bindings.iter() {
            let existing_idx = accumulator
                .iter()
                .position(|b| b.class_property_name == binding.class_property_name);

            let Some(idx) = existing_idx else {
                accumulator.push(binding.clone());
                continue;
            };
            let existing = &accumulator[idx];
            if existing.binding_property_name == binding.binding_property_name
                && existing.class_property_name == binding.class_property_name
                && existing.is_signal == binding.is_signal
            {
                continue;
            }

            // Conflicting alias for the same class property name.
            let conflicts = self.conflicting.entry(owner).or_default();
            let existing_binding_name = existing.binding_property_name.clone();
            let existing_class_name = existing.class_property_name.clone();
            let pos = conflicts.iter().position(|c| {
                c.directive.ref_key == directive.ref_key
                    && c.kind == kind
                    && c.class_property_name == binding.class_property_name
            });
            let conflict = match pos {
                Some(p) => &mut conflicts[p],
                None => {
                    let mut aliases = HashSet::new();
                    aliases.insert(existing_binding_name);
                    conflicts.push(ConflictingHostDirectiveBinding {
                        directive: directive.clone(),
                        kind,
                        class_property_name: existing_class_name,
                        conflicting_aliases: aliases,
                    });
                    let last = conflicts.len() - 1;
                    &mut conflicts[last]
                }
            };
            conflict
                .conflicting_aliases
                .insert(binding.binding_property_name.clone());
        }
    }

    /// Selectorless tracking (spec §4.3, `trackSelectorlessMatchesAndDirectives`).
    fn track_selectorless(
        &mut self,
        owner: DirectiveOwner,
        attrs: &SelectorlessAttrs<'t>,
        references: &'t [Reference],
        directives: Vec<DirectiveMeta>,
    ) {
        if directives.is_empty() {
            return;
        }
        self.track_matched_directives(owner, directives.clone());

        for dir in &directives {
            for input in attrs.inputs {
                if dir.inputs.has_binding_property_name(&input.name) {
                    self.bindings
                        .insert(addr(input), BindingConsumer::Directive(dir.clone()));
                }
            }
            for attr in attrs.attributes {
                if dir.inputs.has_binding_property_name(&attr.name) {
                    self.bindings
                        .insert(addr(attr), BindingConsumer::Directive(dir.clone()));
                }
            }
            for output in attrs.outputs {
                if dir.outputs.has_binding_property_name(&output.name) {
                    self.bindings
                        .insert(addr(output), BindingConsumer::Directive(dir.clone()));
                }
            }
        }

        // Selectorless reference resolution: it is (intentionally, upstream) still
        // unspecified how a `#ref` should behave when several directives match the same
        // selectorless node, so — matching Angular's `t2_binder` — we register the FIRST
        // matched directive as the reference target. `directives` is guaranteed non-empty
        // here (the early return above bails on an empty match).
        for r in references {
            self.references.insert(
                addr(r),
                ReferenceTarget::Directive {
                    directive: directives[0].clone(),
                    node: owner,
                },
            );
        }
    }

    /// Selector-based tracking (spec §4.3, `trackSelectorBasedBindingsAndDirectives`).
    fn track_selector_based(
        &mut self,
        owner: DirectiveOwner,
        node_attrs: &SelectorAttrs<'t>,
        references: &'t [Reference],
        directives: Vec<DirectiveMeta>,
    ) {
        self.track_matched_directives(owner, directives.clone());

        // Reference resolution.
        for r in references {
            let dir_target: Option<DirectiveMeta>;
            if r.value.trim().is_empty() {
                // Empty ref -> primary component (if any), else the node itself.
                dir_target = directives.iter().find(|d| d.is_component).cloned();
            } else {
                // Non-empty ref -> directive with a matching `exportAs`.
                dir_target = directives
                    .iter()
                    .find(|d| {
                        d.export_as
                            .as_ref()
                            .is_some_and(|names| names.iter().any(|v| v == &r.value))
                    })
                    .cloned();
                if dir_target.is_none() {
                    // Unknown target -> leave unmapped.
                    continue;
                }
            }

            match dir_target {
                Some(dir) => {
                    self.references.insert(
                        addr(r),
                        ReferenceTarget::Directive {
                            directive: dir,
                            node: owner,
                        },
                    );
                }
                None => {
                    self.references.insert(addr(r), owner_to_reference_target(owner));
                }
            }
        }

        // Binding ownership.
        let node_consumer = owner_to_binding_consumer(owner);
        for input in node_attrs.inputs {
            self.set_attribute_binding(&directives, &input.name, addr(input), true, &node_consumer);
        }
        for attr in node_attrs.attributes {
            self.set_attribute_binding(&directives, &attr.name, addr(attr), true, &node_consumer);
        }
        for ta in node_attrs.template_attrs {
            let (name, id) = match ta {
                TemplateAttr::Bound(b) => (&b.name, addr(b)),
                TemplateAttr::Text(t) => (&t.name, addr(t)),
            };
            self.set_attribute_binding(&directives, name, id, true, &node_consumer);
        }
        for output in node_attrs.outputs {
            self.set_attribute_binding(&directives, &output.name, addr(output), false, &node_consumer);
        }
    }

    fn set_attribute_binding(
        &mut self,
        directives: &[DirectiveMeta],
        name: &str,
        id: usize,
        is_input: bool,
        node_consumer: &BindingConsumer,
    ) {
        let dir = directives.iter().find(|d| {
            if is_input {
                d.inputs.has_binding_property_name(name)
            } else {
                d.outputs.has_binding_property_name(name)
            }
        });
        let consumer = match dir {
            Some(d) => BindingConsumer::Directive(d.clone()),
            None => node_consumer.clone(),
        };
        self.bindings.insert(id, consumer);
    }

    fn visit_element_or_template_element(&mut self, element: &'t Element) {
        let owner = DirectiveOwner::Element(addr(element));
        let mut matched: Vec<DirectiveMeta> = Vec::new();

        match self.directive_matcher {
            Some(DirectiveMatcher::Selector(sm)) => {
                let css = css_selector_from_element(element);
                sm.match_node(&css, &mut matched);
                let attrs = SelectorAttrs {
                    inputs: &element.inputs,
                    attributes: &element.attributes,
                    outputs: &element.outputs,
                    template_attrs: &[],
                };
                self.track_selector_based(owner, &attrs, &element.references, matched.clone());
            }
            _ => {
                // Selectorless (or no matcher): bare empty references map to the node itself.
                for r in &element.references {
                    if r.value.trim().is_empty() {
                        self.references.insert(addr(r), ReferenceTarget::Element(addr(element)));
                    }
                }
            }
        }

        // Foreign components (element nodes only).
        if let Some(fm) = self.foreign_matcher {
            let foreign = fm.match_name(&element.name);
            if !foreign.is_empty() {
                if !matched.is_empty() {
                    panic!(
                        "Conflict: Element '{}' matches both an Angular directive and a foreign component.",
                        element.name
                    );
                }
                self.foreign_components.insert(addr(element), foreign[0].clone());
            }
        }

        for d in &element.directives {
            self.visit_directive(d);
        }
        for c in &element.children {
            self.visit_node(c);
        }
    }

    fn visit_element_or_template_template(&mut self, template: &'t Template) {
        let owner = DirectiveOwner::Template(addr(template));
        match self.directive_matcher {
            Some(DirectiveMatcher::Selector(sm)) => {
                let css = css_selector_from_template(template);
                let mut matched: Vec<DirectiveMeta> = Vec::new();
                sm.match_node(&css, &mut matched);
                let attrs = SelectorAttrs {
                    inputs: &template.inputs,
                    attributes: &template.attributes,
                    outputs: &template.outputs,
                    template_attrs: &template.template_attrs,
                };
                self.track_selector_based(owner, &attrs, &template.references, matched);
            }
            _ => {
                for r in &template.references {
                    if r.value.trim().is_empty() {
                        self.references
                            .insert(addr(r), ReferenceTarget::Template(addr(template)));
                    }
                }
            }
        }

        for d in &template.directives {
            self.visit_directive(d);
        }
        for c in &template.children {
            self.visit_node(c);
        }
    }
}

/// Bundle of a node's bindable attributes for selector-based tracking.
struct SelectorAttrs<'t> {
    inputs: &'t [BoundAttribute],
    attributes: &'t [TextAttribute],
    outputs: &'t [BoundEvent],
    template_attrs: &'t [TemplateAttr],
}

/// Bundle of a selectorless node's bindable attributes.
struct SelectorlessAttrs<'t> {
    inputs: &'t [BoundAttribute],
    attributes: &'t [TextAttribute],
    outputs: &'t [BoundEvent],
}

fn owner_to_binding_consumer(owner: DirectiveOwner) -> BindingConsumer {
    match owner {
        DirectiveOwner::Element(id) | DirectiveOwner::HostElement(id) => {
            BindingConsumer::Element(id)
        }
        DirectiveOwner::Template(id) => BindingConsumer::Template(id),
        DirectiveOwner::Component(id) => BindingConsumer::Element(id),
        DirectiveOwner::Directive(id) => BindingConsumer::Element(id),
    }
}

fn owner_to_reference_target(owner: DirectiveOwner) -> ReferenceTarget {
    match owner {
        DirectiveOwner::Template(id) => ReferenceTarget::Template(id),
        other => ReferenceTarget::Element(other.id()),
    }
}

impl<'a, 't> Visitor for DirectiveBinder<'a, 't> {
    fn visit_element(&mut self, element: &Element) {
        // SAFETY/identity: see module note — keys are addresses; recursion is over the same tree.
        // We need the `'t` lifetime; the trait gives an anonymous one. Since we only read and
        // store addresses, and the borrowed tree is the same `'t` tree (the binder is only ever
        // driven over a `&'t [Node]`), we reborrow via a structural helper.
        let el: &'t Element = unsafe { std::mem::transmute::<&Element, &'t Element>(element) };
        self.visit_element_or_template_element(el);
    }

    fn visit_template(&mut self, template: &Template) {
        let tpl: &'t Template = unsafe { std::mem::transmute::<&Template, &'t Template>(template) };
        self.visit_element_or_template_template(tpl);
    }

    fn visit_component(&mut self, node: &Component) {
        let node: &'t Component = unsafe { std::mem::transmute::<&Component, &'t Component>(node) };
        let owner = DirectiveOwner::Component(addr(node));
        if let Some(DirectiveMatcher::Selectorless(sm)) = self.directive_matcher {
            let matches = sm.match_name(&node.component_name);
            if !matches.is_empty() {
                let attrs = SelectorlessAttrs {
                    inputs: &node.inputs,
                    attributes: &node.attributes,
                    outputs: &node.outputs,
                };
                self.track_selectorless(owner, &attrs, &node.references, matches);
            } else {
                // The selectorless / auto-import signal.
                self.missing_directives.insert(node.component_name.clone());
            }
        }
        for d in &node.directives {
            self.visit_directive(d);
        }
        for c in &node.children {
            self.visit_node(c);
        }
    }

    fn visit_directive(&mut self, node: &Directive) {
        let node: &'t Directive = unsafe { std::mem::transmute::<&Directive, &'t Directive>(node) };
        if let Some(DirectiveMatcher::Selectorless(sm)) = self.directive_matcher {
            let owner = DirectiveOwner::Directive(addr(node));
            let matches = sm.match_name(&node.name);
            if !matches.is_empty() {
                let attrs = SelectorlessAttrs {
                    inputs: &node.inputs,
                    attributes: &node.attributes,
                    outputs: &node.outputs,
                };
                self.track_selectorless(owner, &attrs, &node.references, matches);
            } else {
                self.missing_directives.insert(node.name.clone());
            }
        }
    }

    fn visit_deferred_block(&mut self, deferred: &DeferredBlock) {
        let was = self.is_in_defer_block;
        self.is_in_defer_block = true;
        for c in &deferred.children {
            self.visit_node(c);
        }
        self.is_in_defer_block = was;
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

    fn visit_switch_block(&mut self, block: &SwitchBlock) {
        for g in &block.groups {
            self.visit_switch_block_case_group(g);
        }
    }
    fn visit_switch_block_case_group(&mut self, block: &SwitchBlockCaseGroup) {
        for c in &block.children {
            self.visit_node(c);
        }
    }

    fn visit_for_loop_block(&mut self, block: &ForLoopBlock) {
        for c in &block.children {
            self.visit_node(c);
        }
        if let Some(empty) = &block.empty {
            self.visit_for_loop_block_empty(empty);
        }
    }
    fn visit_for_loop_block_empty(&mut self, block: &ForLoopBlockEmpty) {
        for c in &block.children {
            self.visit_node(c);
        }
    }

    fn visit_if_block(&mut self, block: &IfBlock) {
        for b in &block.branches {
            self.visit_if_block_branch(b);
        }
    }
    fn visit_if_block_branch(&mut self, block: &IfBlockBranch) {
        for c in &block.children {
            self.visit_node(c);
        }
    }

    fn visit_content(&mut self, content: &Content) {
        for c in &content.children {
            self.visit_node(c);
        }
    }

    fn visit_deferred_block_placeholder(&mut self, block: &DeferredBlockPlaceholder) {
        for c in &block.children {
            self.visit_node(c);
        }
    }
    fn visit_deferred_block_error(&mut self, block: &DeferredBlockError) {
        for c in &block.children {
            self.visit_node(c);
        }
    }
    fn visit_deferred_block_loading(&mut self, block: &DeferredBlockLoading) {
        for c in &block.children {
            self.visit_node(c);
        }
    }
}

// ===========================================================================
// TemplateBinder (spec §4.4).
// ===========================================================================

enum TemplateBinderInput<'t> {
    Nodes(&'t [Node]),
    Host(&'t t::HostElement),
}

struct TemplateBinder<'a, 't> {
    arena: &'a ScopeArena,
    expressions: &'a mut HashMap<usize, TemplateEntity>,
    symbols: &'a mut HashMap<usize, ScopedNode>,
    nesting_level: &'a mut HashMap<ScopedNode, usize>,
    used_pipes: &'a mut HashSet<String>,
    eager_pipes: &'a mut HashSet<String>,
    defer_blocks: &'a mut Vec<(usize, usize)>,
    scope: ScopeId,
    /// The current rootNode (a `Template`/scoped node) or `None` at top level.
    root_node: Option<ScopedNode>,
    level: usize,
    _marker: std::marker::PhantomData<&'t ()>,
}

impl<'a, 't> TemplateBinder<'a, 't> {
    #[allow(clippy::too_many_arguments)]
    fn apply_with_scope(
        input: TemplateBinderInput<'t>,
        arena: &'a ScopeArena,
        root_scope: ScopeId,
        expressions: &'a mut HashMap<usize, TemplateEntity>,
        symbols: &'a mut HashMap<usize, ScopedNode>,
        nesting_level: &'a mut HashMap<ScopedNode, usize>,
        used_pipes: &'a mut HashSet<String>,
        eager_pipes: &'a mut HashSet<String>,
        defer_blocks: &'a mut Vec<(usize, usize)>,
    ) {
        let mut binder = TemplateBinder {
            arena,
            expressions,
            symbols,
            nesting_level,
            used_pipes,
            eager_pipes,
            defer_blocks,
            scope: root_scope,
            root_node: None,
            level: 0,
            _marker: std::marker::PhantomData,
        };
        binder.ingest(input);
    }

    fn ingest(&mut self, input: TemplateBinderInput<'t>) {
        match input {
            TemplateBinderInput::Nodes(nodes) => {
                for n in nodes {
                    self.visit_node(n);
                }
            }
            TemplateBinderInput::Host(host) => {
                // Host elements are always at the top level (nesting 0).
                self.nesting_level
                    .insert(ScopedNode::HostElement(addr(host)), 0);
            }
        }
    }

    /// Visit an expression for property/pipe roots in the current scope.
    fn visit_expr(&mut self, expr: &AstNode) {
        let mut ev = ExprResolver {
            arena: self.arena,
            scope: self.scope,
            is_deferred: self.arena.get(self.scope).is_deferred,
            expressions: self.expressions,
            used_pipes: self.used_pipes,
            eager_pipes: self.eager_pipes,
        };
        ev.visit(expr);
    }

    fn register_symbol(&mut self, entity_id: usize) {
        if let Some(root) = self.root_node {
            self.symbols.insert(entity_id, root);
        }
    }

    /// Recurse into a child scope (spec `ingestScopedNode`).
    fn ingest_scoped(&mut self, node: ScopedNode, body: ScopedBody<'t>) {
        let child_scope = *self.arena.get(self.scope).child_scopes.get(&node).expect(
            "Assertion error: resolved incorrect scope for scoped node",
        );
        let saved_scope = self.scope;
        let saved_root = self.root_node;
        let saved_level = self.level;
        self.scope = child_scope;
        self.root_node = Some(node);
        self.level += 1;

        match body {
            ScopedBody::Template(tpl) => {
                for v in &tpl.variables {
                    self.visit_variable(v);
                }
                for c in &tpl.children {
                    self.visit_node(c);
                }
                self.nesting_level.insert(node, self.level);
            }
            ScopedBody::IfBranch(branch) => {
                if let Some(alias) = &branch.expression_alias {
                    self.visit_variable(alias);
                }
                for c in &branch.children {
                    self.visit_node(c);
                }
                self.nesting_level.insert(node, self.level);
            }
            ScopedBody::ForLoop(block) => {
                self.visit_variable(&block.item);
                for v in &block.context_variables {
                    self.visit_variable(v);
                }
                if let Some(track) = &block.track_by {
                    self.visit_expr(&track.ast);
                }
                for c in &block.children {
                    self.visit_node(c);
                }
                self.nesting_level.insert(node, self.level);
            }
            ScopedBody::Defer(block) => {
                if self.arena.get(self.scope).root_node != Some(node) {
                    panic!("Assertion error: resolved incorrect scope for deferred block");
                }
                self.defer_blocks.push((addr(block), self.scope));
                for c in &block.children {
                    self.visit_node(c);
                }
                self.nesting_level.insert(node, self.level);
            }
            ScopedBody::Children(children) => {
                for c in children {
                    self.visit_node(c);
                }
                self.nesting_level.insert(node, self.level);
            }
        }

        self.scope = saved_scope;
        self.root_node = saved_root;
        self.level = saved_level;
    }
}

enum ScopedBody<'t> {
    Template(&'t Template),
    IfBranch(&'t IfBlockBranch),
    ForLoop(&'t ForLoopBlock),
    Defer(&'t DeferredBlock),
    Children(&'t [Node]),
}

impl<'a, 't> Visitor for TemplateBinder<'a, 't> {
    fn visit_template(&mut self, template: &Template) {
        let tpl: &'t Template = unsafe { std::mem::transmute::<&Template, &'t Template>(template) };
        // Inputs/outputs/directives/templateAttrs/references are processed in the OUTER scope.
        for i in &tpl.inputs {
            self.visit_expr(&i.value);
        }
        for o in &tpl.outputs {
            self.visit_expr(&o.handler);
        }
        for d in &tpl.directives {
            self.visit_directive(d);
        }
        for ta in &tpl.template_attrs {
            if let TemplateAttr::Bound(b) = ta {
                self.visit_expr(&b.value);
            }
        }
        for r in &tpl.references {
            self.visit_reference(r);
        }
        self.ingest_scoped(ScopedNode::Template(addr(tpl)), ScopedBody::Template(tpl));
    }

    fn visit_directive(&mut self, directive: &Directive) {
        for i in &directive.inputs {
            self.visit_expr(&i.value);
        }
        for o in &directive.outputs {
            self.visit_expr(&o.handler);
        }
        for r in &directive.references {
            self.visit_reference(r);
        }
    }

    fn visit_bound_attribute(&mut self, attribute: &BoundAttribute) {
        self.visit_expr(&attribute.value);
    }

    fn visit_bound_event(&mut self, attribute: &BoundEvent) {
        self.visit_expr(&attribute.handler);
    }

    fn visit_bound_text(&mut self, text: &t::BoundText) {
        self.visit_expr(&text.value);
    }

    fn visit_variable(&mut self, variable: &Variable) {
        self.register_symbol(addr(variable));
    }

    fn visit_reference(&mut self, reference: &Reference) {
        self.register_symbol(addr(reference));
    }

    fn visit_let_declaration(&mut self, decl: &LetDeclaration) {
        self.visit_expr(&decl.value);
        self.register_symbol(addr(decl));
    }

    fn visit_deferred_block(&mut self, deferred: &DeferredBlock) {
        let block: &'t DeferredBlock =
            unsafe { std::mem::transmute::<&DeferredBlock, &'t DeferredBlock>(deferred) };
        self.ingest_scoped(ScopedNode::DeferredBlock(addr(block)), ScopedBody::Defer(block));
        if let Some(w) = &block.triggers.when {
            if let t::DeferredTriggerKind::When { value } = &w.kind {
                self.visit_expr(value);
            }
        }
        if let Some(w) = &block.prefetch_triggers.when {
            if let t::DeferredTriggerKind::When { value } = &w.kind {
                self.visit_expr(value);
            }
        }
        if let Some(w) = &block.hydrate_triggers.when {
            if let t::DeferredTriggerKind::When { value } = &w.kind {
                self.visit_expr(value);
            }
        }
        if let Some(p) = &block.placeholder {
            self.visit_deferred_block_placeholder(p);
        }
        if let Some(l) = &block.loading {
            self.visit_deferred_block_loading(l);
        }
        if let Some(e) = &block.error {
            self.visit_deferred_block_error(e);
        }
    }

    fn visit_deferred_block_placeholder(&mut self, block: &DeferredBlockPlaceholder) {
        let b: &'t DeferredBlockPlaceholder = unsafe {
            std::mem::transmute::<&DeferredBlockPlaceholder, &'t DeferredBlockPlaceholder>(block)
        };
        self.ingest_scoped(
            ScopedNode::DeferredBlockPlaceholder(addr(b)),
            ScopedBody::Children(&b.children),
        );
    }
    fn visit_deferred_block_error(&mut self, block: &DeferredBlockError) {
        let b: &'t DeferredBlockError =
            unsafe { std::mem::transmute::<&DeferredBlockError, &'t DeferredBlockError>(block) };
        self.ingest_scoped(
            ScopedNode::DeferredBlockError(addr(b)),
            ScopedBody::Children(&b.children),
        );
    }
    fn visit_deferred_block_loading(&mut self, block: &DeferredBlockLoading) {
        let b: &'t DeferredBlockLoading = unsafe {
            std::mem::transmute::<&DeferredBlockLoading, &'t DeferredBlockLoading>(block)
        };
        self.ingest_scoped(
            ScopedNode::DeferredBlockLoading(addr(b)),
            ScopedBody::Children(&b.children),
        );
    }

    fn visit_switch_block(&mut self, block: &SwitchBlock) {
        self.visit_expr(&block.expression);
        for g in &block.groups {
            self.visit_switch_block_case_group(g);
        }
    }
    fn visit_switch_block_case_group(&mut self, block: &SwitchBlockCaseGroup) {
        let b: &'t SwitchBlockCaseGroup =
            unsafe { std::mem::transmute::<&SwitchBlockCaseGroup, &'t SwitchBlockCaseGroup>(block) };
        for case in &b.cases {
            if let Some(e) = &case.expression {
                self.visit_expr(e);
            }
        }
        self.ingest_scoped(
            ScopedNode::SwitchBlockCaseGroup(addr(b)),
            ScopedBody::Children(&b.children),
        );
    }

    fn visit_for_loop_block(&mut self, block: &ForLoopBlock) {
        let b: &'t ForLoopBlock =
            unsafe { std::mem::transmute::<&ForLoopBlock, &'t ForLoopBlock>(block) };
        self.visit_expr(&b.expression.ast);
        self.ingest_scoped(ScopedNode::ForLoopBlock(addr(b)), ScopedBody::ForLoop(b));
        if let Some(empty) = &b.empty {
            self.visit_for_loop_block_empty(empty);
        }
    }
    fn visit_for_loop_block_empty(&mut self, block: &ForLoopBlockEmpty) {
        let b: &'t ForLoopBlockEmpty =
            unsafe { std::mem::transmute::<&ForLoopBlockEmpty, &'t ForLoopBlockEmpty>(block) };
        self.ingest_scoped(
            ScopedNode::ForLoopBlockEmpty(addr(b)),
            ScopedBody::Children(&b.children),
        );
    }

    fn visit_if_block(&mut self, block: &IfBlock) {
        for b in &block.branches {
            self.visit_if_block_branch(b);
        }
    }
    fn visit_if_block_branch(&mut self, block: &IfBlockBranch) {
        let b: &'t IfBlockBranch =
            unsafe { std::mem::transmute::<&IfBlockBranch, &'t IfBlockBranch>(block) };
        if let Some(e) = &b.expression {
            self.visit_expr(e);
        }
        self.ingest_scoped(ScopedNode::IfBlockBranch(addr(b)), ScopedBody::IfBranch(b));
    }

    fn visit_content(&mut self, content: &Content) {
        let c: &'t Content = unsafe { std::mem::transmute::<&Content, &'t Content>(content) };
        self.ingest_scoped(ScopedNode::Content(addr(c)), ScopedBody::Children(&c.children));
    }
}

/// Expression sub-visitor: resolves `PropertyRead`/`SafePropertyRead` roots against the scope and
/// records pipe usage. Mirrors the expression-AST overrides of `TemplateBinder`.
struct ExprResolver<'a> {
    arena: &'a ScopeArena,
    scope: ScopeId,
    is_deferred: bool,
    expressions: &'a mut HashMap<usize, TemplateEntity>,
    used_pipes: &'a mut HashSet<String>,
    eager_pipes: &'a mut HashSet<String>,
}

impl<'a> ExprResolver<'a> {
    fn maybe_map(&mut self, node: &AstNode, receiver: &AstNode, name: &str) {
        // Only unqualified reads (receiver is ImplicitReceiver) can bind to a scope entity.
        if !matches!(receiver.kind, ExprKind::ImplicitReceiver) {
            return;
        }
        if let Some(entity) = self.arena.lookup(self.scope, name) {
            self.expressions.insert(addr(node), entity);
        }
    }
}

impl<'a> AstVisitor for ExprResolver<'a> {
    fn visit_pipe(&mut self, node: &AstNode) {
        if let ExprKind::BindingPipe { name, exp, args, .. } = &node.kind {
            self.used_pipes.insert(name.clone());
            if !self.is_deferred {
                self.eager_pipes.insert(name.clone());
            }
            self.visit(exp);
            self.visit_all(args);
        }
    }

    fn visit_property_read(&mut self, node: &AstNode) {
        if let ExprKind::PropertyRead { receiver, name, .. } = &node.kind {
            self.maybe_map(node, receiver, name);
            self.visit(receiver);
        }
    }

    fn visit_safe_property_read(&mut self, node: &AstNode) {
        if let ExprKind::SafePropertyRead { receiver, name, .. } = &node.kind {
            self.maybe_map(node, receiver, name);
            self.visit(receiver);
        }
    }
}

// ===========================================================================
// R3BoundTarget (the result object; spec §4.6).
// ===========================================================================

/// `R3BoundTarget` — the queryable result of [`R3TargetBinder::bind`]. Owns all the resolution
/// maps (keyed by node identity, see the module note) plus the scope arena (for defer-trigger and
/// `is_deferred` queries). Lives as long as the bound template (`'t`).
pub struct R3BoundTarget<'t> {
    pub target: Target<'t>,
    scopes: ScopeArena,
    directives: HashMap<DirectiveOwner, Vec<DirectiveMeta>>,
    foreign_components: HashMap<usize, ForeignComponentMeta>,
    eager_directives: Vec<DirectiveMeta>,
    missing_directives: HashSet<String>,
    bindings: HashMap<usize, BindingConsumer>,
    references: HashMap<usize, ReferenceTarget>,
    expressions: HashMap<usize, TemplateEntity>,
    symbols: HashMap<usize, ScopedNode>,
    nesting_level: HashMap<ScopedNode, usize>,
    scoped_node_entities: HashMap<Option<ScopedNode>, Vec<TemplateEntity>>,
    used_pipes: HashSet<String>,
    eager_pipes: HashSet<String>,
    /// (DeferredBlock id, ScopeId), in template order.
    defer_block_scopes: Vec<(usize, usize)>,
    conflicting: HashMap<DirectiveOwner, Vec<ConflictingHostDirectiveBinding>>,
}

impl<'t> R3BoundTarget<'t> {
    /// `getDirectivesOfNode`.
    pub fn get_directives_of_node(&self, node: DirectiveOwner) -> Option<&[DirectiveMeta]> {
        self.directives.get(&node).map(|v| v.as_slice())
    }

    /// `getForeignComponent`.
    pub fn get_foreign_component(&self, element: &Element) -> Option<&ForeignComponentMeta> {
        self.foreign_components.get(&addr(element))
    }

    /// `getReferenceTarget`.
    pub fn get_reference_target(&self, reference: &Reference) -> Option<&ReferenceTarget> {
        self.references.get(&addr(reference))
    }

    /// `getConsumerOfBinding` for a bound attribute.
    pub fn get_consumer_of_bound_attribute(
        &self,
        attribute: &BoundAttribute,
    ) -> Option<&BindingConsumer> {
        self.bindings.get(&addr(attribute))
    }
    /// `getConsumerOfBinding` for a bound event.
    pub fn get_consumer_of_bound_event(&self, event: &BoundEvent) -> Option<&BindingConsumer> {
        self.bindings.get(&addr(event))
    }
    /// `getConsumerOfBinding` for a text attribute.
    pub fn get_consumer_of_text_attribute(
        &self,
        attribute: &TextAttribute,
    ) -> Option<&BindingConsumer> {
        self.bindings.get(&addr(attribute))
    }

    /// `getExpressionTarget` — the scope entity an expression root resolves to.
    pub fn get_expression_target(&self, expr: &AstNode) -> Option<&TemplateEntity> {
        self.expressions.get(&addr(expr))
    }

    /// `getDefinitionNodeOfSymbol` — the scoped node that declares a `TemplateEntity`.
    pub fn get_definition_node_of_symbol(&self, symbol: &TemplateEntity) -> Option<ScopedNode> {
        self.symbols.get(&symbol.id()).copied()
    }

    /// `getNestingLevel`.
    pub fn get_nesting_level(&self, node: ScopedNode) -> usize {
        self.nesting_level.get(&node).copied().unwrap_or(0)
    }

    /// `getEntitiesInScope` — the full lexically-visible entity set at a scope (`None` == root).
    pub fn get_entities_in_scope(&self, node: Option<ScopedNode>) -> Vec<TemplateEntity> {
        self.scoped_node_entities
            .get(&node)
            .cloned()
            .unwrap_or_default()
    }

    /// `getUsedDirectives` — every matched directive (deduped).
    pub fn get_used_directives(&self) -> Vec<DirectiveMeta> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut out = Vec::new();
        for dirs in self.directives.values() {
            for d in dirs {
                if seen.insert(d.ref_key.clone()) {
                    out.push(d.clone());
                }
            }
        }
        out
    }

    /// `getEagerlyUsedDirectives`.
    pub fn get_eagerly_used_directives(&self) -> Vec<DirectiveMeta> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut out = Vec::new();
        for d in &self.eager_directives {
            if seen.insert(d.ref_key.clone()) {
                out.push(d.clone());
            }
        }
        out
    }

    /// `getUsedPipes`.
    pub fn get_used_pipes(&self) -> Vec<String> {
        self.used_pipes.iter().cloned().collect()
    }

    /// `getEagerlyUsedPipes`.
    pub fn get_eagerly_used_pipes(&self) -> Vec<String> {
        self.eager_pipes.iter().cloned().collect()
    }

    /// `getDeferBlocks` — defer-block node ids in template order.
    pub fn get_defer_blocks(&self) -> Vec<usize> {
        self.defer_block_scopes.iter().map(|(b, _)| *b).collect()
    }

    /// `isDeferred(element)` — DFS over each deferred block's scope subtree.
    pub fn is_deferred(&self, element: &Element) -> bool {
        let target = addr(element);
        for (_, scope_id) in &self.defer_block_scopes {
            let mut stack = vec![*scope_id];
            while let Some(id) = stack.pop() {
                let scope = self.scopes.get(id);
                if scope.element_like_in_scope.contains(&target) {
                    return true;
                }
                for &child in scope.child_scopes.values() {
                    stack.push(child);
                }
            }
        }
        false
    }

    /// `referencedDirectiveExists(name)` — the selectorless / auto-import lookup. A directly
    /// referenced selectorless `Component`/`Directive` class name "exists" iff it was matched
    /// (i.e. is NOT in the missing set).
    pub fn referenced_directive_exists(&self, name: &str) -> bool {
        !self.missing_directives.contains(name)
    }

    /// The raw set of class names that failed selectorless matching (the auto-import candidates).
    pub fn missing_directives(&self) -> &HashSet<String> {
        &self.missing_directives
    }

    /// `getConflictingHostDirectiveBindings`.
    pub fn get_conflicting_host_directive_bindings(
        &self,
        node: DirectiveOwner,
    ) -> Option<&[ConflictingHostDirectiveBinding]> {
        self.conflicting.get(&node).map(|v| v.as_slice())
    }
}

// ===========================================================================
// Tests.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expression::ast::{
        AbsoluteSourceSpan, AstNode, ExprKind, ParseSourceSpan, ParseSpan,
    };
    use crate::template::r3_ast::{
        BoundText, Element, Node, Reference, Template, Variable,
    };

    fn span() -> ParseSourceSpan {
        ParseSourceSpan { start: 0, end: 0 }
    }

    fn implicit_read(name: &str) -> AstNode {
        // `name` as `this.name`-style implicit read: PropertyRead { receiver: ImplicitReceiver }.
        let recv = AstNode::new(
            ParseSpan::new(0, 0),
            AbsoluteSourceSpan::new(0, 0),
            ExprKind::ImplicitReceiver,
        );
        AstNode::new(
            ParseSpan::new(0, 0),
            AbsoluteSourceSpan::new(0, 0),
            ExprKind::PropertyRead {
                name_span: AbsoluteSourceSpan::new(0, 0),
                receiver: Box::new(recv),
                name: name.to_string(),
            },
        )
    }

    fn reference(name: &str) -> Reference {
        Reference {
            name: name.to_string(),
            value: String::new(),
            source_span: span(),
            key_span: span(),
            value_span: None,
        }
    }

    fn variable(name: &str, value: &str) -> Variable {
        Variable {
            name: name.to_string(),
            value: value.to_string(),
            source_span: span(),
            key_span: span(),
            value_span: None,
        }
    }

    fn empty_element(name: &str) -> Element {
        Element {
            name: name.to_string(),
            attributes: vec![],
            inputs: vec![],
            outputs: vec![],
            directives: vec![],
            children: vec![],
            references: vec![],
            is_self_closing: false,
            source_span: span(),
            start_source_span: span(),
            end_source_span: None,
            is_void: false,
            i18n: None,
        }
    }

    /// A small template: `<my-cmp #ref>{{ item }}</my-cmp>` inside an `<ng-template let-item>`,
    /// bound with a SELECTOR matcher for `my-cmp` (a component) and a local reference.
    #[test]
    fn binds_component_variable_and_reference_selector_mode() {
        // <ng-template let-item> <my-cmp #ref>{{item}}</my-cmp> </ng-template>
        let inner_text = Node::BoundText(BoundText {
            value: implicit_read("item"),
            source_span: span(),
            i18n: None,
        });
        let mut my_cmp = empty_element("my-cmp");
        my_cmp.references.push(reference("ref"));
        my_cmp.children.push(inner_text);

        let template = Template {
            tag_name: Some("ng-template".to_string()),
            attributes: vec![],
            inputs: vec![],
            outputs: vec![],
            directives: vec![],
            template_attrs: vec![],
            children: vec![Node::Element(my_cmp)],
            references: vec![],
            variables: vec![variable("item", "$implicit")],
            is_self_closing: false,
            source_span: span(),
            start_source_span: span(),
            end_source_span: None,
            i18n: None,
        };
        let nodes = vec![Node::Template(template)];

        let mut sm = SelectorMatcher::new();
        let cmp_meta = DirectiveMeta::new("MyCmp", Some("my-cmp".to_string()), true);
        sm.add_selectables("my-cmp", vec![cmp_meta.clone()]);
        let binder = R3TargetBinder::new(Some(DirectiveMatcher::Selector(sm)));

        let bound = binder.bind(Target {
            template: Some(&nodes),
            host: None,
        });

        // The component must be among the used + eager directives.
        let used = bound.get_used_directives();
        assert!(used.iter().any(|d| d.name == "MyCmp"));
        assert!(bound
            .get_eagerly_used_directives()
            .iter()
            .any(|d| d.name == "MyCmp"));

        // Resolve the inner <my-cmp> node + its empty #ref -> the component directive.
        let Node::Template(tpl) = &nodes[0] else {
            unreachable!()
        };
        let Node::Element(el) = &tpl.children[0] else {
            unreachable!()
        };
        let directives_on_el =
            bound.get_directives_of_node(DirectiveOwner::Element(addr(el)));
        assert!(directives_on_el.is_some());
        assert_eq!(directives_on_el.unwrap()[0].name, "MyCmp");

        // The empty `#ref` should resolve to the component directive on the element.
        let ref_target = bound.get_reference_target(&el.references[0]).unwrap();
        match ref_target {
            ReferenceTarget::Directive { directive, node } => {
                assert_eq!(directive.name, "MyCmp");
                assert_eq!(*node, DirectiveOwner::Element(addr(el)));
            }
            other => panic!("expected directive reference target, got {other:?}"),
        }

        // `{{ item }}` must resolve to the template variable `item`.
        let Node::BoundText(bt) = &el.children[0] else {
            unreachable!()
        };
        let expr_target = bound.get_expression_target(&bt.value).unwrap();
        assert_eq!(expr_target.name(), "item");
        assert!(matches!(expr_target, TemplateEntity::Variable { .. }));

        // The variable's defining scoped node is the template.
        let def = bound.get_definition_node_of_symbol(expr_target).unwrap();
        assert_eq!(def, ScopedNode::Template(addr(tpl)));

        // Nesting level of the inner template is 1.
        assert_eq!(bound.get_nesting_level(ScopedNode::Template(addr(tpl))), 1);

        // The variable is visible in the template's scope.
        let in_scope = bound.get_entities_in_scope(Some(ScopedNode::Template(addr(tpl))));
        assert!(in_scope.iter().any(|e| e.name() == "item"));
    }

    /// The SELECTORLESS keystone: a `<MyComponent>` node matches an imported symbol by class name;
    /// an unmatched `<Unknown>` lands in `missing_directives` and drives auto-import.
    #[test]
    fn selectorless_matches_by_class_name_and_tracks_missing() {
        // <MyComponent #r><Unknown></Unknown></MyComponent>
        let unknown = Component {
            component_name: "Unknown".to_string(),
            tag_name: None,
            full_name: "Unknown".to_string(),
            attributes: vec![],
            inputs: vec![],
            outputs: vec![],
            directives: vec![],
            children: vec![],
            references: vec![],
            is_self_closing: true,
            source_span: span(),
            start_source_span: span(),
            end_source_span: None,
            i18n: None,
        };
        let my_component = Component {
            component_name: "MyComponent".to_string(),
            tag_name: None,
            full_name: "MyComponent".to_string(),
            attributes: vec![],
            inputs: vec![],
            outputs: vec![],
            directives: vec![],
            children: vec![Node::Component(unknown)],
            references: vec![reference("r")],
            is_self_closing: false,
            source_span: span(),
            start_source_span: span(),
            end_source_span: None,
            i18n: None,
        };
        let nodes = vec![Node::Component(my_component)];

        let mut matcher = SelectorlessMatcher::<DirectiveMeta>::new();
        matcher.add(
            "MyComponent",
            DirectiveMeta::new("MyComponent", None, true),
        );
        let binder = R3TargetBinder::new(Some(DirectiveMatcher::Selectorless(matcher)));

        let bound = binder.bind(Target {
            template: Some(&nodes),
            host: None,
        });

        // MyComponent matched -> exists; Unknown didn't -> missing (auto-import candidate).
        assert!(bound.referenced_directive_exists("MyComponent"));
        assert!(!bound.referenced_directive_exists("Unknown"));
        assert!(bound.missing_directives().contains("Unknown"));

        // The matched component is registered on its Component node.
        let Node::Component(cmp) = &nodes[0] else {
            unreachable!()
        };
        let dirs = bound
            .get_directives_of_node(DirectiveOwner::Component(addr(cmp)))
            .expect("MyComponent should be matched");
        assert_eq!(dirs[0].name, "MyComponent");

        // The selectorless reference `#r` points at the first matched directive on the node.
        let target = bound.get_reference_target(&cmp.references[0]).unwrap();
        match target {
            ReferenceTarget::Directive { directive, node } => {
                assert_eq!(directive.name, "MyComponent");
                assert_eq!(*node, DirectiveOwner::Component(addr(cmp)));
            }
            other => panic!("expected directive ref target, got {other:?}"),
        }
    }

    #[test]
    fn input_binding_ownership_selector_mode() {
        // <my-cmp [foo]="bar"></my-cmp>, MyCmp declares input `foo`.
        let mut my_cmp = empty_element("my-cmp");
        my_cmp.inputs.push(BoundAttribute {
            name: "foo".to_string(),
            kind: crate::expression::ast::BindingType::Property,
            security_context: crate::expression::ast::SecurityContext::None,
            value: implicit_read("bar"),
            unit: None,
            source_span: span(),
            key_span: span(),
            value_span: None,
            i18n: None,
        });
        let nodes = vec![Node::Element(my_cmp)];

        let mut sm = SelectorMatcher::new();
        let mut meta = DirectiveMeta::new("MyCmp", Some("my-cmp".to_string()), true);
        meta.inputs = ClassPropertyMapping::from_pairs([("foo", "foo")]);
        sm.add_selectables("my-cmp", vec![meta]);
        let binder = R3TargetBinder::new(Some(DirectiveMatcher::Selector(sm)));
        let bound = binder.bind(Target {
            template: Some(&nodes),
            host: None,
        });

        let Node::Element(el) = &nodes[0] else {
            unreachable!()
        };
        let consumer = bound
            .get_consumer_of_bound_attribute(&el.inputs[0])
            .expect("input should have a consumer");
        match consumer {
            BindingConsumer::Directive(d) => assert_eq!(d.name, "MyCmp"),
            other => panic!("expected directive consumer, got {other:?}"),
        }
    }

    #[test]
    fn unmatched_input_is_owned_by_the_element() {
        // <div [foo]="bar"></div> with no matching directive: the element owns the binding.
        let mut div = empty_element("div");
        div.inputs.push(BoundAttribute {
            name: "foo".to_string(),
            kind: crate::expression::ast::BindingType::Property,
            security_context: crate::expression::ast::SecurityContext::None,
            value: implicit_read("bar"),
            unit: None,
            source_span: span(),
            key_span: span(),
            value_span: None,
            i18n: None,
        });
        let nodes = vec![Node::Element(div)];

        let sm = SelectorMatcher::new();
        let binder = R3TargetBinder::new(Some(DirectiveMatcher::Selector(sm)));
        let bound = binder.bind(Target {
            template: Some(&nodes),
            host: None,
        });

        let Node::Element(el) = &nodes[0] else {
            unreachable!()
        };
        let consumer = bound.get_consumer_of_bound_attribute(&el.inputs[0]).unwrap();
        assert_eq!(*consumer, BindingConsumer::Element(addr(el)));
    }

    #[test]
    fn local_ref_on_plain_element_resolves_to_element() {
        // <div #d></div>, selector mode, no directive: empty ref -> the element node itself.
        let mut div = empty_element("div");
        div.references.push(reference("d"));
        let nodes = vec![Node::Element(div)];

        let sm = SelectorMatcher::new();
        let binder = R3TargetBinder::new(Some(DirectiveMatcher::Selector(sm)));
        let bound = binder.bind(Target {
            template: Some(&nodes),
            host: None,
        });

        let Node::Element(el) = &nodes[0] else {
            unreachable!()
        };
        let target = bound.get_reference_target(&el.references[0]).unwrap();
        assert_eq!(*target, ReferenceTarget::Element(addr(el)));
    }

    #[test]
    fn pipe_eager_vs_deferred_tracking() {
        // {{ x | eagerPipe }} at top level; pipe is eager and used.
        let pipe = AstNode::new(
            ParseSpan::new(0, 0),
            AbsoluteSourceSpan::new(0, 0),
            ExprKind::BindingPipe {
                name_span: AbsoluteSourceSpan::new(0, 0),
                exp: Box::new(implicit_read("x")),
                name: "eagerPipe".to_string(),
                args: vec![],
                pipe_type: crate::expression::ast::BindingPipeType::ReferencedByName,
            },
        );
        let nodes = vec![Node::BoundText(BoundText {
            value: pipe,
            source_span: span(),
            i18n: None,
        })];

        let binder = R3TargetBinder::new(None);
        let bound = binder.bind(Target {
            template: Some(&nodes),
            host: None,
        });
        assert!(bound.get_used_pipes().contains(&"eagerPipe".to_string()));
        assert!(bound
            .get_eagerly_used_pipes()
            .contains(&"eagerPipe".to_string()));
    }

    #[test]
    #[should_panic(expected = "Empty bound targets")]
    fn empty_target_panics() {
        let binder = R3TargetBinder::new(None);
        let _ = binder.bind(Target {
            template: None,
            host: None,
        });
    }

    #[test]
    fn top_level_ref_has_no_defining_template() {
        // A top-level `#x` has rootNode == null, so no symbol entry.
        let mut div = empty_element("div");
        div.references.push(reference("x"));
        let nodes = vec![Node::Element(div)];
        let binder = R3TargetBinder::new(None);
        let bound = binder.bind(Target {
            template: Some(&nodes),
            host: None,
        });
        let Node::Element(el) = &nodes[0] else {
            unreachable!()
        };
        let entity = TemplateEntity::Reference {
            id: addr(&el.references[0]),
            name: "x".to_string(),
        };
        assert!(bound.get_definition_node_of_symbol(&entity).is_none());
        // But it IS visible in the root scope.
        let root = bound.get_entities_in_scope(None);
        assert!(root.iter().any(|e| e.name() == "x"));
    }
}
