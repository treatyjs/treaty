# Port Spec 10 — `t2_binder` (The Binder)

Source:
- `packages/compiler/src/render3/view/t2_binder.ts`
- `packages/compiler/src/render3/view/t2_api.ts`

Angular version: **22.1.0-next.0**

Supporting siblings consulted (for type clarity):
- `packages/compiler/src/directive_matching.ts` (`CssSelector`, `SelectorMatcher`, `SelectorlessMatcher`)
- `packages/compiler/src/render3/view/util.ts` (`createCssSelectorFromNode`)
- `packages/compiler/src/combined_visitor.ts` (`CombinedRecursiveAstVisitor` base class)

---

## 1. Purpose & role in the compilation pipeline

The **binder** is the semantic-resolution stage of the render3 template compiler. After the
template parser (`template.ts` / `r3_ast.ts`) produces an untyped tree of template nodes
(`Element`, `Template`, `Component`, `Directive`, control-flow blocks, etc.), the binder answers
the question: **"what does each piece of the template actually refer to?"**

Concretely it computes, for one `Target` (a template node array and/or a host element):

- **Element → directive/component matching.** Which directives/components match each element or
  `<ng-template>` (selector-based) or each `Component`/`Directive` node (selectorless).
- **Binding ownership.** Which directive (or the bare element) *consumes* each input, output, and
  text attribute (`getConsumerOfBinding`).
- **Reference resolution.** What each `#ref` points to — an element, a template, or a specific
  directive on a node (`getReferenceTarget`).
- **Variable / let / expression resolution.** Which `PropertyRead`/`SafePropertyRead` expression
  roots bind to a template-local `Variable`, `Reference`, or `@let` declaration vs. the component
  context (`getExpressionTarget`).
- **Scoping.** The lexical scope tree of the template, nesting levels, and what entities are
  visible at each scoped node (`getEntitiesInScope`, `getNestingLevel`, `getDefinitionNodeOfSymbol`).
- **Defer metadata.** Eager vs. deferred directive/pipe usage, defer-block trigger targets, and
  whether a node is inside a `@defer` block.

It is the analogue of `ts.TypeChecker` for templates (the file comment says so explicitly). Its
output, `BoundTarget`, is consumed downstream by:
- The **template type-checker** (TCB generation) — needs to know which directive owns a binding and
  what a reference resolves to in order to emit type-check code.
- The **`TemplateDefinitionBuilder` / template pipeline** — needs scope, nesting level, and
  consumer-of-binding info to emit `ɵɵ` instructions.
- **Selectorless + auto-import** — `missingDirectives` and `referencedDirectiveExists` feed the
  compiler's ability to auto-import directly-referenced component/directive classes.

This module **does not emit any `ɵɵ` instructions itself** (see §6). It is pure analysis. It is the
*foundation* on which selectorless and auto-import are built.

---

## 2. Public API (exported symbols with real signatures)

### From `t2_binder.ts`

```ts
// Eager/deferrable directive & pipe discovery for a raw template string.
export function findMatchingDirectivesAndPipes(
  template: string,
  directiveSelectors: string[],
): {
  directives: {regular: string[]; deferCandidates: string[]};
  pipes: {regular: string[]; deferCandidates: string[]};
};

// Union of the two matcher kinds the binder accepts.
export type DirectiveMatcher<DirectiveT extends DirectiveMeta> =
  | SelectorMatcher<DirectiveT[]>
  | SelectorlessMatcher<DirectiveT>;

// The main entry-point class.
export class R3TargetBinder<DirectiveT extends DirectiveMeta>
  implements TargetBinder<DirectiveT>
{
  constructor(
    private directiveMatcher: DirectiveMatcher<DirectiveT> | null,
    private foreignComponentMatcher: SelectorlessMatcher<ForeignComponentMeta> | null = null,
  );

  bind(target: Target<DirectiveT>): BoundTarget<DirectiveT>;
}
```

Non-exported but central implementation classes (internal to the module): `Scope`,
`DirectiveBinder`, `TemplateBinder`, `R3BoundTarget`, and the helper `extractScopedNodeEntities`.

### From `t2_api.ts` (all exported)

