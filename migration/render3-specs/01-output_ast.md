# Port Spec 01 — `output/output_ast.ts`

**Source:** `packages/compiler/src/output/output_ast.ts`
**Angular version:** 22.1.0-next.0
**Target:** Rust + OXC (`oxc_ast` AstBuilder, `oxc_codegen` for emission)
**Spec status:** authoritative for the core output-AST enum.

---

## 1. Purpose & role in the compilation pipeline

`output_ast.ts` defines Angular's **language-agnostic Output AST** (universally aliased as `o.` in the codebase, e.g. `import * as o from '../output/output_ast'`). It is the **intermediate representation that every Angular code generator emits into**. It is *not* TypeScript's AST and *not* OXC's AST — it is a small, deliberately minimal tree of expressions, statements and types that downstream emitters lower into a concrete language.

Pipeline position:

```
  Template/Component metadata
        │  (render3 view/host compilers, partial-compile linker, etc.)
        ▼
  o.Expression / o.Statement / o.Type   ←── THIS MODULE (the IR)
        │
        ├─► output/ts_emitter.ts            → TypeScript source text
        ├─► output/abstract_emitter.ts      → generic source text + sourcemaps
        ├─► output/abstract_js_emitter.ts   → JS source text
        └─► (ngtsc) translator              → ts.Expression / ts.Statement (real TS AST)
```

Every `ɵɵ…` instruction call (e.g. `ɵɵelementStart`, `ɵɵproperty`) the render3 compiler produces is built as an `o.InvokeFunctionExpr` whose `fn` is an `o.ExternalExpr` referencing the instruction. So this module is the substrate on which **all** generated Angular code is constructed. In the Rust/OXC port this enum is the boundary: render3 modules build it; the OXC-backed emitter consumes it and produces `oxc_ast` nodes that `oxc_codegen` prints.

Key design properties to preserve:
- **Visitor-based** double-dispatch (`visitExpression`/`visitStatement`/`visitType`).
- Every node carries an optional `type: Type | null`, `sourceSpan: ParseSourceSpan | null`, and `leadingComments?: LeadingComment[]`.
- Structural equality (`isEquivalent`) that **ignores** types, source spans and (for some nodes) function bodies/arguments — used for constant-pool deduplication.
- `isConstant()` predicate — used to decide what can be hoisted into the constant pool.
- `clone()` — deep-ish clone (note: function/arrow bodies are intentionally *not* deep-cloned; see gotchas).
- Fluent builder methods on `Expression` (`.prop()`, `.callFn()`, `.equals()`, etc.).

---

## 2. Public API — exported functions, classes, enums, consts

### 2.1 Enums

```ts
export enum TypeModifier { None = 0, Const = 1 << 0 }

export enum BuiltinTypeName {
  Dynamic, Bool, String, Int, Number, Function, Inferred, None,
}

export enum UnaryOperator { Minus, Plus }

export enum BinaryOperator {
  Equals, NotEquals, Assign, Identical, NotIdentical, Minus, Plus, Divide,
  Multiply, Modulo, And, Or, BitwiseOr, BitwiseAnd, Lower, LowerEquals,
  Bigger, BiggerEquals, NullishCoalesce, Exponentiation, In, InstanceOf,
  AdditionAssignment, SubtractionAssignment, MultiplicationAssignment,
  DivisionAssignment, RemainderAssignment, ExponentiationAssignment,
  AndAssignment, OrAssignment, NullishCoalesceAssignment,
}

export enum StmtModifier { None = 0, Final = 1<<0, Private = 1<<1, Exported = 1<<2, Static = 1<<3 }

export const enum JSDocTagName { Desc='desc', Id='id', Meaning='meaning', Suppress='suppress' }
```

### 2.2 Type classes (the `o.Type` hierarchy)

```ts
export abstract class Type {
  constructor(public modifiers: TypeModifier = TypeModifier.None)
  abstract visitType(visitor: TypeVisitor, context: any): any
  hasModifier(modifier: TypeModifier): boolean
}
export class BuiltinType extends Type { constructor(public name: BuiltinTypeName, modifiers?) }
export class ExpressionType extends Type {
  constructor(public value: Expression, modifiers?, public typeParams: Type[] | null = null)
}
export class ArrayType extends Type { constructor(public of: Type, modifiers?) }
export class MapType extends Type { public valueType: Type | null; constructor(valueType, modifiers?) }
export class TransplantedType<T> extends Type { constructor(readonly type: T, modifiers?) }

export interface TypeVisitor {
  visitBuiltinType(type, context): any
  visitExpressionType(type, context): any
  visitArrayType(type, context): any
  visitMapType(type, context): any
  visitTransplantedType(type, context): any
}
```

Singleton type consts:
```ts
export const DYNAMIC_TYPE, INFERRED_TYPE, BOOL_TYPE, INT_TYPE,
             NUMBER_TYPE, STRING_TYPE, FUNCTION_TYPE, NONE_TYPE: BuiltinType
```

