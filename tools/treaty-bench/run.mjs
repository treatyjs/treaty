#!/usr/bin/env node
/*
 * treaty-bench runner
 * ===================
 *
 * Orchestrates the Treaty benchmark suite and prints/writes a combined report.
 *
 * What it does:
 *   1. Invokes the two measurement scripts that live next to it:
 *        - compiler-bench.mjs   (Angular compiler vs Treaty-oxc vs Treaty-swc — the
 *          @Component -> Ivy compile path; treaty-swc is "pending" until the swc
 *          backend lands, see migration/SWC-BACKEND-PLAN.md)
 *        - buildtool-bench.mjs   (the build-tool integrations: vite / rspack /
 *          rsbuild / rslib / rolldown + Angular's own `ng` builder)
 *      Those scripts are authored + run by the SIBLING measurement agents; this
 *      runner does NOT measure anything itself. It only drives + reports.
 *   2. Reads every tools/treaty-bench/results/*.json the measurement scripts left
 *      behind (each is a self-describing result file — see RESULT FILE SHAPE below).
 *   3. Prints a combined markdown comparison to stdout AND writes
 *      tools/treaty-bench/results/REPORT.md.
 *
 * It TOLERATES missing / pending rows end to end:
 *   - a bench script that is absent or exits non-zero does not abort the run;
 *   - a results dir with no JSON yet still produces a (mostly empty) report;
 *   - a backend with no measurement (e.g. treaty-swc before the swc backend ships)
 *     renders as `pending` rather than a crash or a fake number.
 *
 * RESULT FILE SHAPE (what the measurement scripts are expected to emit; this runner
 * is defensive about every field):
 *   {
 *     "suite": "compiler" | "buildtool",          // which bench produced it
 *     "kind":  "compile" | "build" | "dev" | ...,  // optional finer label
 *     "unit":  "ms" | "s" | "ops/s" | ...,         // optional; defaults to "ms"
 *     "lowerIsBetter": true,                       // optional; default true
 *     "generatedAt": "<iso string>",               // optional, informational only
 *     "rows": [
 *       {
 *         "scenario": "hello-world @Component -> Ivy",   // the case being measured
 *         "tool":     "ng" | "vite" | "rspack" | ...,    // optional (buildtool axis)
 *         "results": {                                   // one entry per backend
 *           "angular":    { "value": 1234, "unit": "ms", "samples": 5 },
 *           "treaty-oxc": { "value": 210,  "unit": "ms", "samples": 5 },
 *           "treaty-swc": { "status": "pending", "note": "swc backend not yet built" }
 *         }
 *       }
 *     ]
 *   }
 *
 *   A per-backend cell may instead be:
 *     - a bare number              (interpreted as { value })
 *     - { value, unit?, samples? } (a real measurement)
 *     - { status: "pending" | "skipped" | "error", note? }   (no number)
 *     - missing entirely           (rendered as `—`)
 *
 * Run with:   node tools/treaty-bench/run.mjs   (or `npm run bench` in this dir)
 *
 * Flags (all optional):
 *   --no-run        Do not invoke the bench scripts; just collect existing results.
 *   --results <dir> Override the results dir (default: ./results next to this file).
 *   --out <file>    Override the report path (default: <results>/REPORT.md).
 *   --quiet         Suppress the bench scripts' own stdout (still collects JSON).
 */

import { spawnSync } from 'node:child_process'
import { existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const HERE = dirname(fileURLToPath(import.meta.url))

// ---------------------------------------------------------------------------
// CLI args
// ---------------------------------------------------------------------------

function parseArgs(argv) {
  const out = { run: true, quiet: false, resultsDir: join(HERE, 'results'), reportPath: null }
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i]
    if (a === '--no-run') out.run = false
    else if (a === '--quiet') out.quiet = true
    else if (a === '--results') out.resultsDir = resolve(argv[++i] || out.resultsDir)
    else if (a === '--out') out.reportPath = resolve(argv[++i] || '')
  }
  if (!out.reportPath) out.reportPath = join(out.resultsDir, 'REPORT.md')
  return out
}