```ts
export type ScopedNode =
  | Template | SwitchBlockCaseGroup | IfBlockBranch | ForLoopBlock | ForLoopBlockEmpty
  | DeferredBlock | DeferredBlockError | DeferredBlockLoading | DeferredBlockPlaceholder
  | Content | HostElement;

export type ReferenceTarget<DirectiveT> =
  | {directive: DirectiveT; node: Exclude<DirectiveOwner, HostElement>}
  | Element
  | Template;

export type TemplateEntity = Reference | Variable | LetDeclaration;

export type DirectiveOwner = Element | Template | Component | Directive | HostElement;

export interface ConflictingHostDirectiveBinding<DirectiveT> {
  directive: DirectiveT;
  classPropertyName: string;
  conflictingAliases: Set<string>;
  kind: 'input' | 'output';
}

export interface Target<DirectiveT> {
  template?: Node[];
  host?: {node: HostElement; directives: DirectiveT[]};
}

export interface LegacyAnimationTriggerNames {
  includesDynamicAnimations: boolean;
  staticTriggerNames: string[];
}

export interface DirectiveMeta {
  name: string;
  ref: {key: string};
  selector: string | null;
  isComponent: boolean;
  inputs: ClassPropertyMapping;
  outputs: ClassPropertyMapping;
  exportAs: string[] | null;
  isStructural: boolean;
  ngContentSelectors: string[] | null;
  preserveWhitespaces: boolean;
  animationTriggerNames: LegacyAnimationTriggerNames | null;
  matchSource: MatchSource;
}

export interface ForeignComponentMeta {
  name: string;
  ref: {key: string};
}

export enum MatchSource {
  Selector,       // matched by selector
  HostDirective,  // applied as a host directive
}

export interface TargetBinder<D extends DirectiveMeta> {
  bind(target: Target<D>): BoundTarget<D>;
}

export interface BoundTarget<DirectiveT extends DirectiveMeta> {
  readonly target: Target<DirectiveT>;
  getDirectivesOfNode(node: DirectiveOwner): DirectiveT[] | null;
  getForeignComponent(element: Element): ForeignComponentMeta | null;
  getReferenceTarget(ref: Reference): ReferenceTarget<DirectiveT> | null;
  getConsumerOfBinding(
    binding: BoundAttribute | BoundEvent | TextAttribute,
  ): DirectiveT | Element | Template | null;
  getExpressionTarget(expr: AST): TemplateEntity | null;
  getDefinitionNodeOfSymbol(symbol: TemplateEntity): ScopedNode | null;
  getNestingLevel(node: ScopedNode): number;
  getEntitiesInScope(node: ScopedNode | null): ReadonlySet<TemplateEntity>;
  getUsedDirectives(): DirectiveT[];
  getEagerlyUsedDirectives(): DirectiveT[];
  getUsedPipes(): string[];
  getEagerlyUsedPipes(): string[];
  getDeferBlocks(): DeferredBlock[];
  getDeferredTriggerTarget(block: DeferredBlock, trigger: DeferredTrigger): Element | null;
  isDeferred(node: Element): boolean;
  referencedDirectiveExists(name: string): boolean;
  getConflictingHostDirectiveBindings(
    node: DirectiveOwner,
  ): ConflictingHostDirectiveBinding<DirectiveT>[] | null;
}
```

---

## 3. Key data structures + proposed Rust mapping

All AST-bound maps key on identity of parsed template nodes. In Rust under OXC there is no GC
identity; the recommended approach is to assign **stable indices/ids** to template nodes during
parsing (an arena of nodes) and key maps on those ids. AST nodes that come from `oxc_ast` (the
expression AST inside bindings) carry the arena lifetime `'a`; template-structure nodes
(`r3_ast`) will be our own arena type (call it `'t`).

### Internal type aliases (t2_binder.ts)

```ts
type BindingsMap<DirectiveT> = Map<BoundAttribute | BoundEvent | TextAttribute,
                                   DirectiveT | Template | Element>;
type ReferenceMap<DirectiveT> = Map<Reference,
  Template | Element | {directive: DirectiveT; node: Exclude<DirectiveOwner, HostElement>}>;
type MatchedDirectives<DirectiveT> = Map<DirectiveOwner, DirectiveT[]>;
type ScopedNodeEntities = Map<ScopedNode | null, Set<TemplateEntity>>;
type DeferBlockScopes = [DeferredBlock, Scope][];
```

Rust:

