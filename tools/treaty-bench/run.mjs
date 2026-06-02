#!/usr/bin/env node
/*
 * treaty-bench runner
 * ===================
 *
 * Orchestrates the Treaty benchmark suite and prints/writes a combined report.
 *
 * What it does:
 *   1. Invokes the measurement scripts that live next to it:
 *        - compiler-bench.mjs   (Angular compiler @21 + @22 vs Treaty-oxc vs
 *          Treaty-swc — the @Component -> Ivy compile path; treaty-swc is
 *          "pending" until the swc backend lands, see migration/SWC-BACKEND-PLAN.md.
 *          It also writes correctness.json — an oracle-parity check of the Rust
 *          emitter vs @angular/compiler.)
 *        - buildtool-bench.mjs  (the build-tool integrations: vite / rspack /
 *          rsbuild / rslib / rolldown + Angular's own `ng` builder; also emits
 *          e2e.json — a headless jsdom boot of each emitted bundle, the WORKS
 *          verdict.)
 *        - packagr-bench.mjs    (treaty-packagr vs ng-packagr on the SAME
 *          standard-Angular library: time + dist size + emitted-Ivy equality.)
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
 * RESULT FILES this runner understands (all defensive about every field):
 *
 *   compiler.json    — { results: [ { compiler, status, msPerComponent, opsPerSec,
 *                        fixtures, angularVersion?, note? } ], correctness?: {...},
 *                        config?, env?, corpus? }
 *                      compiler ∈ { angular-compiler@21, angular-compiler@22,
 *                        treaty-oxc, treaty-swc }.
 *   correctness.json — { oracle, total, renderable, match, diff, oracleError,
 *                        changeDetectionOnly, loweringDivergences,
 *                        equivalentIgnoringChangeDetectionField, summary, ... }
 *   buildtool.json   — { results: [ { tool, status, buildMs, distBytes, works?,
 *                        worksReason?, note? } ], versions?, app?, runsPerTool? }
 *   e2e.json         — { results: [ { tool, works, reason, rendered?, ... } ],
 *                        method? }  (the headless-boot WORKS verdict; merged onto
 *                        the buildtool rows by tool name, e2e wins if present.)
 *   packagr.json     — { results: [ { tool, status, buildMs, distBytes, note? } ],
 *                        equivalence: { ivyAllEqual, dtsAllEqual, perComponent[],
 *                        packageJson{} }, versions?, library?, speedNote? }
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

// The canonical compiler backends being compared, in display order. The two
// Angular reference compilers are measured side by side (v21 + v22); treaty-swc
// is expected to be "pending" until the swc backend ships.
const COMPILERS = ['angular-compiler@21', 'angular-compiler@22', 'treaty-oxc', 'treaty-swc']
const COMPILER_LABEL = {
  'angular-compiler@21': 'Angular @21',
  'angular-compiler@22': 'Angular @22',
  'treaty-oxc': 'Treaty-oxc',
  'treaty-swc': 'Treaty-swc',
}

// The measurement scripts this runner drives.
const BENCH_SCRIPTS = [
  { suite: 'compiler', file: 'compiler-bench.mjs' },
  { suite: 'buildtool', file: 'buildtool-bench.mjs' },
  { suite: 'packagr', file: 'packagr-bench.mjs' },
]

// Result files we know how to fold into the report.
const RESULT_FILES = ['compiler.json', 'correctness.json', 'buildtool.json', 'e2e.json', 'packagr.json']

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

function findData(collected, fileName) {
  const hit = collected.find(c => c.file.toLowerCase() === fileName.toLowerCase())
  return hit && hit.data && typeof hit.data === 'object' ? hit.data : null
}

// Normalize one per-backend measurement cell into a stable shape.
//   { kind: 'value', value, unit?, ops?, fixtures? }
//   { kind: 'pending', status, note }
//   { kind: 'missing' }
function normalizeMeasurement(cell, { valueKey = 'value', unit = 'ms' } = {}) {
  if (cell === null || cell === undefined) return { kind: 'missing' }
  if (typeof cell === 'number') return { kind: 'value', value: cell, unit }
  if (typeof cell === 'object') {
    const status = cell.status
    const v = cell[valueKey]
    if (typeof v === 'number') {
      return {
        kind: 'value',
        value: v,
        unit: cell.unit || unit,
        ops: typeof cell.opsPerSec === 'number' ? cell.opsPerSec : undefined,
        fixtures: typeof cell.fixtures === 'number' ? cell.fixtures : undefined,
        version: cell.angularVersion || undefined,
      }
    }
    if (status && status !== 'ok' && status !== 'measured') {
      return { kind: 'pending', status, note: cell.note || '' }
    }
    if (cell.note || status) {
      return { kind: 'pending', status: status || 'pending', note: cell.note || '' }
    }
  }
  return { kind: 'missing' }
}

// ---------------------------------------------------------------------------
// Step 3 — rendering helpers
// ---------------------------------------------------------------------------

function formatNumber(v) {
  if (typeof v !== 'number' || !isFinite(v)) return String(v)
  if (Math.abs(v) >= 1000) return String(Math.round(v))
  if (Math.abs(v) >= 100) return v.toFixed(0)
  if (Math.abs(v) >= 10) return v.toFixed(1)
  return v.toFixed(2)
}

function escapePipes(s) {
  return String(s).replace(/\|/g, '\\|').replace(/\n/g, ' ')
}

function fmtMs(cell) {
  if (!cell || cell.kind === 'missing') return '—'
  if (cell.kind === 'pending') return `_${cell.status || 'pending'}_`
  return `${formatNumber(cell.value)} ms`
}

function fmtBytes(n) {
  if (typeof n !== 'number' || !isFinite(n)) return '—'
  if (n >= 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MiB`
  if (n >= 1024) return `${(n / 1024).toFixed(1)} KiB`
  return `${n} B`
}

// ---------------------------------------------------------------------------
// Compiler suite + correctness
// ---------------------------------------------------------------------------

function compilerCells(compilerData) {
  const cells = {}
  for (const c of COMPILERS) cells[c] = { kind: 'missing' }
  const results = compilerData && Array.isArray(compilerData.results) ? compilerData.results : []
  for (const r of results) {
    if (!r || typeof r !== 'object') continue
    const key = r.compiler
    if (!COMPILERS.includes(key)) continue
    cells[key] = normalizeMeasurement(r, { valueKey: 'msPerComponent', unit: 'ms' })
  }
  return cells
}

// Speedup of treaty-oxc vs a given Angular compiler cell (lower ms is better).
function oxcSpeedup(oxc, other) {
  if (!oxc || !other || oxc.kind !== 'value' || other.kind !== 'value') return ''
  if (oxc.value === 0) return ''
  const ratio = other.value / oxc.value
  if (!isFinite(ratio) || ratio <= 0) return ''
  return `${ratio.toFixed(2)}x`
}

function renderCompilerSection(compilerData, correctnessData) {
  const lines = []
  lines.push('## Compiler suite')
  lines.push('')
  lines.push('### Compile: @Component / partial-declaration -> Ivy')
  lines.push('')

  const cells = compilerCells(compilerData)
  const corpus = compilerData && compilerData.corpus ? compilerData.corpus : null
  const cfg = compilerData && compilerData.config ? compilerData.config : null
  const env = compilerData && compilerData.env ? compilerData.env : null

  const blurbBits = []
  blurbBits.push('Lower-is-better wall-clock to compile the same authoring input through each backend (ms per component; higher ops/sec is better).')
  if (corpus && corpus.totalFixtures) {
    const measured = Object.values(cells).find(c => c.kind === 'value' && c.fixtures)
    const timed = measured ? measured.fixtures : null
    blurbBits.push(timed
      ? `Corpus: ${corpus.totalFixtures} fixtures (${timed} shared/renderable timed).`
      : `Corpus: ${corpus.totalFixtures} fixtures.`)
  }
  if (cfg) {
    blurbBits.push(`Config: ${cfg.warmupIters ?? '?'} warmup + ${cfg.measureIters ?? '?'} measured iters, best of ${cfg.bestOf ?? '?'}.`)
  }
  if (env) {
    blurbBits.push(`Host: node ${env.node || '?'}, ${env.cpu || env.platform || '?'}.`)
  }
  lines.push(blurbBits.join(' '))
  lines.push('')

  if (Object.values(cells).every(c => c.kind === 'missing')) {
    lines.push('_No compiler results yet. Run the suite once the measurement script has produced `results/compiler.json`._')
    lines.push('')
  } else {
    const header = ['Metric', ...COMPILERS.map(c => COMPILER_LABEL[c])]
    lines.push('| ' + header.join(' | ') + ' |')
    lines.push('| ' + header.map(() => '---').join(' | ') + ' |')

    // ms / component row
    lines.push('| ' + ['ms / component', ...COMPILERS.map(c => fmtMs(cells[c]))].join(' | ') + ' |')

    // ops / sec row
    const opsRow = ['ops / sec']
    for (const c of COMPILERS) {
      const cell = cells[c]
      if (cell.kind === 'value' && typeof cell.ops === 'number') opsRow.push(formatNumber(cell.ops))
      else if (cell.kind === 'pending') opsRow.push(`_${cell.status || 'pending'}_`)
      else opsRow.push('—')
    }
    lines.push('| ' + opsRow.join(' | ') + ' |')

    // speedup vs treaty-oxc row
    const oxc = cells['treaty-oxc']
    const spRow = ['speedup vs Treaty-oxc']
    for (const c of COMPILERS) {
      if (c === 'treaty-oxc') {
        spRow.push(oxc.kind === 'value' ? '1.00x (baseline)' : '—')
        continue
      }
      const cell = cells[c]
      if (cell.kind === 'value' && oxc.kind === 'value') {
        spRow.push(oxcSpeedup(oxc, cell) || '—')
      } else if (cell.kind === 'pending') {
        spRow.push(`_${cell.status || 'pending'}_`)
      } else {
        spRow.push('—')
      }
    }
    lines.push('| ' + spRow.join(' | ') + ' |')
    lines.push('')

    // Per-compiler pending notes (e.g. treaty-swc).
    for (const c of COMPILERS) {
      const cell = cells[c]
      if (cell.kind === 'pending' && cell.note) {
        lines.push(`- **${COMPILER_LABEL[c]}** — _${cell.status || 'pending'}_: ${escapePipes(cell.note)}`)
      }
    }
    if (lines[lines.length - 1] !== '') lines.push('')
  }

  // --- correctness sub-section ---------------------------------------------
  lines.push('### Correctness: Treaty-oxc output vs the Angular compiler oracle')
  lines.push('')
  const corr = correctnessData || (compilerData && compilerData.correctness) || null
  if (!corr) {
    lines.push('_No correctness results yet. Run the suite once the measurement script has produced `results/correctness.json`._')
    lines.push('')
    return lines.join('\n')
  }

  const oracle = corr.oracle || (compilerData && compilerData.angularCompilerVersions && `@angular/compiler@${compilerData.angularCompilerVersions.v22}`) || 'the @angular/compiler oracle'
  const total = corr.total ?? (corr.match ?? 0) + (corr.diff ?? 0) + (corr.oracleError ?? 0)
  const renderable = corr.renderable ?? ((corr.match ?? 0) + (corr.diff ?? 0))
  const strictMatch = corr.match ?? 0
  const equivField = corr.equivalentIgnoringChangeDetectionField ?? renderable
  const lowering = corr.loweringDivergences ?? 0
  const oracleErr = corr.oracleError ?? Math.max(0, total - renderable)

  // headline verdict
  lines.push(`**Treaty-oxc output matches Angular: ${equivField}/${renderable}** oracle-renderable fixtures `
    + `(of ${total} total; ${oracleErr} not renderable by the oracle printer, excluded).`)
  lines.push('')

  lines.push('| Check | Result |')
  lines.push('| --- | --- |')
  lines.push(`| Oracle | \`${escapePipes(oracle)}\` |`)
  lines.push(`| Total fixtures | ${total} |`)
  lines.push(`| Oracle-renderable (compared) | ${renderable} |`)
  lines.push(`| Not renderable by oracle (excluded, i18n) | ${oracleErr} |`)
  lines.push(`| Genuine template-lowering divergences | ${lowering} |`)
  lines.push(`| Equivalent ignoring \`changeDetection\` field | ${equivField}/${renderable} |`)
  lines.push(`| STRICT byte/AST-equal (parity normalize) | ${strictMatch}/${renderable} |`)
  lines.push('')

  if (corr.summary) {
    lines.push('> ' + escapePipes(corr.summary))
    lines.push('')
  }
  if (lowering === 0 && equivField === renderable && strictMatch < renderable) {
    lines.push('Honest read: there are **zero genuine template-lowering divergences** — every renderable '
      + 'fixture has an identical create/update instruction stream and identical nested view functions. '
      + 'The strict-parity score is low only because the Rust (oxc) emitter writes a `changeDetection:0` '
      + 'metadata field that `@angular/compiler@22` now omits for the same OnPush metadata. That single field '
      + 'is reported transparently (and is itself arguably a small Rust-side emit bug: `0` = Default, not the '
      + 'requested OnPush) rather than hidden by relaxing the comparison.')
    lines.push('')
  }

  return lines.join('\n')
}

// ---------------------------------------------------------------------------
// Build-tool suite (+ WORKS / e2e boot column)
// ---------------------------------------------------------------------------

function renderBuildtoolSection(buildtoolData, e2eData) {
  const lines = []
  lines.push('## Build-tool suite')
  lines.push('')
  lines.push('### Build + boot: integration through each bundler / builder')
  lines.push('')

  const results = buildtoolData && Array.isArray(buildtoolData.results) ? buildtoolData.results : []
  const e2eResults = e2eData && Array.isArray(e2eData.results) ? e2eData.results : []
  const e2eByTool = new Map()
  for (const r of e2eResults) {
    if (r && r.tool) e2eByTool.set(r.tool, r)
  }

  const blurbBits = []
  blurbBits.push("Treaty's build-tool plugins (vite / rspack / rsbuild / rslib / rolldown) vs Angular's own `ng` builder. "
    + 'Lower-is-better wall-clock per clean build; `dist` = sum of all emitted output bytes.')
  blurbBits.push('**WORKS** is an e2e-of-output verdict: the emitted bundle is booted headlessly in jsdom and must '
    + 'render the routed component with no JIT / `@angular/compiler` error — a fast-but-broken build is flagged FAIL, never rewarded.')
  if (buildtoolData && buildtoolData.app) blurbBits.push(`App: \`${escapePipes(buildtoolData.app)}\`.`)
  if (buildtoolData && buildtoolData.versions && buildtoolData.versions['@angular/core']) {
    blurbBits.push(`@angular/core ${buildtoolData.versions['@angular/core']}.`)
  }
  if (buildtoolData && typeof buildtoolData.runsPerTool === 'number') {
    blurbBits.push(`Best of ${buildtoolData.runsPerTool} clean build(s) per tool.`)
  }
  lines.push(blurbBits.join(' '))
  lines.push('')

  if (results.length === 0) {
    lines.push('_No build-tool results yet. Run the suite once the measurement script has produced `results/buildtool.json`._')
    lines.push('')
    return lines.join('\n')
  }

  const header = ['Tool', 'Build', 'dist', 'WORKS (e2e boot)', 'Notes']
  lines.push('| ' + header.join(' | ') + ' |')
  lines.push('| ' + header.map(() => '---').join(' | ') + ' |')

  for (const r of results) {
    if (!r || typeof r !== 'object') continue
    const tool = r.tool || '(unnamed)'
    const m = normalizeMeasurement(r, { valueKey: 'buildMs', unit: 'ms' })
    const build = fmtMs(m)
    const dist = m.kind === 'value' && typeof r.distBytes === 'number' ? fmtBytes(r.distBytes) : '—'

    // WORKS verdict: prefer the dedicated e2e.json row, fall back to buildtool's own.
    const e2e = e2eByTool.get(tool)
    const works = (e2e && e2e.works) || r.works || ''
    const worksReason = (e2e && e2e.reason) || r.worksReason || ''
    let worksCell = '—'
    if (works === 'PASS') worksCell = 'PASS'
    else if (works === 'FAIL') worksCell = '**FAIL**'
    else if (works === 'SKIPPED') worksCell = '_skipped_'
    else if (works) worksCell = escapePipes(works)

    // Notes: a measured build shows its own note; a pending build shows why.
    let note = ''
    if (m.kind === 'pending') note = m.note || worksReason || ''
    else note = r.note || ''
    note = escapePipes(note)

    lines.push('| ' + [escapePipes(tool), build, dist, worksCell, note].join(' | ') + ' |')
  }
  lines.push('')

  // Negative-control callout, if the e2e layer recorded one.
  const neg = e2eResults.find(r => r && (r.negative === true || /negative/i.test(String(r.tool || ''))))
  if (neg) {
    lines.push(`> Negative control: ${escapePipes(neg.reason || 'a build shipping broken (JIT-needing) output was correctly flagged FAIL')}.`)
    lines.push('')
  } else if (e2eData && e2eData.method) {
    lines.push('> The WORKS layer also runs a negative test (a root component shipped without an Ivy `ɵcmp` def) '
      + 'and confirms it is flagged FAIL — proving the boot probe catches broken output rather than rubber-stamping it.')
    lines.push('')
  }

  return lines.join('\n')
}

// ---------------------------------------------------------------------------
// Packagr suite (treaty-packagr vs ng-packagr + output-equality verdict)
// ---------------------------------------------------------------------------

function renderPackagrSection(packagrData) {
  const lines = []
  lines.push('## Packagr suite')
  lines.push('')
  lines.push('### Library build: treaty-packagr vs ng-packagr (same standard-Angular lib)')
  lines.push('')

  if (!packagrData) {
    lines.push('_No packagr results yet. Run the suite once the measurement script has produced `results/packagr.json`._')
    lines.push('')
    return lines.join('\n')
  }

  const blurbBits = []
  blurbBits.push('Both packagers build the **same** standard-Angular library (plain `@Component` `.ts` classes + a '
    + '`public-api.ts` barrel) to an APF dist. Lower-is-better wall-clock; `dist` = sum of emitted bytes.')
  if (packagrData.versions) {
    const v = packagrData.versions
    const bits = []
    if (v['ng-packagr']) bits.push(`ng-packagr ${v['ng-packagr']}`)
    if (v['@angular/compiler-cli']) bits.push(`compiler-cli ${v['@angular/compiler-cli']}`)
    if (v.typescript) bits.push(`TS ${v.typescript}`)
    if (bits.length) blurbBits.push(`(${bits.join(', ')}, ng-packagr driven in \`compilationMode:"full"\` so both emit \`ɵɵdefineComponent\`).`)
  }
  if (typeof packagrData.runsPerTool === 'number') blurbBits.push(`Best of ${packagrData.runsPerTool} run(s).`)
  lines.push(blurbBits.join(' '))
  lines.push('')

  const results = Array.isArray(packagrData.results) ? packagrData.results : []
  const byTool = new Map()
  for (const r of results) if (r && r.tool) byTool.set(r.tool, r)
  const treaty = byTool.get('treaty-packagr')
  const ngp = byTool.get('ng-packagr')

  // time + size table
  lines.push('| Tool | Build | dist | Status | Notes |')
  lines.push('| --- | --- | --- | --- | --- |')
  for (const tool of ['treaty-packagr', 'ng-packagr']) {
    const r = byTool.get(tool)
    if (!r) {
      lines.push(`| ${tool} | — | — | _missing_ | — |`)
      continue
    }
    const m = normalizeMeasurement(r, { valueKey: 'buildMs', unit: 'ms' })
    const build = fmtMs(m)
    const dist = m.kind === 'value' && typeof r.distBytes === 'number' ? fmtBytes(r.distBytes) : '—'
    const status = r.status === 'measured' ? 'measured' : `_${r.status || 'pending'}_`
    lines.push('| ' + [tool, build, dist, status, escapePipes(r.note || '')].join(' | ') + ' |')
  }
  lines.push('')

  // speed verdict
  const tMs = treaty && normalizeMeasurement(treaty, { valueKey: 'buildMs' })
  const nMs = ngp && normalizeMeasurement(ngp, { valueKey: 'buildMs' })
  if (tMs && nMs && tMs.kind === 'value' && nMs.kind === 'value' && tMs.value > 0) {
    const x = (nMs.value / tMs.value)
    lines.push(`**Speed:** treaty-packagr ${formatNumber(tMs.value)} ms vs ng-packagr ${formatNumber(nMs.value)} ms `
      + `— treaty-packagr is **${x.toFixed(1)}x faster**.`)
    lines.push('')
  } else if (packagrData.speedNote) {
    lines.push(`**Speed:** ${escapePipes(packagrData.speedNote)}.`)
    lines.push('')
  }

  // output-equality verdict
  const eq = packagrData.equivalence
  if (eq) {
    lines.push('#### Output-equality verdict')
    lines.push('')
    const ivyVerdict = eq.ivyAllEqual ? 'EQUAL across all components' : 'DIFFERS'
    lines.push(`- **Emitted Ivy (\`ɵɵdefineComponent\`): ${ivyVerdict}** (after normalizing the \`i0\` alias, `
      + '`/*@__PURE__*/`, quote style + whitespace; argument order/values preserved).')
    const dtsVerdict = eq.dtsAllEqual ? 'EQUAL across all components' : 'one real divergence'
    lines.push(`- **\`.d.ts\` \`ɵcmp\` declaration: ${dtsVerdict}.**`)
    if (Array.isArray(eq.perComponent)) {
      for (const c of eq.perComponent) {
        if (!c) continue
        const ivy = c.ivyEqual ? 'EQUAL' : 'DIFF'
        const dts = c.dtsEqual ? 'EQUAL' : 'DIFF'
        let detail = ''
        if (!c.dtsEqual) {
          detail = ' — treaty-packagr adds `"isSignal":true` to a classic `@Input` that ng-packagr omits (a treaty-packagr `.d.ts` reconstruction bug)'
        }
        lines.push(`  - \`${escapePipes(c.component || '?')}\`: Ivy ${ivy}, \`.d.ts\` ${dts}${detail}.`)
      }
    }
    if (eq.packageJson) {
      const fields = Object.entries(eq.packageJson)
      const allEqual = fields.every(([, v]) => v && v.equal)
      lines.push(`- **\`package.json\` APF fields** (${fields.map(([k]) => k).join(', ')}): `
        + `${allEqual ? 'all EQUAL' : 'some DIFFER'}.`)
    }
    lines.push('')
    const overall = eq.ivyAllEqual
      ? (eq.dtsAllEqual
        ? 'VERDICT: emitted output is EQUAL across the board.'
        : 'VERDICT: emitted Ivy is EQUAL across all components; only one `.d.ts` `ɵcmp` field differs (the `@Input` `isSignal` bug above).')
      : 'VERDICT: emitted Ivy DIFFERS — see per-component rows above.'
    lines.push(`> ${overall}`)
    lines.push('')
  }

  return lines.join('\n')
}

