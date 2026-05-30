# Port Spec 11 — `render3/view/compiler.ts` + `render3/view/api.ts`

Angular **22.1.0-next.0** · render3 / direct-to-Ivy compiler

Source files:
- `packages/compiler/src/render3/view/compiler.ts`
- `packages/compiler/src/render3/view/api.ts`

---

## 1. Purpose & role in the compilation pipeline

This module is the **top-level orchestrator of direct-to-Ivy component and
directive compilation**. It takes fully-resolved metadata (`R3ComponentMetadata`
/ `R3DirectiveMetadata`) — produced upstream by the decorator handlers in the
Angular compiler-cli / partial-evaluator — and emits the output AST for the
`ɵɵdefineComponent({...})` / `ɵɵdefineDirective({...})` call expression plus the
`.d.ts` type declaration.

It does **not** itself walk the template AST and emit `ɵɵelement`/`ɵɵtext`/etc.
Instead, `compileComponentFromMetadata` delegates that to the **template
pipeline** (`template/pipeline/src/{ingest,emit}` + `transform`):

```
R3ComponentMetadata
   │  baseDirectiveFields()        → type, selectors, queries, hostBindings,
   │                                 inputs, outputs, exportAs, standalone, signals
   │  addFeatures()                → features:[ ProvidersFeature, HostDirectivesFeature,
   │                                 InheritDefinitionFeature, NgOnChangesFeature, … ]
   │
   │  ingestComponent(template.nodes, …)   → CompilationJob (Ivy IR)
   │  transform(job, Tmpl)                 → lowered IR (the actual ɵɵ instruction stream)
   │  emitTemplateFn(job)                  → template: function MyCmp_Template(rf, ctx){…}
   │
   │  decls / vars / consts / ngContentSelectors / dependencies / styles /
   │  encapsulation / data(animations) / changeDetection
   ▼
ɵɵdefineComponent({ … definitionMap … })   +  ComponentDeclaration<…> type
```

So this file is the **glue between metadata and the IR pipeline**, plus the
owner of all the *non-template* definition fields and the `.d.ts` type emission.
It is the natural "entry point" of the render3 view compiler and one of the most
central modules to port — almost every other render3/view module is reachable
from here.

---

## 2. Public API (exact signatures)

```ts
// ---- compiler.ts exports ----

export function compileDirectiveFromMetadata(
  meta: R3DirectiveMetadata,
  constantPool: ConstantPool,
  bindingParser: BindingParser,
): R3CompiledExpression;

export function compileComponentFromMetadata(
  meta: R3ComponentMetadata<R3TemplateDependency>,
  constantPool: ConstantPool,
  bindingParser: BindingParser,
): R3CompiledExpression;

export function createComponentType(
  meta: R3ComponentMetadata<R3TemplateDependency>,
): o.Type;

export function createDirectiveType(meta: R3DirectiveMetadata): o.Type;

export interface ParsedHostBindings {
  attributes: Record<string, o.Expression>;
  listeners: Record<string, string>;
  properties: Record<string, string>;
  specialAttributes: {styleAttr?: string; classAttr?: string};
}

export function parseHostBindings(host: {
  [key: string]: string | o.Expression;
}): ParsedHostBindings;

export function verifyHostBindings(
  bindings: ParsedHostBindings,
  sourceSpan: ParseSourceSpan,
): ParseError[];

export function encapsulateStyle(style: string, componentIdentifier?: string): string;

export function createHostDirectivesMappingArray(
  mapping: Record<string, string>,
): o.LiteralArrayExpr | null;

export function compileDeferResolverFunction(
  meta: R3DeferResolverFunctionMetadata,
): o.ArrowFunctionExpr;
```

Non-exported helpers (still part of the algorithm, must be ported as private
fns): `baseDirectiveFields`, `addFeatures`, `compileDeclarationList`,
`stringAsType`, `stringMapAsLiteralExpression`, `stringArrayAsType`,
`createBaseDirectiveTypeParams`, `getInputsTypeExpression`,
`createHostBindingsFunction`, `validateNoEventBindings`, `compileStyles`,
`createHostDirectivesType`, `createHostDirectivesFeatureArg`.

