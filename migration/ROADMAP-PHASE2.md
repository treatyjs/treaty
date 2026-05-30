# Treaty — Phase 2 Roadmap (complete the vision)

Goal: every README feature working, **full file-by-file Angular→Ivy compilation** (fast, incremental,
with dead-code elimination + file deletion), bundler plugins for **Vite / Rspack / Rsbuild / Rslib**,
**built-in Module Federation (latest)**, a full **REPL "everything" demo**, a code **restructure**, and
**100% compliance at the core**.

## Where we are (committed this program)
- render3 engine: direct-to-Ivy, 328+ tests; oracle 27/27; **compliance 14/98 and climbing**.
- Authoring plugin layer: `.treaty` + JSX are peer authoring plugins → render3 (`016012f`).
- Nova runtime (`libs/runtime`): macros/RSC (pre)render + server-fn/serverless execution (`587be2c`).
- Server fns: markers (`'use server'`/`$$`/`server:<lang>`), **registry-default binding**, **API/WebSocket/Stream**
  transports, backends **axum (default) / Elysia / Express** (`19c7a0c`). Body never enters the Ivy.
- LSP foundation `@treaty/lsp` (`a4e7317`). Eden `@treaty/httpclient` signal-resource client.

## README feature matrix → status
| README feature | Status now | Workstream |
|---|---|---|
| Vite support | ✅ (existing) → upgrade to Rust file-by-file | C |
| SSR | ✅ | — |
| SSG | 🚧 planned | G (prerender via Nova macros) |
| Authoring to Ivy (direct) | ✅ (render3) | A |
| Server-side function in component | ✅ DONE (update README) | done |
| function chunking | 🏗️ → server-fn extraction + code-split | B/C |
| First-class Module Federation | 🏗️ → this phase | D |
| Build to deploy | 🚧 → bundler build outputs | C/G |
| Zoneless / signals-by-default / OnPush | ✅ | done |

## Workstreams (disjoint territories so they parallelize)
**A. 100% core compliance** — `libs/render3` only. Iterative rounds (analyze→fan-out by file→verify),
keep oracle 27. Currently round 4 (`wkah9q0xi`). Target buckets: `@let` inline-const, arrow const-pool,
projectionDef, i18n, queries via `source_compile` extraction. Goal: 100% of runnable cases.

**B. File-by-file compilation core** — a TS package `@treaty/compiler` wrapping the NAPI addon
(`compileTreatyFile`/`compileComponentSource`) with: a `transform(id, code)→IvyJS` per-file API,
an **incremental cache** (hash→output), a **deleted-file** hook, and **dead-code/tree-shaking** metadata
(emit `/*#__PURE__*/`, `sideEffects:false`, drop unused server-fn client bindings). Fast per file; the
core compliance (A) guarantees correctness. Territory: `libs/treaty/compiler` (new).

