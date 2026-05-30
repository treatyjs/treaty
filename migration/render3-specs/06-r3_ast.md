# Port Spec: `render3/r3_ast.ts` — The Render3 Template AST

> Angular 22.1.0-next.0 — `packages/compiler/src/render3/r3_ast.ts`
> Target: Rust + OXC (`oxc_ast` AstBuilder + `oxc_codegen`).
> Module index: 06.

---

## 1. Purpose & Role in the Compilation Pipeline

`r3_ast.ts` (commonly imported as `import * as t from '.../r3_ast'`, the `t.` namespace
seen all over the compiler) defines the **template-level IR** ("R3 AST" or "t-AST") produced
by the Angular template parser and consumed by the template-to-instruction compiler.

Position in the pipeline:

```
HTML source string
  → html parser (html.Node tree: html.Element, html.Text, html.Attribute, ...)
  → r3_template_transform.ts (HtmlAstToIvyAst visitor)
      ↓  produces
  → r3_ast.ts nodes  (t.Element, t.Template, t.Text, t.BoundText, control-flow blocks ...)
      ↓  consumed by
  → template_pipeline / view compiler  → ɵɵ instructions (TConstants + creation/update fns)
```

Key facts:
- This file is **pure data + a visitor pattern**. It contains **no compilation logic** and
  emits **no `ɵɵ` instructions itself**. It is the *vocabulary* the rest of render3 speaks.
- Every node carries `ParseSourceSpan`s. Spans are first-class and pervasive — this AST is
  used not only for code generation but also by the **language service** and the
  **template type checker** (`HostElement` exists *only* for type checking).
- It is distinct from the **HTML AST** (`ml_parser/ast.ts`, the `html.*` nodes) and from the
  **expression AST** (`expression_parser/ast.ts`, the `e.AST` expression nodes). Bound values
  in this AST embed expression-AST nodes (`AST` / `ASTWithSource`) by reference.

Naming note: the file uses bare names like `Node`, `Element`, `Text`, `Comment`, `Visitor`,
`visitAll`. These collide with both the DOM lib and the HTML AST, which is exactly why
consumers alias the whole module as `t`.

---

## 2. Public API

### 2.1 Core interfaces & free functions

```ts
export interface Node {
  sourceSpan: ParseSourceSpan;
  visit<Result>(visitor: Visitor<Result>): Result;
}

export interface Visitor<Result = any> {
  visit?(node: Node): Result;             // optional generic pre-hook
  visitElement(element: Element): Result;
  visitTemplate(template: Template): Result;
  visitContent(content: Content): Result;
  visitVariable(variable: Variable): Result;
  visitReference(reference: Reference): Result;
  visitTextAttribute(attribute: TextAttribute): Result;
  visitBoundAttribute(attribute: BoundAttribute): Result;
  visitBoundEvent(attribute: BoundEvent): Result;
  visitText(text: Text): Result;
  visitBoundText(text: BoundText): Result;
  visitIcu(icu: Icu): Result;
  visitDeferredBlock(deferred: DeferredBlock): Result;
  visitDeferredBlockPlaceholder(block: DeferredBlockPlaceholder): Result;
  visitDeferredBlockError(block: DeferredBlockError): Result;
  visitDeferredBlockLoading(block: DeferredBlockLoading): Result;
  visitDeferredTrigger(trigger: DeferredTrigger): Result;
  visitSwitchBlock(block: SwitchBlock): Result;
  visitSwitchBlockCase(block: SwitchBlockCase): Result;
  visitSwitchBlockCaseGroup(block: SwitchBlockCaseGroup): Result;
  visitSwitchExhaustiveCheck(block: SwitchExhaustiveCheck): Result;
  visitForLoopBlock(block: ForLoopBlock): Result;
  visitForLoopBlockEmpty(block: ForLoopBlockEmpty): Result;
  visitIfBlock(block: IfBlock): Result;
  visitIfBlockBranch(block: IfBlockBranch): Result;
  visitUnknownBlock(block: UnknownBlock): Result;
  visitLetDeclaration(decl: LetDeclaration): Result;
  visitComponent(component: Component): Result;
  visitDirective(directive: Directive): Result;
}

export class RecursiveVisitor implements Visitor<void> { /* default traversal, see §4.3 */ }

export function visitAll<Result>(visitor: Visitor<Result>, nodes: Node[]): Result[];
```

Note: `visitDeferredTrigger`, `visitVariable`, `visitReference`, `visitTextAttribute`,
`visitBoundAttribute`, `visitBoundEvent`, `visitText`, `visitBoundText`, `visitIcu`,
`visitUnknownBlock`, `visitLetDeclaration` have **no** override in `RecursiveVisitor` (leaf or
no-op nodes). `SwitchBlockCase` and `SwitchExhaustiveCheck` are also no-ops in the recursive
visitor.

### 2.2 Static factory methods

```ts
class BoundAttribute {
  static fromBoundElementProperty(prop: BoundElementProperty, i18n?: I18nMeta): BoundAttribute;
}
class BoundEvent {
  static fromParsedEvent(event: ParsedEvent): BoundEvent;
}
```

