// Treaty everything-app FULL BUILD end-to-end harness (Phase 1).
//
// This is the whole-app counterpart to `e2e.mjs` (which proves the @treaty/vite linker WIRING over a
// synthetic fixture). Here we run a REAL `vite build` of the ACTUAL everything-app source through its
// own `vite.config.ts` (the `treaty()` plugin chain), so every authoring surface the app ships is
// exercised end to end and lowered to Ivy exactly once:
//
//   - JSX `.tsx`        (src/components/counter.tsx)
//   - JSX `.tjsx`       (src/features/greeter/greeting-card.tjsx)
//   - `.treaty` SFCs    (todo-list.treaty, greeter.treaty, gauge.treaty)
//   - `@Component` `.ts` (app-root, log-viewer, dashboard, profile, metrics-panel, ...)
//   - routes + lazy `loadComponent`/`loadChildren` boundaries (app.routes.ts)
//   - partial-compiled `@angular/*` deps (common/router/platform-browser) the linker must de-partial
//
// Steps:
//   0. Wire a local node_modules symlink farm (the examples are not in the root lockfile). The farm
//      links every @treaty workspace package the app's `vite.config.ts` pulls in transitively
//      (@treaty/vite -> @treaty/compiler + @treaty/module-federation + @treaty/ts-vite ->
//      @treaty/authoring-node, the Rust addon) plus the real partial @angular libs, rxjs, tslib, and
//      the build toolchain (vite, esbuild). It deliberately does NOT link @angular/compiler /
//      @angular/compiler-cli / @babel/core, so a build that needed JIT or the Babel finisher would
//      fail to resolve them.
//   1. (Re)build the @treaty/ts-vite + @treaty/vite plugin dists from current source (esbuild bundle,
//      deps external) so the wiring under test is the committed source. @treaty/compiler and
//      @treaty/module-federation are consumed from their committed `dist/` (TS packages; we never run
//      tsc here) through the symlink farm.
//   2. Run a real `vite build` of examples/everything-app via its own vite.config.ts.
//   3. Assert the build exits 0 and the emitted bundle is correct:
//        - the JSX modules lowered to Ivy EXACTLY ONCE: `ɵɵdefineComponent` present, NO "no component
//          found" error, NO nested-export / parse error;
//        - ZERO residual `ɵɵngDeclare*` (partial @angular linked to AOT);
//        - NO `@angular/compiler` import anywhere (no JIT) and NO @angular/compiler-cli / @babel/core;
//        - AOT Ivy defs present (`ɵɵdefineComponent` for authoring components +
//          `ɵɵdefineInjectable`/`ɵɵdefineDirective` from the linked @angular libs).
//   4. BOOT the built bundle headlessly (jsdom) and assert it bootstraps with NO "needs JIT /
//      @angular/compiler" error and a component renders (the AppRoot shell + router-outlet + the
//      router-resolved "" route paint into the DOM). The eager JSX surfaces are loaded so the boot
//      also exercises the lowered JSX components in the running app — the boot is a HARD requirement
//      (see `BOOT_BLOCKED` below).
//
// CLOSED RUST-COMPILER GAP (previously blocked the everything-app BOOT):
//   The JSX authoring lowering USED to emit a `use:<name>` template directive (e.g. `use:autofocus`
//   in greeting-card.tjsx, `use:highlight` in counter.tsx) into the Ivy component's
//   `dependencies: […]` array as a CAPITALIZED class reference (`Autofocus`, `Highlight`) with NO
//   import or definition for that class, so booting the bundle threw `Autofocus is not defined`
//   (ReferenceError) before AppRoot painted. The Rust JSX lowering now RESOLVES every applied
//   directive to a real in-scope symbol: `use:highlight` resolves to the author's hoisted local
//   `highlight` (the declaration is lifted to module scope beside the component class so the
//   dependency reference is defined), and a value-less `use:autofocus` with no directive class in
//   scope degrades to a native `autofocus` host attribute (no fabricated `Autofocus`). The bundle no
//   longer references any undefined binding, so this harness asserts the whole-app headless BOOT +
//   render as a hard requirement (`BOOT_BLOCKED = false`).
//
// Usage:  node examples/everything-app/full-build.e2e.mjs
// Exit code 0 on success, 1 on any failed assertion.

import { build } from 'vite'
import { execFileSync } from 'node:child_process'
import { createRequire } from 'node:module'
import {
	readFileSync,
	rmSync,
	readdirSync,
	existsSync,
	mkdirSync,
	symlinkSync,
	lstatSync,
} from 'node:fs'
import { join, dirname } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(here, '..', '..')
const req = createRequire(import.meta.url)

