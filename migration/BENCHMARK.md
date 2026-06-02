# Benchmark suite — `tools/treaty-bench`

Status: runner + report harness wired at `tools/treaty-bench`; the `treaty-swc` column is **pending**
until the SWC backend lands (see `migration/SWC-BACKEND-PLAN.md`, which is "design / not yet
implemented").
Related: `migration/SWC-BACKEND-PLAN.md` (§8 names this suite), `migration/BACKEND-PARITY.md`,
`.claude/workflows/benchmark.js`, `tools/treaty-bench/run.mjs`.

## What it is

`tools/treaty-bench` is Treaty's **performance** benchmark suite. Where `tools/backend-parity` proves
the backends are *correct* (byte-identical Ivy), this suite measures how *fast* they are. It compares,
on the **same inputs**, three compile backends across the build tools Treaty plugs into.

### Backends compared

| Backend | What it is | State |
| --- | --- | --- |
| **Angular compiler** | Angular's own `@angular/compiler` / `@angular/compiler-cli` and the `ng` builder — the reference baseline | available |
| **Treaty-oxc** | Treaty's Rust/OXC Angular (Ivy) compiler — the **default, shipping** backend | available |
| **Treaty-swc** | Treaty's second SWC parser/codegen backend, kept byte-identical to OXC for hosts that already embed SWC | **pending** (swc backend not yet built) |

Treaty-swc is a *host-integration / performance* option, not a semantic one: it must emit byte-identical
Ivy to the OXC backend (enforced by `tools/backend-parity`). It does not exist yet
(`migration/SWC-BACKEND-PLAN.md` is design-only, phases 0–4 unimplemented), so in every report its
column renders as `pending` rather than a fabricated number. When the swc backend ships, the
measurement scripts populate it and the gap closes with no change to the runner.

### Axes measured

The suite has two axes, each driven by its own measurement script:

1. **Compiler axis** — `compiler-bench.mjs`. The pure `@Component` / partial-declaration **→ Ivy**
   compile step, no bundler: how long each backend takes to turn the same authoring input into Ivy
   (the Angular compiler vs Treaty-oxc vs Treaty-swc).

2. **Build-tool axis** — `buildtool-bench.mjs`. The same work routed through each build-tool
   integration Treaty ships a plugin for, plus Angular's own builder as the reference:

   | Build tool | Role |
   | --- | --- |
   | **vite** | Treaty's Vite plugin (the dev-serve / build default for many apps) |
   | **rspack** | Treaty's Rspack loader |
   | **rsbuild** | Rsbuild integration (Rspack-based app framework) |
   | **rslib** | Rslib integration (library builds) |
   | **rolldown** | the oxc-native bundler (the natural fit for the OXC backend) |
   | **ng** | Angular's own `ng` builder — the reference baseline |

Each build-tool scenario is measured for all three backends where applicable; a build tool that is not
installed in the repo is reported as `skipped` (with a note) rather than aborting the suite.

## Layout

```
tools/treaty-bench/
  package.json          # @treaty/bench (private); `bench` script -> run.mjs
  run.mjs               # the RUNNER: drives the two bench scripts, collects
                        # results/*.json, prints the combined table, writes REPORT.md
  compiler-bench.mjs    # MEASUREMENT (authored/run by the compiler-axis agent)
  buildtool-bench.mjs   # MEASUREMENT (authored/run by the build-tool-axis agent)
  results/
    *.json              # one self-describing result file per measurement run
    REPORT.md           # the combined comparison the runner writes
```

The **runner does not measure anything** — it only orchestrates and reports. The measurement scripts
(`compiler-bench.mjs`, `buildtool-bench.mjs`) are owned by the measurement side of the workflow; they
emit JSON, and the runner turns that JSON into a comparison. This keeps "how we time things" separate
from "how we present things", and lets the runner produce a sensible report even when a measurement is
missing or pending.

### Result file shape

Each measurement script writes one or more self-describing JSON files into `results/`:

```jsonc
{
  "suite": "compiler",        // or "buildtool"
  "unit": "ms",               // optional; default "ms"
  "lowerIsBetter": true,      // optional; default true
  "rows": [
    {
      "scenario": "hello-world @Component -> Ivy",
      "tool": "vite",          // build-tool axis only
      "results": {
        "angular":    { "value": 1234, "unit": "ms", "samples": 5 },
        "treaty-oxc": { "value": 210,  "unit": "ms", "samples": 5 },
        "treaty-swc": { "status": "pending", "note": "swc backend not yet built" }
      }
    }
  ]
}
```

A per-backend cell is either a real measurement (`{ value, unit?, samples? }`) or a non-numeric status
(`{ status: "pending" | "skipped" | "error", note? }`). The runner is defensive about every field, so
a partial or malformed file degrades to `—` / `pending` cells rather than crashing the report.

## How to run it

From the repo root:

```bash
# full suite: invoke both bench scripts, collect results, print + write REPORT.md
node tools/treaty-bench/run.mjs

# or via the package script
cd tools/treaty-bench && npm run bench
```

Useful flags:

```bash
node tools/treaty-bench/run.mjs --no-run        # skip the (slow) bench scripts; just
                                                # re-render the report from existing results/*.json
node tools/treaty-bench/run.mjs --quiet         # suppress the bench scripts' own stdout
node tools/treaty-bench/run.mjs --results <dir> # read/write results from a different dir
node tools/treaty-bench/run.mjs --out <file>    # write the report somewhere other than results/REPORT.md
```

The runner prints the combined markdown comparison to **stdout** and writes the same content to
`tools/treaty-bench/results/REPORT.md`. It **tolerates missing / pending rows** throughout: a bench
script that is absent or exits non-zero does not abort the run, an empty `results/` still yields a
(mostly empty) report, and a backend with no measurement renders as `pending` — never a fake number.

### Via the workflow

`.claude/workflows/benchmark.js` (`Workflow({ name: "benchmark" })`) drives the whole thing in three
phases:

- **Measure** — one agent per axis authors/refreshes and runs `compiler-bench.mjs` /
  `buildtool-bench.mjs`, leaving JSON in `results/`.
- **Report** — one agent runs `node tools/treaty-bench/run.mjs` to collect the JSON and write
  `REPORT.md`.
- **Summary** — one agent reads the report and writes an honest narrative of where Treaty-oxc wins or
  lags vs the Angular compiler, and states plainly that Treaty-swc is pending.

## Reading the report

`REPORT.md` contains: a **Backends compared** table (with Treaty-swc's pending state), a **Run summary**
(which bench scripts ran, how many result files / cells were measured vs pending vs missing), and one
table per axis (compiler suite, build-tool suite). Each row shows the per-backend number plus an
`oxc vs ng` column — the speedup of Treaty-oxc over the Angular compiler for that row, shown only when
both are real measurements.

Honesty rules baked into the harness:

- `—` means no measurement was reported for a cell; `_pending_` / `_skipped_` / `_error_` mean the
  backend reported a non-numeric status. Neither is presented as a result.
- Numbers come straight from the measurement scripts — the runner never invents or extrapolates them.
- Treaty-swc is `pending` until the swc backend (`migration/SWC-BACKEND-PLAN.md`) is implemented; the
  summary phase is instructed not to present pending as a win.

## Why it lives next to backend-parity

`tools/backend-parity` answers *"is the swc backend's output correct?"* (byte-identical Ivy);
`tools/treaty-bench` answers *"is it worth using, and how does Treaty compare to Angular?"* (speed).
Together they make the multi-backend story defensible: parity gates correctness on every PR, and the
bench suite quantifies the performance trade-off — Angular's own compiler vs Treaty-oxc vs (eventually)
Treaty-swc, across the rs family (rspack / rsbuild / rslib), vite, rolldown, and `ng`.