Both throw if `prop.keySpan` / `event.keySpan` is `undefined`. `fromParsedEvent` derives
`target` from `ParsedEventType.Regular` and `phase` from `ParsedEventType.LegacyAnimation`.

### 2.3 Exported classes/interfaces (the node zoo)

Leaf/simple: `Comment`, `Text`, `BoundText`, `TextAttribute`, `BoundAttribute`, `BoundEvent`,
`Variable`, `Reference`, `Icu`, `UnknownBlock`, `LetDeclaration`, `HostElement`.

Containers: `Element`, `Template`, `Content`, `Component`, `Directive`.

Deferred triggers: abstract `DeferredTrigger`, plus `BoundDeferredTrigger`,
`NeverDeferredTrigger`, `IdleDeferredTrigger`, `ImmediateDeferredTrigger`,
`HoverDeferredTrigger`, `TimerDeferredTrigger`, `InteractionDeferredTrigger`,
`ViewportDeferredTrigger`. Interface `DeferredBlockTriggers`.

Blocks (extend `BlockNode`): `DeferredBlock`, `DeferredBlockPlaceholder`,
`DeferredBlockLoading`, `DeferredBlockError`, `SwitchBlock`, `SwitchBlockCase`,
`SwitchBlockCaseGroup`, `SwitchExhaustiveCheck`, `ForLoopBlock`, `ForLoopBlockEmpty`,
`IfBlock`, `IfBlockBranch`. Base class `BlockNode` (not a `Node` itself — has no `visit`).

---

## 3. Key Data Structures + Proposed Rust Mapping

### Overall strategy

The whole AST is owned by an arena. Use a lifetime `'a` and store node references as
`&'a Node<'a>` or in `oxc_allocator::Vec<'a, Node<'a>>`. Expression values (`AST`) and i18n
metadata live in **other** spec modules; reference them by opaque arena handles here:

- `AST` / `ASTWithSource` → `expression_parser/ast` module → `&'a Expr<'a>` (or
  `ExprWithSource<'a>`). Treat as an opaque handle for this spec.
- `I18nMeta` → `i18n/i18n_ast` module → `Option<&'a I18nMeta<'a>>`.
- `ParseSourceSpan` → `parse_util` module → `ParseSourceSpan<'a>` (a small `Copy`/clone struct
  of `{ start: ParseLocation, end, fullStart, details }`). Cheap to clone; pass by value or
  `&'a`.
- `BindingType`, `ParsedEventType`, `SecurityContext` → plain `#[repr(u8)]` enums (values
  below).

Recommended top-level representation: a single tagged enum `TNode<'a>` whose variants are the
node structs, instead of TS's class-hierarchy + double-dispatch. This converts the visitor
double-dispatch into Rust `match`, which is more idiomatic and faster.

```rust
pub enum TNode<'a> {
    Text(&'a Text<'a>),
    BoundText(&'a BoundText<'a>),
    Element(&'a Element<'a>),
    Template(&'a Template<'a>),
    Content(&'a Content<'a>),
    Component(&'a Component<'a>),
    Directive(&'a Directive<'a>),
    Variable(&'a Variable<'a>),
    Reference(&'a Reference<'a>),
    TextAttribute(&'a TextAttribute<'a>),
    BoundAttribute(&'a BoundAttribute<'a>),
    BoundEvent(&'a BoundEvent<'a>),
    Icu(&'a Icu<'a>),
    LetDeclaration(&'a LetDeclaration<'a>),
    UnknownBlock(&'a UnknownBlock<'a>),
    // control flow
    DeferredBlock(&'a DeferredBlock<'a>),
    DeferredBlockPlaceholder(&'a DeferredBlockPlaceholder<'a>),
    DeferredBlockLoading(&'a DeferredBlockLoading<'a>),
    DeferredBlockError(&'a DeferredBlockError<'a>),
    SwitchBlock(&'a SwitchBlock<'a>),
    SwitchBlockCaseGroup(&'a SwitchBlockCaseGroup<'a>),
    SwitchBlockCase(&'a SwitchBlockCase<'a>),
    SwitchExhaustiveCheck(&'a SwitchExhaustiveCheck<'a>),
    ForLoopBlock(&'a ForLoopBlock<'a>),
    ForLoopBlockEmpty(&'a ForLoopBlockEmpty<'a>),
    IfBlock(&'a IfBlock<'a>),
    IfBlockBranch(&'a IfBlockBranch<'a>),
    // Comment and HostElement are NOT part of the visitable Node set in practice
}
```
> `Comment` and `HostElement` both `throw` from `visit()`, so they are deliberately not in the
> double-dispatch set. Keep them as separate structs, not `TNode` variants (or include them but
> never traverse them — match TS by `panic!`/`unreachable!` is acceptable, see §7).

### 3.1 Leaf nodes

```ts
class Text         { value: string; sourceSpan; }
class BoundText    { value: AST; sourceSpan; i18n?: I18nMeta; }
class Comment      { value: string; sourceSpan; }                 // visit() throws
```

