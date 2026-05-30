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

**C. Bundler plugins** — each a new package consuming B:
`@treaty/vite`, `@treaty/rspack`, `@treaty/rsbuild`, `@treaty/rslib`. Per-file transform + HMR/watch +
handle file deletion + production build (build-to-deploy output). Territory: `libs/treaty/{vite,rspack,rsbuild,rslib}` (new, disjoint per package).

**D. Module Federation (latest)** — `@module-federation/enhanced` integrated out-of-the-box across the
bundler plugins (Rspack native MF; Vite via `@module-federation/vite`). A `@treaty/module-federation`
helper package + per-plugin wiring. **Depends on C** (wave 2).

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
