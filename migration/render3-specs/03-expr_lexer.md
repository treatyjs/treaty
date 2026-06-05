# Port Spec 03 — Expression Parser Lexer (`expression_parser/lexer.ts`)

Angular version: **22.1.0-next.0**
Source: `packages/compiler/src/expression_parser/lexer.ts`
Target: Rust + OXC (`oxc_ast` AstBuilder / `oxc_codegen`). This module is *pre-AST* — it produces a flat token stream consumed by the expression parser, so it has **no direct OXC AST dependency**. It is a self-contained scanner that we re-implement in plain Rust.

---

## 1. Purpose & role in the compilation pipeline

The lexer tokenizes the **micro-language used inside Angular template bindings**: interpolation `{{ expr }}`, property bindings `[x]="expr"`, event bindings `(y)="stmt"`, two-way `[(z)]`, structural directive microsyntax (`*ngFor`), and `@if`/`@for` block expressions.

It is the first stage of `expression_parser`:

```
template binding text
   -> Lexer.tokenize(text)        // THIS MODULE: string -> Token[]
   -> Parser (parser.ts)          // Token[] -> AST (ast.ts: AST nodes)
   -> binding/template AST consumed by template compiler -> ɵɵ instructions
```

The lexer is **not** the HTML lexer (`packages/compiler/src/ml_parser/lexer.ts`) — that one tokenizes markup. This lexer only handles the JS-like expression sub-grammar (a restricted ECMAScript: no statements beyond chained pipes/assignments, but including template literals, regex literals, optional chaining, nullish coalescing, private identifiers, spread).

It emits **no diagnostics directly** — lexical errors become `TokenType.Error` tokens carried inline in the stream; the parser decides how to surface them.

---

## 2. Public API (exact TypeScript signatures)

Exported symbols:

```ts
export enum TokenType {
  Character, Identifier, PrivateIdentifier, Keyword,
  String, Operator, Number, RegExpBody, RegExpFlags, Error,
}

export enum StringTokenKind {
  Plain, TemplateLiteralPart, TemplateLiteralEnd,
}

export class Lexer {
  tokenize(text: string): Token[];
}

export class Token {
  constructor(
    public index: number,
    public end: number,
    public type: TokenType,
    public numValue: number,
    public strValue: string,
  );

  isCharacter(code: number): boolean;
  isNumber(): boolean;
  isString(): this is StringToken;
  isOperator(operator: string): boolean;
  isIdentifier(): boolean;
  isPrivateIdentifier(): boolean;
  isKeyword(): boolean;
  isKeywordLet(): boolean;
  isKeywordAs(): boolean;
  isKeywordNull(): boolean;
  isKeywordUndefined(): boolean;
  isKeywordTrue(): boolean;
  isKeywordFalse(): boolean;
  isKeywordThis(): boolean;
  isKeywordTypeof(): boolean;
  isKeywordVoid(): boolean;
  isKeywordIn(): boolean;
  isKeywordInstanceOf(): boolean;
  isError(): boolean;
  isRegExpBody(): boolean;
  isRegExpFlags(): boolean;
  toNumber(): number;                                  // numValue or -1
  isTemplateLiteralPart(): this is StringToken;
  isTemplateLiteralEnd(): this is StringToken;
  isTemplateLiteralInterpolationStart(): boolean;      // isOperator('${')
  toString(): string | null;
}

export class StringToken extends Token {
  constructor(index: number, end: number, strValue: string, readonly kind: StringTokenKind);
}

export const EOF: Token = new Token(-1, -1, TokenType.Character, 0, '');
```

`KEYWORDS` (module-private const, drives `Keyword` vs `Identifier` classification):
```
'var', 'let', 'as', 'null', 'undefined', 'true', 'false',
'if', 'else', 'this', 'typeof', 'void', 'in', 'instanceof'
```

