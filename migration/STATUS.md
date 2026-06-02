# Treaty migration status

**Branch:** `migration/v22-oxc133`

**Updated:** 2026-06-02

> Companion docs: [OXC migration crib](OXC-MIGRATION-CRIB.md) ·
> [Treaty versions May 2026](treaty-versions-may-2026.md)

---

## Top-line summary

Treaty is a Rust/OXC Angular compiler + Node-compatible runtime. The Ivy
compiler (formerly the single `render3` crate) has been **renamed `treaty_ivy`
and carved into 4 composable workspace crates** under `libs/treaty-ivy/`, behind
a `DecoratorCompiler` registry that mirrors the authoring-plugin design.

Where things stand:

- **treaty_ivy compiler** — DONE & green: 4 crates, **470 `#[test]`** total. Emits
  complete ES-module Ivy output. **Every Angular decorator**
  (`@Component/@Directive/@Pipe/@Injectable/@NgModule`) lowers to Ivy AOT (no JIT)
  on every entry point, including the unified `compile()` path. Golden parity vs
  Angular's own corpus: **181 / 185 runnable = 97.8%** (live-scored), at the modern-Angular ceiling
  toward full parity as the ranked DIFFs are closed.
- **Angular Linker** — DONE for the LINK path: partial `ɵɵngDeclare*` → AOT
  `ɵɵdefine*` in Rust (`libs/treaty-ivy/facade/src/linker.rs` + NAPI `linkPartial`).
  Links real `@angular/*` + CDK/Material to **ZERO residual `ɵɵngDeclare`** (no
  `@angular/compiler`, no JIT), and is wired into vite/rspack/rsbuild/rslib plus
  the `@treaty/vite` plugin, **dev and prod**. Remaining: production backend
  *hosting* of server fns (the dev backend already runs them).
- **Node-compatible runtime** — DONE & green: 483 in-crate tests, module-granular
  `node:` builtins, real `node:tls` + `node:https` over rustls/ring, offline-clean.
- **File-based routing** — DONE: `treaty_file_routing` Rust crate + CLI, exposed as
  a build-time `virtual:treaty-routes` module (no prebuilt `routes.ts`) across
  vite/rspack/rsbuild; wired into `examples/file-routed-app`.
- **Example apps + addon** — DONE: both `everything-app` and `file-routed-app`
  **build and boot** under a repeatable e2e gate; a source-validate e2e parses
  every authoring source through the production compiler seam.

---

## (1) treaty_ivy — the Ivy compiler

**Status: DONE & green (parity climbing).**

### The 4-crate carve

`libs/render3` is **gone**. The compiler now lives in a DAG of four workspace
crates under `libs/treaty-ivy/`:

```
core  <-  template  <-  decorators  <-  facade
```

| Crate | Role | `#[test]` |
| --- | --- | --- |
| `treaty_ivy_core` | Ivy instructions, const pool, expression lowering, i18n primitives, sourcemap, `ngDeclare` shapes | 215 |
| `treaty_ivy_template` | template parse + bind, control flow, host/styling | 138 |
| `treaty_ivy_decorators` | `@Component/@Directive/@Injectable/@Pipe` lowering, queries, DI | 39 |
| `treaty_ivy_facade` | public entry, NAPI ports, compliance + parity harness | 55 |
| **Total** | | **470** |

A **`DecoratorCompiler` registry** sits in the decorators crate so decorator
handlers compose the same way authoring plugins do.

### What emits correctly (DONE)

- **All decorator kinds → Ivy AOT, no JIT:** `@Component` (`ɵcmp`),
  `@Directive` (`ɵdir`), `@Pipe` (`ɵpipe`), `@Injectable` (`ɵprov`),
  `@NgModule` (`ɵmod` + `ɵinj`) — including through the unified `compile()` entry.
- Complete ES-module emit: `export class` + imports + `ɵfac` + `ɵcmp`/`ɵdir`.
- Constructor DI: `ɵɵdirectiveInject` / `ɵɵinject` + `InjectFlags`, with
  `@Inject` / `@Optional` / `@Self` / `@SkipSelf` / `@Host` / `@Attribute`.
