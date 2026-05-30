# Port Spec 07 — `render3/r3_template_transform.ts`

Angular **22.1.0-next.0** · render3 compiler · Rust/OXC port

Source: `packages/compiler/src/render3/r3_template_transform.ts` (1267 lines)

---

## 1. Purpose & role in the compilation pipeline

This module is the **HTML AST → render3 (Ivy) template AST transformer**, often called "the
template transform". It sits between the **ML parser** (which turns raw template text into an
`html.Node[]` tree of elements, text, attributes, expansions, blocks, components and directives)
and the **template pipeline / ingest** stage (which lowers the render3 AST into the TIR/`ɵɵ`
instruction stream).

Pipeline position:

```
template string
  -> HtmlParser / tokenizer            (ml_parser/*)        -> html.Node[]
  -> WhitespaceVisitor, i18n extraction (optional)          -> html.Node[]
  -> htmlAstToRender3Ast  (THIS MODULE)                      -> t.Node[]  (render3 / r3_ast)
  -> ingest / template pipeline                              -> ɵɵ instructions
```

Its single responsibility is **structural transformation and binding classification**: it walks
the HTML tree, recognizes Angular-specific syntax embedded in attribute names
(`[prop]`, `(event)`, `[(banana)]`, `*structural`, `#ref`, `let-x`, `@input`, `bind-`, `on-`,
`bindon-`, `ref-`), splits attributes into static text attributes / bound inputs / outputs /
references / variables, recognizes special elements (`<ng-template>`, `<ng-container>`,
`<ng-content>`, `<script>`, `<style>`, stylesheet `<link>`), recognizes selectorless components
and directives, handles control-flow blocks (`@if`/`@for`/`@switch`/`@defer`/`@let`), ICU
expansions, and `ngNonBindable` regions. It **delegates** all actual expression parsing to the
`BindingParser`. It does **not** emit instructions; it produces an AST plus collected
errors / styles / styleUrls / ngContentSelectors.

---

## 2. Public API (full TypeScript signatures)

### `Render3ParseResult` (exported interface)

```ts
export interface Render3ParseResult {
  nodes: t.Node[];
  errors: ParseError[];
  styles: string[];
  styleUrls: string[];
  ngContentSelectors: string[];
  // Will be defined if `Render3ParseOptions['collectCommentNodes']` is true
  commentNodes?: t.Comment[];
}
```

### `htmlAstToRender3Ast` (exported function — the sole entry point)

```ts
export function htmlAstToRender3Ast(
  htmlNodes: html.Node[],
  bindingParser: BindingParser,
  options: Render3ParseOptions,
): Render3ParseResult;
```

Everything else in the file is **module-private**:

- `interface Render3ParseOptions { collectCommentNodes: boolean; }`
- `class HtmlAstToIvyAst implements html.Visitor` — the main visitor.
- `class NonBindableVisitor implements html.Visitor` — verbatim/passthrough visitor for
  `ngNonBindable` subtrees.
- `const NON_BINDABLE_VISITOR = new NonBindableVisitor();`
- helpers: `addEvents(events, boundEvents)`, `textContents(node): string | null`.

---

## 3. Key data structures and proposed Rust mapping

### 3.1 Module-level constants

```ts
const BIND_NAME_REGEXP = /^(?:(bind-)|(let-)|(ref-|#)|(on-)|(bindon-)|(@))(.*)$/;
// capture-group index constants
KW_BIND_IDX=1; KW_LET_IDX=2; KW_REF_IDX=3; KW_ON_IDX=4; KW_BINDON_IDX=5; KW_AT_IDX=6; IDENT_KW_IDX=7;

const BINDING_DELIMS = {
  BANANA_BOX: {start: '[(', end: ')]'},
  PROPERTY:   {start: '[',  end: ']'},
  EVENT:      {start: '(',  end: ')'},
};
const TEMPLATE_ATTR_PREFIX = '*';

const UNSUPPORTED_SELECTORLESS_TAGS =
  new Set(['link','style','script','ng-template','ng-container','ng-content']);
const UNSUPPORTED_SELECTORLESS_DIRECTIVE_ATTRS = new Set(['ngProjectAs','ngNonBindable']);
```

Rust mapping — avoid a regex on the hot path; hand-write the prefix matcher into an enum:

```rust
enum AttrKind { Bind, Let, Ref, On, Bindon, At, None }

/// Returns (kind, identifier_offset) by matching the fixed prefixes in order.
/// Equivalent to BIND_NAME_REGEXP but allocation-free.
fn classify_attr_prefix(name: &str) -> (AttrKind, usize) { /* match bind-/let-/ref-/#/on-/bindon-/@ */ }

const BANANA_BOX: (&str, &str) = ("[(", ")]");
const PROPERTY:   (&str, &str) = ("[",  "]");
const EVENT:      (&str, &str) = ("(",  ")");
const TEMPLATE_ATTR_PREFIX: char = '*';

// Use phf::phf_set! or a small match for the two unsupported-name sets.
```

### 3.2 `Render3ParseResult` / `Render3ParseOptions`

```rust
pub struct Render3ParseResult<'a> {
    pub nodes: oxc_allocator::Vec<'a, t::Node<'a>>,
    pub errors: Vec<ParseError>,
    pub styles: Vec<String>,
    pub style_urls: Vec<String>,
    pub ng_content_selectors: Vec<String>,
    pub comment_nodes: Option<Vec<t::Comment>>, // Some only if collect_comment_nodes
}

pub struct Render3ParseOptions { pub collect_comment_nodes: bool }
```

`'a` is the OXC arena lifetime: render3 nodes reference `oxc_ast` expression nodes
(via `BindingParser`) which are arena-allocated. `styles`/`styleUrls`/`ngContentSelectors`
are plain owned `String`s (they leave the arena).

### 3.3 `HtmlAstToIvyAst` (the visitor — mutable accumulator)

Fields:

```ts
errors: ParseError[]
styles: string[]
styleUrls: string[]
ngContentSelectors: string[]
commentNodes: t.Comment[]
private inI18nBlock: boolean
private processedNodes: Set<html.Block | html.Text>   // nodes already consumed by a prior sibling
constructor(private bindingParser: BindingParser, private options: Render3ParseOptions)
```

Rust mapping:

```rust
struct HtmlAstToIvyAst<'a, 'b> {
    binding_parser: &'b mut BindingParser<'a>,
    options: Render3ParseOptions,
    errors: Vec<ParseError>,
    styles: Vec<String>,
    style_urls: Vec<String>,
    ng_content_selectors: Vec<String>,
    comment_nodes: Vec<t::Comment>,
    in_i18n_block: bool,
    // identity set: store raw pointers / NodeId of the html nodes, NOT the nodes themselves
    processed_nodes: rustc_hash::FxHashSet<HtmlNodeId>,
}
```

> Gotcha: `processedNodes` is a JS `Set` keyed by **object identity**. In Rust the html AST
> nodes will be arena/index based; key the set on a stable `NodeId`/index or a `*const` pointer,
> not on structural equality. This set tracks connected control-flow blocks and the blank text
> nodes between them so that `visitBlock`/`visitText` return `null` for already-consumed siblings.

### 3.4 The render3 output node types (`r3_ast.ts`, module `t`)

These are produced here but **defined** in `r3_ast.ts` (see spec for that file). The transform
constructs the following classes; each is `Node` (has `sourceSpan` and `visit`). Field lists
abbreviated to what this module sets:

| TS class | key fields set here | Rust sketch |
|---|---|---|
| `Text` | `value:string, sourceSpan` | `struct Text<'a>{ value:&'a str, span:Span }` |
| `BoundText` | `value:AST, sourceSpan, i18n?` | `struct BoundText<'a>{ value:Ast<'a>, span:Span, i18n:Option<I18nMeta<'a>> }` |
| `TextAttribute` | `name, value, sourceSpan, keySpan?, valueSpan?, i18n?` | struct of `&'a str` + spans |
| `BoundAttribute` | via `fromBoundElementProperty(bep, i18n)` | `struct BoundAttribute<'a>{ name, type_:BindingType, security_context, value:Ast<'a>, unit:Option<&'a str>, spans, i18n }` |
| `BoundEvent` | via `fromParsedEvent(e)` | `struct BoundEvent<'a>{ name, type_:ParsedEventType, handler:Ast<'a>, target:Option<&'a str>, phase:Option<&'a str>, spans }` |
| `Element` | name, attributes, inputs, outputs, directives, children, references, isSelfClosing, spans, isVoid, i18n? | struct |
| `Template` | tagName:Option, attributes, inputs, outputs, directives, templateAttrs:`Vec<Either<Bound,Text>>`, children, references, variables, isSelfClosing, spans, i18n? | struct |
| `Content` (`<ng-content>`) | selector, attributes, children, isSelfClosing, spans, i18n? | struct |
| `Component` | componentName, tagName:Option, fullName, attributes, inputs, outputs, directives, children, references, isSelfClosing, spans, i18n? | struct |
| `Directive` | name, attributes, inputs, outputs, references, spans, i18n? | struct |
| `Variable` | name, value, sourceSpan, keySpan, valueSpan? | struct |
| `Reference` | name, value, sourceSpan, keySpan, valueSpan? | struct |
| `LetDeclaration` | name, value:AST, sourceSpan, nameSpan, valueSpan | struct |
| `Icu` | `vars:{[k]:BoundText}, placeholders:{[k]:Text|BoundText}, sourceSpan, i18n?` | struct with `FxHashMap<String, ...>` |
| `Comment` | value, sourceSpan | struct (top-level only, not visitable) |
| `UnknownBlock` | name, sourceSpan, nameSpan | struct |
| control-flow blocks (`IfBlock`, `ForLoopBlock`, `SwitchBlock`, `DeferredBlock`, …) | built by the delegate modules, not directly here | covered in their own specs |

`t.Node` should be a Rust enum:

```rust
pub enum Node<'a> {
    Text(Text<'a>), BoundText(BoundText<'a>), Element(Box<Element<'a>>),
    Template(Box<Template<'a>>), Content(Box<Content<'a>>), Component(Box<Component<'a>>),
    Icu(Box<Icu<'a>>), LetDeclaration(LetDeclaration<'a>),
    IfBlock(..), ForLoopBlock(..), SwitchBlock(..), DeferredBlock(..), UnknownBlock(..),
}
```

`BindingType` and `ParsedEventType` are enums imported from `expression_parser/ast` — port as
`#[repr]` Rust enums.

### 3.5 The `prepareAttributes` return record

An anonymous object returned by `prepareAttributes`. In Rust make it a named struct:

```rust
struct PreparedAttributes<'a> {
    attributes: Vec<t::TextAttribute<'a>>,        // static (non-binding) attrs
    bound_events: Vec<t::BoundEvent<'a>>,
    references: Vec<t::Reference<'a>>,
    variables: Vec<t::Variable<'a>>,              // let- vars (ng-template only)
    template_variables: Vec<t::Variable<'a>>,     // vars from *structural microsyntax
    element_has_inline_template: bool,            // saw a *attr
    parsed_properties: Vec<ParsedProperty<'a>>,
    template_parsed_properties: Vec<ParsedProperty<'a>>, // properties from *structural
    i18n_attrs_meta: FxHashMap<String, I18nMeta<'a>>,
}
```

`categorizePropertyAttributes` returns `{ bound: BoundAttribute[]; literal: TextAttribute[] }`
— a small struct `CategorizedAttrs { bound, literal }`.

---

## 4. Algorithm walkthrough

### 4.1 Entry: `htmlAstToRender3Ast`

```ts
const transformer = new HtmlAstToIvyAst(bindingParser, options);
const ivyNodes = html.visitAll(transformer, htmlNodes, htmlNodes);
const allErrors = bindingParser.errors.concat(transformer.errors);
// assemble Render3ParseResult; attach commentNodes iff options.collectCommentNodes
```

Note: `html.visitAll` is called with **`htmlNodes` as both the node list AND the context**. The
context is the *siblings array*; `visitBlock`/`visitText` rely on it (`visitBlock` calls
`context.indexOf(block)`). Every recursive descent into children passes the children array as the
new context (`html.visitAll(this, element.children, element.children)`). The Rust port must
thread the sibling slice as context through the visitor.

Errors come from **two** sources and are concatenated: the `BindingParser`'s accumulated
`errors` plus the transformer's own `errors`.

### 4.2 `visitElement(element)` — the core

1. **i18n root detection**: `isI18nRootNode(element.i18n)`. If true and we are already
   `inI18nBlock`, report "Cannot mark an element as translatable inside of a translatable
   section." Set `inI18nBlock = true` for the duration of this subtree.