```rust
pub struct Text<'a> { pub value: &'a str, pub source_span: ParseSourceSpan<'a> }
pub struct BoundText<'a> {
    pub value: &'a Expr<'a>,
    pub source_span: ParseSourceSpan<'a>,
    pub i18n: Option<&'a I18nMeta<'a>>,
}
pub struct Comment<'a> { pub value: &'a str, pub source_span: ParseSourceSpan<'a> }
```

### 3.2 Attributes, events, refs, variables

```ts
class TextAttribute  { name; value: string; sourceSpan; readonly keySpan?: span; valueSpan?; i18n?; }
class BoundAttribute { name; type: BindingType; securityContext: SecurityContext;
                       value: AST; unit: string|null; sourceSpan; readonly keySpan;
                       valueSpan?: span; i18n?: span; }
class BoundEvent     { name; type: ParsedEventType; handler: AST; target: string|null;
                       phase: string|null; sourceSpan; handlerSpan; readonly keySpan; }
class Variable       { name; value: string; sourceSpan; readonly keySpan; valueSpan?; }
class Reference      { name; value: string; sourceSpan; readonly keySpan; valueSpan?; }
```

> `keySpan` optionality differs per class: `TextAttribute.keySpan: span | undefined`;
> `BoundAttribute.keySpan: span` (required); `Variable/Reference.keySpan: span` (required).
> Preserve these distinctions in Rust (`Option<ParseSourceSpan>` vs `ParseSourceSpan`).

```rust
pub struct TextAttribute<'a> {
    pub name: &'a str,
    pub value: &'a str,
    pub source_span: ParseSourceSpan<'a>,
    pub key_span: Option<ParseSourceSpan<'a>>,
    pub value_span: Option<ParseSourceSpan<'a>>,
    pub i18n: Option<&'a I18nMeta<'a>>,
}
pub struct BoundAttribute<'a> {
    pub name: &'a str,
    pub kind: BindingType,
    pub security_context: SecurityContext,
    pub value: &'a Expr<'a>,
    pub unit: Option<&'a str>,
    pub source_span: ParseSourceSpan<'a>,
    pub key_span: ParseSourceSpan<'a>,
    pub value_span: Option<ParseSourceSpan<'a>>,
    pub i18n: Option<&'a I18nMeta<'a>>,
}
pub struct BoundEvent<'a> {
    pub name: &'a str,
    pub kind: ParsedEventType,
    pub handler: &'a Expr<'a>,
    pub target: Option<&'a str>,
    pub phase: Option<&'a str>,
    pub source_span: ParseSourceSpan<'a>,
    pub handler_span: ParseSourceSpan<'a>,
    pub key_span: ParseSourceSpan<'a>,
}
pub struct Variable<'a>  { pub name: &'a str, pub value: &'a str, pub source_span: ParseSourceSpan<'a>, pub key_span: ParseSourceSpan<'a>, pub value_span: Option<ParseSourceSpan<'a>> }
pub struct Reference<'a> { /* identical shape to Variable */ }
```

Supporting enums (from sibling modules, reproduced for self-containment):

```rust
#[repr(u8)] pub enum BindingType {
    Property = 0, Attribute, Class, Style, LegacyAnimation, TwoWay, Animation,
}
#[repr(u8)] pub enum ParsedEventType { Regular = 0, LegacyAnimation, TwoWay, Animation }
#[repr(u8)] pub enum SecurityContext {
    None = 0, Html = 1, Style = 2, Script = 3, Url = 4, ResourceUrl = 5, AttributeNoBinding = 6,
}
```

### 3.3 Element-family containers

```ts
class Element {
  name; attributes: TextAttribute[]; inputs: BoundAttribute[]; outputs: BoundEvent[];
  directives: Directive[]; children: Node[]; references: Reference[];
  isSelfClosing: boolean; sourceSpan; startSourceSpan; endSourceSpan: span|null;
  readonly isVoid: boolean; i18n?;
}
class Component { componentName; tagName: string|null; fullName; attributes; inputs;
  outputs; directives; children; references; isSelfClosing; sourceSpan; startSourceSpan;
  endSourceSpan; i18n?; }                     // NEW in v19+/selectorless
class Directive { name; attributes; inputs; outputs; references; sourceSpan;
  startSourceSpan; endSourceSpan; i18n?; }    // NEW selectorless directive node
class Template { tagName: string|null; attributes; inputs; outputs; directives;
  templateAttrs: (BoundAttribute|TextAttribute)[]; children; references; variables;
  isSelfClosing; sourceSpan; startSourceSpan; endSourceSpan; i18n?; }
class Content { readonly name='ng-content'; selector; attributes; children;
  isSelfClosing; sourceSpan; startSourceSpan; endSourceSpan; i18n?; }
```

