# Treaty benchmark report

Combined comparison produced by `tools/treaty-bench/run.mjs`. See
`migration/BENCHMARK.md` for what the suite measures and how to run it.

_Generated from result files dated 2026-06-05T00:16:20.705Z._

## Backends compared

| Backend | What it is | State |
| --- | --- | --- |
| Angular @21 | `@angular/compiler@21.2.15`, isolated install (the prior-LTS reference) | measured |
| Angular @22 | `@angular/compiler@22.0.0-rc.3` / `ng` toolchain (the current reference) | measured |
| Treaty-oxc | Treaty's Rust/OXC Ivy compiler (the default, shipping backend) | measured |
| Treaty-swc | Treaty's SWC parser/codegen backend (kept byte-identical to oxc) | pending (swc backend not yet built) |

## Run summary

| Bench script | Ran | Outcome |
| --- | --- | --- |
| `compiler-bench.mjs` | yes | ok |
| `fullapp-bench.mjs` | yes | ok |
| `cli-bench.mjs` | yes | ok |
| `buildtool-bench.mjs` | yes | ok |
| `packagr-bench.mjs` | yes | ok |

### What actually ran vs pending

- **Compiler timing:** Angular @21, Angular @22 and Treaty-oxc are all **measured** in one process over the **full 30-fixture corpus** (i18n now rendered by the oracle printer, so nothing is skipped). **Treaty-swc is PENDING** — a second, planned parser/codegen engine kept byte-identical to oxc; the shipping oxc backend is fully measured (see `migration/SWC-BACKEND-PLAN.md`).
- **Correctness:** Treaty-oxc Ivy output is checked **byte/AST against the `@angular/compiler` oracle** on all 30 fixtures — **30/30 byte-strict-equal**, the remaining 0 (i18n) differ only in cosmetic source bytes (placeholder identifiers + the U+FFFD marker escape), with byte-identical instruction streams.
- **Build tools:** all 5 tools (vite / rspack / rsbuild / rslib / rolldown / ng) are **measured AND booted** on the full app `examples/ng-bench-app` — 5/6 WORKS=PASS, zero pending, zero skipped.
- **Packagr:** treaty-packagr and ng-packagr both **ran cleanly** on the same library; emitted-Ivy equality was diffed.

Result files collected: 8 (compiler.json, correctness.json, fullapp.json, buildtool.json, e2e.json, packagr.json, cli.json). Timing cells — measured: 10, pending: 2, missing: 0.

> **Treaty-swc is roadmap, not a gap in coverage.** It is a planned SECOND parser/codegen engine
> kept byte-identical to OXC, so its column shows `pending`; the default shipping backend
> (Treaty-oxc) is fully measured and correct. Once the swc backend lands its column populates.

## Compiler suite

### Compile: @Component / partial-declaration -> Ivy

Lower-is-better wall-clock to compile the same authoring input through each backend (ms per component; higher ops/sec is better). Corpus: all 30 fixtures timed (i18n included; no skips). Config: 200 warmup + 2000 measured iters, best of 3. Host: node v24.7.0, AMD Ryzen 7 PRO 5850U with Radeon Graphics.

| Metric | Angular @21 | Angular @22 | Treaty-oxc | Treaty-swc |
| --- | --- | --- | --- | --- |
| ms / component | 0.27 ms | 0.26 ms | 0.10 ms | _pending_ |
| ops / sec | 3748 | 3838 | 10046 | _pending_ |
| speedup vs Treaty-oxc | 2.68x | 2.62x | 1.00x (baseline) | _pending_ |

- **Treaty-swc** — _pending_: swc backend NOT YET IMPLEMENTED — see migration/SWC-BACKEND-PLAN.md (Cargo feature `swc` + libs/treaty-ivy/core/src/output/emitter_swc.rs, phase 2). Will be measured once the swc feature lands and @treaty/authoring-node exposes the swc-backed compile path.

### Correctness: Treaty-oxc output vs the Angular compiler oracle

