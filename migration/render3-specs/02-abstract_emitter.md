# Port Spec 02: `output/abstract_emitter.ts` + `output/abstract_js_emitter.ts`

Angular `22.1.0-next.0` — render3 compiler → Rust/OXC port.

Source files:
- `packages/compiler/src/output/abstract_emitter.ts`
- `packages/compiler/src/output/abstract_js_emitter.ts`

---

## 1. Purpose & role in the compilation pipeline

These two files implement the **string serialization layer** of Angular's output
intermediate representation (the `output_ast.ts` "`o.*`" node tree). After the
render3 template/host/declaration compilers build an abstract `o.Statement[]` /
`o.Expression` tree, an emitter visitor walks that tree and produces concrete
source text (TypeScript via the `typescript_emitter` subclass, or JavaScript via
`AbstractJsEmitterVisitor` subclasses such as the JIT `AbstractJsEmitterVisitor`
in `abstract_js_emitter`).

Concretely:

- `abstract_emitter.ts` provides:
  - `EmitterVisitorContext` — a mutable text accumulator that records lines,
    indentation, the emitted parts, and the `ParseSourceSpan` that produced each
    part. It is the bridge to source-map generation.
  - `AbstractEmitterVisitor` — an abstract visitor implementing
    `o.StatementVisitor`, `o.ExpressionVisitor`, and `o.TypeVisitor`. It contains
    the shared TS/JS serialization logic (operators, parenthesization,
    statements, literals, types). Two abstract hooks (`visitExternalExpr`,
    `visitWrappedNodeExpr`) are left to subclasses.
  - `escapeIdentifier()` — string-literal/identifier escaping helper.
- `abstract_js_emitter.ts` provides `AbstractJsEmitterVisitor`, a subclass that
  downlevels ES2015+ constructs (tagged templates, `$localize`, template
  literals, `var` instead of `const`/`let`) into ES5-compatible JavaScript and
  disables comments/types.

**In the Rust/OXC port this entire module is largely replaced.** Instead of
hand-rolling text accumulation + manual source spans, the port converts the
`output_ast` (`o.*`) tree into an **`oxc_ast` AST** (using
`oxc_ast::AstBuilder`) and emits with **`oxc_codegen::Codegen`**. The value of
this spec is to capture the *exact emission semantics* (operator spelling,
parenthesization rules, indentation, quoting, downlevelling) so the
`o.* → oxc_ast` lowering and codegen options reproduce byte-compatible (or
intentionally divergent) output.

---

## 2. Public API (exact signatures)

### `abstract_emitter.ts`

