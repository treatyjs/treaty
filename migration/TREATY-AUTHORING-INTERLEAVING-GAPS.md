# Treaty authoring: free-interleaving & "easy and nice" recommendations

> Research + design doc. Compares the current `.treaty` SFC + JSX authoring model
> against Ripple, Svelte 5, and Vue Vapor, grounded in the real examples under
> `examples/**/*.treaty` and the current scanner in
> `apps/rust/authoring/src/treaty/lexer.rs` (+ `parser.rs`, `token.rs`).
>
> Theme (per the user): Treaty predates RippleTS but is "very similar"; we want it
> **easy and nice — mix file regions in any order**. This doc records concrete gaps
> and a design direction. It does NOT change code.

## 1. What Treaty does today (grounded in the lexer + examples)

The `.treaty` file is a single source with **no `<script>` wrapper**. The scanner
(`lexer.rs`) is a small hand-rolled state machine (`Default / JavaScript / HTML /
CSS / TemplateExpression / ControlFlow / Macro`) that segments the file into chunks:

- **Macro block** — a ```` ``` ````-fenced block, recognized **only at the very top
  of the file** (`at_file_top` flag, cleared after the first token; see
  `lex_default_state`). Optional info string (```` ```rsc ````).
- **TS-by-default body** — anything not inside a tag or `<style>` is `JavaScript`.
- **View** — tag-HTML detected directly by `<` (no `<template>` wrapper required; an
  optional `<template>` wrapper is unwrapped). `{{ }}` interpolation and `@if/@for/
  @switch/@defer` control flow are recognized.
- **`<style lang>`** — captured with an optional preprocessor `lang`.
- **`server { }`** — present in examples (`greeter.treaty`) as a TS marker; the lexer
  treats it as ordinary `JavaScript` (no dedicated token).

What already works well (keep it):

- TS-by-default, no `<script>` ceremony.
- `<template>` is genuinely optional and unwrapped (migration-friendly).
- TS and HTML regions **can already interleave** at top level — verified by the
  lexer test `no_template_wrapper_interleaves_ts_and_html` and by `greeter.treaty`
  (TS → `server {}` → more TS → view → `<style>`).
- Signals-by-default authoring reads cleanly in the examples (`signal`/`computed`/
  `input`/`effect` used bare; no `@Component`).

## 2. How the comparison frameworks handle "mix in any order"

| | Mixing model | Order constraint | Control flow |
|---|---|---|---|
| **Ripple** (.ripple) | Statements + markup are **peers in one lexical scope**; markup nests inside JS blocks and vice-versa | None — fully imperative interleave | Native `if`/`for` wrap markup directly |
| **Svelte 5** (.svelte) | **Sectioned** `<script>` / markup / `<style>`; runes for reactivity | Sections colocated, order conventional | `@if`/`@for` in markup only |
| **Vue Vapor** (.vue) | **Sectioned** `<script setup>` / `<template>` / `<style>` | Convention, not mandated | template-only directives |
| **Treaty today** | Sectionless TS-by-default + inline tag-HTML + `<style>` + top macro | **Macro must be first**; control-flow body not truly nested | `@if/@for` recognized as bare keyword tokens only |

The standout is **Ripple**: markup and logic are the *same* scope, so `if`/`for`
and DOM elements interleave with zero ceremony. Treaty's no-`<template>`, TS-by-default
design is already closer to Ripple than Svelte/Vue are — the gaps below are about
making the *scanner and grammar* actually honor that design.

## 3. Concrete gaps (where `.treaty` is more rigid / rougher than the design implies)

### G1. Control-flow bodies are not scoped — the biggest rough spot
`parse_control_flow` (`lexer.rs:471`) emits a single keyword token (`@if`, `@for`, …)
and **does not capture the `(condition)` or the `{ … }` body**. The `@`-from-default
path (`lex_default_state:137`) calls it **without `push_state`**, and `parser.rs`
turns each into a bare `AstNode::ControlFlow("@if")` string with **no condition, no
body, no nesting**. Consequence: the `(todos().length)` condition and the `{ <ul>…</ul> }`
block in `todo-list.treaty` are re-scanned as loose JS/HTML rather than as a structured
branch. `@else`/`@empty`/`@case` have no link to their opener. This is the core of
"the lexer is rogue/rough."

