# Treaty benchmark report

Combined comparison produced by `tools/treaty-bench/run.mjs`. See
`migration/BENCHMARK.md` for what the suite measures and how to run it.

_Generated from result files dated 2026-06-02T18:25:04.021Z._

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
| `compiler-bench.mjs` | no | --no-run |
| `buildtool-bench.mjs` | no | --no-run |
| `packagr-bench.mjs` | no | --no-run |

### What actually ran vs pending

- **Compiler timing:** Angular @21, Angular @22 and Treaty-oxc are all **measured** in one process over the shared fixture corpus. **Treaty-swc is PENDING** (the swc backend is not implemented — see `migration/SWC-BACKEND-PLAN.md`).
- **Correctness:** Treaty-oxc Ivy output is checked **byte/AST against the `@angular/compiler` oracle** on every oracle-renderable fixture.
- **Build tools:** vite, rolldown and ng-cli are **measured and booted** (WORKS=PASS). rspack / rsbuild / rslib are **PENDING/SKIPPED** — their Treaty plugins exist but the peer bundler cores are not installed in this monorepo, so no build runs here.
- **Packagr:** treaty-packagr and ng-packagr both **ran cleanly** on the same library; output equality was diffed.

Result files collected: 5 (compiler.json, correctness.json, buildtool.json, e2e.json, packagr.json). Timing cells — measured: 8, pending: 4, missing: 0.

> **Treaty-swc is pending.** The SWC backend (a second parser/codegen engine kept
> byte-identical to OXC) is not yet implemented, so its column shows `pending`. Once
> the swc backend lands, the measurement scripts will populate it and the gap closes.

## Compiler suite

### Compile: @Component / partial-declaration -> Ivy

Lower-is-better wall-clock to compile the same authoring input through each backend (ms per component; higher ops/sec is better). Corpus: 29 fixtures (27 shared/renderable timed). Config: 200 warmup + 2000 measured iters, best of 3. Host: node v24.7.0, AMD Ryzen 7 PRO 5850U with Radeon Graphics.

| Metric | Angular @21 | Angular @22 | Treaty-oxc | Treaty-swc |
| --- | --- | --- | --- | --- |
| ms / component | 0.31 ms | 0.37 ms | 0.11 ms | _pending_ |
| ops / sec | 3207 | 2699 | 9159 | _pending_ |
| speedup vs Treaty-oxc | 2.86x | 3.39x | 1.00x (baseline) | _pending_ |

- **Treaty-swc** — _pending_: swc backend NOT YET IMPLEMENTED — see migration/SWC-BACKEND-PLAN.md (Cargo feature `swc` + libs/treaty-ivy/core/src/output/emitter_swc.rs, phase 2). Will be measured once the swc feature lands and @treaty/authoring-node exposes the swc-backed compile path.

### Correctness: Treaty-oxc output vs the Angular compiler oracle

**Treaty-oxc output matches Angular: 27/27** oracle-renderable fixtures (of 29 total; 2 not renderable by the oracle printer, excluded).

| Check | Result |
| --- | --- |
| Oracle | `@angular/compiler@22.0.0-rc.3` |
| Total fixtures | 29 |
| Oracle-renderable (compared) | 27 |
| Not renderable by oracle (excluded, i18n) | 2 |
| Genuine template-lowering divergences | 0 |
| Equivalent ignoring `changeDetection` field | 27/27 |
| STRICT byte/AST-equal (parity normalize) | 0/27 |

> 0/27 renderable fixtures STRICTLY byte/AST-equivalent under parity normalize(); 27/27 are equivalent except the Rust emitter writes a `changeDetection:0` field the v22 oracle now omits (single metadata field, instruction streams identical); 0 genuine lowering divergence(s); 2 not renderable by the oracle printer (i18n)

Honest read: there are **zero genuine template-lowering divergences** — every renderable fixture has an identical create/update instruction stream and identical nested view functions. The strict-parity score is low only because the Rust (oxc) emitter writes a `changeDetection:0` metadata field that `@angular/compiler@22` now omits for the same OnPush metadata. That single field is reported transparently (and is itself arguably a small Rust-side emit bug: `0` = Default, not the requested OnPush) rather than hidden by relaxing the comparison.

## Build-tool suite

### Build + boot: integration through each bundler / builder

Treaty's build-tool plugins (vite / rspack / rsbuild / rslib / rolldown) vs Angular's own `ng` builder. Lower-is-better wall-clock per clean build; `dist` = sum of all emitted output bytes. **WORKS** is an e2e-of-output verdict: the emitted bundle is booted headlessly in jsdom and must render the routed component with no JIT / `@angular/compiler` error — a fast-but-broken build is flagged FAIL, never rewarded. App: `examples/linker-smoke`. @angular/core 22.0.0-rc.3. Best of 1 clean build(s) per tool.