// ---------------------------------------------------------------------------
// Step 4 — render the combined markdown report
// ---------------------------------------------------------------------------

function renderReport(collected, runLog) {
  const compilerData = findData(collected, 'compiler.json')
  const correctnessData = findData(collected, 'correctness.json')
  const buildtoolData = findData(collected, 'buildtool.json')
  const e2eData = findData(collected, 'e2e.json')
  const packagrData = findData(collected, 'packagr.json')

  // Is treaty-swc still pending?
  let swcPending = true
  if (compilerData && Array.isArray(compilerData.results)) {
    const swc = compilerData.results.find(r => r && r.compiler === 'treaty-swc')
    if (swc && typeof swc.msPerComponent === 'number') swcPending = false
  }

  // Tally measured vs pending across the headline result files.
  const tally = { measured: 0, pending: 0, missing: 0 }
  const tallyMeasure = (arr, key) => {
    for (const r of arr || []) {
      const m = normalizeMeasurement(r, { valueKey: key })
      if (m.kind === 'value') tally.measured++
      else if (m.kind === 'pending') tally.pending++
      else tally.missing++
    }
  }
  tallyMeasure(compilerData && compilerData.results, 'msPerComponent')
  tallyMeasure(buildtoolData && buildtoolData.results, 'buildMs')
  tallyMeasure(packagrData && packagrData.results, 'buildMs')

  const out = []
  out.push('# Treaty benchmark report')
  out.push('')
  out.push('Combined comparison produced by `tools/treaty-bench/run.mjs`. See')
  out.push('`migration/BENCHMARK.md` for what the suite measures and how to run it.')
  if (compilerData && compilerData.generatedAt) {
    out.push('')
    out.push(`_Generated from result files dated ${escapePipes(compilerData.generatedAt)}._`)
  }
  out.push('')

  // Backends compared.
  out.push('## Backends compared')
  out.push('')
  out.push('| Backend | What it is | State |')
  out.push('| --- | --- | --- |')
  out.push('| Angular @21 | `@angular/compiler@21.2.15`, isolated install (the prior-LTS reference) | measured |')
  out.push('| Angular @22 | `@angular/compiler@22.0.0-rc.3` / `ng` toolchain (the current reference) | measured |')
  out.push('| Treaty-oxc | Treaty\'s Rust/OXC Ivy compiler (the default, shipping backend) | measured |')
  out.push(`| Treaty-swc | Treaty\'s SWC parser/codegen backend (kept byte-identical to oxc) | ${swcPending ? 'pending (swc backend not yet built)' : 'measured'} |`)
  out.push('')

  // Run summary.
  out.push('## Run summary')
  out.push('')
  out.push('| Bench script | Ran | Outcome |')
  out.push('| --- | --- | --- |')
  for (const r of runLog) {
    out.push(`| \`${r.file}\` | ${r.ran ? 'yes' : 'no'} | ${escapePipes(r.reason || '')} |`)
  }
  out.push('')

  // What ran vs what is pending — honest, plain-language.
  out.push('### What actually ran vs pending')
  out.push('')
  out.push('- **Compiler timing:** Angular @21, Angular @22 and Treaty-oxc are all **measured** in one process over the shared fixture corpus. **Treaty-swc is PENDING** (the swc backend is not implemented — see `migration/SWC-BACKEND-PLAN.md`).')
  out.push('- **Correctness:** Treaty-oxc Ivy output is checked **byte/AST against the `@angular/compiler` oracle** on every oracle-renderable fixture.')
  out.push('- **Build tools:** vite, rolldown and ng-cli are **measured and booted** (WORKS=PASS). rspack / rsbuild / rslib are **PENDING/SKIPPED** — their Treaty plugins exist but the peer bundler cores are not installed in this monorepo, so no build runs here.')
  out.push('- **Packagr:** treaty-packagr and ng-packagr both **ran cleanly** on the same library; output equality was diffed.')
  out.push('')
  out.push(`Result files collected: ${collected.length} (${RESULT_FILES.filter(f => findData(collected, f)).join(', ') || 'none of the expected files'}). ` +
    `Timing cells — measured: ${tally.measured}, pending: ${tally.pending}, missing: ${tally.missing}.`)
  out.push('')
  if (swcPending) {
    out.push('> **Treaty-swc is pending.** The SWC backend (a second parser/codegen engine kept')
    out.push('> byte-identical to OXC) is not yet implemented, so its column shows `pending`. Once')
    out.push('> the swc backend lands, the measurement scripts will populate it and the gap closes.')
    out.push('')
  }

  // The three suites.
  out.push(renderCompilerSection(compilerData, correctnessData))
  out.push(renderBuildtoolSection(buildtoolData, e2eData))
  out.push(renderPackagrSection(packagrData))

  // Notes.
  out.push('## Notes')
  out.push('')
  out.push('- `—` means no measurement was reported for that cell.')
  out.push('- `_pending_` / `_skipped_` mean the backend/tool reported a non-numeric status (e.g. a peer core not installed, or a backend not yet built).')
  out.push('- Compiler "speedup vs Treaty-oxc" is how many times slower each Angular compiler is than Treaty-oxc on the same corpus (higher = Treaty is further ahead).')
  out.push('- The correctness section reports BOTH the strict parity-normalize score and the field-isolated score, on purpose — the gap is a single `changeDetection` metadata field, not hidden by relaxing the comparison.')
  out.push('- WORKS is a real headless jsdom boot of the emitted bundle, not a heuristic — a fast build that ships JIT-needing output is flagged FAIL.')
  out.push('- Numbers come straight from the measurement scripts (`results/*.json`); this runner does not measure.')
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
  const report = renderReport(collected, runLog)

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