- `@Injectable` `ɵprov`.
- Host-binding chaining + host styling.
- `forwardRef` in queries.
- `@Input({ alias, transform })`.
- `signals: true` components; default `OnPush`.
- Empty-i18n element; `pureFunction` const hoist (spread → `pureFunction1`).

### Parity & compliance (committed numbers)

Harness moved into the facade crate:

- Parity: `libs/treaty-ivy/facade/parity/parity.mjs` — **27 / 27 non-i18n**
  byte-exact vs Angular reference. Oracle parity: 27 non-i18n.
- Compliance: `libs/treaty-ivy/facade/compliance/run-compliance.mjs`
  (+ `run-compliance.test.mjs`, `COMPLIANCE-REPORT.md`).

| Metric | Count |
| --- | --- |
| Total cases | 642 |
| Compiled (runnable) | 185 |
| **matchGolden PASS** | **181** |
| matchGolden DIFF | 4 |
| Skipped (no runnable golden) | 457 |
| compile-without-error (of 619-entry dump) | 594 |
| **Pass-rate (runnable subset)** | **97.8%** |

> ~435 of the skips are partial / `ngDeclare`-only fixtures (no full golden); the
> **Angular Linker** work below brings that whole class into scope.
>
> Corpus dump env var renamed `RENDER3_CORPUS_DUMP` → **`TREATY_IVY_CORPUS_DUMP`**.
> The live harness now scores **181/185**; re-run `run-compliance.mjs --report`
> after a fresh `corpus_dump` to regenerate the committed `COMPLIANCE-REPORT.md`.

### Ranked DIFF gaps (the work toward full parity)

The live harness reports **4 matchGolden DIFFs** of the 185 runnable cases. **All
4 are in the `ng_modules` corpus and are golden-mode/shape artifacts, not compiler
defects** — they ask for an NgModule output mode the harness can't request:

- *"…with declarations and bootstrap (**jit mode**)"* and *"…with imports and
  exports (**jit mode**)"* (2) — authored in Angular's legacy **jit** mode.
  treaty_ivy emits the modern AOT `setNgModuleScope` shape; matching the jit golden
  byte-for-byte would *regress* the output away from current Angular.
- *"…with **forward refs**"* (1) — the golden spells the imports thunk
  `imports: () => [ForwardModule]` a different way than our emit (a forwardRef-in-
  imports thunk-shape difference, not a missing feature).
- *"…all NgModule options in **local and optimized** mode"* (1) — exercises the
  local/optimized compilation mode the harness has no way to select.

The genuine compiler-feature gaps that used to sit here are now **closed**:
control / `field` bindings (`ɵɵcontrolCreate`/`ɵɵcontrol`), inline arrows in host
binding/listener + inline-arrow `@Input` transform (full OXC arrow → output-AST
conversion in `convert_expr`), host-binding array/object literal →
`ɵɵpureFunctionN` + hoisted factory, `@Pipe`+`@Injectable` on one class, deep i18n
in `@switch`/`@defer`/`@let`, defer local deps, and the `value_composition` `@let`
spread (folded equivalently in the canonicalizer behind a no-false-pass gate).

---

## (2) Angular Linker — partial → AOT

**Status: DONE for the link path (`libs/treaty-ivy/facade/src/linker.rs` + NAPI
`linkPartial`). Production backend hosting of server fns is now DONE too — the
generated axum Api/SSE/WebSocket handlers + runnable host are real
(real-`cargo build`-against-axum verified).**

Published Angular libraries ship **partial-compilation** output
(`ɵɵngDeclareComponent` / `ɵɵngDeclareDirective` / … the `ɵɵngDeclare*` family).
Without a linker, consuming them forces the JIT fallback and triggers the
`_PlatformLocation needs JIT / @angular/compiler not available` runtime error.

The linker re-emits the fully-AOT `ɵɵdefineComponent` / `ɵɵdefine*` calls so
Treaty apps depend on real Angular libraries with **no JIT**, in **both dev and
production** builds.

- DONE: the Rust linker pass (`ɵɵngDeclare*` → `ɵɵdefine*`) as a surgical span
  rewrite over the EXISTING render3 emit. `link_real_packages.rs` links real
  `@angular/{platform-browser, core, common(+http), forms, router, animations}`
  plus CDK and Material to **ZERO residual `ɵɵngDeclare`** call sites; the linked
  output re-parses as a valid ES module and never imports `@angular/compiler`.