`R3CompiledExpression` (from `render3/util.ts`):
```ts
export interface R3CompiledExpression {
  expression: o.Expression;   // the ɵɵdefineComponent(...) call
  type: o.Type;               // ComponentDeclaration<...> for .d.ts
  statements: o.Statement[];  // always [] here; pool statements live on ConstantPool
}
```

---

## 3. Key data structures + proposed Rust mapping

All output-AST types (`o.Expression`, `o.Type`, `o.Statement`, `o.LiteralMapExpr`,
…) are the arena-allocated Ivy output-AST defined in `output/output_ast` (a
separate spec). Below, `OExpr<'a>` / `OType<'a>` / `OStmt<'a>` denote references
into that arena. Everything AST-bound carries the arena lifetime `'a`.

### 3.1 `R3DirectiveMetadata` (api.ts:19)

Plain data record. Fields:
`name`, `type: R3Reference`, `typeArgumentCount`, `typeSourceSpan`,
`deps: R3DependencyMetadata[] | 'invalid' | null`, `selector: string | null`,
`queries: R3QueryMetadata[]`, `viewQueries: R3QueryMetadata[]`,
`host: R3HostMetadata`, `lifecycle: {usesOnChanges: boolean}`,
`inputs: {[field]: R3InputMetadata}`, `outputs: {[field]: string}`,
`usesInheritance`, `controlCreate: {passThroughInput: string|null}|null`,
`exportAs: string[]|null`, `providers: o.Expression|null`, `isStandalone`,
`isSignal`, `hostDirectives: R3HostDirectiveMetadata[]|null`,
`legacyOptionalChaining`.

```rust
pub struct R3DirectiveMetadata<'a> {
    pub name: String,
    pub r#type: R3Reference<'a>,
    pub type_argument_count: u32,
    pub type_source_span: ParseSourceSpan,
    pub deps: Deps<'a>,                      // enum: List(Vec<R3DependencyMetadata>) | Invalid | None
    pub selector: Option<String>,
    pub queries: Vec<R3QueryMetadata<'a>>,
    pub view_queries: Vec<R3QueryMetadata<'a>>,
    pub host: R3HostMetadata<'a>,
    pub lifecycle: Lifecycle,                // { uses_on_changes: bool }
    pub inputs: IndexMap<String, R3InputMetadata<'a>>,   // insertion-ordered!
    pub outputs: IndexMap<String, String>,
    pub uses_inheritance: bool,
    pub control_create: Option<ControlCreate>,           // { pass_through_input: Option<String> }
    pub export_as: Option<Vec<String>>,
    pub providers: Option<OExpr<'a>>,
    pub is_standalone: bool,
    pub is_signal: bool,
    pub host_directives: Option<Vec<R3HostDirectiveMetadata<'a>>>,
    pub legacy_optional_chaining: bool,
}
```
Note: `deps: ... | 'invalid' | null` is a TS string-literal union → Rust enum.

### 3.2 `R3ComponentMetadata<DeclarationT>` (api.ts:205) — extends directive

```rust
pub struct R3ComponentMetadata<'a, D: R3TemplateDependency> {
    pub base: R3DirectiveMetadata<'a>,        // composition over inheritance
    pub template: ComponentTemplate<'a>,      // { nodes: Vec<t::Node>, ng_content_selectors: Vec<String>, preserve_whitespaces: Option<bool> }
    pub declarations: Vec<D>,
    pub defer: R3ComponentDeferMetadata<'a>,
    pub declaration_list_emit_mode: DeclarationListEmitMode,
    pub styles: Vec<String>,
    pub external_styles: Option<Vec<String>>,
    pub encapsulation: ViewEncapsulation,     // mutated in place during compile!
    pub animations: Option<OExpr<'a>>,
    pub view_providers: Option<OExpr<'a>>,
    pub relative_context_file_path: String,
    pub i18n_use_external_ids: bool,
    pub change_detection: Option<ChangeDetection<'a>>,  // enum: Strategy(i32) | Expr(OExpr<'a>)
    pub relative_template_path: Option<String>,
    pub has_directive_dependencies: bool,
    pub raw_imports: Option<OExpr<'a>>,
    pub foreign_imports: Option<Vec<R3ForeignComponentMetadata<'a>>>,
}
```
`changeDetection: ChangeDetectionStrategy | o.Expression | null` is the
union that drives the `typeof === 'number'` vs `'object'` branch (see §4) → Rust
enum `ChangeDetection { Strategy(i32), Expr(OExpr<'a>) }`, wrapped in `Option`.

