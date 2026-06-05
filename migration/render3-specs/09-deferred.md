# Port Spec 09 — `@defer` Blocks and Triggers

**Source modules** (Angular 22.1.0-next.0, `packages/compiler/src`):

- `render3/r3_deferred_blocks.ts`
- `render3/r3_deferred_triggers.ts`

Target: Rust port using the OXC toolchain (`oxc_ast` `AstBuilder` for AST construction,
`oxc_codegen` for emission). This module is part of **template HTML → render3 template AST (`t.*`)**
construction — it runs at *parse* time, not at *instruction emission* time.

---

## 1. Purpose & Role in the Compilation Pipeline

These two files are the **parser** for Angular control-flow `@defer` blocks (and their
connected `@placeholder`, `@loading`, `@error` blocks) plus the **trigger microsyntax**
(`on idle`, `on timer(500)`, `when cond`, `prefetch on …`, `hydrate on …`, `hydrate never`).

Position in the pipeline:

```
raw template string
  → ml_parser (HTML lexer/parser)          → html.* AST  (html.Block, html.BlockParameter)
  → r3_template_transform (html → render3) → calls createDeferredBlock(...)   ← THIS MODULE
      → r3_deferred_blocks.ts: structural parse of @defer + connected blocks
      → r3_deferred_triggers.ts: micro-syntax parse of trigger params (on/when/hydrate/prefetch)
  → produces t.DeferredBlock (+ t.DeferredBlock*Trigger, t.DeferredBlock{Placeholder,Loading,Error})
  → later: template_pipeline ingests t.DeferredBlock → ɵɵdefer / ɵɵdeferOn* instructions
```

So this module produces **template-AST nodes only**. It emits **no `ɵɵ` instructions itself**;
the downstream `template_pipeline` (a different spec) reads the `t.DeferredBlock` produced here
and lowers it to runtime instructions. This is important for scoping the port: this module is
pure parsing + validation + AST node construction.

The HTML parser is responsible for identifying that a block is named `defer` and gathering the
"connected" blocks (`@placeholder`/`@loading`/`@error`); this module is invoked with the primary
`@defer` `html.Block` and the array of connected `html.Block`s.

---

## 2. Public API (exact signatures)

### `r3_deferred_blocks.ts`

```ts
/**
 * Predicate function that determines if a block with
 * a specific name cam be connected to a `defer` block.
 */
export function isConnectedDeferLoopBlock(name: string): boolean;
// returns name === 'placeholder' || name === 'loading' || name === 'error'

/** Creates a deferred block from an HTML AST node. */
export function createDeferredBlock(
  ast: html.Block,
  connectedBlocks: html.Block[],
  visitor: html.Visitor,
  bindingParser: BindingParser,
): {node: t.DeferredBlock; errors: ParseError[]};
```

Internal (non-exported) helpers in this file:
`parseConnectedBlocks`, `parsePlaceholderBlock`, `parseLoadingBlock`, `parseErrorBlock`,
`parsePrimaryTriggers`.

### `r3_deferred_triggers.ts`

```ts
/** Parses a `when` deferred trigger. */   // NOTE: doc comment says "when" but this is the NEVER parser
export function parseNeverTrigger(
  {expression, sourceSpan}: html.BlockParameter,
  triggers: t.DeferredBlockTriggers,
  errors: ParseError[],
): void;

/** Parses a `when` deferred trigger. */
export function parseWhenTrigger(
  {expression, sourceSpan}: html.BlockParameter,
  bindingParser: BindingParser,
  triggers: t.DeferredBlockTriggers,
  errors: ParseError[],
): void;

/** Parses an `on` trigger */
export function parseOnTrigger(
  {expression, sourceSpan}: html.BlockParameter,
  bindingParser: BindingParser,
  triggers: t.DeferredBlockTriggers,
  errors: ParseError[],
  placeholder: t.DeferredBlockPlaceholder | null,
): void;

/** Gets the index within an expression at which the trigger parameters start. */
export function getTriggerParametersStart(value: string, startPosition = 0): number;

/**
 * Parses a time expression from a deferred trigger to
 * milliseconds. Returns null if it cannot be parsed.
 */
export function parseDeferredTime(value: string): number | null;
```

