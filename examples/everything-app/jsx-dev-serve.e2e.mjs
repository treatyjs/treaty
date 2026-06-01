// Treaty dev-serve JSX end-to-end harness.
//
// Proves the fix for the reported dev-serve bug: `bun run dev:vite` (a real `vite` dev server) over a
// `.tsx` Treaty app aborted its dependency scan with
//
//     "@treaty/jsx/jsx-dev-runtime (imported by .../counter.tsx) could not be resolved"
//
// even though `counter.tsx` imports no such thing. ROOT CAUSE: the app's `tsconfig.json` sets
// `jsx: "react-jsx"` + `jsxImportSource: "@treaty/jsx"` (so the editor / `tsgo` type-check the JSX),
// and esbuild's AUTOMATIC-JSX dev transform reads that tsconfig and INJECTS
// `import { jsxDEV } from "@treaty/jsx/jsx-dev-runtime"` into the scanned file. That injected import
// escapes to Vite's dep-scanner as a phantom dependency and fails to resolve.
//
// Treaty JSX is **Ivy, not React**: `@treaty/vite`'s `enforce: 'pre'` transform lowers a `.tsx`/`.tjsx`
// authoring file to `ɵɵdefineComponent`, consuming the RAW JSX — it never calls a React-style
// `jsx()`/`jsxDEV()` runtime factory. The fix (in `@treaty/vite`'s `config()` hook) forces esbuild
// `jsx: 'preserve'` on BOTH the dependency scanner (`optimizeDeps.esbuildOptions.jsx`) and Vite's main
// transform pass (`esbuild.jsx`), so esbuild leaves the JSX untouched, never injects a foreign runtime
// import, and the Treaty transform stays the sole consumer of the JSX.
//
// This harness builds the `@treaty/vite` dist from current source and, against a `.tsx` fixture whose
// tsconfig reproduces the exact `jsxImportSource: "@treaty/jsx"` setup, asserts end to end:
//
//   1. UNIT (regression): scanning the raw fixture .tsx through esbuild with the project's
//      jsxImportSource AND esbuild's automatic dev transform INJECTS the unresolvable
//      `@treaty/jsx/jsx-dev-runtime` import (the bug), while `jsx: 'preserve'` (what `config()` sets)
//      injects NOTHING — proving the chosen lever is the one that suppresses the phantom dependency.
//   2. CONFIG: the wired `treaty()` authoring plugin's `config()` returns `esbuild.jsx === 'preserve'`
//      and `optimizeDeps.esbuildOptions.jsx === 'preserve'` (so Vite drops the tsconfig jsxImportSource
//      for the bundler — both the scanner and the main transform pass).
//   3. DEV-SERVE (the real failure): a real `vite` dev server boots over the fixture and serves
//      `index.html` WITHOUT aborting the dependency scan — i.e. the
//      "@treaty/jsx/jsx-dev-runtime could not be resolved" error never occurs.
//   4. LOWERED: fetching the served `.tsx` module returns Treaty-lowered Ivy
//      (`@angular/core` + `ɵɵdefineComponent`) with NO injected `@treaty/jsx/jsx-dev-runtime` import
//      and NO React-runtime `jsxDEV(`/`createElement(` call — the Treaty transform owns the file.
//
// Usage:  node examples/everything-app/jsx-dev-serve.e2e.mjs
// Exit code 0 on success, 1 on any failed assertion.

import { execFileSync, spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import {
  readFileSync,
  writeFileSync,
  rmSync,
  existsSync,
  mkdirSync,
  symlinkSync,
  lstatSync,
} from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, '..', '..');
const req = createRequire(import.meta.url);