```ts
export class EmitterVisitorContext {
  static createRoot(): EmitterVisitorContext;
  constructor(private _indent: number);

  println(from?: {sourceSpan: ParseSourceSpan | null} | null, lastPart?: string): void;
  lineIsEmpty(): boolean;
  lineLength(): number;
  print(from: {sourceSpan: ParseSourceSpan | null} | null, part: string, newLine?: boolean): void;
  removeEmptyLastLine(): void;
  incIndent(): void;
  decIndent(): void;
  toSource(): string;
  toSourceMapGenerator(genFilePath: string, startsAtLine?: number): SourceMapGenerator;
  spanOf(line: number, column: number): ParseSourceSpan | null;
  // private get _currentLine(): EmittedLine;
  // private get sourceLines(): EmittedLine[];
}

export abstract class AbstractEmitterVisitor
  implements o.StatementVisitor, o.ExpressionVisitor, o.TypeVisitor
{
  constructor(
    protected readonly printComments: boolean,
    protected readonly printTypes: boolean,
  );

  // Abstract hooks left to subclasses:
  abstract visitExternalExpr(ast: o.ExternalExpr, ctx: EmitterVisitorContext): void;
  abstract visitWrappedNodeExpr(ast: o.WrappedNodeExpr<unknown>, ctx: EmitterVisitorContext): void;

  // Statement visitors:
  visitExpressionStmt(stmt: o.ExpressionStatement, ctx): void;
  visitReturnStmt(stmt: o.ReturnStatement, ctx): void;
  visitIfStmt(stmt: o.IfStmt, ctx): void;
  visitDeclareVarStmt(stmt: o.DeclareVarStmt, ctx): void;
  visitDeclareFunctionStmt(stmt: o.DeclareFunctionStmt, ctx): void;

  // Expression visitors:
  visitInvokeFunctionExpr(expr: o.InvokeFunctionExpr, ctx): void;
  visitTaggedTemplateLiteralExpr(expr: o.TaggedTemplateLiteralExpr, ctx): void;
  visitTemplateLiteralExpr(expr: o.TemplateLiteralExpr, ctx): void;
  visitTemplateLiteralElementExpr(expr: o.TemplateLiteralElementExpr, ctx): void;
  visitTypeofExpr(expr: o.TypeofExpr, ctx): void;
  visitVoidExpr(expr: o.VoidExpr, ctx): void;
  visitReadVarExpr(ast: o.ReadVarExpr, ctx): void;
  visitInstantiateExpr(ast: o.InstantiateExpr, ctx): void;
  visitLiteralExpr(ast: o.LiteralExpr, ctx): void;
  visitRegularExpressionLiteral(ast: o.RegularExpressionLiteralExpr, ctx): void;
  visitLocalizedString(ast: o.LocalizedString, ctx): void;
  visitConditionalExpr(ast: o.ConditionalExpr, ctx): void;
  visitDynamicImportExpr(ast: o.DynamicImportExpr, ctx): void;
  visitNotExpr(ast: o.NotExpr, ctx): void;
  visitFunctionExpr(ast: o.FunctionExpr, ctx): void;
  visitArrowFunctionExpr(ast: o.ArrowFunctionExpr, ctx): void;
  visitUnaryOperatorExpr(ast: o.UnaryOperatorExpr, ctx): void;
  visitBinaryOperatorExpr(ast: o.BinaryOperatorExpr, ctx): void;
  visitReadPropExpr(ast: o.ReadPropExpr, ctx): void;
  visitReadKeyExpr(ast: o.ReadKeyExpr, ctx): void;
  visitLiteralArrayExpr(ast: o.LiteralArrayExpr, ctx): void;
  visitLiteralMapExpr(ast: o.LiteralMapExpr, ctx): void;
  visitCommaExpr(ast: o.CommaExpr, ctx): void;
  visitParenthesizedExpr(ast: o.ParenthesizedExpr, ctx): void;
  visitSpreadElementExpr(ast: o.SpreadElementExpr, ctx): void;

  // Type visitors:
  visitBuiltinType(type: o.BuiltinType, ctx): void;
  visitExpressionType(type: o.ExpressionType, ctx): void;
  visitArrayType(type: o.ArrayType, ctx): void;
  visitMapType(type: o.MapType, ctx): void;
  visitTransplantedType(type: o.TransplantedType<unknown>, ctx): void; // throws

  // Helpers:
  visitAllExpressions(expressions: o.Expression[], ctx, separator: string): void;
  visitAllObjects<T>(handler: (t: T) => void, expressions: T[], ctx, separator: string): void;
  visitAllStatements(statements: o.Statement[], ctx): void;
  protected visitParams(params: o.FnParam[], ctx): void;
  protected shouldParenthesize(expression: o.Expression, containingExpression: o.Expression): boolean;
  protected printLeadingComments(node: o.Expression | o.Statement, ctx): void;

  // private lastIfCondition: o.Expression | null;
}

export function escapeIdentifier(input: string, alwaysQuote?: boolean): string | null;
```

### `abstract_js_emitter.ts`

```ts
export abstract class AbstractJsEmitterVisitor extends AbstractEmitterVisitor {
  constructor(); // calls super(false /* printComments */, false /* emitTypes */)

  override visitWrappedNodeExpr(ast: o.WrappedNodeExpr<any>, ctx): void;          // throws
  override visitDeclareVarStmt(stmt: o.DeclareVarStmt, ctx): void;                 // emits `var`
  override visitTaggedTemplateLiteralExpr(ast: o.TaggedTemplateLiteralExpr, ctx): void; // downlevel
  override visitTemplateLiteralExpr(expr: o.TemplateLiteralExpr, ctx): void;
  override visitTemplateLiteralElementExpr(expr: o.TemplateLiteralElementExpr, ctx): void;
  override visitLocalizedString(ast: o.LocalizedString, ctx): void;               // downlevel $localize
}
```