```rust
pub struct Element<'a> {
    pub name: &'a str,
    pub attributes: Vec<'a, &'a TextAttribute<'a>>,
    pub inputs:     Vec<'a, &'a BoundAttribute<'a>>,
    pub outputs:    Vec<'a, &'a BoundEvent<'a>>,
    pub directives: Vec<'a, &'a Directive<'a>>,
    pub children:   Vec<'a, TNode<'a>>,
    pub references: Vec<'a, &'a Reference<'a>>,
    pub is_self_closing: bool,
    pub source_span: ParseSourceSpan<'a>,
    pub start_source_span: ParseSourceSpan<'a>,
    pub end_source_span: Option<ParseSourceSpan<'a>>,
    pub is_void: bool,
    pub i18n: Option<&'a I18nMeta<'a>>,
}
// Template adds: template_attrs: Vec<TemplateAttr<'a>>, variables: Vec<&Variable>, tag_name: Option<&str>
pub enum TemplateAttr<'a> { Bound(&'a BoundAttribute<'a>), Text(&'a TextAttribute<'a>) }
// Component adds component_name/full_name + tag_name: Option<&str>; Content adds selector + const name="ng-content".
```

Use `oxc_allocator::Vec<'a, T>` for all `[]` fields. `string|null` → `Option<&'a str>`.

### 3.4 `BlockNode` base + control-flow

`BlockNode` is a shared positional base (NOT a `Node`):

```ts
class BlockNode { nameSpan; sourceSpan; startSourceSpan; endSourceSpan: span|null; }
```

In Rust, model as a **shared field struct embedded by composition** rather than inheritance:

```rust
pub struct BlockSpans<'a> {
    pub name_span: ParseSourceSpan<'a>,
    pub source_span: ParseSourceSpan<'a>,
    pub start_source_span: ParseSourceSpan<'a>,
    pub end_source_span: Option<ParseSourceSpan<'a>>,
}
```
Each block struct holds a `spans: BlockSpans<'a>` field.

#### @if

```ts
class IfBlock       { branches: IfBlockBranch[]; + BlockNode spans }
class IfBlockBranch { expression: AST|null; children: Node[];
                      expressionAlias: Variable|null; + spans; i18n?; }
```
```rust
pub struct IfBlock<'a>       { pub branches: Vec<'a, &'a IfBlockBranch<'a>>, pub spans: BlockSpans<'a> }
pub struct IfBlockBranch<'a> {
    pub expression: Option<&'a Expr<'a>>,        // None for the @else branch
    pub children: Vec<'a, TNode<'a>>,
    pub expression_alias: Option<&'a Variable<'a>>,
    pub spans: BlockSpans<'a>,
    pub i18n: Option<&'a I18nMeta<'a>>,
}
```

#### @switch

```ts
class SwitchBlock          { expression: AST; groups: SwitchBlockCaseGroup[];
                             unknownBlocks: UnknownBlock[]; exhaustiveCheck: SwitchExhaustiveCheck|null; + spans }
class SwitchBlockCaseGroup { cases: SwitchBlockCase[]; children: Node[]; + spans; i18n?; }
class SwitchBlockCase      { expression: AST|null; + spans }   // null => @default
class SwitchExhaustiveCheck{ expression: AST|null; + spans }
```
> NOTE (version-sensitive, §7): `SwitchBlockCaseGroup` and `SwitchExhaustiveCheck` are
> **recent additions** (case-grouping + exhaustiveness). Older render3 had `SwitchBlock.cases`
> directly. The current shape is `groups` of `cases`.

```rust
pub struct SwitchBlock<'a> {
    pub expression: &'a Expr<'a>,
    pub groups: Vec<'a, &'a SwitchBlockCaseGroup<'a>>,
    pub unknown_blocks: Vec<'a, &'a UnknownBlock<'a>>,
    pub exhaustive_check: Option<&'a SwitchExhaustiveCheck<'a>>,
    pub spans: BlockSpans<'a>,
}
pub struct SwitchBlockCaseGroup<'a> {
    pub cases: Vec<'a, &'a SwitchBlockCase<'a>>,
    pub children: Vec<'a, TNode<'a>>,
    pub spans: BlockSpans<'a>, pub i18n: Option<&'a I18nMeta<'a>>,
}
pub struct SwitchBlockCase<'a>      { pub expression: Option<&'a Expr<'a>>, pub spans: BlockSpans<'a> }
pub struct SwitchExhaustiveCheck<'a>{ pub expression: Option<&'a Expr<'a>>, pub spans: BlockSpans<'a> }
```

#### @for

```ts
class ForLoopBlock {
  item: Variable; expression: ASTWithSource; trackBy: ASTWithSource|null;
  trackKeywordSpan: span|null; contextVariables: Variable[]; children: Node[];
  empty: ForLoopBlockEmpty|null; sourceSpan; mainBlockSpan; startSourceSpan;
  endSourceSpan; nameSpan; i18n?;
}
class ForLoopBlockEmpty { children: Node[]; + spans; i18n?; }
```
> Note `mainBlockSpan` is an **extra** span beyond `BlockSpans` (the `@for(...) { ... }` body
> span excluding `@empty`). `DeferredBlock` also has `mainBlockSpan`.

