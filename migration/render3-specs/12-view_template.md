# Port Spec 12 — `render3/view/template.ts` + `render3/view/util.ts`

**Angular version:** 22.1.0-next.0
**Source files:**
- `packages/compiler/src/render3/view/template.ts`
- `packages/compiler/src/render3/view/util.ts`

> **CRITICAL VERSION NOTE (read first).** The task brief asks for the
> *“TemplateDefinitionBuilder”* (TDB) and *“creation vs update instructions /
> binding slots”*. **In Angular 22 the `TemplateDefinitionBuilder` no longer
> exists in these files (or anywhere in the compiler).** It was deleted when
> the render3 view compiler was rewritten on top of the **template IR pipeline**
> (`packages/compiler/src/template/pipeline/**`). In Angular 22, `template.ts`
> is now a thin **parser front-door** (`parseTemplate`) plus a `BindingParser`
> factory, and `util.ts` is a small bag of **codegen helpers** (temporaries,
> literal serialization, `DefinitionMap`, CSS-selector extraction). The only
> remaining trace of TDB is a handful of *comments* in the pipeline that say
> “we copy TemplateDefinitionBuilder’s scheme …”.
>
> This spec documents what these two files **actually contain in v22**. The
> creation/update-instruction emission and binding-slot allocation that the
> brief refers to now live in the pipeline phases (e.g.
> `template/pipeline/src/phases/var_counting.ts`,
> `.../ingest.ts`, `.../emit.ts`) and the instruction tables in
> `render3/r3_identifiers.ts` / `render3/instruction.ts`. Those are
> separate modules and should get their own specs. References to them are
> flagged in §5 and §8.

---

## 1. Purpose & role in the compilation pipeline

### `template.ts`
Entry point that converts raw template **text** into the render3 template AST
(`t.Node[]`, from `render3/r3_ast.ts`) plus collected metadata. It is the first
stage of component template compilation and is also reused by tooling (the
Angular Language Service, `extract-i18n`, `@angular-eslint`).

Pipeline position:

```
template string
   │  parseTemplate()                       ← THIS FILE
   ▼
HtmlParser.parse()  → html.Node[]           (ml_parser)
   │  I18nMetaVisitor (ids/attrs)
   │  WhitespaceVisitor (optional)
   ▼
htmlAstToRender3Ast() → t.Node[]            (r3_template_transform)
   ▼
ParsedTemplate { nodes, errors, styles, ngContentSelectors, … }
   ▼
ingest()  (template/pipeline)  → CompilationJob → ɵɵ instructions
```

`makeBindingParser()` builds the shared `BindingParser` (expression `Lexer` +
`Parser` + DOM schema) used both here and by callers that re-parse expressions.

### `util.ts`
Low-level **codegen helpers** consumed by the render3 *definition* compiler
(`render3/view/compiler.ts`, `render3/view/query_generation.ts`,
`render3/partial/directive.ts`). It does **not** emit template instructions
itself; it provides:
- temporary-variable allocation for binding expressions (`temporaryAllocator`);
- a `never`-returning visitor guard (`invalid`);
- JS-value → `o.Expression` literal serialization (`asLiteral`);
- the optimized inputs/outputs map serializer
  (`conditionallyCreateDirectiveBindingLiteral`) used by `ɵɵdefineComponent` /
  `ɵɵdefineDirective`;
- `DefinitionMap`, the builder for definition object literals;
- `createCssSelectorFromNode` for directive matching during template compile.

---

## 2. Public API (exact signatures)

### `template.ts`

```ts
export const LEADING_TRIVIA_CHARS = [' ', '\n', '\r', '\t'];

export interface ParseTemplateOptions { /* see §3 */ }

export function parseTemplate(
  template: string,
  templateUrl: string,
  options: ParseTemplateOptions = {},
): ParsedTemplate;

export function makeBindingParser(selectorlessEnabled = false): BindingParser;

export interface ParsedTemplate { /* see §3 */ }
```

Module-private:
```ts
const elementRegistry = new DomElementSchemaRegistry();  // singleton, shared by makeBindingParser
```