const failures = [];
function check(label, condition, detail) {
  const ok = Boolean(condition);
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${label}${detail ? ` - ${detail}` : ''}`);
  if (!ok) failures.push(label);
  return ok;
}

function resolvePkgDir(name) {
  try {
    return dirname(req.resolve(`${name}/package.json`, { paths: [repoRoot] }));
  } catch {
    return null;
  }
}

function linkInto(nodeModules, name, target) {
  if (!target) return false;
  const dest = join(nodeModules, name);
  mkdirSync(dirname(dest), { recursive: true });
  if (existsSync(dest)) {
    try {
      const st = lstatSync(dest);
      if (st.isSymbolicLink() || st.isDirectory()) return true;
    } catch {
      /* recreate */
    }
    rmSync(dest, { recursive: true, force: true });
  }
  symlinkSync(target, dest, 'junction');
  return true;
}

// ---------------------------------------------------------------------------
// Step 0: wire a node_modules symlink farm for the fixture. CRUCIAL: this links `@treaty/jsx` (the
// authoring-format JSX types + automatic-runtime shim) into node_modules alongside the other
// @treaty/* packages, so even a stray automatic-JSX injection would resolve to its dist — belt and
// suspenders behind the primary `jsx: 'preserve'` fix. The other @treaty/* packages the @treaty/vite
// dist imports (compiler, ts-vite, module-federation, authoring-node) and the real @angular runtime
// libs are linked too so the plugin + Vite resolve everything the way a real Treaty app would.
// ---------------------------------------------------------------------------
const fixtureDir = join(here, '.e2e-jsx-fixture');

function wireNodeModules() {
  const nm = join(fixtureDir, 'node_modules');
  mkdirSync(nm, { recursive: true });
  // @treaty/jsx is NOT in the root lockfile workspace map, so resolve it by its known monorepo path.
  linkInto(nm, '@treaty/jsx', join(repoRoot, 'libs/treaty/jsx'));
  linkInto(nm, '@treaty/compiler', join(repoRoot, 'libs/treaty/compiler'));
  linkInto(nm, '@treaty/module-federation', join(repoRoot, 'libs/treaty/module-federation'));
  linkInto(nm, '@treaty/ts-vite', resolvePkgDir('@treaty/ts-vite') ?? join(repoRoot, 'libs/typescript/vite'));
  linkInto(nm, '@treaty/authoring-node', join(repoRoot, 'libs/authoring/node'));
  for (const name of ['@angular/core', '@angular/common', '@angular/router', 'rxjs', 'tslib', 'vite', 'esbuild']) {
    linkInto(nm, name, resolvePkgDir(name));
  }
  return nm;
}

// ---------------------------------------------------------------------------
// Step 1: build the @treaty/ts-vite + @treaty/vite plugin dists from current source. @treaty/vite is
// bundled as CJS with every package kept EXTERNAL (resolved through the farm), so the harness can
// `require` it and Vite loads the exact dist this run produced.
// ---------------------------------------------------------------------------
function esbuildBundle(entry, out) {
  const bin = req.resolve('esbuild/bin/esbuild', { paths: [repoRoot] });
  mkdirSync(dirname(out), { recursive: true });
  execFileSync(
    process.execPath,
    [bin, entry, '--bundle', '--platform=node', '--format=cjs', '--target=node20', '--packages=external', `--outfile=${out}`],
    { stdio: ['ignore', 'ignore', 'inherit'] },
  );
  return out;
}

const tsViteDist = join(repoRoot, 'libs/typescript/vite/dist/index.js');
const treatyViteDist = join(repoRoot, 'libs/treaty/vite/dist/index.e2e.cjs');

function buildPluginDists() {
  esbuildBundle(join(repoRoot, 'libs/typescript/vite/src/index.ts'), tsViteDist);
  esbuildBundle(join(repoRoot, 'libs/treaty/vite/src/index.ts'), treatyViteDist);
}

// ---------------------------------------------------------------------------
// Step 2: write the `.tsx` fixture that reproduces the bug. Its tsconfig sets the exact
// `jsx: "react-jsx"` + `jsxImportSource: "@treaty/jsx"` the everything-app uses, and the entry is a
// Treaty JSX component (lowercase fn returning JSX) — the kind `@treaty/vite` lowers to Ivy.
// ---------------------------------------------------------------------------
function writeFixture() {
  rmSync(fixtureDir, { recursive: true, force: true });
  mkdirSync(fixtureDir, { recursive: true });

  // The Treaty JSX authoring file: a signal-based component returning JSX. It imports NOTHING from
  // @treaty/jsx — only esbuild's automatic-JSX dev transform would inject jsx-dev-runtime here.
  const counterTsx = [
    "import { signal } from '@angular/core'",
    '',
    'export default function counter() {',
    '  const count = signal(0)',
    '  const bump = (): void => {',
    '    count.update((n) => n + 1)',
    '  }',
    '  return (',
    '    <section class="counter">',
    '      <button class="bump" onClick={bump}>count is {count()}</button>',
    '    </section>',
    '  )',
    '}',
    '',
  ].join('\n');
  writeFileSync(join(fixtureDir, 'counter.tsx'), counterTsx);

  writeFileSync(
    join(fixtureDir, 'main.ts'),
    ["import counter from './counter'", 'globalThis.__treatyCounter = counter', ''].join('\n'),
  );

  writeFileSync(
    join(fixtureDir, 'index.html'),
    [
      '<!doctype html><html><head><title>treaty jsx dev fixture</title></head>',
      '<body><app-root></app-root>',
      '<script type="module" src="/main.ts"></script>',
      '</body></html>',
      '',
    ].join('\n'),
  );

  // The tsconfig that triggers the bug: jsxImportSource: "@treaty/jsx" makes esbuild's automatic-JSX
  // transform target @treaty/jsx/jsx-dev-runtime.
  writeFileSync(
    join(fixtureDir, 'tsconfig.json'),
    JSON.stringify(
      {
        compilerOptions: {
          moduleResolution: 'bundler',
          module: 'esnext',
          target: 'es2022',
          lib: ['es2022', 'dom', 'dom.iterable'],
          strict: true,
          noEmit: true,
          jsx: 'react-jsx',
          jsxImportSource: '@treaty/jsx',
          types: [],
        },
        include: ['*.ts', '*.tsx'],
      },
      null,
      2,
    ) + '\n',
  );

  const pluginPath = treatyViteDist.replace(/\\/g, '/');
  writeFileSync(
    join(fixtureDir, 'vite.config.mjs'),
    [
      "import { createRequire } from 'node:module';",
      'const require = createRequire(import.meta.url);',
      `const treaty = require(${JSON.stringify(pluginPath)}).default;`,
      'export default {',
      `  root: ${JSON.stringify(fixtureDir.replace(/\\/g, '/'))},`,
      '  logLevel: "error",',
      '  plugins: [treaty({ sourceMap: false })],',
      '};',
      '',
    ].join('\n'),
  );

  return join(fixtureDir, 'vite.config.mjs');
}

// ---------------------------------------------------------------------------
// Step (unit): scanning the raw .tsx with the project's jsxImportSource under esbuild's automatic dev
// transform INJECTS the unresolvable import (the bug); `jsx: 'preserve'` injects nothing (the fix).
// ---------------------------------------------------------------------------
async function assertEsbuildLever() {
  const esbuild = req('esbuild');
  const src = readFileSync(join(fixtureDir, 'counter.tsx'), 'utf-8');

  const automatic = await esbuild.transform(src, {
    loader: 'tsx',
    format: 'esm',
    jsx: 'automatic',
    jsxDev: true,
    jsxImportSource: '@treaty/jsx',
  });
  check(
    "REPRO: esbuild automatic-JSX dev transform injects the unresolvable @treaty/jsx/jsx-dev-runtime import",
    /from\s*["']@treaty\/jsx\/jsx-dev-runtime["']/.test(automatic.code),
    'this injected import is what aborted the dep scan',
  );

  const preserved = await esbuild.transform(src, { loader: 'tsx', format: 'esm', jsx: 'preserve' });
  check(
    "FIX LEVER: jsx:'preserve' injects NO @treaty/jsx runtime import (raw JSX kept for the Treaty transform)",
    !/@treaty\/jsx/.test(preserved.code) && /<section/.test(preserved.code),
  );
}

// ---------------------------------------------------------------------------
// Step (config): the wired authoring plugin forces jsx:'preserve' on both esbuild surfaces.
// ---------------------------------------------------------------------------
function authoringPluginOf(plugins) {
  return plugins.flat(Infinity).filter(Boolean).find((p) => p && p.name === 'treaty:vite');
}

function assertConfig() {
  const treaty = req(treatyViteDist).default;
  const plugin = authoringPluginOf(treaty({ sourceMap: false }));
  if (!check('treaty() exposes the treaty:vite authoring plugin', Boolean(plugin))) return;
  const cfg = plugin.config.call({}, {}, { command: 'serve', mode: 'development' });
  check(
    "config() forces esbuild.jsx === 'preserve' (main transform drops tsconfig jsxImportSource)",
    cfg?.esbuild?.jsx === 'preserve',
    `esbuild.jsx=${cfg?.esbuild?.jsx}`,
  );
  check(
    "config() forces optimizeDeps.esbuildOptions.jsx === 'preserve' (the dep scanner)",
    cfg?.optimizeDeps?.esbuildOptions?.jsx === 'preserve',
    `scanner jsx=${cfg?.optimizeDeps?.esbuildOptions?.jsx}`,
  );
  check(
    "config() still teaches esbuild the .tjsx loader",
    cfg?.optimizeDeps?.esbuildOptions?.loader?.['.tjsx'] === 'tsx',
  );
}

// ---------------------------------------------------------------------------
// Step (dev-serve): boot a REAL `vite` dev server over the fixture and assert it serves without the
// dep-scan resolution failure, and that the served .tsx is Treaty-lowered Ivy with no foreign runtime.
// ---------------------------------------------------------------------------
function viteBin() {
  const pkgDir = dirname(req.resolve('vite/package.json', { paths: [repoRoot] }));
  const pkg = req(join(pkgDir, 'package.json'));
  const binRel = typeof pkg.bin === 'string' ? pkg.bin : pkg.bin.vite;
  return join(pkgDir, binRel);
}

async function assertDevServe(configPath) {
  const port = 5377;
  const child = spawn(
    process.execPath,
    [viteBin(), '-c', configPath, '--port', String(port), '--strictPort'],
    { cwd: fixtureDir, stdio: ['ignore', 'pipe', 'pipe'] },
  );
  let stderr = '';
  let stdout = '';
  child.stderr?.on('data', (d) => (stderr += d));
  child.stdout?.on('data', (d) => (stdout += d));

  const base = `http://localhost:${port}`;
  async function waitReady(timeoutMs) {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      // If the dep scan aborted, Vite prints the resolution error and the server never serves; bail
      // early so the assertion reports the real failure rather than just timing out.
      if (/could not be resolved/.test(stderr) || /could not be resolved/.test(stdout)) return false;
      try {
        const r = await fetch(`${base}/index.html`);
        if (r.ok) return true;
      } catch {
        /* not up yet */
      }
      await new Promise((r) => setTimeout(r, 200));
    }
    return false;
  }

  try {
    const ready = await waitReady(60_000);
    const log = `${stderr}\n${stdout}`.slice(-600);
    check(
      'DEV `vite` server serves index.html WITHOUT a dep-scan resolution failure',
      ready,
      ready ? '' : log,
    );
    check(
      'DEV server did NOT report "@treaty/jsx/jsx-dev-runtime could not be resolved" (the bug)',
      !/jsx-dev-runtime[^\n]*could not be resolved|could not be resolved[^\n]*jsx-dev-runtime/.test(log) &&
        !/@treaty\/jsx[^\n]*could not be resolved/.test(log),
      log.match(/could not be resolved/) ? log : '',
    );
    if (!ready) return;

    // Fetch the served .tsx module: it must be Treaty-lowered Ivy, NOT React JSX runtime output.
    let served = '';
    for (const u of [`${base}/counter.tsx`, `${base}/main.ts`]) {
      try {
        const r = await fetch(u);
        if (r.ok) {
          const body = await r.text();
          if (/counter|defineComponent|@angular\/core/.test(body)) {
            served = body;
            if (u.endsWith('counter.tsx')) break;
          }
        }
      } catch {
        /* try next */
      }
    }
    // Pull the counter module specifically (main.ts only re-exports it).
    try {
      const r = await fetch(`${base}/counter.tsx`);
      if (r.ok) served = await r.text();
    } catch {
      /* keep prior */
    }
    if (check('DEV served the lowered counter.tsx module', served.length > 0)) {
      // Vite rewrites the bare `@angular/core` specifier to a prebundled dep URL, so match the Ivy
      // emitter's stable signature that survives that rewrite: the `import * as i0 from "…angular…core…"`
      // namespace import the emitter always prepends, paired with an `ɵɵdefineComponent` member.
      check(
        'served counter.tsx is Treaty-lowered Ivy (i0 @angular/core namespace + ɵɵdefineComponent)',
        /import\s*\*\s*as\s+i0\s+from\s*["'][^"']*(?:@angular[/_]core|angular_core)[^"']*["']/.test(served) &&
          /ɵɵdefineComponent/.test(served),
        served.replace(/\s+/g, ' ').slice(0, 200),
      );
      check(
        'served counter.tsx injects NO @treaty/jsx/jsx-dev-runtime import',
        !/@treaty\/jsx\/jsx-dev-runtime/.test(served),
      );
      check(
        'served counter.tsx has NO React-runtime jsxDEV( / createElement( call (Ivy, not React)',
        !/\bjsxDEV\s*\(/.test(served) && !/\bcreateElement\s*\(/.test(served),
      );
    }
  } finally {
    await new Promise((resolve) => {
      let done = false;
      const finish = () => {
        if (!done) {
          done = true;
          resolve();
        }
      };
      child.once('exit', () => {
        clearTimeout(killTimer);
        finish();
      });
      child.kill();
      const killTimer = setTimeout(() => {
        try {
          child.kill('SIGKILL');
        } catch {
          /* already gone */
        }
        finish();
      }, 3000);
    });
  }
}

// ---------------------------------------------------------------------------
async function main() {
  console.log('== Step 0: write .tsx fixture (jsxImportSource: @treaty/jsx) ==');
  const configPath = writeFixture();
  console.log('== Step 0b: wire node_modules (incl @treaty/jsx) ==');
  wireNodeModules();
  check('fixture node_modules wired (incl @treaty/jsx)', existsSync(join(fixtureDir, 'node_modules/@treaty/jsx')));

  console.log('== Step 1: build @treaty/ts-vite + @treaty/vite dists ==');
  buildPluginDists();
  check('@treaty/vite dist built', existsSync(treatyViteDist));

  console.log('== Step 2 (unit): esbuild automatic-JSX injects the bug; jsx:preserve does not ==');
  await assertEsbuildLever();

  console.log("== Step 3 (config): treaty() forces jsx:'preserve' on both esbuild surfaces ==");
  assertConfig();

  console.log('== Step 4 (dev-serve): real `vite` dev server serves the .tsx app cleanly ==');
  await assertDevServe(configPath);

  rmSync(fixtureDir, { recursive: true, force: true });

  console.log('');
  if (failures.length) {
    console.error(`E2E FAILED: ${failures.length} assertion(s): ${failures.join('; ')}`);
    process.exit(1);
  }
  console.log(
    'E2E PASSED: a .tsx Treaty app dev-serves with no unresolved @treaty/jsx/jsx-dev-runtime; JSX is lowered to Ivy (not React).',
  );
}

main().catch((err) => {
  console.error('E2E ERROR:', err);
  process.exit(1);
});