```rust
// NodeId / RefId / ExprId are arena indices assigned at parse time.
type BindingsMap<'t>  = FxHashMap<BindingNodeId, BindingConsumer<'t>>;
type ReferenceMap<'t> = FxHashMap<RefId, ReferenceTarget<'t>>;
type MatchedDirectives<'t> = FxHashMap<DirectiveOwnerId, Vec<DirectiveMetaId>>;
type ScopedNodeEntities = FxHashMap<Option<ScopedNodeId>, FxHashSet<TemplateEntity>>;
type DeferBlockScopes = Vec<(DeferredBlockId, ScopeId)>;

enum BindingConsumer<'t> { Directive(DirectiveMetaId), Element(NodeId), Template(NodeId) }
enum TemplateEntity { Reference(RefId), Variable(VarId), Let(LetId) } // = ScopedNode-local symbol
```

### `ScopedNode`, `ReferenceTarget`, `TemplateEntity`, `DirectiveOwner` (t2_api.ts)

```rust
// Closed unions over r3_ast node kinds -> Rust enums of node ids.
enum ScopedNode {
    Template(NodeId), SwitchBlockCaseGroup(NodeId), IfBlockBranch(NodeId),
    ForLoopBlock(NodeId), ForLoopBlockEmpty(NodeId),
    DeferredBlock(NodeId), DeferredBlockError(NodeId),
    DeferredBlockLoading(NodeId), DeferredBlockPlaceholder(NodeId),
    Content(NodeId), HostElement(NodeId),
}

enum DirectiveOwner { Element(NodeId), Template(NodeId), Component(NodeId),
                      Directive(NodeId), HostElement(NodeId) }

enum ReferenceTarget<'t> {
    Element(NodeId),
    Template(NodeId),
    Directive { directive: DirectiveMetaId, node: DirectiveOwnerNonHost },
}
```

### `DirectiveMeta` / `ForeignComponentMeta` / `MatchSource`

`DirectiveMeta` is *supplied by the caller* (it is the bridge from the TS type system into the
binder). For the Rust port it is the metadata struct produced by the metadata stage. Key fields the
binder actually reads: `selector`, `exportAs`, `inputs`/`outputs` (only `hasBindingPropertyName`
membership is used during binding), `isComponent`, `matchSource`, `ref.key` (dedup key).

```rust
enum MatchSource { Selector, HostDirective }

struct DirectiveMeta {
    name: String,
    ref_key: String,                 // ref.key — dedup/identity
    selector: Option<String>,
    is_component: bool,
    inputs: ClassPropertyMapping,
    outputs: ClassPropertyMapping,
    export_as: Option<Vec<String>>,
    is_structural: bool,
    ng_content_selectors: Option<Vec<String>>,
    preserve_whitespaces: bool,
    animation_trigger_names: Option<LegacyAnimationTriggerNames>,
    match_source: MatchSource,
}
struct ForeignComponentMeta { name: String, ref_key: String }
```

Because `DirectiveMeta` is generic (`DirectiveT extends DirectiveMeta`) the TS code lets callers
extend it; in Rust prefer a trait `DirectiveMetaLike` or simply store an opaque id + the concrete
struct in a side table, since the binder only needs the fields above. Genericity is **not** worth
reproducing literally — replace with a concrete `DirectiveMeta` plus an id used to look the caller's
richer metadata back up.

### `ConflictingHostDirectiveBinding`

```rust
struct ConflictingHostDirectiveBinding {
    directive: DirectiveMetaId,
    class_property_name: String,
    conflicting_aliases: FxHashSet<String>,
    kind: BindingKind, // Input | Output
}
```

### `Scope` (internal, the lexical scope tree)

Fields: `namedEntities: Map<string, TemplateEntity>`, `elementLikeInScope: Set<Element|Component>`,
`childScopes: Map<ScopedNode, Scope>`, `isDeferred: boolean`, `parentScope: Scope | null`,
`rootNode: ScopedNode | null`.

```rust
struct Scope {
    named_entities: FxHashMap<String, TemplateEntity>,
    element_like_in_scope: FxHashSet<NodeId>,   // Element | Component nodes
    child_scopes: FxHashMap<ScopedNodeId, ScopeId>,
    is_deferred: bool,
    parent_scope: Option<ScopeId>,
    root_node: Option<ScopedNodeId>,            // None => root scope
}
// Store scopes in a Vec<Scope> arena; ScopeId = index. Avoids parent back-pointer aliasing pain.
```

### `R3BoundTarget` (the result object)