Module-private token factory functions (not exported, but define the construction contract):
`newCharacterToken`, `newIdentifierToken`, `newPrivateIdentifierToken`, `newKeywordToken`, `newOperatorToken`, `newNumberToken`, `newErrorToken`, `newRegExpBodyToken`, `newRegExpFlagsToken`.

The actual scanning lives in module-private `class _Scanner` (not exported).

---

## 3. Key data structures + proposed Rust mapping

### 3.1 `TokenType`
A `Character` token carries the char code in `numValue`. Note `EOF` is itself a `Character` token (code 0). `Error` carries the human-readable message in `strValue`.

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TokenType {
    Character,
    Identifier,
    PrivateIdentifier,
    Keyword,
    String,
    Operator,
    Number,
    RegExpBody,
    RegExpFlags,
    Error,
}
```

### 3.2 `StringTokenKind`
```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StringTokenKind {
    Plain,
    TemplateLiteralPart, // text segment ending just before `${`
    TemplateLiteralEnd,  // final text segment ending at the closing backtick
}
```

### 3.3 `Token` / `StringToken`
The TS design is a single class with a `numValue` (used by `Number`/`Character`) and a `strValue` (used by everything else), plus a subclass `StringToken` adding `kind`. For Rust the cleaner mapping is a **tagged enum** that fuses both types; we keep `index`/`end` (UTF-16 code-unit offsets into the source string — see gotchas) on every variant.

Proposed Rust (no lifetime needed if we own `String`; use `&'a str` only if we want to borrow the source arena):

```rust
#[derive(Clone, Debug)]
pub struct Token {
    pub index: u32,         // start offset (UTF-16 code units, see §7)
    pub end: u32,           // end offset
    pub kind: TokenValue,
}

#[derive(Clone, Debug)]
pub enum TokenValue {
    Character(u32),                              // numValue = char code
    Identifier(CompactStr),                      // strValue
    PrivateIdentifier(CompactStr),               // includes leading '#'
    Keyword(Keyword),                            // interned keyword
    Str { value: CompactStr, kind: StringTokenKind },
    Operator(CompactStr),                        // could intern to enum, see below
    Number(f64),                                 // numValue (int or float)
    RegExpBody(CompactStr),                      // text without slashes
    RegExpFlags(CompactStr),
    Error(String),                               // full "Lexer Error: ..." message
}
```

Notes on the mapping:
- `numValue: number` is `f64` in JS. For `Number` tokens it can be an integer or float; for `Character` tokens it is a char code. Splitting into `Character(u32)` and `Number(f64)` removes the overloaded field.
- `strValue` is empty for `Number` tokens in TS (`newNumberToken` passes `''`). The Rust enum carries the value in-variant so this footgun disappears.
- `Operator` strings come from a **small fixed set** (`+ - / % ^ * ? ?? ?. ??= . ... ( ) etc` and the multi-char ones, plus the template-literal `${`). Worth interning into an `enum Operator` for fast `isOperator` comparisons in the parser, but a `CompactStr`/`&'a str` is a faithful 1:1 port. Keep the string form initially to reduce risk; optimize later.
- `Keyword` enum is recommended (the `isKeywordX()` helpers become matches). Variants: `Var, Let, As, Null, Undefined, True, False, If, Else, This, Typeof, Void, In, Instanceof`.
- The boolean predicate methods (`isCharacter`, `isOperator`, `isKeywordLet`, `isTemplateLiteralPart`, etc.) map to inherent `impl Token` methods over the enum. `isTemplateLiteralInterpolationStart` is just `matches!(self.kind, TokenValue::Operator(s) if s == "${")`.
- `EOF` constant maps to a `const fn eof() -> Token` or a `Token { index: u32::MAX (or -1 sentinel), end: ..., kind: Character(0) }`. Since `index/end` are `-1` in TS, use `i32` or an `Option`/sentinel; simplest is to make `index/end` `i32` to match TS exactly. Recommendation: keep `i32` to preserve the `-1` sentinel semantics the parser relies on.

