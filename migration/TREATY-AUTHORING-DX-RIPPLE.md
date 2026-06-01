# Treaty authoring DX — Ripple-inspired free-interleaving recommendations

> Research + design doc. A **ranked, concrete** set of authoring-DX improvements for
> `.treaty` SFCs and JSX (`.tsx`/`.tjsx`), inspired by RippleTS, Svelte 5, and Vue Vapor,
> centered on the user's theme: Treaty predates RippleTS but is "very similar" — we want it
> **easy and nice, mix file regions (markup / logic / style / server) in any order**.
>
> Scope: design only. This doc changes no code. Every claim about Treaty's *current*
> behavior is grounded in the in-repo scanner/parser
> (`apps/rust/authoring/src/treaty/{lexer.rs,parser.rs,token.rs}`) and the examples
> (`examples/**/*.treaty`). External framework behavior is cited; where a claim is from the
> orchestrator-provided Phase 1 research rather than independently re-verified, it is marked
> **[unverified-here]** so it is not mistaken for confirmed fact.
>
> **Cross-references.** A sibling design note already exists at
> `apps/rust/authoring/src/treaty/AUTHORING-INTERLEAVING-RECOMMENDATIONS.md` (gaps G1–G8 +
> design direction). This doc re-ranks that material as actionable DX recommendations and
> adds JSX parity. The task brief refers to `migration/JSX-TREATY-AUTHORING-PLAN.md` as the
> home of the planned lexer-hardening + pipe/directive authoring work; **that file does not
> yet exist in this branch** (verified: no `migration/JSX-TREATY-AUTHORING-PLAN.md`). Where a
> recommendation should land in that plan, it is tagged `→ JSX-TREATY-AUTHORING-PLAN` as a
> forward reference for when the plan is written; its concrete contents are not invented here.

---

## TL;DR — the ranked list

| # | Recommendation | Theme | Kind | Closes gap |
|---|---|---|---|---|
| **R1** | Control flow as a real **nested region** (capture condition + `{ }` body, recurse, pair `@else/@empty/@case`) | any-order + nice | **Compiler** | G1 |
| **R2** | **Balanced, depth-aware embedded-TS scanning** (reuse OXC for spans) | foundation | **Compiler** | G4/G5 |
| **R3** | First-class **`server { }` region** (+ `'use server'` / `$` aliases) | any-order | **Compiler** | G3 |
| **R4** | **Relax "macro must be first"** — allow leading comments/imports, key on the fence | any-order | **Compiler** | G2 |
| **R5** | **Robust recovery + located diagnostics** (no silent fallthrough) | nice | **Compiler** | G7 |
| **R6** | Author-level **binding / pipe / directive** model (tokens + validation) | nice | **Compiler** | G6 |
| **R7** | **One shared interleaving grammar** for `.treaty` + JSX | any-order parity | **Compiler** | G8 |
| **R8** | **Ergonomic conventions, docs & examples** for "any order" (pure authoring) | nice | **Pure-DX** | — |

"Kind" = whether it needs compiler/scanner changes or is achievable as documentation/
authoring convention only. R1–R7 require compiler work; R8 is pure-DX.

---

## Context: what already supports "mix in any order" today

Treaty's `.treaty` file is **sectionless** by design and is *already* closer to Ripple's
"markup + logic are peers" model than the sectioned Svelte 5 / Vue Vapor `<script>` /
template / `<style>` split:

- **TS-by-default body, no `<script>` wrapper.** Anything not inside a tag or `<style>` is a
  `JavaScript` chunk (`lexer.rs` `lex_default_state` → `parse_javascript`; test
  `ts_outside_tags_is_javascript`).
- **No `<template>` wrapper required; an optional one is unwrapped** (`parse_template_wrapper`,
  tests `optional_template_wrapper_is_unwrapped`, `nested_template_wrapper_balances_to_outer_close`).
- **TS and HTML already interleave at top level** — proven by the lexer test
  `no_template_wrapper_interleaves_ts_and_html` (TS → `<header>` → TS → `<footer>`) and by
  `greeter.treaty` (TS → `server {}` → more TS → view → `<style>`).
- **Signals-by-default reads cleanly** in the examples: `signal`/`computed`/`input`/`effect`
  used bare with no `@Component` ceremony (`gauge.treaty`, `greeter.treaty`, `todo-list.treaty`).

So the work below is **not a redesign** — it is making the scanner/grammar *honor* the
"any order" design the examples already assume.

