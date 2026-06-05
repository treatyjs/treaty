# Plan: make the Treaty Rust front-end compile ANY Angular code

Static (read-only) analysis of `libs/render3/src/source_compile.rs`, the emitters under
`libs/render3/src/view/` + `pipe_module_injector.rs`, and the harness
`libs/render3/compliance/run-compliance.mjs`, cross-checked against the real corpus at
`tools/angular-ref/packages/compiler-cli/test/compliance/test_cases/` and the committed
`libs/render3/compliance/COMPLIANCE-REPORT.md`.

## What "compile ANY Angular code" actually means

Be honest about the goal. There are TWO distinct success bars, and they are not the same:

1. **matchGolden PASS** — the front-end accepts the metadata shape, emits an Ivy
   `ɵɵdefineComponent`/`ɵɵdefineDirective`/… block, AND that block matches Angular's golden
   ellipsis-fragments in order. This is what the compliance pass-rate measures.
2. **compile-without-error (the real "compile any app" bar)** — the front-end accepts the shape
   and emits *valid, runnable* Ivy. This is BROADER than matchGolden PASS, because most of the
   corpus has no full `ɵɵdefineComponent` golden to diff against (partial / `ngDeclare`-only), so
   those cases can never produce a matchGolden PASS no matter how correct our output is.

"Compile any Angular app" = bar (2) for every shape the language allows. The compliance pass-rate
(bar 1) is the *measurable proxy* for the subset that has a full golden. This plan tracks both, and
is explicit about which lever moves which bar.

## Current state (verified against committed report + source)

- Corpus: **642** compliance cases.
- Runnable through the source front-end today: **98**.
- **92 PASS**, **6 DIFF** (matchGolden) → **93.9% of the runnable subset**, 14.3% of the full corpus.
- **544 SKIPPED** before/at the front-end.
- render3 crate: **363 `#[test]`** across 22 modules (`cargo test -p render3`), all green at last run.
- Oracle parity (`libs/render3/parity/parity.mjs` vs `@angular/compiler` v21.2.15): **27/27 non-i18n**
  fixtures byte-exact.

The 6 runnable DIFFs (from `COMPLIANCE-REPORT.md`, "Ranked gap categories"):

| Count | First-missing instruction | Cases |
| --- | --- | --- |
| 2 | `ɵɵpureFunction1` | spread elements in array literals; object literals with spread assignments (the 2 **spread** DIFFs) |
| 1 | `ɵɵadvance` | `@let` referenced inside i18n + in a child view |
| 1 | `ɵɵconditionalCreate` | i18n `@if` blocks |
| 1 | `ɵɵi18nEnd` | i18n `@switch` blocks |
| 1 | `ɵɵelementStart` | i18n `@defer` blocks |

So the runnable DIFF closeout = **4 i18n + 2 spread**. The 4 i18n DIFFs are the i18n work in flight
(localized-string / `ɵɵi18n*` slot+expression ordering inside control-flow); the 2 spread DIFFs are
`ɵɵpureFunction1` slot allocation for array/object literals carrying spread elements.

## Executive finding (unchanged — re-verified against current source)

The render3 EMITTER is already nearly complete. `view/compiler.rs` exposes
`compile_directive_from_metadata`, `compile_component_from_metadata`, `DefaultHostBindingsBuilder`
(full `host` property/listener/class/style/attr lowering + `hostAttrs`/`hostVars`),
`parse_host_bindings`, `create_host_directives_feature_arg`/`HostDirectivesFeature`, and full
view/content query support (`view/queries.rs`, proven green by the passing `signal_queries` case).
`pipe_module_injector.rs` exposes `compile_pipe_from_metadata`, `compile_ng_module`,
`compile_injector`.

Almost every SKIP is the FRONT-END (`source_compile.rs`) DECLINING a metadata shape, or the HARNESS
declining a golden — NOT a missing emitter. **Ranks 1-4 below need ZERO new emitter code**: only new
extractor/wiring in `source_compile.rs`, reusing the emitters that already exist.