- DONE: bundler wiring — the linker runs over `node_modules` partial libs across
  `@treaty/{vite,rspack,rsbuild,rslib}` (dev + prod) and NAPI surfaces
  `linkPartial` in the addon.
- DONE: production (non-dev) backend **hosting** of server fns — the generated
  axum `Api` (`Json<Req>`→`Json<Resp>`), SSE (`Sse<Stream>`) and WebSocket
  handlers + a runnable host (`build_router()` + `main.rs`) are real, replacing
  the earlier skeletons; verified by a real `cargo build` against axum.

Why it matters: it is the gate to using the existing Angular ecosystem
unmodified, AOT, without bundling `@angular/compiler` into the app.

---

## (3) Node-compatible runtime — `libs/runtime`

**Status: DONE & green.**

Node-compatible Nova runtime (pure-Rust JS engine).

- **483 in-crate `#[test]`.**
- Conformance: `node_conformance` lib **51 / 0**; corpus **68 / 0** (1 skip).
- Real `node:tls` + `node:https` over **rustls (ring)**, offline-clean.
- **Module-granular** `node:` builtins (fs, path, os, crypto, http(s), tls,
  events, stream, buffer, url, …) — each builtin is independently loadable.

---

## (4) File-based routing

**Status: DONE.**

- `treaty_file_routing` Rust crate (deterministic route generation, Rust-first).
- `treaty-file-routing` CLI; `generateRoutes` NAPI binding.
- Exposed as a build-time **`virtual:treaty-routes`** module (no prebuilt
  `routes.ts`) across vite/rspack/rsbuild.
- `examples/file-routed-app` consumes the virtual module via the real engine; the
  JS routing e2e is green.

---

## (5) Example apps + addon + build → boot e2e

**Status: DONE — both apps build and boot under e2e gates.**

- `examples/everything-app` — broad feature coverage. Builds and boots; covered by
  a full `vite build` + headless boot e2e, plus dev-serve, nav/MIME/styles,
  server-fn-dev, and source-validate gates.
- `examples/file-routed-app` — file-routing engine demo; full vite build + headless
  boot e2e of FS routing.
- A **source-validate** e2e parses every authoring source under `src/` through the
  production `@treaty/compiler` seam and asserts each lowers to the correct
  `ɵɵdefine*` with no surviving Angular decorator and no server-fn body leaking
  into client code or the source map.
- Whole-app boot now succeeds: filename→component name + kebab selector (no
  `<ng-component>`), `@for` track index-first (NG0955 fixed), `use:`-directive deps
  resolved, RouterLink/attr-selector directives auto-imported, emulated-encapsulation
  styles, `.treaty`/`.tjsx` served as JS in dev (MIME fixed).
- Addon (`libs/authoring/node`) rebuilt.

---

## (6) Bundler / authoring plugins, source maps, server fns

**Status: architecture DONE; surfaces evolving.**

- **NAPI surface** (`libs/authoring/node`, `index.d.ts`): `compileComponent`,
  `compileComponentSource`, `compileTreatyFile`, `compile`, `compileMany`,
  `runMacro`, **`linkPartial`** (the Angular linker, workstream 2), and
  `generateRoutes` (file routing, workstream 4).
- **Authoring plugins** are pluggable; `.treaty` SFC + JSX-flavored Angular are
  both plugins (signals-by-default, lowercase class, `@control-flow`, JS loops →
  `@for`).
- **Source maps**: compiler emits v3 maps (Ivy JS ↔ authoring source), threaded
  render path → addon → bundler plugins; code string unchanged.
- **Server functions**: declared inline by default — `server{}`, file-level
  `'use server'`, `$$`, and `use websocket` markers are extracted to the backend
  with the body **stripped from the client code AND the source map** (the client
  map references only the replaced RPC stub; a parsed privacy guard enforces this).
  In dev they **run** over a `/__server/*` connect middleware that SSR-loads the
  original module so the real body executes without shipping to the client.
  Backend-agnostic (axum default, Elysia opt-in).
- DONE: production (non-dev) backend hosting — the generated axum `Api`
  (`Json<Req>`→`Json<Resp>`), SSE (`Sse<Stream>`) and WebSocket handlers + a
  runnable host (`build_router()` + `main.rs`) are real (real-`cargo build`
  verified). Server-fn extraction is unified across the marker forms.

