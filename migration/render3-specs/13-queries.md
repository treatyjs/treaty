# Port Spec 13 — Query Generation (`render3/view/query_generation.ts`)

Angular version: **22.1.0-next.0**
Source: `packages/compiler/src/render3/view/query_generation.ts`
Target: Rust + OXC (`oxc_ast` `AstBuilder` for construction, `oxc_codegen` for emission).

---

## 1. Purpose & role in the compilation pipeline

This module is responsible for generating the **view query** and **content query**
host/template functions that Angular's render3 runtime invokes to wire up decorator-
and signal-based queries on a directive/component definition.

It covers the following Angular query authoring forms:

- Decorator queries: `@ViewChild`, `@ViewChildren`, `@ContentChild`, `@ContentChildren`.
- Signal queries (the FOCUS of this spec): `viewChild()`, `viewChildren()`,
  `contentChild()`, `contentChildren()`.

The compiler (`view/compiler.ts`) calls into this module while assembling the
`ɵɵdefineComponent` / `ɵɵdefineDirective` object literal. The two exported entry
points each return an `o.Expression` (a function expression) that becomes the value
of the `viewQuery` and `contentQueries` fields of the directive definition:

```ts
// view/compiler.ts
import {createContentQueriesFunction, createViewQueriesFunction} from './query_generation';
...
createContentQueriesFunction(meta.queries, constantPool, meta.name)      // → contentQueries field
createViewQueriesFunction(meta.viewQueries, constantPool, meta.name)     // → viewQuery field
```

Each generated function has two phases driven by the render flags param (`rf`):
- **Create phase** (`rf & RenderFlags.Create`): registers the queries with the runtime
  (`ɵɵviewQuery` / `ɵɵviewQuerySignal` / `ɵɵcontentQuery` / `ɵɵcontentQuerySignal`).
- **Update phase** (`rf & RenderFlags.Update`): for legacy (non-signal) queries, refreshes
  the `QueryList` and assigns to the directive property; for signal queries it merely emits
  `ɵɵqueryAdvance(n)` to advance the runtime's lazy query index.

This module is a **leaf-ish IR producer**: it consumes already-resolved
`R3QueryMetadata` and produces `output_ast` (`o.*`) nodes. It does no template parsing
or type checking.

---

## 2. Public API (exact TypeScript signatures)

Three exported symbols.

```ts
export const enum QueryFlags {
  none = 0b0000,
  descendants = 0b0001,
  isStatic = 0b0010,
  emitDistinctChangesOnly = 0b0100,
}
```

```ts
export function getQueryPredicate(
  query: R3QueryMetadata,
  constantPool: ConstantPool,
): o.Expression;
```

```ts
// Define and update any view queries
export function createViewQueriesFunction(
  viewQueries: R3QueryMetadata[],
  constantPool: ConstantPool,
  name?: string,
): o.Expression;
```

```ts
// Define and update any content queries
export function createContentQueriesFunction(
  queries: R3QueryMetadata[],
  constantPool: ConstantPool,
  name?: string,
): o.Expression;
```

### Private (module-internal) helpers — load-bearing, must be ported

```ts
function renderFlagCheckIfStmt(flags: core.RenderFlags, statements: o.Statement[]): o.IfStmt;
function toQueryFlags(query: R3QueryMetadata): number;
function getQueryCreateParameters(
  query: R3QueryMetadata,
  constantPool: ConstantPool,
  prependParams?: o.Expression[],
): o.Expression[];
function collapseAdvanceStatements(
  statements: (o.Statement | typeof queryAdvancePlaceholder)[],
): o.Statement[];

const queryAdvancePlaceholder = Symbol('queryAdvancePlaceholder');
```

---

## 3. Key data structures & proposed Rust mappings

### 3.1 `QueryFlags` (exported `const enum`)

Bit flags, kept in sync with `packages/core/src/render3/interfaces/query.ts`.

