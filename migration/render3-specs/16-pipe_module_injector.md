# Port Spec 16 — Pipe / NgModule / Injector / Class-Metadata Compilers

Source files (Angular 22.1.0-next.0), under
`packages/compiler/src/render3/`:

- `r3_pipe_compiler.ts`
- `r3_module_compiler.ts`
- `r3_injector_compiler.ts`
- `r3_class_metadata_compiler.ts`

These four modules are the "smaller definition" emitters of the render3 compiler.
None of them build the template instruction stream (no `ɵɵelement` / `ɵɵtext` etc.);
they each take a metadata struct and produce an `R3CompiledExpression`
(`{expression, type, statements}`) describing a static `ɵfac`-adjacent definition
field (`ɵpipe`, `ɵmod`, `ɵinj`) or a side-effecting metadata-registration call
(`setClassMetadata`). They are leaf emitters that mostly assemble Output-AST object
literals via `DefinitionMap` and `o.importExpr(...).callFn(...)`.

---

## 1. Purpose & Role in the Compilation Pipeline

For each decorated class the ngtsc/render3 backend collects metadata
(`R3PipeMetadata`, `R3NgModuleMetadata`, `R3InjectorMetadata`, `R3ClassMetadata`)
and calls one of these `compile*` functions. Each returns an Output-AST
`R3CompiledExpression` whose `expression` becomes the right-hand side of a static
class member (e.g. `MyPipe.ɵpipe = ɵɵdefinePipe({...})`), whose `type` becomes the
`.d.ts` declaration type (`ɵɵPipeDeclaration<...>`), and whose `statements` are extra
top-level side-effecting statements (e.g. the `ɵɵsetNgModuleScope` IIFE, the
`ɵɵregisterNgModuleType` call). The class-metadata compiler is different: it emits a
single dev-mode-guarded `setClassMetadata` call so TestBed can recover the original
decorators.

Pipeline position: these run *after* selector/scope analysis and dependency
resolution, and their outputs are interleaved with the factory (`ɵfac`) and
directive/component (`ɵcmp`/`ɵdir`) defs by the caller, not here.

Roles:
- **Pipe** — emit `ɵpipe = ɵɵdefinePipe({ name, type, pure, [standalone] })`.
- **NgModule** — emit `ɵmod = ɵɵdefineNgModule({ type, bootstrap?, declarations?, imports?, exports?, schemas?, id? })`, plus optional scope side effects and `registerNgModuleType`.
- **Injector** — emit `ɵinj = ɵɵdefineInjector({ providers?, imports? })`.
- **Class metadata** — emit a dev-guarded `setClassMetadata(...)` / `setClassMetadataAsync(...)` call for decorator round-tripping.

---

## 2. Public API (full TypeScript signatures)

### `r3_pipe_compiler.ts`
```ts
export interface R3PipeMetadata { /* see §3 */ }

export function compilePipeFromMetadata(metadata: R3PipeMetadata): R3CompiledExpression;
export function createPipeType(metadata: R3PipeMetadata): o.Type;
```

### `r3_module_compiler.ts`
```ts
export enum R3SelectorScopeMode { Inline, SideEffect, Omit }
export enum R3NgModuleMetadataKind { Global, Local, Isolated }

export interface R3NgModuleMetadataGlobal   extends R3NgModuleMetadataCommon { /* §3 */ }
export interface R3NgModuleMetadataLocal    extends R3NgModuleMetadataCommon { /* §3 */ }
export interface R3NgModuleMetadataIsolated extends R3NgModuleMetadataCommon { /* §3 */ }
export type R3NgModuleMetadata =
  | R3NgModuleMetadataGlobal
  | R3NgModuleMetadataLocal
  | R3NgModuleMetadataIsolated;

export function compileNgModule(meta: R3NgModuleMetadata): R3CompiledExpression;
export function compileNgModuleDeclarationExpression(meta: R3DeclareNgModuleFacade): o.Expression;
export function createNgModuleType(meta: R3NgModuleMetadata): o.ExpressionType;
```
Internal (not exported): `interface R3NgModuleMetadataCommon`, `interface R3NgModuleDefMap`,
`function generateSetNgModuleScopeCall(meta): o.Statement | null`,
`function tupleTypeOf(exp: R3Reference[]): o.Type`,
`function tupleOfTypes(types: o.Expression[]): o.Type`.