### `util.ts`

```ts
export const TEMPORARY_NAME = '_t';
export const CONTEXT_NAME   = 'ctx';
export const RENDER_FLAGS   = 'rf';

export function temporaryAllocator(
  pushStatement: (st: o.Statement) => void,
  name: string,
): () => o.ReadVarExpr;

export function invalid<T>(
  this: t.Visitor,
  arg: o.Expression | o.Statement | t.Node,
): never;

export function asLiteral(value: any): o.Expression;

export function conditionallyCreateDirectiveBindingLiteral(
  map: Record<
    string,
    | string
    | {
        classPropertyName: string;
        bindingPropertyName: string;
        transformFunction: o.Expression | null;
        isSignal: boolean;
      }
  >,
  forInputs?: boolean,
): o.Expression | null;

export class DefinitionMap<T = any> {
  values: {key: string; quoted: boolean; value: o.Expression}[];
  set(key: keyof T, value: o.Expression | null): void;
  toLiteralMap(): o.LiteralMapExpr;
}

export function createCssSelectorFromNode(node: t.Element | t.Template): CssSelector;
```

Module-private:
```ts
function getAttrsForDirectiveMatching(
  elOrTpl: t.Element | t.Template,
): {[name: string]: string};
```

---

## 3. Key data structures + proposed Rust mapping

### 3.1 `ParseTemplateOptions` (template.ts)

All fields optional booleans/objects controlling parse behavior:
`preserveWhitespaces`, `preserveLineEndings`, `preserveSignificantWhitespace`,
`range?: LexerRange`, `escapedString`, `leadingTriviaChars?: string[]`,
`enableI18nLegacyMessageIdFormat`, `i18nNormalizeLineEndingsInICUs`,
`alwaysAttemptHtmlToR3AstConversion`, `collectCommentNodes`,
`enableBlockSyntax`, `enableLetSyntax`, `enableSelectorless`.

**Rust:**
```rust
#[derive(Debug, Default, Clone)]
pub struct ParseTemplateOptions<'a> {
    pub preserve_whitespaces: bool,
    pub preserve_line_endings: bool,
    pub preserve_significant_whitespace: Option<bool>, // tri-state; default true
    pub range: Option<LexerRange>,
    pub escaped_string: bool,
    pub leading_trivia_chars: Option<Vec<&'a str>>,
    pub enable_i18n_legacy_message_id_format: Option<bool>, // default true
    pub i18n_normalize_line_endings_in_icus: bool,
    pub always_attempt_html_to_r3_ast_conversion: bool,
    pub collect_comment_nodes: bool,
    pub enable_block_syntax: Option<bool>,  // default true
    pub enable_let_syntax: Option<bool>,    // default true
    pub enable_selectorless: bool,          // default false
}
```
Use `Option<bool>` only where the source distinguishes “unset” from `false`
via `?? true`/`?? false` (significant-whitespace, legacy-id, block/let syntax).

### 3.2 `ParsedTemplate` (template.ts)

```ts
interface ParsedTemplate {
  preserveWhitespaces?: boolean;
  errors: ParseError[] | null;       // null == no errors; otherwise non-empty
  nodes: t.Node[];
  styleUrls: string[];
  styles: string[];
  ngContentSelectors: string[];
  commentNodes?: t.Comment[];        // present iff options.collectCommentNodes
}
```

**Rust:**
```rust
pub struct ParsedTemplate<'a> {
    pub preserve_whitespaces: Option<bool>,
    pub errors: Option<Vec<ParseError>>,      // None == no errors
    pub nodes: Vec<r3::Node<'a>>,             // arena-bound r3 AST
    pub style_urls: Vec<String>,
    pub styles: Vec<String>,
    pub ng_content_selectors: Vec<String>,
    pub comment_nodes: Option<Vec<r3::Comment<'a>>>,
}
```
The `null`-vs-empty distinction maps cleanly to `Option<Vec<…>>` (= `None`),
mirroring `errors.length > 0 ? errors : null`.