`AstBuilder`/arena `'a` is **not** needed here — tokens are not OXC AST nodes. If we want zero-copy slices into the input we can parametrize `Token<'a>` with `&'a str` for identifier/operator/string values, but string-escape handling (see §4) builds *new* strings, so several variants must own data. A `CompactStr` (or `Cow<'a, str>`) is the pragmatic choice.

### 3.4 `_Scanner` (scanner state)
```ts
class _Scanner {
  private readonly tokens: Token[] = [];
  private readonly length: number;
  private peek = 0;          // current char code
  private index = -1;        // index of `peek` in input
  private braceStack: ('interpolation' | 'expression')[] = [];
  constructor(private readonly input: string) { length = input.length; advance(); }
}
```

```rust
struct Scanner<'src> {
    input: &'src [u16],         // see §7: TS indexes UTF-16 code units
    length: usize,
    peek: u32,                  // current code unit, 0 ($EOF) past end
    index: i32,                 // signed: starts at -1, matches TS
    tokens: Vec<Token>,
    brace_stack: Vec<BraceKind>,
}

#[derive(Clone, Copy, PartialEq)]
enum BraceKind { Interpolation, Expression }
```

`braceStack` disambiguates `}` that closes an interpolation hole inside a template literal (`` `a${x}b` ``) from `}` that closes an object literal. This is the one piece of real state coupling lexing decisions across tokens — preserve it exactly.

---

## 4. Algorithm walkthrough

### Entry: `Lexer.tokenize(text)` → `new _Scanner(text).scan()`

`scan()` loops calling `scanToken()` and pushing non-null results until `scanToken()` returns `null` (end of input). Returns the `Token[]`. Note some scan methods push *extra* tokens onto `this.tokens` directly (regex body before flags; template-literal parts before `${`; the `}` char before resuming a template literal) and then return the "final" token — `scan()` pushes the returned one on top.

### `advance()`
`peek = ++index >= length ? $EOF(0) : input.charCodeAt(index)`. Pre-increment then load. Past the end, `peek` becomes 0.

### `scanToken()` → `Token | null`
1. **Skip whitespace**: while `peek <= $SPACE (32)` advance a local index/peek (handles space, tab, LF, CR, VT, FF, and any control char ≤32). Writes back to `this.peek`/`this.index`.
2. If `index >= length` return `null` (terminates scan).
3. If `isIdentifierStart(peek)` → `scanIdentifier()`.
4. If `isDigit(peek)` → `scanNumber(index)`.
5. Otherwise a big `switch (peek)`:
   - `$PERIOD '.'`: advance; if next is digit → `scanNumber(start)` (e.g. `.5`); else if not another `.` → `Character('.')`; else advance, if third is `.` → `Operator('...')` (spread); otherwise error `Unexpected character [.]` (a `..`).
   - `( ) [ ] , : ;` → `scanCharacter` (single `Character` token).
   - `{` → `scanOpenBrace`: push `'expression'` on brace stack, advance, `Character('{')`.
   - `}` → `scanCloseBrace`: advance, **pop** brace stack; if popped value is `'interpolation'`, push a `Character('}')` token and **resume template literal** via `scanTemplateLiteralPart(index)`; else return `Character('}')`.
   - `'` or `"` → `scanString`.
   - `` ` `` ($BT) → advance, `scanTemplateLiteralPart(start)`.
   - `#` → `scanPrivateIdentifier`.
   - `+`, `-`, `%` → `scanComplexOperator(start, sym, $EQ, '=')` → `+`/`+=` etc.
   - `/` → if `isStartOfRegex()` → `scanRegex`, else `scanComplexOperator(start,'/',$EQ,'=')` (`/` or `/=`).
   - `^` → `scanOperator(start,'^')`.
   - `*` → `scanStar` → `*`, `**`, `*=`, `**=`.
   - `?` → `scanQuestion` → `?`, `??`, `??=`, `?.`.
   - `<`, `>` → `scanComplexOperator(start, sym, $EQ, '=')` → `<`/`<=`, `>`/`>=` (note: **no** `<<`, `>>`, `>>>` shifts; not supported).
   - `!` → `scanComplexOperator(start,'!',$EQ,'=',$EQ,'=')` → `!`, `!=`, `!==`.
   - `=` → `scanEquals` → `=`, `==`, `===`, `=>`.
   - `&` → `scanComplexOperator(start,'&',$AMPERSAND,'&',$EQ,'=')` → `&`, `&&`, `&=`. (Note the third-char branch only fires *after* the second matched — yields `&`, `&&`, `&=` but **not** `&&=`. Same shape for `|`.)
   - `|` → `scanComplexOperator(start,'|',$BAR,'|',$EQ,'=')` → `|` (pipe operator!), `||`, `|=`.
   - `$NBSP (160)`: consume all whitespace (`isWhitespace`) then recurse `scanToken()`.