`template.nodes: t.Node[]` are the **r3_ast** template nodes (separate spec) —
keep as `&'a t::Node` slices.

### 3.3 `R3ComponentDeferMetadata` (api.ts:318) — discriminated union

```rust
pub enum R3ComponentDeferMetadata<'a> {
    PerBlock { blocks: HashMap<DeferredBlockId, Option<OExpr<'a>>> },
    PerComponent { dependencies_fn: Option<OExpr<'a>> },
}
```
(`t.DeferredBlock` keys → use an xref/id, not the AST node pointer, in Rust.)

### 3.4 `DeferBlockDepsEmitMode` (api.ts:137) — `const enum`
```rust
#[repr(u8)] pub enum DeferBlockDepsEmitMode { PerBlock = 0, PerComponent = 1 }
```

### 3.5 `DeclarationListEmitMode` (api.ts:159) — `const enum`
```rust
#[repr(u8)] pub enum DeclarationListEmitMode { Direct, Closure, ClosureResolved, RuntimeResolved }
```

### 3.6 `R3InputMetadata` (api.ts:331)
```rust
pub struct R3InputMetadata<'a> {
    pub class_property_name: String,
    pub binding_property_name: String,
    pub required: bool,
    pub is_signal: bool,
    pub transform_function: Option<OExpr<'a>>,
}
```

### 3.7 `R3TemplateDependency*` (api.ts:345–427)
```rust
#[repr(u8)] pub enum R3TemplateDependencyKind { Directive = 0, Pipe = 1, NgModule = 2 }

pub trait R3TemplateDependency<'a> { fn kind(&self) -> R3TemplateDependencyKind; fn ty(&self) -> OExpr<'a>; }

pub enum R3TemplateDependencyMetadata<'a> {
    Directive(R3DirectiveDependencyMetadata<'a>), // selector, inputs:Vec<String>, outputs:Vec<String>, export_as:Option<Vec<String>>, is_component:bool
    Pipe(R3PipeDependencyMetadata<'a>),           // name
    NgModule(R3NgModuleDependencyMetadata<'a>),
}
pub struct R3ForeignComponentMetadata<'a> { pub name: String, pub component: OExpr<'a> }
```

### 3.8 `R3QueryMetadata` (api.ts:432)
```rust
pub struct R3QueryMetadata<'a> {
    pub property_name: String,
    pub first: bool,
    pub predicate: QueryPredicate<'a>,   // enum: Expr(MaybeForwardRefExpression<'a>) | Selectors(Vec<String>)
    pub descendants: bool,
    pub emit_distinct_changes_only: bool,
    pub read: Option<OExpr<'a>>,
    pub r#static: bool,
    pub is_signal: bool,
}
```

### 3.9 `R3HostMetadata` (api.ts:498) + `ParsedHostBindings` (compiler.ts:513)
```rust
pub struct R3HostMetadata<'a> {
    pub attributes: IndexMap<String, OExpr<'a>>,
    pub listeners: IndexMap<String, String>,
    pub properties: IndexMap<String, String>,
    pub special_attributes: SpecialAttrs,   // { style_attr: Option<String>, class_attr: Option<String> }
}
// ParsedHostBindings is structurally identical → reuse the same struct (or a type alias).
```

### 3.10 `R3HostDirectiveMetadata` (api.ts:520)
```rust
pub struct R3HostDirectiveMetadata<'a> {
    pub directive: R3Reference<'a>,
    pub is_forward_reference: bool,
    pub inputs: Option<IndexMap<String, String>>,
    pub outputs: Option<IndexMap<String, String>>,
}
```