Note: `visitExternalExpr` remains abstract — `AbstractJsEmitterVisitor` does **not**
implement it. The concrete JIT emitter (`jit/output_emitter` family) provides it.

### Module-private (not exported, but observable in output)

- `EmittedLine` class (fields `indent`, `partsLength`, `parts`, `srcSpans`).
- Constants: `SINGLE_QUOTE_ESCAPE_STRING_RE = /'|\\|\n|\r/g`,
  `LEGAL_IDENTIFIER_RE = /^[$A-Z_][0-9A-Z_$]*$/i`, `INDENT_WITH = '  '` (two spaces).
- `BINARY_OPERATORS: Map<o.BinaryOperator, string>` (the operator-spelling table).
- `makeTemplateObjectPolyfill` string constant (in the JS emitter).

---

## 3. Key data structures + proposed Rust mapping

### 3.1 `EmittedLine` (private)

```ts
class EmittedLine {
  partsLength = 0;
  readonly parts: string[] = [];
  readonly srcSpans: (ParseSourceSpan | null)[] = [];
  constructor(public indent: number) {}
}
```

A single output line: indentation depth (in indent units, not chars), the
ordered text fragments (`parts`), and a parallel array of the source span that
produced each part (`srcSpans`). `partsLength` caches `sum(parts[i].length)`.

**Rust mapping** — only needed if the port keeps the manual text-accumulation
strategy (see Port Plan; the recommended path uses `oxc_codegen` instead). If
kept:

```rust
struct EmittedLine {
    indent: u32,
    parts_len: usize,            // cached char/byte length sum
    parts: Vec<String>,          // or Vec<&'a str> if arena-bound
    src_spans: Vec<Option<ParseSourceSpan>>, // parallel to parts
}
```

`ParseSourceSpan` is defined in `parse_util` (separate spec). Spans are pure data
(file ref + line/col), so they can be `Copy`/`Clone` value types or `Rc`-shared.

### 3.2 `EmitterVisitorContext` (exported)

Mutable accumulator. Fields: `_lines: EmittedLine[]`, `_indent: number`.
`_currentLine` = last element of `_lines`. `sourceLines` = `_lines` with a
trailing empty line dropped.

**Rust mapping** (if retained):

```rust
pub struct EmitterVisitorContext {
    lines: Vec<EmittedLine>,
    indent: u32,
}
impl EmitterVisitorContext {
    pub fn create_root() -> Self { Self { lines: vec![EmittedLine::new(0)], indent: 0 } }
    fn current_line(&mut self) -> &mut EmittedLine { self.lines.last_mut().unwrap() }
    pub fn print(&mut self, from: Option<&dyn HasSourceSpan>, part: &str, new_line: bool);
    pub fn println(&mut self, from: Option<&dyn HasSourceSpan>, last_part: &str);
    pub fn line_is_empty(&self) -> bool;
    pub fn line_length(&self) -> usize;
    pub fn remove_empty_last_line(&mut self);
    pub fn inc_indent(&mut self);
    pub fn dec_indent(&mut self);
    pub fn to_source(&self) -> String;
    pub fn to_source_map_generator(&self, gen_file_path: &str, starts_at_line: usize) -> SourceMapGenerator;
    pub fn span_of(&self, line: usize, column: usize) -> Option<ParseSourceSpan>;
}
```

The `from` parameter is structurally typed in TS as
`{sourceSpan: ParseSourceSpan | null} | null` — any `o.*` node satisfies it
because all expressions/statements carry a `sourceSpan` field. In Rust model this
as a small trait `HasSourceSpan { fn source_span(&self) -> Option<ParseSourceSpan>; }`
or pass `Option<ParseSourceSpan>` directly at call sites.

**Recommended (OXC) approach:** drop `EmitterVisitorContext` entirely. Source-span
mapping becomes `oxc_codegen`'s source-map output keyed off `Span` byte offsets
on the lowered `oxc_ast` nodes. The TS context only exists because Angular emits
text directly; OXC owns text + maps.