**RippleTS precedent (the north star for this theme):** Ripple components are TypeScript
functions whose body is a single lexical scope in which JavaScript statements, control flow
(`if`/`for`), and markup are **peers** — markup nests inside JS blocks and vice-versa, with
no expression-slot constraints and no wrapper functions. Reactivity is fine-grained signals
with a compiler that emits direct DOM. **[unverified-here]** (Phase 1 research; Ripple by
Dominic Gannaway.) This is exactly the ergonomic Treaty's examples reach for but the scanner
does not yet structurally deliver (see R1).

---

## R1 — Control flow as a real nested region  *(top priority; Compiler)*

**What it is.** Make `@if/@else/@for/@empty/@switch/@case/@default/@defer` capture their
`(condition)` and `{ … }` body as a **structured, recursive region**, so markup,
interpolation, TS, and nested control flow live *inside* the branch — and `@else/@empty/@case`
are linked to their opener.

**Why (grounded).** This is the single biggest divergence from Ripple. Today:
- `parse_control_flow` (`lexer.rs:471`) emits only a **bare keyword token** (`ControlFlow(If)`,
  `ControlFlow(For)`, …) and captures **neither the condition nor the body**.
- The `@`-from-`Default` path (`lex_default_state`, `lexer.rs:137`) calls `parse_control_flow`
  **without `push_state`**, so there is no nested scope to return from.
- `parser.rs` `parse_control_flow_node` (`:44`) turns each kind into a bare
  `AstNode::ControlFlow("@if")` **string** — no condition, no body, no nesting, no else-linkage.

Consequence in `todo-list.treaty`: the `(todos().length)` condition and the
`{ <ul>…</ul> } @else { <p…> }` body are re-scanned as *loose* JS/HTML rather than as one
structured branch, and `@else` has no link to its `@if`.

**Ripple/other-framework precedent.** Ripple's native `if`/`for` wrap markup directly inside
the component scope **[unverified-here]**. Angular's own control-flow block syntax
(`@if (cond) { … } @else { … }`, `@for (x of xs; track …) { … } @empty { … }`) — which
Treaty's examples already use verbatim — is the *target shape*; it is a structured block with
a parenthesized head and a brace body, documented in the Angular control-flow guide
(angular.dev, "Control flow"). Treaty should scan it as that structure, not as a keyword token.