### 2.3 Expression base class

```ts
export abstract class Expression {
  public type: Type | null;
  public sourceSpan: ParseSourceSpan | null;
  constructor(type: Type|null|undefined, sourceSpan?: ParseSourceSpan|null,
              public leadingComments?: LeadingComment[])
  abstract visitExpression(visitor: ExpressionVisitor, context: any): any
  abstract isEquivalent(e: Expression): boolean
  abstract isConstant(): boolean
  abstract clone(): Expression

  // Fluent builders (all return new nodes):
  prop(name, sourceSpan?): ReadPropExpr
  key(index, type?, sourceSpan?): ReadKeyExpr
  callFn(params, sourceSpan?, pure?, leadingComments?): InvokeFunctionExpr
  instantiate(params, type?, sourceSpan?, leadingComments?): InstantiateExpr
  conditional(trueCase, falseCase=null, sourceSpan?, leadingComments?): ConditionalExpr
  equals/notEquals/identical/notIdentical/minus/plus/divide/multiply/modulo/
    power/and/bitwiseOr/bitwiseAnd/or/lower/lowerEquals/bigger/biggerEquals/
    nullishCoalesce(rhs, sourceSpan?): BinaryOperatorExpr
  isBlank(sourceSpan?): Expression          // == this.equals(TYPED_NULL_EXPR)
  toStmt(leadingComments?): Statement        // wraps in ExpressionStatement
}
```

### 2.4 Free functions (factory + helpers)

```ts
export function nullSafeIsEquivalent<T extends {isEquivalent(other:T):boolean}>(base:T|null, other:T|null)
export function areAllEquivalent<T extends {isEquivalent(other:T):boolean}>(base:T[], other:T[])

export function leadingComment(text, multiline=false, trailingNewline=true): LeadingComment
export function jsDocComment(tags: JSDocTag[] = []): JSDocComment
export function variable(name, type?, sourceSpan?, leadingComments?): ReadVarExpr
export function importExpr(id: ExternalReference, typeParams=null, sourceSpan?): ExternalExpr
export function importType(id, typeParams?, typeModifiers?): ExpressionType | null
export function expressionType(expr, typeModifiers?, typeParams?): ExpressionType
export function transplantedType<T>(type: T, typeModifiers?): TransplantedType<T>
export function typeofExpr(expr): TypeofExpr
export function literalArr(values, type?, sourceSpan?): LiteralArrayExpr
export function literalMap(values: {key,quoted,value}[], type=null): LiteralMapExpr
export function unary(operator, expr, type?, sourceSpan?): UnaryOperatorExpr
export function not(expr, sourceSpan?): NotExpr
export function fn(params, body, type?, sourceSpan?, name?): FunctionExpr
export function arrowFn(params, body, type?, sourceSpan?): ArrowFunctionExpr
export function ifStmt(condition, thenClause, elseClause?, sourceSpan?, leadingComments?): IfStmt
export function taggedTemplate(tag, template, type?, sourceSpan?): TaggedTemplateLiteralExpr
export function literal(value, type?, sourceSpan?): LiteralExpr
export function localizedString(metaBlock, messageParts, placeholderNames, expressions, sourceSpan?): LocalizedString
export function isNull(exp): boolean        // exp is LiteralExpr with value === null
```

Exported consts:
```ts
export const NULL_EXPR = new LiteralExpr(null, null, null)
export const TYPED_NULL_EXPR = new LiteralExpr(null, INFERRED_TYPE, null)
```

### 2.5 Visitor interfaces + base visitor

```ts
export interface ExpressionVisitor { /* visit* per node, 28 methods — see §3.4 */ }
export interface StatementVisitor {
  visitDeclareVarStmt, visitDeclareFunctionStmt, visitExpressionStmt, visitReturnStmt, visitIfStmt
}
export class RecursiveAstVisitor implements StatementVisitor, ExpressionVisitor { ... }
```

---

## 3. Key data structures & **proposed Rust mapping**

### 3.0 Mapping strategy overview

Angular models nodes as a **class hierarchy with double-dispatch visitors**. The idiomatic Rust port is a pair of **`enum`s** (`Expr`, `Stmt`, `Type`) with one variant per leaf class, plus visitor traits where needed. Recommendation:

- Use **owned, heap-allocated children via `Box<Expr>` / `Vec<Expr>`** for the IR enum. Do **not** force OXC's bump-arena `'a` lifetime onto this IR — this enum is built incrementally by render3 codegen (which mutates, hoists into constant pools, clones, and dedups), and arena allocation makes mutation/cloning awkward. Keep this IR arena-free and only convert to `oxc_ast` (`&'a` arena nodes) at the final emit step.
- Represent the common fields (`type`, `source_span`, `leading_comments`) either on each variant or via a wrapper struct `Node<T>`. Recommended: a thin `ExprNode { kind: ExprKind, ty: Option<Box<Type>>, span: Option<ParseSourceSpan>, leading_comments: Vec<LeadingComment> }`. Below uses inline-fields-per-variant for clarity; either is acceptable.

