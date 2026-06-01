// Treaty linker-smoke end-to-end harness.
//
// Proves, end to end, that an Angular app consuming published *partial-compiled* Angular libraries
// (`@angular/common`, `@angular/router`, `@angular/platform-browser`) builds and BOOTS with NO JIT
// and NO `@angular/compiler`, once the @treaty linker de-partials those libraries
// (`ɵɵngDeclare*` -> AOT `ɵɵdefine*`).
//
// Steps:
//   0. Wire a local node_modules symlink farm so Vite/Rollup resolve this app's deps from the
//      monorepo (the examples are not part of the root lockfile).
//   1. (Re)build the @treaty/ts-vite plugin dist from current source (esbuild bundle), so the wiring
//      under test is the committed source.
//   2. Run a real `vite build` of this app through the @treaty plugin.
//   3. Assert on the emitted bundle:
//        - ZERO `ɵɵngDeclare` partial declarations survive.
//        - NO `@angular/compiler` import/usage is bundled.
//        - the Angular Ivy AOT defs are present (`ɵɵdefineInjectable` / `ɵɵdefineComponent` /
//          `ɵɵdefineDirective`).
//   4. BOOT the built bundle headlessly (jsdom) and assert:
//        - bootstrap does NOT throw the "needs JIT" / "@angular/compiler is not available" error.
//        - the routed component (which `inject(PlatformLocation)`) renders into the DOM.
//
// Usage:  node examples/linker-smoke/e2e.mjs
// Exit code 0 on success, 1 on any failed assertion.

import { build } from 'vite';
import { execFileSync } from 'node:child_process';
import { createRequire } from 'node:module';
import {
  readFileSync,
  rmSync,
  readdirSync,
  existsSync,
  mkdirSync,
  symlinkSync,
  lstatSync,
} from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

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

// ---------------------------------------------------------------------------
// Step 0: wire a local node_modules so Vite/Rollup resolve the app's deps.
//
// The examples are NOT wired through the root pnpm lockfile, so this app has no node_modules of its
// own. We create a minimal, idempotent symlink farm pointing at the monorepo's already-installed
// packages plus the @treaty workspace packages. This makes the e2e self-contained and reproducible
// without mutating the lockfile.
// ---------------------------------------------------------------------------
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

function wireNodeModules() {
  const nm = join(here, 'node_modules');
  mkdirSync(nm, { recursive: true });
  // @treaty workspace packages.
  linkInto(nm, '@treaty/ts-vite', join(repoRoot, 'libs/typescript/vite'));
  linkInto(nm, '@treaty/authoring-node', join(repoRoot, 'libs/authoring/node'));
  // Runtime + build deps resolved from the monorepo.
  for (const name of [
    '@angular/core',
    '@angular/common',
    '@angular/router',
    '@angular/platform-browser',
    '@angular/compiler-cli',
    'rxjs',
    'tslib',
    'jsdom',
    '@babel/core',
  ]) {
    linkInto(nm, name, resolvePkgDir(name));
  }
}

// ---------------------------------------------------------------------------
// Step 1: (re)build the @treaty/ts-vite plugin dist from current source.
// ---------------------------------------------------------------------------
function buildPluginDist() {
  const esbuild = req.resolve('esbuild/bin/esbuild', { paths: [repoRoot] });
  const entry = join(repoRoot, 'libs/typescript/vite/src/index.ts');
  const out = join(repoRoot, 'libs/typescript/vite/dist/index.js');
  mkdirSync(dirname(out), { recursive: true });
  execFileSync(
    process.execPath,
    [
      esbuild,
      entry,
      '--bundle',
      '--platform=node',
      '--format=cjs',
      '--target=node20',
      '--packages=external',
      `--outfile=${out}`,
    ],
    { stdio: ['ignore', 'ignore', 'inherit'] },
  );
  return out;
}

// ---------------------------------------------------------------------------
// Step 2: real production build of the app through the @treaty plugin.
// ---------------------------------------------------------------------------
// Emit under `dist/` so the generated bundle falls under the repo-wide `**/dist/**` lint/ignore
// globs (and the example's .gitignore) - it is build output, never linted or committed.
const outDir = join(here, 'dist', 'e2e');
async function runBuild() {
  rmSync(outDir, { recursive: true, force: true });
  await build({
    root: here,
    logLevel: 'warn',
    configFile: join(here, 'vite.config.ts'),
    build: { outDir, minify: false },
  });
}

function collectJs() {
  return readdirSync(outDir, { recursive: true })
    .filter((f) => typeof f === 'string' && f.endsWith('.js'))
    .map((f) => join(outDir, f));
}