```rust
pub struct ForLoopBlock<'a> {
    pub item: &'a Variable<'a>,
    pub expression: &'a ExprWithSource<'a>,
    pub track_by: Option<&'a ExprWithSource<'a>>,
    pub track_keyword_span: Option<ParseSourceSpan<'a>>,
    pub context_variables: Vec<'a, &'a Variable<'a>>,   // $index, $count, $first, $last, $even, $odd
    pub children: Vec<'a, TNode<'a>>,
    pub empty: Option<&'a ForLoopBlockEmpty<'a>>,
    pub main_block_span: ParseSourceSpan<'a>,
    pub spans: BlockSpans<'a>,
    pub i18n: Option<&'a I18nMeta<'a>>,
}
```

#### @defer + triggers

```ts
interface DeferredBlockTriggers {
  when?: BoundDeferredTrigger; idle?: IdleDeferredTrigger; immediate?: ImmediateDeferredTrigger;
  hover?: HoverDeferredTrigger; timer?: TimerDeferredTrigger; interaction?: InteractionDeferredTrigger;
  viewport?: ViewportDeferredTrigger; never?: NeverDeferredTrigger;
}
class DeferredBlock extends BlockNode {
  children: Node[];
  readonly triggers; readonly prefetchTriggers; readonly hydrateTriggers;     // 3x DeferredBlockTriggers
  placeholder: DeferredBlockPlaceholder|null; loading: DeferredBlockLoading|null; error: DeferredBlockError|null;
  mainBlockSpan; i18n?;
  // private cached key arrays: definedTriggers / definedPrefetchTriggers / definedHydrateTriggers
  visitAll(visitor): void;     // custom ordered traversal — see §4.2
}
class DeferredBlockPlaceholder { children; minimumTime: number|null; + spans; i18n?; }
class DeferredBlockLoading     { children; afterTime: number|null; minimumTime: number|null; + spans; i18n?; }
class DeferredBlockError       { children; + spans; i18n?; }

abstract class DeferredTrigger { nameSpan: span|null; sourceSpan; prefetchSpan: span|null;
                                 whenOrOnSourceSpan: span|null; hydrateSpan: span|null; }
class BoundDeferredTrigger     extends DeferredTrigger { value: AST; }            // @defer (when expr)
class NeverDeferredTrigger     extends DeferredTrigger {}
class IdleDeferredTrigger      extends DeferredTrigger { timeout: number|null; }
class ImmediateDeferredTrigger extends DeferredTrigger {}
class HoverDeferredTrigger     extends DeferredTrigger { reference: string|null; }
class TimerDeferredTrigger     extends DeferredTrigger { delay: number; }
class InteractionDeferredTrigger extends DeferredTrigger { reference: string|null; }
class ViewportDeferredTrigger  extends DeferredTrigger { reference: string|null; options: LiteralMap|null; }
```

Rust mapping — collapse trigger subclasses into one enum + shared span struct:

```rust
pub struct TriggerSpans<'a> {
    pub name_span: Option<ParseSourceSpan<'a>>,
    pub source_span: ParseSourceSpan<'a>,
    pub prefetch_span: Option<ParseSourceSpan<'a>>,
    pub when_or_on_source_span: Option<ParseSourceSpan<'a>>,
    pub hydrate_span: Option<ParseSourceSpan<'a>>,
}
pub enum DeferredTrigger<'a> {
    When(&'a Expr<'a>),                       // BoundDeferredTrigger (name_span always None)
    Never,
    Idle { timeout: Option<f64> },
    Immediate,
    Hover { reference: Option<&'a str> },
    Timer { delay: f64 },
    Interaction { reference: Option<&'a str> },
    Viewport { reference: Option<&'a str>, options: Option<&'a LiteralMap<'a>> },
}
pub struct DeferredTriggerNode<'a> { pub kind: DeferredTrigger<'a>, pub spans: TriggerSpans<'a> }

pub struct DeferredBlockTriggers<'a> {
    pub when: Option<&'a DeferredTriggerNode<'a>>,
    pub idle: Option<&'a DeferredTriggerNode<'a>>,
    pub immediate: Option<&'a DeferredTriggerNode<'a>>,
    pub hover: Option<&'a DeferredTriggerNode<'a>>,
    pub timer: Option<&'a DeferredTriggerNode<'a>>,
    pub interaction: Option<&'a DeferredTriggerNode<'a>>,
    pub viewport: Option<&'a DeferredTriggerNode<'a>>,
    pub never: Option<&'a DeferredTriggerNode<'a>>,
}
pub struct DeferredBlock<'a> {
    pub children: Vec<'a, TNode<'a>>,
    pub triggers: DeferredBlockTriggers<'a>,
    pub prefetch_triggers: DeferredBlockTriggers<'a>,
    pub hydrate_triggers: DeferredBlockTriggers<'a>,
    pub placeholder: Option<&'a DeferredBlockPlaceholder<'a>>,
    pub loading: Option<&'a DeferredBlockLoading<'a>>,
    pub error: Option<&'a DeferredBlockError<'a>>,
    pub main_block_span: ParseSourceSpan<'a>,
    pub spans: BlockSpans<'a>,
    pub i18n: Option<&'a I18nMeta<'a>>,
}
```
> The TS `definedTriggers` cached key arrays exist purely to preserve **insertion order** during
> traversal. In Rust, traverse a fixed `[when, idle, immediate, hover, timer, interaction,
> viewport, never]` array of `Option`s — but to match TS *exactly* you must preserve the
> original key insertion order, which `Object.keys` reflects. If exact order matters for
> instruction emission, store an additional `order: Vec<'a, TriggerKind>` per group. (For most
> downstream behavior, fixed order is sufficient; verify against golden output — see §7.)