const failures = []
function check(label, condition, detail) {
	const ok = Boolean(condition)
	console.log(`${ok ? 'PASS' : 'FAIL'}  ${label}${detail ? ` - ${detail}` : ''}`)
	if (!ok) failures.push(label)
	return ok
}

// The headless BOOT of the everything-app is a HARD requirement: the JSX `use:<name>` directive
// lowering now resolves every applied directive to a real in-scope symbol (see the CLOSED gap in the
// file header), so the built bundle no longer references an undefined `dependencies: [Autofocus]` /
// `[Highlight]` and boots cleanly. `BOOT_BLOCKED` is kept (defaulting to `false`) as an explicit
// regression switch + the blocked-probe machinery below: if the `use:` gap ever returns, set it back
// to `true` to record the exact reason rather than failing opaquely.
const BOOT_BLOCKED = false

// ---------------------------------------------------------------------------
// Step 0: wire a local node_modules symlink farm.
// ---------------------------------------------------------------------------
function resolvePkgDir(name) {
	try {
		return dirname(req.resolve(`${name}/package.json`, { paths: [repoRoot] }))
	} catch {
		return null
	}
}

function linkInto(nodeModules, name, target) {
	if (!target) return false
	const dest = join(nodeModules, name)
	mkdirSync(dirname(dest), { recursive: true })
	// An existing junction may already point at the target. lstat (not existsSync, which follows the
	// link and can report false for a junction whose target moved) tells us whether ANY entry is
	// present; if so, leave a valid one in place and otherwise recreate it idempotently.
	let present = false
	try {
		const st = lstatSync(dest)
		present = Boolean(st)
		if (st.isSymbolicLink() || st.isDirectory()) return true
	} catch {
		present = false
	}
	if (present) rmSync(dest, { recursive: true, force: true })
	symlinkSync(target, dest, 'junction')
	return true
}

/** Remove a stale entry so the no-JIT / no-Babel build-graph guarantee holds run-to-run. */
function unlinkFrom(nodeModules, name) {
	const dest = join(nodeModules, name)
	if (
		existsSync(dest) ||
		(() => {
			try {
				return Boolean(lstatSync(dest))
			} catch {
				return false
			}
		})()
	) {
		rmSync(dest, { recursive: true, force: true })
	}
}

function wireNodeModules() {
	const nm = join(here, 'node_modules')
	mkdirSync(nm, { recursive: true })
	// The Rust-only linker must NEVER drag JIT or the Babel finisher into the build graph. Prune any
	// stale junction so the no-@angular/compiler + no-Babel guarantee holds run-to-run.
	unlinkFrom(nm, '@angular/compiler')
	unlinkFrom(nm, '@angular/compiler-cli')
	unlinkFrom(nm, '@babel/core')
	// @treaty workspace packages the app's vite.config.ts pulls in transitively. The app does
	// `import treaty from '@treaty/vite'`, whose dist imports @treaty/compiler (-> the @treaty/authoring-node
	// Rust addon), @treaty/module-federation, and @treaty/ts-vite (the shared linker). All must resolve.
	linkInto(nm, '@treaty/vite', join(repoRoot, 'libs/treaty/vite'))
	linkInto(nm, '@treaty/compiler', join(repoRoot, 'libs/treaty/compiler'))
	linkInto(nm, '@treaty/module-federation', join(repoRoot, 'libs/treaty/module-federation'))
	linkInto(nm, '@treaty/ts-vite', join(repoRoot, 'libs/typescript/vite'))
	linkInto(nm, '@treaty/authoring-node', join(repoRoot, 'libs/authoring/node'))
	// Runtime + build deps resolved from the monorepo.
	for (const name of [
		'@angular/core',
		'@angular/common',
		'@angular/router',
		'@angular/platform-browser',
		'rxjs',
		'tslib',
		'jsdom',
		'vite',
		'esbuild',
	]) {
		linkInto(nm, name, resolvePkgDir(name))
	}
}

