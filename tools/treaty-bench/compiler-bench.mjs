// @ts-check
/**
 * Treaty compiler micro-benchmark — "how fast does each backend lower a
 * component template to Ivy?"
 *
 * The SHARED CORPUS and the Angular oracle are reused verbatim from the parity
 * harness at libs/treaty-ivy/facade/parity/parity.mjs — this file imports its
 * exported `FIXTURES` (the (template, selector, className) tuples),
 * `compileWithOracle` (@angular/compiler: parseTemplate + compileComponentFromMetadata,
 * the exact thing parity.mjs measures), and `loadRustAddon` (the @treaty/authoring-node
 * NAPI addon — the oxc backend, the default). Nothing here re-implements the
 * compile path; the bench only times those existing functions over the corpus.
 *
 * Compilers measured:
 *   - "angular-compiler" : @angular/compiler oracle (compileWithOracle).        MEASURED
 *   - "treaty-oxc"       : @treaty/authoring-node compileComponent (oxc backend). MEASURED
 *   - "treaty-swc"       : the Treaty swc backend — NOT YET IMPLEMENTED
 *                          (see migration/SWC-BACKEND-PLAN.md). Emitted as a
 *                          status:"pending" row; will be measured once the swc
 *                          Cargo feature + emitter_swc.rs land and
 *                          @treaty/authoring-node exposes the swc-backed path.
 *
 * Timing: real performance.now() loop. We DISCARD warmup iterations (JIT warm-up
 * + first-call native-bridge cost) and time many warm iterations over ALL
 * fixtures, then report ms/component and ops/sec. A standalone .mjs benchmark
 * may freely use performance.now() (the workflow-script clock/random restriction
 * does not apply here).
 *
 * Run:  node tools/treaty-bench/compiler-bench.mjs
 * Out:  prints a table + writes tools/treaty-bench/results/compiler.json
 */

import { fileURLToPath } from 'node:url';
import { performance } from 'node:perf_hooks';
import path from 'node:path';
import fs from 'node:fs';
import os from 'node:os';

import {
  FIXTURES,
  compileWithOracle,
  loadRustAddon,
} from '../../libs/treaty-ivy/facade/parity/parity.mjs';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

// ---------------------------------------------------------------------------
// Iteration budget. "Iteration" = one full sweep compiling EVERY usable fixture
// once. ms/component is normalized by the fixture count so the three compilers
// are comparable even if they end up running over different usable-fixture sets.
// ---------------------------------------------------------------------------
const WARMUP_ITERS = 200; // discarded — JIT + native-bridge warm-up
const MEASURE_ITERS = 2000; // timed warm iterations

// ---------------------------------------------------------------------------
// Determine which fixtures each compiler can actually lower. The Angular oracle
// printer THROWS on the i18n fixtures ("only important for i18n") and on any
// shape it cannot render — those are not a fair timing target, so we restrict
// the timed corpus to fixtures BOTH measured compilers can compile, keeping the
// ms/component apples-to-apples (identical work per iteration per compiler).
// ---------------------------------------------------------------------------
function usableFixtures(compileOne) {
  const ok = [];
  const skipped = [];
  for (const fx of FIXTURES) {
    try {
      const out = compileOne(fx);
      if (typeof out !== 'string' || out.length === 0) {
        skipped.push({ id: fx.id, reason: 'empty output' });
        continue;
      }
      ok.push(fx);
    } catch (err) {
      skipped.push({ id: fx.id, reason: (err && err.message) || String(err) });
    }
  }
  return { ok, skipped };
}

// ---------------------------------------------------------------------------
// The two measurable compile closures, each: fixture -> emitted Ivy JS string.
// ---------------------------------------------------------------------------
const compileAngular = (fx) => compileWithOracle(fx);

const rust = loadRustAddon();
const compileTreatyOxc = rust.ok
  ? (fx) => {
      const r = rust.compile(fx.template, fx.selector, fx.className);
      if (r && r.errors && r.errors.length) {
        throw new Error('rust diagnostics: ' + r.errors.join('; '));
      }
      return r ? r.code : '';
    }
  : null;