**Concrete `.treaty`/JSX change.**
- Add a dedicated `ControlFlow` scanner state that, on `@if`/`@for`/… : consumes the keyword,
  then a **balanced `( … )`** condition (depends on R2's depth tracking), then a **balanced
  `{ … }`** body that re-enters the region scanner (so it nests markup/TS/control-flow).
- Emit structured tokens, e.g. `ControlFlowOpen { kind, head: String }`, the body's inner
  tokens, and `ControlFlowClose` — replacing the bare `ControlFlow(kind)` keyword token.
- In `parser.rs`, build `AstNode::ControlFlow { kind, condition, body: Vec<AstNode>, branches }`
  and pair `@else/@empty/@case/@default` to the preceding opener (today they are unlinked
  bare strings).
- JSX parity: the same structured node backs `@if`/`@for` (or native `if`/`for`, see R7) in
  `.tsx/.tjsx`.

**Tie-in.** This is the centerpiece of the planned lexer-hardening work. `→ JSX-TREATY-AUTHORING-PLAN`
(structured control-flow grammar). Depends on **R2**.

---

## R2 — Balanced, depth-aware embedded-TS scanning  *(foundation; Compiler)*

**What it is.** Replace the line/`;`-delimited JS scanner with one that tracks `(){}[]` depth,
string + template-literal `${…}` nesting, and comments, so a TS region ends at a *real*
boundary (an element-position `<Tag`, the top fence, `<style`, or a control-flow `@`) instead
of at the next newline or semicolon.

**Why (grounded).**
- `parse_javascript` (`lexer.rs:238`) terminates a `JavaScript` token on `\n`, `\r`, `\f`, or
  `;` with **no brace/paren depth tracking**. A multi-line `computed(() => { … })` (exactly
  what `gauge.treaty` `ratio`/`band`/`delta` and `greeter.treaty` `headline` write) is split
  across several tokens.
- It cannot distinguish "end of statement" from "end of a block," which is *why* R1 (control-flow
  body) and R3 (`server {}` body) cannot be done correctly on top of the current scanner.
- `parse_template_expression` (`lexer.rs:436`) closes `{{ }}` with a magic `brace_count == -2`
  sentinel and only skips string literals — **no template-literal `${…}` and no comments**, so
  `{{ \`x${y}\` }}` or `{{ obj?.['a}b'] }}` can mis-terminate (gap G5).
- `<` inside a TS generic or comparison (`a < b`, `Array<T>`) survives only because the HTML
  branch keys on specific lead-ins; `a<b>c`-style generics in expression position are fragile (G4).

**Precedent.** Compiler-first SFC tools delegate embedded-language spans to a real parser
rather than re-implementing JS tokenization in the SFC scanner (Svelte/Vue compilers parse the
`<script>` contents with a JS/TS parser). For Treaty the in-repo north-star is **OXC-first**
(see `memory/MEMORY.md` → "Rust core / tsgo + oxc", "Node runtime architecture"): the Treaty
scanner should own only *region boundaries* and hand each TS span to OXC's lexer/parser for
balanced scanning. This avoids hand-rolling JS tokenization and aligns with the OXC 0.133
migration this branch is on.

**Concrete change.** Region-boundary scanner + OXC-backed span scanning for TS; rewrite
`parse_javascript` and `parse_template_expression` to be depth/string/template-literal/comment
aware. No authoring-syntax change — purely makes the existing examples lex into clean,
whole-expression chunks.

**Tie-in.** Foundation for R1 and R3. `→ JSX-TREATY-AUTHORING-PLAN` (lexer hardening). Highest
*enabling* leverage even though R1 is the most visible win.

---

## R3 — First-class `server { }` region  *(any-order; Compiler)*

**What it is.** A dedicated `Server` scanner state/token that captures a `server { … }` block
as **one balanced-brace region**, with `'use server'` and the `$` marker as aliases producing
the same region.

**Why (grounded).** `greeter.treaty` uses `server { async function greet(…) { … } }` as a
first-class server-fn marker, but the lexer has **no `Server` state/token** — it falls into
`JavaScript`, and because `parse_javascript` breaks on `;`/newline (not on the matching `}`),
the block is split across multiple `JavaScript` tokens (gap G3). There is **nothing for the
server-fn extraction + client-source-map-exclusion pipeline to anchor on** as a single region.

**Precedent (in-repo memory, authoritative for Treaty's own design).** `memory/MEMORY.md`:
"Server fn inline" — server fns are declared inline by default via `server{}` / `'use server'` /
`$`; "Server fn client source map" — extracted bodies must NOT appear in client source maps,
which requires a clean region boundary to exclude. The external "use server" directive
precedent is React Server Components / Next.js (the `'use server'` directive marking
server-only code) — cited as the lineage for the alias, per the Treaty server-fn memo.

**Concrete change.** `Server` state with balanced-brace scanning → `TokenKind::Server { body }`
→ `AstNode::Server`. The three markers (`server {}`, `'use server'`, `$`) normalize to the
same node so downstream extraction/source-map logic has one anchor. Depends on **R2**.

**Tie-in.** Unblocks the server-fn extraction + source-map exclusion memos.
`→ JSX-TREATY-AUTHORING-PLAN`.

---

## R4 — Relax "macro must be first"  *(any-order; Compiler)*

**What it is.** Allow a leading banner comment, imports, or whitespace before the top
```` ``` ````-fenced macro block — or key the compile-time block on the fence + info string
rather than strict file position.

**Why (grounded).** `at_file_top` (`lexer.rs:41,107`) recognizes the fence as a macro **only**
as the literal first content token; it is cleared after the first token (`lex_default_state:112`).
A leading license banner or import defeats it — the comment becomes the first content token
(gap G2). The lexer test `fence_only_recognized_at_file_top` encodes this hard rule. This is the
one compile-time region with a **mandatory position**, which directly contradicts "mix in any
order." All three example macros (`todo-list`, `greeter`, `gauge`) are forced to put the fence at
byte 0, *above* their explanatory comments.

**Precedent.** Ripple/Svelte have **no must-be-first region** — reactive/compile constructs are
positioned freely **[unverified-here]** (Phase 1 research). Relaxing this is the smallest change
that most literally honors the "any order" promise.

**Concrete change.** Either (a) skip leading comments/imports/whitespace before testing the
fence, or (b) recognize the fenced compile-time block anywhere and key it on the fence + info
string. Note the trade-off: a fence appearing mid-body could collide with a markdown-in-string
or doc use, so option (a) (relax the lead-in only) is the lower-risk first step; option (b)
needs a disambiguation rule. **This needs a deliberate design decision, not a blind change** —
flag for the plan.

**Tie-in.** `→ JSX-TREATY-AUTHORING-PLAN`. Small, high-trust ergonomic once the rule is chosen.

---

## R5 — Robust recovery + located diagnostics  *(nice; Compiler)*

**What it is.** Turn silent/lossy fallthrough into **located diagnostics**: unrecognized
`@word`, mismatched close tags, and unterminated regions should report *where* and *why*,
not slide silently into JS.

**Why (grounded).**
- Mismatched close tags are a **TODO no-op** (`parse_html:401` — the `if &tag_name != expected_tag`
  branch is empty).
- An unrecognized `@word` **silently becomes JavaScript** (`parse_control_flow:511`).
- Slicing is defensively *floored* to a char boundary (`floor_char_boundary`, `slice`) rather
  than diagnosed — robust against panics, but it hides off-by-one bugs instead of surfacing them.
- The parser has no recovery: `previous()` (`parser.rs:86`) `unwrap()`s, and EOF is the only
  terminator.

"Any order parses" must mean "any order parses **or tells you precisely why not**." Without
this, a single typo (e.g. `@fi` instead of `@if`) produces baffling output rather than an error.

**Precedent.** Modern SFC compilers emit positioned diagnostics with source spans (Svelte/Vue
compiler errors carry `start`/`end`). Treaty tokens already carry `start`/`end`
(`token.rs` `Token { start, end }`), so the spans needed for good diagnostics already exist —
they are just unused on the error paths.

**Concrete change.** Add a diagnostics channel keyed off existing token spans; replace the three
silent fallthroughs above with located warnings/errors and minimal recovery (resync to the next
region boundary). Pairs naturally with R1/R2 (which introduce the structured regions whose
mismatches are worth diagnosing).

**Tie-in.** `→ JSX-TREATY-AUTHORING-PLAN`. Small, high-trust; do alongside R1/R2.

---

## R6 — Author-level binding / pipe / directive model  *(nice; Compiler)*

**What it is.** Model HTML attributes and bindings (`[x]`, `(y)`, `[(two)]`), pipes
(`| pipe:arg`), and selectorless directive references as **first-class tokens**, so Treaty can
offer ergonomic authoring + validation rather than passing an opaque string through.

**Why (grounded).** `gauge.treaty` uses a selectorless directive (`<span HighlightDelta
[delta]="delta()">`) and pipes (`ratio() | percent01`, `ratio() | percent01:1`) inside HTML /
interpolation. The scanner does **not** model attributes, bindings, pipes, or directive
references — `parse_html` captures the whole tag run as one opaque `HTML(String)` and
`parse_template_expression` captures the whole `{{ }}` as one opaque string (gap G6). They
survive only because a downstream Angular-template consumer re-parses them. There is **no
Treaty-level ergonomic or validation layer** for authoring a directive or pipe — e.g. a typo in
a pipe name or a binding bracket gets no Treaty feedback.

**Precedent.** Angular's template binding/pipe syntax (`[prop]`, `(event)`, `[(ngModel)]`,
`expr | pipe:arg`) is the authoring target (angular.dev template-syntax + pipes guides), and
selectorless directives/components are the modern Angular direction Treaty already leans into
("selectorless" appears in the gauge/greeter comments and `memory/MEMORY.md` Treaty defaults).
Modeling these as tokens is what lets `.treaty` *own* the "nice" DX (autocomplete, validation,
clear errors) instead of deferring 100% to the template consumer.

**Concrete change.** Sub-tokenize tag interiors and interpolation expressions into
attribute/binding/pipe/directive-reference tokens (built on R2's balanced scanning). Larger than
R1–R5; a follow-up that generalizes the model. The brief calls out "pipe/directive authoring" as
already-planned work.

**Tie-in.** This is the "pipe/directive authoring" track of `→ JSX-TREATY-AUTHORING-PLAN`.

---

## R7 — One shared interleaving grammar for `.treaty` + JSX  *(any-order parity; Compiler)*

**What it is.** Define the region / control-flow / signal model **once** and have both `.treaty`
and `.tsx/.tjsx` reuse it, so "mix in any order" is identical across authoring plugins.

**Why (grounded).** JSX authoring is "a fn returning JSX" — a *different* mental model from the
sectionless `.treaty` file (gap G8). There is no shared interleaving grammar today, so the
"easy and nice, any order" promise has to be re-earned in the JSX path (control flow, signals,
pipes) instead of shared. `memory/MEMORY.md` → "Authoring plugin system" makes front-end formats
pluggable (.treaty + JSX are plugins) and "Treaty JSX authoring" describes a JSX-flavored Angular
front-end with `@control-flow` and JS loops → `@for`, signals-by-default — i.e. the *intent* is
already parity, but the grammar is not yet shared.

**Precedent.** Ripple itself uses **one** model — a TS function whose body interleaves statements
and markup — for its single authoring format **[unverified-here]**; Treaty's parity goal is to
make `.treaty` (sectionless) and JSX (fn-returning-markup) two surfaces over the *same*
region/control-flow/signal core, the way Ripple has one. This keeps R1's structured control flow,
R2's balanced scanning, and R6's binding/pipe model from being implemented twice.

**Concrete change.** Factor the region + control-flow + signal grammar into a shared core
consumed by both authoring plugins; the `.tsx/.tjsx` front-end maps native `if`/`for` (or
`@if`/`@for`) and JSX elements onto the same `AstNode`s as `.treaty`. Largest item; do after
R1–R3 stabilize the core.

**Tie-in.** `→ JSX-TREATY-AUTHORING-PLAN` (the JSX half of the plan's name). Aligns with the
authoring-plugin-system + Treaty-JSX-authoring memos.

---

## R8 — Ergonomic conventions, docs & examples for "any order"  *(pure-DX; no compiler change)*

**What it is.** The only **pure-DX** item: codify and document the "mix in any order" ergonomics
that already work, with example coverage, so authors discover them — independent of R1–R7.

**Why (grounded).** The examples demonstrate interleaving (TS → `server {}` → TS → view →
`<style>` in `greeter.treaty`) and the lexer test `no_template_wrapper_interleaves_ts_and_html`
proves multiple HTML regions interleave with TS — but there is **no authoring guide** stating
the rules (TS-by-default, `<template>` optional, where macros/`server{}` may appear, signals-by-
default). Authors currently infer the model from comments inside the examples.

**Concrete (no scanner change needed today).**
- An authoring guide: "any region, any order — what's allowed and why" (TS-by-default, optional
  `<template>`, `<style lang>`, top macro, `server{}`), with the *current* constraints called
  out honestly (macro-must-be-first until R4 lands; control-flow body limits until R1 lands).
- Example coverage for interleaving patterns beyond the four current files (e.g. multiple view
  regions split by TS; `<style>` before the view).
- Signals-by-default conventions doc (bare `signal/computed/input/effect`, no `@Component`),
  matching the gauge/greeter examples and the "Signals by default" memo.

**Tie-in.** Can ship immediately; gives users the "easy and nice" story while R1–R7 are built.
Update it as each compiler change relaxes a constraint.

---

## Sequencing & prioritization

1. **R2 (balanced TS scanning)** — foundation; nothing structured is correct without it.
2. **R1 (control-flow region)** + **R3 (`server{}` region)** — the most *visible* "easy and
   nice / any order" wins; both build on R2.
3. **R4 (relax macro-first)** + **R5 (diagnostics)** — small, high-trust ergonomics; R4 needs a
   deliberate disambiguation decision.
4. **R6 (binding/pipe/directive)** + **R7 (shared JSX grammar)** — larger follow-ups that
   generalize the model.
5. **R8 (docs/conventions)** — ship now; pure-DX, no waiting on the compiler.

**For the user's "easy and nice, mix in any order" goal specifically, prioritize R2 → R1 → R3 →
R4**, with **R8 in parallel** so the story is documented while the scanner catches up to the
design.

---

_Sources. In-repo (verified this session): `apps/rust/authoring/src/treaty/{lexer.rs,parser.rs,
token.rs}`; `examples/everything-app/src/components/todo-list.treaty`,
`examples/everything-app/src/features/greeter/greeter.treaty`,
`examples/everything-app/src/features/metrics/gauge.treaty`,
`examples/file-routed-app/routes/blog/[slug]/index.treaty`;
`apps/rust/authoring/src/treaty/AUTHORING-INTERLEAVING-RECOMMENDATIONS.md`;
`C:\Users\Jordan\.claude\projects\d--dev-treaty\memory\MEMORY.md` (server-fn-inline,
server-fn-client-source-map, authoring-plugin-system, treaty-jsx-authoring, signals-by-default,
rust-core-ts-shim-layering, node-runtime-architecture). External framework behavior
(RippleTS / Svelte 5 / Vue Vapor) is from the orchestrator-provided Phase 1 research and is
marked **[unverified-here]** where not independently confirmed; Angular control-flow, template-
binding, and pipe syntax are the documented Angular forms (angular.dev) that Treaty's examples
already use verbatim. `migration/JSX-TREATY-AUTHORING-PLAN.md` does not yet exist in this branch;
`→ JSX-TREATY-AUTHORING-PLAN` tags are forward references, not citations of existing content._
