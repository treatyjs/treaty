# Port Spec 05 — `expression_parser/parser.ts`

Angular **22.1.0-next.0**, render3 compiler. Target: Rust + OXC (`oxc_ast` `AstBuilder`, `oxc_codegen`).

Source: `packages/compiler/src/expression_parser/parser.ts` (1951 lines).

---

## 1. Purpose & Role in the Pipeline

This module is the **recursive-descent parser for Angular binding expressions**. It takes the *raw text* of a binding (the contents of `[prop]="..."`, `(event)="..."`, `{{ ... }}`, `*ngFor="..."`, host bindings, ICU switch expressions, etc.) plus a lexer-produced token stream, and produces an **Angular expression AST** (`AST` subclasses defined in `./ast.ts`), wrapped in an `ASTWithSource`.

It does **not** emit any `ɵɵ` runtime instructions itself (see §6). It sits *upstream* of template binding/IR generation: the `@angular/compiler` template parser calls into `Parser` to turn binding text into expression ASTs, which the template IR / `template_parser` / `r3_*` emitters later lower into instruction calls.

Position in flow:

```
HTML/template text
  └─> ml_parser (HTML lexer/parser) ─> attributes, interpolations, ICUs
        └─> expression_parser/lexer.ts  (tokenizer)   <-- sibling, spec elsewhere
              └─> expression_parser/parser.ts  (THIS)  ─> AST (./ast.ts)
                    └─> template binding parser / render3 IR ─> ɵɵ instructions
```

Key responsibilities:
- Split interpolation strings `{{ a }}{{ b }}` into raw-string segments and expression sub-strings.
- Strip `//` line comments (quote-aware) before lexing.
- Reject stray interpolation syntax where a plain expression is expected.
- Implement the full JS-subset expression grammar with correct operator precedence.
- Parse Angular pipes (`exp | name : arg : arg`).
- Parse microsyntax template bindings (`*ngFor="let item of items; index as i"`).
- Track source spans precisely (both relative `ParseSpan` and `AbsoluteSourceSpan`) for diagnostics/language-service.
- Error recovery: emit `ParseError`s and skip to recovery points instead of throwing.

---

## 2. Public API (exported surface)

### Exported interfaces / classes

```ts
export interface InterpolationPiece {
  text: string;
  start: number;
  end: number;
}

export class SplitInterpolation {
  constructor(
    public strings: InterpolationPiece[],
    public expressions: InterpolationPiece[],
    public offsets: number[],
  ) {}
}

export class TemplateBindingParseResult {
  constructor(
    public templateBindings: TemplateBinding[],
    public warnings: string[],
    public errors: ParseError[],
  ) {}
}

export const enum ParseFlags {
  None = 0,
  Action = 1 << 0,   // an output/event binding (assignments & chains allowed, pipes forbidden)
}
```

### `export class Parser` — public methods

```ts
constructor(
  private readonly _lexer: Lexer,
  private readonly _supportsDirectPipeReferences = false,
)

parseAction(input: string, parseSourceSpan: ParseSourceSpan, absoluteOffset: number): ASTWithSource

parseBinding(input: string, parseSourceSpan: ParseSourceSpan, absoluteOffset: number): ASTWithSource

parseSimpleBinding(input: string, parseSourceSpan: ParseSourceSpan, absoluteOffset: number): ASTWithSource
  // host bindings; runs SimpleExpressionChecker (no pipes allowed)

parseTemplateBindings(
  templateKey: string,
  templateValue: string,
  parseSourceSpan: ParseSourceSpan,
  absoluteKeyOffset: number,
  absoluteValueOffset: number,
): TemplateBindingParseResult

parseInterpolation(
  input: string,
  parseSourceSpan: ParseSourceSpan,
  absoluteOffset: number,
  interpolatedTokens: InterpolatedAttributeToken[] | InterpolatedTextToken[] | null,
): ASTWithSource | null   // null if there are no interpolations

parseInterpolationExpression(
  expression: string,
  parseSourceSpan: ParseSourceSpan,
  absoluteOffset: number,
): ASTWithSource   // for ICU switch expressions: treated as one expr, empty pre/suffix strings

splitInterpolation(
  input: string,
  parseSourceSpan: ParseSourceSpan,
  errors: ParseError[],
  interpolatedTokens: InterpolatedAttributeToken[] | InterpolatedTextToken[] | null,
): SplitInterpolation

wrapLiteralPrimitive(
  input: string | null,
  sourceSpanOrLocation: ParseSourceSpan | string,
  absoluteOffset: number,
): ASTWithSource
```