| Member | Value | Meaning |
|---|---|---|
| `none` | `0b0000` | no flags |
| `descendants` | `0b0001` | descend into children |
| `isStatic` | `0b0010` | query computed statically / assigned eagerly (ViewEngine BC) |
| `emitDistinctChangesOnly` | `0b0100` | emit change only when results actually changed |

**Rust mapping** — a `bitflags`-style newtype (the values are OR'd into a single
integer literal, so a true enum is wrong; we need bit ops):

```rust
use bitflags::bitflags;

bitflags! {
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct QueryFlags: u32 {
        const NONE                       = 0b0000;
        const DESCENDANTS                = 0b0001;
        const IS_STATIC                  = 0b0010;
        const EMIT_DISTINCT_CHANGES_ONLY = 0b0100;
    }
}
```

If avoiding the `bitflags` crate, a `#[repr(transparent)] struct QueryFlags(u32)` with
`const` associated values and `|` impl works equivalently. The emitted JS only ever sees
the resulting integer literal, so this never reaches the AST as an enum.

### 3.2 `R3QueryMetadata` (input; defined in `view/api.ts`)

```ts
export interface R3QueryMetadata {
  propertyName: string;
  first: boolean;
  predicate: MaybeForwardRefExpression | string[];
  descendants: boolean;
  emitDistinctChangesOnly: boolean;
  read: o.Expression | null;
  static: boolean;
  isSignal: boolean;
}
```

**Rust mapping.** `predicate` and `read` reference `output_ast` expression nodes; in the
OXC port these will be whatever the project's `output_ast` equivalent is (an owned IR enum,
likely arena-allocated). I sketch it with an `OutputExpr<'a>` placeholder representing the
ported `o.Expression`:

```rust
pub struct R3QueryMetadata<'a> {
    pub property_name: &'a str,        // arena str, becomes a JS property name
    pub first: bool,
    pub predicate: QueryPredicate<'a>,
    pub descendants: bool,
    pub emit_distinct_changes_only: bool,
    pub read: Option<OutputExpr<'a>>,
    pub r#static: bool,                // `static` is a Rust keyword → raw ident
    pub is_signal: bool,
}

pub enum QueryPredicate<'a> {
    Selectors(Vec<&'a str>),                 // string[] form
    Expression(MaybeForwardRefExpression<'a>),
}
```

Note: `'a` lifetime ties to the IR arena because `OutputExpr` nodes are arena-bound. If the
ported `output_ast` is heap-owned (`Box`), drop the lifetime accordingly. This module does
**not** itself touch the OXC AST arena — it only builds `output_ast` IR; OXC AST emission
happens later in the `output_ast → oxc_ast` translation layer.

### 3.3 `MaybeForwardRefExpression<T>` (`render3/util.ts`)

```ts
export interface MaybeForwardRefExpression<T extends o.Expression = o.Expression> {
  expression: T;
  forwardRef: ForwardRefHandling;
}
```

### 3.4 `ForwardRefHandling` (`const enum`, `render3/util.ts`)

```ts
export const enum ForwardRefHandling {
  None,       // = 0, never wrapped
  Wrapped,    // = 1, still wrapped in forwardRef()
  Unwrapped,  // = 2, was wrapped, since unwrapped
}
```

**Rust mapping:**

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForwardRefHandling { None, Wrapped, Unwrapped }