```rust
// Common metadata shared by every expression.
pub struct ExprMeta {
    pub ty: Option<Box<Type>>,
    pub span: Option<ParseSourceSpan>,
    pub leading_comments: Vec<LeadingComment>,
}
```

### 3.1 Types

| TS class | Fields | Rust |
|---|---|---|
| `Type` (abstract) | `modifiers: TypeModifier` | base data folded into `Type` enum + `modifiers: TypeModifier` field |
| `BuiltinType` | `name: BuiltinTypeName` | `Type::Builtin(BuiltinTypeName)` |
| `ExpressionType` | `value: Expression`, `typeParams: Type[]\|null` | `Type::Expression { value: Box<Expr>, type_params: Option<Vec<Type>> }` |
| `ArrayType` | `of: Type` | `Type::Array(Box<Type>)` |
| `MapType` | `valueType: Type\|null` | `Type::Map(Option<Box<Type>>)` |
| `TransplantedType<T>` | `type: T` | `Type::Transplanted(WrappedNode)` — see WrappedNodeExpr note |

```rust
#[derive(Clone)]
pub enum Type {
    Builtin(BuiltinTypeName),
    Expression { value: Box<Expr>, type_params: Option<Vec<Type>>, modifiers: TypeModifier },
    Array { of: Box<Type>, modifiers: TypeModifier },
    Map { value_type: Option<Box<Type>>, modifiers: TypeModifier },
    Transplanted { node: WrappedNodeHandle, modifiers: TypeModifier },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BuiltinTypeName { Dynamic, Bool, String, Int, Number, Function, Inferred, None }

bitflags! { pub struct TypeModifier: u8 { const NONE = 0; const CONST = 1; } }
```
> `BuiltinType` carries `modifiers` like every other type. Store `modifiers` per variant (or hoist Type into a `{ kind, modifiers }` struct). Singletons `DYNAMIC_TYPE`…`NONE_TYPE` become `const fn` constructors or `pub const` values.

### 3.2 Expressions (the central enum)

Each row = one `ExpressionVisitor` leaf. Fields quoted from source.

| TS class | Fields (besides type/span/comments) | Rust variant |
|---|---|---|
| `ReadVarExpr` | `name: string` | `ReadVar { name: String }` |
| `TypeofExpr` | `expr: Expression` | `Typeof(Box<Expr>)` |
| `VoidExpr` | `expr: Expression` | `Void(Box<Expr>)` |
| `WrappedNodeExpr<T>` | `node: T` | `WrappedNode(WrappedNodeHandle)` |
| `InvokeFunctionExpr` | `fn: Expression`, `args: Expression[]`, `pure=false`, `isOptional=false`; getter `receiver→fn` | `Invoke { callee: Box<Expr>, args: Vec<Expr>, pure: bool, optional: bool }` |
| `TaggedTemplateLiteralExpr` | `tag: Expression`, `template: TemplateLiteralExpr` | `TaggedTemplate { tag: Box<Expr>, template: TemplateLiteral }` |
| `InstantiateExpr` | `classExpr: Expression`, `args: Expression[]` | `New { class_expr: Box<Expr>, args: Vec<Expr> }` |
| `RegularExpressionLiteralExpr` | `body: string`, `flags: string\|null` | `RegExpLiteral { body: String, flags: Option<String> }` |
| `LiteralExpr` | `value: number\|string\|boolean\|null\|undefined` | `Literal(LiteralValue)` |
| `TemplateLiteralExpr` | `elements: TemplateLiteralElementExpr[]`, `expressions: Expression[]` | `TemplateLiteral { elements: Vec<TemplateLiteralElement>, expressions: Vec<Expr> }` |
| `TemplateLiteralElementExpr` | `text: string`, `rawText: string` (derived) | `TemplateLiteralElement { text: String, raw_text: String }` (own struct, also an Expr variant) |
| `LocalizedString` | `metaBlock: I18nMeta`, `messageParts: LiteralPiece[]`, `placeHolderNames: PlaceholderPiece[]`, `expressions: Expression[]` | `LocalizedString { meta: I18nMeta, message_parts: Vec<LiteralPiece>, placeholders: Vec<PlaceholderPiece>, expressions: Vec<Expr> }` |
| `ExternalExpr` | `value: ExternalReference`, `typeParams: Type[]\|null` | `External { value: ExternalReference, type_params: Option<Vec<Type>> }` |
| `ConditionalExpr` | `condition: Expression`, `trueCase: Expression`, `falseCase: Expression\|null` | `Conditional { condition, true_case, false_case: Option<Box<Expr>> }` |
| `DynamicImportExpr` | `url: string\|Expression`, `urlComment?: string` | `DynamicImport { url: ImportUrl, url_comment: Option<String> }` where `ImportUrl { Str(String), Expr(Box<Expr>) }` |
| `NotExpr` | `condition: Expression` | `Not(Box<Expr>)` |
| `FunctionExpr` | `params: FnParam[]`, `statements: Statement[]`, `name?: string\|null` | `Function { params: Vec<FnParam>, statements: Vec<Stmt>, name: Option<String> }` |
| `ArrowFunctionExpr` | `params: FnParam[]`, `body: Expression \| Statement[]` | `Arrow { params: Vec<FnParam>, body: ArrowBody }` where `ArrowBody { Expr(Box<Expr>), Block(Vec<Stmt>) }` |
| `UnaryOperatorExpr` | `operator: UnaryOperator`, `expr: Expression`, `parens=true` | `Unary { op: UnaryOperator, expr: Box<Expr>, parens: bool }` |
| `ParenthesizedExpr` | `expr: Expression` | `Parenthesized(Box<Expr>)` |
| `BinaryOperatorExpr` | `operator: BinaryOperator`, `lhs: Expression`, `rhs: Expression`; method `isAssignment()` | `Binary { op: BinaryOperator, lhs: Box<Expr>, rhs: Box<Expr> }` |
| `ReadPropExpr` | `receiver: Expression`, `name: string`, `isOptional=false`; getter `index→name` | `ReadProp { receiver: Box<Expr>, name: String, optional: bool }` |
| `ReadKeyExpr` | `receiver: Expression`, `index: Expression`, `isOptional=false` | `ReadKey { receiver: Box<Expr>, index: Box<Expr>, optional: bool }` |
| `LiteralArrayExpr` | `entries: Expression[]` | `LiteralArray(Vec<Expr>)` |
| `LiteralMapExpr` | `entries: LiteralMapEntry[]`, `valueType: Type\|null` | `LiteralMap { entries: Vec<LiteralMapEntry>, value_type: Option<Box<Type>> }` |
| `CommaExpr` | `parts: Expression[]` | `Comma(Vec<Expr>)` |
| `SpreadElementExpr` | `expression: Expression` | `Spread(Box<Expr>)` |