### 3.3 `BINARY_OPERATORS` table

Maps `o.BinaryOperator` enum → operator string. Full set (preserve exactly):

| `o.BinaryOperator` | string | | `o.BinaryOperator` | string |
|---|---|---|---|---|
| `And` | `&&` | | `Plus` | `+` |
| `Bigger` | `>` | | `In` | `in` |
| `BiggerEquals` | `>=` | | `InstanceOf` | `instanceof` |
| `BitwiseOr` | `\|` | | `AdditionAssignment` | `+=` |
| `BitwiseAnd` | `&` | | `SubtractionAssignment` | `-=` |
| `Divide` | `/` | | `MultiplicationAssignment` | `*=` |
| `Assign` | `=` | | `DivisionAssignment` | `/=` |
| `Equals` | `==` | | `RemainderAssignment` | `%=` |
| `Identical` | `===` | | `ExponentiationAssignment` | `**=` |
| `Lower` | `<` | | `AndAssignment` | `&&=` |
| `LowerEquals` | `<=` | | `OrAssignment` | `\|\|=` |
| `Minus` | `-` | | `NullishCoalesceAssignment` | `??=` |
| `Modulo` | `%` | | `NotEquals` | `!=` |
| `Exponentiation` | `**` | | `NotIdentical` | `!==` |
| `Multiply` | `*` | | `NullishCoalesce` | `??` |
| | | | `Or` | `\|\|` |

**Rust mapping:** since the port lowers to `oxc_ast`, map `o.BinaryOperator` →
`oxc_ast::ast::BinaryOperator` / `LogicalOperator` / `AssignmentOperator`
variants directly; oxc's codegen owns the spelling. Keep this table only as a
reference for the lowering match arms. Note Angular conflates true binary,
logical (`&&`/`||`/`??`), and assignment operators into one enum — OXC splits
them into `BinaryExpression` / `LogicalExpression` / `AssignmentExpression`, so
the lowering must dispatch to three different oxc node kinds.

### 3.4 Unary operators

`o.UnaryOperator.Plus` → `+`, `o.UnaryOperator.Minus` → `-`. Any other value
throws `Unknown operator`. Maps to `oxc_ast::ast::UnaryOperator::UnaryPlus` /
`UnaryNegation`.

### 3.5 `o.BuiltinTypeName` → TS type string (used only when `printTypes`)

`Bool→": boolean"`, `Dynamic→": any"`, `Int|Number→": number"`,
`String→": string"`, `None→": void"`, `Inferred→` (nothing), `Function→": Function"`,
default → `": any"`. Maps to `oxc_ast` `TSType*` nodes (only relevant for the TS
emitter; the JS emitter forces `printTypes=false`).

---

## 4. Algorithm walkthrough

### 4.1 `EmitterVisitorContext.print(from, part, newLine=false)`

1. If `part.length > 0`: push `part` onto `_currentLine.parts`, add to
   `partsLength`, push `from?.sourceSpan ?? null` to `srcSpans` (parallel).
2. If `newLine`: push a fresh `EmittedLine(_indent)` (note: empty `part` with
   `newLine=true` still creates a new line but records no part — that's how blank
   lines are produced).

`println(from, lastPart='')` = `print(from, lastPart, /*newLine*/ true)`.

### 4.2 Indentation

`incIndent`/`decIndent` mutate `_indent`. If the current line is *empty*, the
current line's `indent` is also retroactively updated so the indent change
applies to the line about to be filled. `INDENT_WITH` = two spaces; one indent
unit = 2 chars.

### 4.3 `toSource()`

For each `sourceLine` (trailing empty line dropped): if it has parts, emit
`"  ".repeat(indent) + parts.join('')`, else emit `''`. Join all with `\n`.

### 4.4 `toSourceMapGenerator(genFilePath, startsAtLine=0)`

Builds a `SourceMapGenerator` (`output/source_map.ts`):
1. For `startsAtLine` leading blank lines: `addLine()` + map first offset.
2. For each source line, `addLine()`, then walk `srcSpans`/`parts` in parallel,
   tracking `col0` (start at `indent*2`). Skip leading parts with no span
   (advancing `col0`). For each part **with** a span, call
   `map.addSource(source.url, source.content).addMapping(col0, source.url,
   span.start.line, span.start.col)`, then coalesce subsequent parts that share
   the same span (or have no span) into the same segment.