const ARGS = parseArgs(process.argv.slice(2))

// The canonical backends being compared, in display order. treaty-swc is expected
// to be "pending" until the swc backend ships (migration/SWC-BACKEND-PLAN.md).
const BACKENDS = ['angular', 'treaty-oxc', 'treaty-swc']
const BACKEND_LABEL = {
  angular: 'Angular compiler',
  'treaty-oxc': 'Treaty-oxc',
  'treaty-swc': 'Treaty-swc',
}

// The two measurement scripts this runner drives.
const BENCH_SCRIPTS = [
  { suite: 'compiler', file: 'compiler-bench.mjs' },
  { suite: 'buildtool', file: 'buildtool-bench.mjs' },
]

// ---------------------------------------------------------------------------
// Step 1 — invoke the measurement scripts (tolerate missing / failing ones)
// ---------------------------------------------------------------------------

function runBenchScripts() {
  const log = []
  for (const b of BENCH_SCRIPTS) {
    const scriptPath = join(HERE, b.file)
    if (!existsSync(scriptPath)) {
      log.push({ suite: b.suite, file: b.file, ran: false, reason: 'script not present yet' })
      console.error(`[treaty-bench] skip ${b.file}: not present yet (sibling agent will author it)`)
      continue
    }
    console.error(`[treaty-bench] running ${b.file} ...`)
    const res = spawnSync(process.execPath, [scriptPath], {
      cwd: HERE,
      stdio: ARGS.quiet ? ['ignore', 'ignore', 'inherit'] : 'inherit',
      env: { ...process.env, TREATY_BENCH_RESULTS: ARGS.resultsDir },
    })
    const ok = res.status === 0 && !res.error
    log.push({
      suite: b.suite,
      file: b.file,
      ran: true,
      ok,
      exitCode: res.status,
      reason: res.error ? String(res.error.message || res.error) : ok ? 'ok' : `exit ${res.status}`,
    })
    if (!ok) {
      console.error(`[treaty-bench] ${b.file} did not complete cleanly (${res.error ? res.error.message : 'exit ' + res.status}); continuing.`)
    }
  }
  return log
}

// ---------------------------------------------------------------------------
// Step 2 — collect results/*.json (defensive about every field)
// ---------------------------------------------------------------------------

function collectResults(dir) {
  const collected = []
  if (!existsSync(dir)) return collected
  let files = []
  try {
    files = readdirSync(dir).filter(f => f.toLowerCase().endsWith('.json')).sort()
  } catch {
    return collected
  }
  for (const f of files) {
    const full = join(dir, f)
    try {
      const raw = readFileSync(full, 'utf8')
      const parsed = JSON.parse(raw)
      collected.push({ file: f, data: parsed })
    } catch (e) {
      console.error(`[treaty-bench] skipping unreadable result ${f}: ${e.message}`)
      collected.push({ file: f, data: null, error: e.message })
    }
  }
  return collected
}

// Normalize one per-backend cell into { kind: 'value'|'pending'|'missing', ... }.
function normalizeCell(cell) {
  if (cell === null || cell === undefined) return { kind: 'missing' }
  if (typeof cell === 'number') return { kind: 'value', value: cell }
  if (typeof cell === 'object') {
    if (cell.status && cell.status !== 'ok' && cell.value === undefined) {
      return { kind: 'pending', status: cell.status, note: cell.note || '' }
    }
    if (typeof cell.value === 'number') {
      return { kind: 'value', value: cell.value, unit: cell.unit, samples: cell.samples }
    }
    if (cell.note || cell.status) {
      return { kind: 'pending', status: cell.status || 'pending', note: cell.note || '' }
    }
  }
  return { kind: 'missing' }
}

