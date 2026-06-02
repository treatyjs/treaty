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

import { fileURLToPath, pathToFileURL } from 'node:url';
import { performance } from 'node:perf_hooks';
import { createRequire } from 'node:module';
import { execSync } from 'node:child_process';
import path from 'node:path';
import fs from 'node:fs';
import os from 'node:os';

import {
  FIXTURES,
  compileWithOracle,
  loadRustAddon,
  normalize,
} from '../../libs/treaty-ivy/facade/parity/parity.mjs';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const require = createRequire(import.meta.url);
const repoRoot = path.resolve(__dirname, '..', '..');

// ---------------------------------------------------------------------------
// Resolve the @angular/compiler version the repo currently builds against
// (the "v22" oracle row). Read straight from the installed package.json so the
// row is labelled with the REAL resolved version, never a hard-coded guess.
// ---------------------------------------------------------------------------
function resolvedAngularVersion() {
  try {
    const pkg = require('@angular/compiler/package.json');
    return pkg.version;
  } catch {
    return 'unknown';
  }
}

// ---------------------------------------------------------------------------
// Second, ISOLATED @angular/compiler version, installed into a temp prefix so
// BOTH versions can be timed in one process. We never touch the repo's own
// node_modules: the older compiler lives under os.tmpdir()/treaty-bench-ng<ver>
// and is loaded by its absolute fesm2022 path via a file:// dynamic import.
//
// Returns { ok, ng, version, from } or { ok:false, reason }. The "reason" on
// failure is the REAL installer/loader error so the row can be marked pending
// with a truthful explanation (never faked).
// ---------------------------------------------------------------------------
const ISOLATED_NG_VERSION = '21.2.15';

function loadIsolatedAngular(version) {
  const prefix = path.join(os.tmpdir(), `treaty-bench-ng${version}`);
  const compilerEntry = path.join(
    prefix,
    'node_modules',
    '@angular',
    'compiler',
    'fesm2022',
    'compiler.mjs',
  );
  try {
    // Install only if not already present (idempotent across re-runs).
    if (!fs.existsSync(compilerEntry)) {
      fs.mkdirSync(prefix, { recursive: true });
      // Minimal package.json so npm has a project to install into.
      const pj = path.join(prefix, 'package.json');
      if (!fs.existsSync(pj)) {
        fs.writeFileSync(
          pj,
          JSON.stringify({ name: 'treaty-bench-isolated-ng', private: true }, null, 2),
        );
      }
      // @angular/compiler@<version> only runtime-depends on tslib; install both
      // into the isolated prefix. --no-save keeps it out of any lockfile.
      // NOTE: on Windows `npm` is a `.cmd` shim — execFileSync without a shell
      // throws EINVAL spawning it, so run the whole install line through the
      // shell as a single string (quoting the prefix path for spaces). Inputs
      // here are this file's own constants, not user data.
      const isWin = process.platform === 'win32';
      const cmd =
        `npm install --prefix "${prefix}" ` +
        `@angular/compiler@${version} tslib@^2.3.0 ` +
        `--no-save --no-audit --no-fund --silent`;
      execSync(cmd, {
        stdio: ['ignore', 'pipe', 'pipe'],
        timeout: 180000,
        shell: isWin ? true : '/bin/sh',
      });
    }
    if (!fs.existsSync(compilerEntry)) {
      return { ok: false, reason: `install completed but ${compilerEntry} not found` };
    }
    return { ok: true, entry: compilerEntry };
  } catch (err) {
    const msg = (err && (err.stderr?.toString() || err.message)) || String(err);
    return { ok: false, reason: msg.split('\n').slice(0, 3).join(' ').trim() };
  }
}

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
// The measurable compile closures, each: fixture -> emitted Ivy JS string.
//   compileAngular     -> the repo's @angular/compiler (v22) oracle
//   compileAngularV21  -> the isolated @angular/compiler@21 oracle (set in main)
//   compileTreatyOxc   -> the Rust NAPI addon (oxc backend)
// All three drive the IDENTICAL parity printer + minimal metadata, so the only
// thing differing per row is the compiler doing the lowering.
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