// ---------------------------------------------------------------------------
// Step 3: bundle assertions.
// ---------------------------------------------------------------------------
function assertBundle() {
  const files = collectJs();
  let partial = 0;
  let compiler = false;
  let defineInjectable = 0;
  let defineComponent = 0;
  let defineDirective = 0;
  for (const file of files) {
    const code = readFileSync(file, 'utf-8');
    // Count actual partial-declaration CALL expressions (`ɵɵngDeclareComponent(`, etc.), not bare
    // textual mentions in comments/strings (e.g. this repo's own doc comments reference the marker).
    partial += (code.match(/ɵɵngDeclare[A-Za-z]+\s*\(/g) || []).length;
    if (
      /from\s*['"]@angular\/compiler['"]/.test(code) ||
      /require\(\s*['"]@angular\/compiler['"]\s*\)/.test(code) ||
      /import\(\s*['"]@angular\/compiler['"]\s*\)/.test(code)
    ) {
      compiler = true;
    }
    defineInjectable += (code.match(/ɵɵdefineInjectable/g) || []).length;
    defineComponent += (code.match(/ɵɵdefineComponent/g) || []).length;
    defineDirective += (code.match(/ɵɵdefineDirective/g) || []).length;
  }
  console.log(`[bundle] ${files.length} JS file(s) emitted`);
  check('bundle has ZERO ɵɵngDeclare partial declarations', partial === 0, `found ${partial}`);
  check('bundle does NOT import @angular/compiler (no JIT)', !compiler);
  check(
    'bundle contains AOT Ivy defs (ɵɵdefineInjectable/Component/Directive)',
    defineInjectable + defineComponent + defineDirective > 0,
    `injectable=${defineInjectable} component=${defineComponent} directive=${defineDirective}`,
  );
}

// ---------------------------------------------------------------------------
// Step 4: headless boot (jsdom) of the emitted bundle.
// ---------------------------------------------------------------------------
async function bootHeadless() {
  const { JSDOM } = req('jsdom');
  const dom = new JSDOM(
    `<!doctype html><html><body><smoke-root></smoke-root></body></html>`,
    { url: 'http://localhost/', pretendToBeVisual: true, runScripts: 'outside-only' },
  );
  const { window } = dom;

  // Install the browser globals Angular expects. Some globals (e.g. `navigator`) are read-only
  // accessor props on Node's globalThis, so assign defensively via defineProperty and tolerate the
  // ones the runtime refuses to override (Angular reads them off `window`/`document` regardless).
  const setGlobal = (key, value) => {
    try {
      Object.defineProperty(globalThis, key, { value, configurable: true, writable: true });
    } catch {
      /* read-only Node global (e.g. navigator): Angular reads it from window anyway */
    }
  };
  setGlobal('window', window);
  setGlobal('document', window.document);
  setGlobal('navigator', window.navigator);
  setGlobal('location', window.location);
  setGlobal('HTMLElement', window.HTMLElement);
  setGlobal('Node', window.Node);
  setGlobal('Element', window.Element);
  setGlobal('Event', window.Event);
  setGlobal('customElements', window.customElements);
  setGlobal('getComputedStyle', window.getComputedStyle?.bind(window));
  setGlobal('requestAnimationFrame', (cb) => setTimeout(() => cb(Date.now()), 0));
  setGlobal('cancelAnimationFrame', (id) => clearTimeout(id));
  // Bulk-mirror the remaining DOM constructors/APIs jsdom exposes on `window` (MutationObserver,
  // CustomEvent, DocumentFragment, Text, KeyboardEvent, ...) onto globalThis where missing, so the
  // Angular runtime finds every DOM global it touches during bootstrap.
  for (const key of Object.getOwnPropertyNames(window)) {
    if (key in globalThis) continue;
    const value = window[key];
    if (typeof value === 'function' || (value && typeof value === 'object')) {
      setGlobal(key, value);
    }
  }

  let consoleError = '';
  const origError = console.error;
  console.error = (...args) => {
    consoleError += args.map(String).join(' ') + '\n';
  };

  const entry = collectJs().find((f) => /main/.test(f)) ?? collectJs()[0];
  let importError = null;
  try {
    await import(pathToFileURL(entry).href);
  } catch (e) {
    importError = e;
  }

  // Let Angular's async bootstrap flush.
  await new Promise((r) => setTimeout(r, 300));
  console.error = origError;

  const bootErrEl = window.document.getElementById('bootstrap-error');
  const bootErrText = bootErrEl ? bootErrEl.textContent : '';
  const combined = `${importError ? String(importError.stack ?? importError) : ''}\n${bootErrText}\n${consoleError}`;
  const jitError =
    /needs to be compiled using the JIT compiler|@angular\/compiler|JIT compilation failed|Runtime compiler is not loaded|Component .* is not resolved/i.test(
      combined,
    );

  const heading = window.document.getElementById('smoke-heading');
  const injected = window.document.getElementById('smoke-injected');
  const rendered = Boolean(heading) && /Linker smoke/.test(heading?.textContent ?? '');

  check(
    'boot did NOT throw a JIT / @angular/compiler error',
    !jitError,
    jitError ? combined.trim().split('\n').slice(0, 4).join(' | ') : '',
  );
  check(
    'boot did NOT throw on import/bootstrap',
    !importError && !bootErrText,
    importError ? String(importError.message) : bootErrText.split('\n')[0],
  );
  check(
    'routed component rendered (inject(PlatformLocation) resolved)',
    rendered,
    heading ? heading.textContent : 'no #smoke-heading',
  );
  if (injected) console.log(`[boot] ${injected.textContent?.trim()}`);
}

// ---------------------------------------------------------------------------
async function main() {
  console.log('== Step 0: wire local node_modules ==');
  wireNodeModules();
  check('local node_modules wired', existsSync(join(here, 'node_modules/@angular/router')));

  console.log('== Step 1: build @treaty/ts-vite plugin dist ==');
  check('plugin dist rebuilt', existsSync(buildPluginDist()));

  console.log('== Step 2: vite build of linker-smoke ==');
  await runBuild();

  console.log('== Step 3: bundle assertions ==');
  assertBundle();

  console.log('== Step 4: headless boot ==');
  await bootHeadless();

  console.log('');
  if (failures.length) {
    console.error(`E2E FAILED: ${failures.length} assertion(s): ${failures.join('; ')}`);
    process.exit(1);
  }
  console.log('E2E PASSED: app builds and boots with no JIT and no @angular/compiler.');
}

main().catch((err) => {
  console.error('E2E ERROR:', err);
  process.exit(1);
});
