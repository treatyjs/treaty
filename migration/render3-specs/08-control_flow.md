# Port Spec 08 — `render3/r3_control_flow.ts`

Angular version: **22.1.0-next.0**
Source: `packages/compiler/src/render3/r3_control_flow.ts`
Target: Rust port over OXC (`oxc_ast` AstBuilder + `oxc_codegen`).

---

## 1. Purpose & role in the compilation pipeline

This module is the **desugaring layer for the built-in control-flow blocks** `@if` / `@else if` / `@else`, `@for` / `@empty`, and `@switch` / `@case` / `@default`.

It operates **early in the template-parsing pipeline**, *before* any `ɵɵ` runtime instruction emission. Its job is purely a **tree transform**:

```
HTML ML-parser AST (html.Block + html.BlockParameter)
        │   (r3_template_transform.ts dispatches on block.name)
        ▼
r3_control_flow.create*Block(...)
        │   parses block parameters, validates shape, parses expressions via BindingParser
        ▼
render3 template AST (t.IfBlock, t.ForLoopBlock, t.SwitchBlock, ...)
```

The produced `t.*` nodes are later consumed by the template binder / `template_pipeline` (ingest), which is where the actual `ɵɵconditional`, `ɵɵrepeater`, etc. instructions are generated. **This module emits no `ɵɵ` instructions itself** — it only builds AST and accumulates `ParseError[]`.

The single caller is `render3/r3_template_transform.ts` (`visitBlock`), which:
- dispatches on `block.name` (`if`, `for`, `switch`, `defer`),
- gathers "connected" sibling blocks via `findConnectedBlocks(index, siblings, predicate)` using the two exported predicates,
- pushes returned `errors` into the global error list and keeps `node`.

---

## 2. Public API (full TypeScript signatures)

```ts
// Predicate: can a block with this name connect to a @for block? (true iff name === 'empty')
export function isConnectedForLoopBlock(name: string): boolean;

// Predicate: can a block with this name connect to an @if block?
// (true iff name === 'else' OR matches /^else[^\S\r\n]+if/)
export function isConnectedIfLoopBlock(name: string): boolean;

export function createIfBlock(
  ast: html.Block,
  connectedBlocks: html.Block[],
  visitor: html.Visitor,
  bindingParser: BindingParser,
): {node: t.IfBlock | null; errors: ParseError[]};

export function createForLoop(
  ast: html.Block,
  connectedBlocks: html.Block[],
  visitor: html.Visitor,
  bindingParser: BindingParser,
): {node: t.ForLoopBlock | null; errors: ParseError[]};

export function createSwitchBlock(
  ast: html.Block,
  visitor: html.Visitor,
  bindingParser: BindingParser,
): {node: t.SwitchBlock | null; errors: ParseError[]};
```

Note: `createSwitchBlock` takes **no** `connectedBlocks` (cases/defaults are *children* of the switch block, not siblings), unlike `if`/`for` whose branches are sibling blocks.

### Private helpers (not exported, but must be ported)
- `parseForLoopParameters(block, errors, bindingParser)` → result object or `null`
- `validateTrackByExpression(expression, parseSourceSpan, errors): void`
- `parseLetParameter(sourceSpan, expression, span, loopItemName, context, errors): void`
- `validateIfConnectedBlocks(connectedBlocks): ParseError[]`
- `validateSwitchBlock(ast): ParseError[]`
- `parseBlockParameterToBinding(ast: html.BlockParameter, bindingParser, part?: string): ASTWithSource`
- `parseConditionalBlockParameters(block, errors, bindingParser)` → `{expression, expressionAlias}` or `null`
- `stripOptionalParentheses(param: html.BlockParameter, errors): string | null`
- `class PipeVisitor extends RecursiveAstVisitor { hasPipe = false; visitPipe() }`

