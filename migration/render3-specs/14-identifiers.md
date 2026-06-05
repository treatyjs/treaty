# Port Spec 14 — `render3/r3_identifiers.ts` (Runtime Identifier Table)

Source: `packages/compiler/src/render3/r3_identifiers.ts`
Angular version: **22.1.0-next.0**
Target: Rust + OXC (`oxc_ast` AstBuilder, `oxc_codegen`)

---

## 1. Purpose & Role in the Compilation Pipeline

`r3_identifiers.ts` is the **single source of truth for every Angular runtime symbol** that the render3 (Ivy) compiler can reference in generated code. It defines one class, `Identifiers`, whose static members are `o.ExternalReference` objects. Each `ExternalReference` is a `{name, moduleName}` pair that names a function/class/enum exported (usually under the `ɵɵ` / `ɵ` private prefix) from `@angular/core`.

Role in the pipeline:

- Downstream **template compilers** (`template.ts`, `view/`), **host binding compilers**, **defer compilers**, **i18n compilers**, **DI/factory compilers** (`r3_factory.ts`, `r3_injector_compiler.ts`, etc.), and **partial-linker declaration emitters** import `Identifiers` (almost always aliased `import {Identifiers as R3} from './r3_identifiers'`).
- They feed an `ExternalReference` into `o.importExpr(R3.someInstruction)` to produce an `ExternalExpr` AST node.
- During emission, the output AST translator / partial evaluator resolves the `ExternalReference` into either an imported identifier (e.g. `i0.ɵɵelement(...)`) or, in the partial/linker mode, a property access on the core import namespace.

So this module **emits no code itself** — it is a *constant lookup table*. It sits at the very bottom of the dependency graph: it depends only on `output/output_ast` (for the `ExternalReference` type), and almost every render3 emitter depends on it. This makes it an ideal, low-risk, **port-it-first** module.

The task note says the existing `runtime.rs` is a tiny subset; this spec enumerates the **full set** (214 entries — count via `grep -c` of `name:` lines, equal to the number of static members).

---

## 2. Public API — full TypeScript signatures

There is exactly one export:

```ts
export class Identifiers {
  static core: o.ExternalReference = {name: null, moduleName: CORE};
  static namespaceHTML: o.ExternalReference = {name: 'ɵɵnamespaceHTML', moduleName: CORE};
  // ... 212 more static fields ...
  static assertType = {name: 'ɵassertType', moduleName: CORE};
}
```

- `CORE` is a module-private `const CORE = '@angular/core';`. It is **not exported**.
- All members are `static` fields (no methods, no constructor). The class is never instantiated; it is used purely as a namespace.
- Most fields are typed `o.ExternalReference`; the last four (`InputSignalBrandWriteType`, `UnwrapDirectiveSignalInputs`, `unwrapWritableSignal`, `assertType`) omit the explicit annotation but are structurally identical `{name, moduleName}` object literals.

The referenced type (`packages/compiler/src/output/output_ast.ts:902`):

```ts
export class ExternalReference {
  constructor(
    public moduleName: string | null,
    public name: string | null,
  ) {}
  // Note: no isEquivalent method here as we use this as an interface too.
}
```

Note the deliberate dual use: `ExternalReference` is declared as a `class` but consumed structurally as an interface — the literals in `r3_identifiers.ts` are plain objects `{name, moduleName}`, never `new ExternalReference(...)`. The field order in the constructor (`moduleName`, `name`) differs from the literal order (`name`, `moduleName`); since literals are used, only the property names matter.

Consumed by (`output_ast.ts:1933`):

```ts
export function importExpr(
  id: ExternalReference,
  typeParams: Type[] | null = null,
  sourceSpan?: ParseSourceSpan | null,
): ExternalExpr {
  return new ExternalExpr(id, null, typeParams, sourceSpan);
}
```

and the AST node (`output_ast.ts:870`):

```ts
export class ExternalExpr extends Expression {
  constructor(
    public value: ExternalReference,
    type?: Type | null,
    public typeParams: Type[] | null = null,
    sourceSpan?: ParseSourceSpan | null,
    leadingComments?: LeadingComment[],
  ) { super(type, sourceSpan, leadingComments); }
}
```

---

## 3. Key Data Structures + Proposed Rust Mapping

### 3.1 `ExternalReference`

| TS field | Type | Notes |
|---|---|---|
| `name` | `string \| null` | Symbol name. `null` only for `Identifiers.core` (the bare namespace import). |
| `moduleName` | `string \| null` | Always `'@angular/core'` in this file. `null` possible elsewhere (local refs). |