```rust
#[derive(Clone)]
pub struct Expr { pub kind: ExprKind, pub meta: ExprMeta }

#[derive(Clone)]
pub enum ExprKind {
    ReadVar { name: String },
    Typeof(Box<Expr>),
    Void(Box<Expr>),
    WrappedNode(WrappedNodeHandle),
    Invoke { callee: Box<Expr>, args: Vec<Expr>, pure: bool, optional: bool },
    TaggedTemplate { tag: Box<Expr>, template: TemplateLiteral },
    New { class_expr: Box<Expr>, args: Vec<Expr> },
    RegExpLiteral { body: String, flags: Option<String> },
    Literal(LiteralValue),
    TemplateLiteral { elements: Vec<TemplateLiteralElement>, expressions: Vec<Expr> },
    TemplateLiteralElement(TemplateLiteralElement),
    LocalizedString { meta: I18nMeta, message_parts: Vec<LiteralPiece>,
                      placeholders: Vec<PlaceholderPiece>, expressions: Vec<Expr> },
    External { value: ExternalReference, type_params: Option<Vec<Type>> },
    Conditional { condition: Box<Expr>, true_case: Box<Expr>, false_case: Option<Box<Expr>> },
    DynamicImport { url: ImportUrl, url_comment: Option<String> },
    Not(Box<Expr>),
    Function { params: Vec<FnParam>, statements: Vec<Stmt>, name: Option<String> },
    Arrow { params: Vec<FnParam>, body: ArrowBody },
    Unary { op: UnaryOperator, expr: Box<Expr>, parens: bool },
    Parenthesized(Box<Expr>),
    Binary { op: BinaryOperator, lhs: Box<Expr>, rhs: Box<Expr> },
    ReadProp { receiver: Box<Expr>, name: String, optional: bool },
    ReadKey { receiver: Box<Expr>, index: Box<Expr>, optional: bool },
    LiteralArray(Vec<Expr>),
    LiteralMap { entries: Vec<LiteralMapEntry>, value_type: Option<Box<Type>> },
    Comma(Vec<Expr>),
    Spread(Box<Expr>),
}

#[derive(Clone, PartialEq)]
pub enum LiteralValue { Number(f64), String(String), Bool(bool), Null, Undefined }

pub enum ArrowBody { Expr(Box<Expr>), Block(Vec<Stmt>) }
pub enum ImportUrl { Str(String), Expr(Box<Expr>) }
```

Supporting structs:
```rust
pub struct FnParam { pub name: String, pub ty: Option<Type> }                 // FnParam
pub struct ExternalReference { pub module_name: Option<String>, pub name: Option<String> }
pub struct TemplateLiteralElement { pub text: String, pub raw_text: String }  // raw_text derived if absent

pub enum LiteralMapEntry {                                                    // LiteralMapEntry union
    Property { key: String, value: Expr, quoted: bool },                      // LiteralMapPropertyAssignment
    Spread { expression: Expr },                                              // LiteralMapSpreadAssignment
}

pub struct LiteralPiece { pub text: String, pub source_span: ParseSourceSpan }
pub struct PlaceholderPiece { pub text: String, pub source_span: ParseSourceSpan,
                              pub associated_message: Option<Rc<Message>> }
pub struct CookedRawString { pub cooked: String, pub raw: String, pub range: Option<ParseSourceSpan> }
```