### 3.5 Misc nodes

```ts
class UnknownBlock   { name; sourceSpan; nameSpan; }
class LetDeclaration { name; value: AST; sourceSpan; nameSpan; valueSpan; }   // @let x = expr;
class Icu { vars: {[name:string]: BoundText}; placeholders: {[name:string]: Text|BoundText};
            sourceSpan; i18n?; }
class HostElement {                                  // type-check-only, visit() throws
  readonly tagNames: string[]; readonly bindings: BoundAttribute[];
  readonly listeners: BoundEvent[]; readonly sourceSpan;
  // ctor throws if tagNames.length === 0
}
```
```rust
pub struct UnknownBlock<'a>   { pub name: &'a str, pub source_span: ParseSourceSpan<'a>, pub name_span: ParseSourceSpan<'a> }
pub struct LetDeclaration<'a> { pub name: &'a str, pub value: &'a Expr<'a>, pub source_span: ParseSourceSpan<'a>, pub name_span: ParseSourceSpan<'a>, pub value_span: ParseSourceSpan<'a> }
pub enum IcuPlaceholder<'a> { Text(&'a Text<'a>), Bound(&'a BoundText<'a>) }
pub struct Icu<'a> {
    pub vars: Vec<'a, (&'a str, &'a BoundText<'a>)>,         // ordered map; or hashbrown in arena
    pub placeholders: Vec<'a, (&'a str, IcuPlaceholder<'a>)>,
    pub source_span: ParseSourceSpan<'a>,
    pub i18n: Option<&'a I18nMeta<'a>>,
}
pub struct HostElement<'a> {
    pub tag_names: Vec<'a, &'a str>,        // invariant: non-empty
    pub bindings: Vec<'a, &'a BoundAttribute<'a>>,
    pub listeners: Vec<'a, &'a BoundEvent<'a>>,
    pub source_span: ParseSourceSpan<'a>,
}
```
> `Icu.vars`/`placeholders` are JS objects used as ordered string maps. Preserve insertion order
> (i18n message generation is order-sensitive) → use a `Vec` of pairs or an order-preserving map.

---

## 4. Algorithm Walkthrough (the only behavior in this file)

### 4.1 Double dispatch — `node.visit(visitor)`

Each class implements `visit<Result>(visitor)` that calls the matching `visitor.visitX(this)`.
This is the classic visitor pattern; the node selects the method. In Rust this is replaced by
`match node { TNode::Element(e) => visitor.visit_element(e), ... }` in a single dispatch fn.

### 4.2 `DeferredBlock.visitAll(visitor)` — ordered traversal

```ts
visitAll(visitor) {
  this.visitTriggers(this.definedHydrateTriggers, this.hydrateTriggers, visitor);  // 1. hydrate first
  this.visitTriggers(this.definedTriggers, this.triggers, visitor);                // 2. regular
  this.visitTriggers(this.definedPrefetchTriggers, this.prefetchTriggers, visitor);// 3. prefetch
  visitAll(visitor, this.children);                                                // 4. children
  const remainingBlocks = [placeholder, loading, error].filter(x => x !== null);
  visitAll(visitor, remainingBlocks);                                              // 5. sub-blocks
}
// visitTriggers maps the cached key array to triggers[k]! and calls visitAll.
```
Order is significant ("Visit the hydrate triggers first to match their insertion order").
Port: keep this exact ordering — hydrate, then main, then prefetch, then children, then
`[placeholder, loading, error]` filtered for `Some`.

### 4.3 `RecursiveVisitor` default traversal

A `Visitor<void>` that recurses into children for every container, no-ops for leaves. Per-node
recursion sets (the exact field order matters for visitors that mutate/collect):

- `visitElement`/`visitComponent`: attributes → inputs → outputs → directives → children → references.
- `visitDirective`: attributes → inputs → outputs → references (no children).
- `visitTemplate`: attributes → inputs → outputs → directives → children → references → variables.
- `visitContent`: children.
- `visitDeferredBlock`: delegates to `deferred.visitAll(this)` (§4.2).
- `visitDeferredBlock{Placeholder,Error,Loading}`: children.
- `visitSwitchBlock`: groups. `visitSwitchBlockCaseGroup`: cases → children.
  `visitSwitchBlockCase` / `visitSwitchExhaustiveCheck`: no-op.
- `visitForLoopBlock`: `[item, ...contextVariables, ...children]` then `empty` if present (one
  combined `visitAll`). `visitForLoopBlockEmpty`: children.