### 3.3 `DefinitionMap<T>` (util.ts)

Ordered list of `{key, quoted, value}`; `set` is **first-write-wins on key
identity but last-value-wins on value** (it finds an existing entry and
overwrites its `value`), skips falsy values, never re-orders. `toLiteralMap`
emits an `o.LiteralMapExpr`.

**Rust:**
```rust
pub struct DefMapEntry<'a> { pub key: String, pub quoted: bool, pub value: Expr<'a> }

#[derive(Default)]
pub struct DefinitionMap<'a> { pub values: Vec<DefMapEntry<'a>> }

impl<'a> DefinitionMap<'a> {
    pub fn set(&mut self, key: &str, value: Option<Expr<'a>>) {
        let Some(value) = value else { return };       // skip null/falsy
        if let Some(e) = self.values.iter_mut().find(|e| e.key == key) {
            e.value = value;                            // overwrite existing
        } else {
            self.values.push(DefMapEntry { key: key.into(), value, quoted: false });
        }
    }
    pub fn to_literal_map(&self) -> LiteralMapExpr<'a> { /* o.literalMap */ }
}
```
The TS generic `<T>` is purely a compile-time key-typo guard
(`key: keyof T`); in Rust drop it or replace with a per-call-site enum. Order
preservation matters — use `Vec`, **not** a hash map.

### 3.4 Inputs/outputs map value shape (util.ts)

The `map` argument to `conditionallyCreateDirectiveBindingLiteral` is
`Record<string, string | { classPropertyName, bindingPropertyName,
transformFunction: o.Expression|null, isSignal: boolean }>`.

**Rust:**
```rust
pub enum BindingMapValue<'a> {
    Simple(String),                       // canonical "dirProp: publicProp"
    Detailed {
        class_property_name: String,
        binding_property_name: String,
        transform_function: Option<Expr<'a>>,
        is_signal: bool,
    },
}
// map: insertion-ordered  Vec<(String, BindingMapValue)>  or IndexMap
```
`Object.getOwnPropertyNames` ⇒ insertion order; preserve it (`IndexMap`/`Vec`).

### 3.5 `InputFlags` (from `core.ts`, bitflags)

```ts
enum InputFlags { None = 0, SignalBased = 1<<0, HasDecoratorInputTransform = 1<<1 }
```
**Rust:** `bitflags! { struct InputFlags: u32 { const NONE=0; const SIGNAL_BASED=1; const HAS_DECORATOR_INPUT_TRANSFORM=2; } }`

### 3.6 `BindingType` (from `expression_parser/ast.ts`)

Used by `getAttrsForDirectiveMatching` (only `Property` and `TwoWay` register
an empty-string attr):
```ts
enum BindingType { Property, Attribute, Class, Style, LegacyAnimation, TwoWay }
```
**Rust:** plain C-like enum (`#[repr(u8)]`).

### 3.7 `CssSelector` (from `directive_matching.ts`)

```ts
class CssSelector {
  element: string | null;
  classNames: string[];
  attrs: string[];              // even=name, odd=value pairs
  notSelectors: CssSelector[];
  setElement(name?: string): void;
  addAttribute(name: string, value=''): void;
  addClassName(name: string): void;
}
```
**Rust:** struct mirroring fields; `attrs` is a flat `Vec<String>` of
name/value pairs (keep the flat encoding — downstream consumers depend on it).

---

## 4. Algorithm walkthrough

### 4.1 `parseTemplate(template, templateUrl, options)`

1. Read `selectorlessEnabled = options.enableSelectorless ?? false`. Build a
   `BindingParser` via `makeBindingParser(selectorlessEnabled)`.
2. `new HtmlParser().parse(template, templateUrl, …)` with options spread over
   defaults: `leadingTriviaChars = LEADING_TRIVIA_CHARS`,
   `tokenizeExpansionForms: true`,
   `tokenizeBlocks = options.enableBlockSyntax ?? true`,
   `tokenizeLet = options.enableLetSyntax ?? true`, `selectorlessEnabled`.