2. **preparse** via `preparseElement(element)`:
   - `SCRIPT` → return `null` (dropped for security).
   - `STYLE` → push `textContents(element)` (only if the element has exactly one text child) into
     `styles`; return `null`.
   - `STYLESHEET` and `isStyleUrlResolvable(hrefAttr)` → push `hrefAttr` into `styleUrls`; return
     `null`.
3. Compute `isTemplateElement = isNgTemplate(element.name)`.
4. `prepareAttributes(element.attrs, isTemplateElement)` → splits attrs (§4.6).
5. `extractDirectives(element)` (§4.8) — selectorless directives attached to the element.
6. **children**: if `preparsedElement.nonBindable` → visit children with `NON_BINDABLE_VISITOR`
   and `.flat(Infinity)` (because `visitBlock` returns arrays). Else recurse with `this`.
7. **build the node**:
   - `NG_CONTENT` → `new t.Content(selector, attrs(all visited verbatim), children, …)` and push
     `selector` into `ngContentSelectors`.
   - `isTemplateElement` (`<ng-template>`) → `categorizePropertyAttributes` then `new t.Template(...)`
     with `bound` as inputs, empty templateAttrs, `variables` (let-), `references`.
   - else → `categorizePropertyAttributes`; if `element.name === 'ng-container'`, reject any
     `BindingType.Attribute` bound attr ("Attribute bindings are not supported on ng-container…").
     Build `new t.Element(...)`.
8. **inline template wrapping**: if `elementHasInlineTemplate` (an `*attr` was present), wrap the
   node via `wrapInTemplate(...)` (§4.7).
9. Reset `inI18nBlock = false` if we set it. Return the node.

### 4.3 `visitComponent(component)` — selectorless components

Mirrors `visitElement` but for the parser's `html.Component` nodes (selectorless syntax, e.g.
`<MyComp/>`). Differences:

- i18n root handling identical.
- Reject when `tagName` is in `UNSUPPORTED_SELECTORLESS_TAGS` ("Tag name … cannot be used as a
  component tag") → return `null`.
- `prepareAttributes(component.attrs, /*isTemplateElement*/ false)`.
- `validateSelectorlessReferences(references)` (§4.10).
- `extractDirectives(component)`.
- nonBindable is detected by literally scanning `component.attrs` for an attr named
  `ngNonBindable` (not via preparse).
- Build `new t.Component(componentName, tagName, fullName, attributes, bound inputs, outputs,
  directives, children, references, …)`.
- Wrap in template if `elementHasInlineTemplate`.

### 4.4 `visitText` / `_visitTextWithInterpolation`

`visitText`: if the node is in `processedNodes` (consumed as blank-between-blocks) → `null`.
Otherwise `_visitTextWithInterpolation(value, span, tokens, i18n)`:

```ts
const valueNoNgsp = replaceNgsp(value);                     // normalize &ngsp; entities
const expr = bindingParser.parseInterpolation(valueNoNgsp, span, interpolatedTokens);
return expr ? new t.BoundText(expr, span, i18n) : new t.Text(valueNoNgsp, span);
```

i.e. text containing `{{…}}` becomes a `BoundText`, plain text becomes `Text`.

### 4.5 `visitExpansion` (ICU) / `visitExpansionCase`

- `visitExpansionCase` always returns `null` (cases are folded into the parent expansion).
- `visitExpansion`: if no `expansion.i18n` → `null` (ICUs only meaningful inside i18n blocks). If
  `i18n` is present but not an i18n root `Message` → **throw** (hard error). Otherwise iterate
  `message.placeholders`: keys beginning with `I18N_ICU_VAR_PREFIX` (`VAR_…`) are trimmed and
  parsed via `parseInterpolationExpression` into `vars[key] = BoundText`; other placeholders go
  through `_visitTextWithInterpolation` into `placeholders[key]`. Returns `new t.Icu(vars,
  placeholders, span, message)`.

### 4.6 `prepareAttributes(attrs, isTemplateElement)`

For each `html.Attribute`:

- Record `attribute.i18n` into `i18nAttrsMeta[name]` if present.
- If name starts with `*` (`TEMPLATE_ATTR_PREFIX`): it's a **structural-directive microsyntax**.
  Only one is allowed per element (else error "Can't have multiple template bindings…"). Set
  `elementHasInlineTemplate = true`. Compute `templateKey = name.slice(1)` and call
  `bindingParser.parseInlineTemplateBinding(key, value, sourceSpan, absoluteValueOffset, [],
  templateParsedProperties, parsedVariables, /*isIvyAst*/ true)`. The absolute value offset is
  `valueSpan.fullStart.offset` or, when there is no value, `sourceSpan.fullStart.offset +
  name.length`. Convert each `ParsedVariable` into `t.Variable` pushed onto `templateVariables`.
- Else call `parseAttribute(...)` (§4.9) which returns `hasBinding`.
- If `!hasBinding && !isTemplateBinding`, the attribute is a plain static attribute → push
  `visitAttribute(attribute)` (a `t.TextAttribute`) into `attributes`.

### 4.7 `wrapInTemplate(node, templateProperties, templateVariables, i18nAttrsMeta, isTemplateElement, isI18nRootElement)`

Builds the synthetic `<ng-template>` that hosts a structural-directive (`*ngIf`-style) node:

- `categorizePropertyAttributes('ng-template', templateProperties, i18nAttrsMeta)` → `templateAttrs`
  is `[...literal, ...bound]`.
- **Hoist** attributes for content-projection: if the wrapped node is `Element` or `Component`,
  copy its `attributes` (minus `animate.*`), `inputs` (minus `BindingType.Animation`), and all
  `outputs` onto the wrapping template. (`filterAnimationAttributes` / `filterAnimationInputs`.)
- i18n: if `isTemplateElement && isI18nRootElement` → `undefined` (avoid duplicate i18n
  instructions; meta is taken from children), else `node.i18n`.
- name of wrapping template: `Component.tagName` for components, `null` for an `ng-template`
  (special-cased so the renderer can tell synthetic from authored), else `node.name`.
- Returns `new t.Template(name, hoistedAttrs, hoistedInputs, hoistedOutputs, /*directives*/ [],
  templateAttrs, /*children*/ [node], /*references*/ [], templateVariables, false, spans, i18n)`.

### 4.8 `extractDirectives(node)` — selectorless directives

For each `directive` in `node.directives`:

- Reject if any attr name starts with `*` ("Shorthand template syntax … not supported inside a
  directive context") or is in `UNSUPPORTED_SELECTORLESS_DIRECTIVE_ATTRS`.
- Reject duplicate directive names ("Cannot apply directive … multiple times").
- `prepareAttributes(directive.attrs, false)`, then `validateSelectorlessReferences(references)`,
  then `categorizePropertyAttributes(elementName, parsedProperties, i18nAttrsMeta)`.
- Every bound input must be `BindingType.Property` or `BindingType.TwoWay`; otherwise error
  ("Binding is not supported in a directive context").
- Build `new t.Directive(name, attributes, inputs, boundEvents, references, spans, undefined)`.

### 4.9 `parseAttribute(...)` — the binding classifier (most intricate method)

1. Match `name` against `BIND_NAME_REGEXP`. If matched, dispatch on which group:
   - `bind-` → `parsePropertyBinding(identifier, value, /*isHost*/false, /*isAnimation*/false, …)`.
   - `let-`  → only on `<ng-template>` (`isTemplateElement`); calls `parseVariable`. Else error
     ("`let-` is only supported on ng-template elements.").
   - `ref-` / `#` → `parseReference`.
   - `on-`   → `parseEvent(... isAssignmentEvent=false ...)`; `addEvents`.
   - `bindon-` → `parsePropertyBinding(... isTwoWay=true ...)` **plus** `parseAssignmentEvent`
     (the `…Change` event).
   - `@`     → `parseLiteralAttr(name, value, …)` (animation literal).
   - Return `true`.
2. Otherwise check delimiter syntax via prefix: `[(` (banana box) → property binding +
   assignment event; `[` → property binding; `(` → event. The guard requires the name to also
   **end** with the matching close delim and be longer than `start+end` (legacy non-empty-identifier
   rule). Returns `true` if matched.
3. Otherwise fall through to `parsePropertyInterpolation(name, value, …, valueTokens)` — handles
   attributes whose *value* contains `{{…}}`. Returns its `hasBinding` boolean.

`createKeySpan(srcSpan, prefix, identifier)` computes the key's span by advancing past the prefix
length (accounts for the stripped `data-` prefix done earlier in `normalizeAttributeName`).

### 4.10 `parseVariable` / `parseReference` / `parseAssignmentEvent` / `validateSelectorlessReferences`

- `parseVariable`: reject `-` in name and empty name; push `t.Variable`.
- `parseReference`: reject `-`, empty, and duplicate reference name ("defined more than once");
  push `t.Reference`.
- `parseAssignmentEvent`: synthesizes `${name}Change` event with `isAssignmentEvent=true`.
- `validateSelectorlessReferences`: in selectorless (component/directive) context references may
  not carry a value ("Cannot specify a value for a local reference in this context") and must be
  unique ("Duplicate reference names are not allowed").

### 4.11 `visitBlock(block, context)` — control flow

1. `index = context.indexOf(block)`; throw if `-1` (visitor invoked with wrong context).
2. If `block` already in `processedNodes` (consumed as a connected block) → `null`.
3. Dispatch on `block.name`:
   - `defer`  → `createDeferredBlock(block, findConnectedBlocks(…, isConnectedDeferLoopBlock), this, bindingParser)`.
   - `switch` → `createSwitchBlock(block, this, bindingParser)`.
   - `for`    → `createForLoop(block, findConnectedBlocks(…, isConnectedForLoopBlock), this, bindingParser)`.
   - `if`     → `createIfBlock(block, findConnectedBlocks(…, isConnectedIfLoopBlock), this, bindingParser)`.
   - default  → produce a `t.UnknownBlock`; if the name is a *connected* block used out of place
     (`@else`, `@empty`, `@placeholder`, …) emit a targeted error and mark it processed, else
     "Unrecognized block @name.".
4. Push `result.errors`, return `result.node`. Note each `create*` returns
   `{node: t.Node | null; errors: ParseError[]}` and **receives `this`** so it can re-enter the
   visitor for child nodes.

`findConnectedBlocks(primaryIndex, siblings, predicate)`: scans forward from `primaryIndex+1`,
skipping `html.Comment`, skipping (and marking processed) blank `html.Text`, stopping at the first
non-block or unrelated block; collects related blocks and marks each processed.

### 4.12 `visitLetDeclaration` / `visitComment` / no-ops

- `visitLetDeclaration`: parse `decl.value` via `parseBinding`; if non-empty parse yields an
  `EmptyExpr`, error "@let declaration value cannot be empty"; return `t.LetDeclaration`.
- `visitComment`: only collected (as `t.Comment`) if `options.collectCommentNodes`; returns `null`.
- `visitDirective`, `visitBlockParameter` → `null`.

### 4.13 `NonBindableVisitor`

A second `html.Visitor` used for `ngNonBindable` subtrees: everything is emitted verbatim as
`t.Element` / `t.TextAttribute` / `t.Text` with **no** binding parsing. `<script>`/`<style>`/
stylesheet elements → `null`. Blocks are re-emitted as literal `Text` from their start/end source
spans (as if block tokenization were off) and may return **arrays** of nodes (hence the
`.flat(Infinity)` in the callers). `@let` is re-emitted as literal `@let name = value;` text.
Components are downgraded to plain `t.Element` using `fullName`.

---

## 5. Dependencies on other compiler modules

- `expression_parser/ast` — `BindingType`, `EmptyExpr`, `ParsedEvent`, `ParsedProperty`,
  `ParsedVariable`, `AST`, `ParsedEventType`, `BoundElementProperty`.
- `i18n/i18n_ast` (`i18n.*`, `I18nMeta`, `Message`).
- `ml_parser/ast` (`html.*`: `Node`, `Element`, `Component`, `Directive`, `Attribute`, `Text`,
  `Comment`, `Expansion`, `ExpansionCase`, `Block`, `BlockParameter`, `LetDeclaration`,
  `Visitor`, `visitAll`).
- `ml_parser/html_whitespaces` (`replaceNgsp`).
- `ml_parser/tags` (`isNgTemplate`).
- `ml_parser/tokens` (`InterpolatedAttributeToken`, `InterpolatedTextToken`).
- `parse_util` (`ParseError`, `ParseErrorLevel`, `ParseSourceSpan`).
- `style_url_resolver` (`isStyleUrlResolvable`).
- `template/pipeline/src/ingest` (`isI18nRootNode`).
- `template_parser/binding_parser` (`BindingParser` — does all expression parsing; methods:
  `parseInterpolation`, `parseInterpolationExpression`, `parseInlineTemplateBinding`,
  `parseLiteralAttr`, `parsePropertyBinding`, `parsePropertyInterpolation`, `parseBinding`,
  `createBoundElementProperty`, `parseEvent`, plus `.errors`).
- `template_parser/template_preparser` (`preparseElement`, `PreparsedElementType`).
- `render3/r3_ast` (`t.*` output nodes).
- `render3/r3_control_flow` (`createForLoop`, `createIfBlock`, `createSwitchBlock`,
  `isConnectedForLoopBlock`, `isConnectedIfLoopBlock`).
- `render3/r3_deferred_blocks` (`createDeferredBlock`, `isConnectedDeferLoopBlock`).
- `render3/view/i18n/util` (`I18N_ICU_VAR_PREFIX`).

These define the porting **order**: ml_parser AST + tokens, parse_util spans, expression_parser
AST + BindingParser, preparser, i18n_ast, r3_ast, then r3_control_flow / r3_deferred_blocks must
all exist before this module compiles.

---

## 6. ɵɵ instructions / output emitted

**None.** This module emits *no* `ɵɵ` runtime instructions. It is a pure AST→AST transform; the
TIR/`ɵɵ` instruction stream is produced later by the ingest / template pipeline. Its only
"outputs" are the `Render3ParseResult` fields: `nodes`, `errors`, `styles`, `styleUrls`,
`ngContentSelectors`, and optional `commentNodes`.

---

## 7. Edge cases, gotchas, version-sensitivity

1. **Identity-keyed `processedNodes`** — must be ported with a stable node id, not value equality.
   This is what prevents connected blocks (`@else`, `@empty`, `@placeholder`) and the whitespace
   text nodes between them from being emitted twice or at the wrong nesting.
2. **Context is the sibling array, not a parent** — `visitBlock` does `context.indexOf(block)`.
   The Rust visitor must carry the current sibling slice as context. `visitAll` is invoked with
   `(this, nodes, nodes)` at the root.
3. **`visitBlock` and `NonBindableVisitor.visitBlock` can return arrays** → callers `.flat(Infinity)`.
   In Rust, model children construction so a single html child may expand to 0..n render3 nodes.
4. **`visitExpansion` throws** (not a soft error) when `i18n` is present but not a `Message`. Port
   as a hard panic/`Result::Err` distinct from the accumulated `ParseError`s.
5. **ICU VAR trailing-space bug workaround** — `key.trim()` is required because `{count, select ,
   …}` produces `"VAR_SELECT "` keys; the trim must be preserved exactly.
6. **Legacy delimiter rule** — `[x]`/`(x)`/`[(x)]` only match when the name *also ends* with the
   close delimiter AND `name.length > start.length + end.length`. There is a standing
   `TODO(ayazhafiz)` about malformed bindings; do not "fix" it — replicate the quirk.
7. **`*attr` value offset fallback** — when an `*attr` has no value, the absolute offset is
   `sourceSpan.fullStart.offset + name.length` (one past the name), used for diagnostics.
8. **`bindon-`/banana-box double dispatch** — produces both a two-way property binding *and* a
   synthetic `${name}Change` assignment event. Order matters for span/diagnostic stability.
9. **ng-container attribute-binding rejection** — only for `BindingType.Attribute` bound attrs.
10. **`<ng-template>` + structural directive + i18n root** — i18n meta is intentionally dropped on
    the wrapping template (`isTemplateElement && isI18nRootElement ? undefined : node.i18n`) to
    avoid duplicate i18n instructions; the meta is taken from children. Subtle; preserve exactly.
11. **Animation hoisting filters** — `animate.*` text attributes and `BindingType.Animation`
    inputs are excluded when hoisting onto a wrapping template.
12. **Selectorless syntax is recent / churning** — `Component`/`Directive` html node kinds,
    `UNSUPPORTED_SELECTORLESS_TAGS`, `UNSUPPORTED_SELECTORLESS_DIRECTIVE_ATTRS`, and
    `validateSelectorlessReferences` are newer (22.x) and carry `TODO(crisbeto)` notes — expect
    API churn; the list contents may change across minor versions. Pin to 22.1.0-next.0.
13. **`@let` empty-value check** only triggers when the parse produced no errors and yields
    `EmptyExpr` — order-sensitive.
14. **Styles extraction quirk** — `textContents` returns content only when the `<style>` has
    *exactly one* text child; multi-child `<style>` silently contributes nothing.
15. **`replaceNgsp` runs before interpolation parsing** on every text node — keep that ordering.

---

## 8. Port plan (Rust / OXC)

### Strategy

This is a **tree-walking transformer**, not a parser, so OXC's parser is irrelevant here. What
OXC provides is the **arena** (`oxc_allocator::Allocator`) and the `oxc_ast` expression nodes that
the `BindingParser` produces and that render3 `BoundAttribute`/`BoundText`/`BoundEvent` reference.
The render3 `t::Node` enum and the html AST are project-defined Angular types, not oxc types.

1. **Port `r3_ast` first** (the `t::Node` enum + structs) — spec 06-ish. Make every node arena-aware
   (`'a`) where it holds expression `Ast<'a>`. `Node` becomes an enum, replacing the OO `visit`
   double-dispatch with either an enum `match` or a generated visitor trait
   (`trait Visitor<'a> { fn visit_element(...); … }` mirroring `r3_ast.Visitor`).
2. **Port the visitor as a struct with `&mut self` accumulators** (`errors`, `styles`,
   `style_urls`, `ng_content_selectors`, `comment_nodes`, `in_i18n_block`, `processed_nodes`).
   Thread the OXC `&'a Allocator` and a `&mut BindingParser<'a>`.
3. **Replace JS dynamic dispatch** (`html.visitAll(this, …)`) with a `match` on the html `Node`
   enum returning `SmallVec<[t::Node; 1]>` (to model the array-returning block/nonbindable cases
   without always allocating).
4. **Replace `BIND_NAME_REGEXP`** with a hand-written prefix matcher (`classify_attr_prefix`) — no
   regex crate needed; it is a fixed alternation of literal prefixes. This is also faster.
5. **`processedNodes`** → `FxHashSet<HtmlNodeId>` where html nodes carry an index/id. If the ml
   parser AST is arena-allocated, a `*const Node` pointer or an arena index works as the key.
6. **Anonymous return objects** (`prepareAttributes`, `categorizePropertyAttributes`) → named
   structs (`PreparedAttributes`, `CategorizedAttrs`).
7. **Error model**: keep `ParseError { span, msg, level }` as an owned struct accumulated in a
   `Vec`. Distinguish the one `visitExpansion` *throw* as a `Result`/`panic` path.
8. **`NonBindableVisitor`** → a separate small struct (or a `non_bindable: bool` mode flag passed
   through the main visitor; a separate struct is closer to the source and easier to verify).

### Reuse from oxc
- `oxc_allocator::{Allocator, Vec, Box}` for arena-bound node storage.
- `oxc_ast`/`oxc_span::Span` for expression spans referenced via `BindingParser` (the render3
  spans themselves are Angular `ParseSourceSpan`, ported separately in `parse_util`).
- Nothing from `oxc_codegen` here (no emission).

### Complexity
**High.** ~1270 lines of intricate, quirk-laden control flow with many error branches, two
visitors, identity-set bookkeeping for connected blocks, and four delegated block constructors.
The logic is mechanical but the edge cases (§7) are numerous and individually load-bearing for
diagnostic byte-offsets, so it needs an extensive golden-fixture test corpus diffed against the
TS compiler.

### Ordering vs other modules
Port after: `parse_util` (spans/errors), `ml_parser` AST + tokens + `html_whitespaces` + `tags`,
`expression_parser` AST + `BindingParser`, `template_preparser`, `style_url_resolver`, `i18n_ast`,
`r3_ast`. Port **alongside** `r3_control_flow` + `r3_deferred_blocks` (mutually referenced — they
take `this` visitor back). Port before: ingest / template pipeline (which consumes the output).
Suggested grouping: do `r3_ast` → this module → `r3_control_flow`/`r3_deferred_blocks` as one
cohesive PR set, since the control-flow modules call back into this visitor.