### G2. Macro block is positionally rigid (must be first)
`at_file_top` makes a ```` ``` ```` fence a macro **only** as the literal first token.
A leading import, a license banner comment, or any whitespace-significant lead-in
defeats it (the comment counts as the first content token). This violates "mix in any
order": the one compile-time region has a hard position rule. Compare Ripple/Svelte
where there is no "must be first" region.

### G3. `server { }` is invisible to the scanner
Examples treat `server {}` as a first-class server-fn marker (per the server-fn-inline
memory), but the lexer has **no `Server` state/token** — it falls into `JavaScript`.
Brace-balancing inside it is incidental (the JS scanner breaks on `;`/newline, not on
matching `}`), so a `server {}` block is split across multiple `JavaScript` tokens
rather than captured as one server region. There is no token to anchor extraction/
source-map exclusion on.

### G4. JS scanner breaks the body into line/`;` fragments
`parse_javascript` (`lexer.rs:238`) terminates a `JavaScript` token on newline, `;`,
`\f`, or `\r`. A multi-line `computed(() => { … })` becomes several tokens. It also
has no brace/paren depth tracking, so it cannot tell "end of a statement" from "end of
a block." This is why G1/G3 can't be solved by the current scanner without real
balanced-delimiter scanning. A `<` inside a TS generic or comparison (`a < b`) is only
saved from being mistaken for a tag because the HTML branch keys on specific lead-ins,
but `a<b>c` style generics in expression position are fragile.

### G5. Interpolation/expression scanning is brittle
`parse_template_expression` (`lexer.rs:436`) counts braces with a magic `brace_count
== -2` sentinel for the closing `}}` and only skips strings — no template-literal
`${…}` nesting, no comments. `{{ obj?.['a}b'] }}` or `{{ \`x${y}\` }}` can mis-terminate.

### G6. No directive / pipe authoring story in the scanner
`gauge.treaty` uses a selectorless directive (`<span HighlightDelta …>`) and a pipe
(`ratio() | percent01:1`) **inside HTML/interpolation text**. The scanner does not
model attributes, bindings (`[x]`, `(y)`, `[(two)]`), pipes, or directive references —
they survive only because the whole tag/expression is captured as an opaque string and
handed to a downstream Angular-template consumer. There is no Treaty-level ergonomic or
validation layer for authoring a directive/pipe.

### G7. Recovery is silent / lossy
Mismatched close tags are a TODO no-op (`parse_html:401`), an unrecognized `@word`
silently becomes JS (`parse_control_flow:511`), and slicing is defensively floored
rather than diagnosed. "Any order parses" needs *robust* scanning with real diagnostics,
not silent fallthrough — otherwise a typo produces baffling output instead of an error.

### G8. JSX (.tsx/.tjsx) path is a separate model
JSX authoring is "a fn returning JSX" — a different mental model from the sectionless
`.treaty` file. There is no shared interleaving grammar, so "easy and nice, any order"
has to be re-earned in the JSX path (control flow, signals, pipes) rather than shared.

## 4. Design direction to make it "easy and nice, mix in any order"

Ordered by leverage. None implemented here — design only.

1. **Make control flow a real nested region (fixes G1).** Give `@if/@for/@switch/
   @defer` a dedicated scanner state that captures the `(condition)` (balanced parens)
   and a `{ … }` body (balanced braces), recursing so markup, interpolation, and
   nested control flow live *inside* the branch. Emit structured tokens
   (`ControlFlowOpen{ kind, condition }`, body tokens, `ControlFlowClose`) and pair
   `@else/@empty/@case` to their opener in the parser. This is the single change that
   most closes the gap to Ripple's "native if/for around markup."

2. **Drop the "macro must be first" rule, or relax it (fixes G2).** Either allow a
   leading banner comment / imports before the fence, or (more in spirit) allow the
   compile-time block anywhere and key it on the fence + info string rather than file
   position. Preserves "any order."

3. **First-class `server { }` region (fixes G3).** Add a `Server` state/token with
   balanced-brace scanning so the block is one region the extraction + client
   source-map-exclusion pipeline can anchor on (per the server-fn / source-map memos).
   Treat the other two markers (`'use server'`, `$`) as aliases that produce the same
   region.

4. **Balanced, depth-aware TS scanning (fixes G4/G5).** Replace line/`;` termination
   with brace/paren/bracket depth + template-literal `${}` + comment awareness so a
   TS region ends at a real boundary (a `<TagName` in element position, a top-of-file
   fence, `<style`, or a control-flow `@`), and `a < b`, generics, and `\`${…}\``
   stop being hazards. This unblocks 1 and 3. Consider reusing OXC's lexer/parser for
   the embedded-TS spans rather than hand-rolling (aligns with the OXC-first memory),
   keeping the Treaty scanner responsible only for *region* boundaries.

5. **Robust recovery + diagnostics (fixes G7).** Unrecognized `@word`, mismatched
   close tags, and unterminated regions should produce a located diagnostic, not a
   silent slide into JS. "Any order parses" must mean "any order parses *or* tells you
   precisely why not."

6. **Author-level binding/pipe/directive model (fixes G6).** Model attributes and
   bindings (`[x]`/`(y)`/`[(x)]`), `| pipe:arg`, and selectorless directive references
   as tokens so Treaty can offer ergonomic authoring + validation instead of passing an
   opaque string through. Lets `.treaty` own the "nice" DX rather than deferring 100%
   to the Angular template consumer.

7. **Share one interleaving grammar with JSX (fixes G8).** Define the region/control-
   flow/signal model once and let both `.treaty` and `.tsx/.tjsx` reuse it, so the
   "mix in any order" promise is identical across authoring plugins (aligns with the
   authoring-plugin-system memory).

## 5. Suggested sequencing
G4 (balanced TS scanning) is the foundation; G1 (control-flow region) and G3 (`server`
region) build on it and deliver the most visible "easy and nice" wins; G2 and G7 are
small, high-trust ergonomics; G6 and G8 are larger follow-ups that generalize the model.

_Sources: in-repo only — `apps/rust/authoring/src/treaty/{lexer.rs,parser.rs,token.rs}`
and `examples/**/*.treaty`. External framework claims (Ripple/Svelte 5/Vue Vapor) come
from the orchestrator-provided Phase 1 research and are summarized, not independently
re-verified here._
