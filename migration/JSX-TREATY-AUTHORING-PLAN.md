# JSX + .treaty pipe/directive authoring — ordered, green-at-every-step execution plan

Status: PLAN ONLY (this doc is the single write of phase 3). Every step below was sized against
code that was READ, not regex-matched. All steps edit `apps/rust/authoring/**` and recompile
`cargo` against `treaty_ivy`, so they are serialized AFTER the in-flight server-fn-unify workflow
releases the `apps/rust/authoring` + `cargo target/` lock. Do not start until that workflow is done.

## Invariants that must hold at EVERY step (the green bar)

- `cargo test -p rust_authoring` stays green (the authoring crate owns the lexer, sfc, jsx tests).
- `treaty_ivy` is NOT modified by any step here — so `matchGolden 170/185` (a treaty_ivy/decorators
  property, see `migration/STATUS.md:96`) cannot regress: these steps only add new *callers* of the
  already-shipped `compile_directive_from_metadata` / `compile_pipe_from_metadata` emitters.
- The everything-app `source-validate` gate (`examples/everything-app/source-validate.e2e.mjs`,
  24 owned-file contract rows) stays 24/24. It routes each file through `TreatyCompiler.transform`
  by extension and asserts component→`ɵɵdefineComponent`, `@Directive`→`ɵɵdefineDirective`,
  `@Pipe`→`ɵɵdefinePipe`, no surviving decorator, server-fn privacy. New fixtures added in parts B/C
  must each be added as a PASSING row in the same commit that makes them compile, never before.
- Every new test is PARSE-BASED: assert facts off an oxc/`@babel` parse of the emitted module
  (the crate's existing `assert_well_formed_module` / `assert_*_client_parses` helpers in
  `sfc.rs` / `jsx/mod.rs`), never a regex/substring over authoring source.

## Sequencing rationale

Lexer hardening FIRST (A) — every `.treaty` feature, including part C, reads the lexer's token byte
spans (they drive both `split_chunks` AND `mask_non_js_regions`, sfc.rs:81 / sfc.rs:121). A region
mis-split silently corrupts directive/pipe authoring downstream. THEN JSX (B) — it is
self-contained in `jsx/**` and does not touch the lexer. THEN `.treaty` (C) — it depends on a robust
lexer (A) and reuses the JSX discriminator design proven in (B).

---

## PART A — LEXER HARDENING (`apps/rust/authoring/src/treaty/lexer.rs` unless noted)

Each step: a single rough edge, the file:fn, the fix, the regression test, the invariant. The steps
are independent of each other and ordered cheapest-first; after each, run `cargo test -p
rust_authoring` and confirm green before the next. Note: the `server { … }` statement-position guard
(`plugin/mod.rs:668 server_in_statement_position`) and the non-ASCII char-boundary panics
(`lexer.rs:10 floor_char_boundary` + the `non_ascii_in_every_region_does_not_panic` test) were
VERIFIED already fixed — they are listed here only as the regression baseline, NOT as work.

### A1. Regex literal in a JS chunk truncates the region (`parse_javascript`, lexer.rs:219)

- Bug: the JS scanner only special-cases `'` `"` `` ` `` strings and `//` `/*` comments. A regex
  literal is unhandled, so `/` falls to the bare `self.advance()` arm (line 235). A regex whose body
  contains `</` (e.g. `const re = /<\/div>/g`) or `{{` triggers the `starts_with("</")` break
  (line 242) or the `{{` break (line 243) MID-REGEX, splitting the JS region and shoving the rest of
  the regex into an HTML/interpolation token. The body then fails to parse and its imports leak into
  the wrapper.