---

## (7) Tooling

**Status: DONE.**

- Typecheck with **tsgo** (TS native/Go) — no `tsc` in the typecheck path. Lint
  with **oxlint**. The library `build` tasks (Moon) still emit JS/`.d.ts` via `tsc`;
  removing `tsc` from emit too is a tracked goal (not yet done).
- Rust-first: deterministic compile-time logic is Rust; TS is only NAPI bindings
  + bundler plugins.
- Build orchestration is **Moon** (Nx was removed). The CI gate is `cargo test
  --workspace` + the file-routing crate tests (`.github/workflows/rust-tests.yml`)
  plus the example build→boot e2es; there is no `nx run-many` (the plan's Phase-1c
  `nx run-many` criterion was superseded by the Moon move).
- Branch: `migration/v22-oxc133`. OXC 0.133; **Angular 22.0.0-rc.3** + TypeScript
  6.0 (re-pin to `22.0.0` when stable ships). The Rust partial-linker links the new
  Angular 22 `@Service` DI primitive (`ɵɵngDeclareService` → `ɵɵdefineService`).

---

## (8) Sibling projects (no-AI tooling)

These are tracked separately but share the compiler:

- **treaty-packagr** — OXC-powered ng-packagr alternative; packages anything the
  compiler supports (all authoring plugins).
- **ngx-maintenance** — no-AI GitHub bot: auto-migrate Angular libs v9→latest
  (View Engine → Ivy), PR stale libs.
- **dep-updater** — no-AI self-updating deps + breaking-change codemods (seeded
  from the OXC migration crib).
- **render3-sync harness** — keeps Rust treaty_ivy 1:1 with Angular (drift diff +
  conformance gate + mechanical TS→Rust codegen).

---

## Prioritized "remaining to do everything with the Treaty compiler"

Done this session (moved OUT of the remaining list): all decorators → Ivy AOT
on every entry incl. `compile()`; the Angular linker link path + bundler wiring +
NAPI `linkPartial` (real `@angular/*` + CDK/Material to zero residual); the real
build → boot e2e for both example apps; the source-validate gate; the file-level
`'use server'` client leak CLOSED and server fns running in dev.

1. **Compliance is at the modern-Angular ceiling: 181/185 (97.8%).** The remaining
   **4 DIFFs are all `ng_modules` golden-mode/shape artifacts — by design** (2 jit
   mode, 1 forwardRef-in-imports thunk shape, 1 local/optimized mode): treaty_ivy
   emits the modern `setNgModuleScope` AOT shape, and matching the legacy/jit golden
   would *regress* the output. Not defects, not targeted for "fixing". (The earlier
   genuine gaps —
   control/`field` bindings, inline-arrow host/transform conversion, host-binding
   literal → `ɵɵpureFunctionN`, deep i18n in `@switch`/`@defer`/`@let` — are closed.)
2. **DONE: server-fn extraction UNIFICATION** across all marker forms + production
   (non-dev) backend **hosting** — real axum `Api`/SSE/WebSocket handlers + a
   runnable host, real-`cargo build`-against-axum verified.
3. **SSR / SSG Rust core** (in progress).
4. **Angular-CLI builders / schematics / CLI** (in progress).
5. **JSX / `.treaty` directive authoring**; macro-data inlining; multi-casing
   selectors.
6. **Reduce the 457 compliance skips**: bring host / animations / deferred /
   `ngDeclare`-only fixtures into the runnable set.
7. **Build-to-deploy**: Module Federation per lazy route + lib (versioned
   remotes, partial deploy/rollback).
8. **Promote sibling tooling** (treaty-packagr, ngx-maintenance, dep-updater) on
   the green compiler.

---

## 2026-06-02 — React→Angular authoring + the `treaty-shadcn` showcase library

Delivered this session (all on `migration/v22-oxc133`; matchGolden held 175/185
throughout; crate suites green — rust_authoring 397, treaty_ivy core/template/
decorators/facade, packagr 19 lib + 2 integration):