### `r3_injector_compiler.ts`
```ts
export interface R3InjectorMetadata {
  name: string;
  type: R3Reference;
  providers: o.Expression | null;
  imports: o.Expression[];
}
export function compileInjector(meta: R3InjectorMetadata): R3CompiledExpression;
export function createInjectorType(meta: R3InjectorMetadata): o.Type;
```

### `r3_class_metadata_compiler.ts`
```ts
export type CompileClassMetadataFn = (metadata: R3ClassMetadata) => o.Expression;

export interface R3ClassMetadata {
  type: o.Expression;
  decorators: o.Expression;
  ctorParameters: o.Expression | null;
  propDecorators: o.Expression | null;
}

export function compileClassMetadata(metadata: R3ClassMetadata): o.InvokeFunctionExpr;
export function compileComponentClassMetadata(
  metadata: R3ClassMetadata,
  dependencies: R3DeferPerComponentDependency[] | null,
): o.Expression;
export function compileOpaqueAsyncClassMetadata(
  metadata: R3ClassMetadata,
  deferResolver: o.Expression,
  deferredDependencyNames: string[],
): o.Expression;
export function compileComponentMetadataAsyncResolver(
  dependencies: R3DeferPerComponentDependency[],
): o.ArrowFunctionExpr;
```
Internal: `internalCompileClassMetadata`, `internalCompileSetClassMetadataAsync`.

---

## 3. Key Data Structures + Proposed Rust Mapping

Shared helpers (defined in `render3/util.ts`, ported elsewhere but referenced here):
```ts
export interface R3Reference { value: o.Expression; type: o.Expression; }
export interface R3CompiledExpression { expression: o.Expression; type: o.Type; statements: o.Statement[]; }
```
Rust (arena-bound Output-AST `'a`):
```rust
pub struct R3Reference<'a> { pub value: Expr<'a>, pub r#type: Expr<'a> }
pub struct R3CompiledExpression<'a> {
    pub expression: Expr<'a>,
    pub r#type: OutputType<'a>,
    pub statements: Vec<Stmt<'a>>, // bumpalo Vec if arena-allocated
}
```
(`Expr<'a>` / `OutputType<'a>` / `Stmt<'a>` = the ported Output-AST node enums. All four
compilers are AST-bound, so every struct below carries `'a`.)

### Pipe — `R3PipeMetadata`
| field | TS type | notes |
|---|---|---|
| `name` | `string` | pipe class name |
| `type` | `R3Reference` | reference to the pipe itself |
| `typeArgumentCount` | `number` | generic param count of the class |
| `pipeName` | `string \| null` | the `@Pipe({name})`; null only for some standalone/anon cases |
| `deps` | `R3DependencyMetadata[] \| null` | constructor deps — *not actually used by this file* (factory handles it) |
| `pure` | `boolean` | |
| `isStandalone` | `boolean` | |

```rust
pub struct R3PipeMetadata<'a> {
    pub name: &'a str,
    pub r#type: R3Reference<'a>,
    pub type_argument_count: u32,
    pub pipe_name: Option<&'a str>,
    pub deps: Option<Vec<R3DependencyMetadata<'a>>>, // present for API parity; unused here
    pub pure: bool,
    pub is_standalone: bool,
}
```

### NgModule enums
```rust
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum R3SelectorScopeMode { Inline, SideEffect, Omit }

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum R3NgModuleMetadataKind { Global, Local, Isolated }
```

### NgModule metadata — `R3NgModuleMetadataCommon` (internal base) + three variants
Common fields: `kind`, `type: R3Reference`, `selectorScopeMode: R3SelectorScopeMode`,
`schemas: R3Reference[] | null`, `id: o.Expression | null`.

`R3NgModuleMetadataGlobal` adds: `bootstrap: R3Reference[]`, `declarations: R3Reference[]`,
`publicDeclarationTypes: o.Expression[] | null`, `imports: R3Reference[]`,
`includeImportTypes: boolean`, `exports: R3Reference[]`, `containsForwardDecls: boolean`.