### 3.11 `R3DeferResolverFunctionMetadata` + per-block/per-component deps (api.ts:537–595)
```rust
pub enum R3DeferResolverFunctionMetadata<'a> {
    PerBlock { dependencies: Vec<R3DeferPerBlockDependency<'a>> },
    PerComponent { dependencies: Vec<R3DeferPerComponentDependency> },
}
pub struct R3DeferPerBlockDependency<'a> {
    pub type_reference: OExpr<'a>, pub symbol_name: String,
    pub is_deferrable: bool, pub import_path: Option<String>, pub is_default_import: bool,
}
pub struct R3DeferPerComponentDependency {
    pub symbol_name: String, pub import_path: String, pub is_default_import: bool,
}
```

### 3.12 `DefinitionMap` (view/util.ts:153) — the central builder
```rust
pub struct DefinitionMap<'a> {
    values: Vec<LiteralMapEntry<'a>>,  // { key: String, quoted: bool, value: OExpr<'a> }
}
impl<'a> DefinitionMap<'a> {
    /// no-op when value is None (matches JS `if (value)` truthiness)
    pub fn set(&mut self, key: &str, value: Option<OExpr<'a>>) { … upsert by key … }
    pub fn to_literal_map(&self, b: &AstBuilder<'a>) -> OExpr<'a> { /* o.literalMap */ }
}
```
**Insertion order matters** — emitted object literal key order is observable in
golden output. Use an order-preserving structure (the TS impl pushes to an array
and upserts on duplicate key).

---

## 4. Algorithm walkthrough

### 4.1 `compileComponentFromMetadata` (compiler.ts:178) — the core entry

1. `definitionMap = baseDirectiveFields(meta, pool, bindingParser)` (§4.3).
2. `addFeatures(definitionMap, meta)` (§4.4).
3. **Defer deps fn (PerComponent only):** if
   `meta.defer.mode === PerComponent && meta.defer.dependenciesFn !== null`,
   declare `const <Name>_DeferFn = <dependenciesFn>` as a `DeclareVarStmt` (Final)
   pushed onto `constantPool.statements`, and set `allDeferrableDepsFn = variable(name)`.
4. **Compilation mode:**
   `compilationMode = (meta.isStandalone && !meta.hasDirectiveDependencies) ?
   DomOnly : Full`. Standalone components with no directive deps can use the
   slimmer DOM-only instruction set.
5. **Ingest:** `tpl = ingestComponent(meta.name, meta.template.nodes, pool,
   compilationMode, relativeContextFilePath, i18nUseExternalIds, meta.defer,
   allDeferrableDepsFn, relativeTemplatePath, getTemplateSourceLocationsEnabled(),
   legacyOptionalChaining, foreignImports)` → produces the Ivy IR
   `ComponentCompilationJob`.
6. **Transform:** `transform(tpl, CompilationJobKind.Tmpl)` runs the full ordered
   phase list (this is where the actual `ɵɵelement`/`ɵɵtext`/`ɵɵproperty`/… ops
   are lowered/optimized). See the pipeline specs.
7. **Emit template fn:** `templateFn = emitTemplateFn(tpl, pool)` →
   `function MyComponent_Template(rf, ctx) {…}`.
8. Set definition fields from the job results:
   - `ngContentSelectors` ← `tpl.contentSelectors` (if non-null).
   - `decls` ← `literal(tpl.root.decls)`, `vars` ← `literal(tpl.root.vars)`.
   - `consts`: if `tpl.consts.length > 0` →
     - if `tpl.constsInitializers.length > 0`: emit an arrow fn
       `() => { …initializers…; return [ …consts… ]; }`.
     - else: `literalArr(tpl.consts)`.
   - `template` ← `templateFn`.
9. **Dependencies:** based on `declarationListEmitMode`:
   - not `RuntimeResolved` && `declarations.length > 0` →
     `dependencies` = `compileDeclarationList(literalArr(declarations.map(d => d.type)), mode)` (§4.5).
   - `RuntimeResolved` → `dependencies` = `ɵɵgetComponentDepsFactory(type.value [, rawImports])`.