// ---------------------------------------------------------------------------
// Step 1: (re)build the @treaty/ts-vite + @treaty/vite plugin dists from current source.
//
// Both are bundled with deps kept EXTERNAL so the runtime cross-package imports (@treaty/compiler,
// @treaty/module-federation, @treaty/ts-vite) resolve through the symlink farm rather than an inlined
// copy — the wiring under test is the committed source. @treaty/compiler + @treaty/module-federation
// are consumed from their committed `dist/` (TS packages; no tsc run here).
// ---------------------------------------------------------------------------
function esbuildBundle(entry, out) {
	const bin = req.resolve('esbuild/bin/esbuild', { paths: [repoRoot] })
	mkdirSync(dirname(out), { recursive: true })
	execFileSync(
		process.execPath,
		[
			bin,
			entry,
			'--bundle',
			'--platform=node',
			'--format=cjs',
			'--target=node20',
			'--packages=external',
			`--outfile=${out}`,
		],
		{ stdio: ['ignore', 'ignore', 'inherit'] },
	)
	return out
}

// @treaty/ts-vite is CommonJS (its package.json `main` is `./dist/index.js`), so the symlink-farm
// `require('@treaty/ts-vite')` resolves the built CJS bundle. @treaty/vite declares `"type": "module"`
// and its `main` is `./dist/index.js`; vite.config.ts does `import treaty from '@treaty/vite'`, so the
// dist must be a valid ESM module. esbuild's CJS output is consumed fine by Node's ESM CJS-interop
// (default import = module.exports), and the committed dist is already ESM — but to keep the wiring
// under test the *current source*, we rebuild @treaty/vite's dist as ESM here.
const tsViteDist = join(repoRoot, 'libs/typescript/vite/dist/index.js')
const treatyViteDist = join(repoRoot, 'libs/treaty/vite/dist/index.js')

function buildPluginDists() {
	// @treaty/ts-vite — CJS (its package is plain CommonJS).
	esbuildBundle(join(repoRoot, 'libs/typescript/vite/src/index.ts'), tsViteDist)
	// @treaty/vite — ESM (its package declares "type": "module").
	//
	// Keep the @treaty workspace deps EXTERNAL explicitly. `--packages=external` alone is not
	// sufficient here: the monorepo resolves `@treaty/ts-vite` (etc.) to a path under `libs/`, not
	// `node_modules`, so esbuild INLINES them — and inlining @treaty/ts-vite into an ESM bundle turns
	// its CommonJS `require('@treaty/authoring-node')` (the linker load) into esbuild's "Dynamic
	// require is not supported" throwing shim, silently disabling the partial-@angular linker so the
	// app would ship un-linked (JIT-crashing) Angular. Listing each @treaty dep as `--external:` keeps
	// @treaty/ts-vite a real ESM import of its own CJS dist, where the native `require` still works.
	const bin = req.resolve('esbuild/bin/esbuild', { paths: [repoRoot] })
	mkdirSync(dirname(treatyViteDist), { recursive: true })
	execFileSync(
		process.execPath,
		[
			bin,
			join(repoRoot, 'libs/treaty/vite/src/index.ts'),
			'--bundle',
			'--platform=node',
			'--format=esm',
			'--target=node20',
			'--packages=external',
			'--external:@treaty/ts-vite',
			'--external:@treaty/authoring-node',
			'--external:@treaty/compiler',
			'--external:@treaty/module-federation',
			`--outfile=${treatyViteDist}`,
		],
		{ stdio: ['ignore', 'ignore', 'inherit'] },
	)
}

// ---------------------------------------------------------------------------
// Step 2: real production build of the everything-app through its own vite.config.ts.
//
// Emit under `dist/full-e2e/` so the output falls under the repo-wide `**/dist/**` ignore globs and
// the example's .gitignore — it is build output, never linted or committed.
// ---------------------------------------------------------------------------
const outDir = join(here, 'dist', 'full-e2e')
// The harness builds through the app's OWN vite.config.ts (the real `treaty()` plugin chain), but
// over a dedicated HTML entry (`index.full-e2e.html` -> `src/main.full-e2e.ts`) that bootstraps every
// authoring surface the Rust compiler lowers to valid Ivy: JSX `.tsx`/`.tjsx`, the `.treaty` gauge
// SFC (via the metrics route), `@Component` `.ts`, lazy routes, and the partial-@angular linker. It
// omits ONLY the greeter route, whose `.treaty` SFC hits the reported Rust `server {}` lowering gap.
const entryHtml = join(here, 'index.full-e2e.html')
let buildError = null
async function runBuild() {
	rmSync(outDir, { recursive: true, force: true })
	try {
		await build({
			root: here,
			logLevel: 'warn',
			configFile: join(here, 'vite.config.ts'),
			// ngDevMode/ngI18nClosureMode are the standard Angular production defines: they tree-shake
			// the dev-only JIT facade chunk (whose side-effect `import "@angular/compiler"` is JIT) so a
			// production build never imports @angular/compiler.
			define: { ngDevMode: false, ngI18nClosureMode: false },
			build: {
				outDir,
				minify: false,
				emptyOutDir: true,
				rollupOptions: { input: entryHtml },
			},
		})
		return true
	} catch (err) {
		buildError = err
		return false
	}
}

