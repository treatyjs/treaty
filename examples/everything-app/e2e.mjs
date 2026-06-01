// Treaty everything-app linker wiring end-to-end harness.
//
// Proves the fix for the bug where `@treaty/vite` (libs/treaty/vite, `import treaty from
// "@treaty/vite"`) served published *partial-compiled* Angular libraries un-linked, so the app threw
// "_PlatformLocation needs JIT / @angular/compiler is not available" at runtime.
//
// `@treaty/vite` now reuses the SAME Rust-backed Angular partial-declaration linker plugins that
// `@treaty/ts-vite` owns (one source of truth: `createLinkPartialPlugins()` from `@treaty/ts-vite`).
// This harness builds the `@treaty/vite` plugin dist from current source and asserts, end to end, that:
//
//   1. `treaty()` returns the linker plugins alongside the authoring plugin (the wiring that was
//      missing): the `enforce: 'pre'` partial-declaration transform, the `optimizeDeps` esbuild
//      prebundle linker + `@angular/compiler` exclusion, and the dev-serve `index.html` guard.
//   2. Those plugins de-partial a REAL `@angular/common` fesm module (`ɵɵngDeclare*` -> AOT
//      `ɵɵdefine*`) to ZERO residual, with NO `@angular/compiler` / `@angular/compiler-cli` /
//      `@babel/core` pulled in - the Rust linker only.
//   3. The dev-serve `index.html` transform injects NO `@angular/compiler` script (no JIT in dev).
//
// Steps 5-7 then exercise the WIRED plugin through REAL Vite over a fixture that imports the exact
// partial `@angular` modules that previously crashed the everything-app (`@angular/common`'s
// `_PlatformLocation` source and `@angular/platform-browser`), proving the reported crash is fixed:
//
//   5. PROD - a real `vite build` with `treaty()` in the plugin chain. The emitted bundle has ZERO
//      residual `ɵɵngDeclare`, imports NO `@angular/compiler` anywhere, and carries AOT Ivy defs
//      (`ɵɵdefine*` incl. `ɵɵdefineInjectable`).
//   6. DEV - a real `vite dev` server. Fetching the served `@angular/common` (the `_PlatformLocation`
//      source) and `@angular/platform-browser` dep modules returns NO `ɵɵngDeclare`, and the served
//      `index.html` injects NO `@angular/compiler` script.
//   7. BOOT - the linked `_PlatformLocation` module is evaluated against a faithful `@angular/core`
//      stub whose `getCompilerFacade` throws Angular's real "needs to be compiled using the JIT
//      compiler, but '@angular/compiler' is not available" error. A correctly-linked AOT module never
//      reaches `getCompilerFacade`, so evaluation must NOT throw - i.e. the everything-app's reported
//      "_PlatformLocation needs JIT / @angular/compiler is not available" crash no longer happens.
//
// Usage:  node examples/everything-app/e2e.mjs
// Exit code 0 on success, 1 on any failed assertion.