`R3NgModuleMetadataLocal` adds: `bootstrapExpression`, `declarationsExpression`,
`importsExpression`, `exportsExpression` (each `o.Expression | null`); pins
`selectorScopeMode: SideEffect`.

`R3NgModuleMetadataIsolated` adds: `importsExpression`, `exportsExpression`
(`o.Expression | null`); pins `selectorScopeMode: Omit`.

Because the discriminant determines the field set, the natural Rust mapping is a tagged enum
that **flattens the common fields into each variant** (Rust has no struct inheritance):
```rust
pub enum R3NgModuleMetadata<'a> {
    Global(R3NgModuleMetadataGlobal<'a>),
    Local(R3NgModuleMetadataLocal<'a>),
    Isolated(R3NgModuleMetadataIsolated<'a>),
}

pub struct R3NgModuleCommon<'a> {
    pub r#type: R3Reference<'a>,
    pub selector_scope_mode: R3SelectorScopeMode,
    pub schemas: Option<Vec<R3Reference<'a>>>,
    pub id: Option<Expr<'a>>,
}

pub struct R3NgModuleMetadataGlobal<'a> {
    pub common: R3NgModuleCommon<'a>,
    pub bootstrap: Vec<R3Reference<'a>>,
    pub declarations: Vec<R3Reference<'a>>,
    pub public_declaration_types: Option<Vec<Expr<'a>>>,
    pub imports: Vec<R3Reference<'a>>,
    pub include_import_types: bool,
    pub exports: Vec<R3Reference<'a>>,
    pub contains_forward_decls: bool,
}
pub struct R3NgModuleMetadataLocal<'a> {
    pub common: R3NgModuleCommon<'a>, // selector_scope_mode must == SideEffect
    pub bootstrap_expression: Option<Expr<'a>>,
    pub declarations_expression: Option<Expr<'a>>,
    pub imports_expression: Option<Expr<'a>>,
    pub exports_expression: Option<Expr<'a>>,
}
pub struct R3NgModuleMetadataIsolated<'a> {
    pub common: R3NgModuleCommon<'a>, // selector_scope_mode must == Omit
    pub imports_expression: Option<Expr<'a>>,
    pub exports_expression: Option<Expr<'a>>,
}
```
Note: `kind` is encoded by the Rust enum tag, not a stored field.

### `R3NgModuleDefMap` (internal, the `defineNgModule` literal shape)
Optional fields `type, bootstrap?, declarations?, imports?, exports?, schemas?: LiteralArrayExpr, id?`.
In Rust this need not be a struct — it is only used as the type parameter of
`DefinitionMap<T>`, which is built by `set(key, value)`. Port it as a string-keyed
builder (see `DefinitionMap` below); the field set is documentation only.

### `R3InjectorMetadata`
```rust
pub struct R3InjectorMetadata<'a> {
    pub name: &'a str,                 // carried but unused by the emitter
    pub r#type: R3Reference<'a>,
    pub providers: Option<Expr<'a>>,
    pub imports: Vec<Expr<'a>>,
}
```

### `R3ClassMetadata` + `CompileClassMetadataFn`
```rust
pub struct R3ClassMetadata<'a> {
    pub r#type: Expr<'a>,
    pub decorators: Expr<'a>,
    pub ctor_parameters: Option<Expr<'a>>,
    pub prop_decorators: Option<Expr<'a>>,
}
// CompileClassMetadataFn -> Box<dyn Fn(&R3ClassMetadata<'a>) -> Expr<'a>> or fn pointer.
```

### `R3DeferPerComponentDependency` (from `view/api.ts`, consumed here)
```rust
pub struct R3DeferPerComponentDependency<'a> {
    pub symbol_name: &'a str,
    pub import_path: &'a str,
    pub is_default_import: bool,
}
```