3. **Early-out on HTML errors:** if `!alwaysAttemptHtmlToR3AstConversion` and
   `parseResult.errors.length > 0`, return an empty `ParsedTemplate` carrying
   only those errors (and `commentNodes: []` if `collectCommentNodes`).
4. `rootNodes = parseResult.rootNodes`.
5. `retainEmptyTokens = !(options.preserveSignificantWhitespace ?? true)` —
   computed once and reused across both i18n passes to keep source spans
   consistent.
6. **First i18n pass:** `new I18nMetaVisitor(keepI18nAttrs=!preserveWhitespaces,
   enableI18nLegacyMessageIdFormat, preserveSignificantWhitespace,
   retainEmptyTokens).visitAllWithErrors(rootNodes)`. This scans i18n attrs and
   generates message ids on the *raw* (pre-whitespace-removal) content so that
   `ng extract-i18n` ids stay stable.
7. **Early-out on i18n errors** (same shape as step 3).
8. `rootNodes = i18nMetaResult.rootNodes`.
9. **Whitespace removal** (only if `!preserveWhitespaces`):
   - `html.visitAll(new WhitespaceVisitor(preserveSignificantWhitespace=true,
     originalNodeMap=undefined, requireContext=false), rootNodes)`. Note the
     hard-coded `true` — significant whitespace is *always* preserved here so
     `goog.getMsg`/`$localize` render correctly, deliberately diverging from
     the message ids computed in step 6. The comment documents a known
     `visitAllWithSiblings` context bug they chose not to fix.
   - If `i18nMetaVisitor.hasI18nMeta`, run a **second i18n pass** with
     `keepI18nAttrs=false`, `enableI18nLegacyMessageIdFormat=undefined`,
     `preserveSignificantWhitespace=true`, same `retainEmptyTokens`. Ids from
     the first pass are preserved.
10. `htmlAstToRender3Ast(rootNodes, bindingParser, {collectCommentNodes})`
    → `{ nodes, errors, styleUrls, styles, ngContentSelectors, commentNodes }`.
11. `errors.push(...parseResult.errors, ...i18nMetaResult.errors)` — note the
    transform errors come *first*, then HTML, then i18n.
12. Build `ParsedTemplate`: `errors = errors.length>0 ? errors : null`; attach
    `commentNodes` only if `collectCommentNodes`. Return.

### 4.2 `makeBindingParser(selectorlessEnabled=false)`
`new BindingParser(new Parser(new Lexer(), selectorlessEnabled),
elementRegistry, [])` — `elementRegistry` is a module-level shared
`DomElementSchemaRegistry` singleton; `[]` is the (empty) pipes list.

### 4.3 `temporaryAllocator(pushStatement, name)`
Returns a lazily-memoized thunk. On first call it `pushStatement(new
DeclareVarStmt(TEMPORARY_NAME, undefined, DYNAMIC_TYPE))` and sets
`temp = o.variable(name)`; subsequent calls return the cached `temp`.
**Gotcha:** the declared name is the constant `TEMPORARY_NAME` (`'_t'`) but the
returned variable reads `name` (the argument). They are usually the same, but
the API does not enforce it.

### 4.4 `asLiteral(value)`
Recursive: arrays → `o.literalArr(value.map(asLiteral))`; scalars →
`o.literal(value, o.INFERRED_TYPE)`. (Plain objects fall through to `o.literal`
and are *not* recursed — only arrays are handled specially.)