### Module-level constants (regexes / sets)
```ts
const FOR_LOOP_EXPRESSION_PATTERN = /^\s*([0-9A-Za-z_$]*)\s+of\s+([\S\s]*)/;
const FOR_LOOP_TRACK_PATTERN      = /^track\s+([\S\s]*)/;
const CONDITIONAL_ALIAS_PATTERN   = /^(as\s+)(.*)/;
const ELSE_IF_PATTERN             = /^else[^\S\r\n]+if/;
const FOR_LOOP_LET_PATTERN        = /^let\s+([\S\s]*)/;
const IDENTIFIER_PATTERN          = /^[$A-Z_][0-9A-Z_$]*$/i;
const CHARACTERS_IN_SURROUNDING_WHITESPACE_PATTERN = /(\s*)(\S+)(\s*)/;
const ALLOWED_FOR_LOOP_LET_VARIABLES = new Set(['$index','$first','$last','$even','$odd','$count']);
```

---

## 3. Key data structures + proposed Rust mapping

### 3.1 Inputs (from `ml_parser/ast.ts`)

```ts
export class Block extends NodeWithI18n {
  name: string;
  parameters: BlockParameter[];
  children: Node[];
  sourceSpan: ParseSourceSpan;
  nameSpan: ParseSourceSpan;
  startSourceSpan: ParseSourceSpan;
  endSourceSpan: ParseSourceSpan | null;
  i18n?: I18nMeta;
}
export class BlockParameter implements BaseNode {
  expression: string;          // raw text, e.g. "user of users", "track user.id", "let i = $index", "as foo"
  sourceSpan: ParseSourceSpan;
}
```

### 3.2 Outputs (from `render3/r3_ast.ts`) — all extend `BlockNode(nameSpan, sourceSpan, startSourceSpan, endSourceSpan)`

```ts
class IfBlock        { branches: IfBlockBranch[]; /* + spans */ }
class IfBlockBranch  { expression: AST | null; children: Node[]; expressionAlias: Variable | null; i18n?; /* + spans */ }

class ForLoopBlock {
  item: Variable;                 // loop var (value '$implicit')
  expression: ASTWithSource;      // the iterable
  trackBy: ASTWithSource | null;
  trackKeywordSpan: ParseSourceSpan | null;
  contextVariables: Variable[];   // $index/$first/... plus user `let` aliases
  children: Node[];
  empty: ForLoopBlockEmpty | null;
  mainBlockSpan: ParseSourceSpan; // body-only span (excludes @empty)
  i18n?;                          // + sourceSpan (includes @empty)
}
class ForLoopBlockEmpty { children: Node[]; i18n?; /* + spans */ }

class SwitchBlock {
  expression: AST;
  groups: SwitchBlockCaseGroup[];
  unknownBlocks: UnknownBlock[];          // captured for language-service autocomplete only
  exhaustiveCheck: SwitchExhaustiveCheck | null;
}
class SwitchBlockCase      { expression: AST | null; /* null => @default */ }
class SwitchBlockCaseGroup { cases: SwitchBlockCase[]; children: Node[]; i18n?; }
class SwitchExhaustiveCheck{ expression: AST | null; }  // from `@default never`

class UnknownBlock { name: string; sourceSpan; nameSpan; }
class Variable     { name: string; value: string; sourceSpan; keySpan; valueSpan?; }
```

### Proposed Rust mapping

AST expression types (`AST`, `ASTWithSource`) come from the expression-parser port and are arena-allocated, so any node referencing them carries the `'a` lifetime. `ParseSourceSpan` is a small copy/clone value (two byte offsets + file ref). Use `&'a` arena refs / `oxc_allocator::Box<'a>` for child node vectors. Vectors should be `oxc_allocator::Vec<'a, T>`.

```rust
// All control-flow result nodes are variants of the render3 template-AST enum `t::Node<'a>`.

pub struct IfBlock<'a> {
    pub branches: Vec<'a, IfBlockBranch<'a>>,
    pub source_span: SourceSpan,
    pub start_source_span: SourceSpan,
    pub end_source_span: Option<SourceSpan>,
    pub name_span: SourceSpan,
}
pub struct IfBlockBranch<'a> {
    pub expression: Option<Expr<'a>>,        // None => @else
    pub children: Vec<'a, Node<'a>>,
    pub expression_alias: Option<Variable>,  // the `as` alias
    pub i18n: Option<I18nMeta<'a>>,
    pub source_span: SourceSpan,
    pub start_source_span: SourceSpan,
    pub end_source_span: Option<SourceSpan>,
    pub name_span: SourceSpan,
}