// ---------------------------------------------------------------------------
// The core timed loop. Runs `iters` full sweeps over `fixtures`, compiling each
// once per sweep. Returns total wall time (ms) and component count. We touch the
// output length to keep the optimizer from dead-code-eliminating the compile.
// ---------------------------------------------------------------------------
function timeSweeps(compileOne, fixtures, iters) {
  let sink = 0;
  const start = performance.now();
  for (let i = 0; i < iters; i++) {
    for (let j = 0; j < fixtures.length; j++) {
      const code = compileOne(fixtures[j]);
      sink += code.length;
    }
  }
  const elapsedMs = performance.now() - start;
  return { elapsedMs, components: iters * fixtures.length, sink };
}

function measure(name, compileOne, restrictTo) {
  if (!compileOne) {
    return { compiler: name, status: 'failed', note: 'compiler unavailable' };
  }

  let { ok, skipped } = usableFixtures(compileOne);
  // If a shared fixture set is supplied (the intersection both measured
  // compilers can lower), time exactly that set so ms/component is over the
  // IDENTICAL templates for every compiler — apples-to-apples.
  if (restrictTo) {
    const keep = new Set(restrictTo);
    ok = ok.filter((fx) => keep.has(fx.id));
  }
  if (ok.length === 0) {
    return {
      compiler: name,
      status: 'failed',
      note:
        'no fixtures compiled; skipped: ' +
        skipped.map((s) => `${s.id} (${s.reason})`).join(', '),
    };
  }

  // Warm up (discarded).
  timeSweeps(compileOne, ok, WARMUP_ITERS);

  // Measured. Run 3 trials and keep the best (lowest total time) to reduce GC /
  // scheduler noise — standard micro-benchmark practice.
  let best = Infinity;
  let bestRun = null;
  for (let trial = 0; trial < 3; trial++) {
    const run = timeSweeps(compileOne, ok, MEASURE_ITERS);
    if (run.elapsedMs < best) {
      best = run.elapsedMs;
      bestRun = run;
    }
  }

  const msPerComponent = bestRun.elapsedMs / bestRun.components;
  const opsPerSec = 1000 / msPerComponent;

  return {
    compiler: name,
    status: 'measured',
    fixtures: ok.length,
    components: bestRun.components,
    iterations: MEASURE_ITERS,
    warmup: WARMUP_ITERS,
    totalMs: round(bestRun.elapsedMs, 3),
    msPerComponent: round(msPerComponent, 6),
    opsPerSec: round(opsPerSec, 1),
    skipped: skipped.map((s) => s.id),
    note: restrictTo
      ? `${ok.length} shared fixtures timed (intersection both compilers can lower)` +
        (skipped.length
          ? `; this compiler also cannot render: ${skipped.map((s) => s.id).join(', ')}`
          : '')
      : skipped.length > 0
        ? `${ok.length} fixtures timed; skipped: ${skipped.map((s) => s.id).join(', ')}`
        : `${ok.length} fixtures timed`,
  };
}

function round(n, dp) {
  const f = Math.pow(10, dp);
  return Math.round(n * f) / f;
}