10. **Styles / encapsulation (in-place mutation of `meta.encapsulation`):**
    - If `encapsulation === null` → set to `Emulated`.
    - `hasStyles = !!externalStyles?.length`.
    - If `styles?.length`: when `Emulated`, run `compileStyles(styles, CONTENT_ATTR,
      HOST_ATTR)` (ShadowCss shimming using `_ngcontent-%COMP%` / `_nghost-%COMP%`);
      else use raw. Drop empty-after-trim styles; intern each via
      `pool.getConstLiteral`. If any survive → `hasStyles=true`, set `styles`.
    - If `!hasStyles && Emulated` → downgrade to `None` (no per-element css
      selectors generated).
    - If `encapsulation !== Emulated` → set `encapsulation` field to its literal.
11. **Animations:** if `meta.animations !== null` → set `data: {animation: <expr>}`
    (`quoted:false`).
12. **Change detection:** if `changeDetection !== null`:
    - `typeof === 'number'` && `!== OnPush` → set `changeDetection` literal (OnPush
      is the implicit default at runtime, so omitted).
    - `typeof === 'object'` (local compilation — unresolved expr) → set as-is.
13. `expression = ɵɵdefineComponent(definitionMap.toLiteralMap())`
    (called via `importExpr(R3.defineComponent).callFn([...], undefined, /*pure*/ true)`).
14. `type = createComponentType(meta)` (§4.6).
15. Return `{expression, type, statements: []}`.

### 4.2 `compileDirectiveFromMetadata` (compiler.ts:160)
`baseDirectiveFields` → `addFeatures` → `expression = ɵɵdefineDirective(map)` (pure)
→ `type = createDirectiveType(meta)`. No template, no styles, no ingest.

### 4.3 `baseDirectiveFields` (compiler.ts:39) — shared definition fields
Builds, in order:
`type` ← `meta.type.value`;
`selectors` ← `asLiteral(parseSelectorToR3Selector(selector))` if non-empty;
`contentQueries` ← `createContentQueriesFunction(...)` if queries;
`viewQuery` ← `createViewQueriesFunction(...)` if viewQueries;
`hostBindings` ← `createHostBindingsFunction(...)` (always called — also sets
`hostAttrs`/`hostVars` as a side effect, §4.7);
`inputs` ← `conditionallyCreateDirectiveBindingLiteral(inputs, /*forInputs*/ true)`;
`outputs` ← `conditionallyCreateDirectiveBindingLiteral(outputs)`;
`exportAs` ← `literalArr(...)` if non-null;
`standalone` ← `literal(false)` **only if** `isStandalone === false` (true omitted);
`signals` ← `literal(true)` if `isSignal`.

### 4.4 `addFeatures` (compiler.ts:108) — order is load-bearing
Pushes into `features[]` in this exact order:
1. `ɵɵProvidersFeature(providers [, viewProviders])` if either present
   (providers defaults to `[]` literal when only viewProviders).
2. `ɵɵHostDirectivesFeature(createHostDirectivesFeatureArg(...))` if any —
   **must precede inheritance feature** (comment in source: execution order).
3. `ɵɵInheritDefinitionFeature` if `usesInheritance`.
4. `ɵɵNgOnChangesFeature` if `lifecycle.usesOnChanges`.
5. `ɵɵControlFeature(literal(controlCreate.passThroughInput))` if `controlCreate !== null`.
6. `ɵɵExternalStylesFeature([...])` if `'externalStyles' in meta && externalStyles?.length`.
If `features.length` → set `features: literalArr(features)`.

### 4.5 `compileDeclarationList` (compiler.ts:352)
- `Direct` → list as-is.
- `Closure` → `() => [ …list… ]`.
- `ClosureResolved` → `() => [ …list… ].map(ɵresolveForwardRef)`.
- `RuntimeResolved` → throws (handled by the caller’s separate branch).