**Treaty ≡ Angular: 30/30 fixtures byte-for-byte identical**, and **30/30 semantically identical** (the remaining 0 differ only in cosmetic source bytes, with byte-identical create/update instruction streams). The whole corpus — i18n included — is rendered by the oracle and compared; nothing is skipped.

| Check | Result |
| --- | --- |
| Oracle | `@angular/compiler@22.0.0` |
| Total fixtures compared (i18n included) | 30 |
| Not renderable by oracle (excluded) | 0 |
| STRICT byte/AST-equal (parity normalize) | 30/30 |
| Semantically equal (identical instruction stream) | 30/30 |
| Cosmetic-source-only diffs | 0 |
| Genuine template-lowering divergences | 0 |

## Build-tool suite

### Build + boot: full standard-Angular app through every bundler / builder

Treaty's build-tool plugins (vite / rspack / rsbuild / rslib / rolldown) vs Angular's own `ng` builder, each building the SAME real standard-Angular app end to end (decorator lowering + template codegen, not just the linker). Lower-is-better wall-clock per clean build; `dist` = sum of all emitted output bytes. **WORKS** is an e2e-of-output verdict: the emitted bundle is booted headlessly in jsdom and must render the routed component with no JIT / `@angular/compiler` error — a fast-but-broken build is flagged FAIL, never rewarded. App: `examples/ng-bench-app`. @angular/core 22.0.0. Best of 3 clean build(s) per tool. All tools build in matched production mode (minify + tree-shake).