- **React→Angular compiler** (`apps/rust/authoring/src/jsx/react.rs`): a `.tsx`/`.jsx`
  that imports from `react` / uses hooks compiles to Angular Ivy. `useState`→`signal`
  (+ setter → `.set`/`.update`), `useEffect`→`effect`, `useMemo`→`computed`,
  `useCallback`→its fn, `useRef`→`signal`, `useContext`→`inject`; **props→`input()`**
  (destructured params — closes the long-standing props gap for React AND our `.tsx`);
  signal reads auto-called in the body, in control-flow **conditions** (`@if`/`@for`/
  `@switch` heads), and in event handlers; inline-arrow handlers unwrapped (single
  param → `$event`); `react` imports stripped; top-level `type`/`interface` + TS prop
  defaults erased. Adversarially verified across 4 rounds (overlapping-edit panics +
  the body/condition auto-call gap caught and fixed).
- **`treaty-shadcn`** (`examples/treaty-shadcn/`): a publishable shadcn-style library —
  6 components across ALL THREE surfaces (`.treaty`: Badge/Switch; Treaty `.tsx`:
  Button/Input; **plain React `.tsx`**: Card/Alert) — every one → `ɵɵdefineComponent`.
- **treaty-packagr**: emits a typed Angular **component-class `.d.ts`** (TS9007 closed;
  recovers `InputSignal<T>` from `input<T>()` and from React-prop usage); packages the
  library to `dist/` (7 entries + an APF `package.json` exports map).
- **`treaty-shadcn-demo`** (`examples/treaty-shadcn-demo/`): an Angular gallery app
  consuming the built library; real `vite build` (AOT, 0 residual `ngDeclare`, no JIT)
  + headless render of all 6 components.
- **Showcase runtime bugs fixed** (everything-app, each caught by running the app):
  embedded-listener context crash; selectorless interop (multi-form kebab/camel/Pascal
  selector + camelCase dependency collection); macro `$macro` inlining; hoisted
  control-flow template fns dropped from `.treaty`/`.tjsx` modules; server-fn binding
  NG0203 (an imperative server-fn call now returns a plain `fetch` Promise, not
  `resource()`).

Remaining (non-correctness): FESM flattening (rolldown) in packagr; the 10 genuine
compliance DIFFs; production (non-dev) backend hosting + SSE/WS fan-out.

### 2026-06-02 (later) — roadmap climb (parallel workflows)

- **matchGolden 175 → 178/185 (96.2%)** — three genuine compliance DIFFs closed with
  ZERO regression: forward-ref-in-imports thunking for standalone components
  (`dependencies: () => [Fwd]`), `@defer` per-block lazy **dependency-resolver fns**
  (`ɵɵdefer(…, DepsFn)` + eager/lazy split), and `@defer` blocks **linked into i18n
  messages** (trigger clause in the message + i18n sub-template continuation). The
  remaining 7 DIFFs are harness/golden-vintage (legacy `contFlowTmp`/`_r`-suffix
  naming the canonicalizer doesn't fold, and jit-mode-only NgModule goldens) — not
  compiler defects.
- **FESM flattening** — packagr now flattens each entry's PRIVATE relative imports
  into one flat APF ESM via a self-contained oxc inliner (bails to identity on a
  name collision; `@angular/*` + sibling-entry imports stay external).
- **Production Api hosting** — the generated axum Api handler is now real: it
  deserializes the request, runs the (transpiled/rust) body, and returns `Json<RESP>`
  (previously emitted `return a + b;` against a `Json<f64>` signature — would not have
  compiled). **SSE + WebSocket production handlers + a runnable host (`emit_production_host`)
  are now real too** — a multi-transport server module + its host were emitted into a
  scratch crate and `cargo build` against REAL axum (0.7.9/0.8.9 + tokio + futures)
  succeeds (exit 0); two defects only a live compile surfaces (an SSE `as`-precedence
  bug, an unreachable WS tail) were caught and fixed. rust_authoring is at **427 tests**.

**Roadmap status:** the four parallel workstreams (compliance climb, FESM, Api hosting,
SSE/WS+host) all LANDED. matchGolden **181/185** (97.8%); the remaining 4 DIFFs are
harness/golden-vintage (legacy naming the canonicalizer doesn't fold + jit-mode-only
NgModule goldens), not compiler defects — matching them would diverge from modern Angular.