- `visitIfBlock`: branches. `visitIfBlockBranch`: children, then `expressionAlias?.visit(this)`.
- All attribute/event/text/ref/variable/icu/trigger/unknown/let visits: no-op.

Port as a `RecursiveVisitor` trait with default method bodies, or a single `walk(node, &mut V)`
free fn. Reproduce field ordering exactly.

### 4.4 `visitAll(visitor, nodes) -> Result[]`

```ts
if (visitor.visit) { for (node of nodes) visitor.visit(node); }   // generic hook, ignores result accumulation
else { for (node of nodes) { const r = node.visit(visitor); if (r) result.push(r); } }
return result;
```
Two gotchas to reproduce: (1) when the optional generic `visit?` hook exists, results are NOT
accumulated (returns empty array); (2) in the normal path, **falsy** results (incl. `undefined`,
`null`, `0`, `''`, `false`) are dropped from the result array.

```rust
pub fn visit_all<'a, R, V: Visitor<'a, R>>(visitor: &mut V, nodes: &[TNode<'a>]) -> Vec<R>
where R: Truthy {
    let mut out = Vec::new();
    if let Some(hook) = visitor.generic_hook() {
        for n in nodes { hook(visitor, *n); }
    } else {
        for n in nodes {
            let r = dispatch(*n, visitor);
            if r.is_truthy() { out.push(r); }
        }
    }
    out
}
```
> The "drop falsy" semantics are JS-specific. In Rust the common `R` is `()` (RecursiveVisitor)
> or `Option<&Node>` (transform visitors). Model the accumulating variant as
> `R = Option<TNode>` and push only `Some(_)` — that matches the real usage (transform passes).

---

## 5. Dependencies on Other Compiler Modules

| Import | From | Used by | Port module |
|---|---|---|---|
| `SecurityContext` | `../core` (re-exports `schema/dom_security_schema`) | `BoundAttribute` | enum, inline |
| `AST`, `ASTWithSource` | `../expression_parser/ast` | bound values, expressions, triggers | expression-AST spec |
| `BindingType` | `../expression_parser/ast` | `BoundAttribute.type` | enum, inline |
| `BoundElementProperty` | `../expression_parser/ast` | `BoundAttribute.fromBoundElementProperty` | expression-AST spec |
| `LiteralMap` | `../expression_parser/ast` | `ViewportDeferredTrigger.options` | expression-AST spec |
| `ParsedEvent`, `ParsedEventType` | `../expression_parser/ast` | `BoundEvent` | expression-AST spec |
| `I18nMeta` | `../i18n/i18n_ast` | optional `i18n` on many nodes | i18n-AST spec |
| `ParseSourceSpan` | `../parse_util` | every node | parse-util spec |

Reverse deps (who consumes this): `render3/r3_template_transform.ts` (producer),
the template_pipeline ingest, the i18n extractors, the language service, and the template type
checker. This is a **foundational** module — port it early.

---

## 6. ɵɵ Instructions / Output Emitted

**None.** This module defines no emission and references no `o.*` output AST or `ɵɵ`
instruction. It is the input IR; instruction emission happens in downstream view-compiler /
template_pipeline modules. (The OXC codegen layer is irrelevant to porting *this* file — it has
no `o.Expression` / `o.Statement` output.)

---

## 7. Edge Cases, Gotchas & Version Sensitivity

1. **`Comment` and `HostElement` throw on `visit()`.** `Comment` is only collected at top level
   when `Render3ParseOptions.collectCommentNodes` is set; `HostElement` is type-check-only and
   "cannot be produced from a user's template." Do not put them in the normal traversal. In Rust,
   either exclude from `TNode` or `unreachable!()` in dispatch.
2. **`HostElement` ctor invariant:** throws if `tagNames.length === 0`. Enforce in constructor
   fn (`debug_assert!` + error).