pub struct MaybeForwardRefExpression<'a> {
    pub expression: OutputExpr<'a>,
    pub forward_ref: ForwardRefHandling,
}
```

### 3.5 `queryAdvancePlaceholder` sentinel

A unique `Symbol` used as a sentinel value interleaved into the update-statement list,
later collapsed into one `ɵɵqueryAdvance(count)` call.

**Rust mapping** — model the heterogeneous list as an explicit enum instead of a JS symbol:

```rust
enum UpdateStmt<'a> {
    Stmt(OutputStmt<'a>),
    QueryAdvance,            // the placeholder
}
```

---

## 4. Algorithm walkthroughs

### 4.1 `getQueryPredicate(query, constantPool) -> o.Expression`

1. If `query.predicate` is an **array of strings** (`Array.isArray`):
   - For each selector string, `split(',')`, `trim()` each token, wrap each as `o.literal(token)`.
     (Handles `'ref, ref1, refN'` packed into one array entry → multiple literals.)
   - Collect all into one flat array, build `o.literalArr(predicate)`.
   - Return `constantPool.getConstLiteral(literalArr, /* forceShared */ true)` — the literal
     array is hoisted into the constant pool and shared.
2. Otherwise `query.predicate` is a `MaybeForwardRefExpression`; switch on `forwardRef`:
   - `None` / `Unwrapped` → return `query.predicate.expression` as-is.
   - `Wrapped` → return `o.importExpr(R3.resolveForwardRef).callFn([query.predicate.expression])`
     i.e. `ɵresolveForwardRef(<expr>)`.

### 4.2 `toQueryFlags(query) -> number`

Bitwise-OR of:
- `descendants ? QueryFlags.descendants : none`
- `static ? QueryFlags.isStatic : none`
- `emitDistinctChangesOnly ? QueryFlags.emitDistinctChangesOnly : none`

Note: `first` is **not** encoded in flags — it only affects the update-phase property read
(`.first` vs the list itself).

### 4.3 `getQueryCreateParameters(query, constantPool, prependParams?) -> o.Expression[]`

Builds the argument list for the create-phase runtime call, in this exact order:

1. If `prependParams` provided, spread them first. (Content queries pass `[o.variable('dirIndex')]`;
   view queries pass nothing.)
2. If `query.isSignal`: push `new o.ReadPropExpr(o.variable(CONTEXT_NAME), query.propertyName)`
   — i.e. `ctx.<propertyName>` (the signal field reference, so the runtime can write into it).
3. Push `getQueryPredicate(query, constantPool)`.
4. Push `o.literal(toQueryFlags(query))`.
5. If `query.read` is non-null, push it.

Resulting shapes:
- Legacy view: `(predicate, flags[, read])`
- Signal view: `(ctx.prop, predicate, flags[, read])`
- Legacy content: `(dirIndex, predicate, flags[, read])`
- Signal content: `(dirIndex, ctx.prop, predicate, flags[, read])`

### 4.4 `collapseAdvanceStatements(statements) -> o.Statement[]`

Optimization to coalesce consecutive `queryAdvancePlaceholder`s into a single
`ɵɵqueryAdvance(count)` call (count omitted when it's 1).

1. Iterate the mixed list **in reverse**.
2. Maintain `advanceCollapseCount`; for each placeholder, increment it.
3. On hitting a real statement, `flushAdvanceCount()` (unshift a single
   `ɵɵqueryAdvance(count)` stmt — `[]` args if count===1 else `[literal(count)]`), reset count,
   then `unshift` the real statement.
4. After the loop, `flushAdvanceCount()` once more for any leading placeholders.

Because it `unshift`es while iterating in reverse, the original order is preserved and
runs of placeholders become one call.

### 4.5 `createViewQueriesFunction(viewQueries, constantPool, name?) -> o.Expression`

1. Init `createStatements: o.Statement[]`, `updateStatements: (Statement|placeholder)[]`.
2. Create a `tempAllocator = temporaryAllocator(st => updateStatements.push(st), TEMPORARY_NAME)`
   — lazily declares `let _t;` into the update list on first use.
3. Maintain two **chained-call accumulators** `viewQuerySignalCall` and `viewQueryCall`
   (initially null).
4. For each `query`:
   - `params = getQueryCreateParameters(query, constantPool)` (no prepend).
   - If signal: `viewQuerySignalCall ??= o.importExpr(R3.viewQuerySignal)`; then
     `viewQuerySignalCall = viewQuerySignalCall.callFn(params)` — **chains** subsequent calls
     onto the previous result (fluent `ɵɵviewQuerySignal(...)(...)...`).
   - Else: same pattern with `R3.viewQuery`.
   - If signal: `updateStatements.push(queryAdvancePlaceholder)` then continue (lazy update).
   - Else (legacy): allocate `temporary`, build:
     - `getQueryList = ɵɵloadQuery()`
     - `refresh = ɵɵqueryRefresh(temporary.set(getQueryList))`  → `ɵɵqueryRefresh(_t = ɵɵloadQuery())`
     - `updateDirective = ctx.<prop>.set(query.first ? temporary.prop('first') : temporary)`
     - push `refresh.and(updateDirective).toStmt()` → `(ɵɵqueryRefresh(_t = ɵɵloadQuery()) && (ctx.prop = _t[.first]));`
5. If `viewQuerySignalCall !== null`, push `new o.ExpressionStatement(viewQuerySignalCall)` to create stmts.
6. If `viewQueryCall !== null`, push it likewise. (Signal calls are emitted **before** legacy calls.)
7. `viewQueryFnName = name ? `${name}_Query` : null`.
8. Return `o.fn([FnParam(rf, NUMBER_TYPE), FnParam(ctx, DYNAMIC_TYPE)], [ if(rf&Create){createStatements}, if(rf&Update){collapseAdvanceStatements(updateStatements)} ], INFERRED_TYPE, null, viewQueryFnName)`.

### 4.6 `createContentQueriesFunction(queries, constantPool, name?) -> o.Expression`

Structurally identical to `createViewQueriesFunction` with these differences:
- Uses `R3.contentQuerySignal` / `R3.contentQuery`.
- `getQueryCreateParameters(query, constantPool, [o.variable('dirIndex')])` — prepends `dirIndex`.
- The generated function takes a **third param** `dirIndex` (`NUMBER_TYPE`).
- Function name suffix is `${name}_ContentQueries`.
- Uses a `for...of` loop (vs `forEach`) — behaviorally equivalent.

---

## 5. Dependencies on other compiler modules

| Import | Symbol(s) used | Notes |
|---|---|---|
| `../../constant_pool` | `ConstantPool` (`getConstLiteral`) | hoist/share predicate literal arrays |
| `../../core` | `core.RenderFlags` (`Create`, `Update`), `core.RenderFlags` type for flags | render flag enum |
| `../../output/output_ast` (`o`) | `ifStmt`, `variable`, `literal`, `literalArr`, `importExpr`, `fn`, `FnParam`, `ExpressionStatement`, `ReadPropExpr`, `DeclareVarStmt`, `NUMBER_TYPE`, `DYNAMIC_TYPE`, `INFERRED_TYPE`, expr methods `.callFn`, `.bitwiseAnd`, `.and`, `.set`, `.prop`, `.toStmt` | the IR builder — the bulk of the port surface |
| `../r3_identifiers` (`R3`) | `viewQuery`, `viewQuerySignal`, `contentQuery`, `contentQuerySignal`, `loadQuery`, `queryRefresh`, `queryAdvance`, `resolveForwardRef` | `ExternalReference` records (`{name, moduleName}`) |
| `../util` | `ForwardRefHandling` | enum |
| `./api` | `R3QueryMetadata` (also `MaybeForwardRefExpression` indirectly) | input metadata |
| `./util` | `CONTEXT_NAME` (`'ctx'`), `RENDER_FLAGS` (`'rf'`), `TEMPORARY_NAME` (`'_t'`), `temporaryAllocator` | shared template-fn constants/helpers |

`R3.*` external references resolve to `@angular/core` (the `CORE` module name); the actual
ɵɵ names are listed in §6.

---

## 6. ɵɵ instructions / runtime references emitted

All come from `r3_identifiers.ts`, module `@angular/core`:

| `R3` symbol | Emitted name | When |
|---|---|---|
| `R3.viewQuery` | `ɵɵviewQuery` | create phase, legacy view query |
| `R3.viewQuerySignal` | `ɵɵviewQuerySignal` | create phase, signal view query (`viewChild`/`viewChildren`) |
| `R3.contentQuery` | `ɵɵcontentQuery` | create phase, legacy content query |
| `R3.contentQuerySignal` | `ɵɵcontentQuerySignal` | create phase, signal content query (`contentChild`/`contentChildren`) |
| `R3.loadQuery` | `ɵɵloadQuery` | update phase, legacy only |
| `R3.queryRefresh` | `ɵɵqueryRefresh` | update phase, legacy only |
| `R3.queryAdvance` | `ɵɵqueryAdvance` | update phase, signal queries (collapsed) |
| `R3.resolveForwardRef` | `resolveForwardRef` (NOT a `ɵɵ` instruction) | predicate wrapped in `forwardRef()` |

Representative emitted output:

```js
// signal viewChild, name = "MyCmp"
function MyCmp_Query(rf, ctx) {
  if (rf & 1) { ɵɵviewQuerySignal(ctx.myRef, SomeDir, 5); }   // 5 = descendants|isStatic
  if (rf & 2) { ɵɵqueryAdvance(); }
}