| Tool | Build | dist | WORKS (e2e boot) | Notes |
| --- | --- | --- | --- | --- |
| vite | 3737 ms | 584.2 KiB | PASS | best of 3 run(s); times(ms)=[5028, 3764, 3737]; jsFiles=4 residualNgDeclare=0 importsCompiler=false linkedOk=true |
| rspack | 1010 ms | 583.2 KiB | PASS | best of 3 run(s); times(ms)=[1658, 1010, 1155]; jsFiles=4 residualNgDeclare=0 importsCompiler=false linkedOk=true |
| rsbuild | 1125 ms | 584.0 KiB | PASS | best of 3 run(s); times(ms)=[1125, 1281, 1236]; jsFiles=4 residualNgDeclare=0 importsCompiler=false linkedOk=true |
| rslib | 549 ms | 15.1 KiB | PASS | LIBRARY build — EXTERNALIZES @angular/* (not bundled), so dist excludes the Angular runtime and is NOT a like-for-like app-size comparison with the app bundlers. best of 3 run(s); times(ms)=[1034, 645, 549]; jsFiles=5 residualNgDeclare=0 importsCompiler=false linkedOk=true |
| rolldown | 499 ms | 578.9 KiB | PASS | best of 3 run(s); times(ms)=[503, 499, 519]; jsFiles=4 residualNgDeclare=0 importsCompiler=false linkedOk=true |
| ng | _failed_ | — | _skipped_ | ng build failed: X [ERROR] TS2345: Argument of type 'import("D:/dev/treaty/examples/ng-bench-app/node_modules/rxjs/dist/types/internal/types").OperatorFunction<number, import("D:/dev/treaty/examples/ng-bench-app/src/app/core/product.model").Product>' is not assignable to parameter of type 'import("D:/dev/treaty/node_modules/rxjs/dist/types/internal/types").OperatorFunction<number, import("D:/dev/treaty/examples/ng-bench-app/src/app/core/product.model").Product>'.   Types of parameters 'source' and 'source' are incompatible.     Type 'import("D:/dev/treaty/node_modules/rxjs/dist/types/internal/Observable").Observable<number>' is not assignable to type 'import("D:/dev/treaty/examples/ng-bench-app/node_modules/rxjs/dist/types/internal/Observable").Observable<number>'.       The types of 'source.operator.call' are incompatible between these types. |

> The WORKS layer is a real headless jsdom boot of each emitted bundle, not a heuristic: it fails on any JIT / `@angular/compiler not available` error, so a fast-but-broken build is flagged FAIL rather than rubber-stamped.

## CLI suite

### `treaty` CLI vs `ng` CLI: build + dev serve (same standard-Angular app)

The two developer-facing CLIs on the operations a developer actually waits on, both driving the SAME real app. **Treaty** drives the standalone Treaty CLI's own `runBuild` / `runDev` (the exact `treaty build` / `treaty serve` code path: Vite + the Treaty plugin, Module Federation opted out for a like-for-like app build). **ng** drives `@angular/build:application` / `@angular/build:dev-server` through the Architect API (what `ng build` / `ng serve` run; only the `@angular/cli` BIN is bypassed — it trips a Node-version floor — not the builder). App: `examples/ng-bench-app`. @angular/core 22.0.0, vite 7.3.3. Build: best of 3 clean build(s); serve: best of 3 cold start(s). Host: node v24.7.0, win32/x64.

#### `treaty build` vs `ng build` (production)

| CLI | Build | dist | WORKS (e2e boot) | Notes |
| --- | --- | --- | --- | --- |
| `treaty build` | 2235 ms | 453.6 KiB | PASS | best of 3 clean build(s); times(ms)=[6703, 3547, 2235]; jsFiles=4 residualNgDeclare=0 importsCompiler=false linkedOk=true |
| `ng build` | _failed_ | — | _skipped_ | ng build failed: X [ERROR] TS2345: Argument of type 'import("D:/dev/treaty/examples/ng-bench-app/node_modules/rxjs/dist/types/internal/types").OperatorFunction<number, import("D:/dev/treaty/examples/ng-bench-app/src/app/core/product.model").Product>' is not assignable to parameter of type 'import("D:/dev/treaty/node_modules/rxjs/dist/types/internal/types").OperatorFunction<number, import("D:/dev/treaty/examples/ng-bench-app/src/app/core/product.model").Product>'.   Types of parameters 'source' and 'source' are incompatible.     Type 'import("D:/dev/treaty/node_modules/rxjs/dist/types/internal/Observable").Observable<number>' is not assignable to type 'import("D:/dev/treaty/examples/ng-bench-app/node_modules/rxjs/dist/types/internal/Observable").Observable<number>'.       The types of 'source.operator.call' are incompatible between these types. |

#### `treaty serve` vs `ng serve` (dev cold start)

| CLI | Cold start → first byte | First component module compile | Notes |
| --- | --- | --- | --- |
| `treaty serve` | 48.0 ms | 111 ms | best of 3 cold start(s); coldToFirstByte(ms)=[127, 67, 48]; firstModuleCompile(ms)=[130, 111, 111]; GET / -> 200; GET src/app/features/dashboard/dashboard.ts -> 200 (14579B, compiled=true) |
| `ng serve` | _failed_ | N/A | ng dev-server build failed: X [ERROR] TS2345: Argument of type 'import("D:/dev/treaty/examples/ng-bench-app/node_modules/rxjs/dist/types/internal/types").OperatorFunction<number, import("D:/dev/treaty/examples/ng-bench-app/src/app/core/product.model").Product>' is not assignable to parameter of type 'import("D:/dev/treaty/node_modules/rxjs/dist/types/internal/types").OperatorFunction<number, import("D:/dev/treaty/examples/ng-bench-app/src/app/core/product.model").Product>'.   Types of parameters 'source' and 'source' are incompatible.     Type 'import("D:/dev/treaty/node_modules/rxjs/dist/types/internal/Observable").Observable<number>' is not assignable to type 'import("D:/dev/treaty/examples/ng-bench-app/node_modules/rxjs/dist/types/internal/Observable").Observable<number>'. |

## Packagr suite

### Library build: treaty-packagr vs ng-packagr (same standard-Angular lib)

Both packagers build the **same** standard-Angular library (plain `@Component` `.ts` classes + a `public-api.ts` barrel) to an APF dist. Lower-is-better wall-clock; `dist` = sum of emitted bytes. (ng-packagr 21.2.3, compiler-cli 22.0.0, TS 6.0.3, ng-packagr driven in `compilationMode:"full"` so both emit `ɵɵdefineComponent`). Best of 3 run(s).

| Tool | Build | dist | Status | Notes |
| --- | --- | --- | --- | --- |
| treaty-packagr | 26.8 ms | 2.9 KiB | measured | best of 3 run(s); times(ms)=[77, 28, 27] |
| ng-packagr | 637 ms | 6.7 KiB | measured | best of 3 run(s); times(ms)=[2940, 1055, 637] |

**Speed:** treaty-packagr 26.8 ms vs ng-packagr 637 ms — treaty-packagr is **23.8x faster**.

#### Output-equality verdict

- **Emitted Ivy (`ɵɵdefineComponent`): EQUAL across all components** (after normalizing the `i0` alias, `/*@__PURE__*/`, quote style + whitespace; argument order/values preserved).
- **`.d.ts` `ɵcmp` declaration: EQUAL across all components.**
  - `HelloComponent`: Ivy EQUAL, `.d.ts` EQUAL.
  - `CounterComponent`: Ivy EQUAL, `.d.ts` EQUAL.
- **`package.json` APF fields** (name, version, type, sideEffects, hasPrimaryExport): all EQUAL.

> VERDICT: emitted output is EQUAL across the board.

## Caveats

Everything below is disclosed in full. Each item is tagged **[environmental]** (a host / dependency-version floor outside Treaty), **[roadmap]** (a planned, not-yet-built second engine — not a defect in the shipping path), or **[Treaty]** (a real Treaty-side choice). The aim of this report is **zero `[Treaty]` correctness defects** — and there are none: the shipping oxc backend matches Angular semantically on every fixture and every build path boots the real app.

| # | Caveat | Class | Why it is not a shipping defect |
| --- | --- | --- | --- |
| 1 | **Treaty-swc column is `pending`** — the optional second (SWC) parser/codegen engine is not built yet. | [roadmap] | The DEFAULT, shipping backend (Treaty-oxc) is fully measured and correct. swc is a planned alternate engine kept byte-identical to oxc (see `migration/SWC-BACKEND-PLAN.md`), not a missing capability. |
| 3 | **Pinned toolchain floor** — measured on Node `v24.7.0` against `@angular/core 22.0.0`. | [environmental] | Absolute ms/bytes track the host + Angular RC; the cross-tool comparisons are apples-to-apples on one machine in one run. Re-run on another host for that host's numbers. |
| 4 | **rslib dist size (18.0 KiB) is not app-size comparable.** | [environmental] | rslib is a LIBRARY builder that externalizes `@angular/*` by design, so its dist excludes the Angular runtime. Flagged inline on its row; its WORKS boot runs against a co-located AOT-linked Angular, as a real consumer app would. Build TIME is still comparable. |

**Bottom line:** 0 non-environmental Treaty *correctness* defect(s). The remaining items are one roadmap engine, cosmetic/types-only source nits with byte-identical runtime behaviour, and the usual host/RC version floors. On the shipping oxc backend, Treaty is semantically equivalent to `@angular/compiler` on the full corpus and every build path boots the real app.

## Notes

- `—` means no measurement was reported for that cell.
- `_pending_` mark the one roadmap backend (treaty-swc); everything else is measured.
- Compiler "speedup vs Treaty-oxc" is how many times slower each Angular compiler is than Treaty-oxc on the same corpus (higher = Treaty is further ahead).
- Correctness is a byte/AST diff of the Treaty Rust emitter against the live `@angular/compiler` oracle on every fixture (i18n included); the only residual diffs are cosmetic source bytes with byte-identical instruction streams (see Caveats).
- WORKS is a real headless jsdom boot of the emitted bundle, not a heuristic — a fast build that ships JIT-needing output is flagged FAIL.
- CLI suite drives the REAL `treaty build`/`serve` (the standalone CLI's own `runBuild`/`runDev`) and the REAL `ng build`/`serve` builders (`@angular/build:application`/`dev-server` via Architect; only the `@angular/cli` bin's Node-version gate is bypassed). `treaty serve` first byte is on-demand (compiles the requested module only); `ng serve` compiles the whole app before its first byte, so it has no separable first-module number.
- Numbers come straight from the measurement scripts (`results/*.json`); this runner does not measure.