### 4.5 `conditionallyCreateDirectiveBindingLiteral(map, forInputs?)`
1. `keys = Object.getOwnPropertyNames(map)`; if empty → `return null`.
2. For each key build a literal-map entry:
   - **string value** (canonical `dirProp: publicProp`): `declaredName =
     minifiedName = key`, `publicName = value`, `expressionValue =
     asLiteral(publicName)`.
   - **object value**: `minifiedName = key`,
     `declaredName = classPropertyName`, `publicName = bindingPropertyName`.
     Compute `differentDeclaringName = publicName !== declaredName`,
     `hasDecoratorInputTransform = transformFunction !== null`, and
     `flags` (`SignalBased` if `isSignal`, `HasDecoratorInputTransform` if
     transform present).
     - If `forInputs && (differentDeclaringName || hasDecoratorInputTransform
       || flags !== None)`: emit a **packed array** `[literal(flags),
       asLiteral(publicName)]`, then push `asLiteral(declaredName)` if
       `differentDeclaringName || hasDecoratorInputTransform`, then push
       `transformFunction!` if `hasDecoratorInputTransform`. (Positional
       schema: `[flags, public, declared?, transform?]`.)
     - else `expressionValue = asLiteral(publicName)`.
   - Entry key = `minifiedName`, `quoted = isUnsafeObjectKey(minifiedName)`
     (true when key contains `-` or `.`), `value = expressionValue`.
3. `return o.literalMap(entries)`.

### 4.6 `createCssSelectorFromNode(node)`
- `elementName = node instanceof t.Element ? node.name : 'ng-template'`.
- `attributes = getAttrsForDirectiveMatching(node)`.
- `cssSelector.setElement(splitNsName(elementName)[1])` (strips namespace).
- For each attr name: `addAttribute(splitNsName(name)[1], value)`; if
  `name.toLowerCase() === 'class'`, also `value.trim().split(/\s+/)` →
  `addClassName` each.

### 4.7 `getAttrsForDirectiveMatching(elOrTpl)` (private)
- If `elOrTpl instanceof t.Template && tagName !== 'ng-template'`: only
  `templateAttrs` → each `name → ''`.
- Else: static `attributes` (skip i18n attrs via `isI18nAttribute`) →
  `name → value`; `inputs` where `type` is `Property` or `TwoWay` → `name → ''`;
  all `outputs` → `name → ''`.

---

## 5. Dependencies on other compiler modules

**`template.ts`:**
- `expression_parser/lexer` (`Lexer`), `expression_parser/parser` (`Parser`)
- `ml_parser/ast` (`html.*`, `visitAll`), `ml_parser/html_parser` (`HtmlParser`),
  `ml_parser/html_whitespaces` (`WhitespaceVisitor`), `ml_parser/lexer`
  (`LexerRange`)
- `parse_util` (`ParseError`)
- `schema/dom_element_schema_registry` (`DomElementSchemaRegistry`)
- `template_parser/binding_parser` (`BindingParser`)
- `render3/r3_ast` (`t.Node`, `t.Comment`)
- `render3/r3_template_transform` (`htmlAstToRender3Ast`) ← the heavy lifter
- `render3/view/i18n/meta` (`I18nMetaVisitor`)

**`util.ts`:**
- `core` (`InputFlags`)
- `expression_parser/ast` (`BindingType`)
- `ml_parser/tags` (`splitNsName`)
- `output/output_ast` (`o.*`: `Statement`, `ReadVarExpr`, `DeclareVarStmt`,
  `DYNAMIC_TYPE`, `INFERRED_TYPE`, `Expression`, `LiteralMapExpr`, `variable`,
  `literal`, `literalArr`, `literalMap`)
- `directive_matching` (`CssSelector`)
- `render3/r3_ast` (`t.Element`, `t.Template`, `t.Visitor`)
- `render3/view/i18n/util` (`isI18nAttribute`)
- `render3/util` (`isUnsafeObjectKey`)

**Downstream consumers (so port these after the above):**
`render3/view/compiler.ts`, `render3/view/query_generation.ts`,
`render3/partial/directive.ts` use `util.ts`. The instruction-emitting work
the brief mentions lives in `template/pipeline/**` (`ingest.ts`,
`var_counting.ts`, `emit.ts`, phases) — **separate specs**.

---

## 6. ɵɵ instructions / output emitted

**None directly.** Neither file emits render3 (`ɵɵ…`) creation/update
instructions or allocates binding slots. That is the central thing that
*changed* from older Angular: TDB used to live here and emit
`ɵɵelementStart`/`ɵɵproperty`/`ɵɵtemplate`/etc. — in v22 that responsibility
moved to the template pipeline.

