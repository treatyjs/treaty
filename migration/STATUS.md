# Treaty Rust/OXC Compiler — Migration Status

Branch: `master`. Updated 2026-05-31.

Honest headline: this is a from-scratch Rust/OXC port of Angular's `render3` compiler plus a
Node-compatible runtime and tooling. It is NOT at 100% Angular compliance — see the compliance
numbers below and `migration/COMPILE-ANY-ANGULAR-PLAN.md` for the path to "compile any Angular app".

## Compiler — render3 crate (Rust/OXC)

- **render3 port COMPLETE & GREEN: 363 `#[test]` across 22 modules** (`cargo test -p render3`).
  A from-scratch port of Angular 22.x `packages/compiler` `render3` into `libs/render3`:
  - `output_ast` (owned IR) + `output/emitter` (lowers IR → `oxc_ast` → `oxc_codegen` text)
  - `expression/{lexer,ast,parser}` (binding expressions)
  - `ml_parser` (HTML tokenizer + AST + tree parser, incl. `@`-blocks/ICU)
  - `template/r3_ast` + `template/{template_transform,control_flow,deferred}`
  - `binder` (t2_binder — selectorless + auto-import resolution)
  - `identifiers` (full `ɵɵ*` table), `factory` (`ɵfac`),
    `pipe_module_injector` (`ɵpipe`/`ɵmod`/`ɵinj` — `compile_pipe_from_metadata`/`compile_ng_module`/`compile_injector`)
  - `view/{template,compiler,queries}` — `compile_component_from_metadata`/`compile_directive_from_metadata`
    → `ɵɵdefineComponent`/`ɵɵdefineDirective`, full host-binding builder + view/content queries
  - `source_compile.rs` — the `@Component`/`@Directive` SOURCE front-end (`compile_component_source`)
  - `i18n.rs` — i18n lowering (in flight; see DIFFs below)

## Oracle parity (vs `@angular/compiler` v21.2.15, honest un-normalized diff)

- **27 / 27 non-i18n fixtures BYTE-EXACT** via `libs/render3/parity/parity.mjs`.
  (Rebuild addon before running: `cargo build -p authoring_node --release` → copy dll to the `.node`.)

## Angular compliance harness (vs Angular's own compiler-cli corpus)

`libs/render3/compliance/run-compliance.mjs` runs the Rust front-end (via the `authoring_node` NAPI
addon) against Angular's vendored compliance corpus at
`tools/angular-ref/packages/compiler-cli/test/compliance/test_cases/`. Per committed
`COMPLIANCE-REPORT.md` (2026-05-31):

| Metric | Value |
| --- | --- |
| Total cases | 642 |
| Runnable | 98 |
| PASS (matchGolden) | 92 |
| DIFF | 6 |
| Skipped | 544 |
| **Pass-rate (runnable subset)** | **93.9% (92/98)** |
| Pass-rate (full corpus) | 14.3% (92/642) |

- 6 runnable DIFFs: **4 i18n** (`@let`-in-i18n `ɵɵadvance`/`ɵɵi18nExp`; i18n `@if`/`@switch`/`@defer`)
  + **2 spread** (`ɵɵpureFunction1` slot alloc for array/object-literal spread).
- 544 skips: 477 `no-full-golden(partial/ngDeclare-only)`, 41 `fe:multi-class`, 17 `multi-input-file`,
  4 `no-input-file`, 3 `fe:host/hostDirectives`, 2 `fe:queries`. Most skips are the front-end DECLINING
  a metadata shape or the harness lacking a full golden — NOT missing emitters. See
  `migration/COMPILE-ANY-ANGULAR-PLAN.md`.
- NOTE: a sibling workflow is actively editing `libs/render3` + `run-compliance.mjs` + rebuilding the
  addon, so these numbers are the last committed snapshot and may move.

## Runtime — Node-compatible (DONE & GREEN)

- Node-compatible runtime: **358 tests** green, plus a **Node conformance harness** validating
  behaviour against Node's own semantics.

## Build / output features (DONE)

- **All-format source maps** across the emit pipeline (additive span stamping in the emitter).
- **Function chunking** for emitted output.
- **Module Federation**: zero-config **toggle** + **eject** to plain Angular CLI config.
- **Rust file-based routing + SSG core**.
- **NAPI**: `libs/authoring/node` exposes `compile_component` / `compile_component_source` to JS.

## Tooling (DONE)

- Typecheck with **tsgo** (TS native/Go), lint with **oxlint** — NO `tsc` anywhere.
- No-AI maintenance harnesses:
  - **render3-sync** — keeps the Rust render3 1:1 with Angular (drift diff + conformance gate +
    mechanical OXC TS→Rust codegen).
  - **dep-updater** — self-updating deps + breaking-change codemods for this repo.
  - **ngx-maintenance** — GitHub bot to auto-migrate Angular libs v9→latest (VE→Ivy).

## Next

1. Front-end coverage expansion per `migration/COMPILE-ANY-ANGULAR-PLAN.md`:
   Rank 5 harness golden-widening, then multi-class (Rank 1), host bindings (Rank 2),
   `@Directive` emission (Rank 4), decorator queries (Rank 3).
2. Close the 6 runnable DIFFs: 4 i18n (in flight) + 2 spread (`ɵɵpureFunction1`).
3. Add a `--compile-only` harness mode to track the 477 partial/ngDeclare-only cases under bar (2)
   (compile-without-error), separate from matchGolden pass-rate.
4. When Angular 22 ships stable on npm: re-bump deps; re-sync `tools/angular-ref`.
