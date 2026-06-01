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
// Usage:  node examples/everything-app/e2e.mjs
// Exit code 0 on success, 1 on any failed assertion.

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