What these files *do* produce:
- `parseTemplate` → an **r3 template AST** (`t.Node[]`) consumed by `ingest()`.
- `util.ts` helpers → **`o.Expression` fragments** that become *definition*
  object literals (the `inputs`/`outputs`/`hostBindings`/`selectors` fields of
  `ɵɵdefineComponent`/`ɵɵdefineDirective`), not template instructions:
  - `conditionallyCreateDirectiveBindingLiteral` → `o.LiteralMapExpr | null`
    (the `inputs:`/`outputs:` map literal, with the packed
    `[flags, public, declared?, transform?]` array for non-trivial inputs).
  - `DefinitionMap.toLiteralMap()` → `o.LiteralMapExpr`.
  - `temporaryAllocator` → injects one `DeclareVarStmt('_t')` statement.
  - `createCssSelectorFromNode` → a `CssSelector` for directive matching.

---

## 7. Edge cases, gotchas, version-sensitivity

- **TDB removed (major churn).** Any port plan/issue tracker referencing a
  Rust `TemplateDefinitionBuilder` must be redirected to the pipeline. Search
  hits for `TemplateDefinitionBuilder` in v22 are only explanatory comments
  inside `template/pipeline/src/**`.
- **Tri-state defaults.** `preserveSignificantWhitespace ?? true`,
  `enableBlockSyntax ?? true`, `enableLetSyntax ?? true`,
  `enableSelectorless ?? false`. `retainEmptyTokens` is the *negation* of the
  resolved significant-whitespace flag and must be identical across both i18n
  passes (source-span reuse depends on it).
- **Two i18n passes diverge intentionally.** Message ids come from the *raw*
  content (pass 1), but the JS output is generated with
  `preserveSignificantWhitespace=true` (pass 2). The code comments call out a
  known `visitAllWithSiblings` context bug that is deliberately *not* fixed to
  avoid changing runtime output. Reproduce the exact pass structure and the
  hard-coded `true`, do not “fix” it.
- **Error short-circuit semantics.** On HTML or i18n errors (when
  `alwaysAttemptHtmlToR3AstConversion` is false) the function returns an empty
  template carrying *only* those errors. The language-service path sets that
  flag true to push through partial ASTs.
- **`errors: null` vs `[]`.** Final `errors` is `null` when empty, else a
  non-empty array. The early-out branches instead return the raw error array
  (never null). Preserve both behaviors.
- **Error ordering** in the success path: transform errors, then
  `parseResult.errors`, then `i18nMetaResult.errors`.
- **`elementRegistry` is a shared module singleton** — stateless/idempotent, so
  it is safe to share, but a Rust port should make `DomElementSchemaRegistry`
  `Sync` or use a `OnceLock`.
- **`asLiteral` only recurses arrays**, not objects. `Date`/`Map`/etc. would be
  passed straight to `o.literal` (effectively unsupported) — match this.
- **`DefinitionMap.set` skips falsy** (`if (value)`), de-dupes by key, keeps
  insertion order. `null` values are dropped silently.
- **`isUnsafeObjectKey` = `/[-.]/.test(key)`** — quote keys containing `-`/`.`.
- **`temporaryAllocator` declared name** is the constant `'_t'`, but the
  returned read uses the `name` argument — keep them distinct in the port.
- **`forInputs` packed-array schema** is positional and order-sensitive:
  `[flags, public, declared?, transform?]`. Getting the conditional pushes
  wrong silently breaks runtime input resolution.
- **`getAttrsForDirectiveMatching` ng-template special case:** a `t.Template`
  whose `tagName !== 'ng-template'` (a structural directive desugared onto a
  real element) uses `templateAttrs` only; everything else uses
  attributes/inputs/outputs.
- **`splitNsName` namespace stripping** is applied to both element name and
  every attribute name in `createCssSelectorFromNode`.

---

## 8. Port plan (Rust / OXC)