Holds all the maps above plus `deferredBlocks: DeferredBlock[]` and
`deferredScopes: Map<DeferredBlock, Scope>` derived from the raw `DeferBlockScopes`. Maps directly
to a Rust struct owning the `FxHashMap`s; methods become inherent methods returning `Option<&...>`.

---

## 4. Algorithm walkthrough

### 4.1 `R3TargetBinder.bind(target)` — orchestration

1. Reject empty targets: if neither `template` nor `host` is set, throw.
2. Allocate all the result containers (`directives`, `foreignComponents`, `eagerDirectives`,
   `missingDirectives`, `bindings`, `references`, `scopedNodeEntities`, `expressions`, `symbols`,
   `nestingLevel`, `usedPipes`, `eagerPipes`, `deferBlocks`, `conflictingHostDirectiveBindings`).
3. If `target.template`:
   a. **`Scope.apply(template)`** — build the lexical scope tree (§4.2).
   b. **`extractScopedNodeEntities(scope, scopedNodeEntities)`** — flatten the scope tree into,
      for each scoped node, the *full* set of entities visible (own + inherited) (§4.5).
   c. **`DirectiveBinder.apply(...)`** — directive matching, binding ownership, reference
      resolution, foreign components, host-directive merge/conflict detection (§4.3).
   d. **`TemplateBinder.applyWithScope(...)`** — expression→entity resolution, symbol→defining-node,
      nesting levels, used/eager pipes, defer-block scopes (§4.4).
4. If `target.host`: register the host directives directly into `directives`, then run **only**
   `TemplateBinder.applyWithScope` over the host element in its own scope. *Directive matching is
   intentionally skipped for host context* — directives don't apply to themselves there.
5. Construct and return `R3BoundTarget` wrapping all the maps.

### 4.2 `Scope.apply` / `Scope.ingest` — scope construction (Visitor)

`Scope` implements the `r3_ast` `Visitor`. A root scope (`parentScope = null, rootNode = null`) is
created and `ingest`ed.

- `ingest(nodeOrNodes)` dispatches by container kind. For a `Template`, variables are declared in
  the *inner* scope; for `IfBlockBranch` the `expressionAlias` is a variable; for `ForLoopBlock`
  the `item` plus `contextVariables` are variables; switch-case-groups / empties / defer
  sub-blocks / content just recurse over children. A plain `Node[]` is the top level. `HostElement`
  is explicitly ignored here.
- `visitElement` / `visitComponent` → `visitElementLike`: visit child `directives` (to capture
  their references), declare `references`, recurse into children, and add the node to
  `elementLikeInScope`.
- `visitTemplate`: visit the template's `directives`; declare `references` **in the outer scope**
  (references on `<ng-template>` are visible outside it); then `ingestScopedNode(template)` creates
  the inner child scope.
- `visitDirective`: declares the directive's `references` (selectorless reference capture).
- `visitVariable` / `visitReference` / `visitLetDeclaration` → `maybeDeclare`: register by name,
  **first declaration wins** (`if (!namedEntities.has(name))`).
- Control-flow visitors (`visitDeferredBlock`, `visitForLoopBlock`, `visitIfBlock`, `visitSwitchBlock`,
  etc.) call `ingestScopedNode` to create child scopes; defer blocks also descend into their
  placeholder/loading/error sub-blocks; switch/if descend into their groups/branches.
- `isDeferred` is inherited: true if any ancestor scope is deferred or if `rootNode` is a
  `DeferredBlock`.
- `lookup(name)` walks up `parentScope` chain. `getChildScope(node)` asserts presence.

### 4.3 `DirectiveBinder.apply` — directive matching (Visitor)

Tracks `isInDeferBlock` (toggled in `visitDeferredBlock` children only, not in
placeholder/loading/error). Two matching modes depending on the matcher kind:

**Selector-based (`SelectorMatcher`)** — `visitElement`/`visitTemplate` → `visitElementOrTemplate`:
1. `createCssSelectorFromNode(node)` (util.ts) builds a `CssSelector` from element name +
   attributes + property/two-way inputs + outputs + (for `Template`) `templateAttrs`.