3. Special-case: if the first mapped span is at line 0 col 0, don't emit the
   synthetic first-offset mapping; otherwise emit a synthetic source `' '` mapped
   at offset 0 (so tools don't try to load the gen file from disk; uses
   `ng:///`-style virtual URLs).

### 4.5 `AbstractEmitterVisitor` dispatch

The visitor is invoked by the `o.*` nodes themselves: each statement's
`visitStatement(this, ctx)` and each expression's `visitExpression(this, ctx)`
double-dispatches into the corresponding `visit*` method here. So `visitAllStatements`
just iterates and calls `stmt.visitStatement(this, ctx)`.

Per-node serialization highlights (exact emission rules to replicate):

- **`visitExpressionStmt`**: leading comments, visit expr, `println(';')`.
- **`visitReturnStmt`**: `return `, visit value, `;`.
- **`visitIfStmt`**: `if (` + condition (sets `lastIfCondition` so the condition
  expression skips its own redundant parens — see gotcha §7) + `) {`. If
  `trueCase.length <= 1` and no else: single-line ` <stmt> ` with
  `removeEmptyLastLine()`. Else multi-line with `incIndent`/`decIndent`, and an
  optional `} else {` block. Closes `}`.
- **`visitDeclareVarStmt`**: `const` if `StmtModifier.Final` else `let`;
  `<kind> <name>`; optional type; optional ` = <value>`; `;`. (JS emitter
  overrides to always emit `var`.)
- **`visitInvokeFunctionExpr`**: optionally parenthesize the callee (per
  `shouldParenthesize`); then `(` or `?.(` if optional; comma-separated args; `)`.
- **`visitInstantiateExpr`**: `new ` + classExpr + `(args)`.
- **`visitLiteralExpr`**: strings → `escapeIdentifier(value)` (always quoted);
  everything else → template-stringified `${value}` (numbers/booleans/null).
- **`visitConditionalExpr`**: always wrapped `(cond ? t : f)`. `falseCase` is
  optional (`?.`).
- **`visitNotExpr`**: `!` + condition.
- **`visitFunctionExpr` / `visitDeclareFunctionStmt`**: `function[ name](params)`
  + optional type + ` {` newline, indented body, `}`.
- **`visitArrowFunctionExpr`**: `(params)` + optional type + ` => `; block body
  (`Array.isArray(body)`) → braces+indent; expression body → optionally
  parenthesized (e.g. literal-map body `() => ({...})`).
- **`visitUnaryOperatorExpr` / `visitBinaryOperatorExpr`**: wrap in parens unless
  this node IS the `lastIfCondition`. Binary: `lhs ` + op + ` rhs`.
- **`visitReadPropExpr`**: receiver + (`.` or `?.`) + name.
- **`visitReadKeyExpr`**: receiver + (`[` or `?.[`) + index + `]`.
- **`visitLiteralArrayExpr`**: `[` entries (`, `) `]`.
- **`visitLiteralMapExpr`**: `{` entries (`, `) `}`; each entry either
  `...spread` (`LiteralMapSpreadAssignment`) or
  `escapeIdentifier(key, quoted): value`.
- **`visitCommaExpr`**: `(parts joined by ', ')`.
- **`visitParenthesizedExpr`**: just visits inner expr (everything is already
  aggressively parenthesized — see TODO in source).
- **`visitSpreadElementExpr`**: `...expr`.
- **Template literals / tagged templates / `$localize`**: native backtick form in
  the base emitter (`` `...${expr}...` ``); downlevelled in the JS emitter (§4.6).
- **Types** (only when `printTypes`): builtin/expression/array/map type strings;
  `visitTransplantedType` always throws `'TransplantedType nodes are not supported'`.

### 4.6 `visitAllObjects<T>` — the 80-column line wrapping

Iterates items, inserting `separator` between them. Key behavior: when
`ctx.lineLength() > 80`, it inserts the separator **with** a newline
(`print(null, sep, true)`) and, on the first such wrap, double-`incIndent`
(continuation lines get double indentation). After the loop, restores indent with
two `decIndent` calls if it wrapped. `visitAllExpressions` and `visitParams` are
thin wrappers.

