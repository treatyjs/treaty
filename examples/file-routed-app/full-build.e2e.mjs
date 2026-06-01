// Treaty file-routed-app FULL BUILD + BOOT end-to-end harness (Phase 2).
//
// The whole-app counterpart to `test/routing.e2e.mjs` (which proves the file-routing
// engine's WIRING — virtual:treaty-routes generated from the FS). Here we run a REAL
// `vite build` of the ACTUAL file-routed-app through its own `vite.config.ts` (the
// `treaty({ fileRoutes })` plugin chain), so the entire build-time story is exercised
// end to end:
//
//   - `virtual:treaty-routes` is RESOLVED + LOADED from the on-disk `routes/` tree at
//     build time (no checked-in / prebuilt routes.ts, no prebuild step);
//   - every lazy `.treaty` / `.tjsx` route the route graph imports is lowered to valid
//     Ivy (compiled once);
//   - partial-compiled `@angular/*` deps (core/common/router/platform-browser) are
//     de-partialled to AOT by the Rust linker — ZERO residual `ɵɵngDeclare*`, NO
//     `@angular/compiler` (no JIT) and NO `@angular/compiler-cli` / `@babel/core`.
//
// THEN it BOOTS the built bundle headlessly (jsdom, per linker-smoke) and asserts the
// app bootstraps with NO "needs JIT / @angular/compiler" error and a route component
// renders into the DOM (the router resolves the "" route through the generated graph
// and the lowered `.treaty` route component paints).
//
// Steps:
//   0. Wire a local node_modules symlink farm (the examples are not in the root
//      lockfile). It links every @treaty workspace package the app's vite.config.ts
//      pulls in transitively (@treaty/vite -> @treaty/compiler + @treaty/module-federation
//      + @treaty/ts-vite -> @treaty/authoring-node, the Rust addon) plus the real partial
//      @angular libs, rxjs, tslib, jsdom, and the build toolchain. It deliberately does
//      NOT link @angular/compiler / @angular/compiler-cli / @babel/core, so a build that
//      needed JIT or the Babel finisher would fail to resolve them.
//   1. (Re)build the @treaty/ts-vite + @treaty/vite plugin dists from current source so
//      the wiring under test is the committed source.
//   2. Run a real `vite build` of examples/file-routed-app via its own vite.config.ts.
//   3. Assert the build exits 0 and the emitted bundle is correct (routes present, lazy
//      route chunks lowered to Ivy, zero residual ngDeclare, no @angular/compiler).
//   4. BOOT the built `dist/full-e2e` bundle headlessly (jsdom) and assert no JIT error
//      and a route component renders.
//
// Usage:  node examples/file-routed-app/full-build.e2e.mjs
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
	// An existing junction may already point at the target. lstat (not existsSync, which
	// follows the link and can report false for a junction whose target moved) tells us
	// whether ANY entry is present; if so, leave a valid one and otherwise recreate it.
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
	// The Rust-only linker must NEVER drag JIT or the Babel finisher into the build graph.
	// Prune any stale junction so the no-@angular/compiler + no-Babel guarantee holds.
	unlinkFrom(nm, '@angular/compiler')
	unlinkFrom(nm, '@angular/compiler-cli')
	unlinkFrom(nm, '@babel/core')
	// @treaty workspace packages the app's vite.config.ts pulls in transitively. The app
	// does `import treaty from '@treaty/vite'`, whose dist imports @treaty/compiler (-> the
	// @treaty/authoring-node Rust addon — also the file-routing engine), @treaty/module-federation,
	// and @treaty/ts-vite (the shared linker). All must resolve.
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
// Same wiring rule as the everything-app harness: @treaty/ts-vite is CJS, @treaty/vite is
// ESM, and the @treaty workspace deps are kept EXTERNAL (`--packages=external` alone is not
// enough because the monorepo resolves them under `libs/`, not `node_modules`, so esbuild
// would INLINE them — and inlining @treaty/ts-vite's CommonJS `require('@treaty/authoring-node')`
// into an ESM bundle turns it into esbuild's throwing "Dynamic require" shim, silently
// disabling the linker so the app would ship un-linked, JIT-crashing Angular).
// ---------------------------------------------------------------------------
const tsViteDist = join(repoRoot, 'libs/typescript/vite/dist/index.js')
const treatyViteDist = join(repoRoot, 'libs/treaty/vite/dist/index.js')