Private helpers on `Parser`: `checkSimpleExpression`, `_parseBindingAst`, `createInterpolationAst`, `_stripComments`, `_commentStart`, `_checkNoInterpolation`, `_getInterpolationEndIndex`, `_forEachUnquotedChar` (a generator).

### Internal (non-exported) helpers
- `class _ParseAST` — the actual recursive-descent state machine (the bulk of the file).
- `enum ParseContextFlags { None = 0, Writable = 1 }`
- `const SUPPORTED_REGEX_FLAGS = new Set(['d','g','i','m','s','u','v','y'])`
- `class SimpleExpressionChecker extends RecursiveAstVisitor` — rejects pipes.
- `function getParseError(message, input, locationText, parseSourceSpan): ParseError`
- `function getLocation(span): string`
- `function getIndexMapForOriginalTemplate(interpolatedTokens): Map<number, number>` — maps decoded-input indices to original-template indices, accounting for `ENCODED_ENTITY` tokens.

---

## 3. Key Data Structures + Proposed Rust Mapping

### 3.1 `Parser` (public façade)

Stateless apart from injected `_lexer` and `_supportsDirectPipeReferences`. Each parse call builds a fresh `_ParseAST`.

```rust
pub struct Parser<'a> {
    lexer: &'a Lexer,
    supports_direct_pipe_references: bool,
}
```