6. Default: advance, return `error('Unexpected character [X]', 0)`.

### `scanIdentifier()`
Record `start = index`; advance; while `isIdentifierPart(peek)` advance. Slice `input[start..index]`. If in `KEYWORDS` → `Keyword`, else `Identifier`.

### `scanPrivateIdentifier()`
`start = index` (the `#`); advance. If next is **not** `isIdentifierStart` → `error('Invalid character [#]', -1)`. Else consume `isIdentifierPart`. Token text **includes** the leading `#` (slice is from `start`).

### `scanNumber(start)`
- `simple = (index === start)` — false when entered via leading `.` (so `.5` is non-simple/float).
- `hasSeparators = false`. Advance past the first digit, then loop:
  - digit → continue.
  - `_` separator → valid only if surrounded by digits (`isDigit(input[index-1]) && isDigit(input[index+1])`), else `error('Invalid numeric separator', 0)`. Sets `hasSeparators`.
  - `.` → `simple = false`.
  - exponent start (`e`/`E`) → advance; optional sign (`+`/`-`) advance; require a digit next else `error('Invalid exponent', -1)`; `simple = false`.
  - else break.
- Slice text; if separators, strip `_` (regex `/_/g`). Value = `simple ? parseIntAutoRadix(str) : parseFloat(str)`.
- `parseIntAutoRadix` uses JS `parseInt(text)` (radix auto: `0x` hex, `0o`/`0b` are **not** auto-detected by bare `parseInt` — leading `0x` → hex, anything else → base 10; `parseInt('0b1')` = 0). Throws on `NaN`.