### Skip breakdown (from committed report)

| Count | Skip reason | Real cause | Where decided |
| --- | --- | --- | --- |
| 477 | `no-full-golden(partial/ngDeclare-only)` | HARNESS golden selection — most ALSO compile fine, they just have no full golden to diff | `run-compliance.mjs` `pickFullGolden` |
| 41 | `fe:multi-class` | front-end refuses >1 decorated class | `source_compile.rs:791-796` |
| 17 | `multi-input-file` | HARNESS: case has >1 input `.ts` | `run-compliance.mjs` input gate |
| 4 | `no-input-file` | HARNESS: case has 0 input `.ts` | `run-compliance.mjs` input gate |
| 3 | `fe:host/hostDirectives` | front-end refuses `host:`/`hostDirectives:` keys | `source_compile.rs:368-374, 811-816` |
| 2 | `fe:queries` | front-end refuses `@ViewChild`/`@ViewChildren` member decorators | `source_compile.rs:377-384, 415-419` |

Of the 642, only ~**144 are "runnable candidates"** (exactly 1 input file AND a referenced full
`defineComponent` golden). 98 compile today; the remaining ~46 runnable-candidate skips are blocked
purely by the three `fe:*` declines (41 + 3 + 2). The other 498 are blocked BEFORE the front-end ever
runs, so front-end work alone cannot raise their matchGolden score — but the front-end CAN make them
compile-without-error (bar 2), and a harness golden-widening can convert the on-disk-full-golden slice
of them into matchGolden candidates.

## The decline sites in `source_compile.rs` (exact, re-verified)

1. **Multi-class** — `compile_program`, **L791-796**:
   ```rust
   if decorated.len() > 1 {
       return err(format!("multi-class files unsupported: found {} decorated classes", decorated.len()));
   }
   ```
   `decorated` is built collecting every class carrying `@Component`/`@Directive` (NgModule/Pipe
   classes are not even collected). Declines the moment a 2nd `@Component` OR `@Directive` is present.

2. **host / hostDirectives / queries / providers / viewProviders** — `UNSUPPORTED_DECORATOR_KEYS`
   at **L368-374**, enforced at **L811-816**:
   ```rust
   const UNSUPPORTED_DECORATOR_KEYS: &[&str] = &["providers","viewProviders","host","hostDirectives","queries"];
   ```

3. **Member decorators** (`@ViewChild`/`@ViewChildren`/`@ContentChild`/`@ContentChildren`/
   `@HostBinding`/`@HostListener`) — `UNSUPPORTED_PROPERTY_DECORATORS` at **L377-384**, enforced in
   `collect_io` at **L415-419**.

4. **`@Directive` emission** — `compile_program` **L944-947** returns
   `"@Directive emission not yet supported (only @Component)"` even though
   `compile_directive_from_metadata` exists and works.

5. **templateUrl** — **L817-819** (corpus `fe:templateUrl`; needs a virtual-fs/template resolver;
   out of scope for the runnable-candidate slice).

## Ranked capability list (cases-unlocked-per-effort)

Estimates are upper bounds within the runnable-candidate set unless a harness change is also noted.

### Rank 1 — Multi-class file: emit ALL decorated classes  (unlocks ~41; 21 immediately)
- **Decline site:** `source_compile.rs:791-796`.
- **Corpus shape (verified):** of 41 multi-class runnable candidates, 21 have exactly ONE `@Component`
  (siblings are `@Directive`/`@NgModule`/`@Pipe`); 20 have ≥2 `@Component`. Histogram:
  `comp=1,dir=1,mod=1`(11), `comp=2,mod=1`(9), `comp=2`(7), `comp=1,dir=1`(5), `comp=2,dir=1,mod=1`(3),
  `comp=1,dir=2`(2), `comp=1,dir=3`(2), `comp=1,dir=3,mod=1`(1), `comp=3,mod=1`(1).
