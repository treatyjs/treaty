# Treaty Rust/OXC Compiler — Migration Status

Branch: `migration/v22-oxc133`. Updated 2026-05-30.

## Done & green
- **Phase 1a** — legacy `apps/rust/authoring` migrated OXC `0.29 → 0.133` (edition 2024); the
  previously-orphaned `angular/` module wired in via `lib.rs`; `cargo build`/`test` green.
  (Root blocker was dead `swc_*` deps, removed.)
- **Phase 1b** — Nx removed (moon is the task runner); **Angular `18 → 21.2.15`** across all
  `package.json` (22 not yet on npm stable; fast-follow). `bun install` clean (2042 pkgs).
  `@angular/compiler` 18→21 API edits applied to `treat-to-ivy.ts`/`compiler.ts` (typecheck clean).
  vite `7.3.3`, TS `5.9`, zone.js `0.16`.
- **render3 port — COMPLETE & GREEN: all 16 modules, ~27k lines Rust, 214 tests pass.**
  A from-scratch port of Angular 22.1's `packages/compiler` `render3` into `libs/render3`:
  - `output_ast` (owned IR) + `output/emitter` (lowers IR → `oxc_ast` → `oxc_codegen` text)
  - `expression/{lexer,ast,parser}` (binding expressions)
  - `ml_parser` (HTML tokenizer + AST + tree parser, incl. `@`-blocks/ICU)
  - `template/r3_ast` (template IR) + `template/{template_transform,control_flow,deferred}`
  - `binder` (t2_binder — **selectorless + auto-import** resolution)
  - `identifiers` (full `ɵɵ*` table), `factory` (`ɵfac`), `pipe_module_injector` (`ɵpipe`/`ɵmod`/`ɵinj`)
  - `view/{template,compiler,queries}` — `compileComponentFromMetadata` → `ɵɵdefineComponent`

## End-to-end + fidelity — DONE & GREEN (242 tests)
- `compile.rs`: `compile_component(html, selector, class) → ɵɵdefineComponent` emits **runnable Ivy**
  with `import * as i0 from "@angular/core"` + `i0.ɵɵ…` aliasing (emitter `ImportManager`).
- `expression_converter.rs`: binding AST → `output_ast` (PropertyRead/Binary/Conditional/Call/
  Keyed/Literal/array/map/unary, safe-nav, action chains).
- `view/template` TDB now emits real **`ɵɵproperty`** (bound inputs), **`ɵɵlistener`** (bound events),
  correct multi-part interpolation arity (`ɵɵtextInterpolate`/`…1..8`/`…V`), and a real **`vars`** count.
- `view/compiler`: real **query generation** wired (`ɵɵviewQuery`/`ɵɵcontentQuery`/`ɵɵqueryRefresh`).
- **NAPI** (`libs/authoring/node`) exposes `compile_component` to JS.
- **Oracle parity harness**: `libs/render3/parity/parity.mjs` (+README) diffs Rust output vs
  `@angular/compiler`.
- 17-case **robustness sweep** (nested/bindings/control-flow/refs): no panics.

## Oracle parity progress (vs @angular/compiler v21.2.15, honest un-normalized diff)
- **17 / 23 fixtures BYTE-EXACT.** PASS: static/interpolation/nested/attribute/multiple-bindings,
  property-binding, event-binding, class-binding, style-binding, two-interpolations, deep-nesting,
  sibling-elements, static-and-bound-mix, multiple-event-bindings, control-flow-if, control-flow-for,
  for-index-count. Run: `node libs/render3/parity/parity.mjs` (rebuild addon: `cargo build -p authoring_node --release` → copy dll to the `.node`).
- 6 remaining DIFFs (all `view/template.rs`): @if-else-if/@switch branch chaining onto one
  `ɵɵconditionalCreate`; nested-view fn naming; mixed prop/class emission+consts order;
  `<ng-template #ref>` (`ɵɵtemplateRefExtractor`); `[attr.x]` → `ɵɵattribute` (not domProperty).

## Remaining NOTE(port) seams (follow-up)
- Control-flow **instruction** emission (`@if`/`@for`/`@defer` → `ɵɵconditional`/`ɵɵrepeater`/defer) —
  parsed into r3_ast but not yet lowered to instructions.
- Pipes: `BindingPipe` → `__pipe_*` placeholder (needs `ɵɵpipeBind`/pure-function slot allocation).
- Safe-navigation uses optional chaining instead of Angular's temporary-guard expansion.
- Host bindings, ShadowCss style encapsulation, i18n (`LocalizedString`) deferred;
  `parse_util` spans are minimal placeholders.

## Next
1. Finish end-to-end `compile_component` (background agent).
2. Wire NAPI (`libs/authoring/node`) to expose the Rust compiler to JS.
3. Parity-diff vs `@angular/compiler` oracle on a `.treaty`/component fixture corpus.
4. Phase 1c gate: REPL `vite build` + renders a `.treaty` on the Rust backend.
5. When Angular 22 ships stable: re-bump deps; re-sync `tools/angular-ref`.