> **`WrappedNodeExpr<T>` / `TransplantedType<T>`** wrap a *foreign* node (a `ts.Expression`/`ts.Node` in ngtsc, or any host AST). In the OXC port the natural `T` is an `oxc_ast` node reference. Because the IR is arena-free but oxc nodes are arena-bound, model the handle as an index/opaque token into a side table (`WrappedNodeHandle = u32` index into a `Vec<&'a oxc_ast::Expression<'a>>`), resolved only at emit. This avoids threading `'a` through the whole IR. The `isEquivalent` for these uses **reference identity** (`this.node === e.node`), so the handle must preserve identity — store indices, compare indices.

### 3.3 Statements

| TS class | Fields | Rust |
|---|---|---|
| `Statement` (abstract) | `modifiers: StmtModifier`, `sourceSpan`, `leadingComments?` | `StmtMeta` shared struct |
| `DeclareVarStmt` | `name: string`, `value?: Expression`, `type: Type\|null` | `DeclareVar { name: String, value: Option<Expr>, ty: Option<Type> }` |
| `DeclareFunctionStmt` | `name: string`, `params: FnParam[]`, `statements: Statement[]`, `type: Type\|null` | `DeclareFunction { name, params: Vec<FnParam>, statements: Vec<Stmt>, ty: Option<Type> }` |
| `ExpressionStatement` | `expr: Expression` | `Expression(Expr)` |
| `ReturnStatement` | `value: Expression` | `Return(Expr)` |
| `IfStmt` | `condition: Expression`, `trueCase: Statement[]`, `falseCase: Statement[]=[]` | `If { condition: Expr, true_case: Vec<Stmt>, false_case: Vec<Stmt> }` |

```rust
pub struct Stmt { pub kind: StmtKind, pub meta: StmtMeta }
pub struct StmtMeta { pub modifiers: StmtModifier, pub span: Option<ParseSourceSpan>,
                      pub leading_comments: Vec<LeadingComment> }

pub enum StmtKind {
    DeclareVar { name: String, value: Option<Expr>, ty: Option<Type> },
    DeclareFunction { name: String, params: Vec<FnParam>, statements: Vec<Stmt>, ty: Option<Type> },
    Expression(Expr),
    Return(Expr),
    If { condition: Expr, true_case: Vec<Stmt>, false_case: Vec<Stmt> },
}

bitflags! { pub struct StmtModifier: u8 {
    const NONE=0; const FINAL=1; const PRIVATE=2; const EXPORTED=4; const STATIC=8; } }
```

> **Note the small statement set.** There are only 5 statement kinds — no loops, no switch, no try/catch, no class declarations at this layer. Classes are expressed structurally (see ts_emitter/translator handling `DeclareFunctionStmt` + `FunctionExpr`). Keep the enum exactly this size; do not invent extra variants.

### 3.4 Comments & JSDoc

```ts
export class LeadingComment { constructor(public text, public multiline, public trailingNewline); toString() }
export class JSDocComment extends LeadingComment { constructor(public tags: JSDocTag[]); toString() }
export type JSDocTag = { tagName: JSDocTagName|string; text?: string } | { tagName?: undefined; text: string }
```

```rust
pub enum LeadingComment {
    Plain { text: String, multiline: bool, trailing_newline: bool },
    JsDoc { tags: Vec<JsDocTag> },   // multiline=true, trailing_newline=true implicitly
}
pub struct JsDocTag { pub tag_name: Option<String>, pub text: Option<String> }
```
`LeadingComment::to_string()` and `JsDocComment` serialization map to free functions `tag_to_string`/`serialize_tags` (see §6).

### 3.5 Operator enums

```rust
#[derive(Clone, Copy, PartialEq, Eq)] pub enum UnaryOperator { Minus, Plus }

#[derive(Clone, Copy, PartialEq, Eq)] pub enum BinaryOperator {
    Equals, NotEquals, Assign, Identical, NotIdentical, Minus, Plus, Divide, Multiply,
    Modulo, And, Or, BitwiseOr, BitwiseAnd, Lower, LowerEquals, Bigger, BiggerEquals,
    NullishCoalesce, Exponentiation, In, InstanceOf, AdditionAssignment, SubtractionAssignment,
    MultiplicationAssignment, DivisionAssignment, RemainderAssignment, ExponentiationAssignment,
    AndAssignment, OrAssignment, NullishCoalesceAssignment,
}
impl BinaryOperator {
    pub fn is_assignment(self) -> bool { matches!(self,
        Assign|AdditionAssignment|SubtractionAssignment|MultiplicationAssignment|
        DivisionAssignment|RemainderAssignment|ExponentiationAssignment|
        AndAssignment|OrAssignment|NullishCoalesceAssignment) }
}
```
> **`In` and `InstanceOf` exist in the enum but have NO fluent builder method on `Expression`** and are not produced by `BinaryOperatorExpr`'s helpers — they are constructed directly elsewhere. Keep them in the enum.