- **What to implement (front-end, ZERO new emitter):** replace the hard `len()>1` error with: collect
  ALL decorated classes (`@Component`, `@Directive`, `@Pipe`, `@NgModule`), compile EACH to its own
  definition via the existing `compile_component_from_metadata` / `compile_directive_from_metadata` /
  `compile_pipe_from_metadata` / `compile_ng_module`, and concatenate the emitted statements in source
  order (exactly what ngtsc does — one file → many defs). Cross-class auto-import (a
  `<my-forward-directive>` used in a component template, declared later in the SAME file) reuses the
  existing `collect_imported_names` + selectorless resolver, but must ALSO seed candidates from sibling
  decorated class names + their selectors.
- **Bars moved:** matchGolden for the 21 single-`@Component` files (harness's
  `extractDefineComponentBlock` takes the FIRST `defineComponent`); compile-without-error for the rest.
- **Lowest-risk first slice:** the 21 single-component files.

### Rank 2 — `host` bindings metadata  (unlocks 3 runnable now; prerequisite for ~20 `host_bindings`)
- **Decline site:** `source_compile.rs:368-374` (`"host"`) + `L811-816`; member side `L415-419`.
- **What to implement (front-end, ZERO new emitter):** parse the `host: { '[prop]':'expr',
  '(event)':'handler', 'class':'…', 'style':'…', 'attr.x':'…' }` object literal into the input shape
  for the EXISTING `view::compiler::parse_host_bindings` → `ParsedHostBindings`, populate
  `R3HostMetadata` on `base`, and swap `StubHostBindingsBuilder` for the EXISTING
  `DefaultHostBindingsBuilder` in `compile_component_meta`. Also parse `@HostBinding('x')` /
  `@HostListener('evt',[...])` member decorators (remove them from `UNSUPPORTED_PROPERTY_DECORATORS`)
  into the same `R3HostMetadata`.
- **Bars moved:** matchGolden for the 3 runnable host candidates now; once a harness golden-widening
  (Rank 5) exposes the ~20 `host_bindings` on-disk-full-golden cases, this capability is what makes
  them PASS.

### Rank 3 — Decorator-based queries  (unlocks 2 runnable)
- **Decline site:** `source_compile.rs:377-384, 415-419`. These cases are ALSO multi-class
  (component + `@NgModule`), so **Rank 1 is a co-requisite**.
- **What to implement (front-end, ZERO new emitter):** parse `@ViewChild(locator, opts)` /
  `@ViewChildren` / `@ContentChild` / `@ContentChildren` into the EXISTING `R3QueryMetadata` (the same
  struct `parse_signal_query` builds, with `is_signal:false`, and `static`/`read`/`descendants` from
  the options object). Route content-vs-view exactly like `collect_signal_queries`. Emitter
  (`view/queries.rs`) already proven by the passing `signal_queries` case.

### Rank 4 — `@Directive`-only emission  (unlocks the 28 directive-only no-full-golden + feeds Rank 1)
- **Decline site:** `source_compile.rs:944-947`.
- **What to implement (front-end, ZERO new emitter):** in the `TopLevel::Directive` arm, build
  `R3DirectiveMetadata` (selector, inputs/outputs, host, queries — same extractors as the component
  path, minus template) and call the EXISTING `compile_directive_from_metadata`, emitting
  `ɵɵdefineDirective`. Directly serves the directive siblings produced by Rank 1.
- **Bars moved:** compile-without-error for 28 directive-only cases now; matchGolden once Rank 5 lets
  the harness accept a `defineDirective` golden.

### Rank 5 — HARNESS: golden selection widening  (unlocks ~102 matchGolden + ~45 more; not a front-end change)
- **Decline site:** `run-compliance.mjs` `pickFullGolden` — only considers goldens REFERENCED in the
  case's `expectations.files`, and only those containing `ɵɵdefineComponent`.
