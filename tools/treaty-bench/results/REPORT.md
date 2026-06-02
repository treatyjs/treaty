# Treaty benchmark report

Combined comparison produced by `tools/treaty-bench/run.mjs`. See
`migration/BENCHMARK.md` for what the suite measures and how to run it.

## Backends compared

| Backend | What it is | State |
| --- | --- | --- |
| Angular compiler | Angular's own `@angular/compiler` / `ng` toolchain (the reference) | available |
| Treaty-oxc | Treaty's Rust/OXC Ivy compiler (the default, shipping backend) | available |
| Treaty-swc | Treaty's SWC parser/codegen backend (byte-identical to oxc) | available |

## Run summary

| Bench script | Ran | Outcome |
| --- | --- | --- |
| `compiler-bench.mjs` | no | --no-run |
| `buildtool-bench.mjs` | no | --no-run |

Result files collected: 1. Cells — measured: 0, pending: 0, missing: 0.

## Compiler suite

### Compile: @Component / partial-declaration -> Ivy

Lower-is-better wall-clock to compile the same authoring input through each backend.

_No results yet. Run the suite once the measurement script has produced JSON in `results/`._

## Build-tool suite

### Build: integration through each bundler / builder

Treaty's build-tool plugins (vite / rspack / rsbuild / rslib / rolldown) vs Angular's own `ng` builder. Lower-is-better wall-clock per scenario.

_No results yet. Run the suite once the measurement script has produced JSON in `results/`._

## Notes

- `—` means no measurement was reported for that cell.
- `_pending_` / `_skipped_` / `_error_` mean the backend reported a non-numeric status.
- `oxc vs ng` is the speedup of Treaty-oxc over the Angular compiler for that row (higher is better), only shown when both are real measurements.
- Numbers come straight from the measurement scripts; this runner does not measure.