// Pull a flat list of normalized rows out of all collected result files for one suite.
function rowsForSuite(collected, suite) {
  const rows = []
  for (const { data } of collected) {
    if (!data || typeof data !== 'object') continue
    if (data.suite && data.suite !== suite) continue
    if (!data.suite && suite !== 'compiler') {
      // a suite-less file is assumed to be the compiler suite; don't double-count it.
      continue
    }
    const suiteUnit = data.unit || 'ms'
    const lowerIsBetter = data.lowerIsBetter !== false
    const list = Array.isArray(data.rows) ? data.rows : []
    for (const r of list) {
      const cells = {}
      const src = (r && typeof r.results === 'object' && r.results) || {}
      for (const be of BACKENDS) cells[be] = normalizeCell(src[be])
      rows.push({
        scenario: (r && (r.scenario || r.name)) || '(unnamed)',
        tool: (r && r.tool) || '',
        kind: (r && r.kind) || data.kind || '',
        unit: (r && r.unit) || suiteUnit,
        lowerIsBetter,
        cells,
      })
    }
  }
  return rows
}

// ---------------------------------------------------------------------------
// Step 3 — render the combined markdown report
// ---------------------------------------------------------------------------

function fmtCell(cell, unit) {
  if (!cell || cell.kind === 'missing') return '—'
  if (cell.kind === 'pending') {
    const status = cell.status || 'pending'
    return cell.note ? `_${status}_<br/>${escapePipes(cell.note)}` : `_${status}_`
  }
  const u = cell.unit || unit || ''
  const n = formatNumber(cell.value)
  const samples = cell.samples ? ` <sup>n=${cell.samples}</sup>` : ''
  return `${n}${u ? ' ' + u : ''}${samples}`
}

function formatNumber(v) {
  if (typeof v !== 'number' || !isFinite(v)) return String(v)
  if (Math.abs(v) >= 100) return String(Math.round(v))
  if (Math.abs(v) >= 10) return v.toFixed(1)
  return v.toFixed(2)
}

function escapePipes(s) {
  return String(s).replace(/\|/g, '\\|').replace(/\n/g, ' ')
}

// Speedup of treaty-oxc vs angular for a row, when both are real measurements.
function speedupVsAngular(cells, lowerIsBetter) {
  const a = cells.angular
  const t = cells['treaty-oxc']
  if (!a || !t || a.kind !== 'value' || t.kind !== 'value') return ''
  if (a.value === 0 || t.value === 0) return ''
  const ratio = lowerIsBetter ? a.value / t.value : t.value / a.value
  if (!isFinite(ratio) || ratio <= 0) return ''
  return `${ratio.toFixed(2)}x`
}

function renderSuiteTable(title, blurb, rows, opts = {}) {
  const lines = []
  lines.push(`### ${title}`)
  lines.push('')
  if (blurb) {
    lines.push(blurb)
    lines.push('')
  }
  if (rows.length === 0) {
    lines.push('_No results yet. Run the suite once the measurement script has produced JSON in `results/`._')
    lines.push('')
    return lines.join('\n')
  }
  const showTool = opts.showTool && rows.some(r => r.tool)
  const header = ['Scenario']
  if (showTool) header.push('Tool')
  for (const be of BACKENDS) header.push(BACKEND_LABEL[be])
  header.push('oxc vs ng')
  lines.push('| ' + header.join(' | ') + ' |')
  lines.push('| ' + header.map(() => '---').join(' | ') + ' |')
  for (const r of rows) {
    const cols = [escapePipes(r.scenario)]
    if (showTool) cols.push(escapePipes(r.tool || '—'))
    for (const be of BACKENDS) cols.push(fmtCell(r.cells[be], r.unit))
    cols.push(speedupVsAngular(r.cells, r.lowerIsBetter) || '—')
    lines.push('| ' + cols.join(' | ') + ' |')
  }
  lines.push('')
  return lines.join('\n')
}

function summarize(compilerRows, buildtoolRows, runLog, collected) {
  const allRows = [...compilerRows, ...buildtoolRows]
  let real = 0
  let pending = 0
  let missing = 0
  for (const r of allRows) {
    for (const be of BACKENDS) {
      const c = r.cells[be]
      if (!c || c.kind === 'missing') missing++
      else if (c.kind === 'pending') pending++
      else real++
    }
  }
  const swcPending = allRows.length > 0 && allRows.every(r => {
    const c = r.cells['treaty-swc']
    return !c || c.kind === 'missing' || c.kind === 'pending'
  })
  return { allRows, real, pending, missing, swcPending, collected }
}

