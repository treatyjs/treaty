/*
 * benchmark — run Treaty's performance benchmark suite and summarize it.
 *
 * Treaty is a Rust/OXC Angular (Ivy) compiler. This workflow drives the bench
 * harness at `tools/treaty-bench` to compare, on the SAME inputs:
 *   - the Angular compiler (Angular's own @angular/compiler / `ng` — the reference),
 *   - Treaty-oxc (Treaty's default, shipping Rust/OXC Ivy backend), and
 *   - Treaty-swc (Treaty's second SWC parser/codegen backend — PENDING until the
 *     swc backend lands; see migration/SWC-BACKEND-PLAN.md),
 * across the build-tool integrations: vite / rspack / rsbuild / rslib / rolldown,
 * plus Angular's own `ng` builder.
 *
 * Flow:
 *   (Measure)  Two measurement agents, one per axis, author/refresh + RUN the
 *              measurement scripts (compiler-bench.mjs, buildtool-bench.mjs) so
 *              each leaves self-describing JSON in tools/treaty-bench/results/.
 *   (Report)   One agent runs the runner (`node tools/treaty-bench/run.mjs`),
 *              which collects results/*.json, prints the combined markdown table,
 *              and writes results/REPORT.md.
 *   (Summary)  One agent reads REPORT.md + the raw results and writes a short,
 *              honest narrative summary (where Treaty-oxc wins/loses vs Angular;
 *              that Treaty-swc is pending).
 *
 * Invoke with:
 *   Workflow({ name: "benchmark" })
 *
 * NOTE: workflow scripts run with the nondeterministic clock/random builtins
 * disabled (Date.now() / Math.random() throw here). Anything that needs to "vary"
 * (labels, fallbacks, ordering) is varied by ARRAY INDEX, never by time/random.
 */

export const meta = {
  name: 'benchmark',
  description: 'Run the Treaty benchmark suite (tools/treaty-bench): Angular compiler vs Treaty-oxc vs Treaty-swc across vite/rspack/rsbuild/rslib/rolldown + ng; collect results/*.json, render the combined REPORT.md, and summarize where Treaty wins/loses (treaty-swc pending the swc backend)',
  phases: [
    { title: 'Measure', detail: 'one agent per axis authors/refreshes + runs compiler-bench.mjs and buildtool-bench.mjs, leaving JSON in tools/treaty-bench/results/' },
    { title: 'Report', detail: 'run tools/treaty-bench/run.mjs to collect results/*.json and write results/REPORT.md' },
    { title: 'Summary', detail: 'read REPORT.md + raw results and write an honest narrative of the comparison' },
  ],
}

const ROOT = 'd:/dev/treaty'
const BENCH = ROOT + '/tools/treaty-bench'
const RESULTS = BENCH + '/results'

// The backends being compared, and the build-tool axis. Pure literals (no clock/random).
const BACKENDS = ['angular', 'treaty-oxc', 'treaty-swc']
const BUILD_TOOLS = ['vite', 'rspack', 'rsbuild', 'rslib', 'rolldown', 'ng']

// ---------------------------------------------------------------------------
// Schemas
// ---------------------------------------------------------------------------

const MEASURE_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['suite', 'scriptPath', 'ran', 'resultFiles', 'rowCount', 'pendingBackends', 'summary'],
  properties: {
    suite: { type: 'string', description: 'which axis was measured: "compiler" or "buildtool"' },
    scriptPath: { type: 'string', description: 'absolute path to the measurement script that was authored/run' },
    ran: { type: 'boolean', description: 'did the measurement script actually run to completion and emit JSON?' },
    resultFiles: { type: 'array', description: 'absolute paths of results/*.json files this run produced/updated', items: { type: 'string' } },
    rowCount: { type: 'number', description: 'number of measured scenario rows written across this suite' },
    pendingBackends: { type: 'array', description: 'backends reported as pending/unmeasured for this suite (e.g. ["treaty-swc"])', items: { type: 'string' } },
    notes: { type: 'string', description: 'any caveats: tools not installed, scenarios skipped, why a backend is pending' },
    summary: { type: 'string', description: 'trailing stdout / file evidence proving the JSON was emitted' },
  },
}

const REPORT_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['ran', 'reportPath', 'collectedFiles', 'tableMarkdown', 'summary'],
  properties: {
    ran: { type: 'boolean', description: 'did `node tools/treaty-bench/run.mjs` run and write REPORT.md?' },
    reportPath: { type: 'string', description: 'absolute path to the written REPORT.md' },
    collectedFiles: { type: 'number', description: 'number of results/*.json files the runner collected' },
    tableMarkdown: { type: 'string', description: 'the combined comparison markdown the runner printed to stdout (verbatim, or its tables)' },
    cmd: { type: 'string', description: 'the exact command that was run' },
    summary: { type: 'string', description: 'trailing runner stdout proving it completed' },
  },
}