### `DefinitionMap<T>` (from `view/util.ts`) — the central builder
```ts
class DefinitionMap<T = any> {
  values: {key: string; quoted: boolean; value: o.Expression}[] = [];
  set(key: keyof T, value: o.Expression | null): void; // ignores falsy/null values
  toLiteralMap(): o.LiteralMapExpr;
}
```
Rust:
```rust
pub struct DefinitionMap<'a> {
    pub values: Vec<LiteralMapEntry<'a>>, // {key: &str, quoted: bool, value: Expr<'a>}
}
impl<'a> DefinitionMap<'a> {
    pub fn set(&mut self, key: &'a str, value: Option<Expr<'a>>) { /* skip None; overwrite existing key */ }
    pub fn to_literal_map(&self) -> Expr<'a> /* LiteralMapExpr */ { .. }
}
```
Gotcha: `set` is a no-op when `value` is `null`/falsy AND overwrites an existing entry with
the same key rather than appending. The Rust port must preserve both behaviours.

---

## 4. Algorithm Walkthrough

### `compilePipeFromMetadata(metadata)`
1. Build a list of `{key, value, quoted:false}` entries (note: this file uses a plain array,
   **not** `DefinitionMap`).
2. `name`: `o.literal(metadata.pipeName ?? metadata.name)` — fall back to class name.
3. `type`: `metadata.type.value`.
4. `pure`: `o.literal(metadata.pure)`.
5. If `isStandalone === false` (strict `=== false`), push `standalone: false`. (Standalone
   `true` is the default and omitted.)
6. `expression = importExpr(R3.definePipe).callFn([literalMap(entries)], undefined, /*pure*/ true)`.
7. `type = createPipeType(metadata)`.
8. Return `{expression, type, statements: []}`.

`createPipeType` → `ExpressionType(importExpr(R3.PipeDeclaration, [ typeWithParameters(type.type, typeArgumentCount), ExpressionType(LiteralExpr(pipeName)), ExpressionType(LiteralExpr(isStandalone)) ]))`.

### `compileNgModule(meta)` (main NgModule entry)
1. `statements = []`; `definitionMap = new DefinitionMap()`; `set('type', meta.type.value)`.
2. **bootstrap**: only for `Global` and only when `bootstrap.length > 0` →
   `set('bootstrap', refsToArray(meta.bootstrap, meta.containsForwardDecls))`.
3. **scope** by `selectorScopeMode`:
   - `Inline`: for each of `declarations`/`imports`/`exports` with `length > 0`, set the field
     to `refsToArray(refs, containsForwardDecls)`. (These fields only exist on `Global`.)
   - `SideEffect`: `setNgModuleScopeCall = generateSetNgModuleScopeCall(meta)`; if non-null,
     push onto `statements`.
   - `Omit`: nothing.
4. **schemas**: if `meta.schemas !== null && length > 0` → `set('schemas', literalArr(schemas.map(r => r.value)))`.
5. **id**: if `meta.id !== null` → `set('id', meta.id)` AND push side-effect statement
   `importExpr(R3.registerNgModuleType).callFn([meta.type.value, meta.id]).toStmt()`.
6. `expression = importExpr(R3.defineNgModule).callFn([definitionMap.toLiteralMap()], undefined, /*pure*/ true)`.
7. `type = createNgModuleType(meta)`. Return `{expression, type, statements}`.

### `generateSetNgModuleScopeCall(meta)`
1. Build `scopeMap = DefinitionMap<{declarations, imports, exports, bootstrap}>`.
2. For `declarations`/`imports`/`exports`: if `Global` & `length>0` use `refsToArray(...)`;
   if `Local` use the corresponding `*Expression` field directly (if truthy).
3. For `bootstrap`: only `Local` with `bootstrapExpression`.
4. If `scopeMap` is empty → return `null`.
5. Build `setNgModuleScope(meta.type.value, scopeMap.toLiteralMap())` as `InvokeFunctionExpr`.
6. Guard: `jitOnlyGuardedExpression(fnCall)` → `(typeof ngJitMode === 'undefined' || ngJitMode) && setNgModuleScope(...)`.
7. Wrap in zero-arg `FunctionExpr` whose body is `[guardedCall.toStmt()]`, then immediately
   invoke it (`InvokeFunctionExpr(iife, [])`), `.toStmt()`. → an IIFE statement.