### 4.7 `AbstractJsEmitterVisitor` overrides (downlevelling)

- **`visitDeclareVarStmt`**: emit `var <name>[ = value];` (ignores `Final`/types).
- **`visitTaggedTemplateLiteralExpr`**: emit
  `tag(POLYFILL([cooked...], [raw...]), expr1, expr2, ...)` where `POLYFILL` is
  the `makeTemplateObjectPolyfill` string; cooked/raw arrays use
  `escapeIdentifier(part.text)` / `escapeIdentifier(part.rawText)`.
- **`visitLocalizedString`**: same downlevel pattern but `$localize(POLYFILL(...))`
  with cooked/raw from `serializeI18nHead()` + `serializeI18nTemplatePart(i)`.
- **`visitTemplateLiteralExpr` / `visitTemplateLiteralElementExpr`**: re-declared
  identically to the base (no real change; present for override clarity).
- **`visitWrappedNodeExpr`**: throws `'Cannot emit a WrappedNodeExpr in Javascript.'`.

### 4.8 `escapeIdentifier(input, alwaysQuote=true)`

Returns `null` if input is `null`. Replaces `'`, `\`, `\n`, `\r` with escaped
forms (`\n`→`\\n`, `\r`→`\\r`, else `\\<char>`). If `alwaysQuote` or the body
fails `LEGAL_IDENTIFIER_RE`, wrap in single quotes; otherwise return bare.

---

## 5. Dependencies on other compiler modules

- `./output_ast` (`o.*`) — the entire node hierarchy + visitor interfaces
  (`StatementVisitor`, `ExpressionVisitor`, `TypeVisitor`) and the enums
  (`BinaryOperator`, `UnaryOperator`, `BuiltinTypeName`, `StmtModifier`),
  comment classes (`LeadingComment`, `JSDocComment`), and node classes
  (`LiteralMapSpreadAssignment`, `ArrowFunctionExpr`, `FunctionExpr`,
  `LiteralMapExpr`, `InvokeFunctionExpr`, etc.). **Hard dependency** — must be
  ported first (spec 01).
- `./source_map` (`SourceMapGenerator`) — `addSource`, `addLine`, `addMapping`.
  Needed by `toSourceMapGenerator`.
- `../parse_util` (`ParseSourceSpan`) — source span type stored per part.

**Reverse dependencies (consumers):** `typescript_emitter.ts` (extends
`AbstractEmitterVisitor`), `abstract_js_emitter.ts` (extends it), and JIT
emitters which extend `AbstractJsEmitterVisitor`. The render3 pipeline builds the
`o.*` tree; this module turns it into text.

---

## 6. ɵɵ instructions / output emitted

**None.** This module emits **generic JS/TS source text**, not render3 runtime
instructions. It is operator-spelling, punctuation, indentation, quoting, and
source-map plumbing. The `ɵɵ`-prefixed instruction *calls* are constructed as
`o.InvokeFunctionExpr` / `o.ExternalExpr` nodes upstream; this emitter only
serializes whatever function-name string those nodes carry. The closest thing to
"emitted output" worth enumerating is the downlevel polyfill literal:

```
(this&&this.__makeTemplateObject||function(e,t){return Object.defineProperty?Object.defineProperty(e,"raw",{value:t}):e.raw=t,e})
```

emitted by `AbstractJsEmitterVisitor` for tagged templates and `$localize`.

---

## 7. Edge cases, gotchas, version sensitivity

1. **`lastIfCondition` parenthesization hack.** `visitIfStmt` stores the
   condition expression in `this.lastIfCondition`; `visitBinaryOperatorExpr` /
   `visitUnaryOperatorExpr` skip their own wrapping parens **iff** `ast ===
   lastIfCondition` (identity check). This is *stateful* and *not reentrant* in
   an obvious way. In the OXC port, parenthesization is handled by oxc_codegen's
   precedence logic; do **not** replicate the flag — instead lower the if-stmt
   condition without an extra wrapper and let codegen decide.
2. **Aggressive parenthesization.** Conditional exprs are always wrapped in `()`;
   binary/unary ops are always wrapped unless they're the if-condition. The
   source even has `// TODO: Do we *need* to parenthesize everything?` in
   `visitParenthesizedExpr`. OXC codegen will emit *minimal* parens by default —
   this is a **behavioral divergence** to flag. If byte-compatibility with
   tsc/ngc golden files matters, you may need custom parenthesization or to
   accept (and re-baseline) the difference.