// ---------------------------------------------------------------------------
// Run.
// ---------------------------------------------------------------------------
function main() {
  console.log('Treaty compiler micro-benchmark — Ivy lowering throughput');
  console.log('='.repeat(72));
  console.log(`corpus fixtures (shared, from parity.mjs): ${FIXTURES.length}`);
  console.log(`warmup iters/sweep: ${WARMUP_ITERS}   measured iters/sweep: ${MEASURE_ITERS} (best of 3)`);
  console.log(`node ${process.version}  on  ${os.cpus()[0]?.model?.trim() || 'unknown CPU'}`);
  console.log(
    `treaty-oxc addon: ${rust.ok ? 'LOADED from ' + path.basename(rust.from) : 'NOT AVAILABLE'}`,
  );
  console.log('');

  const results = [];

  // Build the SHARED timed set = fixtures BOTH measured compilers can lower, so
  // every compiler is timed over the identical templates (the Angular oracle
  // printer throws on the i18n fixtures; the intersection drops those for both).
  const angOk = new Set(usableFixtures(compileAngular).ok.map((f) => f.id));
  const oxcOk = compileTreatyOxc
    ? new Set(usableFixtures(compileTreatyOxc).ok.map((f) => f.id))
    : new Set();
  const shared = FIXTURES.map((f) => f.id).filter(
    (id) => angOk.has(id) && (!compileTreatyOxc || oxcOk.has(id)),
  );
  console.log(`shared timed fixtures (intersection): ${shared.length}/${FIXTURES.length}`);
  console.log('');

  // angular-compiler — @angular/compiler oracle (measured today).
  console.log('measuring angular-compiler ...');
  results.push(measure('angular-compiler', compileAngular, shared));

  // treaty-oxc — the Rust NAPI addon, oxc backend (measured today).
  console.log('measuring treaty-oxc ...');
  results.push(measure('treaty-oxc', compileTreatyOxc, shared));

  // treaty-swc — not yet implemented. Emit a pending row per the task + SWC plan.
  results.push({
    compiler: 'treaty-swc',
    status: 'pending',
    note:
      'swc backend NOT YET IMPLEMENTED — see migration/SWC-BACKEND-PLAN.md ' +
      '(Cargo feature `swc` + libs/treaty-ivy/core/src/output/emitter_swc.rs, ' +
      'phase 2). Will be measured once the swc feature lands and ' +
      '@treaty/authoring-node exposes the swc-backed compile path.',
  });

  // -----------------------------------------------------------------------
  // Table.
  // -----------------------------------------------------------------------
  console.log('');
  console.log('='.repeat(72));
  const measured = results.filter((r) => r.status === 'measured');
  const fastest =
    measured.length > 0
      ? measured.reduce((a, b) => (a.opsPerSec >= b.opsPerSec ? a : b))
      : null;

  const cols = [
    pad('compiler', 18),
    pad('status', 10),
    pad('fixtures', 9),
    pad('ms/component', 14),
    pad('ops/sec', 14),
    'speedup',
  ].join('');
  console.log(cols);
  console.log('-'.repeat(72));
  for (const r of results) {
    let speedup = '';
    if (r.status === 'measured' && fastest) {
      const x = r.opsPerSec / fastest.opsPerSec;
      speedup = x === 1 ? '1.00x (fastest)' : x.toFixed(2) + 'x';
    }
    console.log(
      [
        pad(r.compiler, 18),
        pad(r.status, 10),
        pad(r.status === 'measured' ? String(r.fixtures) : '-', 9),
        pad(r.status === 'measured' ? r.msPerComponent.toFixed(6) : '-', 14),
        pad(
          r.status === 'measured' ? r.opsPerSec.toLocaleString('en-US') : '-',
          14,
        ),
        speedup,
      ].join(''),
    );
  }
  console.log('-'.repeat(72));
  for (const r of results) {
    if (r.note) console.log(`  ${r.compiler}: ${r.note}`);
  }

  // -----------------------------------------------------------------------
  // JSON artifact.
  // -----------------------------------------------------------------------
  const outDir = path.join(__dirname, 'results');
  fs.mkdirSync(outDir, { recursive: true });
  const outFile = path.join(outDir, 'compiler.json');
  const payload = {
    benchmark: 'compiler-ivy-lowering',
    generatedAt: new Date().toISOString(),
    corpus: {
      source: 'libs/treaty-ivy/facade/parity/parity.mjs#FIXTURES',
      totalFixtures: FIXTURES.length,
    },
    env: {
      node: process.version,
      platform: process.platform,
      arch: process.arch,
      cpu: os.cpus()[0]?.model?.trim() || 'unknown',
    },
    config: { warmupIters: WARMUP_ITERS, measureIters: MEASURE_ITERS, bestOf: 3 },
    angularCompilerVersionNote:
      'angular-compiler row measured with the @angular/compiler resolved at repo node_modules',
    results,
  };
  fs.writeFileSync(outFile, JSON.stringify(payload, null, 2) + '\n', 'utf8');
  console.log('');
  console.log(`wrote ${path.relative(path.resolve(__dirname, '..', '..'), outFile)}`);

  return results;
}

function pad(s, n) {
  s = String(s);
  return s.length >= n ? s + ' ' : s + ' '.repeat(n - s.length);
}

main();