### `createNgModuleType(meta)`
- `Local`: `new ExpressionType(meta.type.value)`.
- `Isolated`: `ExpressionType(importExpr(R3.NgModuleDeclaration, [ ExpressionType(type.type), NONE_TYPE, importsExpression?expressionType(...):NONE_TYPE, exportsExpression?...:NONE_TYPE ]))`.
- `Global`: `ExpressionType(importExpr(R3.NgModuleDeclaration, [ ExpressionType(type.type), publicDeclarationTypes===null ? tupleTypeOf(declarations) : tupleOfTypes(publicDeclarationTypes), includeImportTypes ? tupleTypeOf(imports) : NONE_TYPE, tupleTypeOf(exports) ]))`.
- `tupleTypeOf(refs)` → if non-empty `expressionType(literalArr(refs.map(r => typeofExpr(r.type))))` else `NONE_TYPE`.
- `tupleOfTypes(types)` → same but maps `typeofExpr(type)` over raw expressions.

### `compileNgModuleDeclarationExpression(meta: R3DeclareNgModuleFacade)` (JIT/linker path)
Builds a `DefinitionMap` from a facade object, wrapping each raw TS node in
`WrappedNodeExpr`: always `type`; conditionally `bootstrap/declarations/imports/exports/schemas/id`
when `!== undefined`. Returns `importExpr(R3.defineNgModule).callFn([map.toLiteralMap()])`
(NOTE: **no** `pure: true` flag here, unlike `compileNgModule`).

### `compileInjector(meta)`
1. `definitionMap = DefinitionMap<{providers, imports}>`.
2. If `providers !== null` → `set('providers', meta.providers)`.
3. If `imports.length > 0` → `set('imports', literalArr(meta.imports))`.
4. `expression = importExpr(R3.defineInjector).callFn([map.toLiteralMap()], undefined, /*pure*/ true)`.
5. `type = createInjectorType(meta)` = `ExpressionType(importExpr(R3.InjectorDeclaration, [ExpressionType(meta.type.type)]))`.

### Class metadata
- `compileClassMetadata(metadata)`:
  `arrowFn([], [devOnlyGuardedExpression(internalCompileClassMetadata(metadata)).toStmt()]).callFn([])`
  → `(() => { ngDevMode && setClassMetadata(...); })()`.
- `internalCompileClassMetadata` → `importExpr(R3.setClassMetadata).callFn([type, decorators, ctorParameters ?? literal(null), propDecorators ?? literal(null)])`.
- `compileComponentClassMetadata(metadata, dependencies)`:
  if `dependencies` null/empty → delegate to `compileClassMetadata`. Else call
  `internalCompileSetClassMetadataAsync(metadata, dependencies.map(d => new FnParam(d.symbolName, DYNAMIC_TYPE)), compileComponentMetadataAsyncResolver(dependencies))`.
- `compileOpaqueAsyncClassMetadata(metadata, deferResolver, names)`:
  `internalCompileSetClassMetadataAsync(metadata, names.map(n => new FnParam(n, DYNAMIC_TYPE)), deferResolver)`.
- `internalCompileSetClassMetadataAsync(metadata, wrapperParams, resolverFn)`:
  builds `setClassMetaWrapper = arrowFn(wrapperParams, [internalCompileClassMetadata(metadata).toStmt()])`,
  `setClassMetaAsync = importExpr(R3.setClassMetadataAsync).callFn([metadata.type, resolverFn, setClassMetaWrapper])`,
  then `arrowFn([], [devOnlyGuardedExpression(setClassMetaAsync).toStmt()]).callFn([])`.
- `compileComponentMetadataAsyncResolver(dependencies)`:
  for each dep build `innerFn = arrowFn([FnParam('m', DYNAMIC_TYPE)], variable('m').prop(isDefaultImport ? 'default' : symbolName))`;
  then `new DynamicImportExpr(importPath).prop('then').callFn([innerFn], undefined, undefined, [tsIgnoreComment()])`.
  Return `arrowFn([], literalArr(dynamicImports))` — `() => [ import('...').then(m => m.X), ... ]`.

---

## 5. Dependencies on Other Compiler Modules