3. **Single-quote string literals.** `escapeIdentifier` always single-quotes and
   only escapes `'`, `\`, `\n`, `\r` (not `\t`, not unicode, not `"`).
   `oxc_codegen` defaults to double quotes / different escaping. Configure
   `CodegenOptions` (quote style) or post-process to match; otherwise goldens
   diff.
4. **Number/`null`/`bool` literals via `` `${value}` ``.** Non-string literal
   values are stringified with JS template coercion — `null`→`"null"`,
   `true`→`"true"`, numbers via JS `Number.prototype.toString`. Watch
   float/`NaN`/`Infinity`/`-0` and large-int formatting differences between
   JS and Rust when lowering `o.LiteralExpr`.
5. **80-column wrapping with double-indent continuations** (`visitAllObjects`).
   This produces a *specific* wrapping/indent shape. oxc_codegen has its own line
   width logic (or single-line minified output). Goldens that depend on Angular's
   exact wrapping will differ unless reproduced.
6. **Two-space indent** (`INDENT_WITH`). oxc_codegen default indent differs;
   configure `indent_width`/`indent_char` accordingly.
7. **`visitTransplantedType` throws.** Must surface as an error in the port (these
   nodes only appear in some TS-emit paths and are intentionally unsupported here).
8. **JS emitter throws on `WrappedNodeExpr`.** `WrappedNodeExpr` wraps a foreign
   (e.g. TS `ts.Node`) AST that cannot be re-serialized to plain JS.
9. **`removeEmptyLastLine` + single-line if.** The single-statement if path emits
   the statement (which `println`s, creating a trailing empty line) then removes
   that empty line and adds a space — produces `if (c) { stmt; }`. Subtle; verify
   when reproducing.
10. **Comment emission** (`printLeadingComments`) only runs when `printComments`
    is true (JS emitter disables it). `JSDocComment` → `/*...*/`,
    multiline `LeadingComment` → `/* text */`, single-line → per-line `// text`.
    `trailingNewline` controls whether the comment ends the line.
11. **Version churn risk (medium).** Angular's `output_ast` adds/renames node
    kinds across versions (recent additions visible here:
    `RegularExpressionLiteralExpr`, `DynamicImportExpr`, `TaggedTemplateLiteralExpr`,
    `TemplateLiteralExpr`/`Element`, assignment-operator binary ops,
    `LiteralMapSpreadAssignment`, `SpreadElementExpr`, `ParenthesizedExpr`). When
    bumping Angular versions, re-diff this file — new `visit*` methods mean new
    `o.*` node kinds the lowering must handle. Pin to `22.1.0-next.0`.

---

## 8. Port plan (Rust / OXC)

### Strategy: lower `o.*` → `oxc_ast`, emit via `oxc_codegen` (do NOT port the text accumulator)

The TS emitter exists because Angular has no general JS AST + printer at hand.
The Rust port **does** (oxc). So the right architecture is:

1. **`output_ast` port (prereq, spec 01)**: port the `o.*` node enums/structs as
   arena-allocated Rust types (`#[derive]`, `oxc_allocator::Box<'a>`/`Vec<'a>`).