// ===========================================================================
// CORRECTNESS — prove the Rust (oxc) output MATCHES the Angular oracle, not just
// that it is fast. Reuses the EXACT oracle + normalize() from parity.mjs (the
// same comparison the parity harness reports), so this bench's correctness
// verdict is byte-for-byte the same check, never a re-implementation.
//
// For each fixture: compile via @angular/compiler (oracle) AND via the Rust
// addon, normalize() BOTH, and compare. We classify the outcome so the report is
// honest and informative rather than a bare pass/fail:
//   - "match"        : normalized oracle === normalized rust (byte/AST-equivalent)
//   - "diff"         : a genuine template-lowering divergence
//   - "oracle-error" : the parity printer cannot render this fixture (the i18n
//                      `only important for i18n` throw) — excluded from scoring,
//                      same as the parity harness treats them.
// For every non-matching renderable fixture we also record WHICH normalized field
// first diverges, so a single-field metadata delta (e.g. the v22 emit dropping a
// `changeDetection` field the Rust side still writes) is visible and not hidden.
// ===========================================================================
function firstDivergence(a, b) {
  const n = Math.min(a.length, b.length);
  for (let i = 0; i < n; i++) {
    if (a[i] !== b[i]) {
      const s = Math.max(0, i - 24);
      return { index: i, oracle: a.slice(s, i + 28), rust: b.slice(s, i + 28) };
    }
  }
  if (a.length !== b.length) {
    return {
      index: n,
      oracle: a.slice(Math.max(0, n - 24)),
      rust: b.slice(Math.max(0, n - 24)),
    };
  }
  return null;
}

// The Rust emitter writes `changeDetection:0` into the definition object; the
// v22 @angular/compiler oracle OMITS the field entirely for the same metadata
// (an emit-default change in v22). Stripping that single field from BOTH sides
// (NOT from normalize() — only inside this classifier, transparently) tells us
// whether a fixture's ONLY divergence is that one metadata field versus a real
// template-lowering difference. We report both verdicts so nothing is masked.
const stripChangeDetectionField = (s) => s.replace(/,?changeDetection:\d+/g, '');

function runCorrectness() {
  const perFixture = [];
  let match = 0; // normalized oracle === normalized rust (strict, parity's check)
  let diff = 0; // any non-match (strict)
  let oracleError = 0; // oracle printer cannot render (i18n)
  let changeDetectionOnly = 0; // diff whose SOLE divergence is the changeDetection field

  for (const fx of FIXTURES) {
    let oracleCode;
    try {
      oracleCode = compileWithOracle(fx);
    } catch (err) {
      oracleError++;
      perFixture.push({
        id: fx.id,
        result: 'oracle-error',
        reason: (err && err.message) || String(err),
      });
      continue;
    }

    if (!compileTreatyOxc) {
      perFixture.push({ id: fx.id, result: 'rust-unavailable' });
      continue;
    }

    let rustResult;
    try {
      rustResult = rust.compile(fx.template, fx.selector, fx.className);
    } catch (err) {
      diff++;
      perFixture.push({
        id: fx.id,
        result: 'diff',
        reason: 'rust threw: ' + ((err && err.message) || String(err)),
      });
      continue;
    }
    const rustDiag =
      rustResult.errors && rustResult.errors.length ? rustResult.errors : null;

    const nOracle = normalize(oracleCode);
    const nRust = normalize(rustResult.code);
    const d = firstDivergence(nOracle, nRust);
    if (d === null) {
      match++;
      perFixture.push({ id: fx.id, result: 'match', rustDiagnostics: rustDiag });
    } else {
      diff++;
      // Is the changeDetection metadata field the ONLY thing that differs?
      const cdOnly =
        stripChangeDetectionField(nOracle) === stripChangeDetectionField(nRust);
      if (cdOnly) changeDetectionOnly++;
      perFixture.push({
        id: fx.id,
        result: 'diff',
        category: cdOnly ? 'changeDetection-field-only' : 'lowering-divergence',
        rustDiagnostics: rustDiag,
        firstDivergenceIndex: d.index,
        oracleNear: d.oracle,
        rustNear: d.rust,
      });
    }
  }

  const renderable = FIXTURES.length - oracleError;
  const realDiff = diff - changeDetectionOnly;
  const equivalentIgnoringCd = match + changeDetectionOnly;
  return {
    total: FIXTURES.length,
    renderable,
    match, // strict byte/AST equivalence under parity's normalize()
    diff,
    oracleError,
    changeDetectionOnly,
    loweringDivergences: realDiff,
    equivalentIgnoringChangeDetectionField: equivalentIgnoringCd,
    summary:
      `${match}/${renderable} renderable fixtures STRICTLY byte/AST-equivalent under parity normalize(); ` +
      `${equivalentIgnoringCd}/${renderable} are equivalent except the Rust emitter writes a ` +
      `\`changeDetection:0\` field the v22 oracle now omits (single metadata field, instruction ` +
      `streams identical); ${realDiff} genuine lowering divergence(s); ` +
      `${oracleError} not renderable by the oracle printer (i18n)`,
    diffFixtures: perFixture.filter((f) => f.result === 'diff').map((f) => f.id),
    loweringDivergenceFixtures: perFixture
      .filter((f) => f.result === 'diff' && f.category === 'lowering-divergence')
      .map((f) => f.id),
    perFixture,
  };
}

