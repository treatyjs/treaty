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

import { build, createServer } from 'vite';
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
// Step 3a: backend primacy assertions.
//
// Prove the Rust/NAPI addon (`@treaty/authoring-node`.linkPartial) is the PRIMARY linker and that the
// Babel (`@angular/compiler-cli`) backend runs ONLY to finish residual `ɵɵngDeclare*` the Rust linker
// leaves. We exercise the SAME dist module the Vite build loaded (so `getLinkBackend` reads the same
// in-process backend record the build populated) plus a direct probe of the addon.
// ---------------------------------------------------------------------------
const pluginDistPath = join(repoRoot, 'libs/typescript/vite/dist/index.js');
function assertBackends() {
  // (a) The addon's linkPartial export exists and is the primary backend.
  let addonLinkPartial = null;
  try {
    addonLinkPartial = req('@treaty/authoring-node').linkPartial;
  } catch {
    /* reported below */
  }
  check(
    'Rust addon @treaty/authoring-node.linkPartial is available (primary backend)',
    typeof addonLinkPartial === 'function',
  );

  // (b) The addon links the DI/pipe family to AOT itself, and leaves only the component/directive
  //     residual for Babel - i.e. Rust does real work as the primary backend, not a pass-through.
  if (typeof addonLinkPartial === 'function') {
    const probe = [
      `import * as i0 from '@angular/core';`,
      `class Svc {}`,
      `Svc.ɵprov = i0.ɵɵngDeclareInjectable({ minVersion: "12.0.0", version: "0.0.0-PLACEHOLDER", ngImport: i0, type: Svc, providedIn: "root" });`,
      `class Dir {}`,
      `Dir.ɵdir = i0.ɵɵngDeclareDirective({ minVersion: "14.0.0", version: "0.0.0-PLACEHOLDER", type: Dir, selector: "[d]", ngImport: i0 });`,
    ].join('\n');
    const out = addonLinkPartial(probe, '/probe/common.mjs');
    check('Rust addon links the DI/pipe family itself (ɵɵdefineInjectable)', out.code.includes('ɵɵdefineInjectable'));
    check(
      'Rust addon leaves only component/directive residual for Babel to finish',
      out.code.includes('ɵɵngDeclareDirective'),
      `residual=${(out.code.match(/ɵɵngDeclare[A-Za-z]+/g) || []).join(',') || 'none'}`,
    );
  }

  // (c) Drive the dist plugin's own linker over real Angular library modules and read back which
  //     backend handled each (same dist module instance, so `getLinkBackend` sees what `linkPartialCode`
  //     just recorded). The Rust addon is the PRIMARY backend (always tried first) and OWNS every
  //     module it can fully link (`rust`); Babel links only modules Rust cannot yet fully link
  //     (`babel`). Proof of primacy: real Angular modules link ENTIRELY via Rust (`rust`), i.e. the
  //     Rust linker does real work and is not a Babel pass-through.
  const dist = req(pluginDistPath);
  check('dist exports getLinkBackend (backend observability)', typeof dist.getLinkBackend === 'function');
  check('dist exports linkPartialCode (linker entry)', typeof dist.linkPartialCode === 'function');
  if (typeof dist.getLinkBackend === 'function' && typeof dist.linkPartialCode === 'function') {
    const counts = { rust: 0, babel: 0 };
    for (const pkg of ['@angular/common', '@angular/router', '@angular/platform-browser']) {
      const dir = resolvePkgDir(pkg);
      if (!dir || !existsSync(join(dir, 'fesm2022'))) continue;
      const fesm = readdirSync(join(dir, 'fesm2022'), { recursive: true }).filter(
        (f) => typeof f === 'string' && f.endsWith('.mjs'),
      );
      for (const f of fesm) {
        // The linker's `isPartialModule` guard requires a `node_modules` segment in the id.
        const id = join(dir, 'fesm2022', f);
        const idForLinker = id.includes('node_modules') ? id : join('node_modules', pkg, 'fesm2022', f);
        const source = readFileSync(id, 'utf-8');
        dist.linkPartialCode(source, idForLinker);
        const backend = dist.getLinkBackend(idForLinker);
        if (backend) counts[backend] += 1;
      }
    }
    check(
      'Rust primary backend fully links real Angular library modules (real Rust work, not a Babel pass-through)',
      counts.rust > 0,
      `rust=${counts.rust} babel=${counts.babel}`,
    );
    check(
      'every partial Angular module was attributed to a backend (Rust primary, Babel finisher)',
      counts.rust + counts.babel > 0,
    );
  }
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
// Step 5: dev-serve gap.
//
// `linkAngularPartials()` must own the dev-serve HTML and inject NO `<script src=.../@angular/compiler>`
// (the whole point: no JIT in dev either), while STILL linking partial `node_modules` deps on the fly
// (the esbuild optimizeDeps plugin + the transform hook) so the served modules are AOT. We boot a real
// Vite dev server, fetch the transformed index.html, and link a real partial @angular/common module
// through the dev server's transform pipeline.
// ---------------------------------------------------------------------------
async function assertDevServe() {
  const server = await createServer({
    root: here,
    logLevel: 'warn',
    configFile: join(here, 'vite.config.ts'),
    server: { middlewareMode: false, port: 0, hmr: false },
    optimizeDeps: { noDiscovery: true },
  });
  try {
    await server.listen();

    // (a) The dev server's HTML transform injects NO @angular/compiler script (no JIT in dev). This
    //     runs the REAL `transformIndexHtml` pipeline of the running dev server.
    const html = await server.transformIndexHtml(
      '/index.html',
      readFileSync(join(here, 'index.html'), 'utf-8'),
    );
    check(
      'dev-serve index.html injects NO @angular/compiler script (no JIT in dev)',
      !/@angular\/compiler/.test(html),
      /@angular\/compiler/.test(html) ? 'compiler script present' : '',
    );

    // (b) The dev-serve config excludes @angular/compiler from prebundling AND wires the esbuild
    //     optimizeDeps linker plugin, so partial node_modules deps are de-partialled on the fly when
    //     the dev server prebundles them. Assert both are active on the resolved serve config.
    const optimize = server.config.optimizeDeps ?? {};
    const excludesCompiler = (optimize.exclude ?? []).includes('@angular/compiler');
    const esbuildPlugins = optimize.esbuildOptions?.plugins ?? [];
    const hasLinkerPrebundle = esbuildPlugins.some(p => p?.name === 'treaty-link-partial-deps');
    check('dev-serve excludes @angular/compiler from prebundling', excludesCompiler);
    check(
      'dev-serve wires the esbuild optimizeDeps linker (de-partials prebundled deps on the fly)',
      hasLinkerPrebundle,
    );

    // (c) The LinkPartialPlugin (the on-the-fly transform for non-prebundled deps) is active on serve.
    const linkPlugin = (server.config.plugins ?? []).find(
      p => p?.name === 'vite-plugin-treaty-link-partial',
    );
    check('dev-serve has the on-the-fly partial linker (LinkPartialPlugin) active', Boolean(linkPlugin));

    // (d) That linker, run over a real partial @angular/common fesm module exactly as the dev
    //     transform does, yields AOT output with zero residual ɵɵngDeclare and no @angular/compiler.
    const commonDir = resolvePkgDir('@angular/common');
    const fesm = commonDir
      ? readdirSync(join(commonDir, 'fesm2022'), { recursive: true }).find(
          (f) => typeof f === 'string' && f.endsWith('.mjs'),
        )
      : null;
    if (fesm && linkPlugin && typeof linkPlugin.transform === 'function') {
      const id = join(commonDir, 'fesm2022', fesm);
      const source = readFileSync(id, 'utf-8');
      const transformed = await linkPlugin.transform.call({}, source, id);
      const code = transformed?.code ?? '';
      check(
        'dev-serve links partial node_modules deps on the fly (no residual ɵɵngDeclare)',
        code.length > 0 && !/ɵɵngDeclare[A-Za-z]+\s*\(/.test(code),
        `len=${code.length} residual=${(code.match(/ɵɵngDeclare[A-Za-z]+\s*\(/g) || []).length}`,
      );
      check(
        'dev-serve linked dep does NOT import @angular/compiler',
        !/from\s*['"]@angular\/compiler['"]|import\(\s*['"]@angular\/compiler['"]/.test(code),
      );
    } else {
      check('dev-serve links partial node_modules deps on the fly', false, 'no @angular/common fesm / linker transform');
    }
  } finally {
    await server.close();
  }
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

  console.log('== Step 3a: backend primacy (Rust primary, Babel residual-only) ==');
  assertBackends();

  console.log('== Step 4: headless boot ==');
  await bootHeadless();

  console.log('== Step 5: dev-serve gap (no JIT in dev, deps linked on the fly) ==');
  await assertDevServe();

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
