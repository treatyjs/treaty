# Port Spec 04 — `expression_parser/ast.ts`

**Source:** `packages/compiler/src/expression_parser/ast.ts`
**Angular version:** 22.1.0-next.0
**Target:** Rust, using the OXC toolchain (`oxc_ast` `AstBuilder` + `oxc_codegen`) for the downstream emission stages. This module itself is pure data + a visitor; it does not touch OXC directly, but its node shapes drive what the later "output AST → OXC" lowering must produce.

---

## 1. Purpose & role in the compilation pipeline

This file defines the **Angular template expression AST** — the typed tree produced when Angular parses the *expression* fragments embedded inside templates: interpolations (`{{ a + b }}`), property/event bindings (`[x]="…"`, `(y)="…"`), two-way bindings, and `*`-microsyntax (`*ngFor="let item of items"`).

It is **not** the TypeScript AST and **not** Angular's "output AST" (`output/output_ast.ts`, the IR that is finally emitted as JS via OXC in this port). It is the intermediate semantic model of the *small expression language* that Angular allows in templates. The pipeline position is:

```
template text
   │  lexer.ts          → tokens
   ▼
parser.ts (expression_parser/parser.ts)   ← produces these AST nodes
   │
   ▼  ast.ts  (THIS MODULE: the node definitions + visitors)
   │
   ▼  template parser / R3 AST (render3/r3_ast.ts) embeds these as binding values
   │
   ▼  template_binding / expression lowering → output_ast.ts
   │
   ▼  output → OXC oxc_ast nodes → oxc_codegen → JS string
```