### 4.6 `createComponentType` / `createDirectiveType` (compiler.ts:334 / 434)
`createBaseDirectiveTypeParams` → `[ typeWithParameters(type.type, typeArgCount),
selectorForType|NONE, exportAs|NONE, inputsTypeExpr, outputsMap, queryPropNames ]`
(selector newlines stripped for the `.d.ts` string literal). Then:
- component: push `ngContentSelectors` type, `isStandalone`, hostDirectives type,
  and `isSignal` (only if true).
- directive: push `NONE_TYPE` (no ngContentSelectors slot), `isStandalone`,
  hostDirectives type, `isSignal` (if true).
Wrap in `ComponentDeclaration<…>` / `DirectiveDeclaration<…>` expression type.

### 4.7 `createHostBindingsFunction` (compiler.ts:451)
1. `bindingParser.createBoundHostProperties(host.properties, span)`.
2. `bindingParser.createDirectiveHostEventAsts(host.listeners, span)`.
3. Fold `specialAttributes.styleAttr`/`classAttr` back into `attributes` map as
   `style`/`class` literals.
4. `ingestHostBinding({componentName, componentSelector, properties, events,
   attributes, legacyOptionalChaining}, bindingParser, pool)` → host IR job.
5. `transform(hostJob, CompilationJobKind.Host)`.
6. Set `hostAttrs` ← `hostJob.root.attributes`; if `hostJob.root.vars > 0` set
   `hostVars` ← literal.
7. Return `emitHostBindingFunction(hostJob)` (may be `null`).

### 4.8 `parseHostBindings` (compiler.ts:520)
Classifies each `host` key: `(...)`→listener, `[...]`→property (incl. synthetic
`@`-prefixed), `class`/`style`→specialAttributes, else→attributes (string→literal,
else expr). Throws if listener/property value isn’t a string.

### 4.9 `verifyHostBindings` (compiler.ts:583)
Re-parses listeners + properties through a fresh `makeBindingParser()`, runs
`validateNoEventBindings` (rejects `on*` bound props/attrs for security), returns
`bindingParser.errors`.

### 4.10 `compileDeferResolverFunction` (compiler.ts:748)
Emits `() => [ <dep imports…> ]`. Per dep, deferrable → `import('path').then(m =>
m.<default|symbol>)` (with a `@ts-ignore` leading comment), non-deferrable
(`PerBlock` only) → bare `typeReference`. `PerComponent` always emits the dynamic
import form.

---

## 5. Dependencies on other compiler modules

- `../../constant_pool` (`ConstantPool`) — interning, `statements`, `getConstLiteral`.
- `../../core` — `parseSelectorToR3Selector`, `ViewEncapsulation`,
  `ChangeDetectionStrategy`, `InputFlags`.
- `../../output/output_ast` (`o.*`) — the entire emitted AST vocabulary.
- `../../parse_util` — `ParseError`, `ParseSourceSpan`.
- `../../shadow_css` (`ShadowCss`) — emulated-encapsulation CSS shimming.
- `../../template/pipeline/src/compilation` — `CompilationJobKind`, `TemplateCompilationMode`.
- `../../template/pipeline/src/emit` — `transform`, `emitTemplateFn`, `emitHostBindingFunction`.
- `../../template/pipeline/src/ingest` — `ingestComponent`, `ingestHostBinding`.
- `../../template_parser/binding_parser` (`BindingParser`).
- `../r3_identifiers` (`Identifiers as R3`) — all `ɵɵ*` symbol refs.
- `../util` — `R3CompiledExpression`, `tsIgnoreComment`, `typeWithParameters`, `R3Reference`, `MaybeForwardRefExpression`.
- `./api` — all metadata interfaces/enums.
- `./config` — `getTemplateSourceLocationsEnabled()`.
- `./query_generation` — `createContentQueriesFunction`, `createViewQueriesFunction`.
- `./template` — `makeBindingParser`.
- `./util` — `asLiteral`, `conditionallyCreateDirectiveBindingLiteral`, `DefinitionMap`.
- `../r3_ast` (`t.*`), `../r3_factory` (`R3DependencyMetadata`) via api.ts.

The **heavy** dependency is the template pipeline (`ingest`/`transform`/`emit`):
that is where the `ɵɵ` instruction stream is actually produced. This module owns
only the definition-object shell + type emission.