Internal (non-exported) members in this file:
`enum OnTriggerType`, `type ReferenceTriggerValidator`, `interface ParsedParameter`,
`class OnTriggerParser`, `class DynamicAstValidator`, plus helpers
`getPrefetchSpan`, `getHydrateSpan`, `trackTrigger`,
`createIdleTrigger`, `createTimerTrigger`, `createImmediateTrigger`,
`createHoverTrigger`, `createInteractionTrigger`, `createViewportTrigger`,
`validatePlainReferenceBasedTrigger`, `validateHydrateReferenceBasedTrigger`.

---

## 3. Key Data Structures & Proposed Rust Mapping

### 3.1 Inputs (from `ml_parser/ast.ts`)

```ts
export class Block extends NodeWithI18n {
  constructor(
    public name: string,
    public parameters: BlockParameter[],
    public children: Node[],
    sourceSpan: ParseSourceSpan,
    public nameSpan: ParseSourceSpan,
    public startSourceSpan: ParseSourceSpan,
    public endSourceSpan: ParseSourceSpan | null = null,
    i18n?: I18nMeta,
  ) { ... }
}

export class BlockParameter implements BaseNode {
  constructor(
    public expression: string,
    public sourceSpan: ParseSourceSpan,
  ) {}
}
```

`ParseSourceSpan` has `.start` / `.end` `ParseLocation`s; `ParseLocation.moveBy(delta)` returns a
new location offset by `delta` characters (handling line/col), and `.offset` is the absolute
character offset.

### 3.2 Trigger micro-types (in `r3_deferred_triggers.ts`)

```ts
enum OnTriggerType {
  IDLE = 'idle', TIMER = 'timer', INTERACTION = 'interaction',
  IMMEDIATE = 'immediate', HOVER = 'hover', VIEWPORT = 'viewport', NEVER = 'never',
}

type ReferenceTriggerValidator =
  (type: OnTriggerType, parameters: ParsedParameter[]) => void;

interface ParsedParameter {
  expression: string; // raw text of the parameter
  start: number;      // index within the trigger where the parameter starts
}
```

Rust:

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
enum OnTriggerType { Idle, Timer, Interaction, Immediate, Hover, Viewport, Never }
impl OnTriggerType { fn as_str(self) -> &'static str { /* "idle" etc. */ } }

struct ParsedParameter<'a> {
    expression: &'a str, // borrow of the source-arena expression slice
    start: u32,          // byte/char offset within the trigger expression
}

// Validator is a small enum (no closures) chosen by trigger flavour:
enum ReferenceTriggerValidator { Plain, Hydrate }
impl ReferenceTriggerValidator {
    fn validate(&self, ty: OnTriggerType, params: &[ParsedParameter]) -> Result<(), TriggerError> { ... }
}
```

> Use an enum, not a `Box<dyn Fn>`, for the validator — there are exactly two variants
> (`validatePlainReferenceBasedTrigger`, `validateHydrateReferenceBasedTrigger`).

### 3.3 Output AST nodes (from `render3/r3_ast.ts`)

```ts
export abstract class DeferredTrigger implements Node {
  constructor(
    public nameSpan: ParseSourceSpan | null,
    public sourceSpan: ParseSourceSpan,
    public prefetchSpan: ParseSourceSpan | null,
    public whenOrOnSourceSpan: ParseSourceSpan | null,
    public hydrateSpan: ParseSourceSpan | null,
  ) {}
}

export class BoundDeferredTrigger extends DeferredTrigger { public value: AST; /* `when` */ }
export class NeverDeferredTrigger extends DeferredTrigger {}
export class IdleDeferredTrigger extends DeferredTrigger { public timeout: number | null; }
export class ImmediateDeferredTrigger extends DeferredTrigger {}
export class HoverDeferredTrigger extends DeferredTrigger { public reference: string | null; }
export class TimerDeferredTrigger extends DeferredTrigger { public delay: number; }
export class InteractionDeferredTrigger extends DeferredTrigger { public reference: string | null; }
export class ViewportDeferredTrigger extends DeferredTrigger {
  readonly reference: string | null;
  readonly options: LiteralMap | null;
}

export interface DeferredBlockTriggers {
  when?: BoundDeferredTrigger;
  idle?: IdleDeferredTrigger;
  immediate?: ImmediateDeferredTrigger;
  hover?: HoverDeferredTrigger;
  timer?: TimerDeferredTrigger;
  interaction?: InteractionDeferredTrigger;
  viewport?: ViewportDeferredTrigger;
  never?: NeverDeferredTrigger;
}