2. `directiveMatcher.match(cssSelector, cb)` collects all matched `DirectiveT`.
3. `trackSelectorBasedBindingsAndDirectives`:
   - `trackMatchedDirectives` → dedup/merge host directives (§4.3.1), store in `directives`, and if
     not in a defer block, push to `eagerDirectives`.
   - **Reference resolution:** for each `ref` on the node — empty value (`#x` / `#x=""`) targets the
     *primary component* directive (`dir.isComponent`) if present, else the node itself; a non-empty
     value (`#x="exportName"`) targets the directive whose `exportAs` array contains the value; if
     none match, the reference is **left unmapped**.
   - **Binding ownership:** for each input / attribute / (template) templateAttr the consumer is the
     first directive whose `inputs.hasBindingPropertyName(name)` is true, else the node; same for
     outputs against `outputs.hasBindingPropertyName`.

**Selectorless (`SelectorlessMatcher`)** — `visitComponent` / `visitDirective`:
1. `visitComponent`: `directiveMatcher.match(node.componentName)`. If matches, track; **if none,
   `missingDirectives.add(node.componentName)`** — this is the selectorless/auto-import signal.
2. `visitDirective`: `directiveMatcher.match(node.name)`; same missing-tracking on `node.name`.
3. `trackSelectorlessMatchesAndDirectives`: `trackMatchedDirectives`, then for each matched
   directive set binding ownership for inputs/attributes/outputs that the directive claims by
   `hasBindingPropertyName`. **References** under selectorless always point to the *first* matched
   directive (TODO in source: multi-host-directive reference semantics undecided).

In selector mode, when *not* using `SelectorMatcher` (i.e. selectorless), bare empty references on
elements/templates are mapped to the node itself in `visitElementOrTemplate`'s `else` branch.

**Foreign components** (`foreignMatcher: SelectorlessMatcher<ForeignComponentMeta>`): in
`visitElementOrTemplate`, for `Element` nodes, `foreignMatcher.match(node.name)`. If a foreign match
*and* an Angular directive both matched the element → **throw conflict error**. Otherwise record the
first foreign match in `foreignComponents`.

#### 4.3.1 Host-directive dedup & merge (`dedupeAndMergeDirectives`)

When matches include `MatchSource.HostDirective` entries (not all `Selector`):
- Partition by `ref.key` into selector matches vs. host directives.
- Drop host directives whose key also matched via selector.
- For a key with a single host directive, keep it. For multiple, **merge** their input/output
  `ClassPropertyMapping`s via `mergeMapping`: identical bindings (same binding name, class name,
  `isSignal`) coalesce; differing aliases for the same `classPropertyName` are recorded into
  `conflictingHostDirectiveBindings` as a `ConflictingHostDirectiveBinding`.
- Reassemble preserving original order; selector matches pass through, merged host directives are
  emitted once.

### 4.4 `TemplateBinder.applyWithScope` — expression/symbol binding

Extends `CombinedRecursiveAstVisitor` (which itself extends the expression `RecursiveAstVisitor` and
implements the template `RecursiveVisitor`), so it walks **both** template nodes and the embedded
expression ASTs.

- Constructed with the scope, the current `rootNode` (`Template` or null), and a `level` (top =
  `0`). `ingest` mirrors `Scope.ingest`'s container dispatch and records `nestingLevel` for each
  scoped node. For `DeferredBlock` it asserts `scope.rootNode === block` and pushes
  `[block, scope]` to `deferBlocks`.
- `visitTemplate`: visits inputs/outputs/directives/templateAttrs/references (in the *outer* scope),
  then `ingestScopedNode` recurses with `childScope`, `node` as new rootNode, `level + 1`.
- `visitVariable` / `visitReference` / `visitLetDeclaration`: register the entity → current
  `rootNode` in `symbols` (only when `rootNode !== null`).
- `visitPipe(ast)`: `usedPipes.add(ast.name)`; if the scope is **not** deferred,
  `eagerPipes.add(ast.name)`. This is how eager vs. deferrable pipe split is computed.
- `visitPropertyRead` / `visitSafePropertyRead` → `maybeMap(ast, name)`: **the core expression
  resolution.** Only if `ast.receiver instanceof ImplicitReceiver` (i.e. an unqualified read like
  `{{ foo }}`), `scope.lookup(name)`; if found, record `expressions[ast] = entity`. Otherwise the
  name is assumed to be a property on the component context and left unmapped.
- `ingestScopedNode` recurses into the matching child scope with incremented level.

### 4.5 `extractScopedNodeEntities`