function renderReport(compilerRows, buildtoolRows, runLog, collected) {
  const s = summarize(compilerRows, buildtoolRows, runLog, collected)
  const out = []
  out.push('# Treaty benchmark report')
  out.push('')
  out.push('Combined comparison produced by `tools/treaty-bench/run.mjs`. See')
  out.push('`migration/BENCHMARK.md` for what the suite measures and how to run it.')
  out.push('')

  out.push('## Backends compared')
  out.push('')
  out.push('| Backend | What it is | State |')
  out.push('| --- | --- | --- |')
  out.push('| Angular compiler | Angular\'s own `@angular/compiler` / `ng` toolchain (the reference) | available |')
  out.push('| Treaty-oxc | Treaty\'s Rust/OXC Ivy compiler (the default, shipping backend) | available |')
  out.push(`| Treaty-swc | Treaty\'s SWC parser/codegen backend (byte-identical to oxc) | ${s.swcPending ? 'pending (swc backend not yet built)' : 'available'} |`)
  out.push('')

  out.push('## Run summary')
  out.push('')
  out.push('| Bench script | Ran | Outcome |')
  out.push('| --- | --- | --- |')
  for (const r of runLog) {
    out.push(`| \`${r.file}\` | ${r.ran ? 'yes' : 'no'} | ${escapePipes(r.reason || '')} |`)
  }
  out.push('')
  out.push(`Result files collected: ${collected.length}. ` +
    `Cells — measured: ${s.real}, pending: ${s.pending}, missing: ${s.missing}.`)
  out.push('')
  if (s.swcPending) {
    out.push('> **Treaty-swc is pending.** The SWC backend (a second parser/codegen engine kept')
    out.push('> byte-identical to OXC) is not yet implemented, so its column shows `pending`. Once')
    out.push('> the swc backend lands, the measurement scripts will populate it and the gap closes.')
    out.push('')
  }

  out.push('## Compiler suite')
  out.push('')
  out.push(renderSuiteTable(
    'Compile: @Component / partial-declaration -> Ivy',
    'Lower-is-better wall-clock to compile the same authoring input through each backend.',
    compilerRows,
    { showTool: false },
  ))

  out.push('## Build-tool suite')
  out.push('')
  out.push(renderSuiteTable(
    'Build: integration through each bundler / builder',
    'Treaty\'s build-tool plugins (vite / rspack / rsbuild / rslib / rolldown) vs Angular\'s own `ng` builder. Lower-is-better wall-clock per scenario.',
    buildtoolRows,
    { showTool: true },
  ))

  out.push('## Notes')
  out.push('')
  out.push('- `—` means no measurement was reported for that cell.')
  out.push('- `_pending_` / `_skipped_` / `_error_` mean the backend reported a non-numeric status.')
  out.push('- `oxc vs ng` is the speedup of Treaty-oxc over the Angular compiler for that row (higher is better), only shown when both are real measurements.')
  out.push('- Numbers come straight from the measurement scripts; this runner does not measure.')
  out.push('')

  return out.join('\n')
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

function main() {
  if (!existsSync(ARGS.resultsDir)) {
    mkdirSync(ARGS.resultsDir, { recursive: true })
  }

  const runLog = ARGS.run
    ? runBenchScripts()
    : BENCH_SCRIPTS.map(b => ({ suite: b.suite, file: b.file, ran: false, reason: '--no-run' }))

  const collected = collectResults(ARGS.resultsDir)
  const compilerRows = rowsForSuite(collected, 'compiler')
  const buildtoolRows = rowsForSuite(collected, 'buildtool')

  const report = renderReport(compilerRows, buildtoolRows, runLog, collected)

  // stdout: the combined markdown comparison.
  process.stdout.write(report + '\n')

  // and persist it.
  try {
    writeFileSync(ARGS.reportPath, report + '\n', 'utf8')
    console.error(`[treaty-bench] wrote ${ARGS.reportPath}`)
  } catch (e) {
    console.error(`[treaty-bench] could not write report: ${e.message}`)
  }
}

main()
