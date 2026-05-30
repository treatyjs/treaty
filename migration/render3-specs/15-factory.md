# 15 — `render3/r3_factory.ts` — Port Spec (ɵfac / DI Factory Codegen)

> Source: `packages/compiler/src/render3/r3_factory.ts` (Angular `22.1.0-next.0`)
> Target: Rust port using OXC (`oxc_ast::AstBuilder` + `oxc_codegen`).
> Replaces the hand-rolled subset in `apps/rust/authoring/src/angular/transformers/dependency.rs`.

---

## 1. Purpose & role in the compilation pipeline

`r3_factory.ts` generates the **factory function** for any Angular type that participates in
dependency injection — components, directives, pipes, injectables, and NgModules. The factory is
the expression that becomes the `ɵfac` static member on a class (e.g.
`MyCmp.ɵfac = function MyCmp_Factory(t) { return new (t || MyCmp)(i0.ɵɵdirectiveInject(Dep)); };`).

It is **downstream** of metadata collection and **upstream** of definition assembly. The entry point
`compileFactoryFunction(meta)` is called by every render3 "compile X" routine:

- `compileComponentFromMetadata` / `compileDirectiveFromMetadata` (`render3/view/compiler.ts`)
- `compilePipeFromMetadata` (`render3/r3_pipe_compiler.ts`)
- `compileInjectable` (`injectable_compiler_2.ts`)
- `compileNgModule` (`render3/r3_module_compiler.ts`)
- The partial-compilation / declaration emitters and the JIT facade.

Its responsibilities:

1. Decide **how the instance is created**: direct constructor invocation, a delegated factory
   (another class/function), an arbitrary user expression, or inherited from a base class.
2. Compile each constructor dependency into the correct `ɵɵinject` / `ɵɵdirectiveInject` /
   `ɵɵinjectAttribute` call, encoding `@Self/@SkipSelf/@Host/@Optional` flags.
3. Emit `ɵɵinvalidFactory()` / `ɵɵinvalidFactoryDep(i)` calls for unresolvable deps so the program
   still compiles and fails at runtime instead of compile time.
4. Produce the `ɵɵFactoryDeclaration<...>` **type** for `.d.ts` emission.

It does **not** assign the result to a class member or build a class — it returns an
`R3CompiledExpression` (expression + type + side-effect statements) that the caller wires in. This is
the crucial difference from the current `dependency.rs`, which both builds the function *and* injects
it as a `ClassElement` while only supporting the simplest case.

---

## 2. Public API (full TypeScript signatures)

```ts
// Primary entry point.
export function compileFactoryFunction(meta: R3FactoryMetadata): R3CompiledExpression;

// Type-only factory declaration (for .d.ts). Also called internally by compileFactoryFunction.
export function createFactoryType(meta: R3FactoryMetadata): o.ExpressionType;

// Discriminant type guards over the R3FactoryMetadata union.
export function isDelegatedFactoryMetadata(
  meta: R3FactoryMetadata,
): meta is R3DelegatedFnOrClassMetadata;

export function isExpressionFactoryMetadata(
  meta: R3FactoryMetadata,
): meta is R3ExpressionFactoryMetadata;
```

Exported types/enums: `R3ConstructorFactoryMetadata`, `R3FactoryDelegateType`,
`R3DelegatedFnOrClassMetadata`, `R3ExpressionFactoryMetadata`, `R3FactoryMetadata`,
`R3DependencyMetadata`.

Module-private helpers (not exported, but must be ported):
`injectDependencies`, `compileInjectDependency`, `createCtorDepsType`, `createCtorDepType`,
`getInjectFn`.

---

## 3. Key data structures + proposed Rust mapping

All AST expression types (`o.Expression`, `o.Type`, etc.) are arena-bound in the Rust port (OXC
`Expression<'a>`), so every struct carrying them is parameterized by `'a`.

### `R3ConstructorFactoryMetadata`

```ts
export interface R3ConstructorFactoryMetadata {
  name: string;
  type: R3Reference;                       // { value: o.Expression; type: o.Expression }
  typeArgumentCount: number;
  deps: R3DependencyMetadata[] | 'invalid' | null;
  target: FactoryTarget;
}
```

The `deps` tri-state is the most important field. `null` = no constructor (inherit from base);
`'invalid'` = at least one dep unresolvable (emit `ɵɵinvalidFactory`); `[]`/non-empty = real deps.