Iteratively (stack) walks the scope tree, memoizing per `rootNode` the **merged** map of
entities = parent's merged entities ∪ this scope's `namedEntities`. Then copies each merged map's
values into a `Set` keyed by the scope's `rootNode` (or `null` for root) in `scopedNodeEntities`.
This is what `getEntitiesInScope` returns — the full lexically-visible entity set at each scope.

### 4.6 `R3BoundTarget` query methods (notable logic)

- `getDeferredTriggerTarget(block, trigger)`: only `Interaction`/`Viewport`/`Hover` triggers
  resolve. If the trigger has no `reference`, infer the *single root element* of the block's
  placeholder. Otherwise look the named entity up in the block's scope (skipping symbols defined
  inside the block itself), resolve the `Reference` to its target and coerce to an `Element`
  (`referenceTargetToElement`, which recurses through `{directive, node}` targets and bails on
  Template/Component/Directive/HostElement). Falls back to the placeholder scope.
- `isDeferred(element)`: DFS over each deferred block's scope subtree checking
  `elementLikeInScope`.
- `referencedDirectiveExists(name)`: `!missingDirectives.has(name)` — the selectorless API.

### 4.7 `findMatchingDirectivesAndPipes(template, directiveSelectors)`

Builds a `SelectorMatcher` of *fake* `DirectiveMeta`s (inputs/outputs `hasBindingPropertyName`
always false, `matchSource: Selector`) from the given selectors, `parseTemplate`s the string, binds
it, then returns eager vs. defer-candidate selector/pipe lists via set-difference (`diff`). Used to
discover deferrable dependencies for standalone components.

---

## 5. Dependencies on other compiler modules

- `../../directive_matching` — `CssSelector`, `SelectorMatcher`, `SelectorlessMatcher` (the actual
  matching engines). **Must port first** (spec for this module separately).
- `../../expression_parser/ast` — `AST`, `BindingPipe`, `ImplicitReceiver`, `PropertyRead`,
  `SafePropertyRead`. This is the **oxc-bound expression AST** (`'a` lifetime in the port).
- `../r3_ast` — the entire template node hierarchy + `Visitor`/`RecursiveVisitor`.
- `../../combined_visitor` — `CombinedRecursiveAstVisitor` base for `TemplateBinder`.
- `./t2_api` — the public interfaces/types (same module pair).
- `./template` — `parseTemplate` (only for `findMatchingDirectivesAndPipes`).
- `./util` — `createCssSelectorFromNode`.
- `../../property_mapping` — `ClassPropertyMapping`, `ClassPropertyName`, `InputOrOutput` (host
  directive merge).

---

## 6. `ɵɵ` instructions / output emitted

**None.** This module emits **no** runtime instructions and produces **no** `output_ast` /
`oxc_codegen` output. It is a pure analysis pass that produces an in-memory `BoundTarget`. (Confirmed:
`util.ts` imports `output/output_ast` but the binder itself does not, and no `ɵɵ` symbols, no
`o.*` emission, no `DefinitionMap` usage occur in `t2_binder.ts`.) Downstream modules consume its
results to emit instructions.

`emittedInstructions: []`.

---

## 7. Edge cases, gotchas, version-sensitivity

- **Selectorless is the matcher-kind switch.** The single `directiveMatcher` field is typed as a
  union (`SelectorMatcher | SelectorlessMatcher`), and the whole `DirectiveBinder` branches on
  `instanceof`. Selector mode visits `Element`/`Template`; selectorless mode visits
  `Component`/`Directive` nodes (a distinct r3_ast node kind that only exists when the template was
  parsed in selectorless mode). Porting must replicate this dual dispatch faithfully — the Rust
  matcher should be an `enum DirectiveMatcher { Selector(...), Selectorless(...) }`.
- **`missingDirectives` drives auto-import.** Only populated in selectorless mode, keyed by the raw
  `componentName` / directive `name`. `referencedDirectiveExists` is the lookup. Easy to miss
  because it's a `Set<string>`, not tied to nodes.
- **Reference resolution precedence:** empty ref → primary component, non-empty → `exportAs` match,
  no match → unmapped (selector mode) but → node-itself in selectorless's else-branch on plain
  elements. Subtle and must be preserved.
- **First-declaration-wins for named entities** (`maybeDeclare`). Shadowing is *not* allowed within
  a scope; outer scopes are only consulted on `lookup` miss.