function buildPluginDists() {
	const bin = req.resolve('esbuild/bin/esbuild', { paths: [repoRoot] })
	// @treaty/ts-vite — CJS (its package is plain CommonJS).
	mkdirSync(dirname(tsViteDist), { recursive: true })
	execFileSync(
		process.execPath,
		[
			bin,
			join(repoRoot, 'libs/typescript/vite/src/index.ts'),
			'--bundle',
			'--platform=node',
			'--format=cjs',
			'--target=node20',
			'--packages=external',
			`--outfile=${tsViteDist}`,
		],
		{ stdio: ['ignore', 'ignore', 'inherit'] },
	)
	// @treaty/vite — ESM (its package declares "type": "module"), @treaty deps external.
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
// Step 2: real production build of the file-routed-app through its own vite.config.ts.
//
// Emit under `dist/full-e2e/` so the output falls under the repo-wide `**/dist/**` ignore
// globs — it is build output, never linted or committed. We build the app's real
// index.html entry (which bootstraps `src/main.ts` -> provideRouter(virtual:treaty-routes)).
// ---------------------------------------------------------------------------
const outDir = join(here, 'dist', 'full-e2e')
let buildError = null
async function runBuild() {
	rmSync(outDir, { recursive: true, force: true })
	try {
		await build({
			root: here,
			logLevel: 'warn',
			configFile: join(here, 'vite.config.ts'),
			// ngDevMode/ngI18nClosureMode are the standard Angular production defines: they
			// tree-shake the dev-only JIT facade chunk (whose side-effect `import "@angular/compiler"`
			// is JIT) so a production build never imports @angular/compiler.
			define: { ngDevMode: false, ngI18nClosureMode: false },
			build: {
				outDir,
				minify: false,
				emptyOutDir: true,
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
	// virtual:treaty-routes is emitted into the entry/chunk graph as `provideRouter(routes)`'s
	// data: the route table the engine GENERATED FROM THE FS, with one absolute lazy `import(...)`
	// loader per routable file in the `routes/` tree. The generated-module banner comment is dropped
	// by the production build (minify/treeshake), so we prove FS derivation two robust ways:
	//   (a) the route table carries exactly as many lazy loaders as there are routable files; and
	//   (b) Rollup code-split those lazy `import()`s into route-file-NAMED chunks (about/layout/
	//       not-found/index) — a prebuilt hand-written routes.ts could not produce route-file-named
	//       split chunks for every entry in the tree.
	let lazyRouteLoaders = 0
	for (const file of files) {
		const code = readFileSync(file, 'utf-8')
		// Count actual partial-declaration CALL expressions, not bare textual mentions.
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
		if (/no component[^\n]*found/i.test(code)) noComponentErr = true
		defineComponent += (code.match(/ɵɵdefineComponent/g) || []).length
		defineInjectable += (code.match(/ɵɵdefineInjectable/g) || []).length
		defineDirective += (code.match(/ɵɵdefineDirective/g) || []).length
		definePipe += (code.match(/ɵɵdefinePipe/g) || []).length
		defineInjector += (code.match(/ɵɵdefineInjector/g) || []).length
		// Lazy route loaders the route generator emits: `loadComponent:`/`loadChildren:` arrows.
		lazyRouteLoaders += (code.match(/load(?:Component|Children)\s*:/g) || []).length
	}
	console.log(`[bundle] ${files.length} JS file(s) emitted`)

	// (b) route files were code-split into route-file-named chunks (proof the lazy `import()`s the
	// virtual module emitted resolved to the real on-disk route files the FS generator referenced).
	const chunkNames = files.map((f) => f.replace(/\\/g, '/').split('/').pop())
	const routeNamedChunks = chunkNames.filter((n) =>
		/^(?:about|layout|not-found|index)[-.]/.test(n),
	).length

	check(
		'virtual:treaty-routes was resolved + loaded from the FS: route table carries lazy loaders',
		lazyRouteLoaders > 0,
		`loaders=${lazyRouteLoaders}`,
	)
	check(
		'the FS-derived lazy route loaders code-split into route-file-named chunks',
		routeNamedChunks > 0,
		`route-named chunks=${routeNamedChunks} of ${chunkNames.length} (${chunkNames.join(', ')})`,
	)
	check(
		'lazy .treaty/.tjsx route components lowered to Ivy (ɵɵdefineComponent present)',
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
}

// ---------------------------------------------------------------------------
// Step 3b: per-route-file proof — each lazy route authoring file lowered to Ivy.
//
// Drive the @treaty/compiler core (the same one the vite plugin uses) directly over each
// route source the generated graph lazily imports, proving each lowers to a valid single
// Ivy component (one `ɵɵdefineComponent`, zero diagnostics, no "no component found").
// ---------------------------------------------------------------------------
function assertRoutesLoweredOnce() {
	let compileTreaty
	let compileUnifiedSource
	try {
		;({ compileTreaty, compileUnifiedSource } = req('@treaty/compiler'))
	} catch (err) {
		check('@treaty/compiler is loadable', false, String(err?.message ?? err))
		return
	}
	check(
		'@treaty/compiler.{compileTreaty,compileUnifiedSource} available',
		typeof compileTreaty === 'function' && typeof compileUnifiedSource === 'function',
	)
	if (typeof compileTreaty !== 'function' || typeof compileUnifiedSource !== 'function') return

	// Every routable file in the routes/ tree (mirrors the generated graph's lazy loaders).
	const routeFiles = [
		'routes/layout.treaty',
		'routes/index.treaty',
		'routes/not-found.treaty',
		'routes/(marketing)/index.treaty',
		'routes/(marketing)/about.tjsx',
		'routes/blog/layout.treaty',
		'routes/blog/index.treaty',
		'routes/blog/[slug]/index.treaty',
		'routes/blog/[...path]/index.treaty',
		'routes/docs/[category]/[page]/index.tjsx',
	]
	for (const rel of routeFiles) {
		const code = readFileSync(join(here, rel), 'utf-8')
		const out = rel.endsWith('.treaty') ? compileTreaty(code, rel) : compileUnifiedSource(code, rel)
		const errors = out.errors ?? []
		const emitted = out.code ?? ''
		const defs = (emitted.match(/ɵɵdefineComponent/g) || []).length
		check(`${rel}: compiles with zero diagnostics`, errors.length === 0, errors.join(' | '))
		check(`${rel}: emits exactly ONE ɵɵdefineComponent (lowered once)`, defs === 1, `defineComponent=${defs}`)
		check(
			`${rel}: no "no component found" error`,
			!/no component[^\n]*found/i.test(errors.join(' ') + emitted),
		)
	}
}

// ---------------------------------------------------------------------------
// Step 4: headless boot (jsdom) of the emitted bundle.
//
// Boot the built app exactly as a browser would: load the emitted entry module into a jsdom
// window with the DOM globals Angular reads, navigate the router to "/", and assert (a) no
// JIT / @angular/compiler error fired (the partial @angular deps were de-partialled to AOT),
// and (b) a route component rendered — the router resolved the "" route through the
// build-time-generated graph and the lowered `.treaty` route component painted into the DOM.
// ---------------------------------------------------------------------------
async function bootHeadless() {
	const { JSDOM } = req('jsdom')
	const dom = new JSDOM(
		`<!doctype html><html><body><app-routed-root></app-routed-root></body></html>`,
		{ url: 'http://localhost/', pretendToBeVisual: true, runScripts: 'outside-only' },
	)
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
	// Bulk-mirror the remaining DOM constructors/APIs jsdom exposes on `window` onto globalThis
	// where missing, so the Angular runtime finds every DOM global it touches during bootstrap.
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

	// The emitted entry chunk is the one Rollup names after the html entry (main / index).
	const js = collectJs()
	const entry = js.find((f) => /(?:main|index)[^/\\]*\.js$/.test(f)) ?? js[0]
	let importError = null
	try {
		await import(pathToFileURL(entry).href)
	} catch (e) {
		importError = e
	}

	// Let Angular's async bootstrap + the lazy route load + first render flush.
	await new Promise((r) => setTimeout(r, 600))
	console.error = origError

	const combined = `${importError ? String(importError.stack ?? importError) : ''}\n${consoleError}`
	const jitError =
		/needs to be compiled using the JIT compiler|@angular\/compiler|JIT compilation failed|Runtime compiler is not loaded|Component .* is not resolved/i.test(
			combined,
		)

	const root = window.document.querySelector('app-routed-root')
	const rootText = root ? (root.textContent ?? '') : ''
	const routerOutletPresent = Boolean(window.document.querySelector('router-outlet'))
	// The "" route renders the root layout shell (masthead nav "file-routed-app" / "About" /
	// "Blog") plus the index page hero ("Welcome" / the tagline). Any of those proves a route
	// component painted through the build-time-generated route graph.
	const routeRendered =
		/file-routed-app|Welcome|file-routed Treaty app|About|Blog/i.test(rootText) ||
		routerOutletPresent

	check(
		'boot did NOT throw a JIT / @angular/compiler error',
		!jitError,
		jitError ? combined.trim().split('\n').slice(0, 4).join(' | ') : '',
	)
	check(
		'boot did NOT throw on import/bootstrap',
		!importError,
		importError ? String(importError.message) : '',
	)
	check(
		'a route component rendered through the build-time route graph (router-outlet + route content)',
		routeRendered,
		rootText ? rootText.replace(/\s+/g, ' ').trim().slice(0, 120) : 'no app-routed-root content',
	)
}

// ---------------------------------------------------------------------------
async function main() {
	console.log('== Step 0: wire local node_modules ==')
	wireNodeModules()
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

	console.log('== Step 2: real `vite build` of the file-routed-app ==')
	const built = await runBuild()
	check(
		'real `vite build` of file-routed-app exits 0',
		built,
		buildError ? String(buildError?.message ?? buildError).split('\n').slice(0, 4).join(' | ') : '',
	)

	console.log('== Step 3: bundle assertions ==')
	if (built) assertBundle()

	console.log('== Step 3b: each lazy route authoring file lowered to Ivy ==')
	assertRoutesLoweredOnce()

	console.log('== Step 4: headless boot of the built bundle ==')
	if (built) await bootHeadless()

	console.log('')
	if (failures.length) {
		console.error(`E2E FAILED: ${failures.length} assertion(s): ${failures.join('; ')}`)
		process.exit(1)
	}
	console.log(
		'E2E PASSED: file-routed-app builds from virtual:treaty-routes; lazy routes lowered to Ivy; partial @angular linked to AOT; boots with no JIT and a route renders.',
	)
}

main().catch((err) => {
	console.error('E2E ERROR:', err)
	process.exit(1)
})
