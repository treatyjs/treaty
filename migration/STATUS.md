# Treaty migration status

**Branch:** `migration/v22-oxc133`

**Updated:** 2026-05-31

> Companion docs: [OXC migration crib](OXC-MIGRATION-CRIB.md) ·
> [Treaty versions May 2026](treaty-versions-may-2026.md)

---

## Top-line summary

Treaty is a Rust/OXC Angular compiler + Node-compatible runtime. The Ivy
compiler (formerly the single `render3` crate) has been **renamed `treaty_ivy`
and carved into 4 composable workspace crates** under `libs/treaty-ivy/`, behind
a `DecoratorCompiler` registry that mirrors the authoring-plugin design.

Where things stand:

- **treaty_ivy compiler** — DONE & green: 4 crates, **447 `#[test]`** total. Emits
  complete ES-module Ivy output (component/directive/injectable/pipe). Golden
  parity vs Angular's own corpus: **142 / 185 runnable = 76.8%** (committed
  report), climbing toward full parity as the ranked DIFFs are closed.
- **Angular Linker** — IN PROGRESS: partial `ɵɵngDeclare*` → AOT `ɵɵdefine*`
  in `libs/treaty-ivy/facade/src/linker.rs`. Lets Treaty apps consume real
  published Angular libraries with **no JIT** in dev *and* prod. Bundler wiring
  is the next step.
- **Node-compatible runtime** — DONE & green: 483 in-crate tests, module-granular
  `node:` builtins, real `node:tls` + `node:https` over rustls/ring, offline-clean.
- **File-based routing** — DONE: `treaty_file_routing` Rust crate + CLI, wired into
  `examples/file-routed-app`.
- **Example apps + addon** — DONE (sources); real vite build + boot e2e is
  **queued**, blocked on the linker so partial Angular libs link.

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
| `treaty_ivy_facade` | public entry, NAPI ports, compliance + parity harness, linker | 55 |
| **Total** | | **447** |

A **`DecoratorCompiler` registry** sits in the decorators crate so decorator
handlers compose the same way authoring plugins do.

### What emits correctly (DONE)

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
| **matchGolden PASS** | **142** |
| matchGolden DIFF | 43 |
| Skipped (no runnable golden) | 457 |
| **Pass-rate (runnable subset)** | **76.8%** |

> 435 of the skips are partial / `ngDeclare`-only fixtures (no full golden); the
> rest are still-unsupported scenarios (host, animations, deferred, etc.).
>
> Corpus dump env var renamed `RENDER3_CORPUS_DUMP` → **`TREATY_IVY_CORPUS_DUMP`**.

### Ranked DIFF gaps (43 total — the work remaining for full parity)

| Category | Count |
| --- | --- |
| `ɵɵelement` | 12 |
| misc-shape | 9 |
| `ɵɵclassProp` | 3 |
| `ɵɵelementStart` | 3 |
| `ɵɵadvance` | 2 |
| `ɵɵdomProperty` | 2 |
| `ɵɵpureFunction1` | 2 |
| `ɵɵviewQuery` | 2 |
| `ɵɵcontentQuery` | 2 |
| `ɵɵqueryAdvance` | 1 |
| `ɵɵsyntheticHostListener` | 1 |
| `ɵɵstyleProp` | 1 |
| nested-fn-shape | 1 |
| `ɵɵdefer` | 1 |
| `ɵɵattribute` | 1 |

---

## (2) Angular Linker — partial → AOT

**Status: IN PROGRESS.**

`libs/treaty-ivy/facade/src/linker.rs` + a NAPI `linkPartial` entry.

Published Angular libraries ship **partial-compilation** output
(`ɵɵngDeclareComponent` / `ɵɵngDeclareDirective` / … the `ɵɵngDeclare*` family).
Without a linker, consuming them forces the JIT fallback and triggers the
`_PlatformLocation needs JIT / @angular/compiler not available` runtime error.

The linker re-emits the fully-AOT `ɵɵdefineComponent` / `ɵɵdefine*` calls so
Treaty apps depend on real Angular libraries with **no JIT** — and it must run in
**both dev and production** builds.

- DONE: Rust linker pass (`ɵɵngDeclare*` → `ɵɵdefine*`) in the facade crate.
- REMAINING: bundler wiring (run the linker over `node_modules` partial libs
  during vite/build), and NAPI surfacing of `linkPartial` in the addon. This is
  the blocker for the real build + boot e2e (workstream 5).

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
- `treaty-file-routing` CLI.
- `examples/file-routed-app` generates its routes via the real engine; the JS
  routing e2e is green.

---

## (5) Example apps + addon + queued build e2e

**Status: sources DONE; real build e2e QUEUED (blocked on linker).**

- `examples/everything-app` — broad feature coverage.
- `examples/file-routed-app` — file-routing engine demo.
- `AppRoot` complete-module export fixed.
- Addon (`libs/authoring/node`) rebuilt.
- REMAINING: real **vite build + boot e2e**, queued — blocked on the Angular
  linker (workstream 2) so partial Angular libs link AOT.

---

## (6) Bundler / authoring plugins, source maps, server fns

**Status: architecture DONE; surfaces evolving.**

- **NAPI surface** (`libs/authoring/node`, `index.d.ts`): `compileComponent`,
  `compileComponentSource`, `compileTreatyFile`, `compile`, `compileMany`,
  `runMacro`. `linkPartial` to be added with the linker (workstream 2).
- **Authoring plugins** are pluggable; `.treaty` SFC + JSX-flavored Angular are
  both plugins (signals-by-default, lowercase class, `@control-flow`, JS loops →
  `@for`).
- **Source maps**: compiler emits v3 maps (Ivy JS ↔ authoring source), threaded
  render path → addon → bundler plugins; code string unchanged.
- **Server functions**: declared inline by default (`server{}` / `'use server'`
  / `$`); extracted bodies must **not** appear in client source maps — the client
  map references only the replaced API call. Backend-agnostic (axum default,
  Elysia opt-in).

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

1. **Close the 43 compliance DIFFs**, by category (biggest first):
   - `ɵɵelement` shape (12) — the single largest cluster.
   - misc instruction-shape mismatches (9).
   - `ɵɵclassProp` (3), `ɵɵelementStart` (3).
   - `ɵɵadvance` (2), `ɵɵdomProperty` (2), `ɵɵpureFunction1` (2),
     `ɵɵviewQuery` (2), `ɵɵcontentQuery` (2).
   - tail: `ɵɵqueryAdvance`, `ɵɵsyntheticHostListener`, `ɵɵstyleProp`,
     nested-fn-shape, `ɵɵdefer`, `ɵɵattribute` (1 each).
2. **Finish the Angular Linker**: bundler wiring (link partial `node_modules`
   libs during build) + NAPI `linkPartial`; verify AOT in dev *and* prod.
3. **Unblock + run the real build → boot e2e** for `everything-app` and
   `file-routed-app` (depends on #2).
4. **Reduce the 457 compliance skips**: bring host / animations / deferred /
   `ngDeclare`-only fixtures into the runnable set.
5. **Build-to-deploy**: Module Federation per lazy route + lib (versioned
   remotes, partial deploy/rollback), Angular CLI wrap (builders + schematics).
6. **Source-map verification** end-to-end (server-fn bodies excluded from client
   maps).
7. **Promote sibling tooling** (treaty-packagr, ngx-maintenance, dep-updater) on
   the green compiler.