### Strategy
These two files are **AST/codegen glue**, not OXC-AST emission. They operate on
Angular’s own `output_ast` (`o.*`) and r3 AST, *not* on `oxc_ast`. The
`oxc_ast` `AstBuilder` + `oxc_codegen` only come into play in the *final*
output stage that lowers `o.Expression` → JS. So for this module:

- **Do not** try to wire `oxc_ast` here. Port against the Rust ports of
  `output_ast` (Spec for `output/output_ast.ts`) and `r3_ast`
  (`render3/r3_ast.ts`). Those are prerequisites.
- **Reuse from OXC:** nothing directly in these two files. (OXC enters later,
  when `o.Expression` trees are emitted to `oxc_ast` nodes for `oxc_codegen`.)

### `util.ts` — port first (LOW complexity)
Pure, mostly stateless functions over `o.Expression`/`r3` AST:
- `TEMPORARY_NAME` / `CONTEXT_NAME` / `RENDER_FLAGS` consts.
- `asLiteral` — recursive over `serde_json::Value`-like input or a dedicated
  `JsValue` enum; only arrays recurse.
- `conditionallyCreateDirectiveBindingLiteral` — straightforward; needs
  `InputFlags` bitflags and insertion-ordered map input.
- `DefinitionMap` — `Vec`-backed (order matters); generic `<T>` dropped.
- `temporaryAllocator` — closure capturing a `&mut Vec<Statement>`-style push;
  in Rust prefer returning a small struct with a `get(&mut self)` method (a
  `FnMut` closure capturing a sink also works but is awkward with the borrow
  checker — a struct is cleaner).
- `createCssSelectorFromNode` + `getAttrsForDirectiveMatching` — depends on the
  `CssSelector` port (`directive_matching.rs`) and `splitNsName`
  (`ml_parser/tags`). Match `t.Template` vs `t.Element` via the r3 AST enum.
- `invalid` — model as a `panic!`/`unreachable!` helper or `Result::Err`; if the
  Rust visitors are exhaustive enums this is largely unnecessary.

**Estimated effort:** ~0.5 day once `output_ast`, `r3_ast`,
`directive_matching`, `core::InputFlags`, and `isUnsafeObjectKey` are available.

### `template.ts` — port after its heavy dependencies (MEDIUM complexity)
`parseTemplate` itself is thin orchestration (~120 lines of control flow), but
it transitively requires the **biggest** dependencies in the compiler:
- `HtmlParser` + `Lexer`/`Parser` (ml_parser, expression_parser)
- `I18nMetaVisitor` (render3/view/i18n/meta)
- `WhitespaceVisitor` (ml_parser/html_whitespaces)
- `htmlAstToRender3Ast` (render3/r3_template_transform)
- `BindingParser`, `DomElementSchemaRegistry`

So `parseTemplate` should be one of the **last** render3 modules ported — it is
the integration point. The orchestration logic itself is low-risk: replicate
the two early-outs, the `retainEmptyTokens` invariant, the dual i18n passes, and
the error concatenation order exactly. Use `Option<Vec<ParseError>>` for
`errors`. `makeBindingParser` is trivial once `BindingParser` exists; make the
`DomElementSchemaRegistry` a `OnceLock` singleton.

### Ordering vs other modules
1. `output_ast`, `r3_ast`, `core` (`InputFlags`), `directive_matching`,
   `ml_parser/tags`, `render3/util` (`isUnsafeObjectKey`), `render3/view/i18n/util`.
2. **`util.ts`** (this spec) — small, unblocks `view/compiler.ts`,
   `query_generation.ts`, `partial/directive.ts`.
3. The big parsers (ml_parser, expression_parser, i18n meta,
   r3_template_transform).
4. **`template.ts::parseTemplate`** (this spec) — integration glue, near-last.
5. The template **pipeline** (`template/pipeline/**`) — where the
   creation/update instructions and binding-slot allocation the brief asked
   about actually live; needs its own specs.

**Overall complexity:** `util.ts` = LOW, `template.ts` = MEDIUM (logic simple,
dependency surface large). Combined module risk: MEDIUM.