- `../output/output_ast` (`o`): `importExpr`, `literal`, `literalMap`, `literalArr`,
  `expressionType`, `typeofExpr`, `arrowFn`, `variable`, `ExpressionType`, `LiteralExpr`,
  `WrappedNodeExpr`, `ExternalExpr`, `InvokeFunctionExpr`, `FunctionExpr`, `FnParam`,
  `ArrowFunctionExpr`, `DynamicImportExpr`, `LiteralArrayExpr`, `LiteralMapExpr`,
  `NONE_TYPE`, `DYNAMIC_TYPE`, `Type`, `Expression`, `Statement`. **Hard dependency on the
  Output-AST port** (spec for output_ast must land first).
- `./r3_identifiers` (`Identifiers as R3`): the `ExternalReference` constants for every
  emitted symbol.
- `./util`: `R3CompiledExpression`, `R3Reference`, `typeWithParameters`, `refsToArray`,
  `jitOnlyGuardedExpression`, `devOnlyGuardedExpression`, `tsIgnoreComment`.
- `./view/util`: `DefinitionMap`.
- `./view/api`: `R3DeferPerComponentDependency` (class-metadata only).
- `./r3_factory`: `R3DependencyMetadata` (pipe type only; field is unused in emission).
- `../compiler_facade_interface`: `R3DeclareNgModuleFacade` (module JIT path only).

---

## 6. ɵɵ Instructions / Output Emitted

These are runtime/core symbols (not template instructions), resolved through
`r3_identifiers` (all `moduleName: '@angular/core'`):

| Symbol (emitted name) | Identifier | Emitted by |
|---|---|---|
| `ɵɵdefinePipe` | `R3.definePipe` | `compilePipeFromMetadata` (pure call) |
| `ɵɵPipeDeclaration` | `R3.PipeDeclaration` | `createPipeType` (`.d.ts` type) |
| `ɵɵdefineNgModule` | `R3.defineNgModule` | `compileNgModule` (pure), `compileNgModuleDeclarationExpression` (non-pure) |
| `ɵɵsetNgModuleScope` | `R3.setNgModuleScope` | `generateSetNgModuleScopeCall` (JIT-guarded IIFE) |
| `ɵɵregisterNgModuleType` | `R3.registerNgModuleType` | `compileNgModule` when `id != null` |
| `ɵɵNgModuleDeclaration` | `R3.NgModuleDeclaration` | `createNgModuleType` (`.d.ts` type) |
| `ɵɵdefineInjector` | `R3.defineInjector` | `compileInjector` (pure) |
| `ɵɵInjectorDeclaration` | `R3.InjectorDeclaration` | `createInjectorType` (`.d.ts` type) |
| `ɵsetClassMetadata` | `R3.setClassMetadata` | class-metadata compilers (dev-guarded) |
| `ɵsetClassMetadataAsync` | `R3.setClassMetadataAsync` | async class-metadata compilers |

Guard wrappers emitted: `ngJitMode` (module scope), `ngDevMode` (class metadata). The
"pure" flag (third `callFn` arg `true`) emits a `/*@__PURE__*/` annotation enabling
tree-shaking of the def call.

---

## 7. Edge Cases, Gotchas, Version-Sensitivity

- **Pure flag asymmetry**: `compileNgModule` / `compilePipeFromMetadata` / `compileInjector`
  pass `true` as the third `callFn` arg (pure). `compileNgModuleDeclarationExpression`
  (JIT/linker path) does **not**. Replicate exactly — affects emitted `/*@__PURE__*/`.
- **`standalone` only when false**: pipe emits `standalone: false` only on strict `=== false`;
  standalone-true is the default and omitted. Same pattern recurs across render3 emitters.
- **`pipeName` fallback**: `name` field = `pipeName ?? metadata.name` (class name fallback).
  But in `createPipeType` the *type* uses `pipeName` directly (can be `null` literal).
- **`DefinitionMap.set` semantics**: silently drops `null`/falsy values and *overwrites*
  same-key entries (does not append). The pipe compiler bypasses `DefinitionMap` and uses a
  raw array — preserve that distinction.
- **Discriminated union by `kind`**: `compileNgModule`/`generateSetNgModuleScopeCall` branch on
  `meta.kind`. `Global` carries `R3Reference[]` arrays + `containsForwardDecls`; `Local`/`Isolated`
  carry pre-built `o.Expression`s. The Rust enum must not expose Global-only fields on other
  variants (TS relies on control-flow narrowing here).