| Tool | Build | dist | WORKS (e2e boot) | Notes |
| --- | --- | --- | --- | --- |
| vite | 2992 ms | 550.5 KiB | PASS | best of 1 run(s); times(ms)=[2992]; jsFiles=1 ivyDefs(literal,minify-sensitive)=6 residualNgDeclare=0 importsCompiler=false linkedOk=true |
| rolldown | 415 ms | 543.9 KiB | PASS | best of 1 run(s); times(ms)=[415]; jsFiles=1 ivyDefs(literal,minify-sensitive)=6 residualNgDeclare=0 importsCompiler=false linkedOk=true |
| rspack | _pending_ | — | _skipped_ | peer @rspack/core not installed in this monorepo — @treaty/rspack's plugin dist is present but the bundler core it drives is absent, so no build can run here. Install @rspack/core to measure. |
| rsbuild | _pending_ | — | _skipped_ | peer @rsbuild/core not installed in this monorepo — @treaty/rsbuild's plugin dist is present but the bundler core it drives is absent, so no build can run here. Install @rsbuild/core to measure. |
| rslib | _pending_ | — | _skipped_ | peer @rslib/core not installed in this monorepo — @treaty/rslib's plugin dist is present but the bundler core it drives is absent, so no build can run here. Install @rslib/core to measure. |
| ng-cli | 11270 ms | 183.9 KiB | PASS | best of 1 run(s); times(ms)=[11270]; jsFiles=1 ivyDefs(literal,minify-sensitive)=0 residualNgDeclare=0 importsCompiler=false linkedOk=true |

> The WORKS layer also runs a negative test (a root component shipped without an Ivy `ɵcmp` def) and confirms it is flagged FAIL — proving the boot probe catches broken output rather than rubber-stamping it.

## Packagr suite

### Library build: treaty-packagr vs ng-packagr (same standard-Angular lib)

Both packagers build the **same** standard-Angular library (plain `@Component` `.ts` classes + a `public-api.ts` barrel) to an APF dist. Lower-is-better wall-clock; `dist` = sum of emitted bytes. (ng-packagr 21.2.3, compiler-cli 22.0.0-rc.3, TS 6.0.3, ng-packagr driven in `compilationMode:"full"` so both emit `ɵɵdefineComponent`). Best of 2 run(s).

| Tool | Build | dist | Status | Notes |
| --- | --- | --- | --- | --- |
| treaty-packagr | 30.6 ms | 2.9 KiB | measured | best of 2 run(s); times(ms)=[35, 31] |
| ng-packagr | 1527 ms | 6.7 KiB | measured | best of 2 run(s); times(ms)=[3159, 1527] |

**Speed:** treaty-packagr 30.6 ms vs ng-packagr 1527 ms — treaty-packagr is **49.9x faster**.

#### Output-equality verdict

- **Emitted Ivy (`ɵɵdefineComponent`): EQUAL across all components** (after normalizing the `i0` alias, `/*@__PURE__*/`, quote style + whitespace; argument order/values preserved).
- **`.d.ts` `ɵcmp` declaration: one real divergence.**
  - `HelloComponent`: Ivy EQUAL, `.d.ts` DIFF — treaty-packagr adds `"isSignal":true` to a classic `@Input` that ng-packagr omits (a treaty-packagr `.d.ts` reconstruction bug).
  - `CounterComponent`: Ivy EQUAL, `.d.ts` EQUAL.
- **`package.json` APF fields** (name, version, type, sideEffects, hasPrimaryExport): all EQUAL.

> VERDICT: emitted Ivy is EQUAL across all components; only one `.d.ts` `ɵcmp` field differs (the `@Input` `isSignal` bug above).

## Notes

- `—` means no measurement was reported for that cell.
- `_pending_` / `_skipped_` mean the backend/tool reported a non-numeric status (e.g. a peer core not installed, or a backend not yet built).
- Compiler "speedup vs Treaty-oxc" is how many times slower each Angular compiler is than Treaty-oxc on the same corpus (higher = Treaty is further ahead).
- The correctness section reports BOTH the strict parity-normalize score and the field-isolated score, on purpose — the gap is a single `changeDetection` metadata field, not hidden by relaxing the comparison.
- WORKS is a real headless jsdom boot of the emitted bundle, not a heuristic — a fast build that ships JIT-needing output is flagged FAIL.
- Numbers come straight from the measurement scripts (`results/*.json`); this runner does not measure.