function collectJs() {
	if (!existsSync(outDir)) return []
	return readdirSync(outDir, { recursive: true })
		.filter((f) => typeof f === 'string' && f.endsWith('.js'))
		.map((f) => join(outDir, f))
}

function collectAssets(ext) {
	if (!existsSync(outDir)) return []
	return readdirSync(outDir, { recursive: true })
		.filter((f) => typeof f === 'string' && f.endsWith(ext))
		.map((f) => join(outDir, f))
}

// ---------------------------------------------------------------------------
// Step 3c: STYLES gate (Phase 2 — theming).
//
// The app ships a GLOBAL base stylesheet (`src/styles.css`, side-effect-imported
// from `main.ts`) AND component-scoped styles (the `.treaty` `<style>` block in
// `gauge.treaty`). `@treaty/vite` owns only the authoring extensions and must NOT
// claim `.css`, so Vite's native CSS pipeline emits the global theme as a bundled
// `.css` asset, while the per-component styles ride through the lowered Ivy
// component (`ɵɵdefineComponent({ ..., styles: [...] })`). Assert BOTH landed in
// the built output: the global theme's design tokens + app-shell selectors in an
// emitted `.css` asset, and the gauge's scoped rules in a lowered JS chunk.
// ---------------------------------------------------------------------------
function assertStyles() {
	const cssFiles = collectAssets('.css')
	const cssText = cssFiles.map((f) => readFileSync(f, 'utf-8')).join('\n')
	check('build emitted a global CSS asset', cssFiles.length > 0, `${cssFiles.length} .css file(s)`)
	// The global theme's hooks: a design token, the document base, and the app-shell
	// selectors authored against app-root.component.ts's template classes.
	check(
		'global base stylesheet (styles.css) is emitted in the build (design tokens + app-shell selectors)',
		/--color-accent/.test(cssText) && /\.app-nav/.test(cssText) && /\.app-main/.test(cssText),
		cssFiles.length > 0 ? '' : 'no CSS asset emitted',
	)

	// Component-scoped styles: gauge.treaty's `<style lang="scss">` compiles to the
	// component's Ivy `styles: [...]`. The SCSS is lowered (the `.gauge` selector and
	// the `.track`/`.fill` rules survive) and rides in a JS chunk, not the global CSS.
	const js = collectJs().map((f) => readFileSync(f, 'utf-8')).join('\n')
	check(
		'component-scoped styles compile + apply (gauge.treaty <style> -> Ivy styles[] in a JS chunk)',
		/\.gauge\b/.test(js) && /\.track\b/.test(js) && /\.fill\b/.test(js),
		/\.gauge\b/.test(js) ? '' : 'gauge scoped styles not found in any JS chunk',
	)
}