In OXC the produced AST nodes are *Angular* AST nodes, not `oxc_ast` nodes (Angular's expression AST differs from JS AST — e.g. `Unary(+)` is desugared to `Binary('-', x, 0)`). So we allocate Angular AST into a bump arena:

```rust
pub struct Parser<'a> {
    lexer: &'a Lexer,
    supports_direct_pipe_references: bool,
}
// arena: &'arena bumpalo::Bump (or oxc_allocator::Allocator) threaded into parse methods.
```

### 3.2 `_ParseAST` (the workhorse)

Fields:
```ts
private rparensExpected = 0;
private rbracketsExpected = 0;
private rbracesExpected = 0;
private context = ParseContextFlags.None;
private sourceSpanCache = new Map<string, AbsoluteSourceSpan>();
private index = 0;
// ctor params: input, parseSourceSpan, absoluteOffset, tokens, parseFlags, errors, offset, supportsDirectPipeReferences
```

```rust
struct ParseAst<'a, 'arena> {
    input: &'a str,
    parse_source_span: ParseSourceSpan,
    absolute_offset: i32,
    tokens: &'a [Token<'a>],
    parse_flags: ParseFlags,        // bitflags
    errors: &'a mut Vec<ParseError>,
    offset: i32,                    // relative offset of this fragment within `input`
    supports_direct_pipe_references: bool,

    rparens_expected: u32,
    rbrackets_expected: u32,
    rbraces_expected: u32,
    context: ParseContextFlags,     // bitflags
    source_span_cache: FxHashMap<(i32, i32, i32), AbsoluteSourceSpan>,
    index: usize,
    arena: &'arena Allocator,
}

bitflags! { struct ParseFlags: u8 { const NONE = 0; const ACTION = 1; } }
bitflags! { struct ParseContextFlags: u8 { const NONE = 0; const WRITABLE = 1; } }
```

Note on `sourceSpanCache`: the TS key is a string serial `` `${start}@${this.inputIndex}:${artificialEndIndex}` ``. In Rust use a tuple key `(start, input_index, artificial_end)` where `artificial_end` is encoded as `-1` for the `undefined` case.

### 3.3 Span types (from `./ast.ts`)

```ts
class ParseSpan { start: number; end: number; toAbsolute(off): AbsoluteSourceSpan }
class AbsoluteSourceSpan { start: number; end: number }   // (defined in ast.ts)
```
```rust
#[derive(Clone, Copy)] pub struct ParseSpan { pub start: i32, pub end: i32 }
#[derive(Clone, Copy)] pub struct AbsoluteSourceSpan { pub start: i32, pub end: i32 }
impl ParseSpan { pub fn to_absolute(self, off: i32) -> AbsoluteSourceSpan { AbsoluteSourceSpan { start: off+self.start, end: off+self.end } } }
```

### 3.4 Angular expression AST (`./ast.ts`, consumed here)

The parser constructs these node types. All share `(span: ParseSpan, sourceSpan: AbsoluteSourceSpan)`; `ASTWithName` adds `nameSpan`. Map to a single arena-allocated enum:

```rust
pub enum Ast<'a> {
    EmptyExpr(ParseSpan, AbsoluteSourceSpan),
    ImplicitReceiver(ParseSpan, AbsoluteSourceSpan),
    ThisReceiver(ParseSpan, AbsoluteSourceSpan),
    Chain { span: ParseSpan, source_span: AbsoluteSourceSpan, expressions: Vec<'a, &'a Ast<'a>> },
    Conditional { span, source_span, condition: &'a Ast<'a>, true_exp: &'a Ast<'a>, false_exp: &'a Ast<'a> },
    PropertyRead { span, source_span, name_span: AbsoluteSourceSpan, receiver: &'a Ast<'a>, name: &'a str },
    SafePropertyRead { /* same as PropertyRead */ },
    KeyedRead { span, source_span, receiver: &'a Ast<'a>, key: &'a Ast<'a> },
    SafeKeyedRead { /* same */ },
    BindingPipe { span, source_span, name_span, exp: &'a Ast<'a>, name: &'a str, args: Vec<'a, &'a Ast<'a>>, ty: BindingPipeType },
    LiteralPrimitive { span, source_span, value: LiteralValue<'a> },
    LiteralArray { span, source_span, expressions: Vec<'a, &'a Ast<'a>> },
    LiteralMap { span, source_span, keys: Vec<'a, LiteralMapKey>, values: Vec<'a, &'a Ast<'a>> },
    SpreadElement { span, source_span, expression: &'a Ast<'a> },
    Interpolation { span, source_span, strings: Vec<'a, &'a str>, expressions: Vec<'a, &'a Ast<'a>> },
    Binary { span, source_span, operation: BinaryOp, left: &'a Ast<'a>, right: &'a Ast<'a> },
    Unary { span, source_span, operator: UnaryOp, expr: &'a Ast<'a>, /* desugared binary fields */ },
    PrefixNot { span, source_span, expression: &'a Ast<'a> },
    TypeofExpression { span, source_span, expression: &'a Ast<'a> },
    VoidExpression { span, source_span, expression: &'a Ast<'a> },
    NonNullAssert { span, source_span, expression: &'a Ast<'a> },
    Call { span, source_span, receiver: &'a Ast<'a>, args: Vec<'a, &'a Ast<'a>>, argument_span: AbsoluteSourceSpan },
    SafeCall { /* same */ },
    ParenthesizedExpression { span, source_span, expression: &'a Ast<'a> },
    ArrowFunction { span, source_span, params: Vec<'a, ArrowFunctionParameter<'a>>, body: &'a Ast<'a> },
    TemplateLiteral { span, source_span, elements: Vec<'a, TemplateLiteralElement<'a>>, expressions: Vec<'a, &'a Ast<'a>> },
    TemplateLiteralElement { span, source_span, text: &'a str },
    TaggedTemplateLiteral { span, source_span, tag: &'a Ast<'a>, template: &'a Ast<'a> },
    RegularExpressionLiteral { span, source_span, pattern: &'a str, flags: Option<&'a str> },
}

pub enum LiteralValue<'a> { Null, Undefined, Bool(bool), Number(f64), String(&'a str) }
```

**`BindingPipeType`** (`./ast.ts:156`):
```rust
pub enum BindingPipeType { ReferencedByName, ReferencedDirectly }
```

**`Binary` operation** is a string-union `BinaryOperation` (`./ast.ts:282`). The parser-relevant subset (assignments only created in `Action` flag or arrow-function bodies):
```rust
pub enum BinaryOp {
    // assignment (Binary.isAssignmentOperation)
    Assign, AddAssign, SubAssign, MulAssign, DivAssign, ModAssign, ExpAssign, AndAssign, OrAssign, NullishAssign,
    // logical / nullish
    And, Or, Nullish,
    // equality
    Eq, Neq, StrictEq, StrictNeq,
    // relational
    Lt, Gt, Le, Ge, In, InstanceOf,
    // additive / multiplicative / exponentiation
    Add, Sub, Mul, Mod, Div, Exp,
}
```
`AssignmentOperation` (`./ast.ts:271`) is the first 10 above; mirror `Binary.isAssignmentOperation` as `BinaryOp::is_assignment`.

**`Unary`** (`./ast.ts:345`): inherits from `Binary` for back-compat. `createMinus` => desugars to `Binary('-', 0, expr)`; `createPlus` => `Binary('-', expr, 0)`. Store the real `operator: '+'|'-'` and `expr`, plus the desugared binary fields for consumers that treat it as `Binary`. In Rust represent as a dedicated `Unary` variant carrying both views, or compute the binary view lazily.

**`LiteralMapKey`** (`./ast.ts:226`):
```rust
pub enum LiteralMapKey {
    Property { key: String, quoted: bool, span: ParseSpan, source_span: AbsoluteSourceSpan, is_shorthand_initialized: bool },
    Spread   { span: ParseSpan, source_span: AbsoluteSourceSpan },
}
```

**`ASTWithSource`** (wrapper returned to callers):
```rust
pub struct AstWithSource<'a> {
    pub ast: &'a Ast<'a>,
    pub source: Option<&'a str>,
    pub location: String,
    pub absolute_offset: i32,
    pub errors: Vec<ParseError>,
}
```

**Template-binding types** (`./ast.ts`): `TemplateBinding = ExpressionBinding | VariableBinding`; `TemplateBindingIdentifier { source: string; span: AbsoluteSourceSpan }`.
```rust
pub struct TemplateBindingIdentifier { pub source: String, pub span: AbsoluteSourceSpan }
pub enum TemplateBinding<'a> {
    Expression { source_span: AbsoluteSourceSpan, key: TemplateBindingIdentifier, value: Option<AstWithSource<'a>> },
    Variable   { source_span: AbsoluteSourceSpan, key: TemplateBindingIdentifier, value: Option<TemplateBindingIdentifier> },
}
```

### 3.5 Token API used (from `./lexer.ts`)

`_ParseAST` only touches tokens through these predicates / accessors:
`type`, `index`, `end`, `strValue`, `numValue`, `kind`; `isCharacter(code)`, `isNumber()`, `isString()`, `isOperator(op)`, `isIdentifier()`, `isPrivateIdentifier()`, `isKeyword()`, `isKeywordLet/As/Null/Undefined/True/False/This/Typeof/Void/In/InstanceOf()`, `isRegExpBody()`, `isRegExpFlags()`, `isTemplateLiteralPart/End/InterpolationStart()`, `isError()`, `toNumber()`, `toString()`. Plus the `EOF` sentinel token (`Token(-1,-1,Character,0,'')`). `TokenType` and `StringTokenKind` enums.

---

## 4. Algorithm Walkthrough

### 4.1 Entry points → `_ParseAST.parseChain()`

`parseAction` / `parseBinding` / `parseSimpleBinding` all funnel through `_parseBindingAst`:
1. `_checkNoInterpolation(errors, input, span)` — scans unquoted chars; if a `{{` ... `}}` pair is found, push *"Got interpolation where expression was expected"*.
2. `_stripComments(input)` — find first unquoted `//` (via `_commentStart`) and truncate.
3. `_lexer.tokenize(strippedSource)` → `Token[]`.
4. `new _ParseAST(...).parseChain()`.
5. Wrap in `ASTWithSource(ast, input, location, absoluteOffset, errors)`.

`parseAction` passes `ParseFlags.Action`; the binding variants pass `None`. `parseSimpleBinding` additionally runs `SimpleExpressionChecker` (errors on any pipe).

### 4.2 `parseChain(): AST`
Loop while tokens remain:
- `expr = parsePipe()`, push to `exprs`.
- If next is `;`: only allowed under `ParseFlags.Action` (else error *"Binding expression cannot contain chained expression"*); consume all consecutive `;`.
- Else if tokens remain: error *"Unexpected token '...'"*. Guard against infinite loop — if `error()`'s `skip()` didn't advance past `errorIndex`, `break`.

Result: 0 exprs → `EmptyExpr` spanning whole input; 1 expr → that expr; ≥2 → `Chain`.

### 4.3 Precedence ladder (descending binding strength)
Each level calls the next-tighter level for operands. Entry is `parsePipe` (loosest):

| Method | Operators / construct | Node |
|---|---|---|
| `parsePipe` | `\|` pipes (right side: name + `:`-separated args) | `BindingPipe` |
| `parseExpression` | (delegates) | — |
| `parseConditional` | `? : ` ternary | `Conditional` |
| `parseLogicalOr` | `\|\|` | `Binary` |
| `parseLogicalAnd` | `&&` | `Binary` |
| `parseNullishCoalescing` | `??` | `Binary` |
| `parseEquality` | `== != === !==` | `Binary` |
| `parseRelational` | `< > <= >= in instanceof` | `Binary` |
| `parseAdditive` | `+ -` | `Binary` |
| `parseMultiplicative` | `* % /` | `Binary` |
| `parseExponentiation` | `**` (right-assoc; errors if base is a unary/not/typeof/void) | `Binary` |
| `parsePrefix` | prefix `+ - !`, `typeof`, `void` | `Unary` / `PrefixNot` / `TypeofExpression` / `VoidExpression` |
| `parseCallChain` | `. ?. [] () ! template-literal-tag` postfix | member/keyed/call/non-null/tagged |
| `parsePrimary` | literals, identifiers, `(...)`, `[...]`, `{...}`, `this`, arrow funcs, regex, template literals | leaf nodes |

All binary levels use a `while` loop accumulating left-associatively, except `**` which recurses for right-associativity.

`parsePipe` detail: after `parseExpression`, if `|` consumed → error under `Action`. For each pipe segment: read identifier/keyword name (empty-name recovery with whitespace-extended span), then zero-or-more `: arg` (each `parseExpression`). `type` = `ReferencedDirectly` when `supportsDirectPipeReferences` **and** the name's first char is `_` or `A`–`Z`, else `ReferencedByName`.

### 4.4 `parseCallChain()` postfix loop
Infinite loop dispatching on next token:
- `.` → `parseAccessMember(result, start, isSafe=false)`
- `?.` → if followed by `(` → `parseCall(..., isSafe=true)`; if `[` → `parseKeyedReadOrWrite(..., isSafe=true)`; else `parseAccessMember(..., isSafe=true)`.
- `[` → `parseKeyedReadOrWrite(..., false)`
- `(` → `parseCall(..., false)`
- `!` → `NonNullAssert`
- template-literal-end token → `parseNoInterpolationTaggedTemplateLiteral`
- template-literal-part token → `parseTaggedTemplateLiteral`
- otherwise → return `result`.

### 4.5 `parsePrimary()`
Dispatch order (significant): arrow function (lookahead via `isArrowFunction`), `(` parenthesized (`ParenthesizedExpression`), `null`/`undefined`/`true`/`false` keywords (`LiteralPrimitive`), `this` (`ThisReceiver`), `[` array, `{` map, identifier (`PropertyRead` on `ImplicitReceiver`), number, template-literal end/part, plain string, private-identifier (error → `EmptyExpr`), regexp body, EOF (error), else error.

`parseAccessMember`: reads identifier under `Writable` context. If `isSafe` and an assignment op follows → error *"'?.' operator cannot be used in the assignment"*. If non-safe and assignment op follows: under `Action` produce `Binary(op, PropertyRead, parseConditional())`, else error *"Bindings cannot contain assignments"*. Otherwise `PropertyRead` / `SafePropertyRead`.

`parseKeyedReadOrWrite`: under `Writable`; `key = parsePipe()` (error if `EmptyExpr`); expect `]`; assignment handling parallels member access (`KeyedRead` rvalue / `Binary` for writes / `SafeKeyedRead`).

`parseCall` / `parseCallArguments`: `(` already consumed; args are comma-separated `parsePipe()` (or `parseSpreadElement` for `...`); expect `)`. Produces `Call` / `SafeCall` with a separate `argumentSpan`.

`parseLiteralArray` / `parseLiteralMap`: comma lists; map supports `...spread`, string/identifier/keyword keys, quoted-key requires `:`, and **shorthand** `{x}` → `PropertyRead(x)` with `isShorthandInitialized = true`.

`parseArrowFunction`: lookahead `isArrowFunction()` (scans `id =>` or `( id, id ) =>`). Params: single bare identifier or parenthesized list. Expect `=>`. Body: `{` is rejected (*"Multi-line arrow functions are not supported..."*); otherwise body parsed with `parseFlags` temporarily forced to `Action` (arrows may contain assignments even inside bindings).

`parseRegularExpressionLiteral`: body token + optional flags token; validates flags against `SUPPORTED_REGEX_FLAGS`, errors on unsupported/duplicate flags.

Template literals: `parseTemplateLiteral` walks part/end tokens building `TemplateLiteralElement`s and `${ ... }` interpolation expressions (each `parsePipe`, error if empty). `parseNoInterpolationTemplateLiteral` is the single-element fast path.

### 4.6 `splitInterpolation` / `parseInterpolation`
- `splitInterpolation`: scans `input` for `{{`/`}}` pairs (quote/comment-aware via `_getInterpolationEndIndex`), producing `strings[]`, `expressions[]`, `offsets[]`. Blank expressions → error. Unterminated `{{` → extend the last raw string. Offsets remapped through `getIndexMapForOriginalTemplate` to account for encoded HTML entities.
- `parseInterpolation`: split; if no expressions return `null`. For each expression: strip comments, tokenize, comment-only-empty → error, else `parseChain` with the per-expression `offset`. Then `createInterpolationAst(strings, exprNodes, ...)` → `Interpolation` wrapped in `ASTWithSource`.

### 4.7 `parseTemplateBindings` (microsyntax)
1. First binding from the directive key itself via `parseDirectiveKeywordBindings(templateKey)`.
2. Loop: try `parseLetBinding()` (`let x = y`); else read a key, try `parseAsBinding(key)` (`value as key`); else treat key as directive keyword (prefix-camelCase: `of` → `ngForOf`) and `parseDirectiveKeywordBindings`. Consume statement terminator (`;`/`,`).
3. `getDirectiveBoundTarget()` returns `null` at EOF/`as`/`let`, else `parsePipe()` wrapped in `ASTWithSource`.
Returns `TemplateBindingParseResult(bindings, [], errors)`.

### 4.8 Span tracking
- `inputIndex` = current token start + `offset` (or `currentEndIndex` at EOF).
- `currentEndIndex` = previous token end + offset.
- `span(start, artificialEndIndex?)` builds a `ParseSpan`, with a **bug workaround** swapping start/end if `start > endIndex` (commented TODO in source).
- `sourceSpan(...)` caches via the serial key and calls `span(...).toAbsolute(absoluteOffset)`.

### 4.9 Error recovery — `error()` + `skip()`
`error(msg, index?)` pushes a `ParseError` then `skip()`s. `skip()` advances until a recovery point: EOF, `;`, `|`, conditionally `)`/`}`/`]` (only when the matching `r*Expected` counter > 0), or an assignment operator while in `Writable` context. Error tokens encountered during skipping are also reported. The `r*Expected` counters are incremented/decremented around grouping productions exactly as in the source — **must be replicated faithfully** or recovery diverges.

---

## 5. Dependencies on Other Compiler Modules

- `../chars` — char-code constants (`$SLASH`, `$SEMICOLON`, `$LPAREN`, `$COLON`, `$_`, `$A`, `$Z`, …) and `isQuote`.
- `../ml_parser/tokens` — `InterpolatedAttributeToken`, `InterpolatedTextToken`, `TokenType as MlParserTokenType` (only `ENCODED_ENTITY` used).
- `../parse_util` — `ParseError`, `ParseSourceSpan` (and `ParseLocation` transitively).
- `./ast` — the entire Angular expression AST + `AbsoluteSourceSpan`, `ParseSpan`, `RecursiveAstVisitor`, `BindingPipeType`, `Binary.isAssignmentOperation`, `Unary.createPlus/createMinus`, template-binding node types.
- `./lexer` — `Lexer`, `Token`, `TokenType`, `StringTokenKind`, `EOF`.

Downstream consumers (not dependencies): template parser, render3 IR builder, type-check block generator, language service.

**Porting order:** `chars` → `parse_util` (spans/errors) → `ast` → `lexer` must all be ported *before* this module.

---

## 6. ɵɵ Instructions / Output Emitted

**None.** This module emits **no** `ɵɵ` runtime instructions and does no `oxc_codegen` output. Its output is purely the in-memory Angular expression `AST` (wrapped in `ASTWithSource`) plus a list of `ParseError`s. Instruction emission happens in later render3 stages that consume this AST.

---

## 7. Edge Cases, Gotchas, Version Sensitivity

- **Unary desugaring**: `+x`/`-x` become `Binary` (`x - 0` / `0 - x`) via `Unary` which *extends* `Binary` with `left/right/operation` redeclared `never`. Porting must preserve both the surface `operator`/`expr` and the desugared binary view, since downstream code may pattern-match either. Version-sensitive: the source comments this inheritance is slated for removal in a future major.
- **`**` precedence guard**: a unary/`!`/`typeof`/`void` operand directly left of `**` is an error (matches JS) — must replicate.
- **`supportsDirectPipeReferences`** (newer flag, default `false`): toggles `BindingPipeType.ReferencedDirectly` when the pipe name starts with `_` or uppercase. Version-sensitive: this is a relatively recent addition; older Angular always used `ReferencedByName`.
- **Empty-pipe-name span extension**: when `|` has no following identifier, the name span is zero-length placed at the end of trailing whitespace (`fullSpanEnd`). Subtle span math.
- **`span()` start/end swap workaround**: an acknowledged upstream parser bug. Replicate verbatim to match span output exactly.
- **`sourceSpanCache` serial key**: depends on stateful `inputIndex`; keying must use the *same* tuple semantics (start, current inputIndex, artificialEnd-or-sentinel) or cached spans diverge.
- **Comment stripping is quote-aware**: `//` inside a string literal is not a comment. `_commentStart` tracks one outer quote char.
- **Interpolation entity remap**: `getIndexMapForOriginalTemplate` only special-cases `ENCODED_ENTITY` MlParser tokens; offsets feed diagnostics in the *original* (encoded) template, not the decoded input.
- **Arrow body forces `Action` flag**: assignments allowed inside arrow bodies even within a binding; multi-line `{...}` bodies rejected.
- **Shorthand object property** `{x}` produces a `PropertyRead` on `ImplicitReceiver` with `isShorthandInitialized = true`.
- **Infinite-loop guards**: both `parseChain` (errorIndex check) and `skip()` (recovery counters) prevent non-progress; port the guards exactly.
- **`parseSimpleBinding` (host bindings)** forbids pipes via `SimpleExpressionChecker`.
- **`EmptyExpr` for ICU/keyed-empty/missing-token**: many recovery paths return `EmptyExpr`; downstream relies on these for graceful degradation.
- **Number value**: `LiteralPrimitive.value` is `number | string | boolean | null | undefined`; `undefined` is a *distinct* value from `null` (keyword `undefined`). Rust `LiteralValue` must keep them separate.
- **Regex flags set** is frozen at `d,g,i,m,s,u,v,y` — keep in sync with the source constant.

---

## 8. Port Plan (Rust / OXC)

### What to reuse from OXC
- **Allocator/arena**: use `oxc_allocator::Allocator` + `oxc_allocator::Vec`/`Box` for the Angular AST. Do **not** reuse `oxc_ast::ast` JS nodes — Angular's expression AST is a different shape (desugared unary, pipes, safe-navigation, template bindings, implicit receiver). Define a parallel `angular_ast` crate of arena types.
- **`oxc_codegen`**: not used here (no emission). Relevant only to downstream instruction emitters.
- Optionally `bitflags` for `ParseFlags` / `ParseContextFlags`; `rustc-hash` `FxHashMap` for the span cache.

### Recommended implementation steps
1. Port span/error primitives (`ParseSpan`, `AbsoluteSourceSpan`, `ParseError`, `ParseSourceSpan`) and the Angular `Ast` arena enum + `BindingPipeType`, `BinaryOp`, `LiteralMapKey`, template-binding types. (Spec 04/`ast` work.)
2. Port `Lexer`/`Token` (separate spec) — must land first; the parser is a thin consumer of token predicates.
3. Implement `ParseAst` state struct with `peek/next/advance/atEOF/inputIndex/currentEndIndex/span/sourceSpan/withContext/consumeOptional*/expect*`.
4. Implement the precedence ladder methods (`parsePipe` → `parsePrimary`) — mechanical 1:1 translation; mind right-assoc `**` and the unary guard.
5. Implement `parseCallChain`, member/keyed/call, literal array/map, arrow, template-literal, regex.
6. Implement error recovery (`error`/`skip` + recovery counters) — port last but test heavily; subtle.
7. Implement `Parser` façade methods: comment stripping, interpolation splitting (`_forEachUnquotedChar` becomes an iterator/closure), `parseInterpolation`, `parseTemplateBindings`, `wrapLiteralPrimitive`.
8. Port `getIndexMapForOriginalTemplate` + `SplitInterpolation`/`InterpolationPiece`.
9. Port `SimpleExpressionChecker` (needs the `RecursiveAstVisitor` trait/visit infra from the `ast` port).

### Rust-specific notes
- Generators: `_forEachUnquotedChar` → an explicit iterator struct or an inline loop with a callback. `consumeStatementTerminator` short-circuit `||` → sequential `if`.
- Closures borrowing `&mut self` (`withContext(cb)`): replace with a guard pattern (set flag, run inline block, reset) rather than passing a closure, to satisfy the borrow checker.
- `errors` is a shared `&mut Vec<ParseError>` threaded through; alternatively store on `ParseAst` and drain at the end.
- String values from tokens (`strValue`, identifiers) should be arena-interned `&'arena str` to avoid allocations.
- The span-swap workaround and exact counter bookkeeping are **observable behavior** — golden-test against the TS output.

### Estimated complexity & ordering
- **Complexity: HIGH.** ~1950 LOC, ~30 mutually-recursive methods, stateful span/recovery bookkeeping, many edge cases with diagnostic-exact output. The grammar itself is mechanical, but matching Angular's span and error-recovery semantics byte-for-byte is the hard part.
- **Ordering vs other modules**: depends on `chars`, `parse_util`, `ast`, and `lexer` being ported first. It is a *leaf* of the front-end parse stage and a *prerequisite* for the template parser and all render3 IR/instruction emitters. Port it immediately after the lexer and AST modules, before any template-IR work.
- Build a shared corpus of `(input, expected-AST, expected-errors, expected-spans)` golden cases (Angular's own `expression_parser_spec.ts` is the reference) and diff against the TS implementation.