The values are **static, never arena-bound, never mutated, always `'static` string slices**. They do not touch the OXC AST arena. A reference becomes arena-bound only when wrapped into an `ExternalExpr` AST node at emit time — and *that* node is built with the arena `'a` by whatever module constructs the import expression, not here.

Proposed Rust:

```rust
/// Mirror of o.ExternalReference. 'static because all identifier
/// table values are compile-time constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExternalReference {
    pub name: Option<&'static str>,
    pub module_name: Option<&'static str>,
}

const CORE: &str = "@angular/core";

const fn core_ref(name: &'static str) -> ExternalReference {
    ExternalReference { name: Some(name), module_name: Some(CORE) }
}
```

### 3.2 The identifier table

The natural, allocation-free mapping is a Rust **enum that names every instruction**, plus a `const fn` (or `match`) returning the `ExternalReference`. This preserves type-safety (callers reference `R3::Element` rather than a stringly-typed key) and is `const`-evaluable.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum R3 {
    Core,
    NamespaceHTML, NamespaceMathML, NamespaceSVG,
    Element, ElementStart, ElementEnd,
    // ... full variant list (see §6) ...
    AssertType,
}

impl R3 {
    pub const fn reference(self) -> ExternalReference {
        match self {
            R3::Core             => ExternalReference { name: None, module_name: Some(CORE) },
            R3::NamespaceHTML    => core_ref("ɵɵnamespaceHTML"),
            R3::Element          => core_ref("ɵɵelement"),
            // ...
            R3::AssertType       => core_ref("ɵassertType"),
        }
    }
}
```

Recommendation: **generate the enum + match from the TS source** with a small build script (parse the `static X: ... = {name: 'Y', ...}` lines) so the table cannot drift from Angular. Variant name = TS field name in PascalCase; the wire `name` string is taken verbatim (preserving the literal `ɵɵ`/`ɵ` prefixes and the few non-prefixed entries).

Avoid a runtime `HashMap<&str, ExternalReference>` for the primary representation — it adds allocation and loses compile-time checking. A `phf` map is acceptable only if a string-keyed lookup is genuinely needed (e.g. for a linker that round-trips names).

---

## 4. Algorithm Walkthrough

There is **no algorithm**. The module is a flat declaration of constants. The only "entry point" is field access:

1. A caller imports `Identifiers` (aliased `R3`).
2. Caller reads a static field, e.g. `R3.element`, yielding the literal `{name: 'ɵɵelement', moduleName: '@angular/core'}`.
3. Caller wraps it: `o.importExpr(R3.element)` → `new ExternalExpr({...}, null, null, undefined)`.
4. At codegen, the translator resolves `ExternalExpr.value` into the emitted import reference (`i0.ɵɵelement`).

Rust equivalent: `R3::Element.reference()` returns the `ExternalReference`; an emitter helper builds the OXC `Expression` (member access on the core import namespace) from it.

---

## 5. Dependencies on Other Compiler Modules

- **`output/output_ast` (`o`)** — the only import. Used solely for the `ExternalReference` type annotation.

No other dependencies. The module is a graph sink (leaf) on the dependency side. (Reverse-dependents: nearly the entire `render3/` tree.)

---

## 6. Full ɵɵ Instruction / Symbol Table Emitted

This module does not emit instructions itself, but it **enumerates every runtime symbol** the compiler may emit. Full set of `(static field → wire name)`, grouped by purpose. All `moduleName = '@angular/core'`.

**Namespace**
- `core` → `null`
- `namespaceHTML` → `ɵɵnamespaceHTML`
- `namespaceMathML` → `ɵɵnamespaceMathML`
- `namespaceSVG` → `ɵɵnamespaceSVG`

**Element / DOM creation**
- `element` → `ɵɵelement`
- `elementStart` → `ɵɵelementStart`
- `elementEnd` → `ɵɵelementEnd`
- `foreignComponent` → `ɵɵforeignComponent`
- `domElement` → `ɵɵdomElement`
- `domElementStart` → `ɵɵdomElementStart`
- `domElementEnd` → `ɵɵdomElementEnd`
- `domElementContainer` → `ɵɵdomElementContainer`
- `domElementContainerStart` → `ɵɵdomElementContainerStart`
- `domElementContainerEnd` → `ɵɵdomElementContainerEnd`
- `domTemplate` → `ɵɵdomTemplate`
- `domListener` → `ɵɵdomListener`
- `elementContainerStart` → `ɵɵelementContainerStart`
- `elementContainerEnd` → `ɵɵelementContainerEnd`
- `elementContainer` → `ɵɵelementContainer`

**Advance / view**
- `advance` → `ɵɵadvance`
- `nextContext` → `ɵɵnextContext`
- `resetView` → `ɵɵresetView`
- `getCurrentView` → `ɵɵgetCurrentView`
- `restoreView` → `ɵɵrestoreView`
- `reference` → `ɵɵreference`
- `enableBindings` → `ɵɵenableBindings`
- `disableBindings` → `ɵɵdisableBindings`
- `templateCreate` → `ɵɵtemplate`  *(field name differs from wire name)*

**Property / attribute / style binding**
- `syntheticHostProperty` → `ɵɵsyntheticHostProperty`
- `syntheticHostListener` → `ɵɵsyntheticHostListener`
- `attribute` → `ɵɵattribute`
- `classProp` → `ɵɵclassProp`
- `styleMap` → `ɵɵstyleMap`
- `classMap` → `ɵɵclassMap`
- `styleProp` → `ɵɵstyleProp`
- `domProperty` → `ɵɵdomProperty`
- `ariaProperty` → `ɵɵariaProperty`
- `property` → `ɵɵproperty`
- `control` → `ɵɵcontrol`
- `controlCreate` → `ɵɵcontrolCreate`

**Interpolation (text-less expression form)**
- `interpolate` → `ɵɵinterpolate`
- `interpolate1..8` → `ɵɵinterpolate1` … `ɵɵinterpolate8`
- `interpolateV` → `ɵɵinterpolateV`

**Text interpolation**
- `text` → `ɵɵtext`
- `textInterpolate` → `ɵɵtextInterpolate`
- `textInterpolate1..8` → `ɵɵtextInterpolate1` … `ɵɵtextInterpolate8`
- `textInterpolateV` → `ɵɵtextInterpolateV`

**Defer (`@defer`)**
- `defer` → `ɵɵdefer`
- `deferWhen` → `ɵɵdeferWhen`
- `deferOnIdle` → `ɵɵdeferOnIdle`
- `deferOnImmediate` → `ɵɵdeferOnImmediate`
- `deferOnTimer` → `ɵɵdeferOnTimer`
- `deferOnHover` → `ɵɵdeferOnHover`
- `deferOnInteraction` → `ɵɵdeferOnInteraction`
- `deferOnViewport` → `ɵɵdeferOnViewport`
- `deferPrefetchWhen` → `ɵɵdeferPrefetchWhen`
- `deferPrefetchOnIdle` → `ɵɵdeferPrefetchOnIdle`
- `deferPrefetchOnImmediate` → `ɵɵdeferPrefetchOnImmediate`
- `deferPrefetchOnTimer` → `ɵɵdeferPrefetchOnTimer`
- `deferPrefetchOnHover` → `ɵɵdeferPrefetchOnHover`
- `deferPrefetchOnInteraction` → `ɵɵdeferPrefetchOnInteraction`
- `deferPrefetchOnViewport` → `ɵɵdeferPrefetchOnViewport`
- `deferHydrateWhen` → `ɵɵdeferHydrateWhen`
- `deferHydrateNever` → `ɵɵdeferHydrateNever`
- `deferHydrateOnIdle` → `ɵɵdeferHydrateOnIdle`
- `deferHydrateOnImmediate` → `ɵɵdeferHydrateOnImmediate`
- `deferHydrateOnTimer` → `ɵɵdeferHydrateOnTimer`
- `deferHydrateOnHover` → `ɵɵdeferHydrateOnHover`
- `deferHydrateOnInteraction` → `ɵɵdeferHydrateOnInteraction`
- `deferHydrateOnViewport` → `ɵɵdeferHydrateOnViewport`
- `deferEnableTimerScheduling` → `ɵɵdeferEnableTimerScheduling`
- `enableIncrementalHydrationRuntime` → `ɵɵenableIncrementalHydrationRuntime`

**Control flow (`@if` / `@switch` / `@for`)**
- `conditionalCreate` → `ɵɵconditionalCreate`
- `conditionalBranchCreate` → `ɵɵconditionalBranchCreate`
- `conditional` → `ɵɵconditional`
- `repeater` → `ɵɵrepeater`
- `repeaterCreate` → `ɵɵrepeaterCreate`
- `repeaterTrackByIndex` → `ɵɵrepeaterTrackByIndex`
- `repeaterTrackByIdentity` → `ɵɵrepeaterTrackByIdentity`
- `componentInstance` → `ɵɵcomponentInstance`

**Pure functions / pipes**
- `pureFunction0..8` → `ɵɵpureFunction0` … `ɵɵpureFunction8`
- `pureFunctionV` → `ɵɵpureFunctionV`
- `pipeBind1..4` → `ɵɵpipeBind1` … `ɵɵpipeBind4`
- `pipeBindV` → `ɵɵpipeBindV`
- `pipe` → `ɵɵpipe`

**Animations**
- `animationEnterListener` → `ɵɵanimateEnterListener`  *(field name ≠ wire name)*
- `animationLeaveListener` → `ɵɵanimateLeaveListener`  *(field name ≠ wire name)*
- `animationEnter` → `ɵɵanimateEnter`  *(field name ≠ wire name)*
- `animationLeave` → `ɵɵanimateLeave`  *(field name ≠ wire name)*

**i18n**
- `i18n` → `ɵɵi18n`
- `i18nAttributes` → `ɵɵi18nAttributes`
- `i18nExp` → `ɵɵi18nExp`
- `i18nStart` → `ɵɵi18nStart`
- `i18nEnd` → `ɵɵi18nEnd`
- `i18nApply` → `ɵɵi18nApply`
- `i18nPostprocess` → `ɵɵi18nPostprocess`

**Projection / content**
- `projection` → `ɵɵprojection`
- `projectionDef` → `ɵɵprojectionDef`

**Dependency injection**
- `inject` → `ɵɵinject`
- `injectAttribute` → `ɵɵinjectAttribute`
- `directiveInject` → `ɵɵdirectiveInject`
- `invalidFactory` → `ɵɵinvalidFactory`
- `invalidFactoryDep` → `ɵɵinvalidFactoryDep`
- `templateRefExtractor` → `ɵɵtemplateRefExtractor`
- `forwardRef` → `forwardRef`  *(no ɵ prefix)*
- `resolveForwardRef` → `resolveForwardRef`  *(no ɵ prefix)*
- `getInheritedFactory` → `ɵɵgetInheritedFactory`
- `resolveWindow` → `ɵɵresolveWindow`
- `resolveDocument` → `ɵɵresolveDocument`
- `resolveBody` → `ɵɵresolveBody`
- `getComponentDepsFactory` → `ɵɵgetComponentDepsFactory`

**Metadata / HMR**
- `replaceMetadata` → `ɵɵreplaceMetadata`
- `getReplaceMetadataURL` → `ɵɵgetReplaceMetadataURL`

**Injectable / Service**
- `ɵɵdefineInjectable` → `ɵɵdefineInjectable`  *(field name itself is prefixed)*
- `declareInjectable` → `ɵɵngDeclareInjectable`
- `InjectableDeclaration` → `ɵɵInjectableDeclaration`
- `defineService` → `ɵɵdefineService`
- `declareService` → `ɵɵngDeclareService`

**Component**
- `defineComponent` → `ɵɵdefineComponent`
- `declareComponent` → `ɵɵngDeclareComponent`
- `setComponentScope` → `ɵɵsetComponentScope`
- `ChangeDetectionStrategy` → `ChangeDetectionStrategy`  *(enum, no prefix)*
- `ViewEncapsulation` → `ViewEncapsulation`  *(enum, no prefix)*
- `ComponentDeclaration` → `ɵɵComponentDeclaration`

**Factory**
- `FactoryDeclaration` → `ɵɵFactoryDeclaration`
- `declareFactory` → `ɵɵngDeclareFactory`
- `FactoryTarget` → `ɵɵFactoryTarget`

**Directive**
- `defineDirective` → `ɵɵdefineDirective`
- `declareDirective` → `ɵɵngDeclareDirective`
- `DirectiveDeclaration` → `ɵɵDirectiveDeclaration`

**Injector**
- `InjectorDef` → `ɵɵInjectorDef`
- `InjectorDeclaration` → `ɵɵInjectorDeclaration`
- `defineInjector` → `ɵɵdefineInjector`
- `declareInjector` → `ɵɵngDeclareInjector`

**NgModule**
- `NgModuleDeclaration` → `ɵɵNgModuleDeclaration`
- `ModuleWithProviders` → `ModuleWithProviders`  *(no prefix)*
- `defineNgModule` → `ɵɵdefineNgModule`
- `declareNgModule` → `ɵɵngDeclareNgModule`
- `setNgModuleScope` → `ɵɵsetNgModuleScope`
- `registerNgModuleType` → `ɵɵregisterNgModuleType`

**Pipe**
- `PipeDeclaration` → `ɵɵPipeDeclaration`
- `definePipe` → `ɵɵdefinePipe`
- `declarePipe` → `ɵɵngDeclarePipe`

**Class metadata / debug**
- `declareClassMetadata` → `ɵɵngDeclareClassMetadata`
- `declareClassMetadataAsync` → `ɵɵngDeclareClassMetadataAsync`
- `setClassMetadata` → `ɵsetClassMetadata`  *(single ɵ)*
- `setClassMetadataAsync` → `ɵsetClassMetadataAsync`  *(single ɵ)*
- `setClassDebugInfo` → `ɵsetClassDebugInfo`  *(single ɵ)*

**Queries**
- `queryRefresh` → `ɵɵqueryRefresh`
- `viewQuery` → `ɵɵviewQuery`
- `loadQuery` → `ɵɵloadQuery`
- `contentQuery` → `ɵɵcontentQuery`
- `viewQuerySignal` → `ɵɵviewQuerySignal`
- `contentQuerySignal` → `ɵɵcontentQuerySignal`
- `queryAdvance` → `ɵɵqueryAdvance`

**Two-way binding**
- `twoWayProperty` → `ɵɵtwoWayProperty`
- `twoWayBindingSet` → `ɵɵtwoWayBindingSet`
- `twoWayListener` → `ɵɵtwoWayListener`

**`@let` declarations**
- `declareLet` → `ɵɵdeclareLet`
- `storeLet` → `ɵɵstoreLet`
- `readContextLet` → `ɵɵreadContextLet`

**Misc**
- `arrowFunction` → `ɵɵarrowFunction`
- `attachSourceLocations` → `ɵɵattachSourceLocations`
- `listener` → `ɵɵlistener`

**Features**
- `NgOnChangesFeature` → `ɵɵNgOnChangesFeature`
- `ControlFeature` → `ɵɵControlFeature`
- `InheritDefinitionFeature` → `ɵɵInheritDefinitionFeature`
- `ProvidersFeature` → `ɵɵProvidersFeature`
- `HostDirectivesFeature` → `ɵɵHostDirectivesFeature`
- `ExternalStylesFeature` → `ɵɵExternalStylesFeature`

**Sanitization**
- `sanitizeHtml` → `ɵɵsanitizeHtml`
- `sanitizeStyle` → `ɵɵsanitizeStyle`
- `validateAttribute` → `ɵɵvalidateAttribute`
- `sanitizeResourceUrl` → `ɵɵsanitizeResourceUrl`
- `sanitizeScript` → `ɵɵsanitizeScript`
- `sanitizeUrl` → `ɵɵsanitizeUrl`
- `sanitizeUrlOrResourceUrl` → `ɵɵsanitizeUrlOrResourceUrl`
- `trustConstantHtml` → `ɵɵtrustConstantHtml`
- `trustConstantResourceUrl` → `ɵɵtrustConstantResourceUrl`

**Decorators (no prefix — public API names)**
- `inputDecorator` → `Input`
- `outputDecorator` → `Output`
- `viewChildDecorator` → `ViewChild`
- `viewChildrenDecorator` → `ViewChildren`
- `contentChildDecorator` → `ContentChild`
- `contentChildrenDecorator` → `ContentChildren`

**Type-checking helpers (single ɵ, untyped literals)**
- `InputSignalBrandWriteType` → `ɵINPUT_SIGNAL_BRAND_WRITE_TYPE`
- `UnwrapDirectiveSignalInputs` → `ɵUnwrapDirectiveSignalInputs`
- `unwrapWritableSignal` → `ɵunwrapWritableSignal`
- `assertType` → `ɵassertType`

Total: **214 static members.**

---

## 7. Edge Cases, Gotchas, Version Sensitivity

1. **Field name ≠ wire name.** Several entries deliberately diverge: `templateCreate` → `ɵɵtemplate`; the four `animation*` fields → `ɵɵanimate*`; `ɵɵdefineInjectable` (the *field* is named with the prefix). Do **not** auto-derive the wire string from the field name — copy the literal.

2. **Prefix variance — three families:**
   - `ɵɵ` (double) — most runtime instructions.
   - `ɵ` (single) — `setClassMetadata`, `setClassMetadataAsync`, `setClassDebugInfo`, `InputSignalBrandWriteType`, `UnwrapDirectiveSignalInputs`, `unwrapWritableSignal`, `assertType`.
   - no prefix — public-API names: `forwardRef`, `resolveForwardRef`, `ChangeDetectionStrategy`, `ViewEncapsulation`, `ModuleWithProviders`, and the six decorators (`Input`, `Output`, `ViewChild`, etc.).
   The `ɵ` is U+0275 (Latin small letter barred O). Ensure the Rust source is UTF-8 and the literals are byte-for-byte identical — a wrong codepoint produces a runtime import that does not resolve.

3. **`Identifiers.core` has `name: null`.** This is the namespace import (`import * as i0 from '@angular/core'`). Model `name` as `Option<&str>`, not `&str`.

4. **Untyped tail entries.** The final four members lack the `: o.ExternalReference` annotation; structurally identical. In Rust they map to the same struct — no special handling.

5. **`ExternalReference` dual class/interface nature.** TS uses it as both a class and a structural interface, and the constructor parameter order (`moduleName`, `name`) is the *reverse* of the literal field order. In Rust there is no ambiguity — use one struct with named fields.

6. **High version churn (Angular-internal API).** This file changes more often than almost any other in the compiler: new instructions appear with every minor (recent additions visible here: `foreignComponent`, the `domElement*`/`domListener`/`domProperty` family, `control`/`controlCreate`/`ControlFeature`, `conditionalCreate`/`conditionalBranchCreate`, the full `deferHydrate*` set, `ariaProperty`, signal queries, `arrowFunction`, `attachSourceLocations`). The exact set is tied to the runtime in `@angular/core@22.1.0-next.0`. The Rust port must:
   - pin the table to a specific Angular version, and
   - be trivially regenerable when bumping Angular. A codegen build step keyed off this TS file is strongly recommended over hand-maintenance.

7. **Linker/partial mode names.** `declare*` entries (`ɵɵngDeclare*`) are emitted in *partial compilation* output and consumed by the partial linker; `define*` entries are emitted in *full* (JIT/AOT-local) output. Both must be present; do not prune one set. Which one a caller uses is decided by the emitter, not here.

8. **No behavior, no parsing, no error paths.** Nothing to test functionally; correctness is purely "every name string matches Angular's exports". A golden test comparing the Rust table to the parsed TS source is the right verification.

---

## 8. Port Plan (Rust / OXC)

**Representation.** A `#[derive(Clone, Copy, PartialEq, Eq, Hash)] enum R3` with one variant per static field, plus `const fn reference(self) -> ExternalReference`. `ExternalReference { name: Option<&'static str>, module_name: Option<&'static str> }`. Everything is `'static` and `const` — zero arena involvement, zero allocation.