const SUMMARY_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['headline', 'wins', 'gaps', 'swcStatus', 'caveats'],
  properties: {
    headline: { type: 'string', description: 'one-line takeaway of the whole comparison' },
    wins: { type: 'array', description: 'scenarios/axes where Treaty-oxc clearly beats the Angular compiler, with the measured factor', items: { type: 'string' } },
    gaps: { type: 'array', description: 'scenarios where Treaty-oxc is slower than / on par with Angular, or where data is missing', items: { type: 'string' } },
    swcStatus: { type: 'string', description: 'the state of the Treaty-swc column (expected: pending until the swc backend ships)' },
    caveats: { type: 'string', description: 'honesty caveats: small sample counts, uninstalled tools, pending rows — do NOT over-claim' },
  },
}

// ---------------------------------------------------------------------------
// Phase 1 — Measure: one agent per axis. They author/refresh + RUN the scripts.
// ---------------------------------------------------------------------------

phase('Measure')

log(`benchmark: measuring ${BACKENDS.join(' vs ')} across ${BUILD_TOOLS.join(' / ')}`)
log(`results land in ${RESULTS}/*.json`)

const measureSpecs = [
  {
    suite: 'compiler',
    file: 'compiler-bench.mjs',
    axis: 'the @Component / partial-declaration -> Ivy COMPILE path (no bundler)',
    detail: `Measure the pure compile step: take a small fixed set of authoring inputs (a trivial @Component, a medium component with bindings/control-flow, and a partial-declaration module for the linker path) and time how long each backend takes to turn them into Ivy.
- Angular: use @angular/compiler (or @angular/compiler-cli) as the reference.
- Treaty-oxc: drive Treaty's Rust/OXC Ivy compiler (its NAPI binding / CLI as wired in apps/rust + libs/treaty-ivy/facade).
- Treaty-swc: the SWC backend is NOT implemented yet (migration/SWC-BACKEND-PLAN.md is "design / not yet implemented"); emit its cell as { status: "pending", note: "swc backend not yet built" } — do NOT fabricate a number.`,
  },
  {
    suite: 'buildtool',
    file: 'buildtool-bench.mjs',
    axis: 'the BUILD-TOOL integrations (vite / rspack / rsbuild / rslib / rolldown + ng)',
    detail: `Measure a small build/transform through each build-tool integration Treaty ships a plugin for: vite, rspack, rsbuild, rslib, rolldown — and Angular's own \`ng\` builder as the reference. Each row is { scenario, tool, results: { angular, "treaty-oxc", "treaty-swc" } }.
- If a given build tool is not installed in this repo, mark that tool's row(s) { status: "skipped", note: "<tool> not installed" } rather than failing the whole suite.
- Treaty-swc: emit { status: "pending", note: "swc backend not yet built" } for every row.`,
  },
]

const measured = (await parallel(measureSpecs.map((spec, idx) => () =>
  agent(
    `You are MEASURE agent #${idx + 1} for Treaty's benchmark suite. Treaty is a Rust/OXC Angular (Ivy) compiler. Your axis: ${spec.axis}.

Author (or refresh, if it already exists) and then RUN the measurement script at:
  ${BENCH}/${spec.file}

It must write one or more self-describing JSON result files into ${RESULTS}/ (create the dir if needed). The runner (tools/treaty-bench/run.mjs, already authored) reads every results/*.json and renders the combined report, so your JSON MUST match this shape exactly:

{
  "suite": "${spec.suite}",
  "unit": "ms",
  "lowerIsBetter": true,
  "rows": [
    {
      "scenario": "<the case being measured>",${spec.suite === 'buildtool' ? '\n      "tool": "<vite|rspack|rsbuild|rslib|rolldown|ng>",' : ''}
      "results": {
        "angular":    { "value": <number>, "unit": "ms", "samples": <n> },
        "treaty-oxc": { "value": <number>, "unit": "ms", "samples": <n> },
        "treaty-swc": { "status": "pending", "note": "swc backend not yet built" }
      }
    }
  ]
}

A per-backend cell may be a real measurement ({ value, unit?, samples? }) OR a non-numeric status ({ status: "pending"|"skipped"|"error", note? }). NEVER fabricate a number — if you cannot measure a backend (tool missing, backend not built), emit a status cell.

WHAT TO MEASURE: ${spec.detail}

RULES:
- The backends compared are: ${BACKENDS.join(', ')}.${spec.suite === 'buildtool' ? `\n- The build tools are: ${BUILD_TOOLS.join(', ')}.` : ''}
- Take multiple samples per cell and report the median (record the sample count in "samples"). Warm up before timing where a backend has cold-start cost.
- It is FINE for this to be a small, honest micro-benchmark. Do not over-engineer. Determinism and honest numbers matter more than scale.
- Do NOT touch tools/treaty-bench/run.mjs, package.json, the workflow, or migration/BENCHMARK.md (other agents own those). Only author ${spec.file} and write into results/.
- Do NOT verify emitted/compiled output with regex; if you assert correctness, parse or run it.

Report what you produced via the StructuredOutput tool (suite="${spec.suite}").`,
    { label: `measure:${spec.suite}`, phase: 'Measure', schema: MEASURE_SCHEMA }
  )
))).filter(Boolean)