**Covers ALL authoring, including plain Angular itself (user, 2026-05-30):** the per-file transform
applies to `.treaty`, JSX (`.tsx`/`.tjsx`), AND **standard Angular `.ts`** — so a dev using plain
Angular still benefits from Treaty's fast direct-to-Ivy file-by-file compile + dead-code/file-deletion.
It must handle **every Angular decorator kind** found in a `.ts` (`@Component`, `@Directive`, `@Pipe`,
`@Injectable`, `@NgModule`), compiling each to Ivy, and **pass through non-Angular TS unchanged**
(return null so the bundler's normal TS handling applies).

**Expose render3 directly via NAPI (user 2026-05-30):** the current addon (`libs/authoring/node`) wraps
`apps/rust/authoring`'s three entry points and uses `render3` only transitively. Add a **render3 NAPI
binding** (a `libs/render3/node` crate, or extend the authoring addon) that exposes render3's FULL Ivy
compiler surface — component/directive/pipe/injector/module compilers + DI (`angular.rs`) — behind a
single per-file Angular entry point, so `@treaty/compiler` drives the complete render3 package for full
all-decorator Angular file-by-file, not just the three authoring functions. Verified already: all
bundler plugins route through `@treaty/compiler` → `@treaty/authoring-node` → the Rust compiler (no TS
reimplementation). Sequenced with H (touches NAPI + render3 + core; after Wave 1).

**C. Bundler plugins** — each a new package consuming B:
`@treaty/vite`, `@treaty/rspack`, `@treaty/rsbuild`, `@treaty/rslib`. Per-file transform + HMR/watch +
handle file deletion + production build (build-to-deploy output). Territory: `libs/treaty/{vite,rspack,rsbuild,rslib}` (new, disjoint per package).

**D. Module Federation (latest, AUTOMATIC + zero-config — user 2026-05-30)** —
`@module-federation/enhanced` integrated out-of-the-box across the bundler plugins (Rspack native MF;
Vite via `@module-federation/vite`). **EVERY Treaty app is Module Federation automatically** — the user
configures NOTHING. A `@treaty/module-federation` helper generates the host/remote config from project
structure (no hand-written `ModuleFederationPlugin`). Wired via the Angular-CLI integration (I) so
`ng build`/`ng serve` produce federated host+remotes by default. **Depends on C** (wave 2).

**D2. Federation as deployment granularity (user 2026-05-31) — "module deployment without a full app":**
MF is the unit of DEPLOYMENT, not just config. (1) **Every lazy feature route auto-becomes a remote**
(route-graph pass; no manual `exposes`). (2) **Libs are federated modules** too. (3) **Versioned,
independently-deployable** modules via a runtime **manifest** (module→version→URL) + an
`@module-federation/enhanced` runtime plugin that resolves each remote's current version at load. (4)
**Partial deploy + partial ROLLBACK** — update ONE module's manifest entry to deploy/roll back a single
route/lib without redeploying the app. Treaty emits the federated modules + manifest; the platform
serves + flips versions. Layer on the D foundation (after wave 2). See [[treaty-federation-deployment]].

**I. Angular-CLI integration — Treaty as a wrapper around Angular itself (user 2026-05-30)** — zero
the user has to configure:
- **`angular.json` builders** (`@treaty/build`, `@angular-devkit/architect` `createBuilder`): a
  `build`/`serve` builder that runs Treaty's Rust compiler + the rspack(+MF) plugin, referenced as the
  project's `architect.build.builder`. So `ng build` / `ng serve` use Treaty + automatic federation.
- **Schematics** (`@treaty/schematics`, `@angular-devkit/schematics`): `ng add @treaty` rewrites
  `angular.json` to the Treaty builders and scaffolds projects/libs **pre-wired as MF host + remotes**;
  `ng generate` app/lib schematics keep the federation structure out-of-the-box. Nothing to configure.
- **CLI** (`@treaty/cli`, bin `treaty`): the STANDALONE driver for projects WITHOUT `angular.json`
  (user 2026-05-30) — `treaty dev`/`build`/`generate` drive the bundler plugins + `@treaty/compiler`
  directly, no Angular workspace file. (It is NOT an `ng` wrapper; angular.json projects use `ng`
  itself plus the Treaty builders/schematics above.)
- **Depends on C/D** (wave 2). Territory: `libs/treaty/{build,schematics,cli,module-federation}` (new).

**Dual-mode: angular.json AND standalone (user 2026-05-30)** — the build tools support BOTH:
- **angular.json projects** → the architect builders / schematics (workstream I): `ng build`/`ng serve`.
- **standalone** (no `angular.json`) → the bundler plugins (workstream C) used directly in
  `vite.config` / `rspack.config` / rsbuild / rslib config.
Both paths share the same `@treaty/compiler` core and keep Module Federation automatic. The user picks
either; nothing forces an Angular workspace file.

**E. REPL "everything" demo** — `apps/repl`: an example app exercising `.treaty` + JSX + signals + control
flow + server fns (all 3 transports) + macros + each bundler plugin + a Module Federation host/remote
example, with plugin output viewers. **Depends on B/C/D** (wave 2).

**F. Code restructure** — target layout (SOLO step, rewrites paths repo-wide):
- `libs/compiler/render3` (was `libs/render3`), `libs/compiler/authoring` (was `apps/rust/authoring` — it is a
  library, not an app), `libs/runtime` (Nova). `libs/treaty/*` for TS packages (httpclient, lsp, compiler,
  vite, rspack, rsbuild, rslib, module-federation). `apps/repl` stays an app.
- Update workspace members, NAPI dep paths, `tsconfig.base.json` paths, moon projects.

**G. Remaining README features** — SSG (prerender via Nova macros at build), build-to-deploy (bundler
production outputs), function chunking (lazy server-fn/route code-split). Mostly fall out of B/C.

**H. Performance / parallelism (first-class goal: the FASTEST compiler, user 2026-05-30)** —
multi-thread and parallelize everywhere it is sound, since file-by-file compiles are independent:
- **Rust**: `rayon` parallel iteration to compile independent files/components across cores; keep
  internals thread-safe (per-file `oxc_allocator::Allocator`/arena, no shared mutable global state).
- **NAPI**: a `compileMany(files: {id,code}[]) -> results[]` batch entry that fans the work across a
  thread pool (rayon / napi AsyncTask) and returns all results in ONE call — avoids per-file JS↔Rust
  round-trips. Bundlers/the core call this for cold builds.
- **JS**: `@treaty/compiler` + bundler plugins use the batch API / a worker pool instead of serial
  per-file calls; keep the incremental cache for warm rebuilds.
- Benchmark harness: compile a large fixture project, compare serial vs parallel wall-clock.
Cross-cutting (NAPI + render3 + authoring + core) → runs AFTER Wave 1 (it touches crates the
compliance + bundler workflows are editing). Goal: beat ngtsc on cold + incremental builds.

**J. Delete the legacy TypeScript compiler (GATED — user 2026-05-30)** — once the Rust compiler is
fully working "how we want" INCLUDING all authoring, DELETE the old TS compiler entirely. Targets:
`apps/repl/src/tools/treaty-sfc/treat-to-ivy.ts` + `printer.ts`, `libs/typescript/compiler/`, and the
TS `treatyToIvy` fallback in the REPL's `rust-compiler-loader.ts`. **Gate (all must hold):** NAPI
exposes ALL authoring incl. bare-JSX `.tsx` + the full render3 surface (workstream B); the REPL runs
end-to-end on the Rust path with the TS fallback removed; oracle/compliance parity holds. This is the
headline instance of the "dead code + file deletion" goal — do NOT delete before the gate is met.

## Harness
- **File-by-file harness**: compile a corpus of individual `.treaty`/`.tsx`/`.ts` files through `@treaty/compiler`
  and assert each emits valid Ivy + round-trips through each bundler plugin; measure per-file time
  (fast-path budget). Add to `libs/render3/compliance` sibling: `libs/treaty/compiler/test/file-by-file`.
- **Compliance harness** (exists): `libs/render3/compliance/run-compliance.mjs` — the 100%-core gate.
- **Dead-code/deletion harness**: assert removed files drop from output + unused bindings are tree-shaken.

## Parallelization & sequencing
- **Wave 1 (now):** A (compliance, running) ‖ C-core: a single workflow doing B (`@treaty/compiler`) then
  the 4 bundler plugins in parallel (disjoint packages) + verify.
- **Wave 2:** D (Module Federation) ‖ E (REPL demo) — both after C.
- **Solo:** F (restructure) — between waves, when nothing else is editing, then fix-ups.
- Each workflow edits a disjoint territory; the restructure is the only repo-wide one and runs alone.
- Commit each workstream selectively on green; `cargo test --workspace` + each package `tsc` as gates.