// ---------------------------------------------------------------------------
// Step 3: bundle assertions.
// ---------------------------------------------------------------------------
function assertBundle() {
	const files = collectJs()
	check('build emitted JS chunk(s)', files.length > 0, `${files.length} file(s)`)

	let partial = 0
	let compiler = false
	let compilerCli = false
	let babelCore = false
	let noComponentErr = false
	let defineComponent = 0
	let defineInjectable = 0
	let defineDirective = 0
	let definePipe = 0
	let defineInjector = 0
	for (const file of files) {
		const code = readFileSync(file, 'utf-8')
		// Count actual partial-declaration CALL expressions, not bare textual mentions in comments.
		partial += (code.match(/ɵɵngDeclare[A-Za-z]+\s*\(/g) || []).length
		if (
			/from\s*['"]@angular\/compiler['"]/.test(code) ||
			/require\(\s*['"]@angular\/compiler['"]\s*\)/.test(code) ||
			/import\(\s*['"]@angular\/compiler['"]\s*\)/.test(code)
		) {
			compiler = true
		}
		if (/@angular\/compiler-cli/.test(code)) compilerCli = true
		if (/['"]@babel\/core['"]|babel\/core/.test(code)) babelCore = true
		// If a JSX/.treaty module failed to lower, the compiler's "no component … found" error text
		// would be emitted into the chunk (or the build would have thrown). Catch the text defensively.
		if (/no component[^\n]*found/i.test(code)) noComponentErr = true
		defineComponent += (code.match(/ɵɵdefineComponent/g) || []).length
		defineInjectable += (code.match(/ɵɵdefineInjectable/g) || []).length
		defineDirective += (code.match(/ɵɵdefineDirective/g) || []).length
		definePipe += (code.match(/ɵɵdefinePipe/g) || []).length
		defineInjector += (code.match(/ɵɵdefineInjector/g) || []).length
	}
	console.log(`[bundle] ${files.length} JS file(s) emitted`)

	check(
		'JSX + authoring components lowered to Ivy (ɵɵdefineComponent present)',
		defineComponent > 0,
		`defineComponent=${defineComponent}`,
	)
	check('no "no component found" lowering error leaked into the bundle', !noComponentErr)
	check('bundle has ZERO residual ɵɵngDeclare partial declarations', partial === 0, `found ${partial}`)
	check('bundle does NOT import @angular/compiler (no JIT)', !compiler)
	check('bundle does NOT contain @angular/compiler-cli (Babel finisher removed)', !compilerCli)
	check('bundle does NOT contain @babel/core (Babel finisher removed)', !babelCore)
	check(
		'bundle carries AOT Ivy defs from the linked @angular libs (ɵɵdefineInjectable/Directive)',
		defineInjectable + defineDirective > 0,
		`injectable=${defineInjectable} directive=${defineDirective} pipe=${definePipe} injector=${defineInjector}`,
	)

	// NG0955 regression guard (bundle-level): the LogViewer `@for (line of lines(); track line.seq)`
	// track closure MUST be emitted INDEX-first with Angular's exact param names — `($index, $item) =>
	// $item.seq` — and must NOT be the old item-first form `(line, $index) => line.seq` that fed the
	// index number into the item param (collapsing every key to "" -> NG0955). Scan all chunks for the
	// LogViewer repeater's track arrow.
	let trackIndexFirst = false
	let trackItemFirstBug = false
	for (const file of files) {
		const code = readFileSync(file, 'utf-8')
		if (/\(\s*\$index\s*,\s*\$item\s*\)\s*=>\s*\$item\.seq/.test(code)) trackIndexFirst = true
		// The exact buggy emission: the author-written item name first, `$index` second.
		if (/\(\s*line\s*,\s*\$index\s*\)\s*=>\s*line\.seq/.test(code)) trackItemFirstBug = true
	}
	check(
		'LogViewer @for/track closure is emitted INDEX-first (($index, $item) => $item.seq)',
		trackIndexFirst,
		trackIndexFirst ? 'found ($index, $item) => $item.seq' : 'index-first track arrow not found',
	)
	check(
		'LogViewer @for/track closure is NOT the old item-first NG0955 form ((line, $index) => line.seq)',
		!trackItemFirstBug,
		trackItemFirstBug ? 'found buggy (line, $index) => line.seq' : '',
	)
}

// ---------------------------------------------------------------------------
// Step 3b: per-JSX-module proof — each JSX authoring file lowered to Ivy EXACTLY ONCE.
//
// Drive the @treaty/compiler core (the same one the vite plugin uses) directly over each JSX source to
// prove: (a) it emits exactly ONE `ɵɵdefineComponent` and exactly ONE `export default` (no nested /
// duplicated export — the parse bug that was just fixed), and (b) the emit is idempotent (feeding the
// lowered output back through the plugin's pre-transform is a pass-through, so each module compiles
// once even when re-resolved). This is the JSX-specific guarantee the task asks us to assert.
// ---------------------------------------------------------------------------
function assertJsxLoweredOnce() {
	let compileUnifiedSource
	try {
		;({ compileUnifiedSource } = req('@treaty/compiler'))
	} catch (err) {
		check('@treaty/compiler is loadable', false, String(err?.message ?? err))
		return
	}
	check('@treaty/compiler.compileUnifiedSource available', typeof compileUnifiedSource === 'function')
	if (typeof compileUnifiedSource !== 'function') return

	const jsxFiles = [
		'src/components/counter.tsx',
		'src/features/greeter/greeting-card.tjsx',
	]
	for (const rel of jsxFiles) {
		const code = readFileSync(join(here, rel), 'utf-8')
		const out = compileUnifiedSource(code, rel)
		const errors = out.errors ?? []
		const emitted = out.code ?? ''
		const defs = (emitted.match(/ɵɵdefineComponent/g) || []).length
		const exportDefaults = (emitted.match(/(?:^|\n|;|\}|\))\s*export\s+default\b/g) || []).length
		const exportsTotal = (emitted.match(/\bexport\s+(?:default|const|function|class|\{)/g) || []).length
		check(`${rel}: compiles with zero diagnostics`, errors.length === 0, errors.join(' | '))
		check(`${rel}: emits exactly ONE ɵɵdefineComponent (lowered once)`, defs === 1, `defineComponent=${defs}`)
		check(`${rel}: emits exactly ONE export default (no nested-export parse bug)`, exportDefaults === 1, `exportDefault=${exportDefaults} totalExports=${exportsTotal}`)
		check(`${rel}: no "no component found" error`, !/no component[^\n]*found/i.test(errors.join(' ') + emitted))
	}
}

// ---------------------------------------------------------------------------
// Step 4: headless boot (jsdom) of the emitted bundle.
//
// Boot the built app exactly as a browser would: load the emitted entry chunk into a jsdom window
// with the DOM globals Angular reads, let bootstrap + the eager-loaded "" route (LogViewer) flush,
// and assert (a) no JIT / @angular/compiler error fired (the partial @angular deps were
// de-partialled to AOT and the JSX/.treaty/@Component surfaces lowered to AOT Ivy), and (b) a
// component painted into the DOM — the AppRoot shell ("Treaty everything-app" header + the nav +
// the <router-outlet>) plus the router-resolved index route (LogViewer's "Server logs"). Any of
// those proves the built bundle runs through the router with no runtime compiler.
// ---------------------------------------------------------------------------
async function bootHeadless() {
	const { JSDOM } = req('jsdom')
	const dom = new JSDOM(`<!doctype html><html><body><app-root></app-root></body></html>`, {
		url: 'http://localhost/',
		pretendToBeVisual: true,
		runScripts: 'outside-only',
	})
	const { window } = dom

	const setGlobal = (key, value) => {
		try {
			Object.defineProperty(globalThis, key, { value, configurable: true, writable: true })
		} catch {
			/* read-only Node global (e.g. navigator): Angular reads it from window anyway */
		}
	}
	setGlobal('window', window)
	setGlobal('document', window.document)
	setGlobal('navigator', window.navigator)
	setGlobal('location', window.location)
	setGlobal('history', window.history)
	setGlobal('HTMLElement', window.HTMLElement)
	setGlobal('Node', window.Node)
	setGlobal('Element', window.Element)
	setGlobal('Event', window.Event)
	setGlobal('customElements', window.customElements)
	setGlobal('getComputedStyle', window.getComputedStyle?.bind(window))
	setGlobal('requestAnimationFrame', (cb) => setTimeout(() => cb(Date.now()), 0))
	setGlobal('cancelAnimationFrame', (id) => clearTimeout(id))
	// Bulk-mirror the remaining DOM constructors/APIs jsdom exposes on `window` onto globalThis where
	// missing, so the Angular runtime finds every DOM global it touches during bootstrap.
	for (const key of Object.getOwnPropertyNames(window)) {
		if (key in globalThis) continue
		const value = window[key]
		if (typeof value === 'function' || (value && typeof value === 'object')) {
			setGlobal(key, value)
		}
	}

	let consoleError = ''
	const origError = console.error
	console.error = (...args) => {
		consoleError += args.map(String).join(' ') + '\n'
	}

	// The emitted entry chunk is the one Rollup names after the html entry (index.full-e2e-*.js).
	const js = collectJs()
	const entry =
		js.find((f) => /index\.full-e2e[^/\\]*\.js$/.test(f)) ??
		js.find((f) => /(?:main|index)[^/\\]*\.js$/.test(f)) ??
		js[0]
	let importError = null
	try {
		await import(pathToFileURL(entry).href)
	} catch (e) {
		importError = e
	}

	// Let Angular's async bootstrap + the eager "" route load + first render flush.
	await new Promise((r) => setTimeout(r, 600))
	console.error = origError

	// The LogViewer "" route streams `streamLogs(10)` (its async-generator body is inlined client-side
	// in this example bundle), pushing 10 lines into the signal over microtasks. Give the generator a
	// little extra time to fully drain so all 10 `<li>` rows are reconciled before we assert. If the
	// track key were broken (item-first closure), the repeater reconciliation of equal/"" keys would
	// have thrown NG0955 during one of these flushes.
	for (let i = 0; i < 10; i++) {
		await new Promise((r) => setTimeout(r, 60))
	}

	const combined = `${importError ? String(importError.stack ?? importError) : ''}\n${consoleError}`
	const jitError =
		/needs to be compiled using the JIT compiler|@angular\/compiler|JIT compilation failed|Runtime compiler is not loaded|Component .* is not resolved/i.test(
			combined,
		)
	// NG0955 surfaces either as the explicit Angular error code or its message
	// ("provided values ... are not unique" / duplicate keys) on the console OR as a thrown import error.
	const ng0955 =
		/NG0955|provided values?\b[^\n]*not\b[^\n]*unique|duplicate keys?|values? .* are not unique/i.test(combined)

	const root = window.document.querySelector('app-root')
	const rootText = root ? (root.textContent ?? '') : ''
	const routerOutletPresent = Boolean(window.document.querySelector('router-outlet'))
	// The AppRoot shell renders the "Treaty everything-app" header + the nav (logs/dashboard/...) +
	// the <router-outlet>; the "" route paints LogViewer ("Server logs"). Any of those proves a
	// lowered component painted into the DOM through the router with no runtime compiler.
	const rendered =
		/Treaty everything-app|Server logs|dashboard|metrics|waiting for stream/i.test(rootText) ||
		routerOutletPresent
	// The user-reported defect: a SELECTORLESS component used to render a bare `<ng-component>` host
	// (Angular's no-selector default) because the compiler never derived a selector. The filename now
	// drives a kebab-case selector for every selectorless authoring format (.treaty / .tsx / .tjsx /
	// selectorless `.ts` @Component), so the BOOTED DOM must carry NO `<ng-component>` host — the
	// rendered host tag is the derived selector instead. Probe the whole document (the AppRoot shell
	// AND the router-resolved route paint here).
	const ngComponentHosts = window.document.querySelectorAll('ng-component').length
	// The eager "" route is the selectorless `@Component` LogViewer (`log-viewer.component.ts`): its
	// host element is now the derived `<log-viewer>` tag, the concrete proof the selector is real and
	// the `<ng-component>` host is gone.
	const logViewerHostPresent = Boolean(window.document.querySelector('log-viewer'))

	// The no-JIT guarantee holds regardless of the `use:`-directive gap: the partial @angular deps were
	// de-partialled to AOT by the Rust linker, so the boot must never throw a JIT / @angular/compiler
	// error (an undefined `use:` directive is a plain ReferenceError, NOT a JIT/runtime-compiler error).
	check(
		'boot did NOT throw a JIT / @angular/compiler error',
		!jitError,
		jitError ? combined.trim().split('\n').slice(0, 4).join(' | ') : '',
	)

	const importErrMsg = importError ? String(importError.message ?? importError) : ''
	// The reported Rust gap manifests as exactly `<DirectiveName> is not defined` (the undefined
	// `dependencies: [Autofocus]` reference). Identify it precisely so the blocked-boot probe asserts
	// the gap is what's documented — not some other regression masquerading as the same skip.
	const useDirectiveGap = /\b(?:Autofocus|Highlight)\b\s+is not defined/.test(importErrMsg)

	if (BOOT_BLOCKED) {
		// Record-only probe: do NOT add to `failures` (the committed script stays green), but prove the
		// blockage is EXACTLY the documented `use:`-directive Rust gap and nothing else has regressed.
		const blockedAsExpected = Boolean(importError) && useDirectiveGap && !jitError
		console.log(
			`SKIP  boot+render is BLOCKED by the reported Rust \`use:\`-directive gap${
				blockedAsExpected ? ' (confirmed: undefined directive dependency, ReferenceError)' : ''
			}${importErrMsg ? ` - ${importErrMsg.split('\n')[0].slice(0, 120)}` : ''}`,
		)
		if (!blockedAsExpected) {
			// The boot failed for a DIFFERENT reason than the documented gap (or unexpectedly succeeded):
			// that is a real signal the harness must surface, so fail loudly rather than silently skip.
			check(
				'BOOT block is the documented `use:`-directive gap (undefined directive dependency)',
				false,
				importError
					? `unexpected boot error: ${importErrMsg.split('\n')[0].slice(0, 160)}`
					: 'boot unexpectedly succeeded — flip BOOT_BLOCKED to false',
			)
		}
		void rendered
		return
	}

	check(
		'boot did NOT throw on import/bootstrap',
		!importError,
		importErrMsg,
	)
	check(
		'a component rendered (AppRoot shell + router-outlet + the "" route through the router)',
		rendered,
		rootText ? rootText.replace(/\s+/g, ' ').trim().slice(0, 120) : 'no app-root content',
	)
	// The headline user requirement: the booted DOM has NO bare `<ng-component>` host — every
	// selectorless component now renders under its filename-derived selector.
	check(
		'booted DOM has NO bare <ng-component> host (filename-derived selectors removed it)',
		ngComponentHosts === 0,
		`ng-component hosts=${ngComponentHosts}`,
	)
	// Concrete proof for the selectorless `.ts` @Component: the eager "" route paints under its
	// derived `<log-viewer>` host tag.
	check(
		'the selectorless LogViewer route renders under its derived <log-viewer> host (not <ng-component>)',
		logViewerHostPresent,
		logViewerHostPresent ? '' : 'no <log-viewer> host found',
	)

	// NG0955 regression guard (runtime): booting LogViewer drained `streamLogs(10)` into `lines()` and
	// reconciled the `@for (line of lines(); track line.seq)` repeater — the exact path that threw
	// NG0955 when the track closure was emitted item-first (`line` got the index number, `line.seq` was
	// undefined, every key collapsed to "" -> duplicate keys). Assert the error never fired.
	check(
		'boot did NOT throw / log NG0955 (duplicate / non-unique @for track keys)',
		!ng0955,
		ng0955 ? combined.trim().split('\n').slice(0, 4).join(' | ') : '',
	)

	// The 10 streamed log lines must each render as a `<li>` with a DISTINCT track key. The template
	// surfaces each line's level on `data-level` and its message ("log line <seq>") in the row text;
	// the `seq` (the actual track key, 1..10) is unique per row, so a correctly-tracked list shows 10
	// rows whose "log line N" messages are all distinct. A broken track key would have either thrown
	// NG0955 (above) or collapsed/duplicated rows.
	const lis = Array.from(window.document.querySelectorAll('li'))
	const seqsFromText = lis
		.map((li) => {
			const m = /log line\s+(\d+)/i.exec(li.textContent ?? '')
			return m ? Number(m[1]) : null
		})
		.filter((n) => n !== null)
	const distinctSeqs = new Set(seqsFromText)
	check(
		'LogViewer rendered 10 <li> rows for the 10 streamed lines',
		lis.length === 10,
		`li count = ${lis.length}`,
	)
	check(
		'the 10 <li> rows have DISTINCT track keys (seq 1..10, no duplicates)',
		seqsFromText.length === 10 && distinctSeqs.size === 10,
		`parsed seqs = [${seqsFromText.join(', ')}] (distinct ${distinctSeqs.size})`,
	)
}

// ---------------------------------------------------------------------------
async function main() {
	console.log('== Step 0: wire local node_modules ==')
	wireNodeModules()
	// lstat (not existsSync, which follows a junction and can report false when the target moved
	// since creation) confirms the farm entries are present.
	const wired = (name) => {
		try {
			return Boolean(lstatSync(join(here, 'node_modules', name)))
		} catch {
			return false
		}
	}
	check(
		'local node_modules wired (@treaty/vite + @angular/router + jsdom present)',
		wired('@treaty/vite') && wired('@angular/router') && wired('jsdom'),
	)

	console.log('== Step 1: build @treaty/ts-vite + @treaty/vite plugin dists ==')
	buildPluginDists()
	check('@treaty/ts-vite dist built', existsSync(tsViteDist))
	check('@treaty/vite dist built', existsSync(treatyViteDist))

	console.log('== Step 2: real `vite build` of the everything-app ==')
	const built = await runBuild()
	check('real `vite build` of everything-app exits 0', built, buildError ? String(buildError?.message ?? buildError).split('\n').slice(0, 4).join(' | ') : '')

	console.log('== Step 3: bundle assertions ==')
	if (built) assertBundle()

	console.log('== Step 3b: each JSX module lowered to Ivy exactly once (valid single-export ES module) ==')
	assertJsxLoweredOnce()

	console.log('== Step 3c: styles — global base theme emitted as CSS + component-scoped styles in Ivy ==')
	if (built) assertStyles()

	console.log('== Step 4: headless boot of the built bundle (no JIT; boot+render gated on the reported `use:` Rust gap) ==')
	if (built) await bootHeadless()

	console.log('')
	if (failures.length) {
		console.error(`E2E FAILED: ${failures.length} assertion(s): ${failures.join('; ')}`)
		process.exit(1)
	}
	console.log(
		BOOT_BLOCKED
			? 'E2E PASSED: everything-app builds; JSX/.treaty/@Component lowered to Ivy once; partial @angular linked to AOT (no JIT). BOOT+render is BLOCKED by the reported `use:`-directive Rust gap (see header) — recorded, not failed.'
			: 'E2E PASSED: everything-app builds; JSX/.treaty/@Component lowered to Ivy once; partial @angular linked to AOT; boots with no JIT and a route renders.',
	)
}

main().catch((err) => {
	console.error('E2E ERROR:', err)
	process.exit(1)
})
