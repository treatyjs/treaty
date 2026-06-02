# Treaty

A Rust/OXC Angular compiler and Node-compatible runtime. Treaty compiles
Angular **directly to Ivy** in Rust (no `tsc`, no `@angular/compiler` at runtime),
ships its own Node-compatible runtime, and lets you author with `.treaty` SFCs or
JSX-flavored Angular — signals by default.

> Branch: `migration/v22-oxc133` · OXC 0.133 · Angular **22.0.0-rc.3** (re-pin to `22.0.0` when it ships) · TypeScript 6.0.
> Detailed state: [`migration/STATUS.md`](migration/STATUS.md).

## Features

| Area | What | Status |
| --- | --- | --- |
| Compiler | `treaty_ivy` — direct-to-Ivy in Rust (4-crate carve: core/template/decorators/facade), `DecoratorCompiler` registry | ✅ 181/185 runnable Angular golden parity (97.8%) |
| All decorators → Ivy AOT | `@Component/@Directive/@Pipe/@Injectable/@NgModule` lower to Ivy `ɵɵdefine*` (no JIT) on every entry, including the unified `compile()` path | ✅ |
| Authoring → Ivy | `.treaty` SFC + JSX-flavored Angular authoring plugins → `treaty_ivy`; selectorless multi-form selector (kebab/camel/Pascal) + class name derived from the file name (no `<ng-component>`) | ✅ |
| **React → Angular** | A plain **React** `.tsx`/`.jsx` (imports from `react`, hooks) compiles to Angular Ivy: `useState`→`signal` (setter→`.set`/`.update`), `useEffect`→`effect`, `useMemo`→`computed`, `useCallback`/`useRef`/`useContext`, **props→`input()`**; signal reads auto-called in body, control-flow conditions, and handlers; inline-arrow handlers unwrapped; `react` import stripped | ✅ (`examples/treaty-shadcn` Card/Alert) |
| Decorators / DI | constructor DI (`ɵɵinject`/`ɵɵdirectiveInject` + `InjectFlags`), queries, `@Input({alias,transform})`, host bindings/styling, `signals: true` | ✅ |
| Consuming Angular libs (linker) | Built-in **Angular Linker**: partial `ɵɵngDeclare*` → AOT `ɵɵdefine*` in Rust. Links real `@angular/*` + CDK/Material to ZERO residual `ɵɵngDeclare` (no `@angular/compiler`); wired into vite/rspack/rsbuild/rslib + the `@treaty/vite` plugin, dev + prod | ✅ link path (dev + prod) |
| Nav / RouterLink | `RouterLink` + attribute-selector directives auto-imported into `dependencies[]` so navigation works | ✅ |
| Component style encapsulation | Emulated encapsulation via a ported `ShadowCss` (`_ngcontent-%COMP%` scoping) | ✅ |
| File-based routing | `treaty_file_routing` crate + CLI; build-time `virtual:treaty-routes` module (no prebuilt `routes.ts`) across vite/rspack/rsbuild | ✅ (`examples/file-routed-app`) |
| Server functions | Inline by default (`server{}` / file-level `'use server'` / `$$` / `use websocket`); bodies extracted to the backend and stripped from client code **and** the source map; run in dev over `/__server/*`. Backend-agnostic (axum default, Elysia opt-in). **Production**: generates real axum Api (`Json<Req>`→`Json<Resp>`) + SSE (`Sse<Stream>`) + WebSocket handlers + a runnable host (`build_router()` + `main.rs`) — verified by a real `cargo build` against axum | ✅ dev + production hosting (Api/SSE/WS) |
| Runtime | Node-compatible Nova runtime, module-granular `node:` builtins, real `node:tls`/`node:https` over rustls (ring), offline-clean | ✅ 483 tests; conformance 51/0 + corpus 68/0 |
| SSG (static generation) | Rust-first `treaty_ssg` core: route discovery → static Ivy-template interpret → hydration-ready document + per-route hydration manifest + `sitemap.xml`/`robots.txt` (Nova prerender execution injected) | ✅ core (`libs/ssg-core`) |
| SSR (request-time render) | Live server render over the same interpreter | 🏗️ |
| Source maps | v3 maps (Ivy JS ↔ authoring source) threaded through addon + bundler plugins; server-fn bodies excluded | ✅ |
| Bundler / NAPI | `compileComponent`, `compileComponentSource`, `compileTreatyFile`, `compile`, `compileMany`, `runMacro`, `linkPartial` | ✅ |
| Tooling | Typecheck with **tsgo** (TS native/Go — no `tsc` in the typecheck path), lint with **oxlint**. Library JS/`.d.ts` *emit* still goes through `tsc` in the Moon `build` tasks (removing `tsc` from emit too is a tracked goal) | ✅ typecheck/lint |
| Build → boot e2e | Real vite build + boot of `everything-app` / `file-routed-app`; a source-validate gate parses every authoring source | ✅ (e2e-gated) |

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