// ---------------------------------------------------------------------------
// Run.
// ---------------------------------------------------------------------------
async function main() {
  console.log('Treaty compiler micro-benchmark — Ivy lowering throughput');
  console.log('='.repeat(72));
  console.log(`corpus fixtures (shared, from parity.mjs): ${FIXTURES.length}`);
  console.log(`warmup iters/sweep: ${WARMUP_ITERS}   measured iters/sweep: ${MEASURE_ITERS} (best of 3)`);
  console.log(`node ${process.version}  on  ${os.cpus()[0]?.model?.trim() || 'unknown CPU'}`);
  console.log(
    `treaty-oxc addon: ${rust.ok ? 'LOADED from ' + path.basename(rust.from) : 'NOT AVAILABLE'}`,
  );

  const v22Version = resolvedAngularVersion();
  console.log(`angular-compiler (repo): @angular/compiler@${v22Version}`);

  // Load the isolated older @angular/compiler so v21 and v22 are timed in ONE
  // run. If install/load fails the v21 row becomes status:"pending" with the
  // REAL reason (never faked).
  console.log(`isolating @angular/compiler@${ISOLATED_NG_VERSION} ...`);
  const isolated = loadIsolatedAngular(ISOLATED_NG_VERSION);
  let ng21 = null;
  let v21LoadReason = isolated.ok ? null : isolated.reason;
  if (isolated.ok) {
    try {
      ng21 = await import(pathToFileURL(isolated.entry).href);
      const got = ng21.VERSION ? ng21.VERSION.full : ISOLATED_NG_VERSION;
      console.log(`  loaded @angular/compiler@${got} from ${isolated.entry}`);
    } catch (err) {
      ng21 = null;
      v21LoadReason = 'dynamic import failed: ' + ((err && err.message) || String(err));
    }
  }
  if (!ng21) console.log(`  v21 NOT AVAILABLE: ${v21LoadReason}`);
  // v21 oracle closure: identical printer/metadata path, driven by the v21 module.
  const compileAngularV21 = ng21 ? (fx) => compileWithOracle(fx, ng21) : null;
  console.log('');

  const results = [];

  // -----------------------------------------------------------------------
  // CORRECTNESS first — does Treaty-oxc's emitted Ivy MATCH the Angular oracle?
  // -----------------------------------------------------------------------
  console.log('checking correctness (treaty-oxc vs @angular/compiler oracle) ...');
  const correctness = runCorrectness();
  console.log(`  strict match (parity normalize): ${correctness.match}/${correctness.renderable}`);
  console.log(
    `  equivalent except the v22 changeDetection-field emit change: ` +
      `${correctness.equivalentIgnoringChangeDetectionField}/${correctness.renderable} ` +
      `(${correctness.changeDetectionOnly} fixtures differ ONLY by \`changeDetection:0\`)`,
  );
  console.log(`  genuine lowering divergences: ${correctness.loweringDivergences}`);
  if (correctness.loweringDivergenceFixtures.length) {
    console.log(`  lowering-divergence fixtures: ${correctness.loweringDivergenceFixtures.join(', ')}`);
  }
  console.log(`  oracle-unrenderable (i18n): ${correctness.oracleError}`);
  console.log('');

  // Build the SHARED timed set = fixtures ALL measured compilers can lower, so
  // every compiler is timed over the identical templates (the Angular oracle
  // printer throws on the i18n fixtures; the intersection drops those for all).
  const angOk = new Set(usableFixtures(compileAngular).ok.map((f) => f.id));
  const oxcOk = compileTreatyOxc
    ? new Set(usableFixtures(compileTreatyOxc).ok.map((f) => f.id))
    : new Set();
  const v21Ok = compileAngularV21
    ? new Set(usableFixtures(compileAngularV21).ok.map((f) => f.id))
    : null;
  const shared = FIXTURES.map((f) => f.id).filter(
    (id) =>
      angOk.has(id) &&
      (!compileTreatyOxc || oxcOk.has(id)) &&
      (!v21Ok || v21Ok.has(id)),
  );
  console.log(`shared timed fixtures (intersection): ${shared.length}/${FIXTURES.length}`);
  console.log('');

  // angular-compiler@22 — the repo's @angular/compiler oracle.
  console.log(`measuring angular-compiler@22 (${v22Version}) ...`);
  const r22 = measure(`angular-compiler@22`, compileAngular, shared);
  r22.angularVersion = v22Version;
  results.push(r22);

  // angular-compiler@21 — the isolated older @angular/compiler oracle.
  if (compileAngularV21) {
    console.log(`measuring angular-compiler@21 (${ISOLATED_NG_VERSION}) ...`);
    const r21 = measure(`angular-compiler@21`, compileAngularV21, shared);
    r21.angularVersion = ISOLATED_NG_VERSION;
    results.push(r21);
  } else {
    results.push({
      compiler: 'angular-compiler@21',
      status: 'pending',
      angularVersion: ISOLATED_NG_VERSION,
      note:
        `@angular/compiler@${ISOLATED_NG_VERSION} could not be isolated on this ` +
        `machine: ${v21LoadReason}`,
    });
  }

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
  // JSON artifacts.
  // -----------------------------------------------------------------------
  const outDir = path.join(__dirname, 'results');
  fs.mkdirSync(outDir, { recursive: true });
  const env = {
    node: process.version,
    platform: process.platform,
    arch: process.arch,
    cpu: os.cpus()[0]?.model?.trim() || 'unknown',
  };

  // (1) correctness.json — oracle parity verdict (treaty-oxc vs @angular/compiler).
  const correctnessFile = path.join(outDir, 'correctness.json');
  fs.writeFileSync(
    correctnessFile,
    JSON.stringify(
      {
        benchmark: 'compiler-correctness',
        generatedAt: new Date().toISOString(),
        oracle: `@angular/compiler@${v22Version}`,
        treaty: rust.ok ? path.basename(rust.from) : 'NOT AVAILABLE',
        comparison:
          'normalize() from libs/treaty-ivy/facade/parity/parity.mjs (same check the parity harness reports)',
        corpus: {
          source: 'libs/treaty-ivy/facade/parity/parity.mjs#FIXTURES',
          totalFixtures: FIXTURES.length,
        },
        env,
        ...correctness,
      },
      null,
      2,
    ) + '\n',
    'utf8',
  );

  // (2) compiler.json — timing rows (v21 vs v22 vs treaty-oxc) + correctness summary.
  const outFile = path.join(outDir, 'compiler.json');
  const payload = {
    benchmark: 'compiler-ivy-lowering',
    generatedAt: new Date().toISOString(),
    corpus: {
      source: 'libs/treaty-ivy/facade/parity/parity.mjs#FIXTURES',
      totalFixtures: FIXTURES.length,
    },
    env,
    config: { warmupIters: WARMUP_ITERS, measureIters: MEASURE_ITERS, bestOf: 3 },
    angularCompilerVersions: {
      v22: v22Version,
      v21: ng21 ? (ng21.VERSION ? ng21.VERSION.full : ISOLATED_NG_VERSION) : null,
      v21Status: ng21 ? 'isolated+measured' : 'pending: ' + v21LoadReason,
    },
    correctness: {
      file: 'correctness.json',
      summary: correctness.summary,
      match: correctness.match,
      diff: correctness.diff,
      oracleError: correctness.oracleError,
      diffFixtures: correctness.diffFixtures,
    },
    results,
  };
  fs.writeFileSync(outFile, JSON.stringify(payload, null, 2) + '\n', 'utf8');
  console.log('');
  console.log(`wrote ${path.relative(repoRoot, correctnessFile)}`);
  console.log(`wrote ${path.relative(repoRoot, outFile)}`);

  return results;
}

function pad(s, n) {
  s = String(s);
  return s.length >= n ? s + ' ' : s + ' '.repeat(n - s.length);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