**Generation.** Write a build-time generator (or one-off script) that reads `r3_identifiers.ts`, matches each `static <field>... = {name: '<wire>'... }` line, and emits the `enum` + `match` arm `R3::<PascalField> => core_ref("<wire>")`. This guarantees fidelity and makes Angular version bumps a one-command refresh. Hand-writing 214 entries is error-prone (the `ɵ` vs `ɵɵ` distinction especially).

**What to reuse from OXC.** Nothing for the table itself — it is plain Rust constants. OXC enters only at the *consumption* site: a helper (likely in the output-AST/emit module, not here) takes an `ExternalReference` and builds an `oxc_ast` `Expression<'a>` — either a member-expression `i0.<name>` (full mode) or a bare `IdentifierReference` resolved through an import map. That helper, parameterized by the arena `&'a AstBuilder`, lives downstream; this module supplies only the data.

**Verification.** Golden/snapshot test that re-parses the TS file and asserts the Rust table is identical (same count, same name strings, same `null`s). Add a unit test that `R3::Core.reference().name == None`.

**Complexity:** **Low.** Mechanical, no logic, no edge-case branching. Risk is limited to (a) transcription accuracy of the `ɵ` prefixes and (b) keeping in sync with Angular — both solved by codegen.

**Ordering vs other modules:** **Port first** (foundation). It depends only on the `ExternalReference` type from the output-AST module, so the minimal prerequisite is defining that struct. Every render3 emitter (template, host bindings, defer, i18n, factory, injector, component/directive/pipe/ngmodule def, partial declarations) needs `R3::*`, so doing it up front unblocks all of them.