const totalRows = measured.reduce((a, m) => a + (m && Number(m.rowCount) || 0), 0)
log(`measure: ${measured.length} suite(s) measured; ${totalRows} total scenario row(s) written`)

// ---------------------------------------------------------------------------
// Phase 2 — Report: run the runner to collect + render the combined comparison.
// ---------------------------------------------------------------------------

phase('Report')

const measuredDigest = measured.length
  ? measured.map((m, i) => `  ${i + 1}. [${m && m.suite}] ran=${m && m.ran}, rows=${m && m.rowCount}, files=${(m && m.resultFiles || []).length}, pending=${(m && m.pendingBackends || []).join('/') || 'none'}`).join('\n')
  : '  (no measurements this run)'

const report = await agent(
  `You are the REPORT phase of Treaty's benchmark suite. The measurement agents have written results/*.json into ${RESULTS}.

Measure-phase results this run:
${measuredDigest}

Run, from the repo root (${ROOT}):
  node tools/treaty-bench/run.mjs

This runner invokes compiler-bench.mjs + buildtool-bench.mjs (already authored), collects every results/*.json, prints the combined markdown comparison to stdout, and writes ${RESULTS}/REPORT.md. It TOLERATES missing/pending rows, so run it even if a suite produced partial data.

If you want to avoid re-running the (slow) measurement scripts because the JSON is already fresh from the Measure phase, run instead:
  node tools/treaty-bench/run.mjs --no-run

Capture the combined comparison markdown the runner printed (the backends table + the compiler suite table + the build-tool suite table). Confirm REPORT.md was written. Set ran=true only if the runner completed and REPORT.md exists; record how many results files it collected.

Do NOT edit any source or the runner; reporting only. Return via the StructuredOutput tool.`,
  { label: 'report', phase: 'Report', schema: REPORT_SCHEMA }
)

log(`report: ran=${report && report.ran}; collected ${report && report.collectedFiles} result file(s)`)

// ---------------------------------------------------------------------------
// Phase 3 — Summary: an honest narrative of the comparison.
// ---------------------------------------------------------------------------

phase('Summary')

const summary = await agent(
  `You are the SUMMARY phase of Treaty's benchmark suite. Read the combined report at ${RESULTS}/REPORT.md and the raw ${RESULTS}/*.json, then write an HONEST narrative of how the backends compare.

The comparison is: the Angular compiler (reference) vs Treaty-oxc (default Rust/OXC Ivy backend) vs Treaty-swc (second SWC backend), across the build tools ${BUILD_TOOLS.join(' / ')}.

Here is the combined comparison the runner produced this run:
${report && report.tableMarkdown ? report.tableMarkdown : '(no table captured — read REPORT.md directly)'}

Write:
  - headline: the one-line takeaway.
  - wins: where Treaty-oxc clearly beats the Angular compiler, citing the measured factor from the table (e.g. "compile hello-world: 5.8x faster"). Only list rows backed by real numbers for BOTH backends.
  - gaps: where Treaty-oxc is slower / on par, or where data is missing.
  - swcStatus: state the Treaty-swc column plainly — it is expected to be PENDING until the swc backend ships (migration/SWC-BACKEND-PLAN.md is design-only). Do NOT present pending as a result.
  - caveats: be honest — small sample counts, uninstalled build tools, micro-benchmark scope, pending rows. Do NOT over-claim.

Do NOT edit files; summarize only. Return via the StructuredOutput tool.`,
  { label: 'summary', phase: 'Summary', schema: SUMMARY_SCHEMA }
)

log(`summary: ${summary && summary.headline}`)

return {
  config: { root: ROOT, bench: BENCH, backends: BACKENDS, buildTools: BUILD_TOOLS },
  measured,
  report: report && { ran: report.ran, reportPath: report.reportPath, collectedFiles: report.collectedFiles },
  summary,
}