pub struct ForLoopBlock<'a> {
    pub item: Variable,
    pub expression: AstWithSource<'a>,
    pub track_by: Option<AstWithSource<'a>>,
    pub track_keyword_span: Option<SourceSpan>,
    pub context_variables: Vec<'a, Variable>,
    pub children: Vec<'a, Node<'a>>,
    pub empty: Option<ForLoopBlockEmpty<'a>>,
    pub main_block_span: SourceSpan,
    pub i18n: Option<I18nMeta<'a>>,
    /* + sourceSpan/start/end/name spans */
}
pub struct ForLoopBlockEmpty<'a> { pub children: Vec<'a, Node<'a>>, /* + spans, i18n */ }

pub struct SwitchBlock<'a> {
    pub expression: Expr<'a>,
    pub groups: Vec<'a, SwitchBlockCaseGroup<'a>>,
    pub unknown_blocks: Vec<'a, UnknownBlock>,
    pub exhaustive_check: Option<SwitchExhaustiveCheck<'a>>,
    /* + spans */
}
pub struct SwitchBlockCase<'a>      { pub expression: Option<Expr<'a>> /* None => default */, /* spans */ }
pub struct SwitchBlockCaseGroup<'a> { pub cases: Vec<'a, SwitchBlockCase<'a>>, pub children: Vec<'a, Node<'a>>, /* spans, i18n */ }
pub struct SwitchExhaustiveCheck<'a>{ pub expression: Option<Expr<'a>>, /* spans */ }

pub struct UnknownBlock { pub name: String /* or &'a str */, pub source_span: SourceSpan, pub name_span: SourceSpan }