- **Verified facts:** **102** of the 477 `no-full-golden` cases HAVE a full `ɵɵdefineComponent` golden
  sitting ON DISK in the case dir (e.g. `host_bindings.js` beside the referenced `*_partial.js`) — top
  dirs: 20 `r3_view_compiler_listener`, 20 `host_bindings`, 16 `r3_view_compiler_deferred`,
  7 `elements`, 6 `i18n/nested_nodes`, 5 `style_bindings`, … A further ~**45** have a
  `defineDirective`/`defineNgModule`/`definePipe` full golden on disk (28 directive-only,
  13 ngmodule/injector-only, 4 pipe-only).
- **What to implement (harness only — owned by the sibling workflow, NOT this analysis):** widen
  `pickFullGolden` to also scan unreferenced `.js` in the case dir for a full
  `ɵɵdefineComponent`/`ɵɵdefineDirective`/`ɵɵdefinePipe` block. Highest single-jump, compiler-risk-free.
  (Documented here for ranking; the compiler changes in Ranks 1-4 are what compile the inputs that
  these newly-selected goldens are then diffed against.)

### Rank 6 — multi-input files  (17 cases, highest effort, schedule last)
- **Decline site:** `run-compliance.mjs` input gate (`inputFiles.length !== 1`).
- **Corpus:** 7 `queries`, 6 `deferred`, 3 `template_variables`, 1 misc — component split across files.
  Requires the harness to resolve/concatenate multiple inputs, or the front-end to accept a multi-file
  program. Higher effort, lower count.

## Runnable-DIFF closeout (orthogonal to the rank list; raises the 93.9% directly)

- **4 i18n DIFFs (in flight):** `@let`-in-i18n `ɵɵadvance`/`ɵɵi18nExp` ordering; i18n `@if`
  (`ɵɵconditionalCreate`), `@switch` (`ɵɵi18nEnd`), `@defer` (`ɵɵelementStart`) inside i18n blocks.
  This is the `i18n.rs` / control-flow-instruction lowering work already underway.
- **2 spread DIFFs:** `ɵɵpureFunction1` slot allocation for array-literal spread elements and
  object-literal spread assignments (`expression_converter.rs` pure-function slot path).

Closing all 6 takes the runnable subset from 92/98 toward 98/98 without unlocking any new cases.

## The 477 partial/ngDeclare-only cases (the "compile any app" long tail)

These have NO full `ɵɵdefineComponent` golden, so **matchGolden PASS is structurally impossible** for
them. The goal here is bar (2): **compile-without-error and emit valid Ivy**. Track this SEPARATELY
from the compliance pass-rate, e.g. a `--compile-only` harness mode that asserts the front-end returns
`errors.is_empty()` and the emit parses. Ranks 1-4 (multi-class, host, queries, `@Directive`) plus a
`templateUrl`/virtual-fs resolver are what move this needle; ~102 + ~45 of them ALSO have a full golden
on disk and become matchGolden candidates via Rank 5.

## Suggested order

1. **Rank 5** (harness golden widening) — biggest single matchGolden jump (~102), zero compiler risk.
2. **Rank 1** (multi-class, emit all) — 21 immediate matchGolden, sets up Ranks 3/4 siblings.
3. **Rank 2** (host bindings) — 3 runnable now + ~20 `host_bindings` once Rank 5 lands.
4. **Rank 4** (`@Directive` emission) — 28 directive-only goldens + multi-class directive siblings.
5. **Rank 3** (decorator queries) — 2 cases, co-req Rank 1.
6. **DIFF closeout** (4 i18n in flight + 2 spread) — raises 93.9% toward 100% of the runnable subset.
7. **Rank 6** (multi-input) — 17 cases, highest effort.

All compiler changes are confined to `libs/render3/src/source_compile.rs` and reuse the existing
`view/compiler.rs` + `pipe_module_injector.rs` + `view/queries.rs` emitters. **No new emitter code is
required for Ranks 1-4.** The harness change (Rank 5) and the multi-input change (Rank 6) are owned by
the sibling workflow that edits `run-compliance.mjs`.