Rust:

```rust
/// Tri-state for constructor deps. Encodes the `R3DependencyMetadata[] | 'invalid' | null` union.
pub enum FactoryDeps<'a> {
    /// No constructor — inherit factory from the base class.
    Inherit,                               // was `null`
    /// One or more deps could not be resolved.
    Invalid,                               // was `'invalid'`
    /// Resolved dependency list (may be empty).
    Deps(Vec<R3DependencyMetadata<'a>>),
}

pub struct R3Reference<'a> {
    pub value: Expression<'a>,
    pub ty: Expression<'a>,                // `type` is reserved in Rust
}

pub struct R3ConstructorFactoryMetadata<'a> {
    pub name: String,
    pub ty: R3Reference<'a>,
    pub type_argument_count: u32,
    pub deps: FactoryDeps<'a>,
    pub target: FactoryTarget,
}
```

### `R3FactoryDelegateType` (enum)

```ts
export enum R3FactoryDelegateType { Class = 0, Function = 1 }
```

```rust
pub enum R3FactoryDelegateType { Class, Function }
```

### The metadata union

```ts
export interface R3DelegatedFnOrClassMetadata extends R3ConstructorFactoryMetadata {
  delegate: o.Expression;
  delegateType: R3FactoryDelegateType;
  delegateDeps: R3DependencyMetadata[];
}
export interface R3ExpressionFactoryMetadata extends R3ConstructorFactoryMetadata {
  expression: o.Expression;
}
export type R3FactoryMetadata =
  | R3ConstructorFactoryMetadata
  | R3DelegatedFnOrClassMetadata
  | R3ExpressionFactoryMetadata;
```

The TS union is discriminated at runtime by *presence of a field* (`isDelegatedFactoryMetadata`
checks `delegateType !== undefined`; `isExpressionFactoryMetadata` checks
`expression !== undefined`). Port this as a proper Rust enum rather than duck-typing — the common
`R3ConstructorFactoryMetadata` fields live in a shared `base`:

```rust
pub enum R3FactoryMetadata<'a> {
    /// Created by direct constructor invocation (or base-class inheritance when deps == Inherit).
    Constructor(R3ConstructorFactoryMetadata<'a>),
    /// Created by delegating to another class (`new delegate(...)`) or function (`delegate(...)`).
    Delegated {
        base: R3ConstructorFactoryMetadata<'a>,
        delegate: Expression<'a>,
        delegate_type: R3FactoryDelegateType,
        delegate_deps: Vec<R3DependencyMetadata<'a>>,
    },
    /// Created by evaluating an arbitrary user expression (e.g. `useFactory`/`useValue`).
    Expression {
        base: R3ConstructorFactoryMetadata<'a>,
        expression: Expression<'a>,
    },
}

impl<'a> R3FactoryMetadata<'a> {
    pub fn base(&self) -> &R3ConstructorFactoryMetadata<'a> { /* match arms */ }
}
```

The two `is*` guards collapse into Rust `match` arms (no separate functions needed).

### `R3DependencyMetadata`

```ts
export interface R3DependencyMetadata {
  token: o.Expression | null;             // null => invalid dep
  attributeNameType: o.Expression | null; // non-null => @Attribute() dep
  host: boolean;
  optional: boolean;
  self: boolean;
  skipSelf: boolean;
}
```

```rust
pub struct R3DependencyMetadata<'a> {
    /// `None` => the dep could not be resolved (emit `ɵɵinvalidFactoryDep(i)`).
    pub token: Option<Expression<'a>>,
    /// `Some` => this is an `@Attribute()` dependency (emit `ɵɵinjectAttribute`).
    pub attribute_name_type: Option<Expression<'a>>,
    pub host: bool,
    pub optional: bool,
    pub self_: bool,                       // `self` is reserved
    pub skip_self: bool,
}
```

### Supporting enums (defined in sibling modules — see §5)

```rust
// from core.ts  (const enum InjectFlags — bitflags)
bitflags! {
    pub struct InjectFlags: u8 {
        const DEFAULT  = 0;
        const HOST     = 1 << 0;
        const SELF     = 1 << 1;
        const SKIP_SELF= 1 << 2;
        const OPTIONAL = 1 << 3;
        const FOR_PIPE = 1 << 4;
    }
}

// from compiler_facade_interface.ts (enum FactoryTarget)
pub enum FactoryTarget {
    Directive = 0, Component = 1, Injectable = 2, Pipe = 3, NgModule = 4, Service = 5,
}
```