pub struct Variable {
    pub name: String,
    pub value: String,                 // '$implicit' for the loop item; alias name otherwise
    pub source_span: SourceSpan,
    pub key_span: SourceSpan,
    pub value_span: Option<SourceSpan>,
}
```

Return type `{node, errors}` maps to `(Option<T>, Vec<ParseError>)`. Prefer returning `errors` as an out-param `&mut Vec<ParseError>` (mirrors the TS `errors` accumulator passed into helpers) plus `Option<T>` return; this matches the existing code flow more closely than a tuple.

Internal `parseForLoopParameters` result object →
```rust
struct ForLoopParams<'a> {
    item_name: Variable,
    track_by: Option<TrackBy<'a>>,   // { expression: AstWithSource<'a>, keyword_span: SourceSpan }
    expression: AstWithSource<'a>,
    context: Vec<'a, Variable>,
}
```

`PipeVisitor` → a small struct implementing the expression `RecursiveAstVisitor` trait, overriding `visit_pipe` to set `has_pipe = true`.

---

## 4. Algorithm walkthroughs

### 4.1 `createIfBlock(ast, connectedBlocks, visitor, bindingParser)`
1. `errors = validateIfConnectedBlocks(connectedBlocks)` — checks: only one `@else`; `@else` must be last; `@else` cannot have parameters; any block that is neither `else` nor matches `ELSE_IF_PATTERN` is "Unrecognized conditional block @{name}".
2. Parse the **main** block params with `parseConditionalBlockParameters(ast, …)`. If non-null, push an `IfBlockBranch(expression, children=visitAll(children), expressionAlias, …spans, i18n)`.
3. For each connected block:
   - if name matches `ELSE_IF_PATTERN` → parse params, push a branch (with expression + optional alias).
   - else if name === `'else'` → push a branch with `expression=null, expressionAlias=null`.
4. Compute the outer span: `startSourceSpan` = first branch's start (else `ast.startSourceSpan`); `endSourceSpan` = last branch's end (else `ast.endSourceSpan`). `wholeSourceSpan` runs from that start to the **last branch's sourceSpan.end** (if any branch exists).
5. Return `new t.IfBlock(branches, wholeSourceSpan, ast.startSourceSpan, ifBlockEndSourceSpan, ast.nameSpan)`.

`parseConditionalBlockParameters`:
- If `parameters.length === 0` → push "Conditional block does not have an expression", return null.
- `expression = parseBlockParameterToBinding(parameters[0], …)`.
- For each remaining param: match `CONDITIONAL_ALIAS_PATTERN` (`/^(as\s+)(.*)/`).
  - no match → "Unrecognized conditional parameter".
  - alias only allowed on `if`/`else if` blocks (else error).
  - only one `as` allowed (else error).
  - alias name `aliasMatch[2].trim()` must satisfy `IDENTIFIER_PATTERN`, else "must be a valid JavaScript identifier". Variable span computed by offsetting past `aliasMatch[1]` length.
- Returns `{expression, expressionAlias}`.

### 4.2 `createForLoop(ast, connectedBlocks, visitor, bindingParser)`
1. `params = parseForLoopParameters(ast, errors, bindingParser)`.
2. Walk `connectedBlocks`: a block named `'empty'` produces a single `ForLoopBlockEmpty` (errors if duplicate, or if it has parameters). Any other connected name → "Unrecognized @for loop block".
3. If `params !== null`:
   - `endSpan = empty?.endSourceSpan ?? ast.endSourceSpan`; outer `sourceSpan` = `[ast.sourceSpan.start, endSpan.end]` (includes the `@empty`).
   - track handling: if `params.trackBy === null` → push "@for loop must have a \"track\" expression" and leave track null. Else extract `trackExpression`/`trackKeywordSpan` and run `validateTrackByExpression` (pipes forbidden).
   - Build `t.ForLoopBlock(item, expression, trackExpr, trackKeywordSpan, context, children, empty, sourceSpan, ast.sourceSpan /* = mainBlockSpan */, ast.startSourceSpan, endSpan, ast.nameSpan, ast.i18n)`.

`parseForLoopParameters` (the trickiest function):
- 0 params → "@for loop does not have an expression", null.
- First param = expression param; rest = secondary params.
- `stripOptionalParentheses(expressionParam)` then match `FOR_LOOP_EXPRESSION_PATTERN` (`<identifier> of <expression>`). On no match or empty RHS → error + null.
- Capture `itemName` and `rawExpression`. If `itemName` is one of the reserved `$index/...` names → error (but continues).
- `variableName = expressionParam.expression.split(' ')[0]` (only the declared item name, NOT the ` of x` part). Build `item = Variable(itemName, '$implicit', variableSpan, variableSpan)`.
- Seed `context` with all 6 reserved variables, each with an **empty span at the end of `block.startSourceSpan`** (ambient, not user-written).
- `expression = parseBlockParameterToBinding(expressionParam, …, rawExpression)`.
- For each secondary param:
  - match `FOR_LOOP_LET_PATTERN` → `parseLetParameter(...)` (parses comma-separated `name = $reserved` aliases, validating each).
  - else match `FOR_LOOP_TRACK_PATTERN` → set `trackBy` (error on duplicate; error if the parsed expression is an `EmptyExpr`; compute `keywordSpan`).
  - else → "Unrecognized @for loop parameter".

`parseLetParameter`: split on `,`; each part split on `=` into `name = variableName`. Errors for: malformed (`<name> = <var>` shape), unknown reserved variable, alias named same as loop item, duplicate alias name. Otherwise compute key/value spans (using `CHARACTERS_IN_SURROUNDING_WHITESPACE_PATTERN` to skip surrounding whitespace) and push a `Variable(name, variableName, sourceSpan, keySpan, valueSpan)`. `startSpan` advances by `part.length + 1` per iteration (the +1 skips the comma).

### 4.3 `createSwitchBlock(ast, visitor, bindingParser)`
1. `errors = validateSwitchBlock(ast)`: exactly one parameter (else early return with that single error). Iterates children skipping comments and whitespace-only text; non-`case`/`default`/`default never` blocks → error; duplicate `@default` → error; `@default` with params → error; `@case` must have exactly one param.
2. `primaryExpression` = parse `parameters[0]` if present, else parse the empty string binding.
3. Walk `ast.children` that are `html.Block`:
   - Skip blocks that are not valid cases (collect into `unknownBlocks`): condition is `(name !== 'case' || params.length === 0) && name !== 'default' && name !== 'default never'`.
   - If an `exhaustiveCheck` was already set → error "@default block with \"never\" parameter must be the last case".
   - `'case'` → `expression = parse(parameters[0])`.
   - `'default never'` → optional expression; body must be empty (else error); cannot follow an empty-bodied `@case` (else error); sets `exhaustiveCheck` and `continue`s.
   - Build `SwitchBlockCase(expression, …)` and push to `collectedCases`.
   - **Fall-through grouping**: a case with empty body (`children.length===0` AND collapsed `endSourceSpan`, i.e. `start.offset === end.offset`) does NOT close a group — record `firstCaseStart` and `continue`. When a case with a body is reached, the accumulated `collectedCases` become one `SwitchBlockCaseGroup`, with the source/start spans merged from `firstCaseStart` to the body case. Reset `collectedCases`.
4. Return `new t.SwitchBlock(primaryExpression, groups, unknownBlocks, exhaustiveCheck, …spans)`.

### 4.4 `parseBlockParameterToBinding(ast, bindingParser, part?)`
- If `part` given: `start = max(0, ast.expression.lastIndexOf(part)); end = start + part.length`. Else span the whole expression.
- Calls `bindingParser.parseBinding(ast.expression.slice(start, end), /*isHostBinding*/ false, ast.sourceSpan, ast.sourceSpan.start.offset + start)`.

### 4.5 `stripOptionalParentheses(param, errors)`
Scans from the left counting leading `(` (skipping whitespace) and from the right matching `)`. Returns the inner slice if balanced, the original if none, or `null` (+ "Unclosed parentheses in expression") if unbalanced.

---

## 5. Dependencies on other compiler modules

| Import | Used for |
|---|---|
| `../expression_parser/ast` → `AST`, `ASTWithSource`, `EmptyExpr`, `RecursiveAstVisitor` | expression AST types; `EmptyExpr` to detect empty track; `RecursiveAstVisitor` base for `PipeVisitor` |
| `../ml_parser/ast` (`html`) → `Block`, `BlockParameter`, `Visitor`, `visitAll`, `Comment`, `Text` | input HTML AST + recursive child traversal |
| `../parse_util` → `ParseError`, `ParseSourceSpan` | error reporting + span arithmetic (`moveBy`, `start`, `end`, `offset`) |
| `../template_parser/binding_parser` → `BindingParser` | `parseBinding(value, isHost, sourceSpan, absoluteOffset): ASTWithSource` |
| `./r3_ast` (`t`) | all output node classes + `Variable` |

Consumer: `render3/r3_template_transform.ts` (`visitBlock`, `findConnectedBlocks`).

**Port ordering implication:** depends on ported `ParseSourceSpan`/`ParseError`, the html ML-parser AST, the expression-parser AST + `BindingParser`, and the render3 `t.*` AST node definitions. All four must precede this module.

---

## 6. ɵɵ instructions / output emitted

**None.** This is a desugaring/AST-building module. It emits `t.*` template-AST nodes and `ParseError`s only. The `ɵɵconditional` / `ɵɵrepeater` / `ɵɵrepeaterCreate` / switch-equivalent instructions are produced later, in the `template_pipeline` ingest/emit stages — out of scope here.

---

## 7. Edge cases, gotchas & version sensitivity

- **`@switch` cases are children, not siblings** — `createSwitchBlock` takes no `connectedBlocks`. `@if`/`@for` branches are siblings collected by the caller.
- **Fall-through cases** (`@case (a) {}` `@case (b) { body }`) are merged into one `SwitchBlockCaseGroup`; the body-less case detection relies on a *collapsed* `endSourceSpan` (`start.offset === end.offset`), not just `children.length === 0`. Port the span-offset check exactly.
- **`@default never`** — note the block "name" literally contains a space (`'default never'`). This is the exhaustiveness-check feature (relatively new / version-sensitive). It must be last, must have no body, and cannot follow an empty-bodied `@case`.
- **Reserved `@for` context variables** are always seeded (all 6: `$index, $first, $last, $even, $odd, $count`) with empty ambient spans at `block.startSourceSpan.end`. User `let` aliases (`let e = $even`) are pushed alongside.
- **`item` value is the literal string `'$implicit'`**, not the iterable.
- **`variableName = expressionParam.expression.split(' ')[0]`** — relies on the loop var being the first whitespace-delimited token; this is *separate* from the regex `itemName` capture and is used only to compute the variable's span. Preserve both.
- **`track` must exist**; missing track and `EmptyExpr` track both error. **Pipes are forbidden in track** (`PipeVisitor`).
- **`parseBlockParameterToBinding` uses `lastIndexOf(part)`** to find the sub-expression offset (comment in source explains the regex `d`-flag alternative was avoided). When porting, replicate `lastIndexOf` semantics, not a naive `find`.
- **Span arithmetic** (`moveBy`, `.start`, `.end`, `.offset`) is pervasive and easy to get off-by-one. The `let` parameter span logic (`parseLetParameter`) is the most intricate: it advances `startSpan` by `part.length + 1` per comma-separated part and computes key/value spans by skipping leading whitespace.
- **Error accumulation is non-fatal**: most validation errors are pushed and parsing continues (e.g. reserved item name still builds a node). Don't short-circuit unless the TS code returns `null`.
- **Version churn risk**: `@default never` exhaustive-check, the exact reserved-variable set, and the regex patterns are the most likely to change between Angular minor versions. Centralize the regexes/constants so they're easy to bump. `isConnectedIfLoopBlock` is misnamed in source ("IfLoop") — keep behavior, rename freely in Rust.
- **`html.visitAll(visitor, children, children)`** passes `children` as both nodes and context — the visitor is the `r3_template_transform` instance recursing into block bodies. The Rust port must thread the same visitor/context.

---

## 8. Port plan (Rust / OXC)

**Reuse from OXC:** very little directly. OXC's `oxc_ast` does not model Angular template AST; you build the render3 `t.*` node enum yourself in the AST-port module. Use `oxc_allocator::{Allocator, Box, Vec, String}` for arena allocation so spans/exprs stay zero-copy. Use plain `&str` slicing for the regex-style parsing.

**Regex:** port the seven patterns. They are simple enough that for hot paths (`FOR_LOOP_EXPRESSION_PATTERN`, `ELSE_IF_PATTERN`) you could hand-roll matchers, but using the `regex` crate keeps fidelity and avoids subtle bugs (esp. `[\S\s]`, `[^\S\r\n]`, the `/i` flag on `IDENTIFIER_PATTERN`, and capture-group indexing). Recommendation: use `regex` crate with `once_cell`/`std::sync::LazyLock` for the compiled statics.

**Suggested structure:**
- `control_flow.rs` exposing `create_if_block`, `create_for_loop`, `create_switch_block`, `is_connected_for_loop_block`, `is_connected_if_loop_block`.
- Private fns mirror the TS helpers 1:1. Keep `errors: &mut Vec<ParseError>` out-params.
- `PipeVisitor` implements the expression `RecursiveAstVisitor` trait.

**Implementation ordering (within this module):**
1. Constants + `is_connected_*` predicates (trivial, unit-testable immediately).
2. `parse_block_parameter_to_binding` + `strip_optional_parentheses` (pure string/span ops; need `BindingParser` port for the former).
3. `create_switch_block` + `validate_switch_block` (self-contained; good first full feature — no sibling collection).
4. `create_if_block` + `parse_conditional_block_parameters` + `validate_if_connected_blocks`.
5. `create_for_loop` + `parse_for_loop_parameters` + `parse_let_parameter` + `validate_track_by_expression` (most span-heavy; do last).

**Estimated complexity: MEDIUM.** No instruction emission and no codegen, but heavy on (a) precise span arithmetic, (b) regex capture semantics, (c) the switch fall-through grouping and `default never` rules. The logic is self-contained and pure (transform in, AST+errors out), which makes it very test-friendly via golden-file comparison against the TS output.

**Ordering vs other modules:** Implement *after* the foundational ports — `ParseSourceSpan`/`ParseError`, `ml_parser` html AST, `expression_parser` AST + `BindingParser`, and the render3 `t.*` AST — but *before* `r3_template_transform` (its caller) can be completed, since `r3_template_transform.visitBlock` dispatches into these three `create*` functions.