### `scanString()`
Quote = `peek` (`'` or `"`). Advance. Accumulate `buffer`; `marker` tracks start of current literal run. Loop while `peek != quote`:
- `\` → `scanStringBackslash(buffer, marker)`; if it returns an error Token, return it; else update buffer + reset marker.
- `$EOF` → `error('Unterminated quote', 0)`.
- else advance.
On close: append `input[marker..index]`, advance past quote, return `StringToken(start, index, buffer+last, Plain)`.

### `scanTemplateLiteralPart(start)`
Loop while `peek != $BT` (backtick):
- `\` → backslash handling (as in string).
- `$` ($$) → record `dollar = index`, advance; if next is `{` → push `'interpolation'` on brace stack; push a `StringToken(start, dollar, buffer + input[marker..dollar], TemplateLiteralPart)`; advance; return `Operator('${')` (text sliced as `input[dollar..index]`). If `$` not followed by `{`, fall through (the `$` becomes literal text — note no explicit advance in the else-less branch beyond the one already done, the loop continues scanning from after `$`).
- `$EOF` → `error('Unterminated template literal', 0)`.
- else advance.
On close backtick: append `input[marker..index]`, advance, return `StringToken(start, index, buffer+last, TemplateLiteralEnd)`.

### `scanStringBackslash(buffer, marker)` → `string | ErrorToken`
Append `input[marker..index]` to buffer. Advance past `\`. If next is `u`: read 4 hex chars `input[index+1..index+5]`; validate `/^[0-9a-f]+$/i` (note: matches **any length** of hex in that 4-char window — but it always reads exactly 4 and advances 5 total). Parse hex → code. Else `unescapedCode = unescape(peek)` (n→LF, f→FF, r→CR, t→TAB, v→VTAB, default→identity) and advance once. Append `String.fromCharCode(unescapedCode)`. Returns updated buffer.

### `scanEquals(start)`
`=`; if `==` then maybe `===`; **but** `=>` is special: after first `=`, if next is `>` → return `=>` immediately (arrow). So: `=`, `==`, `===`, `=>`.

### `scanStar` / `scanQuestion`
`scanStar`: `*`, `**`, `**=`, `*=`. `scanQuestion`: `?`, `??`, `??=`, `?.`.

### Regex: `isStartOfRegex()` + `scanRegex(start)`
`isStartOfRegex()` decides whether `/` begins a regex literal or is division. Rules (using previously emitted tokens):
- No prior tokens → regex.
- Prev is `!` operator → look back one more: regex only if it's a **negation** (before-prev is null, or not an Identifier and not `)` and not `]`); i.e. `!/re/` is regex but `x!/2` (non-null assertion then divide) is division.
- Otherwise regex iff prev token is an `Operator`, or a `Character` `(`, `[`, `,`, or `:`.

`scanRegex`: advance past `/`; `textStart = index`. Loop with `inEscape`/`inCharacterClass` flags: `$EOF` → `error('Unterminated regular expression', 0)`; `\` toggles escape; `[`/`]` track char class; `/` outside char class → break. Body value = `input[textStart..index]` (slashes excluded from value but included in span). Advance past closing `/`. Build `RegExpBody` token; then `scanRegexFlags`. If flags present, **push the body token** and return flags token; else return body token.

`scanRegexFlags(start)`: if `peek` not ascii letter → `null`; else consume ascii letters → `RegExpFlags` token.

### `error(message, offset)`
`position = index + offset`. Returns `Error` token with `index=position`, `end=index`, `strValue = "Lexer Error: {message} at column {position} in expression [{input}]"`.

### Char classification helpers (module-private)
```ts
isIdentifierStart(c): a-z | A-Z | '_' | '$'
isIdentifierPart(c):  isAsciiLetter(c) | isDigit(c) | '_' | '$'
isExponentStart(c):   c == 'e' | 'E'
isExponentSign(c):    c == '-' | '+'
unescape(c):          n->LF, f->FF, r->CR, t->TAB, v->VTAB, else c
parseIntAutoRadix(s): parseInt(s); throw if NaN
```
From `../chars`: `isWhitespace`, `isDigit`, `isAsciiLetter`, plus the many `$*` code-point constants (see chars.ts; e.g. `$EOF=0`, `$SPACE=32`, `$NBSP=160`, `$BT=96`).

---

## 5. Dependencies on other compiler modules

- **`../chars` (`packages/compiler/src/chars.ts`)** — the only import. Provides code-point constants (`$EOF`, `$SPACE`, `$LPAREN`, `$BT`, `$NBSP`, etc.) and predicates `isWhitespace`, `isDigit`, `isAsciiLetter`. In Rust: port `chars.ts` as a `chars` module of `const`s and small `fn`s (it is tiny — see source). Several other compiler modules also depend on `chars`, so port it once as a shared crate-internal module.

**Downstream consumers** (not dependencies, but define the contract this lexer must satisfy): `expression_parser/parser.ts` (the recursive-descent parser) and `expression_parser/ast.ts` (AST node defs). The parser reads `Token.index`/`end` for span info, calls the `isKeywordX`/`isOperator`/`isCharacter` predicates heavily, and relies on the `${` operator + `TemplateLiteralPart`/`End` string-token sequence to assemble template-literal ASTs.

No dependency on OXC, no dependency on the HTML/ML parser, no dependency on output AST or instruction emission.

---

## 6. ɵɵ instructions / output emitted

**None.** This is a lexer; it emits a `Token[]` only. It produces no `ɵɵ` runtime instructions and no OXC AST. Instruction emission happens far downstream in the template compiler. (Enumerated: none.)

---

## 7. Edge cases, gotchas, version-sensitive notes

1. **UTF-16 indexing.** TS uses `input.charCodeAt(index)` and `input.length` — these are **UTF-16 code units**, and all `index`/`end` offsets are UTF-16 offsets. A naive Rust port over `&str`/byte offsets or `char` (Unicode scalar) offsets will produce **different span numbers** that the downstream parser and source-map machinery rely on. **Recommendation:** operate over a `&[u16]` (collect `input.encode_utf16()`), keep `index`/`end` as UTF-16 offsets to match Angular exactly. This matters for any astronaut/emoji/non-BMP chars inside string or template literals.

2. **`peek <= $SPACE` whitespace skip** treats *all* control chars (codes 1–31) as whitespace, plus `$NBSP (160)` is special-cased separately (the `<= 32` check misses 160). Reproduce both paths: the main loop AND the `$NBSP` case that consumes `isWhitespace` runs then recurses.

3. **Overloaded `numValue`.** It is the char code for `Character` and the numeric value for `Number`; `strValue` is `''` for numbers. The Rust tagged-enum mapping eliminates this, but be careful translating any code that reads `token.numValue` generically.

4. **`EOF` constant has `index = end = -1`.** Keep a signed offset type or sentinel. The parser compares against `EOF` and reads its `-1` index.

5. **Tokens pushed mid-scan.** `scanCloseBrace` (pushes `}` then returns next template part), `scanTemplateLiteralPart` (pushes the part token then returns `${`), and `scanRegex` (pushes body then returns flags) **mutate `this.tokens` directly** in addition to their return value. A Rust port must give `Scanner::scan_*` methods access to `&mut self.tokens`, not be pure functions.

6. **`braceStack` correctness is essential.** `{` always pushes `'expression'`; `${` pushes `'interpolation'`. `}` pops and branches. Mismatched/empty stack: `pop()` on empty returns `undefined` in JS (treated as not-interpolation → plain `}`). Rust `Vec::pop()` returns `Option`; treat `None` like the non-interpolation branch (return `Character('}')`).

7. **Operators NOT supported** (so don't add them): bit-shift `<< >> >>>`, `&&=`, `||=` (only `&=`/`|=` via the complex-operator shape), unary `~`, comma operator semantics (comma is a `Character`, not operator). `**` and `**=` ARE supported. `=>` arrow IS supported. `${` is emitted as an `Operator` (only inside template literals).

8. **Pipe `|` is the Angular pipe operator**, lexed as the `|` operator token — the parser, not the lexer, gives it pipe semantics. `||` and `|=` also exist. Keep all three.

9. **`@ts-expect-error` comments** in `scanQuestion`, `scanTemplateLiteralPart`, `scanStar` are TS narrowing artifacts (after `advance()` TS thinks `this.peek` can't equal the just-checked value). They have **no runtime effect** — ignore when porting.

10. **Number parsing semantics differ from Rust.** `parseIntAutoRadix` = JS `parseInt` (stops at first non-numeric char, hex via `0x`, throws on `NaN` here). `parseFloat` (JS) parses leading float, ignores trailing garbage, and supports exponent. Rust `str::parse::<f64>()` is stricter and won't accept e.g. `0x` or trailing junk. Since the lexer already validated the numeric shape character-by-character, the slice should be clean; but **`0x` hex and integer-vs-float** need care: implement `parse_int_auto_radix` to mirror JS (`0x`/`0X` → radix 16 on the remainder; else base 10), returning `f64`. Numeric separators `_` are stripped before parsing.

11. **Unicode escape window `input.substring(index+1, index+5)`** reads exactly 4 chars and validates them with `/^[0-9a-f]+$/i`. If fewer than 4 remain, the substring is short and the regex still passes for the available hex digits (a subtle leniency). Then it unconditionally `advance()`es 5 times even past EOF (advance just yields `$EOF`/0). Reproduce: read up to 4, validate, advance 5.

12. **Regex detection is heuristic** and stateful (looks back 1–2 tokens). The `!` special-case (negation vs non-null-assertion) is the trickiest branch — port `isStartOfRegex` verbatim. Getting it wrong silently turns `a / b` into a regex or vice-versa.

13. **Error tokens are inline, not thrown** — except `parseIntAutoRadix` which `throw`s on `NaN` (should be unreachable given prior validation, but mirror it as a panic/`Result`).

14. **Keyword set is fixed at 14 entries** including TS-flavored `var`/`let`/`as`/`typeof`/`void`/`in`/`instanceof`. `KEYWORDS.indexOf` is a linear scan; in Rust use a `match` or a perfect-hash/`phf`.

15. **Version sensitivity (Angular-internal churn).** This file changes across minors: template-literal lexing (`StringTokenKind`, `${`, `braceStack`) is relatively recent; private-identifier (`#x`), regex literals (`RegExpBody`/`RegExpFlags`), and the `??=`/`**=`/`...` operators were added incrementally. Pin behavior to **22.1.0-next.0**. Treat `TokenType`/`StringTokenKind` ordinal values as **not** ABI-stable across versions — don't serialize the numeric enum discriminants.

---

## 8. Port plan (Rust / OXC)

**Reuse from OXC: essentially nothing.** OXC has its own JS lexer, but Angular's expression grammar is a different (smaller + Angular-specific) language — pipes, `${`-as-operator, regex heuristic, microsyntax. We must hand-port this module; do **not** try to bolt on `oxc_parser`/`oxc_lexer`. OXC enters the picture only much later, when we emit the generated factory/instruction code via `AstBuilder`/`oxc_codegen` — this lexer feeds Angular's own AST, not OXC's.

**Concrete steps:**
1. Port `chars.ts` → `chars.rs` (consts + `is_whitespace`/`is_digit`/`is_ascii_letter`). Trivial. Shared by other modules.
2. Define `TokenType`, `StringTokenKind`, `Keyword`, `Token { index: i32, end: i32, kind: TokenValue }`, and `TokenValue` tagged enum (§3.3). Implement the predicate methods (`is_operator`, `is_character`, `is_keyword_let`, `is_template_literal_part`, etc.) and an `EOF` const.
3. Build `Scanner<'src>` over `&[u16]` (UTF-16). Port `advance`, `scan`, `scan_token`, and all `scan_*` helpers 1:1, preserving the `tokens.push` side effects (methods take `&mut self`).
4. Implement `parse_int_auto_radix` + float parsing to match JS semantics (hex via `0x`, separator stripping, `f64` result).
5. Public entry `Lexer::tokenize(&str) -> Vec<Token>` that encodes to UTF-16, runs the scanner, returns owned tokens.

**Testing:** snapshot/golden tests against Angular's own `lexer_spec.ts` cases (port the spec); especially exercise template literals with nested `${...}`, numeric separators, regex-vs-division boundary cases, the `!`/regex disambiguation, NBSP, and unicode escapes.

**Estimated complexity: MEDIUM.** ~800 lines, self-contained, no AST/arena, no async, deterministic. The hard parts are (a) faithful UTF-16 offset semantics, (b) JS number-parsing parity, and (c) the stateful regex heuristic + `braceStack` template-literal interplay. No external dependency beyond `chars`.

**Ordering vs other modules:** This is a **foundational, early** module. Port order: `chars` → **this lexer** → `expression_parser/ast` (node defs) → `expression_parser/parser`. Everything in the expression pipeline depends on it; it depends on nothing but `chars`, so it can be ported and fully unit-tested in isolation immediately.