---

## 6. ɵɵ instructions / output emitted

This module emits **definition-level calls and feature wrappers** (not per-node
instructions — those come from the pipeline). Enumerated:

- `ɵɵdefineComponent({...})` — pure call (compiler.ts:323).
- `ɵɵdefineDirective({...})` — pure call (compiler.ts:168).
- Feature calls/refs: `ɵɵProvidersFeature(...)`, `ɵɵHostDirectivesFeature(...)`,
  `ɵɵInheritDefinitionFeature`, `ɵɵNgOnChangesFeature`, `ɵɵControlFeature(...)`,
  `ɵɵExternalStylesFeature([...])`.
- `ɵɵgetComponentDepsFactory(type [, rawImports])` (RuntimeResolved deps).
- `ɵresolveForwardRef` (ClosureResolved deps `.map(...)`).
- `.d.ts` type refs: `ComponentDeclaration<…>`, `DirectiveDeclaration<…>`.
- Definition-map keys emitted (component): `type, selectors, contentQueries,
  viewQuery, hostBindings, hostAttrs, hostVars, inputs, outputs, exportAs,
  standalone, signals, features, ngContentSelectors, decls, vars, consts,
  template, dependencies, styles, encapsulation, data, changeDetection`.
- Defer: `<Name>_DeferFn` const declaration; `import('…').then(m => m.X)` chains.

The `template:` function body is the host of the real `ɵɵelement`/`ɵɵtext`/
`ɵɵproperty`/`ɵɵlistener`/… stream, produced by `emitTemplateFn`.

---

## 7. Edge cases, gotchas, version-sensitivity

- **In-place mutation of `meta.encapsulation`** (compiler.ts:266/291). The
  metadata is mutated during compile (null→Emulated, Emulated→None when no
  styles). Rust port should take `&mut` or copy-then-mutate; don’t assume meta is
  immutable.
- **Feature ordering is semantic** — HostDirectives must precede
  InheritDefinition (runtime execution order). Preserve exactly.
- **`DefinitionMap.set` is falsy-skipping** — `if (value)` means `null`/`undefined`
  are dropped silently. Mirror with `Option<OExpr>` + skip on `None`. Also it
  *upserts* by key (later set overwrites).
- **OnPush is the implicit default**: numeric `changeDetection === OnPush` is
  intentionally *not* emitted. Object-typed `changeDetection` (local compilation)
  is passed through verbatim.
- **`standalone` field is only emitted when false**; `true` is the runtime
  default and omitted. `signals` only when true.
- **`'externalStyles' in meta`** narrows component-only field on a union type;
  Rust enum/composition makes this a simple `is_component` branch.
- **`getTemplateSourceLocationsEnabled()`** is a module-level global flag (in
  `./config`) — thread it as config, not a global, in Rust.
- **`tsIgnoreComment()` on dynamic imports** — needed because emitted import paths
  may omit extensions; the leading `@ts-ignore` must be attached to the emitted
  call. Make sure the output-AST port supports leading comments.
- **`consts` arrow form**: when `constsInitializers` is non-empty, `consts` becomes
  a function returning the array, not a bare array. Both forms must be supported.
- **`hasDirectiveDependencies` + standalone** drives DomOnly vs Full mode — a
  real golden-output divergence; don’t guess.
- **Version churn (api.ts churn)**: `controlCreate`/`ɵɵControlFeature`,
  `foreignImports`/`R3ForeignComponentMetadata`, `legacyOptionalChaining`,
  `rawImports`, `externalStyles`/`ɵɵExternalStylesFeature`, signal inputs
  (`isSignal`, `R3InputMetadata.transformFunction`), and the `isSignal` trailing
  type param (TODO comments reference v16/v17 compatibility) are all relatively
  recent additions. Pin behavior to **22.1.0-next.0** exactly; older Angular
  golden tests will differ.
- **`R3ComponentDeferMetadata` is a discriminated union** keyed on `mode`; the
  `PerBlock` variant keys a `Map` by `t.DeferredBlock` AST node identity — in Rust
  use a stable id/xref, never pointer identity.
