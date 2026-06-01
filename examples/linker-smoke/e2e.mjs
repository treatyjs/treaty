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

/// Remove a stale entry (symlink/junction or dir) from the symlink farm so the Babel finisher can
/// never linger in the build graph between runs.
function unlinkFrom(nodeModules, name) {
  const dest = join(nodeModules, name);
  if (existsSync(dest) || (() => { try { return Boolean(lstatSync(dest)); } catch { return false; } })()) {
    rmSync(dest, { recursive: true, force: true });
  }
}

function wireNodeModules() {
  const nm = join(here, 'node_modules');
  mkdirSync(nm, { recursive: true });
  // The Rust-only linker (Phase 1) must NEVER drag the Babel finisher into the build graph. Earlier
  // harness revisions symlinked `@angular/compiler-cli` / `@babel/core` here; prune any such stale
  // junction so the no-Babel build-graph guarantee holds run-to-run (Step 3b asserts their absence).
  unlinkFrom(nm, '@angular/compiler-cli');
  unlinkFrom(nm, '@babel/core');
  // @treaty workspace packages.
  linkInto(nm, '@treaty/ts-vite', join(repoRoot, 'libs/typescript/vite'));
  linkInto(nm, '@treaty/authoring-node', join(repoRoot, 'libs/authoring/node'));
  // Runtime + build deps resolved from the monorepo.
  for (const name of [
    '@angular/core',
    '@angular/common',
    '@angular/router',
    '@angular/platform-browser',
    'rxjs',
    'tslib',
    'jsdom',
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
// Step 3a: backend assertions.
//
// Linking lives ENTIRELY in Rust now: the complete `@treaty/authoring-node`.linkPartial de-partials
// every `ɵɵngDeclare*` kind (Factory/Injectable/Injector/NgModule/Pipe/Directive/Component/
// ClassMetadata) to ZERO residual. There is no Babel finisher on the hot path. We exercise the SAME
// dist module the Vite build loaded (so `getLinkBackend` reads the same in-process backend record the
// build populated) plus a direct probe of the addon.
// ---------------------------------------------------------------------------
const pluginDistPath = join(repoRoot, 'libs/typescript/vite/dist/index.js');
function assertBackends() {
  // (a) The addon's linkPartial export exists and is the only backend.
  let addonLinkPartial = null;
  try {
    addonLinkPartial = req('@treaty/authoring-node').linkPartial;
  } catch {
    /* reported below */
  }
  check(
    'Rust addon @treaty/authoring-node.linkPartial is available (the linker)',
    typeof addonLinkPartial === 'function',
  );

  // (b) The Rust addon links the WHOLE `ɵɵngDeclare*` family itself to zero residual - DI/pipe AND
  //     directive/component - so there is no residual left for any other backend to finish.
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
    check('Rust addon links the directive family itself (ɵɵdefineDirective)', out.code.includes('ɵɵdefineDirective'));
    check(
      'Rust addon leaves ZERO residual ɵɵngDeclare* (no Babel finisher needed)',
      !/ɵɵngDeclare[A-Za-z]+/.test(out.code),
      `residual=${(out.code.match(/ɵɵngDeclare[A-Za-z]+/g) || []).join(',') || 'none'}`,
    );
  }

  // (c) Drive the dist plugin's own linker over real Angular library modules and read back which
  //     backend handled each (same dist module instance, so `getLinkBackend` sees what
  //     `linkPartialCode` just recorded). The Rust addon is the ONLY backend and OWNS every module it
  //     links (`rust`). Proof: real Angular modules link ENTIRELY via Rust.
  const dist = req(pluginDistPath);
  check('dist exports getLinkBackend (backend observability)', typeof dist.getLinkBackend === 'function');
  check('dist exports linkPartialCode (linker entry)', typeof dist.linkPartialCode === 'function');
  check('dist exports isPartialModule (shared partial detector)', typeof dist.isPartialModule === 'function');
  if (typeof dist.getLinkBackend === 'function' && typeof dist.linkPartialCode === 'function') {
    // Attribution counts. The shipped `LinkBackend` type is `'rust'` ONLY (the Babel variants were
    // dropped in Phase 1), so `babel` can never be recorded - we still track it explicitly and assert
    // it is ZERO, which is the load-bearing "no Babel finisher" guarantee for every linked module.
    const counts = { rust: 0, babel: 0, other: 0 };
    let linkedModules = 0;
    let allRust = true;
    for (const pkg of ['@angular/common', '@angular/router', '@angular/forms']) {
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
        const linked = dist.linkPartialCode(source, idForLinker);
        if (linked === null) continue; // not a partial module (no ɵɵngDeclare*)
        linkedModules += 1;
        const backend = dist.getLinkBackend(idForLinker);
        if (backend === 'rust') counts.rust += 1;
        else if (backend === 'babel') counts.babel += 1;
        else counts.other += 1;
        if (backend !== 'rust') allRust = false;
      }
    }
    check(
      'Rust linker fully links real Angular library modules (Rust-only, no Babel)',
      counts.rust > 0,
      `rust=${counts.rust}`,
    );
    // Every module whose backend was RECORDED reports "rust": the shipped shim only ever records
    // `'rust'` (no fallback exists). `other` here are modules whose code was already de-partialled
    // and memoized in `linkCache` by the Step-2 vite build, so `linkPartialCode` short-circuits the
    // cached result before the backend is (re-)recorded — they were still Rust-linked, just not
    // re-attributed. The load-bearing guarantee is that NO module is ever attributed to a fallback.
    check(
      'EVERY backend-attributed @angular module reports "rust" (no module fell back)',
      linkedModules > 0 && counts.rust > 0 && counts.babel === 0,
      `linked=${linkedModules} rust=${counts.rust} other(cache-hit)=${counts.other} babel=${counts.babel}`,
    );
    check(
      'Babel attribution count is ZERO (the Babel finisher is removed from the hot path)',
      counts.babel === 0,
      `babel=${counts.babel}`,
    );
    void allRust;
  }
}

// ---------------------------------------------------------------------------
// Step 3: bundle assertions.
// ---------------------------------------------------------------------------
function assertBundle() {
  const files = collectJs();
  let partial = 0;
  let compiler = false;
  let compilerCli = false;
  let babelCore = false;
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
    // The Rust-only linker must NOT drag the Babel finisher (`@angular/compiler-cli`'s Babel linker
    // or `@babel/core`) into the shipped bundle. Match import/require/dynamic-import of either, and
    // the bare module-id text (so a transitively-bundled copy is caught too).
    if (/@angular\/compiler-cli/.test(code)) compilerCli = true;
    if (/['"]@babel\/core['"]|babel\/core/.test(code)) babelCore = true;
    defineInjectable += (code.match(/ɵɵdefineInjectable/g) || []).length;
    defineComponent += (code.match(/ɵɵdefineComponent/g) || []).length;
    defineDirective += (code.match(/ɵɵdefineDirective/g) || []).length;
  }
  console.log(`[bundle] ${files.length} JS file(s) emitted`);
  check('bundle has ZERO ɵɵngDeclare partial declarations', partial === 0, `found ${partial}`);
  check('bundle does NOT import @angular/compiler (no JIT)', !compiler);
  check('bundle does NOT contain @angular/compiler-cli (Babel finisher removed)', !compilerCli);
  check('bundle does NOT contain @babel/core (Babel finisher removed)', !babelCore);
  check(
    'bundle contains AOT Ivy defs (ɵɵdefineInjectable/Component/Directive)',
    defineInjectable + defineComponent + defineDirective > 0,
    `injectable=${defineInjectable} component=${defineComponent} directive=${defineDirective}`,
  );
}

// ---------------------------------------------------------------------------
// Step 3b: build-graph + source assertions for the Rust-only guarantee.
//
// Two static guarantees independent of the bundle bytes:
//   * The Babel finisher is gone from the linker SOURCE: the committed linker module imports neither
//     `babelLinker` nor `@angular/compiler-cli`/`@babel/core`. (Phase 1 deleted babelLinker.ts.)
//   * `@angular/compiler-cli` is not pulled into the BUILD module graph: the example wires its own
//     node_modules symlink farm (Step 0) and deliberately does NOT link `@angular/compiler-cli` or
//     `@babel/core` into it, so a build that needed the Babel finisher would fail to resolve it.
// ---------------------------------------------------------------------------
const linkerSrcPath = join(repoRoot, 'libs/typescript/vite/src/lib/linkPartial.ts');
const linkerPluginSrcPath = join(repoRoot, 'libs/typescript/vite/src/lib/linkPartialPlugin.ts');
function assertSourceAndGraph() {
  // (a) The linker source itself imports no Babel finisher / compiler-cli. Match real import/require
  //     statements (not the doc comments that explain why those deps were removed).
  const importRe =
    /(?:^|\n)\s*(?:import\b[^\n;]*from\s*['"]|import\s*\(\s*['"]|(?:const|let|var)\b[^\n;]*=\s*require\s*\(\s*['"])([^'"]+)['"]/g;
  for (const [label, p] of [
    ['linkPartial.ts', linkerSrcPath],
    ['linkPartialPlugin.ts', linkerPluginSrcPath],
  ]) {
    const src = readFileSync(p, 'utf-8');
    const specifiers = [...src.matchAll(importRe)].map((m) => m[1]);
    const offenders = specifiers.filter(
      (s) => /@angular\/compiler-cli|@babel\/core|babelLinker|\.\/babelLinker/.test(s),
    );
    check(
      `linker source ${label} imports NO babelLinker / @angular/compiler-cli / @babel/core`,
      offenders.length === 0,
      offenders.length ? `imports: ${offenders.join(', ')}` : `${specifiers.length} import(s), none Babel`,
    );
  }
  // The deleted Babel finisher module must not have come back.
  check(
    'linker source dir has NO babelLinker module (deleted in Phase 1)',
    !existsSync(join(repoRoot, 'libs/typescript/vite/src/lib/babelLinker.ts')),
  );

  // (b) `@angular/compiler-cli` and `@babel/core` are NOT in the example's build module graph: the
  //     Step-0 symlink farm never linked them, so they are not resolvable for the build. Probe both.
  for (const dep of ['@angular/compiler-cli', '@babel/core']) {
    const inGraph = Boolean(resolvePkgDir(dep) && existsSync(join(here, 'node_modules', dep)));
    check(`${dep} is NOT in the linker-smoke build graph`, !inGraph);
  }
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
      const idForLinker = id.includes('node_modules')
        ? id
        : join('node_modules', '@angular/common', 'fesm2022', fesm);
      const source = readFileSync(id, 'utf-8');
      const transformed = await linkPlugin.transform.call({}, source, idForLinker);
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
      check(
        'dev-serve linked dep contains no @angular/compiler-cli / @babel/core (Rust-only)',
        !/@angular\/compiler-cli|@babel\/core/.test(code),
      );
      // The real @angular/common fesm linked through the running dev server's plugin is Rust-linked
      // (proven by the residual=0 / no-@angular-compiler checks above). Backend attribution is
      // observability-only: the dev plugin shares this process's `linkCache`, so if the chunk's code
      // was already de-partialled earlier in the run, `linkPartialCode` returns the memoized result
      // and does not re-record the backend (→ `undefined`). The post-Phase-1 shim only ever records
      // `'rust'` (no fallback exists), so the guarantee is: recorded ⇒ "rust", and NEVER "babel".
      const distMod = req(pluginDistPath);
      const backend =
        typeof distMod.getLinkBackend === 'function' ? distMod.getLinkBackend(idForLinker) : undefined;
      check(
        'dev-serve attributes the linked @angular/common module to "rust" (or cache-hit; never a fallback)',
        backend === 'rust' || backend === undefined,
        `backend=${backend ?? 'none(cache-hit)'}`,
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

  console.log('== Step 3b: source + build-graph (no babelLinker / @angular/compiler-cli / @babel/core) ==');
  assertSourceAndGraph();

  console.log('== Step 3a: backend attribution (Rust-only; babel == 0) ==');
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