- **`<ng-template>` reference/variable asymmetry:** references are declared in the **outer** scope,
  variables in the **inner** scope. Replicating this split is the most error-prone scoping detail.
- **Host context skips directive matching** entirely and runs only `TemplateBinder`.
- **Host-directive merge & conflict reporting** (`dedupeAndMergeDirectives` / `mergeMapping`) is
  intricate and depends on `ClassPropertyMapping` semantics; it is keyed on `ref.key` string
  identity.
- **Foreign components** are a relatively recent addition; `foreignComponentMatcher` defaults to
  `null`. The element-name-only matching and the directive-vs-foreign conflict throw are version
  sensitive.
- **`getDeferredTriggerTarget`** has placeholder single-root-element inference and skips Comment
  nodes "just in case" comments are captured — depends on parser config.
- **API churn:** `t2_api.ts` carries the explicit "t2 is the replacement for `TemplateDefinitionBuilder`"
  comment and several TODOs (`alxhub` on multi-claim bindings, `crisbeto` on selectorless reference
  semantics). `LegacyAnimationTriggerNames` / `animationTriggerNames` reflect the legacy-animations
  migration. `Component`/`Directive`/`HostElement` as r3_ast node kinds and the `DirectiveOwner`
  union are 17+→22 additions. Pin behavior to 22.1.0-next.0.
- **Identity-keyed maps** everywhere — the single biggest porting hazard. In TS these rely on object
  identity of parsed nodes; the Rust port must assign stable ids at parse time and key on those.
- **Generic `DirectiveT`** is pervasive; do not reproduce the generic — use a concrete metadata
  struct + id (see §3).

---

## 8. Port plan (Rust / OXC)

**Reuse from OXC:** the *expression* AST nodes (`PropertyRead`, `SafePropertyRead`, `BindingPipe`,
`ImplicitReceiver`) come from the ported expression parser built on `oxc_ast` (`'a` arena). The
binder only reads them, so no `AstBuilder` needed here. `oxc_codegen` is irrelevant (no emission).

**New Rust scaffolding required:**
1. **Node-id arena for `r3_ast`.** Every template node gets a `NodeId`; matchers/maps key on ids.
   Build this when porting `r3_ast` (a prerequisite). Use `oxc_index`/`Idx`-style newtype indices.
2. **`directive_matching` port** (`CssSelector`, `SelectorMatcher`, `SelectorlessMatcher`) — depends
   only on string maps; straightforward. Port *before* the binder.
3. **`ClassPropertyMapping`** (`property_mapping`) — needed by host-directive merge; port before.
4. **Scope tree** as a `Vec<Scope>` arena with `ScopeId` parent/child links (avoid `Rc`/back-ptrs).
5. **Three visitor passes** as structs implementing the ported template `Visitor` trait:
   `ScopeBuilder`, `DirectiveBinder`, `TemplateBinder`. `TemplateBinder` needs the combined
   template+expression visitor base (port `CombinedRecursiveAstVisitor`).
6. **`R3BoundTarget`** owning `FxHashMap`s keyed by ids; query methods return `Option<&...>` / `Vec`.

**Recommended structure:** replace the `BindingConsumer` / `ReferenceTarget` / `TemplateEntity`
unions with Rust `enum`s; replace `Map<Node, X>` with `FxHashMap<NodeId, X>`; use
`enum DirectiveMatcher` for the selector/selectorless split rather than trait-object dispatch.

**Estimated complexity: HIGH.** Rationale: three interlocking visitor passes, identity-map
translation to id-maps throughout, the `<ng-template>` inner/outer scope asymmetry, host-directive
merge/conflict logic, selectorless dual-dispatch, and defer-trigger resolution. The algorithms are
not numerically hard but are *fiddly* and have many edge cases that the test suite will exercise.

**Ordering vs. other modules:**
1. `r3_ast` (+ node-id arena) and the template `Visitor` trait — prerequisite.
2. expression AST / parser (already required broadly) — prerequisite for `TemplateBinder`.
3. `directive_matching`, `property_mapping`, `combined_visitor` — direct prerequisites.
4. **then this module (`t2_binder`).**
5. Downstream: template type-check block generation and `TemplateDefinitionBuilder` (the
   instruction emitters) consume `BoundTarget` and come *after*.

This module is foundational for selectorless + auto-import, so it should be among the **first
analysis modules** ported once `r3_ast`, the expression AST, and the matcher are in place.