- **`deps: ... | 'invalid'`** sentinel string — easy to miss; model as enum.
- **`inputs` map iteration order** feeds `getInputsTypeExpression` and the inputs
  literal — must be insertion-ordered (`IndexMap`).

---

## 8. Port plan (Rust / OXC)

### Strategy
This module is *orchestration + object-literal construction*, not heavy
algorithmic work. The hard parts are entirely in its dependencies (the pipeline
`ingest`/`transform`/`emit`, `ConstantPool`, `output_ast`, `ShadowCss`,
`BindingParser`, `query_generation`). So:

- Port `api.ts` (pure data types) **first and cheaply** — it’s just structs/enums.
  This unblocks many downstream modules.
- Port `DefinitionMap`, `asLiteral`, `conditionallyCreateDirectiveBindingLiteral`
  (view/util.ts) alongside.
- Port `compiler.ts` **after** the output-AST builder and the pipeline entry
  points exist, since `compileComponentFromMetadata` is a thin coordinator over
  them.

### What to reuse from OXC
- The Ivy `output_ast` is its **own** AST, distinct from `oxc_ast` ESTree. You
  build `o.Expression`/`o.Statement` nodes, then a separate output emitter lowers
  them to text. Two viable approaches:
  1. Keep a faithful Rust port of `output_ast` (arena-allocated, `'a`), then a
     final pass converts `output_ast` → `oxc_ast` and uses **`oxc_codegen`** for
     printing. Recommended: matches Angular’s architecture and keeps golden
     fidelity.
  2. Skip `output_ast` and build `oxc_ast` directly via `AstBuilder` — larger
     rewrite, loses the `ConstantPool`/typed-expr conveniences. Not recommended
     for this module.
- Use `oxc_allocator::Allocator` arena for all `OExpr<'a>` nodes; thread `&'a
  AstBuilder` / arena into `DefinitionMap::to_literal_map`, `compileDeclarationList`, etc.
- Reuse `oxc_codegen` only at the very end (whole-file emission), not per node.

### Concrete Rust shape
- `compile_component_from_metadata(meta: &mut R3ComponentMetadata<'a, D>, pool:
  &mut ConstantPool<'a>, binding_parser: &mut BindingParser, b: &'a AstBuilder<'a>)
  -> R3CompiledExpression<'a>`.
- `R3CompiledExpression { expression: OExpr<'a>, ty: OType<'a>, statements:
  Vec<OStmt<'a>> }` (statements always empty here; pool owns the rest).
- Model TS unions as Rust enums (`Deps`, `ChangeDetection`, `QueryPredicate`,
  `R3ComponentDeferMetadata`, `R3TemplateDependencyMetadata`).
- Use `IndexMap` for every `{[k]: v}` whose iteration order is emitted
  (`inputs`, `outputs`, `host.*`, hostDirectives `inputs`/`outputs`).

### Complexity & ordering
- **api.ts: low.** Pure data. Do early.
- **view/util.ts (DefinitionMap etc.): low.** Do with api.ts.
- **compiler.ts: medium** — the logic is straightforward branching, but it has a
  wide dependency surface. Its *true* complexity is gated by the pipeline modules.
  Schedule **after**: `output_ast`, `constant_pool`, `r3_identifiers`,
  `r3_ast`, `binding_parser`, `query_generation`, `shadow_css`, and the
  `template/pipeline` `ingest`/`transform`/`emit` trio. Once those exist, this
  module is a 1–2 day wiring job.
- `parseHostBindings`/`verifyHostBindings`/`encapsulateStyle`/
  `compileDeferResolverFunction` are independent leaf helpers and can be ported
  in isolation early (only need output_ast + ShadowCss + BindingParser).

### Test strategy
Golden-diff `ɵɵdefineComponent({...})` output against the TS compiler for a matrix
covering: standalone vs not, DomOnly vs Full, OnPush vs default vs local-expr
changeDetection, Emulated/None/ShadowDom encapsulation (with/without styles),
each feature flag, each `DeclarationListEmitMode`, and both defer modes.