// legacy @ViewChild
function MyCmp_Query(rf, ctx) {
  if (rf & 1) { ɵɵviewQuery(_c0, 1); }
  if (rf & 2) {
    let _t;
    ɵɵqueryRefresh(_t = ɵɵloadQuery()) && (ctx.myRef = _t.first);
  }
}

// content, signal
function MyCmp_ContentQueries(rf, ctx, dirIndex) {
  if (rf & 1) { ɵɵcontentQuerySignal(dirIndex, ctx.items, SomeDir, 4); }
  if (rf & 2) { ɵɵqueryAdvance(); }
}
```

Mixed lists chain create calls fluently, e.g.
`ɵɵviewQuerySignal(...)( ... next signal ... )` and a separate
`ɵɵviewQuery(...)(...)` statement; signal-call statement is emitted **before** the legacy one.

---

## 7. Edge cases, gotchas & version sensitivity

- **`first` is not a flag.** It only changes the update-phase read (`_t.first` vs `_t`). Signal
  queries ignore it entirely at this layer (multiplicity is encoded in runtime / the signal field).
- **`static` is ignored for signal queries** at runtime (per `R3QueryMetadata.static` docs) but is
  still OR'd into the emitted flags by `toQueryFlags` regardless of `isSignal`. Port must replicate
  this unconditional OR — do not special-case signal.
- **Signal queries push `ctx.<prop>` into the create params** (`ReadPropExpr`), legacy queries do not.
- **Call chaining accumulator pattern** (`viewQueryCall ??= importExpr(...); viewQueryCall = viewQueryCall.callFn(params)`):
  the first iteration builds `importExpr(R3.x)` then `.callFn`, subsequent iterations call `.callFn`
  on the *previous call result*. This produces curried `f(...)(...)` chains, not multiple statements.
  Port must preserve this exact shape (it's load-bearing for byte-identical output).
- **Order of statements in create phase: signal calls first, then legacy calls.** Both functions
  push the signal accumulator before the legacy accumulator.
- **`collapseAdvanceStatements` reverse iteration + `unshift`** must be ported faithfully; count===1
  emits zero-arg `ɵɵqueryAdvance()`, count>1 emits `ɵɵqueryAdvance(count)`.
- **`getConstLiteral(..., true)`**: the `true` forces sharing/hoisting of the predicate literal array;
  the constant-pool port must support a `force_shared` flag.
- **Predicate string splitting on comma**: a single array entry `'a, b'` becomes two literals; whitespace
  is trimmed. Replicate exactly (`split(',')` then `trim()`).
- **`forwardRef` `Wrapped`** wraps the predicate expr in `resolveForwardRef(...)`; `None`/`Unwrapped`
  pass through. Note this differs from `convertFromMaybeForwardRefExpression` in `util.ts` (which wraps
  on `Unwrapped`) — do not confuse the two; this module's switch is the authoritative one for predicates.
- **Version churn**: `QueryFlags` must stay bit-compatible with
  `packages/core/src/render3/interfaces/query.ts` (`TQueryFlags`). The signal-query instructions
  (`ɵɵviewQuerySignal`, `ɵɵcontentQuerySignal`, `ɵɵqueryAdvance`) and the lazy/advance update model are
  relatively recent (signal queries era). Treat the instruction names as a versioned table tied to the
  pinned Angular `@angular/core` runtime; mismatched runtime = broken emit.
- **`name` optional**: when absent, the function is anonymous (`null` name) — used for inline/JIT cases.

---

## 8. Port plan (Rust / OXC)

### Strategy
This module is a **pure IR transform**: `R3QueryMetadata[] -> output_ast function expression`.
It does **not** build `oxc_ast` directly, so it should be ported on top of the project's already-
ported `output_ast` IR layer, NOT on OXC `AstBuilder`. OXC enters only later when `output_ast` is
lowered to `oxc_ast` and emitted via `oxc_codegen`. So this module's port depends on the IR builder
being available, not on OXC primitives directly.

### What to reuse from OXC
- Nothing directly here. Reuse comes transitively: the `output_ast` lowering layer uses
  `oxc_ast::AstBuilder` (for `BinaryExpression` `&` / `&&`, `CallExpression`, `FunctionExpression`,
  `IfStatement`, `AssignmentExpression`, `MemberExpression`) and `oxc_codegen::Codegen` for printing.
- `bitflags` crate (optional) for `QueryFlags`.

### Implementation steps (ordered)
1. Port `QueryFlags` (bitflags newtype) and `toQueryFlags`.
2. Port `getQueryPredicate` — needs `ConstantPool.get_const_literal(expr, force_shared: bool)`,
   `output_ast` `literal`, `literal_arr`, `import_expr`, and `R3` identifier table for `resolveForwardRef`.
   String split/trim is trivial in Rust (`s.split(',').map(str::trim)`).
3. Port `getQueryCreateParameters` (`ReadPropExpr`, `variable`, ordering logic, optional prepend).
4. Port `collapseAdvanceStatements` using the `UpdateStmt` enum from §3.5 (reverse iterate, `VecDeque`
   front-insert or build reversed then reverse, to mirror `unshift`).
5. Port `renderFlagCheckIfStmt` (`if_stmt(variable(rf).bitwise_and(literal(flags)), stmts)`).
6. Port `temporaryAllocator` interaction (it lives in `view/util` — ensure it's ported first; closure-
   based statement pusher → in Rust pass `&mut Vec<UpdateStmt>` or a callback).
7. Port `createViewQueriesFunction` and `createContentQueriesFunction` (share a private helper to avoid
   duplication — the only deltas are: prepend `dirIndex`, the third FnParam, the instruction ids, and the
   name suffix; factor these into parameters).
8. Snapshot-test against Angular golden output for: legacy view, signal view, legacy content, signal
   content, mixed legacy+signal (verify call chaining + signal-before-legacy ordering), multi-signal
   advance collapsing (verify `ɵɵqueryAdvance(2)`), `forwardRef`-wrapped predicate, string-array predicate
   with comma packing, `read` present/absent, `first` true/false.

### Complexity
**Low–medium.** Logic is small and self-contained (~300 LOC), no async, no recursion beyond a single
reverse loop. The only subtlety is the curried call-chaining accumulator and the advance-collapse
optimization — both must be byte-faithful. The real cost is upstream: it depends on a faithful
`output_ast` IR + `ConstantPool` + `R3` identifier table + `view/util` constants being ported first.

### Ordering vs other modules
Depends on (port these first):
- `output_ast` IR builder (foundational).
- `constant_pool` (`getConstLiteral` with shared flag).
- `r3_identifiers` (`R3` external reference table).
- `render3/util` (`ForwardRefHandling`, `MaybeForwardRefExpression`) and `render3/view/util`
  (`CONTEXT_NAME`, `RENDER_FLAGS`, `TEMPORARY_NAME`, `temporaryAllocator`).
- `core.RenderFlags` enum.
- `view/api` (`R3QueryMetadata`).

Consumed by: `render3/view/compiler.ts` (directive/component def assembly) — port this module before
`compiler.ts`'s query wiring.