Concretely this module provides three things:
1. **The node classes** (`Binary`, `Conditional`, `PropertyRead`, `Call`, `BindingPipe`, …) — the closed set of expression node types.
2. **The visitor contract** (`AstVisitor` interface + `RecursiveAstVisitor` base) used by every consumer that walks expressions (type-checking, i18n, the template compiler's expression converter, lint/language-service tooling).
3. **Binding metadata wrappers** (`ParsedProperty`, `ParsedEvent`, `ParsedVariable`, `BoundElementProperty`, `TemplateBinding`) that pair an expression with its source span and binding kind.

Role for the port: this is a **foundational, dependency-light module**. It should be ported early because `parser.ts`, the R3 template AST, and the expression-to-output converter all reference these types.

---

## 2. Public API — every export with full TypeScript signatures

### Spans
```ts
export class ParseSpan {
  constructor(public start: number, public end: number) {}
  toAbsolute(absoluteOffset: number): AbsoluteSourceSpan;
}

export class AbsoluteSourceSpan {
  constructor(public readonly start: number, public readonly end: number) {}
}
```

### Abstract bases
```ts
export abstract class AST {
  constructor(public span: ParseSpan, public sourceSpan: AbsoluteSourceSpan) {}
  abstract visit(visitor: AstVisitor, context?: any): any;
  toString(): string; // returns 'AST'
}

export abstract class ASTWithName extends AST {
  constructor(span: ParseSpan, sourceSpan: AbsoluteSourceSpan, public nameSpan: AbsoluteSourceSpan);
}
```

### Concrete expression node classes (all `extends AST` unless noted)
```ts
export class EmptyExpr extends AST                                   // (no extra fields)
export class ImplicitReceiver extends AST                           // (no extra fields)
export class ThisReceiver extends ImplicitReceiver? // NOTE: declared `extends AST`, semantically a `this`
export class Chain extends AST                  { expressions: AST[] }
export class Conditional extends AST            { condition: AST; trueExp: AST; falseExp: AST }
export class PropertyRead extends ASTWithName   { receiver: AST; name: string }
export class SafePropertyRead extends ASTWithName { receiver: AST; name: string }
export class KeyedRead extends AST              { receiver: AST; key: AST }
export class SafeKeyedRead extends AST          { receiver: AST; key: AST }
export class BindingPipe extends ASTWithName    { exp: AST; name: string; args: AST[]; readonly type: BindingPipeType }
export class LiteralPrimitive extends AST       { value: string | number | boolean | null | undefined }
export class LiteralArray extends AST           { expressions: AST[] }
export class SpreadElement extends AST          { readonly expression: AST }
export class LiteralMap extends AST             { keys: LiteralMapKey[]; values: AST[] }
export class Interpolation extends AST          { strings: string[]; expressions: AST[] }
export class Binary extends AST                 { operation: BinaryOperation; left: AST; right: AST;
                                                  static isAssignmentOperation(op: string): op is AssignmentOperation }
export class Unary extends Binary               { operator: '+' | '-'; expr: AST;  // left/right/operation = never
                                                  static createMinus(span, sourceSpan, expr: AST): Unary
                                                  static createPlus(span, sourceSpan, expr: AST): Unary }
export class PrefixNot extends AST              { expression: AST }
export class TypeofExpression extends AST       { expression: AST }
export class VoidExpression extends AST         { expression: AST }
export class NonNullAssert extends AST          { expression: AST }
export class Call extends AST                   { receiver: AST; args: AST[]; argumentSpan: AbsoluteSourceSpan }
export class SafeCall extends AST               { receiver: AST; args: AST[]; argumentSpan: AbsoluteSourceSpan }
export class TaggedTemplateLiteral extends AST  { tag: AST; template: TemplateLiteral }
export class TemplateLiteral extends AST        { elements: TemplateLiteralElement[]; expressions: AST[] }
export class TemplateLiteralElement extends AST { text: string }
export class ParenthesizedExpression extends AST { expression: AST }
export class ArrowFunction extends AST          { parameters: ArrowFunctionParameter[]; body: AST }
export class RegularExpressionLiteral extends AST { readonly body: string; readonly flags: string | null }
export class ASTWithSource<T extends AST = AST> extends AST {
  constructor(public ast: T, public source: string | null, public location: string,
              absoluteOffset: number, public errors: ParseError[]);
}
```

### Arrow-function parameters
```ts
export class ArrowFunctionIdentifierParameter {
  constructor(public name: string, public span: ParseSpan, public sourceSpan: AbsoluteSourceSpan) {}
}
export type ArrowFunctionParameter = ArrowFunctionIdentifierParameter; // rest params TODO
```

### Literal-map key descriptors
```ts
export interface LiteralMapPropertyKey {
  kind: 'property'; key: string; quoted: boolean;
  span: ParseSpan; sourceSpan: AbsoluteSourceSpan; isShorthandInitialized?: boolean;
}
export interface LiteralMapSpreadKey { kind: 'spread'; span: ParseSpan; sourceSpan: AbsoluteSourceSpan }
export type LiteralMapKey = LiteralMapPropertyKey | LiteralMapSpreadKey;
```

### Operator string unions
```ts
export type AssignmentOperation = '=' | '+=' | '-=' | '*=' | '/=' | '%=' | '**=' | '&&=' | '||=' | '??=';
type BinaryOperation = AssignmentOperation
  | '&&' | '||' | '??'                         // logical
  | '==' | '!=' | '===' | '!=='                // equality
  | '<' | '>' | '<=' | '>=' | 'in' | 'instanceof'  // relational
  | '+' | '-'                                  // additive
  | '*' | '%' | '/'                            // multiplicative
  | '**';                                      // exponentiation
// NOTE: `BinaryOperation` is NOT exported; only `AssignmentOperation` is.
```

### Enums
```ts
export enum BindingPipeType { ReferencedByName, ReferencedDirectly }
export enum ParsedPropertyType { DEFAULT, LITERAL_ATTR, LEGACY_ANIMATION, TWO_WAY, ANIMATION }
export enum ParsedEventType { Regular, LegacyAnimation, TwoWay, Animation }
export enum BindingType { Property, Attribute, Class, Style, LegacyAnimation, TwoWay, Animation }
```

### Microsyntax / template bindings
```ts
export type TemplateBinding = VariableBinding | ExpressionBinding;
export class VariableBinding {
  constructor(readonly sourceSpan: AbsoluteSourceSpan,
              readonly key: TemplateBindingIdentifier,
              readonly value: TemplateBindingIdentifier | null) {}
}
export class ExpressionBinding {
  constructor(readonly sourceSpan: AbsoluteSourceSpan,
              readonly key: TemplateBindingIdentifier,
              readonly value: ASTWithSource | null) {}
}
export interface TemplateBindingIdentifier { source: string; span: AbsoluteSourceSpan }
```

### Visitor contract
```ts
export interface AstVisitor {
  visitUnary?(ast: Unary, context: any): any;          // optional (back-compat)
  visitBinary(ast: Binary, context: any): any;
  visitChain(ast: Chain, context: any): any;
  visitConditional(ast: Conditional, context: any): any;
  visitThisReceiver?(ast: ThisReceiver, context: any): any;  // optional (back-compat)
  visitImplicitReceiver(ast: ImplicitReceiver, context: any): any;
  visitInterpolation(ast: Interpolation, context: any): any;
  visitKeyedRead(ast: KeyedRead, context: any): any;
  visitLiteralArray(ast: LiteralArray, context: any): any;
  visitLiteralMap(ast: LiteralMap, context: any): any;
  visitLiteralPrimitive(ast: LiteralPrimitive, context: any): any;
  visitPipe(ast: BindingPipe, context: any): any;
  visitPrefixNot(ast: PrefixNot, context: any): any;
  visitTypeofExpression(ast: TypeofExpression, context: any): any;
  visitVoidExpression(ast: TypeofExpression, context: any): any;  // NOTE: param typed TypeofExpression (Angular typo)
  visitNonNullAssert(ast: NonNullAssert, context: any): any;
  visitPropertyRead(ast: PropertyRead, context: any): any;
  visitSafePropertyRead(ast: SafePropertyRead, context: any): any;
  visitSafeKeyedRead(ast: SafeKeyedRead, context: any): any;
  visitCall(ast: Call, context: any): any;
  visitSafeCall(ast: SafeCall, context: any): any;
  visitTemplateLiteral(ast: TemplateLiteral, context: any): any;
  visitTemplateLiteralElement(ast: TemplateLiteralElement, context: any): any;
  visitTaggedTemplateLiteral(ast: TaggedTemplateLiteral, context: any): any;
  visitParenthesizedExpression(ast: ParenthesizedExpression, context: any): any;
  visitArrowFunction(ast: ArrowFunction, context: any): any;
  visitRegularExpressionLiteral(ast: RegularExpressionLiteral, context: any): any;
  visitSpreadElement(ast: SpreadElement, context: any): any;
  visitASTWithSource?(ast: ASTWithSource, context: any): any;  // optional
  visitEmptyExpr?(ast: EmptyExpr, context: any): any;          // optional
  visit?(ast: AST, context?: any): any;                        // optional gate
}

export class RecursiveAstVisitor implements AstVisitor {
  visit(ast: AST, context?: any): any;
  // …implements every visitX by recursing into children…
  visitAll(asts: AST[], context: any): any;   // helper, not part of interface
}
```

### Binding metadata wrappers
```ts
export class ParsedProperty {
  readonly isLiteral: boolean; readonly isLegacyAnimation: boolean; readonly isAnimation: boolean;
  constructor(public name: string, public expression: ASTWithSource, public type: ParsedPropertyType,
              public sourceSpan: ParseSourceSpan, readonly keySpan: ParseSourceSpan,
              public valueSpan: ParseSourceSpan | undefined);
}
export class ParsedEvent { /* overloaded ctor; fields: name, targetOrPhase, type: ParsedEventType,
              handler: ASTWithSource, sourceSpan, handlerSpan, keySpan: ParseSourceSpan */ }
export class ParsedVariable {
  constructor(readonly name: string, readonly value: string, readonly sourceSpan: ParseSourceSpan,
              readonly keySpan: ParseSourceSpan, readonly valueSpan?: ParseSourceSpan);
}
export class BoundElementProperty {
  constructor(public name: string, public type: BindingType, public securityContext: SecurityContext,
              public value: ASTWithSource, public unit: string | null, public sourceSpan: ParseSourceSpan,
              readonly keySpan: ParseSourceSpan | undefined, public valueSpan: ParseSourceSpan | undefined);
}
```

---

## 3. Key data structures & proposed Rust mapping

The dominant decision: **the `AST` class hierarchy → a single Rust enum** `Expr` with one variant per concrete class. Visitor dispatch in TS is virtual `visit()`; in Rust this is a `match`. Every node carries `span: ParseSpan` and `source_span: AbsoluteSourceSpan`, so factor those into a wrapper rather than repeating them in each variant.

### 3.1 Spans (Copy value types, no lifetimes)
```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseSpan { pub start: u32, pub end: u32 }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AbsoluteSourceSpan { pub start: u32, pub end: u32 }

impl ParseSpan {
    pub fn to_absolute(self, absolute_offset: u32) -> AbsoluteSourceSpan {
        AbsoluteSourceSpan { start: absolute_offset + self.start, end: absolute_offset + self.end }
    }
}
```
(Use `u32`/`i64` to match OXC span conventions; Angular uses JS `number`. Watch for `-1`/sentinel offsets — see §7.)

### 3.2 The core expression enum

Recommended shape: a wrapper struct holding the common spans + a `kind` enum. AST nodes are tree-shaped and arena allocation pays off (matches OXC's `oxc_allocator` model), so bind to arena lifetime `'a` and use `&'a` / `oxc_allocator::Vec<'a, _>` / `Box<'a, _>`.

```rust
pub struct AstNode<'a> {
    pub span: ParseSpan,
    pub source_span: AbsoluteSourceSpan,
    pub kind: ExprKind<'a>,
}

pub enum ExprKind<'a> {
    EmptyExpr,
    ImplicitReceiver,
    ThisReceiver,
    Chain          { expressions: Vec<'a, AstNode<'a>> },
    Conditional    { condition: Box<'a, AstNode<'a>>, true_exp: Box<'a, AstNode<'a>>, false_exp: Box<'a, AstNode<'a>> },
    PropertyRead   { name_span: AbsoluteSourceSpan, receiver: Box<'a, AstNode<'a>>, name: &'a str },
    SafePropertyRead { name_span: AbsoluteSourceSpan, receiver: Box<'a, AstNode<'a>>, name: &'a str },
    KeyedRead      { receiver: Box<'a, AstNode<'a>>, key: Box<'a, AstNode<'a>> },
    SafeKeyedRead  { receiver: Box<'a, AstNode<'a>>, key: Box<'a, AstNode<'a>> },
    BindingPipe    { name_span: AbsoluteSourceSpan, exp: Box<'a, AstNode<'a>>, name: &'a str,
                     args: Vec<'a, AstNode<'a>>, pipe_type: BindingPipeType },
    LiteralPrimitive { value: LiteralValue<'a> },
    LiteralArray   { expressions: Vec<'a, AstNode<'a>> },
    SpreadElement  { expression: Box<'a, AstNode<'a>> },
    LiteralMap     { keys: Vec<'a, LiteralMapKey<'a>>, values: Vec<'a, AstNode<'a>> },
    Interpolation  { strings: Vec<'a, &'a str>, expressions: Vec<'a, AstNode<'a>> },
    Binary         { operation: BinaryOperation, left: Box<'a, AstNode<'a>>, right: Box<'a, AstNode<'a>> },
    Unary          { operator: UnaryOperator, expr: Box<'a, AstNode<'a>> }, // see §7: do NOT model as Binary subclass
    PrefixNot      { expression: Box<'a, AstNode<'a>> },
    TypeofExpression { expression: Box<'a, AstNode<'a>> },
    VoidExpression { expression: Box<'a, AstNode<'a>> },
    NonNullAssert  { expression: Box<'a, AstNode<'a>> },
    Call           { receiver: Box<'a, AstNode<'a>>, args: Vec<'a, AstNode<'a>>, argument_span: AbsoluteSourceSpan },
    SafeCall       { receiver: Box<'a, AstNode<'a>>, args: Vec<'a, AstNode<'a>>, argument_span: AbsoluteSourceSpan },
    TaggedTemplateLiteral { tag: Box<'a, AstNode<'a>>, template: Box<'a, AstNode<'a>> /* TemplateLiteral */ },
    TemplateLiteral { elements: Vec<'a, TemplateLiteralElement<'a>>, expressions: Vec<'a, AstNode<'a>> },
    TemplateLiteralElement { text: &'a str },
    ParenthesizedExpression { expression: Box<'a, AstNode<'a>> },
    ArrowFunction  { parameters: Vec<'a, ArrowFunctionParameter<'a>>, body: Box<'a, AstNode<'a>> },
    RegularExpressionLiteral { body: &'a str, flags: Option<&'a str> },
}
```

`LiteralPrimitive.value` is `string | number | boolean | null | undefined` → tagged enum:
```rust
pub enum LiteralValue<'a> {
    Str(&'a str), Num(f64), Bool(bool), Null, Undefined,
}
```

### 3.3 `Binary` vs `Unary` (critical mapping note)
In TS, `Unary extends Binary` purely for legacy back-compat: `Unary` overrides `left`/`right`/`operation` to `never`, stores `operator: '+'|'-'` and `expr`, and *also* fills the inherited binary slots with a desugared form (`-x` → `0 - x`, `+x` → `x - 0`). Its `visit()` dispatches to `visitUnary` if present, else falls back to `visitBinary`.

**Do not replicate inheritance in Rust.** Model `Unary` as its own enum variant. Provide constructors mirroring the statics:
```rust
pub enum UnaryOperator { Plus, Minus }
impl<'a> AstNode<'a> {
    pub fn create_minus(/* span, source_span */ expr: AstNode<'a>) -> AstNode<'a> { /* operator: Minus */ }
    pub fn create_plus (/* span, source_span */ expr: AstNode<'a>) -> AstNode<'a> { /* operator: Plus  */ }
}
```
A visitor abstraction must offer a default that treats `Unary` like the desugared `Binary` form for consumers that don't special-case unary (preserve the fallback semantics).

### 3.4 Operators
```rust
pub enum AssignmentOperation { Assign, AddAssign, SubAssign, MulAssign, DivAssign, ModAssign,
                               PowAssign, AndAssign, OrAssign, NullishAssign }
pub enum BinaryOperation {
    Assignment(AssignmentOperation),
    And, Or, Nullish,
    Eq, Neq, Identity, NotIdentity,
    Lt, Gt, Le, Ge, In, Instanceof,
    Add, Sub, Mul, Mod, Div, Pow,
}
```
Keep a `is_assignment(&self) -> bool` (mirrors `Binary.isAssignmentOperation`). Keep string round-trip helpers (`as_str`, `from_str`) since the parser builds these from token text and codegen needs the symbol back.

### 3.5 Literal-map keys (discriminated union)
```rust
pub enum LiteralMapKey<'a> {
    Property { key: &'a str, quoted: bool, span: ParseSpan, source_span: AbsoluteSourceSpan,
               is_shorthand_initialized: bool },
    Spread   { span: ParseSpan, source_span: AbsoluteSourceSpan },
}
```
`LiteralMap` keeps `keys` and `values` as parallel vectors — preserve that (do not zip into pairs), because spread keys have no corresponding value index in the same way property keys do. Validate length invariants explicitly.

### 3.6 `ASTWithSource`
```rust
pub struct AstWithSource<'a> {
    pub ast: Box<'a, AstNode<'a>>,
    pub source: Option<&'a str>,
    pub location: &'a str,
    pub absolute_offset: u32,
    pub errors: Vec<'a, ParseError<'a>>,
    // span/source_span derived in ctor from source length (see §7)
}
```
This is itself an `AST` subtype in TS (so it can appear anywhere an `AST` does). In Rust, give `ExprKind` an `AstWithSource(AstWithSource<'a>)` variant rather than a separate type if it needs to nest; in practice it is almost always the *root* wrapper, so a top-level struct is usually fine.

### 3.7 Visitor
Replace the `AstVisitor` interface with a Rust trait whose methods have default impls that recurse (i.e. fold `RecursiveAstVisitor` and `AstVisitor` together — Rust traits give defaults for free):
```rust
pub trait AstVisitor<'a> {
    type Output;
    fn visit(&mut self, node: &AstNode<'a>) -> Self::Output { self.walk(node) }
    fn walk(&mut self, node: &AstNode<'a>) -> Self::Output { /* match node.kind, recurse */ }
    // per-variant hooks with default = recurse, override as needed
}
```
The "optional method" pattern (`visitUnary?`, `visitThisReceiver?`, `visitEmptyExpr?`, `visitASTWithSource?`, gate `visit?`) becomes trait methods with sensible defaults. Preserve the `RecursiveAstVisitor` child-visit order exactly (it is observable, e.g. i18n message ordering): e.g. `Conditional` visits condition→true→false; `Call` visits receiver then args; `TemplateLiteral` interleaves element[i] then expression[i].

### 3.8 Binding metadata wrappers
These reference `ParseSourceSpan` (from `parse_util.ts`) and `SecurityContext` (from `core`/`schema/dom_security_schema`) — note: **`ParseSourceSpan` (location-based) is different from `AbsoluteSourceSpan` (offset pair)**. Map:
```rust
pub enum ParsedPropertyType { Default, LiteralAttr, LegacyAnimation, TwoWay, Animation }
pub enum ParsedEventType    { Regular, LegacyAnimation, TwoWay, Animation }
pub enum BindingType        { Property, Attribute, Class, Style, LegacyAnimation, TwoWay, Animation }

pub struct ParsedProperty<'a> {
    pub name: &'a str, pub expression: AstWithSource<'a>, pub ty: ParsedPropertyType,
    pub source_span: ParseSourceSpan, pub key_span: ParseSourceSpan, pub value_span: Option<ParseSourceSpan>,
    // derived flags:
    pub is_literal: bool, pub is_legacy_animation: bool, pub is_animation: bool,
}
pub struct ParsedEvent<'a> { pub name: &'a str, pub target_or_phase: Option<&'a str>, pub ty: ParsedEventType,
    pub handler: AstWithSource<'a>, pub source_span: ParseSourceSpan, pub handler_span: ParseSourceSpan, pub key_span: ParseSourceSpan }
pub struct ParsedVariable<'a> { pub name: &'a str, pub value: &'a str, pub source_span: ParseSourceSpan,
    pub key_span: ParseSourceSpan, pub value_span: Option<ParseSourceSpan> }
pub struct BoundElementProperty<'a> { pub name: &'a str, pub ty: BindingType, pub security_context: SecurityContext,
    pub value: AstWithSource<'a>, pub unit: Option<&'a str>, pub source_span: ParseSourceSpan,
    pub key_span: Option<ParseSourceSpan>, pub value_span: Option<ParseSourceSpan> }
```
`ParsedProperty`'s three booleans are computed in the constructor from `type`; in Rust either compute them in a `new` or expose them as methods (`fn is_literal(&self) -> bool { self.ty == ParsedPropertyType::LiteralAttr }`) — methods are preferable (no stale derived state).

### 3.9 Microsyntax bindings
```rust
pub struct TemplateBindingIdentifier<'a> { pub source: &'a str, pub span: AbsoluteSourceSpan }
pub enum TemplateBinding<'a> {
    Variable   { source_span: AbsoluteSourceSpan, key: TemplateBindingIdentifier<'a>,
                 value: Option<TemplateBindingIdentifier<'a>> },
    Expression { source_span: AbsoluteSourceSpan, key: TemplateBindingIdentifier<'a>,
                 value: Option<AstWithSource<'a>> },
}
```

---

## 4. Algorithm walkthrough

This module is mostly declarative. The behaviour worth porting precisely:

### 4.1 `AST.visit` dispatch
Each concrete class's `visit(visitor, context)` calls the matching `visitor.visitXxx(this, context)`. For the back-compat-optional methods it uses optional chaining: e.g. `EmptyExpr.visit` → `visitor.visitEmptyExpr?.(this, context)` (no-op if undefined), `ThisReceiver.visit` → `visitor.visitThisReceiver?.(this, context)`.
**Rust:** a `match node.kind { … }` in `walk`, with the optional hooks defaulting to no-op / recurse.

### 4.2 `Unary.visit` fallback
```ts
visit(visitor, context) {
  if (visitor.visitUnary !== undefined) return visitor.visitUnary(this, context);
  return visitor.visitBinary(this, context);   // legacy: treat unary as the desugared binary
}
```
Step: prefer `visitUnary`; otherwise the node *is* a valid `Binary` (`0 - x` / `x - 0`) and is visited as one. **Port:** the visitor trait's `visit_unary` default delegates to `visit_binary` on the desugared form.

### 4.3 `ASTWithSource.visit`
```ts
visit(visitor, context) {
  if (visitor.visitASTWithSource) return visitor.visitASTWithSource(this, context);
  return this.ast.visit(visitor, context);   // transparent: forward to wrapped node
}
```
Default = transparent forwarding to the inner AST. **Port:** trait `visit_ast_with_source` default = `self.visit(&node.ast)`.

### 4.4 `RecursiveAstVisitor`
`visit(ast)` calls `ast.visit(this)`; each `visitXxx` recurses into children via `this.visit(child)` / `this.visitAll(children)`. The class is meant to be subclassed and have `visit()` overridden to selectively descend. The exact recursion order per node is the spec (enumerated in §3.7). `visitTemplateLiteral` is the only non-trivial loop:
```ts
for (let i = 0; i < ast.elements.length; i++) {
  this.visit(ast.elements[i], context);
  const expression = i < ast.expressions.length ? ast.expressions[i] : null;
  if (expression !== null) this.visit(expression, context);
}
```
Invariant: `expressions.length === elements.length - 1`.

### 4.5 `Unary.createMinus` / `createPlus`
`createMinus(span, sourceSpan, expr)` → `Unary` with `operator:'-'`, `expr`, and binary slots `'-' , 0 , expr` (i.e. `0 - x`).
`createPlus(...)` → `operator:'+'`, binary slots `'-', expr, 0` (i.e. `x - 0`).
The synthetic `0` is a `LiteralPrimitive(span, sourceSpan, 0)`.

### 4.6 `Binary.isAssignmentOperation(op)`
Pure predicate: true iff `op` ∈ the 10 assignment tokens. Type guard `op is AssignmentOperation`. Trivial port to a `match`.

---

## 5. Dependencies on other compiler modules

| Imported symbol | From | Used by |
|---|---|---|
| `SecurityContext` | `../core` (re-exported from `schema/dom_security_schema`) | `BoundElementProperty.securityContext` |
| `ParseError` | `../parse_util` | `ASTWithSource.errors` |
| `ParseSourceSpan` | `../parse_util` | `ParsedProperty`, `ParsedEvent`, `ParsedVariable`, `BoundElementProperty` |

`ParseSourceSpan` (and its `ParseLocation` / `ParseSourceFile`) is **location-based** (file + offset + line + col), distinct from this module's offset-only `AbsoluteSourceSpan`. `ParseError extends Error` with `{span: ParseSourceSpan, msg, level: ParseErrorLevel, relatedError?}`.

**Reverse (who depends on this):** `expression_parser/parser.ts` (constructs every node), `expression_parser/lexer.ts` (operators), `render3/r3_ast.ts` and template parser (embed `ASTWithSource` as binding values), the expression-to-output converter, type-check-block generation, i18n extraction, and the language service. This is a hub module — port it before any of those.

So the port order: `parse_util` span/error types and `SecurityContext` enum must exist (or be stubbed) **before** this module.

---

## 6. ɵɵ instructions / output emitted

**None.** This module emits no `ɵɵ…` render3 runtime instructions and produces no JS. It is purely the in-memory expression model + visitor. The `ɵɵ` instruction emission happens far downstream (render3 template compiler → `output_ast` → OXC codegen). The relevance of this module to emission is indirect: these node types are the *input* to the expression converter that eventually produces operands for instructions such as `ɵɵproperty`, `ɵɵpipeBind*`, `ɵɵinterpolate*`, `ɵɵlistener`, etc. Notably:
- `BindingPipe` ⟶ later lowered to `ɵɵpipe` / `ɵɵpipeBindN`.
- `Interpolation` ⟶ `ɵɵinterpolateN` family (arity depends on `expressions.length`).
- `SafePropertyRead`/`SafeKeyedRead`/`SafeCall` ⟶ desugared to null-guarded temporaries downstream.

These mappings belong in the converter spec, not here; listed only for orientation.

---

## 7. Edge cases, gotchas, version-sensitivity

1. **`Unary extends Binary` legacy hack.** `Unary` overrides `left`/`right`/`operation` to `never` and simultaneously stores a desugared binary form. The class comment says the inheritance "can be deleted in some future major." This is exactly the kind of internal churn to expect. In Rust, model `Unary` independently but **preserve the visitor fallback** (`visitUnary` optional → falls back to `visitBinary` on `0 - x` / `x - 0`). Don't lose the synthetic-zero desugaring if a consumer relies on the binary view.

2. **`ThisReceiver`'s declared base is `AST`, but `RecursiveAstVisitor.visitImplicitReceiver` is typed `(ast: ThisReceiver, …)`** — Angular conflates `ImplicitReceiver`/`ThisReceiver` in places. Keep them as distinct variants; `this`-receiver matters for scope resolution.

3. **Typo in the visitor interface:** `visitVoidExpression(ast: TypeofExpression, …)` — the parameter is typed `TypeofExpression`, not `VoidExpression`. `RecursiveAstVisitor.visitVoidExpression` correctly uses `VoidExpression`. Port to the correct `VoidExpression`; this is a known Angular source typo, not semantics.

4. **Optional visitor methods** (`visitUnary?`, `visitThisReceiver?`, `visitEmptyExpr?`, `visitASTWithSource?`, gate `visit?`) exist for back-compat with external visitor implementations. In Rust trait defaults make these "always present"; preserve the *behavioral* default (no-op for `visitEmptyExpr`, transparent forward for `visitASTWithSource`, binary-fallback for `visitUnary`).

5. **`LiteralPrimitive.value` includes both `null` and `undefined`** — must be distinct in the Rust enum (`Null` vs `Undefined`), they are not the same downstream.

6. **`LiteralMap` parallel arrays + spread keys.** `keys: LiteralMapKey[]` and `values: AST[]` are parallel, but `LiteralMapSpreadKey` participates differently from `LiteralMapPropertyKey`. `isShorthandInitialized?` on property keys is optional. Preserve array parallelism; validate lengths.

7. **`Interpolation` invariant:** `strings.length === expressions.length + 1`. `TemplateLiteral`: `expressions.length === elements.length - 1`. Enforce in constructors/`debug_assert!`.

8. **`ASTWithSource` derived spans.** Its ctor computes `span = ParseSpan(0, source?.length ?? 0)` and `sourceSpan = AbsoluteSourceSpan(absoluteOffset, source===null ? absoluteOffset : absoluteOffset + source.length)`. `source` is nullable. Reproduce this length-derivation in the Rust ctor; mind UTF-16 vs byte length (Angular uses JS `string.length` = UTF-16 code units, OXC spans are UTF-8 byte offsets — a real conversion hazard, see #11).

9. **`ParsedProperty` derived booleans** computed in ctor from `type`; keep them as methods to avoid drift.

10. **`ParsedEvent` overloaded constructor** — TS overloads narrow `handler` to `ASTWithSource<NonNullAssert | PropertyRead | KeyedRead>` for `TwoWay` events. In Rust this is just a single struct; the narrowing is advisory. Optionally encode with an enum if you want the type safety.

11. **Offset model.** Angular `number` offsets are JS UTF-16 code-unit indices. OXC/`oxc_span::Span` uses `u32` UTF-8 byte offsets. If these spans are ever fed to OXC or used to slice OXC-tokenized source, a UTF-16→UTF-8 remap is required. Keep this module's spans as their own `u32` types and centralize any conversion at the OXC boundary.

12. **New-ish node types** (version-sensitive surface area in 22.x): `TypeofExpression`, `VoidExpression`, `TaggedTemplateLiteral`, `TemplateLiteral`/`TemplateLiteralElement`, `ParenthesizedExpression`, `ArrowFunction`, `RegularExpressionLiteral`, `SpreadElement`, `LiteralMapSpreadKey`, and the expanded `BinaryOperation` set (`in`, `instanceof`, `**`, all the assignment ops, `??`). Older Angular references will lack these — do not copy an older mapping. `ArrowFunctionParameter` is currently only `ArrowFunctionIdentifierParameter` (rest params are a TODO in source).

---

## 8. Port plan (Rust/OXC)

### Approach
- Create crate module `expression_ast` (mirrors `expression_parser/ast.ts`).
- Implement as an **arena-allocated enum tree** parameterized by `'a`, aligned with OXC's `oxc_allocator::Allocator`, `Box<'a, T>`, and `Vec<'a, T>`. Reuse `oxc_allocator` directly — do not invent an allocator. This keeps the AST allocation model consistent with the OXC nodes used at emission time and avoids `Rc`/clone churn.
- Spans: define local `ParseSpan` / `AbsoluteSourceSpan` as `Copy` `u32` structs. **Do not** alias them to `oxc_span::Span` yet — semantics (UTF-16) differ; convert only at the OXC boundary.
- Strings: store as `&'a str` (arena/interned) rather than `String` to match how the parser slices source.

### What to reuse from OXC
- `oxc_allocator::{Allocator, Box, Vec}` for the tree.
- Nothing from `oxc_ast` here — these are Angular nodes, not ESTree. `oxc_ast`/`oxc_codegen` come in only when lowering the *output* IR (different spec).
- Operator-symbol round-tripping can mirror, but not reuse, OXC's `BinaryOperator`/`LogicalOperator`/`AssignmentOperator` enums (Angular's set differs slightly — e.g. Angular merges everything into one `BinaryOperation` and treats `&&`/`||`/`??` as binary, plus the unary desugaring).

### Implementation steps (ordered)
1. Span types + `to_absolute` (+ unit tests).
2. Operator enums (`BinaryOperation`, `AssignmentOperation`, `UnaryOperator`) with `as_str`/`from_str`/`is_assignment` (+ table-driven tests against the TS string unions).
3. `LiteralValue`, `LiteralMapKey`, `ArrowFunctionParameter`, `TemplateLiteralElement`, `TemplateBindingIdentifier`.
4. `AstNode` + `ExprKind` enum (the bulk).
5. `AstWithSource` with derived-span constructor logic.
6. `AstVisitor` trait + default recursive walk (the merged `AstVisitor`/`RecursiveAstVisitor`), matching child-visit order exactly. Add a fold/mut variant if downstream needs rewriting.
7. `Unary::create_minus`/`create_plus` constructors + visitor fallback semantics.
8. Binding wrappers (`ParsedProperty`, `ParsedEvent`, `ParsedVariable`, `BoundElementProperty`) + enums — these depend on `ParseSourceSpan` and `SecurityContext`, so stub or port those first.
9. `TemplateBinding` enum.

### Dependencies / ordering vs other modules
- **Before this module:** `parse_util` (`ParseSpan`/`ParseLocation`/`ParseSourceSpan`/`ParseError`/`ParseErrorLevel`) and the `SecurityContext` enum. At minimum the *types* must exist (can be thin ports).
- **This module gates:** `expression_parser/lexer`, `expression_parser/parser`, the R3 template AST, the expression→output converter, type-check generation, i18n. Port this **immediately after** the span/error primitives and **before** the parser.

### Estimated complexity: **Medium**
Large surface area (≈30 node kinds + 4 enums + 5 binding wrappers + visitor) but algorithmically shallow — no parsing, no emission, just data + a recursive walk. Risks are mechanical, not conceptual: (a) faithfully reproducing the `Unary`/`Binary` desugaring and visitor fallback, (b) the UTF-16↔UTF-8 span hazard at the OXC boundary, (c) keeping `RecursiveAstVisitor` child-order identical for downstream determinism, (d) the `visitVoidExpression` typo and the optional-method defaults. With those handled, it is a straightforward, high-leverage early port.