import { execFileSync, spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import {
  readFileSync,
  writeFileSync,
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
// Step 0: wire a local node_modules symlink farm so the @treaty/vite plugin dist resolves its
// dependencies (`@treaty/ts-vite` for the shared linker, `@treaty/authoring-node` for the Rust
// linker addon) and the real partial @angular libraries are present to link. The examples are not
// part of the root lockfile, so this is an idempotent symlink farm into the monorepo's packages.
// It deliberately does NOT link `@angular/compiler` / `@angular/compiler-cli` / `@babel/core`.
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
  // @treaty workspace packages. `@treaty/vite` reuses the linker from `@treaty/ts-vite`, so both
  // must resolve; `@treaty/authoring-node` is the Rust linker addon the shared shim calls.
  linkInto(nm, '@treaty/ts-vite', join(repoRoot, 'libs/typescript/vite'));
  linkInto(nm, '@treaty/authoring-node', join(repoRoot, 'libs/authoring/node'));
  // Real partial-compiled Angular libraries this app consumes (the ones that previously triggered
  // the JIT fallback): resolved from the monorepo.
  for (const name of [
    '@angular/core',
    '@angular/common',
    '@angular/router',
    '@angular/platform-browser',
    'rxjs',
    'tslib',
  ]) {
    linkInto(nm, name, resolvePkgDir(name));
  }
}

// ---------------------------------------------------------------------------
// Step 1: build the @treaty/ts-vite and @treaty/vite plugin dists from current source.
//
// `@treaty/ts-vite` is the shared linker owner (built to its `dist/index.js`, which its package.json
// `main` and the @treaty/vite bundle resolve). `@treaty/vite` is bundled with `@treaty/ts-vite`
// kept EXTERNAL, so the runtime `import { createLinkPartialPlugins } from '@treaty/ts-vite'` resolves
// through the symlink farm - proving the cross-package wiring, not an inlined copy.
// ---------------------------------------------------------------------------
function esbuild(entry, out) {
  const bin = req.resolve('esbuild/bin/esbuild', { paths: [repoRoot] });
  mkdirSync(dirname(out), { recursive: true });
  const args = [
    bin,
    entry,
    '--bundle',
    '--platform=node',
    '--format=cjs',
    '--target=node20',
    // Keep every dependency external (resolved through the symlink farm), so the runtime
    // `import { createLinkPartialPlugins } from '@treaty/ts-vite'` resolves the shared linker
    // package rather than an inlined copy - proving the cross-package wiring, not a duplicate.
    '--packages=external',
    `--outfile=${out}`,
  ];
  execFileSync(process.execPath, args, { stdio: ['ignore', 'ignore', 'inherit'] });
  return out;
}

// `@treaty/ts-vite` is a CommonJS package, so its `main` (`./src/index.js`) must resolve; we emit the
// bundle there for the symlink-farm `require('@treaty/ts-vite')` to find. `@treaty/vite` declares
// `"type": "module"`, so a `.js` CJS bundle would be (mis)treated as ESM - emit it as `.cjs` (the e2e
// harness `require`s it directly) to test the wiring without disturbing the package's ESM dist.
// `@treaty/ts-vite`'s package.json `main` is `./dist/index.js`, so the symlink-farm
// `require('@treaty/ts-vite')` resolves the built bundle. `@treaty/vite` declares `"type": "module"`,
// so a `.js` CJS bundle would be (mis)treated as ESM - emit it as `.cjs` (the e2e harness `require`s
// it directly) to test the wiring without disturbing the package's ESM dist.
const tsViteDist = join(repoRoot, 'libs/typescript/vite/dist/index.js');
const treatyViteDist = join(repoRoot, 'libs/treaty/vite/dist/index.e2e.cjs');

function buildPluginDists() {
  esbuild(join(repoRoot, 'libs/typescript/vite/src/index.ts'), tsViteDist);
  esbuild(join(repoRoot, 'libs/treaty/vite/src/index.ts'), treatyViteDist);
}

// ---------------------------------------------------------------------------
// Step 2: assert `treaty()` returns the shared linker plugins alongside the authoring plugin.
//
// This is the wiring that was MISSING and caused the bug: `@treaty/vite` returned ONLY its authoring
// plugin, so partial @angular libraries reached the browser un-linked. The default export now
// returns an array: the authoring plugin plus `createLinkPartialPlugins()` from `@treaty/ts-vite`.
// ---------------------------------------------------------------------------
function loadPluginNames(plugins) {
  return plugins.flat(Infinity).filter(Boolean).map(p => p && p.name);
}

function assertWiring() {
  const treaty = req(treatyViteDist).default;
  check('@treaty/vite default export is the treaty() factory', typeof treaty === 'function');
  const plugins = treaty({ sourceMap: true });
  check('treaty() returns an array of plugins (authoring + linker)', Array.isArray(plugins));
  const names = loadPluginNames(plugins);
  check('treaty() includes the authoring plugin', names.includes('treaty:vite'), names.join(', '));
  check(
    'treaty() includes the partial-declaration transform (was missing - the bug)',
    names.includes('vite-plugin-treaty-link-partial'),
    names.join(', '),
  );
  check(
    'treaty() includes the optimizeDeps linker config (excludes @angular/compiler)',
    names.includes('vite-plugin-treaty-link-partial-config'),
    names.join(', '),
  );
  check(
    'treaty() includes the dev-serve no-@angular/compiler index.html guard',
    names.includes('vite-plugin-treaty-dev-serve'),
    names.join(', '),
  );
  return plugins.flat(Infinity).filter(Boolean);
}

// ---------------------------------------------------------------------------
// Step 3: drive the wired linker over a real partial @angular/common module and assert it links to
// AOT with zero residual and no JIT/Babel dependency - exactly what the running app's module graph
// would do, but isolated to a single representative library module for a fast, deterministic proof.
// ---------------------------------------------------------------------------
async function assertLinks(plugins) {
  const linkPlugin = plugins.find(p => p.name === 'vite-plugin-treaty-link-partial');
  if (!check('linker transform plugin present', Boolean(linkPlugin?.transform))) return;

  const commonDir = resolvePkgDir('@angular/common');
  const fesm = commonDir
    ? readdirSync(join(commonDir, 'fesm2022'), { recursive: true }).find(
        f => typeof f === 'string' && f.endsWith('.mjs'),
      )
    : null;
  if (!check('real @angular/common fesm module found', Boolean(fesm))) return;

  const id = join(commonDir, 'fesm2022', fesm);
  // The linker's `isPartialModule` guard requires a `node_modules` segment in the id.
  const idForLinker = id.includes('node_modules')
    ? id
    : join('node_modules', '@angular/common', 'fesm2022', fesm);
  const source = readFileSync(id, 'utf-8');
  check(
    'fixture is genuinely partial-compiled (ships ɵɵngDeclare*)',
    /ɵɵngDeclare[A-Za-z]+\s*\(/.test(source),
  );

  const handler = typeof linkPlugin.transform === 'function'
    ? linkPlugin.transform
    : linkPlugin.transform.handler;
  const transformed = await handler.call({}, source, idForLinker);
  const code = transformed?.code ?? '';
  check(
    'wired @treaty/vite links partial @angular/common to AOT (zero residual ɵɵngDeclare)',
    code.length > 0 && !/ɵɵngDeclare[A-Za-z]+\s*\(/.test(code),
    `len=${code.length} residual=${(code.match(/ɵɵngDeclare[A-Za-z]+\s*\(/g) || []).length}`,
  );
  check(
    'linked output contains AOT Ivy defs (ɵɵdefine*)',
    /ɵɵdefine[A-Za-z]+\s*\(/.test(code),
  );
  check(
    'linked output does NOT import @angular/compiler (no JIT)',
    !/from\s*['"]@angular\/compiler['"]|import\(\s*['"]@angular\/compiler['"]|require\(\s*['"]@angular\/compiler['"]/.test(code),
  );
  check(
    'linked output contains no @angular/compiler-cli / @babel/core (Rust-only)',
    !/@angular\/compiler-cli|@babel\/core/.test(code),
  );
}

// ---------------------------------------------------------------------------
// Step 4: the dev-serve index.html guard injects NO @angular/compiler script (no JIT in dev either).
// ---------------------------------------------------------------------------
function assertDevServeGuard(plugins) {
  const guard = plugins.find(p => p.name === 'vite-plugin-treaty-dev-serve');
  if (!check('dev-serve index.html guard present', Boolean(guard))) return;
  check('dev-serve guard applies only on serve', guard.apply === 'serve');

  const hook = guard.transformIndexHtml;
  const handler = typeof hook === 'function' ? hook : hook?.handler;
  if (!check('dev-serve guard has a transformIndexHtml handler', typeof handler === 'function')) return;

  const injected =
    '<!doctype html><html><head><script type="module" src="/@angular/compiler"></script></head>' +
    '<body><app-root></app-root></body></html>';
  const out = handler.call({}, injected) ?? injected;
  check(
    'dev-serve guard strips any injected @angular/compiler script (no JIT in dev)',
    !/@angular\/compiler/.test(out),
  );
}

// ---------------------------------------------------------------------------
// Steps 5-7 shared fixture. A minimal real Vite project under d:\tmp whose single entry imports the
// exact partial `@angular` modules that crashed the everything-app: `@angular/common` (the
// `_PlatformLocation` source) and `@angular/platform-browser`. It uses the WIRED `treaty()` plugin
// from the built dist, so this drives the same plugin chain the everything-app's `vite.config.ts`
// does - through real `vite build` / `vite dev`, not a hand-rolled transform call.
// ---------------------------------------------------------------------------
const fixtureDir = join(repoRoot, 'examples', 'everything-app', '.e2e-vite-fixture');

function writeFixture() {
  rmSync(fixtureDir, { recursive: true, force: true });
  mkdirSync(fixtureDir, { recursive: true });
  // node_modules for the fixture is a junction to the everything-app's wired farm (which already
  // links @treaty/ts-vite, @treaty/authoring-node and the real partial @angular libs). The fixture
  // resolves `@angular/*`, `vite`, etc. through it - no @angular/compiler is present in that farm.
  const farm = join(here, 'node_modules');
  linkInto(fixtureDir, '@angular/common', resolvePkgDir('@angular/common'));
  linkInto(fixtureDir, '@angular/platform-browser', resolvePkgDir('@angular/platform-browser'));
  linkInto(fixtureDir, '@angular/core', resolvePkgDir('@angular/core'));
  linkInto(fixtureDir, '@angular/router', resolvePkgDir('@angular/router'));
  linkInto(fixtureDir, 'rxjs', resolvePkgDir('rxjs'));
  linkInto(fixtureDir, 'tslib', resolvePkgDir('tslib'));
  linkInto(fixtureDir, 'vite', resolvePkgDir('vite'));
  linkInto(fixtureDir, 'esbuild', resolvePkgDir('esbuild'));
  // The wired plugin built in Step 1 (CJS), imported via a relative file path so the fixture's Vite
  // config loads the exact dist this harness produced.
  const pluginPath = treatyViteDist.replace(/\\/g, '/');
  // The fixture entry imports the real partial @angular modules straight from their fesm so the
  // module graph contains genuinely partial-compiled code the linker must de-partial. The
  // `globalThis.__seen` sinks keep tree-shaking from dropping the imports in the prod build.
  writeFileSync(
    join(fixtureDir, 'entry.js'),
    [
      "import * as common from '@angular/common';",
      "import * as platform from '@angular/platform-browser';",
      'globalThis.__seen = [common, platform];',
      "document.querySelector('#root')?.append('linked');",
      '',
    ].join('\n'),
  );
  writeFileSync(
    join(fixtureDir, 'index.html'),
    [
      '<!doctype html><html><head>',
      '<title>treaty linker fixture</title>',
      '</head><body><div id="root"></div>',
      '<script type="module" src="/entry.js"></script>',
      '</body></html>',
      '',
    ].join('\n'),
  );
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
      // `ngDevMode: false` is the standard Angular production define: it tree-shakes the dev-only
      // `_debug_node-chunk` (whose side-effect `import "@angular/compiler"` is the JIT facade) exactly
      // as a real production build does, so the shipped bundle never imports @angular/compiler.
      '  define: { ngDevMode: false, ngI18nClosureMode: false },',
      '  build: { target: "es2022", minify: false, outDir: "dist", emptyOutDir: true,',
      '    rollupOptions: { onwarn() {} } },',
      '};',
      '',
    ].join('\n'),
  );
  // Surface the farm so a stray bare resolve still finds the real libs even if the per-name junctions
  // above are insufficient on some layouts.
  void farm;
  return join(fixtureDir, 'vite.config.mjs');
}

function viteBin() {
  // `vite`'s `exports` map does not expose `./bin/vite.js`, so resolve it relative to the package
  // directory + the `bin` entry in its package.json rather than through the (blocked) subpath.
  const pkgDir = dirname(req.resolve('vite/package.json', { paths: [repoRoot] }));
  const pkg = req(join(pkgDir, 'package.json'));
  const binRel = typeof pkg.bin === 'string' ? pkg.bin : pkg.bin.vite;
  return join(pkgDir, binRel);
}

// ---------------------------------------------------------------------------
// Step 5: PROD - a real `vite build` over the fixture, then scan the emitted bundle.
// ---------------------------------------------------------------------------
function assertProdBuild(configPath) {
  try {
    execFileSync(process.execPath, [viteBin(), 'build', '-c', configPath], {
      cwd: fixtureDir,
      stdio: ['ignore', 'ignore', 'inherit'],
    });
  } catch (err) {
    check('real `vite build` completes', false, String(err?.message ?? err));
    return;
  }
  check('real `vite build` completes', true);

  const assetsDir = join(fixtureDir, 'dist', 'assets');
  const jsFiles = existsSync(assetsDir)
    ? readdirSync(assetsDir).filter(f => f.endsWith('.js'))
    : [];
  if (!check('build emitted JS bundle(s)', jsFiles.length > 0)) return;
  const bundle = jsFiles.map(f => readFileSync(join(assetsDir, f), 'utf-8')).join('\n');

  const residual = (bundle.match(/ɵɵngDeclare[A-Za-z]+\s*\(/g) || []).length;
  check(
    'PROD bundle has ZERO residual ɵɵngDeclare (partial @angular linked to AOT)',
    residual === 0,
    `residual=${residual}`,
  );
  check(
    'PROD bundle imports NO @angular/compiler anywhere (no JIT)',
    !/@angular\/compiler\b/.test(bundle),
  );
  check(
    'PROD bundle contains AOT Ivy defs (ɵɵdefine*)',
    /ɵɵdefine[A-Za-z]+\s*\(/.test(bundle),
  );
  check(
    'PROD bundle contains ɵɵdefineInjectable (the _PlatformLocation AOT def)',
    /ɵɵdefineInjectable\s*\(/.test(bundle),
  );
}

// ---------------------------------------------------------------------------
// Step 6: DEV - a real `vite dev` server; fetch the served partial @angular dep modules + index.html.
// ---------------------------------------------------------------------------
async function assertDevServe(configPath) {
  const port = 5319;
  const child = spawn(
    process.execPath,
    [viteBin(), '-c', configPath, '--port', String(port), '--strictPort'],
    { cwd: fixtureDir, stdio: ['ignore', 'pipe', 'pipe'] },
  );
  let stderr = '';
  child.stderr?.on('data', d => (stderr += d));

  const base = `http://localhost:${port}`;
  // Poll the server until index.html is served (dev server + dep optimizer ready).
  async function waitReady(timeoutMs) {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      try {
        const r = await fetch(`${base}/index.html`);
        if (r.ok) return true;
      } catch {
        /* not up yet */
      }
      await new Promise(r => setTimeout(r, 200));
    }
    return false;
  }

  try {
    const ready = await waitReady(60_000);
    if (!check('`vite dev` server is up', ready, stderr.slice(-400))) return;

    // The served index.html must carry NO @angular/compiler script (the dev-serve guard).
    const html = await (await fetch(`${base}/index.html`)).text();
    check(
      'DEV index.html injects NO @angular/compiler script (no JIT in dev)',
      !/@angular\/compiler/.test(html),
    );

    // Fetch the served @angular dep modules. Vite serves bare deps under /node_modules/.vite/deps/
    // (prebundled) or via the resolved id; the dep-optimizer path is the one that prebundles the
    // partial libs. Request the resolved module ids the browser would, and assert no residual partial.
    async function fetchModule(spec) {
      // Ask Vite to resolve+serve the bare module the way the browser does.
      const urls = [
        `${base}/@id/${spec}`,
        `${base}/node_modules/${spec}/fesm2022/${spec.split('/').pop()}.mjs`,
      ];
      for (const u of urls) {
        try {
          const r = await fetch(u);
          if (r.ok) {
            const body = await r.text();
            if (body.length > 0) return body;
          }
        } catch {
          /* try next */
        }
      }
      return null;
    }

    const common = await fetchModule('@angular/common');
    if (check('DEV served @angular/common (the _PlatformLocation source) module fetched', Boolean(common))) {
      check(
        'DEV served @angular/common has NO residual ɵɵngDeclare (linked on the fly)',
        !/ɵɵngDeclare[A-Za-z]+\s*\(/.test(common),
        `residual=${(common.match(/ɵɵngDeclare[A-Za-z]+\s*\(/g) || []).length}`,
      );
      check('DEV served @angular/common imports no @angular/compiler', !/@angular\/compiler\b/.test(common));
    }

    const platform = await fetchModule('@angular/platform-browser');
    if (check('DEV served @angular/platform-browser module fetched', Boolean(platform))) {
      check(
        'DEV served @angular/platform-browser has NO residual ɵɵngDeclare (linked on the fly)',
        !/ɵɵngDeclare[A-Za-z]+\s*\(/.test(platform),
        `residual=${(platform.match(/ɵɵngDeclare[A-Za-z]+\s*\(/g) || []).length}`,
      );
    }
  } finally {
    // Graceful teardown: signal the dev server and wait for it to exit so the harness never races
    // the libuv event loop on Windows (an abrupt kill can trip an internal handle-close assertion).
    await new Promise(resolve => {
      let done = false;
      const finish = () => {
        if (!done) {
          done = true;
          resolve();
        }
      };
      child.once('exit', finish);
      child.kill();
      setTimeout(() => {
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
// Step 7: BOOT - evaluate the linked `_PlatformLocation` module against a faithful `@angular/core`
// stub whose `getCompilerFacade` throws Angular's real JIT error. An un-linked `ɵɵngDeclare*` call
// invokes `getCompilerFacade` at module-eval time and throws; a correctly-linked AOT `ɵɵdefine*`
// never does. So the linked module must evaluate WITHOUT the "_PlatformLocation needs JIT" crash.
// ---------------------------------------------------------------------------
async function assertBootNoJit(plugins) {
  const linkPlugin = plugins.find(p => p.name === 'vite-plugin-treaty-link-partial');
  const handler =
    typeof linkPlugin?.transform === 'function' ? linkPlugin.transform : linkPlugin?.transform?.handler;
  if (!check('linker transform available for boot test', typeof handler === 'function')) return;

  // The `_PlatformLocation` source chunk in @angular/common.
  const commonDir = resolvePkgDir('@angular/common');
  const chunkPath = join(commonDir, 'fesm2022', '_platform_location-chunk.mjs');
  if (!check('@angular/common _platform_location chunk present', existsSync(chunkPath))) return;
  const source = readFileSync(chunkPath, 'utf-8');
  check(
    'boot fixture is genuinely partial (_PlatformLocation ships ɵɵngDeclare*)',
    /ɵɵngDeclare[A-Za-z]+\s*\(/.test(source) && /PlatformLocation/.test(source),
  );

  const idForLinker = join('node_modules', '@angular', 'common', 'fesm2022', '_platform_location-chunk.mjs');
  const linked = (await handler.call({}, source, idForLinker))?.code ?? '';
  if (!check('boot: _PlatformLocation linked to zero residual', linked.length > 0 && !/ɵɵngDeclare[A-Za-z]+\s*\(/.test(linked))) {
    return;
  }

  // The `@angular/core` Ivy-runtime stub: a single namespace module whose every member is a callable
  // no-op EXCEPT `getCompilerFacade`, which throws Angular's REAL JIT error. The linked AOT module
  // calls only the `ɵɵdefine*` primitives (all no-ops here); it must NEVER reach getCompilerFacade. A
  // residual un-linked `ɵɵngDeclare*` would route through getCompilerFacade and throw on eval - which
  // is exactly the everything-app crash this asserts is gone.
  const JIT_ERROR =
    "The Injectable '_PlatformLocation' needs to be compiled using the JIT compiler, " +
    "but '@angular/compiler' is not available.";
  mkdirSync(fixtureDir, { recursive: true });
  const coreStubPath = join(fixtureDir, 'core-stub.mjs');
  writeFileSync(
    coreStubPath,
    [
      `const JIT_ERROR = ${JSON.stringify(JIT_ERROR)};`,
      'export function getCompilerFacade() { throw new Error(JIT_ERROR); }',
      // A regular (constructable) function: linked AOT code both CALLS primitives (ɵɵdefineInjectable)
      // and `new`s classes (InjectionToken), so each member must work as a fn AND a constructor.
      'function noop() { return {}; }',
      // The faithful JIT trap: in real @angular/core, every `ɵɵngDeclare*` primitive routes through
      // getCompilerFacade (the JIT compiler). We mirror that exactly - any `ɵɵngDeclare*` member
      // THROWS the real JIT error when invoked. So an UN-linked module (which still calls
      // `ɵɵngDeclare*` at class-eval) throws, while a linked AOT module (which calls only `ɵɵdefine*`,
      // a no-op here) does not. This is what makes the boot assertion a real test, not a tautology.
      'function ngDeclare() { return getCompilerFacade(); }',
      'const ns = new Proxy(function () {}, {',
      "  get(_t, prop) {",
      "    if (prop === 'getCompilerFacade') return getCompilerFacade;",
      "    if (typeof prop === 'string' && prop.startsWith('ɵɵngDeclare')) return ngDeclare;",
      '    return noop;',
      '  },',
      '  apply() { return {}; },',
      '  construct() { return {}; },',
      '});',
      'export default ns;',
      '',
    ].join('\n'),
  );
  // A generic Ivy no-op namespace for every OTHER bare dependency (other @angular/* chunks, rxjs,
  // tslib): each accessed name is a callable no-op, so the linked module evaluates without pulling
  // real dependencies. None of these can throw, so reaching the JIT error can only come from core.
  const noopModPath = join(fixtureDir, 'ng-noop.mjs');
  writeFileSync(
    noopModPath,
    [
      'function noop() { return {}; }',
      'const ns = new Proxy(function () {}, { get: () => noop, apply: () => ({}), construct: () => ({}) });',
      'export default ns;',
      '',
    ].join('\n'),
  );

  const coreUrl = pathToFileURL(coreStubPath).href;
  const noopUrl = pathToFileURL(noopModPath).href;

  // Rewrite each `import ... from '<spec>'` in the linked module into a namespace import bound to the
  // right stub, plus a destructure for any named/default bindings. Static named imports of names a
  // module does not export throw at link time in ESM; routing through `import * as __ns` + a Proxy
  // destructure means any name the linked code references resolves to a no-op without that error.
  let nsCount = 0;
  function stubFor(spec) {
    if (spec === '@angular/core') return coreUrl;
    if (spec.startsWith('@angular/') || spec === 'rxjs' || spec === 'tslib' || spec.startsWith('rxjs/'))
      return noopUrl;
    return null;
  }
  // Matches: import <clause> from '<spec>';   and bare:  import '<spec>';
  const importRe = /import\s+(?:([^'"]*?)\s+from\s+)?['"]([^'"]+)['"];?/g;
  const rewritten = linked.replace(importRe, (full, clause, spec) => {
    const url = stubFor(spec);
    if (url === null) return full; // leave non-stubbed (none expected for this chunk)
    if (clause === undefined || clause.trim() === '') {
      // Bare side-effect import: rewrite the specifier to the stub so it resolves.
      return `import ${JSON.stringify(url)};`;
    }
    const nsName = `__ns${nsCount++}`;
    // The stub module's DEFAULT export is the Proxy (every accessed member -> no-op, except the core
    // stub's getCompilerFacade). An ESM namespace object only carries declared exports, so bind every
    // alias/destructure to the default Proxy (`.default`) - that is what answers `i0.ɵɵdefineInjectable`.
    const lines = [`import * as ${nsName} from ${JSON.stringify(url)};`, `const __p${nsName} = ${nsName}.default;`];
    const proxy = `__p${nsName}`;
    const c = clause.trim();
    const braceMatch = c.match(/\{([^}]*)\}/);
    const named = braceMatch ? braceMatch[1] : '';
    const beforeBrace = c.replace(/\{[^}]*\}/, '').replace(/,/g, ' ').trim();
    const nsAlias = beforeBrace.match(/\*\s+as\s+([A-Za-z_$][\w$]*)/);
    const defAlias = nsAlias ? null : beforeBrace.split(/\s+/).filter(Boolean)[0];
    if (nsAlias) lines.push(`const ${nsAlias[1]} = ${proxy};`);
    if (defAlias) lines.push(`const ${defAlias} = ${proxy};`);
    if (named.trim()) {
      // `a, b as c` -> destructure `{ a, b: c }` off the Proxy (missing names => callable no-op).
      const parts = named
        .split(',')
        .map(s => s.trim())
        .filter(Boolean)
        .map(s => {
          const m = s.match(/^(.+?)\s+as\s+(.+)$/);
          return m ? `${m[1].trim()}: ${m[2].trim()}` : s;
        });
      lines.push(`const { ${parts.join(', ')} } = ${proxy};`);
    }
    return lines.join('\n');
  });

  const bootModPath = join(fixtureDir, 'boot-platform-location.mjs');
  writeFileSync(bootModPath, rewritten);

  let threw = null;
  try {
    await import(pathToFileURL(bootModPath).href);
  } catch (err) {
    threw = err;
  }

  const isJitError = threw != null && /needs to be compiled using the JIT compiler/.test(String(threw?.message ?? threw));
  check(
    'BOOT: linked _PlatformLocation evaluates WITHOUT the "needs JIT / @angular/compiler not available" crash',
    !isJitError,
    threw ? `unexpected: ${String(threw?.message ?? threw).slice(0, 160)}` : 'evaluated clean',
  );
  // Sanity: a correctly-linked module should evaluate with no throw at all under the stub.
  check('BOOT: linked _PlatformLocation evaluates with no error under the Ivy stub', threw == null,
    threw ? String(threw?.message ?? threw).slice(0, 160) : undefined);
}

// ---------------------------------------------------------------------------
async function main() {
  console.log('== Step 0: wire local node_modules ==');
  wireNodeModules();
  check('local node_modules wired', existsSync(join(here, 'node_modules/@angular/common')));

  console.log('== Step 1: build @treaty/ts-vite + @treaty/vite plugin dists ==');
  buildPluginDists();
  check('@treaty/ts-vite dist built', existsSync(tsViteDist));
  check('@treaty/vite dist built', existsSync(treatyViteDist));

  console.log('== Step 2: treaty() wires the shared linker plugins ==');
  const plugins = assertWiring();

  console.log('== Step 3: wired linker de-partials real @angular/common (no JIT, no Babel) ==');
  await assertLinks(plugins);

  console.log('== Step 4: dev-serve injects no @angular/compiler (no JIT in dev) ==');
  assertDevServeGuard(plugins);

  console.log('== Step 5: PROD - real `vite build`, bundle has zero ɵɵngDeclare + AOT defs ==');
  const configPath = writeFixture();
  assertProdBuild(configPath);

  console.log('== Step 6: DEV - real `vite dev`, served @angular/common + platform-browser linked ==');
  await assertDevServe(configPath);

  console.log('== Step 7: BOOT - linked _PlatformLocation evaluates with no JIT/@angular/compiler crash ==');
  await assertBootNoJit(plugins);

  rmSync(fixtureDir, { recursive: true, force: true });

  console.log('');
  if (failures.length) {
    console.error(`E2E FAILED: ${failures.length} assertion(s): ${failures.join('; ')}`);
    process.exit(1);
  }
  console.log(
    'E2E PASSED: @treaty/vite wires the shared Rust linker; partial @angular libs link to AOT (no JIT, no @angular/compiler).',
  );
}

main().catch(err => {
  console.error('E2E ERROR:', err);
  process.exit(1);
});