- **`includeImportTypes` / `publicDeclarationTypes`** affect only the `.d.ts` type, not runtime.
  `publicDeclarationTypes === null` means "all declarations public".
- **Forward declarations**: `refsToArray(refs, shouldForwardDeclare)` wraps the literal array
  in `() => [...]` when true. Drives whether emitted arrays are eager or lazy.
- **`tsIgnoreComment()`**: a `@ts-ignore` leading comment attached to dynamic-import `.then`
  call args so emitted code compiles even if module path extensions are unknown. The Output-AST
  comment/`LeadingComment` machinery must support this.
- **`DynamicImportExpr`** must emit native `import('...')`. Ensure the OXC codegen path emits
  the `ImportExpression` node, not a require/helper.
- **Version churn risk**: symbol names carry the Ivy `ɵ`/`ɵɵ` prefixes; `R3NgModuleMetadataKind`
  gained `Isolated` (isolated-declarations / `.d.ts`-emit mode) — relatively recent. The local
  compilation mode (`Local`) and isolated mode are the most actively churned; full/global mode
  (`Global`) is stable. Keep the `kind`-based union open to new variants.
- **`deps` on `R3PipeMetadata`** is declared but unread in this file (factory compiler consumes
  it). Keep the field for metadata-struct parity but do not emit from it here.

---

## 8. Port Plan (Rust / OXC)

**Strategy.** These are the simplest render3 emitters and make an ideal early target once the
Output-AST + `DefinitionMap` + `r3_identifiers` + `util` helpers exist. No template instruction
machinery is involved. Each `compile*` is a pure function from a metadata struct to
`R3CompiledExpression<'a>`.

**Reuse from OXC / existing ports.**
- The emitted nodes are Output-AST nodes (Angular's `o.*`), not raw OXC AST. So the immediate
  dependency is the **ported Output-AST module** (`importExpr`, `literal`, `literalArr`,
  `literalMap`, `arrowFn`, `FnParam`, `DynamicImportExpr`, `ExpressionType`, `typeofExpr`,
  `NONE_TYPE`, `DYNAMIC_TYPE`, `WrappedNodeExpr`, etc.). OXC enters only later when the
  Output-AST is lowered to `oxc_ast` via the AstBuilder and printed by `oxc_codegen`.
- Reuse `DefinitionMap`, `R3Reference`, `R3CompiledExpression`, `refsToArray`,
  `typeWithParameters`, `jitOnlyGuardedExpression`, `devOnlyGuardedExpression`,
  `tsIgnoreComment` from the ported `util` / `view/util` modules.
- Reuse `r3_identifiers::Identifiers` constants for the `ExternalReference`s.

**Implementation order within this spec.**
1. `compileInjector` / `createInjectorType` — smallest, 2 fields. Validates `DefinitionMap` +
   `importExpr` + `ExpressionType` plumbing.
2. `compilePipeFromMetadata` / `createPipeType` — adds raw-array literal map, pure flag,
   `typeWithParameters`, `LiteralExpr` type args.
3. Class metadata — adds `arrowFn`, `FnParam`, `devOnlyGuardedExpression`, `DynamicImportExpr`,
   leading comments. (Defer-dependency async path is the only non-trivial part.)
4. NgModule — the largest: the 3-variant discriminated union, scope modes, JIT-guarded IIFE,
   `registerNgModuleType` side effect, `.d.ts` tuple-type generation, and the separate facade
   path. Do last.

**Estimated complexity.** Injector/Pipe: **low**. Class metadata: **low–medium** (async/defer
resolver and comment attachment add a little). NgModule: **medium** (union narrowing, two emit
paths, IIFE construction, type-tuple helpers). Overall module: **medium**, gated almost entirely
on the Output-AST port being complete.

**Ordering vs other modules.** Depends on: Output-AST, `r3_identifiers`, `render3/util`,
`render3/view/util` (DefinitionMap), and (for class metadata only) `view/api`
`R3DeferPerComponentDependency`. Should be ported *after* those foundation modules but can land
*before* the heavyweight directive/component/template-instruction compilers, since it shares no
template machinery and exercises the AST-emission plumbing end-to-end on small inputs.