3. **`keySpan` optionality varies** (see §3.2). Don't unify to one `Option` type blindly.
4. **`BlockNode` is not a `Node`** — it has no `visit`/`sourceSpan` in the `Node` interface sense
   (it does have `sourceSpan` field but isn't visitable). Don't make it a `TNode` variant.
5. **`fromBoundElementProperty` / `fromParsedEvent` throw** on missing `keySpan`. Port as
   `Result`/`Option` returning constructors or panicking factories matching TS.
6. **`fromParsedEvent` target/phase derivation** is type-driven: `target` set only for
   `ParsedEventType.Regular`, `phase` only for `ParsedEventType.LegacyAnimation`. Note
   `TwoWay` and `Animation` get neither.
7. **DeferredBlock trigger ordering** relies on `Object.keys` insertion order (§3.4/§4.2). This
   is subtle and easy to get wrong in Rust if you use a fixed-field struct. Verify against golden
   output; keep an explicit order vector if needed.
8. **`Icu.vars`/`placeholders` are ordered maps** keyed by string (§3.5) — preserve order.
9. **Version churn (Angular-internal, no semver guarantee):**
   - `Component` and `Directive` nodes are recent additions for **selectorless components**
     (v19+/v20+). `Component` has `componentName` + `tagName` + `fullName`; `Directive` carries
     its own attrs/inputs/outputs/refs. If targeting an older Angular these may be absent — the
     spec is pinned to **22.1.0-next.0** which includes them.
   - `SwitchBlockCaseGroup`, `SwitchExhaustiveCheck`, and `SwitchBlock.unknownBlocks` /
     `exhaustiveCheck` are newer than the original `@switch` shape.
   - `NeverDeferredTrigger`, `hydrateTriggers`, and `hydrateSpan` on `DeferredTrigger` are part
     of the hydration/`@defer (hydrate ...)` work — relatively new.
   - `LetDeclaration` (`@let`) is v18.1+.
   - `DeferredTrigger` got `whenOrOnSourceSpan` (renamed/added) and `prefetchSpan`/`hydrateSpan`
     over time; field order in constructors has shifted between versions — pin to source.
10. **`visitAll` drops falsy results** and **skips accumulation when `visit?` hook present**
    (§4.4). Both behaviors are load-bearing for transform passes.
11. **`Element.isVoid` vs `isSelfClosing`** are distinct: `isVoid` = HTML void element (e.g.
    `<br>`), `isSelfClosing` = author wrote `/>`. `Element` has both; `Template`/`Content`/
    `Component` have only `isSelfClosing`.
12. **`ForLoopBlock.expression`/`trackBy` are `ASTWithSource`** (not plain `AST`) — different
    wrapper than most bound values; keep the distinction.

---

## 8. Port Plan (Rust / OXC)

### 8.1 Representation choice
- **Replace class hierarchy + double-dispatch with a tagged enum `TNode<'a>` + `match`-based
  dispatch.** This is the single biggest structural decision. It is faster and idiomatic and
  removes the need for trait objects.
- **Replace `extends BlockNode` inheritance with a composed `BlockSpans<'a>` field.**
- **Collapse the 8 `DeferredTrigger` subclasses into one `DeferredTrigger` payload enum +
  `TriggerSpans<'a>`** (§3.4).
- Arena-allocate everything via `oxc_allocator::Allocator`; collections are
  `oxc_allocator::Vec<'a, T>`; strings are `&'a str` (arena-interned or borrowed from source).

### 8.2 What to reuse from OXC
- `oxc_allocator::{Allocator, Vec, Box}` for arena allocation of nodes and lists.
- `oxc_span` concepts are analogous to `ParseSourceSpan`, but Angular's span is richer (it
  references a `ParseSourceFile`, has `fullStart`, `details`). **Do not** reuse `oxc_span::Span`
  directly for `ParseSourceSpan`; define Angular's own span in the `parse_util` port and just
  follow oxc's arena patterns. `oxc_span::Span` (byte offsets) can back the low-level offsets if
  desired.
- This module does **not** touch `oxc_ast` (the JS AST) or `oxc_codegen` — those are for the
  *output* JS, which this file does not produce. Reuse is limited to the allocator.

### 8.3 Visitor port
- Define `trait Visitor<'a, R>` with one method per node + optional generic hook, mirroring the
  TS interface.
- Provide a `walk_*` free-function family (or a `RecursiveVisitor` default impl via a trait with
  provided methods) reproducing §4.3 field ordering exactly.
- Provide `visit_all` reproducing §4.4 semantics (truthy filtering, hook short-circuit) — but
  for the common `R = ()` / `R = Option<TNode>` cases, specialize.
- Reproduce `DeferredBlock::visit_all` ordering (§4.2) exactly.

### 8.4 Constructors / factories
- Port `BoundAttribute::from_bound_element_property` and `BoundEvent::from_parsed_event`
  including the keySpan checks and target/phase derivation.

### 8.5 Complexity & ordering
- **Estimated complexity: LOW–MEDIUM.** It is almost entirely data definitions; the only logic
  is the visitor traversal (mechanical) and two small factories. The volume is large (~30 node
  types) and the lifetime/arena plumbing plus enum-flattening decisions require care, which
  pushes it above trivial. No algorithms, no codegen.
- **Ordering vs other modules:** Port **early/foundational**. It depends only on
  `parse_util` (`ParseSourceSpan`), `expression_parser/ast` (`AST`, enums), `i18n/i18n_ast`
  (`I18nMeta`), and `core` (`SecurityContext`). Those four (especially the expression AST and
  parse_util span types) should be ported first or stubbed as opaque handles. Once this t-AST
  exists, both the producer (`r3_template_transform`) and the consumers (template pipeline /
  view compiler) can be ported against it.
- **Suggested sequence:** `parse_util` span types → `SecurityContext`/`BindingType`/
  `ParsedEventType` enums → `expression_parser/ast` (or stub) → `i18n_ast` (or stub) →
  **this module** → `r3_template_transform` → template pipeline.

### 8.6 Risks
- Getting the DeferredBlock trigger insertion-order semantics wrong (§7.7).
- Mismatched `keySpan`/`valueSpan` optionality (§7.3).
- `ASTWithSource` vs `AST` distinction for `@for` (§7.12).
- Version drift in deferred-trigger / selectorless node fields — pin strictly to 22.1.0-next.0.