export class DeferredBlockPlaceholder extends BlockNode {
  public children: Node[]; public minimumTime: number | null; /* + spans, i18n */
}
export class DeferredBlockLoading extends BlockNode {
  public children: Node[]; public afterTime: number|null; public minimumTime: number|null;
}
export class DeferredBlockError extends BlockNode { public children: Node[]; }

export class DeferredBlock extends BlockNode {
  readonly triggers: Readonly<DeferredBlockTriggers>;
  readonly prefetchTriggers: Readonly<DeferredBlockTriggers>;
  readonly hydrateTriggers: Readonly<DeferredBlockTriggers>;
  public children: Node[];
  public placeholder: DeferredBlockPlaceholder | null;
  public loading: DeferredBlockLoading | null;
  public error: DeferredBlockError | null;
  public mainBlockSpan: ParseSourceSpan;  // span of just the @defer{} block
  // sourceSpan covers @defer + all connected blocks (computed by createDeferredBlock)
  // plus cached defined*Triggers key arrays for visit ordering
}
```

`BlockNode` base = `{ nameSpan, sourceSpan, startSourceSpan, endSourceSpan }`.

Proposed Rust mapping — represent the trigger as **one enum** carrying the shared span bundle.
The TS class hierarchy collapses cleanly because the only per-variant data are: `timeout`,
`delay`, `reference`, `value` (an expression AST), `options` (a `LiteralMap` expression AST).

```rust
struct TriggerSpans<'a> {
    name_span: Option<Span>,           // null for `when`
    source_span: Span,
    prefetch_span: Option<Span>,
    when_or_on_source_span: Option<Span>,
    hydrate_span: Option<Span>,
}

