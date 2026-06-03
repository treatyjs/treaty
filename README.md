# Treaty

A Rust/OXC Angular compiler, packager, CLI and Node-compatible runtime. Treaty
compiles Angular **directly to Ivy** in Rust (no `tsc`, no `@angular/compiler` at
runtime), ships its own Node-compatible runtime, packages libraries (an
ng-packagr alternative), provides an `ng`-compatible native CLI, and lets you
author with `.treaty` SFCs, JSX-flavored Angular, or **plain React** — signals by
default.

> Branch: `migration/v22-oxc133` · OXC 0.133 · Angular **22.0.0-rc.3** (re-pin to `22.0.0` when it ships) · TypeScript 6.0.
> Detailed state: [`migration/STATUS.md`](migration/STATUS.md).

## Features

| Area | What | Status |
| --- | --- | --- |
| Compiler | `treaty_ivy` — direct-to-Ivy in Rust (4-crate carve: core/template/decorators/facade), `DecoratorCompiler` registry | ✅ **185/185 runnable Angular golden parity (100%)** |
| Dual parse backend (oxc / swc) | `ParseBackend` trait + engine-neutral IR: parse with **oxc (default/preferred)** or **swc**, gated byte-identical — parse-parity **1225/1225** over the real Angular corpus, emit-parity 29/29, matchGolden 185/185. A true **zero-oxc** swc variant (no oxc linked under `--features swc`) is the wide-port follow-up | ✅ swc parse byte-identical · 🏗️ zero-oxc wide port |
| All decorators → Ivy AOT | `@Component/@Directive/@Pipe/@Injectable/@NgModule/@Service` lower to Ivy `ɵɵdefine*` (no JIT) on every entry, including the unified `compile()` path | ✅ |
| Authoring → Ivy | `.treaty` SFC + JSX-flavored Angular authoring plugins → `treaty_ivy`; selectorless multi-form selector (kebab/camel/Pascal) + class name derived from the file name (no `<ng-component>`) | ✅ |
| **React → Angular** | A plain **React** `.tsx`/`.jsx` (imports from `react`, hooks) compiles to Angular Ivy: `useState`→`signal` (setter→`.set`/`.update`), `useEffect`→`effect`, `useMemo`→`computed`, `useCallback`/`useRef`/`useContext`, **props→`input()`**; signal reads auto-called in body, control-flow conditions, and handlers; inline-arrow handlers unwrapped; `react` import stripped | ✅ (`examples/treaty-shadcn` Card/Alert) |
| Modernizer (opt-in) | Compile-time lowering of legacy Angular to modern: `*ngIf/*ngFor/*ngSwitch`→`@if/@for/@switch`, `@Input`→`input()`, `@Output`→`output()`. Flag-gated; `changeDetection` stays the Angular default (`Default`) unless the signal modernizer opts into `OnPush` | ✅ opt-in |
| Decorators / DI | constructor DI (`ɵɵinject`/`ɵɵdirectiveInject` + `InjectFlags`), queries, `@Input({alias,transform})`, host bindings/styling, `signals: true` | ✅ |
| Consuming Angular libs (linker) | Built-in **Angular Linker**: partial `ɵɵngDeclare*` → AOT `ɵɵdefine*` in Rust. Links real `@angular/*` + CDK/Material to ZERO residual `ɵɵngDeclare` (no `@angular/compiler`); wired into vite/rolldown/rspack/rsbuild/rslib + the `@treaty/vite` plugin, dev + prod | ✅ link path (dev + prod) |
| Cross-module selectors | Additive `SelectorRegistry`: an **unmodified** Angular app whose components use real `@Component.selector` (e.g. `app-*`) resolves its children across modules; supplied by the native CLI + `@treaty/vite`/`rolldown`/`rspack` (class-name folding when absent, byte-identical) | ✅ |
| Nav / RouterLink | `RouterLink` + attribute-selector directives auto-imported into `dependencies[]` so navigation works | ✅ |
| Component style encapsulation | Emulated encapsulation via a ported `ShadowCss` (`_ngcontent-%COMP%` scoping) | ✅ |
| Library packager | **`treaty-packagr`** — oxc-powered **ng-packagr alternative**: SCSS/Sass (grass), `ShadowCss` scoping, FESM flatten, `.d.ts` barrel flatten, esbuild-faithful CSS optimizer, APF `package.json` + secondary entry points, **both** `compilationMode` (full AOT + partial `ɵɵngDeclare`). Byte-parity with **ng-packagr@21** on emitted Ivy, `.d.ts` ɵcmp, and FESM-scoped CSS; ~35× faster | ✅ |
| Native CLI (`treaty`) | **1:1 with `ng`**: reads `angular.json` (projects + architect targets + per-config overrides), runs `build`/`serve` through Treaty's native Rust module-graph build / **tokio+axum** dev server (HMR + toggleable dev source maps), and `generate`/`new`/`update`/`test`/`lint` spawn the **real `@angular-devkit` schematics + migrations** (genuinely 1:1). In-process `treaty_ivy` (no NAPI) | ✅ |
| File-based routing | `treaty_file_routing` crate + CLI; build-time `virtual:treaty-routes` module (no prebuilt `routes.ts`) across vite/rspack/rsbuild | ✅ (`examples/file-routed-app`) |
| Server functions | Inline by default (`server{}` / file-level `'use server'` / `$$` / `use websocket`); bodies extracted to the backend and stripped from client code **and** the source map; run in dev over `/__server/*`. Backend-agnostic (axum default, Elysia opt-in). **Production**: generates real axum Api (`Json<Req>`→`Json<Resp>`) + SSE (`Sse<Stream>`) + WebSocket handlers + a runnable host (`build_router()` + `main.rs`) — verified by a real `cargo build` against axum | ✅ dev + production hosting (Api/SSE/WS) |
| Runtime | Node-compatible Nova runtime, module-granular `node:` builtins, real `node:tls`/`node:https` over rustls (ring), offline-clean | ✅ 483 tests; conformance 51/0 + corpus 68/0 |
| SSG (static generation) | Rust-first `treaty_ssg` core: route discovery → static Ivy-template interpret → hydration-ready document + per-route hydration manifest + `sitemap.xml`/`robots.txt` (Nova prerender execution injected) | ✅ core (`libs/ssg-core`) |
| SSR (request-time render) | Live server render over the same interpreter | 🏗️ |
| Source maps | v3 maps (Ivy JS ↔ authoring source) threaded through addon + bundler plugins; server-fn bodies excluded | ✅ |
| Bundler plugins | `@treaty/vite`, `@treaty/rolldown`, `@treaty/rspack`, `@treaty/rsbuild`, `@treaty/rslib` — call the oxc compiler directly via NAPI (`compileComponent`, `compileComponentSource`, `compileTreatyFile`, `compile`, `compileMany`, `runMacro`, `linkPartial`, `build*SelectorRegistry`) | ✅ |
| Benchmark suite | `tools/treaty-bench` — compiler (Treaty-oxc / Treaty-swc vs Angular v21 + v22, with correctness/output-equality), build tools (vite/rolldown/rspack/rsbuild/rslib/native vs `ng`), packagr (vs ng-packagr), and CLI (vs `ng`); matched-optimization full-app **production** builds, every output e2e-booted (headless boot gate) | 🏗️ capstone (v21+v22 matrix, real swc column, caveat-free report) finalizing |
| Tooling | Typecheck with **tsgo** (TS native/Go — no `tsc` in the typecheck path), lint with **oxlint** | ✅ typecheck/lint |
| Build → boot e2e | Real vite build + boot of the example apps; a standing **boot gate** requires every supported build output to render (no JIT, full app), with a negative control | ✅ (e2e-gated) |

Legend: ✅ done · 🏗️ in progress · ⏳ queued.

## Examples

- [`examples/everything-app`](examples/everything-app) — broad feature coverage.
- [`examples/file-routed-app`](examples/file-routed-app) — file-routing engine demo.
- [`examples/treaty-shadcn`](examples/treaty-shadcn) — a publishable shadcn-style component library across ALL THREE surfaces (`.treaty`, Treaty `.tsx`, and **plain React `.tsx`**), packaged to `dist/` (typed `.d.ts` + APF `package.json`) via `treaty-packagr`.
- [`examples/treaty-shadcn-demo`](examples/treaty-shadcn-demo) — an Angular gallery app consuming the built `treaty-shadcn` library (proves JSX→Angular **and** React→Angular end to end).

## Status

See [`migration/STATUS.md`](migration/STATUS.md) for the full per-workstream
breakdown (compiler parity, the Angular linker, runtime conformance, file
routing, examples, tooling) and the prioritized roadmap.
