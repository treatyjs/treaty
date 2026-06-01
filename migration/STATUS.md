# Treaty migration status

**Branch:** `migration/v22-oxc133`

**Updated:** 2026-06-01

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
  Angular's own corpus: **170 / 185 runnable = 91.9%** (live-scored), climbing
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
| **matchGolden PASS** | **170** |
| matchGolden DIFF | 15 |
| Skipped (no runnable golden) | 457 |
| compile-without-error (of 619-entry dump) | 594 |
| **Pass-rate (runnable subset)** | **91.9%** |

> ~435 of the skips are partial / `ngDeclare`-only fixtures (no full golden); the
> **Angular Linker** work below brings that whole class into scope.
>
> Corpus dump env var renamed `RENDER3_CORPUS_DUMP` → **`TREATY_IVY_CORPUS_DUMP`**.
> The committed `COMPLIANCE-REPORT.md` is regenerated to 170/185 (`--report`).

### Ranked DIFF gaps (the work toward full parity)

The live harness reports **15 matchGolden DIFFs** of the 185 runnable cases. The
categories below enumerate the outstanding gap shapes (genuine compiler features
plus harness/golden-authoring limits); a few categories collapse to one DIFF case
each, so the per-category tallies are upper bounds, not the headline DIFF count.

Genuine features (large / bespoke):
- control / `field` bindings (2) — need `ɵɵcontrolCreate` (create) + `ɵɵcontrol` (update), breaking the element-create chain (`R3DirectiveMetadata.control_create` scaffolding exists, unpopulated).
- inline arrows in host binding/listener (2) + inline-arrow `@Input` transform (1) — need full OXC arrow/function-expression → output-AST conversion in `convert_expr`.
- host-binding array/object literal value → `ɵɵpureFunctionN` + hoisted factory (1).
- a class with both `@Pipe` and `@Injectable` (ctor DI + `ɵpipe` + `ɵprov`) (1).
- deep i18n in `@switch`/`@defer`/`@let` (3), defer local deps (1), animation `syntheticHostListener` (1), signal-query `queryAdvance` (1).

Harness / golden-authoring limits (not compiler defects):
- `ng_modules` JIT-mode goldens (4) — the harness can't request linker JIT mode.
- `value_composition` `@let` spread (2) — Angular's own goldens spell the binding name two incompatible ways under the canonicalizer.
- standalone `forwardRef`-in-imports thunk shape (1).

---

## (2) Angular Linker — partial → AOT

**Status: DONE for the link path (`libs/treaty-ivy/facade/src/linker.rs` + NAPI
`linkPartial`). Production backend HOSTING of server fns is the remaining piece.**

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
- REMAINING: production (non-dev) backend **hosting** of server fns — real SSE/WS
  transport fan-out (the in-dev `/__server/*` backend already runs them; the
  production axum handlers are skeletons).

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
- REMAINING: server-fn extraction UNIFICATION across all marker forms (in
  progress); production (non-dev) backend hosting + real SSE/WS transport fan-out
  (the dev backend runs; the production axum handlers are skeletons).

---

## (7) Tooling

**Status: DONE.**

- Typecheck with **tsgo** (TS native/Go). Lint with **oxlint**. **No `tsc`**
  anywhere.
- Rust-first: deterministic compile-time logic is Rust; TS is only NAPI bindings
  + bundler plugins.
- Branch: `migration/v22-oxc133`. OXC 0.133, Angular 22 targets.

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

1. **Close the remaining 15 compliance DIFFs** (toward 185) — see the ranked DIFF
   gaps above (control/`field` bindings, inline-arrow host/transform conversion,
   host-binding literal → `ɵɵpureFunctionN`, `@Pipe`+`@Injectable`, deep i18n in
   `@switch`/`@defer`/`@let`, plus the harness-limited goldens).
2. **Server-fn extraction UNIFICATION** across all marker forms (in progress), then
   production (non-dev) backend **hosting** + real SSE/WS transport fan-out (the
   dev `/__server/*` backend runs them; the production axum handlers are skeletons).
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