enum DeferredTrigger<'a> {
    Bound  { value: Expr<'a>, spans: TriggerSpans<'a> },        // `when`
    Never  { spans: TriggerSpans<'a> },
    Idle   { timeout: Option<f64>, spans: TriggerSpans<'a> },
    Immediate { spans: TriggerSpans<'a> },
    Hover  { reference: Option<&'a str>, spans: TriggerSpans<'a> },
    Timer  { delay: f64, spans: TriggerSpans<'a> },
    Interaction { reference: Option<&'a str>, spans: TriggerSpans<'a> },
    Viewport { reference: Option<&'a str>, options: Option<LiteralMap<'a>>, spans: TriggerSpans<'a> },
}

// The TS object-as-map (`DeferredBlockTriggers`) keyed by trigger kind. Use a struct of Options
// to preserve "duplicate trigger" detection and insertion-order semantics cheaply.
#[derive(Default)]
struct DeferredBlockTriggers<'a> {
    when:        Option<DeferredTrigger<'a>>,
    idle:        Option<DeferredTrigger<'a>>,
    immediate:   Option<DeferredTrigger<'a>>,
    hover:       Option<DeferredTrigger<'a>>,
    timer:       Option<DeferredTrigger<'a>>,
    interaction: Option<DeferredTrigger<'a>>,
    viewport:    Option<DeferredTrigger<'a>>,
    never:       Option<DeferredTrigger<'a>>,
    // For visit-order parity, track insertion order of populated keys:
    order: Vec<TriggerKind>,
}

enum TriggerKind { When, Idle, Immediate, Hover, Timer, Interaction, Viewport, Never }

struct DeferredBlock<'a> {
    children: Vec<'a, TmplNode<'a>>,
    triggers: DeferredBlockTriggers<'a>,
    prefetch_triggers: DeferredBlockTriggers<'a>,
    hydrate_triggers: DeferredBlockTriggers<'a>,
    placeholder: Option<DeferredBlockPlaceholder<'a>>,
    loading: Option<DeferredBlockLoading<'a>>,
    error: Option<DeferredBlockError<'a>>,
    name_span: Span, source_span: Span, main_block_span: Span,
    start_source_span: Span, end_source_span: Option<Span>,
    i18n: Option<I18nMeta<'a>>,
}
```

Notes on lifetimes/arena:
- `value` and `options` are **expression-parser AST** nodes (`AST` / `LiteralMap`). These are
  produced by `BindingParser` and live in whatever arena the binding parser uses — make them
  `'a`-bound borrows or arena boxes consistent with the expression-AST port (separate spec).
- `reference`/`expression` strings: keep as `&'a str` slices of the original template source
  (the TS code slices the original `expression` string), avoiding allocation.
- `Span` here is Angular's `ParseSourceSpan` (start/end with line/col), **not** OXC's byte-only
  `oxc_span::Span`. Keep a dedicated `ParseSourceSpan` type — it carries `ParseLocation` with
  `moveBy` arithmetic that this code depends on heavily.
- `DeferredBlockTriggers` `order: Vec` is required to reproduce `Object.keys(...)` insertion
  order, which `DeferredBlock.visitAll` relies on.

---

## 4. Algorithm Walkthrough

### 4.1 `createDeferredBlock(ast, connectedBlocks, visitor, bindingParser)` — main entry

1. `errors: ParseError[] = []`.
2. `parseConnectedBlocks(connectedBlocks, errors, visitor)` → `{placeholder, loading, error}`.
3. `parsePrimaryTriggers(ast, bindingParser, errors, placeholder)` →
   `{triggers, prefetchTriggers, hydrateTriggers}`.
4. Compute the combined span covering `@defer` **plus all connected blocks**:
   - default `lastEndSourceSpan = ast.endSourceSpan`, `endOfLastSourceSpan = ast.sourceSpan.end`.
   - if `connectedBlocks.length > 0`, take the **last** connected block's `endSourceSpan` / `sourceSpan.end`.
   - `sourceSpanWithConnectedBlocks = new ParseSourceSpan(ast.sourceSpan.start, endOfLastSourceSpan)`.
5. Construct `new t.DeferredBlock(...)` passing:
   - `html.visitAll(visitor, ast.children, ast.children)` (recursively transform children),
   - the three trigger maps, the three connected blocks,
   - `nameSpan`, `sourceSpanWithConnectedBlocks` (as `sourceSpan`),
     `ast.sourceSpan` (as `mainBlockSpan`), `startSourceSpan`, `lastEndSourceSpan`, `i18n`.
6. Return `{node, errors}`. **Errors are collected, never thrown** out of the entry point.

> Subtlety: `sourceSpan` (full, incl. connected blocks) and `mainBlockSpan` (just `@defer{}`)
> are intentionally different. `DeferredBlock`'s ctor arg order is
> `(children, triggers, prefetch, hydrate, placeholder, loading, error, nameSpan, sourceSpan,
> mainBlockSpan, startSourceSpan, endSourceSpan, i18n)` — note `mainBlockSpan` sits between
> `sourceSpan` and `startSourceSpan`.

### 4.2 `parseConnectedBlocks`

For each connected block:
- wrapped in `try/catch`; any thrown `Error` becomes a `ParseError(block.startSourceSpan, msg)`.
- if `!isConnectedDeferLoopBlock(block.name)` → push `Unrecognized block "@<name>"` and **`break`**
  (stops processing remaining connected blocks).
- switch on name:
  - `placeholder`: if already set → error "can only have one @placeholder block"; else `parsePlaceholderBlock`.
  - `loading`: same single-instance rule; else `parseLoadingBlock`.
  - `error`: same; else `parseErrorBlock`.

### 4.3 `parsePlaceholderBlock`

- `minimumTime: number | null = null`.
- For each `param`:
  - if `MINIMUM_PARAMETER_PATTERN` (`/^minimum\s/`):
    - if already set → throw "can only have one minimum parameter".
    - `parseDeferredTime(param.expression.slice(getTriggerParametersStart(param.expression)))`;
      if `null` → throw "Could not parse time value of parameter \"minimum\"".
  - else → throw "Unrecognized parameter in @placeholder block: …".
- returns `t.DeferredBlockPlaceholder(children, minimumTime, …spans, i18n)`.

### 4.4 `parseLoadingBlock`

- two slots: `afterTime`, `minimumTime`.
- per param: `AFTER_PARAMETER_PATTERN` (`/^after\s/`) → `afterTime`; `MINIMUM_PARAMETER_PATTERN` →
  `minimumTime`. Each has duplicate-guard and parse-failure throw. Otherwise → "Unrecognized parameter".

### 4.5 `parseErrorBlock`

- if `ast.parameters.length > 0` → throw "@error block cannot have parameters".
- returns `t.DeferredBlockError(children, …spans, i18n)`.

### 4.6 `parsePrimaryTriggers`

- three maps: `triggers`, `prefetchTriggers`, `hydrateTriggers`.
- For each `param.expression`, dispatch by regex (lexer strips leading whitespace, so the keyword
  is at the start):
  | Pattern | Handler | Target map |
  |---|---|---|
  | `/^when\s/` | `parseWhenTrigger` | `triggers` |
  | `/^on\s/` | `parseOnTrigger(..., placeholder)` | `triggers` |
  | `/^prefetch\s+when\s/` | `parseWhenTrigger` | `prefetchTriggers` |
  | `/^prefetch\s+on\s/` | `parseOnTrigger` | `prefetchTriggers` |
  | `/^hydrate\s+when\s/` | `parseWhenTrigger` | `hydrateTriggers` |
  | `/^hydrate\s+on\s/` | `parseOnTrigger` | `hydrateTriggers` |
  | `/^hydrate\s+never(\s*)$/` | `parseNeverTrigger` | `hydrateTriggers` |
  | else | push `ParseError(param.sourceSpan, 'Unrecognized trigger')` | — |
- After the loop: if `hydrateTriggers.never && Object.keys(hydrateTriggers).length > 1` →
  error "Cannot specify additional `hydrate` triggers if `hydrate never` is present".

### 4.7 `parseWhenTrigger`

- `whenIndex = expression.indexOf('when')`; build `whenSourceSpan` = `[start+whenIndex, +4]`.
- `prefetchSpan = getPrefetchSpan(...)`, `hydrateSpan = getHydrateSpan(...)`.
- if `whenIndex === -1` → error. Else:
  - `start = getTriggerParametersStart(expression, whenIndex + 1)`.
  - `bindingParser.parseBinding(expression.slice(start), /*isHostBinding*/ false, sourceSpan,
     sourceSpan.start.offset + start)` → an `ASTWithSource`.
  - `trackTrigger('when', triggers, errors, new t.BoundDeferredTrigger(parsed, sourceSpan, prefetchSpan, whenSourceSpan, hydrateSpan))`.

### 4.8 `parseNeverTrigger`

- finds `'never'`, builds span, gets prefetch/hydrate spans, then
  `trackTrigger('never', …, new t.NeverDeferredTrigger(neverSourceSpan, sourceSpan, prefetchSpan, null, hydrateSpan))`.
  (`whenOrOnSourceSpan = null` for never.)

### 4.9 `parseOnTrigger` + `OnTriggerParser` — the hard part

`parseOnTrigger`:
- `onIndex = expression.indexOf('on')`; build `onSourceSpan`.
- `start = getTriggerParametersStart(expression, onIndex + 1)`.
- `isHydrationTrigger = expression.startsWith('hydrate')`.
- construct `OnTriggerParser` with validator =
  `isHydrationTrigger ? validateHydrateReferenceBasedTrigger : validatePlainReferenceBasedTrigger`,
  then `parser.parse()`.

`OnTriggerParser`:
- ctor tokenizes `expression.slice(start)` via `new Lexer().tokenize(...)` (the **expression-parser**
  lexer, not the HTML lexer).
- `parse()` loop over tokens. Each iteration:
  - current token must be an identifier (`token.isIdentifier()`), else `unexpectedToken` and break.
  - if next token is `,` or this is the last token (`isFollowedByOrLast($COMMA)`) → it's a
    parameterless trigger: `consumeTrigger(token, [])`, then `advance()`.
  - else if followed by `(` (`isFollowedByOrLast($LPAREN)`) → `advance()` to the paren,
    `consumeParameters()`; if errors increased, break; else `consumeTrigger(token, parameters)`,
    `advance()` past `)`.
  - else if not the last token → `unexpectedToken(tokens[index+1])`.
  - `advance()` (the trailing always-advance).
- `consumeParameters()`: expects `(`. Walks tokens tracking a `commaDelimStack` (matching
  `{}`/`[]`/`()` pairs via `COMMA_DELIMITED_SYNTAX`). A `,` at top level (`commaDelimStack` empty)
  splits parameters. Stops at the matching top-level `)`. Each parameter's text is reconstructed
  from the original `expression` slice via `tokenRangeText` (start of first token .. end of last).
  On unbalanced/missing `)` → "Unexpected end of expression"; if a stray token follows that isn't
  `,` → `unexpectedToken`.
- `consumeTrigger(identifier, parameters)`: computes `nameSpan`/`sourceSpan`/`endSpan` via
  `span.start.moveBy(...)` arithmetic. **`prefetch`/`on`/`hydrate` spans are attached only to the
  first trigger** (`isFirstTrigger = identifier.index === 0`); subsequent comma-separated triggers
  get `null` for those. Switch on `identifier.toString()` over `OnTriggerType` to call the
  appropriate `create*Trigger` factory, wrapped in try/catch that converts thrown messages into
  `error(identifier, msg)`.

  > Inconsistency to preserve: for `idle` the call uses the locally-narrowed
  > `prefetchSourceSpan/onSourceSpan/hydrateSourceSpan` (null-on-non-first), whereas the other
  > cases pass `this.prefetchSpan/this.onSourceSpan/this.hydrateSpan` directly (always the
  > original). This asymmetry is in the source; replicate it for byte-exact parity.

### 4.10 Trigger factories (validation rules)

- `createIdleTrigger`: ≤1 param; optional `timeout` from `parseDeferredTime`.
- `createTimerTrigger`: exactly 1 param; `delay` from `parseDeferredTime` (throws if unparseable).
- `createImmediateTrigger`: 0 params.
- `createHoverTrigger`: runs `validator`; `reference = parameters[0]?.expression ?? null`.
- `createInteractionTrigger`: same shape as hover.
- `createViewportTrigger`: runs `validator`; then:
  - 0 params → `reference = options = null`.
  - param doesn't start with `{` → `reference = param.expression`, `options = null`.
  - param starts with `{` → `bindingParser.parseBinding(...)`; require `parsed.ast instanceof LiteralMap`;
    reject spread keys (`key.kind === 'spread'`); reject a `root` property key; find a `trigger`
    property:
      - none → `options = parsed.ast`, `reference = null`.
      - found → value must be `PropertyRead` with `ImplicitReceiver`; `reference = value.name`;
        `options` = a new `LiteralMap` with the `trigger` key/value removed.
  - if hydration trigger and `reference !== null` → throw `"viewport" hydration trigger cannot have a "trigger"`.
  - if `options` present → `DynamicAstValidator.findDynamicNode(options)` must be null
    (only `ASTWithSource`/`LiteralPrimitive`/`LiteralArray`/`LiteralMap` allowed); else throw
    listing the offending node's `constructor.name`.

- `validatePlainReferenceBasedTrigger`: ≤1 param.
- `validateHydrateReferenceBasedTrigger`: `viewport` ≤1 param; all others 0 params.

### 4.11 `trackTrigger`

- if `allTriggers[name]` already present → `ParseError(trigger.sourceSpan, 'Duplicate "<name>" trigger is not allowed')`;
  else `allTriggers[name] = trigger`.

### 4.12 `getTriggerParametersStart` / `parseDeferredTime`

- `getTriggerParametersStart(value, startPosition=0)`: scan from `startPosition`; set
  `hasFoundSeparator` on the first whitespace (`/^\s$/`); return the index of the first
  non-whitespace **after** a separator; `-1` if none.
- `parseDeferredTime(value)`: match `/^\d+\.?\d*(ms|s)?$/`; `parseFloat(time) * (units==='s' ? 1000 : 1)`.
  Note `ms` and bare number both → ×1; only `s` → ×1000. Returns `null` if no match.

---

## 5. Dependencies on Other Compiler Modules

| Dependency | Used for | Port-spec relationship |
|---|---|---|
| `../ml_parser/ast` (`html.Block`, `html.BlockParameter`, `html.Visitor`, `html.visitAll`) | input AST + child transform | HTML-AST port (upstream) |
| `./r3_ast` (`t.*` Deferred* nodes, `DeferredBlockTriggers`) | output AST | render3 template-AST port |
| `../template_parser/binding_parser` (`BindingParser.parseBinding`) | parse `when` expr & `viewport` options | binding-parser port |
| `../expression_parser/ast` (`AST`, `ASTWithSource`, `ImplicitReceiver`, `LiteralArray`, `LiteralMap`, `LiteralPrimitive`, `PropertyRead`, `RecursiveAstVisitor`) | viewport options validation | expression-AST port |
| `../expression_parser/lexer` (`Lexer`, `Token`, `TokenType`) | tokenize `on` microsyntax | expression-lexer port |
| `../chars` (`$LBRACE`,`$RBRACE`,`$LBRACKET`,`$RBRACKET`,`$LPAREN`,`$RPAREN`,`$COMMA`) | bracket/comma matching | small char-constants module |
| `../parse_util` (`ParseError`, `ParseSourceSpan`) | error + span types | core parse-util port |

This module must be ordered **after**: expression lexer, expression AST, binding parser, HTML AST,
render3 template AST (`r3_ast`), and `parse_util`.

---

## 6. ɵɵ Instructions / Output Emitted

**None directly.** This module emits no `ɵɵ` runtime instructions and produces no `o.Expression`
output. It only constructs template-AST (`t.DeferredBlock` and friends).

For downstream context (NOT this module's responsibility — belongs to the template_pipeline spec),
the `t.DeferredBlock` produced here is later lowered to instructions such as `ɵɵdefer`,
`ɵɵdeferOnIdle`, `ɵɵdeferOnImmediate`, `ɵɵdeferOnTimer`, `ɵɵdeferOnHover`,
`ɵɵdeferOnInteraction`, `ɵɵdeferOnViewport`, and their `ɵɵdeferPrefetchOn*` /
`ɵɵdeferHydrateOn*` / `ɵɵdeferHydrateWhen` / `ɵɵdeferHydrateNever` variants, plus
`ɵɵdeferWhen` / `ɵɵdeferPrefetchWhen`. Listing here only so the porter does not conflate the two
phases; do **not** implement instruction emission inside this module.

---

## 7. Edge Cases, Gotchas, Version Sensitivity

1. **`sourceSpan` vs `mainBlockSpan`.** `DeferredBlock.sourceSpan` spans `@defer` + all connected
   blocks; `mainBlockSpan` is just the `@defer{}`. The combined-span computation walks to the
   *last* connected block. Reproduce both exactly.
2. **First-trigger-only span attachment.** In `on hover(x), interaction(y)`, `prefetch`/`on`/
   `hydrate` spans attach to the first trigger only; subsequent ones get `null`. Comment in source
   explicitly flags this as a candidate for a future `OnGroup` AST — i.e. **API-churn risk** in
   later Angular versions.
3. **`idle` factory span asymmetry** (see 4.9) — `idle` uses null-on-non-first spans while other
   trigger factories use the always-original `this.*` spans. Almost certainly a latent bug in the
   source, but must be mirrored for parity unless deliberately fixing.
4. **`parseNeverTrigger` doc comment lies** — it says "Parses a `when` deferred trigger." Ignore the
   comment; it parses `never`.
5. **`hydrate never` exclusivity** — enforced only *after* all params parsed, keyed on
   `Object.keys(hydrateTriggers).length > 1`. In Rust this maps to: count populated fields in the
   hydrate `DeferredBlockTriggers` struct (or `order.len()`), require == 1 when `never` set.
6. **`Object.keys` insertion order** drives `DeferredBlock.visitAll` ordering (hydrate, then
   regular, then prefetch). Preserve insertion order — hence the `order: Vec<TriggerKind>` field.
7. **Comma microsyntax overload** — top-level commas separate triggers (`on idle,timer(500)`), but
   commas inside `()`/`{}`/`[]` are *within* a single parameter. The `commaDelimStack` stack logic
   is the crux; the lexer has already collapsed string literals into single tokens, so strings need
   no special handling.
8. **`parseBinding` offset math** — absolute offsets are computed as
   `sourceSpan.start.offset + start (+ parameters[0].start)`. Off-by-one here breaks language-service
   span mapping. The `whenIndex + 1` / `onIndex + 1` passed to `getTriggerParametersStart` skips into
   the keyword so the separator scan starts after the first keyword char.
9. **`parseDeferredTime` units** — bare number and `ms` are both milliseconds (×1); only `s` is ×1000.
   Regex `^\d+\.?\d*(ms|s)?$` allows trailing dot (`5.`) and rejects leading dot (`.5`).
10. **Error handling discipline** — connected-block and placeholder/loading/error/factory code path
    uses `throw` internally, caught and converted to `ParseError`. Trigger-dispatch and the
    `OnTriggerParser` push `ParseError`s directly. The public entry never throws — it returns
    `{node, errors}`. The Rust port should mirror this: a recoverable-error accumulator
    (`&mut Vec<ParseError>`) plus internal `Result`/panic-free throw-equivalent (`Result<T, String>`
    converted to a `ParseError` at the catch boundaries).
11. **`viewport` options rejections** — spread keys, a `root` key, non-`LiteralMap` options, and
    non-literal (dynamic) option values are all errors. The `trigger` option value must be a bare
    identifier (`PropertyRead` on `ImplicitReceiver`).
12. **`LiteralMap.keys[].kind`** — keys are discriminated (`'property' | 'spread'`); the expression-AST
    port must expose that discriminant. This is a relatively recent Angular shape (object-literal
    spread support) — version-sensitive.
13. **`html.visitAll(visitor, ast.children, ast.children)`** passes `children` as *both* nodes and
    context — match the HTML-visitor port's signature.

---

## 8. Port Plan (Rust / OXC)

### 8.1 What to reuse from OXC

- **Little of OXC's AST applies directly here.** This module works over Angular's *own* HTML AST and
  Angular's *own* expression AST + `ParseSourceSpan`, none of which are OXC types. OXC's
  `AstBuilder`/`oxc_codegen` are relevant only to the *downstream* instruction-emission phase.
- Reuse `oxc_allocator::Allocator` + `oxc_allocator::Vec`/`Box` for arena allocation of the
  Angular template AST (`Vec<'a, TmplNode<'a>>`, boxed expression nodes), keeping all `&'a str`
  slices pointing into the arena-owned source text. This avoids string copies for `reference` and
  parameter `expression` values.
- Do **not** reuse `oxc_span::Span` for `ParseSourceSpan`: Angular spans carry line/column and
  `moveBy` semantics. Implement a dedicated `ParseSourceSpan`/`ParseLocation` in the `parse_util`
  port and depend on it here.

### 8.2 Implementation order within this module

1. Port `getTriggerParametersStart` + `parseDeferredTime` (pure string/number, no deps) — trivially
   unit-testable first.
2. Port the span helpers `getPrefetchSpan`/`getHydrateSpan` (need `ParseSourceSpan::move_by`).
3. Port `DeferredBlockTriggers` struct + `track_trigger` (duplicate detection + order vec).
4. Port the trigger factories (`create_*_trigger`) and validators. `create_viewport_trigger`
   + `DynamicAstValidator` are the heaviest (depend on expression AST + binding parser).
5. Port `OnTriggerParser` (depends on expression lexer/`Token` port). The `consume_parameters`
   bracket-stack loop is the most intricate piece.
6. Port `parseWhenTrigger`, `parseNeverTrigger`, `parseOnTrigger`.
7. Port `parseConnectedBlocks` + `parse*Block` helpers + `parsePrimaryTriggers`.
8. Port `createDeferredBlock` + `isConnectedDeferLoopBlock` last (the orchestrator).

### 8.3 Cross-module ordering

Must come **after** the ports of: `parse_util` (spans/errors), `chars`, expression-parser
**lexer** + **AST**, **binding parser**, **HTML AST/visitor**, and **`r3_ast`** template nodes.
It is a leaf consumer in the html→render3 transform; the html→render3 transform driver
(`r3_template_transform`, separate spec) calls `create_deferred_block` and is its only caller.

### 8.4 Estimated complexity

**Medium.** The block/connected-block/param parsing (blocks file) is straightforward
regex-dispatch + validation. The complexity concentrates in `r3_deferred_triggers.ts`:
- `OnTriggerParser.consumeParameters` bracket/comma stack machine,
- the span arithmetic and first-trigger-only span attachment (easy to get off-by-one),
- `viewport` options validation requiring real expression-AST introspection (`LiteralMap`,
  `PropertyRead`, `ImplicitReceiver`, recursive `DynamicAstValidator`).

The hard prerequisites (expression lexer/AST, binding parser) are separate, larger ports; given
those exist, this module itself is roughly a few hundred lines of mechanical-but-fiddly Rust.
Risk areas: exact span/offset parity (language-service relies on it) and faithfully reproducing
the two documented inconsistencies (idle span asymmetry, never-doc-comment).

### 8.5 Testing strategy

- Golden tests on representative templates: `on idle`, `on timer(500ms)`, `on viewport(ref)`,
  `on viewport({threshold: 0.5})`, `when cond`, `prefetch on hover`, `hydrate never`,
  `hydrate on viewport`, `on idle,timer(1s)`, plus every error path (duplicate trigger, unknown
  trigger, parameter-count violations, viewport `root`/spread/dynamic rejections).
- Assert both the produced AST node fields **and** the resulting `ParseError` spans/messages, since
  downstream (language service) consumes these spans.