2. **A lowering pass `o_to_oxc<'a>(node, &AstBuilder<'a>) -> oxc_ast nodes`** that
   replaces `AbstractEmitterVisitor`. Each `visit*` method becomes a match arm
   that *constructs* an `oxc_ast` node via `AstBuilder`:
   - `o.BinaryOperatorExpr` → `ast.expression_binary` / `expression_logical` /
     `expression_assignment` (dispatch on the operator class — see §3.3).
   - `o.UnaryOperatorExpr` → `ast.expression_unary`.
   - `o.ConditionalExpr` → `ast.expression_conditional`.
   - `o.InvokeFunctionExpr` → `ast.expression_call` (handle `isOptional`).
   - `o.InstantiateExpr` → `ast.expression_new`.
   - `o.ReadPropExpr` → `ast.member_expression_static` (optional chaining).
   - `o.ReadKeyExpr` → `ast.member_expression_computed`.
   - `o.LiteralArrayExpr` / `LiteralMapExpr` → `ast.expression_array` /
     `expression_object` (+ spread elements/`LiteralMapSpreadAssignment`).
   - `o.FunctionExpr` / `ArrowFunctionExpr` / `DeclareFunctionStmt` →
     `ast.function` / `arrow_function_expression`.
   - `o.IfStmt`, `ReturnStatement`, `ExpressionStatement`, `DeclareVarStmt` →
     statement builders (`if`/`return`/`expression_statement`/`variable_declaration`
     with `const`/`let`/`var` kind).
   - Template literals / tagged templates → `ast.template_literal` /
     `tagged_template_expression`.
   - Types (TS emitter) → `TSType*` builders, gated on `printTypes`.
3. **Emission**: feed the lowered `oxc_ast::Program` to `oxc_codegen::Codegen`
   with `CodegenOptions` (quote style, indentation, source maps via
   `CodegenReturn { code, map }`). This replaces `toSource()` /
   `toSourceMapGenerator()` outright. Set the JS-emitter equivalent by toggling
   `printComments`/`printTypes`-style options + the downlevel transforms.
4. **Downlevel JS path** (`AbstractJsEmitterVisitor`): implement as lowering
   variants — when targeting ES5 JS, lower `o.TaggedTemplateLiteralExpr` /
   `o.LocalizedString` into the `makeTemplateObject` polyfill **call expressions**
   (built with AstBuilder) rather than template-literal nodes, and force `var`
   variable declarations. Reuse the polyfill string verbatim.
5. **Source spans**: carry each `o.*` node's `ParseSourceSpan` onto the
   corresponding `oxc_ast` node's `Span` (byte offsets) so oxc_codegen's source
   map maps back to original template positions. This subsumes the entire
   `EmittedLine.srcSpans` / `toSourceMapGenerator` machinery. May require a span
   registry/side-table since Angular spans are line/col into multiple source
   files, while oxc `Span` is byte offsets into one generated buffer — keep a
   `Vec<(generated_offset, ParseSourceSpan)>` and synthesize the source map
   manually if oxc's chunk-source model doesn't fit (port `source_map.ts`'s
   VLQ/segment logic if needed — see spec for `source_map.ts`).

### What to reuse from oxc
- `oxc_allocator::Allocator` + `oxc_ast::AstBuilder<'a>` — node construction.
- `oxc_ast::ast::{Expression, Statement, BinaryOperator, LogicalOperator,
  AssignmentOperator, UnaryOperator, TSType*}` — target node kinds.
- `oxc_codegen::{Codegen, CodegenOptions, CodegenReturn}` — printing + source map.
- oxc's built-in **precedence-based parenthesization** (replaces the manual
  always-parenthesize logic and the `lastIfCondition` hack).
- oxc's string-literal escaping / quote handling (configure to match Angular's
  single-quote convention if golden parity is required).

### Estimated complexity: **Medium**
- The per-node mapping is mechanical (~30 node kinds) but voluminous, and three
  semantic mismatches require care: (a) operator-class split, (b)
  parenthesization/quoting divergence vs. Angular goldens, (c) source-map model
  (line/col multi-file → byte-offset single-buffer). The `$localize`/tagged-template
  downlevel and `serializeI18n*` integration add a bit more.

### Ordering vs. other modules
1. `output_ast` (spec 01) — **must** precede this.
2. `parse_util::ParseSourceSpan` and `source_map` — needed for span mapping;
   `source_map` can be deferred if source maps are initially disabled.
3. *This* lowering+emit layer — sits between the render3 instruction builders and
   final text. Port it **after** `output_ast` and **before/with** the first
   end-to-end emit test, since every downstream compiler stage produces `o.*`
   trees that need printing to be testable.
4. The TS-emitter and JIT-JS-emitter subclasses come after this base.