### Return type — `R3CompiledExpression` (from `render3/util.ts`)

```ts
export interface R3CompiledExpression {
  expression: o.Expression;
  type: o.Type;
  statements: o.Statement[];
}
```

```rust
pub struct R3CompiledExpression<'a> {
    pub expression: Expression<'a>,
    pub ty: Type<'a>,                      // output_ast Type, not a TS type annotation
    pub statements: Vec<Statement<'a>>,    // always empty for the factory
}
```

---

## 4. Algorithm walkthrough

### `compileFactoryFunction(meta) -> R3CompiledExpression`

Local variable names that appear verbatim in output: `t` = `__ngFactoryType__`,
`r` = `__ngConditionalFactory__`, the function name = `${meta.name}_Factory`, the base factory var =
`ɵ${meta.name}_BaseFactory`.

1. **Set up `t`.** `const t = o.variable('__ngFactoryType__');` — the factory's single parameter
   (the subclass type, used so subclasses can reuse a parent's factory).

2. **Compute `typeForCtor`** — what `new` is called on:
   - Not delegated → `new BinaryOperatorExpr(Or, t, meta.type.value)` i.e. `t || MyType`.
   - Delegated → just `t`.

3. **Compute `factoryComments`.** If `deps` is a real non-empty array, attach `[tsIgnoreComment()]`
   (`@ts-ignore`, leading, multiline). This is `undefined` otherwise. The comment must sit on a
   *statement*, never on an expression (the newline would break a `return`).

4. **Build `ctorExpr` and maybe `baseFactoryVar`:**
   - `deps !== null`:
     - `deps !== 'invalid'` → `ctorExpr = new InstantiateExpr(typeForCtor, injectDependencies(deps, target))`.
     - `deps === 'invalid'` → `ctorExpr` stays `null` (handled later as invalid factory).
   - `deps === null` → no constructor: `baseFactoryVar = o.variable('ɵ${name}_BaseFactory')` and
     `ctorExpr = baseFactoryVar.callFn([typeForCtor])`.

5. **Inner closure `makeConditionalFactory(nonCtorExpr) -> ReadVarExpr`** (used for delegated &
   expression metadata). Builds:
   ```
   var __ngConditionalFactory__ = null;
   if (t) { __ngConditionalFactory__ = <ctorExpr>; }   // or ɵɵinvalidFactory() if ctorExpr null
   else   { __ngConditionalFactory__ = <nonCtorExpr>; } // always gets a @ts-ignore
   ```
   Pushes a `DeclareVarStmt(r, NULL_EXPR, DYNAMIC_TYPE)` then an `ifStmt`. The "then" branch uses
   `ctorExpr` (with `factoryComments`) or, if `ctorExpr` is null, `ɵɵinvalidFactory().toStmt()`. The
   "else" branch sets `r` to `nonCtorExpr` and always carries a `tsIgnoreComment()`. Returns `r`.

6. **Compute `retExpr`** by metadata kind:
   - **Delegated** (`isDelegatedFactoryMetadata`): `delegateArgs = injectDependencies(delegateDeps, target)`.
     Pick `InstantiateExpr` (Class) or `InvokeFunctionExpr` (Function) over `meta.delegate` with
     those args, then `retExpr = makeConditionalFactory(factoryExpr)`.
   - **Expression** (`isExpressionFactoryMetadata`): `retExpr = makeConditionalFactory(meta.expression)`.
   - **Plain constructor**: `retExpr = ctorExpr`.

7. **Build the function body's return:**
   - `retExpr === null` → push `ɵɵinvalidFactory().toStmt()` (the whole factory is invalid).
   - `baseFactoryVar !== null` → memoized inherited factory:
     ```
     return (ɵMyType_BaseFactory || (ɵMyType_BaseFactory = ɵɵgetInheritedFactory(MyType)))(typeForCtor);
     ```
     Built as `BinaryOperatorExpr(Or, baseFactoryVar, baseFactoryVar.set(getInheritedFactoryCall))`
     then `.callFn([typeForCtor])`, wrapped in `ReturnStatement`.
   - else → `new ReturnStatement(retExpr, null, factoryComments)`.

8. **Wrap into a function expression:**
   ```ts
   o.fn([new o.FnParam(t.name, o.DYNAMIC_TYPE)], body, o.INFERRED_TYPE, undefined, `${meta.name}_Factory`)
   ```

9. **If `baseFactoryVar !== null`, wrap in a pure IIFE** so the base-factory var is hoisted once:
   ```ts
   (() => { let ɵMyType_BaseFactory; return function MyType_Factory(t){...}; })()   // marked pure
   ```
   via `o.arrowFn([], [DeclareVarStmt(baseFactoryVar.name, undefined, DYNAMIC_TYPE), ReturnStatement(factoryFn)]).callFn([], undefined, /*pure*/ true)`.

10. **Return** `{ expression: factoryFn, statements: [], type: createFactoryType(meta) }`.

### `createFactoryType(meta) -> ExpressionType`

```ts
const ctorDepsType = meta.deps !== null && meta.deps !== 'invalid'
  ? createCtorDepsType(meta.deps) : o.NONE_TYPE;
return o.expressionType(o.importExpr(R3.FactoryDeclaration, [
  typeWithParameters(meta.type.type, meta.typeArgumentCount),
  ctorDepsType,
]));
```
Produces the `ɵɵFactoryDeclaration<MyType, [dep-type-tuple]>` type used in `.d.ts` emission.

### `injectDependencies(deps, target)`

`deps.map((dep, i) => compileInjectDependency(dep, target, i))`.

### `compileInjectDependency(dep, target, index)`

1. `dep.token === null` → `o.importExpr(R3.invalidFactoryDep).callFn([o.literal(index)])`
   → emits `i0.ɵɵinvalidFactoryDep(i)`.
2. `dep.attributeNameType === null` (normal dep):
   - Build flags: `Default | Self? | SkipSelf? | Host? | Optional? | (target===Pipe ? ForPipe : 0)`.
   - `flagsParam` is emitted only when `flags !== Default || dep.optional`.
   - Args = `[token]` plus `flagsParam` if present.
   - `injectFn = getInjectFn(target)`; emit `importExpr(injectFn).callFn(args)`.
3. else (`@Attribute()` dep) → `o.importExpr(R3.injectAttribute).callFn([dep.token])`.
   Note: the *value* token is used in JS; `attributeNameType` is only used for typings.

### `getInjectFn(target)`

- `Component | Directive | Pipe` → `R3.directiveInject` (`ɵɵdirectiveInject`).
- `NgModule | Injectable | default` → `R3.inject` (`ɵɵinject`).
  (`Service` falls through to default → `ɵɵinject`.)

### `createCtorDepsType(deps)` / `createCtorDepType(dep)`

Build the dep-type tuple for `.d.ts`. For each dep, `createCtorDepType` emits a `LiteralMapExpr`
with keys (unquoted) `attribute` (from `attributeNameType`), `optional`, `host`, `self`, `skipSelf`
— only for the flags that are set — or `null` if no keys. If *any* dep produced a map,
`createCtorDepsType` wraps the array in `expressionType(literalArr(...))`; otherwise returns
`o.NONE_TYPE`.

---

## 5. Dependencies on other compiler modules

| Import | Used for | Spec |
| --- | --- | --- |
| `../output/output_ast` (`o`) | All expression/statement/type constructors: `variable`, `fn`, `arrowFn`, `importExpr`, `literal`, `literalArr`, `literalMap`, `BinaryOperatorExpr`, `BinaryOperator.Or`, `InstantiateExpr`, `InvokeFunctionExpr`, `ReadVarExpr`, `DeclareVarStmt`, `ReturnStatement`, `FnParam`, `ifStmt`, `expressionType`, `NULL_EXPR`, `DYNAMIC_TYPE`, `INFERRED_TYPE`, `NONE_TYPE`, `leadingComment`, `.set/.callFn/.toStmt` | 01 |
| `./r3_identifiers` (`R3`) | External refs: `inject`, `directiveInject`, `injectAttribute`, `invalidFactory`, `invalidFactoryDep`, `getInheritedFactory`, `FactoryDeclaration` | 14 |
| `./util` | `R3CompiledExpression`, `R3Reference`, `typeWithParameters`, `tsIgnoreComment` | (this/util) |
| `../compiler_facade_interface` | `FactoryTarget` enum | — |
| `../core` | `InjectFlags` const enum | — |
| Emitter (`oxc_codegen` in Rust) | Renders the resulting AST to JS | 02 |

This module is **purely a producer of output_ast** — it does no parsing and reads no template AST.

---

## 6. ɵɵ instructions / runtime symbols emitted

Not "instructions" in the template-instruction sense, but the runtime symbols this module can
reference (all `moduleName: CORE`, normally aliased `i0.` on emit):

| Symbol | When emitted |
| --- | --- |
| `ɵɵinject` | Normal dep for `Injectable`/`NgModule`/`Service` targets |
| `ɵɵdirectiveInject` | Normal dep for `Component`/`Directive`/`Pipe` targets |
| `ɵɵinjectAttribute` | `@Attribute()` dependency |
| `ɵɵinvalidFactory` | `deps === 'invalid'` or no formable expression |
| `ɵɵinvalidFactoryDep` | A single dep with `token === null` (arg = its index) |
| `ɵɵgetInheritedFactory` | `deps === null` (no own constructor → inherit) |
| `ɵɵFactoryDeclaration` | Type-only, in `createFactoryType` (for `.d.ts`) |

Shapes of the emitted factory expression (caller assigns to `Type.ɵfac`):

- **Plain:** `function MyType_Factory(t){ return new (t || MyType)(i0.ɵɵdirectiveInject(Dep)); }`
- **No deps:** `function MyType_Factory(t){ return new (t || MyType)(); }`
- **Inherited:** `(() => { let ɵMyType_BaseFactory; return function MyType_Factory(t){ return (ɵMyType_BaseFactory || (ɵMyType_BaseFactory = i0.ɵɵgetInheritedFactory(MyType)))(t || MyType); }; })()`
- **Delegated/expression:** function with the `__ngConditionalFactory__` if/else pattern.
- **Invalid:** `function MyType_Factory(t){ return i0.ɵɵinvalidFactory(); }`

Injection flag literal: only emitted when non-Default or optional; value is the OR of the
`InjectFlags` bits (e.g. `8` for optional, `12` for optional+skipSelf).

---

## 7. Edge cases, gotchas & version-sensitivity

- **Tri-state `deps`.** The `null` vs `'invalid'` vs array distinction drives three completely
  different output shapes. The Rust `FactoryDeps` enum makes this explicit — do not collapse to
  `Option<Vec<...>>`.
- **`@ts-ignore` placement.** Comments must attach to *statements* (the `ReturnStatement` /
  `set(...).toStmt(...)`), never expressions, because they introduce a newline. The alternate branch
  in `makeConditionalFactory` *always* gets a `@ts-ignore`; the main branch gets one only when there
  are real deps. OXC's codegen comment handling must place these as leading comments on the
  statement.
- **`(t || Type)` parenthesization.** `new (t || MyType)(...)` requires the `||` to be parenthesized
  inside the `new`. `InstantiateExpr` over a `BinaryOperatorExpr` handles precedence in the emitter;
  the Rust port must preserve the parens (matching the existing `dependency.rs` which wraps in
  `parenthesized_expression`).
- **Pure IIFE marker.** The inherited-factory IIFE is `.callFn([], undefined, /*pure*/ true)` — emit
  a `/*@__PURE__*/` annotation so bundlers can tree-shake. OXC: set the pure flag / emit the comment.
- **`flagsParam` emission rule.** `flags !== Default || dep.optional`. The redundant `|| dep.optional`
  is intentional defensiveness (optional already sets a bit) — replicate exactly to match golden
  output byte-for-byte.
- **`getInjectFn` default arm.** `Service` (FactoryTarget = 5) and any future target fall through to
  `ɵɵinject`. Use a catch-all arm in Rust, do not enumerate exhaustively-then-panic.
- **`attributeNameType` JS vs typings split.** For `@Attribute()`, the *runtime* call uses
  `dep.token` (which may be a non-literal expression like `foo()`), while `attributeNameType` only
  feeds `.d.ts` type generation. Don't conflate them.
- **Names are literal API.** `__ngFactoryType__`, `__ngConditionalFactory__`,
  `ɵ${name}_BaseFactory`, `${name}_Factory` are exact and asserted by Angular's golden tests.
- **`createCtorDepType` key order** is `attribute, optional, host, self, skipSelf` and keys are
  unquoted. Preserve order.
- **Version churn.** `FactoryTarget` gained `Service = 5` in recent versions; `InjectFlags.ForPipe`
  is `@internal`. `R3.inject` is `ɵɵinject` (the older `inject`/`directiveInject` split is stable as
  of v22). Pin these against the v22.1 `r3_identifiers.ts` (spec 14) rather than older docs.
- **The current `dependency.rs` is wrong/incomplete:** it emits `ɵɵinject` for everything (ignores
  `getInjectFn` target dispatch), has no flags, no invalid handling, no delegated/expression/
  inherited paths, no `_Factory` naming, and also performs class mutation. The port should replace
  its codegen core with this module and leave class-wiring to the caller.

---

## 8. Port plan (Rust / OXC)

### Module layout
Create `apps/rust/authoring/src/render3/r3_factory.rs` (mirroring the spec set). Keep it a **pure
expression builder** returning `R3CompiledExpression<'a>`; do not have it touch `Class` nodes. The
existing `transformers/dependency.rs` becomes a *caller* that maps decorator metadata →
`R3FactoryMetadata` and assigns the result to a `ɵfac` `ClassElement`.

### What to reuse from OXC
- `AstBuilder<'a>` for every node: `function`, `arrow_function_expression`, `call_expression`,
  `new_expression`, `logical_expression` (for `||`), `return_statement`, `variable_declaration`,
  `if_statement`, `static_member_expression` (for `i0.ɵɵinject`), `number_literal`,
  `array_expression`, `object_expression`.
- `oxc_codegen` for emission, including the `/*@__PURE__*/` comment and leading `@ts-ignore`
  comments (verify OXC's comment-attachment story — this is the riskiest emitter detail).
- An `output_ast` shim (spec 01) if the port keeps Angular's `o.*` indirection; otherwise build OXC
  nodes directly. Recommendation: **build OXC nodes directly** here — this module is small and the
  `o.InstantiateExpr/InvokeFunctionExpr/BinaryOperatorExpr` set maps cleanly to OXC `NewExpression`/
  `CallExpression`/`LogicalExpression`, so the `o.*` layer adds little.

### Identifiers
Depends on spec 14 (`r3_identifiers`): need a helper `import_expr(R3::Inject)` →
`i0.ɵɵinject` member expression. Port `R3.{inject, directiveInject, injectAttribute, invalidFactory,
invalidFactoryDep, getInheritedFactory, FactoryDeclaration}` as constants.

### Implementation ordering (suggested)
1. `InjectFlags` bitflags + `FactoryTarget` enum + `getInjectFn` (trivial, no AST).
2. `R3DependencyMetadata` + `compile_inject_dependency` + `inject_dependencies`.
3. `R3FactoryMetadata` enum + `compile_factory_function` plain-constructor path.
4. Inherited (base factory + IIFE) path.
5. `make_conditional_factory` + delegated + expression paths.
6. `create_factory_type` + `create_ctor_deps_type` (only needed if `.d.ts` emission is in scope;
   can be deferred/stubbed for JS-only output).

### Complexity & dependencies
- **Estimated complexity: medium.** ~345 LOC of pure builder logic, no parsing, no recursion over
  template AST. The hard parts are exact comment placement, the IIFE/pure marker, and `||`
  parenthesization — all emitter concerns.
- **Depends on:** spec 01 (output_ast / OXC builder conventions), spec 14 (identifiers). **Independent
  of** the template pipeline (specs 03–13), so it can be ported early and in parallel.
- **Depended on by:** the component/directive/pipe/injectable/module compilers (the future specs that
  assemble `ɵcmp`/`ɵdir`/`ɵpipe`/`ɵprov`/`ɵmod` and attach `ɵfac`). Port this **before** those
  assembly modules.
- Recommended sequence vs other modules: do `14-identifiers` → `15-factory` → `16-pipe_module_injector`
  → component/directive definition assembly.

### Test strategy
Golden-file the four shapes (plain, no-dep, inherited, delegated/expression) plus invalid-dep and
invalid-factory against Angular's emitted output. Verify `@ts-ignore`, `/*@__PURE__*/`, the
`(t || Type)` parens, and the exact generated names. Reuse Angular's own
`packages/compiler/test/render3/r3_compiler_spec` expectations as the oracle.