- Fix: add a `/`-arm branch that, when the `/` is in regex position, consumes a full regex literal
  (body honoring `\`-escapes and `[...]` character classes, then flags). Regex-vs-divide
  disambiguation: track the last significant non-whitespace code char; a `/` is a regex start unless
  that char is one that can end an expression (ident char, `)`, `]`, `}`, or a digit) — the same
  `can_end_statement` rule already proven in `plugin/mod.rs:720`. Keep it conservative: when unsure,
  treat as divide (current behavior), so this only ADDS coverage.
- Test (`lexer.rs` tests mod): `regex_literal_with_close_tag_stays_in_js` — lex
  `const re = /<\/div>/g;\n<div>hi</div>` and assert exactly one `JavaScript` token contains the whole
  `/<\/div>/g` and the `HTML` token is `<div>hi</div>` (not a fragment). Add a paired sfc.rs test
  `treaty_body_regex_does_not_split_region` compiling the same source and asserting the emitted module
  RE-PARSES (existing oxc parse helper) and `defineComponent` is present.
- Invariant: the JS/HTML split is byte-correct → `mask_non_js_regions` masks the right spans →
  server-fn lift + import hoist stay correct. No treaty_ivy change.

### A2. Unrecognized `@token` loses the `@` and one char (`parse_control_flow`, lexer.rs:471)

- Bug: the `else` fallthrough (lexer.rs:510-515) for an `@` that is NOT a known control-flow keyword
  does `self.state = LexerState::JavaScript; self.advance(); self.parse_javascript()`. The bare
  `self.advance()` consumes the `@` (and `parse_javascript` starts AFTER it), so an authored
  `@Input`-style decorator or an email `a@b.com` or `@HostBinding` written in a JS chunk loses its
  leading `@`, corrupting that JS token. (It also does NOT push a state to balance the pop in
  `parse_javascript`, but the start-position bug is the user-visible one.)
- Fix: in the fallthrough, do NOT advance past the `@`. Re-enter JS scanning at the `@` so the byte
  is preserved in the JavaScript token. Concretely: set state to JavaScript and call
  `parse_javascript` with the cursor still on `@` (drop the `self.advance()`); ensure `parse_javascript`
  treats a leading `@` as an ordinary code char (its `'@'` arm at lexer.rs:244 currently pushes
  ControlFlow + breaks immediately on a zero-length token — guard it so a `@` at `start_pos` is
  consumed as code rather than producing an empty token / re-looping).
- Test: `at_decorator_in_js_chunk_is_preserved` — lex `const x = a@b;\n<div></div>` (or a clearer
  `@`-bearing JS) and assert the `JavaScript` token still contains the `@`. Guard against an infinite
  loop / empty-token regression with an explicit token-count assert.
- Invariant: no JS byte is dropped at an `@` boundary; control-flow markers (`@if`/`@for`/…) still
  tokenize unchanged (the existing control-flow path is untouched).

### A3. `@`-at-JS-start emits a zero-length JS token then re-enters (`parse_javascript`, lexer.rs:244)

- Bug: when `lex_default_state` sees `@` (line 137) it calls `parse_control_flow` directly. For a
  recognized keyword that is fine. But `parse_javascript`'s own `'@'` arm (line 244) `push_state(
  ControlFlow); break` fires even when `@` is the FIRST char of the JS scan, producing a zero-width
  `JavaScript("")` token and an unbalanced state push. The empty token survives into `split_chunks`
  as an empty `javascript` entry (harmless to join, but a latent correctness/þerf smell and a real
  hazard once A2 makes JS re-enter on `@`).
- Fix: in `parse_javascript`, only break on `@` when `self.pos > start_pos` (i.e. there is real JS
  before it). When `@` is at `start_pos`, let the default-state router handle it (it already does via
  line 137), or consume it as code per A2. Do not emit empty `JavaScript` tokens.
- Test: `no_empty_javascript_token_before_control_flow` — lex `@if (x) { }<div></div>`-shaped input
  and assert NO `JavaScript("")` (empty) token is produced and the `@if` `ControlFlow` token is.
- Invariant: token stream carries no zero-width JS tokens; control flow still recognized. (Do A2+A3
  in one commit since they share the `parse_javascript` `'@'` arm.)

### A4. `parse_html` leading `consume_whitespace` + single-root break drops sibling/text content (`parse_html`, lexer.rs:385)

- Bug: `parse_html` breaks as soon as `tag_stack` empties after the FIRST root element closes
  (lexer.rs:408-410). Trailing TEXT in the same logical template region after that close (e.g.
  `</span> of {{ max() }}` as in `gauge.treaty:69`) is left for the default router, which — seeing a
  non-`<`, non-`{{`, non-`@` char — routes it to `parse_javascript`, so template prose leaks into the
  JS body. Today this is masked because such prose currently sits inside a single enclosing element,
  but interleaved text-after-element is a documented `.treaty` shape (TS/HTML freely interleave,
  sfc.rs:80). Also, the top-of-loop `consume_whitespace` (line 390) before re-reading `ch` means a
  region that is pure leading whitespace+text is mishandled.
- Fix: this is the highest-risk lexer step — scope it tightly. Make `parse_html` treat trailing text
  up to the next region boundary (`<` of a new tag/`</`, `<style`, `{{`, `@`, or EOF) as part of the
  HTML token rather than breaking immediately at `tag_stack` empty when the following non-whitespace
  byte is plain text. Keep `{{ … }}` INSIDE the HTML token (the existing contract, sfc.rs:80 — the
  interpolation stays in the surrounding HTML chunk). Do NOT change behavior when the next byte is a
  new top-level `<tag>`/`<style>`/`@block` (those legitimately start a new region).
- Test: `html_trailing_text_after_root_close_stays_html` — lex `<span>x</span> tail text {{ y }}\n` and
  assert the `HTML` token contains `tail text {{ y }}` and NO `JavaScript` token captures `tail text`.
  Add the gauge-shaped sfc.rs test `treaty_interleaved_text_after_element_binds_in_template`
  asserting the emitted template binds `ctx.max` (proving the prose reached render3, not the JS body).
- Invariant: template prose never leaks into the JS body; `{{ }}` stays in HTML; a new top-level
  element/style/block still opens a fresh region. This must be the LAST A-step because it is the most
  behavior-sensitive; run the full sfc.rs suite after it.

### A5. Macro block binds `$macro=null` while the body uses bare macro identifiers (`run_and_encode_macros`, sfc.rs:710 / `compile_treaty_file_inner`, sfc.rs:630)

- Bug: a statement-only macro (no `return`, e.g. `gauge.treaty:1-12` which ends with
  `const macroMeta = {…}`) produces `$macro = null` (run_macro returns null), yet the component body
  references the bare `macroMeta` identifier (`gauge.treaty:39,60`). The macro SOURCE is dropped
  (sfc.rs:629 comment), so `macroMeta` is undefined at runtime — the body silently loses its
  compile-time constants. Today `gauge.treaty` only "works" because nothing asserts `macroMeta`
  resolves.
- Fix: when a macro block has no `return`, inject its top-level `const`/`let` declarations as
  module-visible constants (their COMPUTED values, JSON-encoded the same way `run_and_encode_macros`
  already encodes the `$macro` value) under their authored names, in addition to (or instead of) the
  null `$macro`. Mechanism: extend `treaty_runtime::run_macro` usage to also surface the macro's
  top-level binding map (already evaluated on Nova), then emit `const macroMeta = <json>;` for each.
  Encode-safety is identical to the existing `$macro` splice (serde_json → valid JS literal,
  sfc.rs:732). Keep the existing `$macro` behavior for macros that DO `return` (back-compat).
- Test (sfc.rs tests mod): `macro_top_level_const_is_injected_under_its_own_name` — compile a source
  with a `return`-less macro declaring `const meta = { a: 1 }` and a body/template using `meta.a`;
  assert the emitted module contains `const meta = {` with `"a":1`, that `meta` is in the returned
  bindings, and that the module RE-PARSES. Assert the macro SOURCE arithmetic does not leak (mirror
  `macro_data_is_injected_and_bindable_in_template`, sfc.rs:1966).
- Invariant: every macro-declared constant the body references resolves at runtime; macro source
  never leaks; `$macro` (return form) unchanged. CONFIRMED SCOPE: this step DOES touch
  `treaty_runtime` (`libs/runtime/src/lib.rs`). Verified: `run_macro` (lib.rs:351) wraps the body in
  `(function () { …body…; return undefined; })()` (`transpile_macro_to_js`, lib.rs:388-398), so a
  `return`-less macro's top-level `const`s are IIFE LOCALS and only `undefined`→null surfaces. The fix
  must add a binding-capture variant (e.g. a `run_macro_with_bindings` returning both the value AND a
  `Map<name, json>` of the macro's top-level declarations) by appending a synthesized
  `return { <names> };` (or capturing the function's own scope) — a `treaty_runtime` change — then
  sfc.rs emits one `const <name> = <json>;` per binding. Because this crosses crate boundaries,
  A5 may be split into a `treaty_runtime` sub-step (green on its own unit tests) then the sfc.rs
  consumption; both are still serialized after the cargo lock frees. If A5 proves heavier than its
  payoff this iteration, it can be deferred WITHOUT blocking parts B/C (it is an independent bug, not
  a dependency of directive/pipe authoring) — note it as a follow-up rather than gating B/C on it.

---

## PART B — JSX pipe + directive authoring (`apps/rust/authoring/src/jsx/**`)

Today `jsx::compile` (jsx/mod.rs:215) ALWAYS lowers to a component: it locates a JSX-returning
function (`find_component`, jsx/mod.rs:411) and funnels through
`compile_from_parts_with_directives_and_map` (jsx/mod.rs:327), which builds an `R3ComponentMetadata`
and emits `ɵɵdefineComponent` (sfc.rs:960, 993). A directive/pipe authored in `.tsx`/`.tjsx` is
either mis-emitted as a component or fails. Parts B reuses the SHIPPED emitters
`compile_directive_from_metadata` (decorators/src/compiler.rs:1713 → `ɵɵdefineDirective`) and
`compile_pipe_from_metadata` (decorators/src/pipe_module_injector.rs:155 → `ɵɵdefinePipe`), exactly
as the `.ts` path does (source_compile.rs:2761 `compile_directive_meta`, :2781 `compile_pipe_class`).
No new IR.

### B1. New shared standalone-def emit helpers in `sfc.rs` (no JSX wiring yet)

- Add two crate-public funcs in `sfc.rs` beside `compile_from_parts*`, each returning
  `CompiledComponent { code, errors }` (reuse the existing struct; "component" is just the carrier):
  - `compile_directive_from_parts(class_name, javascript, file_name) -> CompiledComponent` — build an
    `R3DirectiveMetadata` IDENTICAL to the component base literal at sfc.rs:936-958 EXCEPT
    `selector: None` (directives are legitimately selectorless — source_compile.rs:2115 does exactly
    this), reuse `extract_io` (sfc.rs:289) for `input()`/`output()`/`model()` → inputs/outputs, call
    `compile_directive_from_metadata(&base, &mut StubHostBindingsBuilder)`, emit via
    `emit_expression`, and assemble the module with a `<Class>.ɵdir = …;` static (mirror
    `build_module` sfc.rs:510 but with `\u{0275}dir` and a directive factory — factor the wrapper so
    the component/directive/pipe paths share the import-hoist + `ɵfac` emit).
  - `compile_pipe_from_parts(class_name, javascript, pipe_name, file_name) -> CompiledComponent` —
    build `R3PipeMetadata { name: class_name, type: class_ref, type_argument_count: 0, pipe_name:
    Some(name), deps: None, pure: true, is_standalone: true }` (fields verified at
    pipe_module_injector.rs:135-150), call `compile_pipe_from_metadata` (its `statements` is always
    empty — pipe_module_injector.rs:196 — so NO constant-pool hoist), emit, assemble with a
    `<Class>.ɵpipe = …;` static.
- Test (sfc.rs tests mod), pure unit, no JSX: `directive_from_parts_emits_define_directive` and
  `pipe_from_parts_emits_define_pipe` — call each helper directly, assert the emitted module RE-PARSES
  (existing oxc helper), contains `ɵɵdefineDirective` / `ɵɵdefinePipe`, the `ɵdir`/`ɵpipe` static, the
  `ɵfac` factory, and `export default <Class>;`. Pipe test asserts `name: "<pipeName>"` literal.
- Invariant: these helpers are dead code until B2/B3 call them, so this step is purely additive and
  cannot regress any existing path; matchGolden untouched (treaty_ivy unchanged, only new callers).

### B2. JSX discriminator: route a `.tsx`/`.tjsx` file to component / directive / pipe (jsx/mod.rs)

- Discriminator (read off the already-in-hand `ret.program` parse, jsx/mod.rs:236 — never a regex):
  classify the file's primary authored unit in priority order, mirroring how `find_component` scans:
  1. PIPE — a class/function carrying a `transform(...)` method AND a Treaty pipe marker. Treaty has
     no decorators in JSX, so the marker is an explicit `pipe(...)`/`definePipe`-style helper OR (the
     low-ceremony form) a default-exported function/class named with a `Pipe` suffix that returns no
     JSX and exposes `transform`. DECISION POINT: pick ONE marker convention and document it in the
     doc-comment; recommend a `pipe('name')` wrapper call (analogous to `input()`), so the pipe NAME is
     explicit and authored, not derived. The pipe name string is the auto-import key the binder uses.
  2. DIRECTIVE — a unit that exposes a `host`-spec / lifecycle but returns NO JSX (the
     `highlight.directive.ts:23` shape: a function returning `{ host: {...} }`). In JSX the
     discriminator is "default export returns no JSX element AND is not a pipe". `find_component`
     returning `None` (jsx/mod.rs:280) is exactly today's "no JSX" signal — today that is an error;
     B2 reinterprets a no-JSX default export as a candidate directive instead of an error.
  3. COMPONENT — the existing path (a JSX-returning function). Unchanged.
- Wire: in `jsx::compile`, after the server-lift/preprocess (unchanged, jsx/mod.rs:221-229) and the
  TSX parse (jsx/mod.rs:236), branch on the discriminator. Component branch = the current body
  verbatim. Directive branch = assemble the flat body (reuse `assemble_flat_body`, jsx/mod.rs:632, but
  WITHOUT a template) and call `compile_directive_from_parts`. Pipe branch = call
  `compile_pipe_from_parts` with the authored pipe name. All three still flow through the SAME
  server-lift, signals pass (directives have inputs too), map emission, and `export_server_fn_bindings`
  wiring already in `compile` — refactor `compile` so the tail (steps 4-6, jsx/mod.rs:299-385) is
  shared and only the "lower to def" middle differs.
- Tests (jsx/mod.rs tests mod), parse-based, using the existing `assert_well_formed_module`
  (jsx/mod.rs:774):
  - `jsx_directive_emits_define_directive` — a no-JSX default export with a `host` spec compiles to a
    module containing `ɵɵdefineDirective`, NO `ɵɵdefineComponent`, and re-parses.
  - `jsx_pipe_emits_define_pipe` — a pipe-marked unit compiles to `ɵɵdefinePipe` with the authored
    `name:` literal, NO component, and re-parses.
  - `jsx_component_still_emits_define_component` — regression: an existing JSX component fixture
    (reuse `compiles_trivial_default_export_function_component`, jsx/mod.rs:814) is UNCHANGED.
- Invariant: component path byte-identical to today (assert one existing component test's emit is
  unchanged); directive/pipe paths emit the correct `define*`; no JSX-returning unit is ever
  mis-routed. The "no component found" error (jsx/mod.rs:281) only fires now when the unit is neither
  component NOR directive NOR pipe.

### B3. Fixture + source-validate row for JSX directive/pipe

- Add ONE JSX directive fixture and ONE JSX pipe fixture under
  `examples/everything-app/src/**` (e.g. a `.tjsx` directive sibling to `counter.tsx`, and a `.tjsx`
  pipe), authored in the chosen Treaty conventions, and add their PASSING rows to
  `source-validate.e2e.mjs` (the gate enumerates `src/**`, so a new file is auto-picked up; ensure it
  asserts `ɵɵdefineDirective` / `ɵɵdefinePipe` and no surviving decorator). Commit the fixture +
  the green gate together so the gate stays 24/24→26/26.
- Invariant: `node examples/everything-app/source-validate.e2e.mjs` exits 0 with the new rows PASS.

---

## PART C — .treaty pipe + directive authoring (lexer region + `apps/rust/authoring/src/sfc.rs`)

Today `.treaty` always emits a component: `compile_treaty_file_inner` (sfc.rs:596) joins HTML→template
and funnels through `compile_from_parts` (sfc.rs:684 / :762 → `ɵɵdefineComponent`). A `.treaty`
directive has NO template; a `.treaty` pipe is a transform with a name. The lexer change is minimal
because directive/pipe authoring is PURE TypeScript in the JS region — no new markup region is needed.

### C1. `.treaty` discriminator over the JS region (sfc.rs, reuse B1 helpers)

- A `.treaty` file's unit kind is decided over the JOINED `javascript` chunk (already parsed by
  `extract_io`/`extract_wrapper_parts` with oxc, sfc.rs:289/410 — reuse that parse, never regex):
  - PIPE — a top-level declaration carrying `transform` + the same `pipe('name')` marker chosen in
    B2 (one convention across both front-ends). A `.treaty` pipe has NO HTML chunk.
  - DIRECTIVE — the file has NO HTML chunk (`chunks.html` empty, sfc.rs:615) AND declares a
    `host`-spec unit (or, low-ceremony: any `.treaty` with no template region is a directive
    candidate). The "no template" signal is the natural `.treaty` discriminator and needs NO lexer
    change — a directive `.treaty` is just TS with no `<tag>`/`{{ }}`.
  - COMPONENT — there is an HTML/template region. Unchanged (the common case).
- Wire: in `compile_treaty_file_inner`, after `split_chunks` (sfc.rs:612), branch:
  `template_html.is_empty()` + pipe-marker → `compile_pipe_from_parts`; `template_html.is_empty()` +
  directive-shape → `compile_directive_from_parts`; else the existing component path verbatim. The
  server-lift, macro injection (A5), style compile, and map emission stay in the shared prologue;
  only the "lower" call differs. The server-aware wrapper `compile_treaty_authoring` (sfc.rs:1058) is
  untouched — it already delegates to `compile_treaty_file_with_map`, which sees the branched output.
- LEXER touch (minimal, only if a marker token is needed): the chosen `pipe('name')` marker is plain
  TS, so it lands in a `JavaScript` token with NO lexer change. Confirm by a lexer test
  `pipe_marker_call_stays_in_js` (lex a `.treaty`-shaped pipe source, assert the `pipe('x')` call is
  inside the single `JavaScript` token). Only if a DEDICATED region marker is later wanted would
  `LexerState`/`TokenKind` grow — NOT in this plan; keep it TS-only.
- Tests (sfc.rs tests mod), parse-based with the existing `assert_treaty_client_parses` (sfc.rs:1628):
  - `treaty_directive_emits_define_directive` — a `.treaty` with no template + a `host` unit compiles
    to `ɵɵdefineDirective`, no `ɵɵdefineComponent`, re-parses.
  - `treaty_pipe_emits_define_pipe` — a `.treaty` pipe compiles to `ɵɵdefinePipe` with the authored
    `name:` literal, re-parses.
  - `treaty_component_with_template_unchanged` — regression: an existing component `.treaty` fixture
    (reuse `compiles_treaty_file_template_and_interpolation`, sfc.rs:1234) emits `ɵɵdefineComponent`
    exactly as before.
- Invariant: a `.treaty` WITH a template still emits a component (no false directive routing); a
  template-less `.treaty` routes to directive/pipe per marker; emitted modules re-parse; server-fn
  privacy + map redaction (sfc.rs:1131) still apply because they live in the shared tail.

### C2. Fixture + source-validate row for .treaty directive/pipe

- Add ONE `.treaty` directive fixture and ONE `.treaty` pipe fixture under
  `examples/everything-app/src/**` and their PASSING rows in `source-validate.e2e.mjs`
  (`ɵɵdefineDirective` / `ɵɵdefinePipe`, no surviving decorator). Commit fixture + green gate together.
- Invariant: `source-validate` exits 0; the everything-app now demonstrates pipe+directive authoring
  in `.ts`, `.tsx`/`.tjsx`, AND `.treaty` (the user's requirement).

---

## Step count

- Part A (lexer hardening): 5 steps (A1–A5); A2+A3 land in one commit (shared `'@'` arm).
- Part B (JSX): 3 steps (B1 emit helpers, B2 discriminator+wiring, B3 fixtures+gate).
- Part C (.treaty): 2 steps (C1 discriminator+wiring, C2 fixtures+gate).
- Total: 10 steps across 3 parts, each green at the bar above.

## Biggest risk

A4 (`parse_html` trailing-text / single-root break) is the single highest-risk change: `parse_html`
is the most behavior-sensitive lexer function, its token spans feed BOTH `split_chunks` AND
`mask_non_js_regions`, and a too-greedy fix would swallow a following top-level `<style>`/`@if`
block or a sibling component tag into the HTML region — silently corrupting every interleaved
`.treaty` (e.g. `gauge.treaty`, which interleaves text, `@if`, and `<style>`). Mitigation: implement
A4 LAST in part A, keep the boundary set explicit and conservative (only absorb PLAIN trailing text up
to the next `<`/`{{`/`@`/`<style`/EOF), and gate it behind the full sfc.rs interleave suite
(`compiles_treaty_without_template_wrapper_interleaving_ts_and_html` sfc.rs:1376,
`no_template_wrapper_interleaves_ts_and_html` lexer.rs:897) plus a new gauge-shaped end-to-end test
BEFORE committing. Secondary risk: the B2/C1 directive/pipe DISCRIMINATOR convention — Treaty has no
JSX/`.treaty` decorators, so "is this a pipe vs a directive vs a component" must rest on an explicit,
documented authoring marker (recommended `pipe('name')`, and "no template" for directives); choosing
an implicit/heuristic marker would mis-route real components. Lock the convention in B2's doc-comment
and reuse it verbatim in C1 so the two front-ends never diverge.