---

## 4. Algorithm walkthroughs (main behaviors to reproduce)

The module is mostly data + three cross-cutting algorithms. Reproduce each faithfully.

### 4.1 `isEquivalent(other)` — structural equality (per node)

Used by the **constant pool** to dedup expressions. Rules, per node, exactly as written:
- Same concrete variant **and** scalar fields equal **and** child expressions recursively `isEquivalent`.
- **Ignores** `type`, `sourceSpan`, `leadingComments`.
- `InvokeFunctionExpr`: also compares `pure` (but **not** args' optional flag, **not** `isOptional`).
- `ReadPropExpr`/`ReadKeyExpr`: also compare `isOptional`.
- `FunctionExpr.isEquivalent`: matches `FunctionExpr` **or** `DeclareFunctionStmt`, compares params + statements.
- `LocalizedString.isEquivalent` **always returns `false`** (commented-out impl). Reproduce verbatim.
- `WrappedNodeExpr`: reference identity on `node`.
- `ExternalExpr`: compares `value.name` and `value.moduleName` only.
- Array comparisons go through `areAllEquivalent` (length check + pairwise).
- `nullSafeIsEquivalent`: both null → equal; one null → not equal; else delegate.

Rust: implement as a method `Expr::is_equivalent(&self, other: &Expr) -> bool` matching variant pairs. `LiteralExpr` uses `===` (strict) — map `LiteralValue` `PartialEq`, but beware `NaN`/`-0` and `null` vs `undefined` distinctions (see gotchas).

### 4.2 `isConstant()` — hoistability predicate

- Literals (`LiteralExpr`, `RegularExpressionLiteralExpr`, `TemplateLiteralElementExpr`): `true`.
- `LiteralArrayExpr`: `entries.every(isConstant)`.
- `LiteralMapExpr`: `entries.every(isConstant)` (each `LiteralMapEntry` has its own `isConstant`).
- `TypeofExpr`/`VoidExpr`/`ParenthesizedExpr`/`SpreadElementExpr`: delegate to inner expr.
- Everything else (`ReadVarExpr`, `Invoke`, `Conditional`, `Binary`, `Function`, `Arrow`, `External`, `LocalizedString`, `TemplateLiteral`, `Comma`, `WrappedNode`, `New`, `DynamicImport`, `Not`, `Unary`, `ReadProp`, `ReadKey`, `ReadVar`, `TaggedTemplate`): `false`.

### 4.3 `clone()` — structural copy

- Recursively clones child expressions/types.
- **Drops `leadingComments`** (passes `[]` or omits) and frequently drops `type`/`sourceSpan` on inner rebuilds (varies per node — follow each constructor call exactly).
- `FunctionExpr.clone()` and `ArrowFunctionExpr.clone()`: **do NOT deep-clone statements** (comment: `// TODO: Should we deep clone statements?`). Body statements are shared by reference. Reproduce this (or document the intentional divergence) — Rust will be forced to actually clone `Vec<Stmt>` unless you use `Rc<[Stmt]>`. **Recommended:** clone the Vec (semantically equivalent, no shared mutation in this IR) and note the divergence.
- `DynamicImportExpr.clone()`: clones `url` only if it is an `Expression`.

### 4.4 `RecursiveAstVisitor` — default traversal

Implements both `StatementVisitor` and `ExpressionVisitor`. For each node: visits children first (calling `child.visitExpression(this, ctx)` / `visitAllStatements`), then calls `this.visitExpression(node, ctx)` which in turn visits `node.type` if present. Note quirks to preserve:
- `visitConditionalExpr` uses `ast.falseCase!` (non-null assertion) — it will throw if `falseCase` is null. In Rust, guard with `if let Some(fc)`.
- `visitExpression` visits `ast.type` via `type.visitType(this, ctx)`.
- `visitTransplantedType` returns the node without recursing.

Rust: a `trait Visitor { fn visit_expr(...); ... }` with a default `walk_expr`/`walk_stmt` free function (the OXC-idiomatic split of visit vs walk). Provide default no-op visitor and a `RecursiveVisitor` equivalent.

### 4.5 `LocalizedString` serialization (i18n `$localize`)

`serializeI18nHead()` and `serializeI18nTemplatePart(i)` build `CookedRawString` for `$localize` tagged templates. Constants: `MEANING_SEPARATOR='|'`, `ID_SEPARATOR='@@'`, `LEGACY_ID_INDICATOR='␟'` (U+241F). Escaping helpers (`escapeSlashes`, `escapeStartingColon`, `escapeColons`, `escapeForTemplateLiteral`) + `createCookedRawString`. `serializeI18nTemplatePart` calls `computeMsgId(...)` from `../i18n/digest`. This subsystem is i18n-specific; can be **deferred** in the port (stub returning the cooked/raw without legacy-id handling) unless `$localize` output is in scope.

---

## 5. Dependencies on other compiler modules

Imports at top of file:
```ts
import {computeMsgId} from '../i18n/digest';          // used only by LocalizedString
import {Message} from '../i18n/i18n_ast';             // PlaceholderPiece.associatedMessage type
import {ParseSourceSpan} from '../parse_util';        // span on every node
import type {I18nMeta} from '../render3/view/i18n/meta'; // LocalizedString.metaBlock (type-only)
```

**Downstream consumers (who build/consume this IR)** — not imported here but essential context:
- `output/abstract_emitter.ts`, `output/abstract_js_emitter.ts`, `output/ts_emitter.ts` — the emitters (next specs).
- `render3/view/*`, `render3/r3_*` — the instruction generators that build `o.*`.
- `constant_pool.ts` — uses `isEquivalent` / `isConstant` for dedup.
- ngtsc `translator` — converts `o.*` to real `ts.*`.

Port ordering implication: **this module has almost no inbound dependencies** (only `ParseSourceSpan`, plus i18n types that can be stubbed). It is the natural **first** module to port. `ParseSourceSpan` must be ported (or stubbed) first.

---

## 6. `ɵɵ` instructions / output emitted

**This module emits NO `ɵɵ` instructions and produces no source text.** It is pure data + helpers. The `ɵɵ…` runtime instruction *calls* are constructed *using* this module (as `InvokeFunctionExpr` over `ExternalExpr`) by the render3 view/host compilers, and *printed* by the emitters — neither lives here.

The only "output" produced by this file is **JSDoc/comment string serialization**:
- `LeadingComment.toString()` → `` ` ${text} ` `` if multiline else `text`.
- `JSDocComment.toString()` → `serializeTags(tags)`.
- `tagToString(tag)` → e.g. `" @param {x} y"`; **throws** `Error('JSDoc text cannot contain "/*" and "*/"')` if text contains `/*` or `*/`; escapes `@` → `\@`.
- `serializeTags(tags)`:
  - `[]` → `''`.
  - single tag with `tagName` and no `text` → `` `*${tagToString(tag)} ` `` (single-line `/** @x */` form).
  - else multi-line `*\n` + per-tag ` *…\n` + trailing ` `.
- `createCookedRawString(metaBlock, messagePart, range)` → `$localize` cooked/raw pair with the escaping rules in §4.5.

Reproduce these string builders **byte-for-byte** — emitter golden tests depend on exact whitespace.

---

## 7. Edge cases, gotchas, version-sensitivity

1. **`LiteralExpr.value` is `number|string|boolean|null|undefined`.** JS conflates `null`/`undefined` and uses `===`. In Rust, model `LiteralValue::Null` and `LiteralValue::Undefined` distinctly; `isEquivalent` uses strict equality so `null !== undefined`. Numbers are JS doubles — store `f64` and beware `NaN`/`-0` (JS `===` treats `NaN !== NaN`, `-0 === 0`). Match `===` semantics, not Rust `PartialEq` for `f64` blindly.
2. **`isConstant()` does not exist for `RegularExpressionLiteralExpr`'s flags** — it's `true` regardless of body; fine.
3. **`LocalizedString.isEquivalent` always returns `false`** (intentional, commented-out body). Do not "fix" it.
4. **`FunctionExpr`/`ArrowFunctionExpr` `clone()` shares statement bodies (no deep clone).** Documented TODO in source. Decide explicitly in Rust (recommend deep clone; note divergence).
5. **`ConditionalExpr` constructor**: `super(type || trueCase.type, …)` — default type inherited from `trueCase`. `BinaryOperatorExpr` inherits `lhs.type`; `CommaExpr` inherits last part's type; `UnaryOperatorExpr` defaults to `NUMBER_TYPE`; `NotExpr` forces `BOOL_TYPE`; `TemplateLiteralElementExpr`/`LocalizedString` force `STRING_TYPE`. **Preserve these default-type rules** — emitters and type printers read `.type`.
6. **`RecursiveAstVisitor.visitConditionalExpr` non-null asserts `falseCase`** — will crash on null. Guard in Rust.
7. **`ArrowFunctionExpr.body` is a union** `Expression | Statement[]`. `Array.isArray` discrimination → Rust `ArrowBody` enum. `isEquivalent` returns false if the two bodies are of different shapes.
8. **`InvokeFunctionExpr` has `isOptional` (8th ctor arg) but `isEquivalent` ignores it**, while `clone()` preserves it. Subtle asymmetry — replicate.
9. **`ReadPropExpr.set()` rebuilds via `this.receiver.prop(this.name)`** (a fresh `ReadPropExpr` then assignment) — not just wrapping `this`. Match exactly if porting builder methods.
10. **Getters `InvokeFunctionExpr.receiver` (→`fn`) and `ReadPropExpr.index` (→`name`)** exist so generic code can treat calls/prop-reads/key-reads uniformly. In Rust expose accessor methods (`fn receiver(&self)`), or normalize field names.
11. **`TemplateLiteralElementExpr.rawText`** is auto-derived when not provided via `escapeForTemplateLiteral(escapeSlashes(text))`. Comment warns the `sourceSpan` may be wrong (PR #60267) — don't use span to recover raw text.
12. **`In`/`InstanceOf` binary operators** have no builder helpers (see §3.5). **Assignment operators** (`AdditionAssignment`…`NullishCoalesceAssignment`) added relatively recently — version-sensitive; older Angular versions lack them. For 22.x they are present.
13. **`SpreadElementExpr`, `ParenthesizedExpr`, `RegularExpressionLiteralExpr`, `DynamicImportExpr`, `TaggedTemplateLiteralExpr`, `TemplateLiteralExpr`** are comparatively new additions to this AST (post-v17 surface area). The full visitor has **28** expression methods — Angular has churned this list across versions; pin to the v22.1 set above.
14. **`MapType.valueType`** normalizes `undefined`→`null` in ctor. `LiteralMapExpr.valueType` is copied from its `MapType` type if present.
15. **`StmtModifier`/`TypeModifier` are bitflags** — use `bitflags!`, support `hasModifier`.
16. **`WrappedNodeExpr`/`TransplantedType` reference-identity equality** — see §3.2 handle note.

---

## 8. Port plan (Rust / OXC)

### 8.1 What to build
- A standalone crate/module `output_ast` containing:
  - `Type` enum + `BuiltinTypeName`, `TypeModifier` bitflags + builtin-type consts.
  - `Expr`/`ExprKind`, `Stmt`/`StmtKind`, support structs (`FnParam`, `ExternalReference`, `LiteralMapEntry`, `TemplateLiteralElement`, `LiteralPiece`, `PlaceholderPiece`, `CookedRawString`).
  - `UnaryOperator`, `BinaryOperator` (+`is_assignment`), `StmtModifier`.
  - `LeadingComment`/`JsDocTag` + `serialize_tags`/`tag_to_string`.
  - Methods `is_equivalent`, `is_constant`, `clone` (derive `Clone` + custom where needed for body-sharing semantics), and the fluent builders as inherent methods on `Expr`.
  - Free factory fns (`variable`, `literal`, `import_expr`, `literal_arr`, `literal_map`, `fn_`, `arrow_fn`, `if_stmt`, etc.) — direct 1:1 with §2.4.
  - A `Visitor` trait + `walk_*` defaults replicating `RecursiveAstVisitor`.

### 8.2 What to reuse from OXC
- **Nothing in the IR itself** — keep this IR arena-free and OXC-independent. OXC's AST is the *target*, reached only in the emitter module (next spec). The benefit: render3 builders can mutate/clone/hoist freely without bump-arena friction.
- At emit time, a separate `to_oxc` lowering will map each `ExprKind`/`StmtKind` to `oxc_ast::ast::Expression`/`Statement` via `oxc_ast::AstBuilder`, then `oxc_codegen::Codegen` prints. Map operator enums to OXC's `BinaryOperator`/`UnaryOperator`/`AssignmentOperator`/`LogicalOperator` (note: OXC splits logical (`&&`,`||`,`??`) and assignment operators into separate enums — Angular lumps them; build a conversion table). `WrappedNodeExpr` resolves its handle to a borrowed `&'a oxc` node and is spliced directly.
- Reuse OXC's string-escaping/codegen for string and template literals where possible, but Angular's `$localize` cooked/raw and `rawText` escaping are bespoke — port the helpers directly.

### 8.3 Complexity
**Low–medium.** It is mostly mechanical data definition. Sources of friction:
- Faithful `LiteralValue` `===` semantics (NaN/-0/null/undefined).
- `WrappedNode`/`Transplanted` handle indirection.
- `clone` body-sharing decision.
- Exact JSDoc/`$localize` string serialization (golden-test sensitive).
- The arrow-body union and the type-default rules.

Estimate: ~1–1.5k lines of Rust; 1–2 days including unit tests mirroring Angular's `output_ast` specs.

### 8.4 Ordering vs other modules
**Port FIRST** (after a tiny `ParseSourceSpan` + i18n type stubs). Rationale: zero meaningful inbound deps; *everything* downstream (emitters, constant pool, all render3 instruction generators) depends on this enum. Suggested order:
1. `parse_util` (`ParseSourceSpan`) stub/port.
2. **`output_ast` (this module).**
3. `output/abstract_emitter` + `output/abstract_js_emitter` + `output/ts_emitter` (consume this IR; produce text) — or the OXC `to_oxc` lowering as the modern replacement.
4. `constant_pool` (uses `is_equivalent`/`is_constant`).
5. render3 `r3_*` / view compilers (build `o.*`).

Defer (stub) until needed: `LocalizedString` i18n serialization (`computeMsgId`, legacy ids) — only required when `$localize` output is in scope.
