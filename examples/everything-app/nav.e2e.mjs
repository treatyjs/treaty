// Treaty everything-app NAV + STYLES DEV end-to-end harness (Phase 3).
//
// This is the ROUTER-DRIVING counterpart to `dev-serve.e2e.mjs`. Where that
// harness proves each authoring surface is SERVED lowered to Ivy through the dev
// pipeline, this one proves the app's NAVIGATION works end to end: every route
// in the app's `provideRouter` graph (the eager `''` index plus the lazy
// `dashboard` / `greeter` / `metrics` / `profile` features) (a) has its lazy
// component MODULE LOAD over a REAL HTTP dev server with a JavaScript
// content-type — NO `NS_ERROR_CORRUPTED_CONTENT`, NO "disallowed MIME type ()" —
// and (b) RENDERS into the `<router-outlet>` when the real Angular `Router`
// navigates to it (the nav links actually change the view).
//
// THE REPORTED LIVE BUG (this harness's primary regression guard):
//   Navigating to the `greeter` route failed in the browser with
//       GET /src/features/greeter/greeter.treaty?import → NS_ERROR_CORRUPTED_CONTENT
//       "blocked because of a disallowed MIME type ()"
//   because Vite labels a served module `text/javascript` ONLY when the request
//   reaches its transform branch (gated on a known-JS-extension regex covering
//   `.[jt]sx?`/`.m[jt]s`/… plus `?import`/CSS/script fetches). `.treaty`/`.tjsx`
//   are NOT in that regex, so a request for one WITHOUT `?import` (a hard nav, a
//   re-request of the bare URL, a proxy that drops `sec-fetch-dest`) skipped
//   transform and was served RAW by the static/fs middleware with an EMPTY
//   content-type → the browser blocked it → the lazy route never loaded → the
//   nav links appeared dead. The `@treaty/vite` dev-serve fix (an `enforce:'pre'`
//   `configureServer` middleware that injects `?import` for bare `.treaty`/`.tjsx`
//   requests) routes those requests through Vite's JS transform branch so they
//   are labelled `text/javascript`. This harness boots a REAL listening HTTP dev
//   server and fetches EVERY route's lazy module — and the bare `.treaty`/`.tjsx`
//   URLs both with and without `?import` — asserting each is served with a JS
//   content-type, never an empty/octet-stream type, never the raw authoring
//   source. The reported MIME failure is gone.
//
// REPORTED OUT-OF-SCOPE RUST-COMPILER GAP (greeter route — REPORTED, not fixed):
//   The `greeter` route's component imports the `.treaty` SFC `greeter.treaty`,
//   whose body carries an inline `server { … }` server-fn block. The Rust
//   compiler does NOT extract that block — it emits it VERBATIM into the lowered
//   module (`server { async function greet(who: string) … }`), which is not
//   parseable JS, so the `.treaty`/`.tjsx` type-strip pass (esbuild) throws
//   `Unexpected "{"` and the module fails to transform with a 500 (NOT the MIME
//   failure — the request DID reach the JS transform branch, proving the MIME fix
//   works; the transform itself then hit the Rust gap). That extraction lives in
//   `libs/treaty-ivy` / `libs/authoring/node` (out of scope to edit). This
//   harness REPORTS the gap precisely (it asserts the greeter `.treaty` request
//   reaches the JS transform branch — the MIME fix — and records the residual
//   `server {}` transform failure as the documented Rust gap, mirroring the
//   `BOOT_BLOCKED` switch in `full-build.e2e.mjs`) and drives the router across
//   every OTHER cleanly-lowering route as a hard requirement. `GREETER_BLOCKED`
//   (defaulting to `true`) is the explicit switch: if the `server {}` extraction
//   lands in Rust, flip it to `false` and the greeter route joins the hard matrix.
//
// REPORTED OUT-OF-SCOPE RUST-COMPILER GAP (metrics route RENDER — REPORTED, not fixed):
//   The metrics route's component + the `.treaty` Gauge it hosts BOTH load + lower
//   to Ivy cleanly (proven over HTTP in Step 2). Its in-browser RENDER is blocked by
//   a SECOND Rust gap: the Treaty compiler lowers only COMPONENTS (`@Component` /
//   `.treaty` / JSX → `ɵɵdefineComponent`) to Ivy — a standalone `@Pipe`
//   (`Percent01Pipe`) or `@Directive` (`HighlightDelta`) is a PASS-THROUGH (the
//   core's `transform` returns null), so it ships as a raw decorated class with NO
//   Ivy `ɵpipe`/`ɵdir` definition. The metrics template both pipes through
//   `percent01` and applies `HighlightDelta`, so at render time Ivy's `ɵɵpipe` reads
//   the missing pipe def and throws `Cannot read properties of undefined (reading
//   'onDestroy')`. That pipe/directive AOT lowering lives in `libs/treaty-ivy` /
//   `libs/authoring/node` (out of scope). `METRICS_BLOCKED` (defaulting to `true`)
//   records it; metrics' module-LOAD + MIME stay HARD requirements. Flip to `false`
//   when the Rust compiler lowers standalone `@Pipe`/`@Directive` to Ivy.
//
// Usage:  node examples/everything-app/nav.e2e.mjs
// Exit code 0 on success, 1 on any failed assertion.

import { build, createServer } from 'vite'
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

// The greeter route's `server {}` extraction gap is now CLOSED in Rust: the
// `.treaty` SFC path lifts the inline `server { … }` block to a typed binding
// (the served module opens `import { greet } from "/@id/__x00__treaty-server-fn…"`)
// and emits valid client JS that esbuild type-strips without the prior
// `Unexpected "{"`. The greeter route therefore now LOADS + RENDERS like any other
// — kept as `false` so the harness HARD-asserts that (a regression that re-broke
// the extraction would flip this assertion red).
const GREETER_BLOCKED = false

// The metrics route's standalone-`@Pipe`/`@Directive` Ivy-lowering gap is now
// CLOSED in Rust: the unified `.ts` authoring router lowers EVERY decorator kind —
// a standalone `@Pipe` (`Percent01Pipe`) → `ɵɵdefinePipe` (+ `ɵfac`) and a
// standalone `@Directive` (`HighlightDelta`) → `ɵɵdefineDirective` (+ `ɵfac`), no
// surviving raw decorator. The metrics template pipes through `percent01` and
// applies the `HighlightDelta` directive, and both now carry real Ivy defs, so the
// prior `ɵɵpipe` → `Cannot read properties of undefined (reading 'onDestroy')` is
// gone and the route RENDERS. Kept as `false` so the harness HARD-asserts the
// metrics render (and a regression that dropped a kind back to pass-through would
// flip it red).
const METRICS_BLOCKED = false

// ---------------------------------------------------------------------------
// Step 0: wire a local node_modules symlink farm (mirrors dev-serve.e2e.mjs).
//
// The examples are not in the root lockfile, so the app has no node_modules of
// its own. This links every @treaty workspace package the app's vite.config.ts
// pulls in transitively — including @treaty/jsx (whose absence caused the user's
// "@treaty/jsx/jsx-dev-runtime could not be resolved") — plus the real partial
// @angular libs, rxjs/tslib, jsdom (the headless render driver), and the build
// toolchain (vite, esbuild). It deliberately does NOT link @angular/compiler /
// @angular/compiler-cli / @babel/core, so the no-JIT / Rust-only guarantee holds.
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
	unlinkFrom(nm, '@angular/compiler')
	unlinkFrom(nm, '@angular/compiler-cli')
	unlinkFrom(nm, '@babel/core')
	linkInto(nm, '@treaty/jsx', join(repoRoot, 'libs/treaty/jsx'))
	linkInto(nm, '@treaty/vite', join(repoRoot, 'libs/treaty/vite'))
	linkInto(nm, '@treaty/compiler', join(repoRoot, 'libs/treaty/compiler'))
	linkInto(nm, '@treaty/module-federation', join(repoRoot, 'libs/treaty/module-federation'))
	linkInto(nm, '@treaty/ts-vite', join(repoRoot, 'libs/typescript/vite'))
	linkInto(nm, '@treaty/authoring-node', join(repoRoot, 'libs/authoring/node'))
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
	return nm
}

// ---------------------------------------------------------------------------
// Step 1: (re)build the @treaty/ts-vite + @treaty/vite plugin dists from current
// source (esbuild bundle, deps external) so the wiring under test is the
// committed source. Identical to dev-serve.e2e.mjs / full-build.e2e.mjs.
// ---------------------------------------------------------------------------
const tsViteDist = join(repoRoot, 'libs/typescript/vite/dist/index.js')
const treatyViteDist = join(repoRoot, 'libs/treaty/vite/dist/index.js')

function buildPluginDists() {
	const bin = req.resolve('esbuild/bin/esbuild', { paths: [repoRoot] })
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

// A served module is JS when its content-type is a JavaScript/ECMAScript MIME.
const isJsContentType = (ct) =>
	/(?:^|[^-\w])(?:text|application)\/(?:javascript|ecmascript)\b/i.test(ct || '')

// ---------------------------------------------------------------------------
// Step 2: per-route MODULE-LOAD matrix over a REAL listening HTTP dev server.
//
// Every route in the app's router graph resolves a lazy component via a real
// dynamic `import()`. We boot a REAL listening `vite` dev server (the exact
// surface the browser hits) and FETCH each route's lazy module over HTTP,
// asserting it is served with a JavaScript content-type and a lowered-Ivy body
// (`ɵɵdefineComponent`) — i.e. the module the router would `import()` LOADS, with
// no MIME block. The greeter route additionally fetches the bare `.treaty`/`.tjsx`
// URLs (with and without `?import`) to prove the reported MIME fix, and records
// the residual Rust `server {}` gap.
// ---------------------------------------------------------------------------
async function assertPerRouteModuleLoadOverHttp() {
	const port = 5394
	const server = await createServer({
		root: here,
		logLevel: 'silent',
		configFile: join(here, 'vite.config.ts'),
		server: { port, strictPort: true, hmr: false },
		optimizeDeps: { noDiscovery: true, include: [] },
	})
	const base = `http://localhost:${port}`

	/** Fetch a URL over the real HTTP dev server; never throw (return a record). */
	async function get(url) {
		try {
			const r = await fetch(base + url)
			const ct = r.headers.get('content-type') || ''
			const body = await r.text()
			return { status: r.status, ct, body }
		} catch (err) {
			return { status: 0, ct: '', body: `fetch failed: ${err?.message ?? err}` }
		}
	}

	try {
		await server.listen()

		// Each route's lazy component module — the exact specifier the router's
		// `loadComponent`/`loadChildren` resolves (extension included so the dev
		// server serves the lowered module, mirroring import-analysis rewriting).
		const routeModules = [
			["'' (logs)", '/src/components/log-viewer.component.ts', /ɵɵdefineComponent/],
			['dashboard', '/src/features/dashboard/dashboard.component.ts', /ɵɵdefineComponent/],
			['metrics', '/src/features/metrics/metrics-panel.component.ts', /ɵɵdefineComponent/],
			// metrics hosts the `.treaty` Gauge selectorlessly — its module must also load.
			['metrics → gauge.treaty', '/src/features/metrics/gauge.treaty', /ɵɵdefineComponent/],
			['profile (loadChildren)', '/src/features/profile/profile.routes.ts', /ProfileComponent|loadChildren|component/],
			['profile → ProfileComponent', '/src/features/profile/profile.component.ts', /ɵɵdefineComponent/],
		]
		for (const [route, url, bodyRe] of routeModules) {
			const r = await get(url)
			check(
				`[load] route ${route}: GET ${url} → JS content-type (module LOADS, no NS_ERROR / disallowed MIME)`,
				r.status === 200 && isJsContentType(r.ct),
				`status=${r.status} content-type="${r.ct}"`,
			)
			check(
				`[load] route ${route}: served module is the lowered route component (not raw source)`,
				bodyRe.test(r.body) && !/^```/.test(r.body.trimStart()),
				r.body.replace(/\s+/g, ' ').slice(0, 90),
			)
		}

		// The greeter route's component module + the `.tjsx` surface it hosts.
		const greeterPage = await get('/src/features/greeter/greeter-page.component.ts')
		check(
			'[load] route greeter: GET greeter-page.component.ts → JS content-type (the @Component .ts module LOADS)',
			greeterPage.status === 200 && isJsContentType(greeterPage.ct),
			`status=${greeterPage.status} content-type="${greeterPage.ct}"`,
		)
		const greetingCard = await get('/src/features/greeter/greeting-card.tjsx')
		check(
			'[load] route greeter → greeting-card.tjsx: GET → JS content-type (the .tjsx surface LOADS)',
			greetingCard.status === 200 && isJsContentType(greetingCard.ct),
			`status=${greetingCard.status} content-type="${greetingCard.ct}"`,
		)

		// THE REPORTED MIME FIX, proven for the EXACT failing extensions. A bare
		// `.treaty`/`.tjsx` request (no `?import`) must NOT be served raw with an
		// empty content-type any more — it must reach Vite's JS transform branch.
		// `gauge.treaty` is the clean `.treaty` proof (it has no `server {}` block);
		// `greeting-card.tjsx` is the `.tjsx` proof. Both with and without `?import`.
		for (const path of [
			'/src/features/metrics/gauge.treaty',
			'/src/features/greeter/greeting-card.tjsx',
		]) {
			for (const url of [path, `${path}?import`]) {
				const r = await get(url)
				check(
					`[MIME] GET ${url} → JS content-type (NOT empty/octet-stream — the reported bug is gone)`,
					r.status === 200 && isJsContentType(r.ct),
					`status=${r.status} content-type="${r.ct}"`,
				)
				check(
					`[MIME] GET ${url} serves LOWERED Ivy (ɵɵdefineComponent), not the raw authoring source`,
					/ɵɵdefineComponent/.test(r.body) && !/^```/.test(r.body.trimStart()),
					r.body.replace(/\s+/g, ' ').slice(0, 80),
				)
			}
		}

		// THE GREETER `.treaty` REQUEST: prove the MIME fix routed it through the JS
		// transform branch (it is NOT served raw with an empty content-type by the
		// static middleware any more), then RECORD the residual Rust `server {}`
		// extraction gap that blocks its transform.
		for (const url of [
			'/src/features/greeter/greeter.treaty',
			'/src/features/greeter/greeter.treaty?import',
		]) {
			const r = await get(url)
			// The reported MIME failure was a 200 with an EMPTY content-type carrying
			// the RAW authoring source. The fix means the request now reaches the JS
			// transform branch — which, for greeter.treaty specifically, surfaces the
			// Rust `server {}` gap as a 500 transform error rather than the raw-source
			// MIME block. Either way it is NEVER served as raw authoring source with an
			// empty content-type (the exact reported failure).
			const servedRawWithEmptyMime =
				r.status === 200 && !isJsContentType(r.ct) && /^```/.test(r.body.trimStart())
			check(
				`[MIME] greeter GET ${url}: NOT served as RAW .treaty source with an empty content-type (the reported NS_ERROR/MIME block is gone)`,
				!servedRawWithEmptyMime,
				`status=${r.status} content-type="${r.ct}" head="${r.body.replace(/\s+/g, ' ').slice(0, 50)}"`,
			)
			// Precisely identify the residual Rust gap: the request reached the
			// transform branch and the `server {}` block failed to lower.
			const isRustServerGap =
				r.status === 500 && /Unexpected\s+"\{"|greeter\.treaty/.test(r.body)
			if (GREETER_BLOCKED) {
				console.log(
					`SKIP  greeter ${url}: transform BLOCKED by the reported Rust \`server {}\` extraction gap` +
						`${isRustServerGap ? ' (confirmed: 500 transform error, `server {}` emitted verbatim → invalid JS)' : ''}` +
						` — the MIME fix routed it through the JS transform branch; the residual block is the unextracted server block (libs/treaty-ivy / libs/authoring/node, out of scope)`,
				)
				if (!isRustServerGap) {
					// The greeter request failed for a DIFFERENT reason than the documented
					// Rust gap (or unexpectedly succeeded): surface it loudly.
					check(
						`greeter ${url} block is the documented Rust \`server {}\` extraction gap (500 transform error)`,
						false,
						r.status === 200
							? 'greeter.treaty unexpectedly transformed cleanly — flip GREETER_BLOCKED to false'
							: `unexpected greeter response: status=${r.status} "${r.body.replace(/\s+/g, ' ').slice(0, 120)}"`,
					)
				}
			} else {
				check(
					`[load] route greeter: GET ${url} → JS content-type (the .treaty module LOADS)`,
					r.status === 200 && isJsContentType(r.ct) && /ɵɵdefineComponent/.test(r.body),
					`status=${r.status} content-type="${r.ct}"`,
				)
			}
		}

		// STYLES over the real HTTP dev server: the global base stylesheet is served
		// as CSS (Vite owns `.css`; `@treaty/vite` does not claim it) carrying its
		// design tokens + app-shell selectors; and a component-scoped style
		// (gauge.treaty `<style>`) rides in the lowered Ivy component's `styles: [...]`.
		const css = await get('/src/styles.css')
		check(
			'[styles] global base stylesheet (src/styles.css) is served (design tokens + app-shell selectors), untouched by treaty()',
			css.status === 200 &&
				/--color-accent/.test(css.body) &&
				/\.app-nav/.test(css.body) &&
				/\.app-main/.test(css.body),
			`status=${css.status} content-type="${css.ct}"`,
		)
		const gauge = await get('/src/features/metrics/gauge.treaty')
		check(
			'[styles] component-scoped styles compile + apply (gauge.treaty <style> → Ivy styles[])',
			/styles\s*:/.test(gauge.body) && /\.gauge\b/.test(gauge.body) && /\.track\b/.test(gauge.body),
			gauge.body.replace(/\s+/g, ' ').slice(0, 100),
		)
	} finally {
		await server.close()
	}
}

// ---------------------------------------------------------------------------
// Step 3: RENDER proof — drive the REAL Angular Router across every route and
// assert the `<router-outlet>` view changes per route (the nav links work).
//
// jsdom cannot resolve the dev server's bare `@angular/*` specifiers / HMR client
// over HTTP, so the faithful, deterministic render driver is a REAL `vite build`
// of the app (its own vite.config.ts → the real `treaty()` chain, partial @angular
// linked to AOT, every authoring surface lowered to Ivy), booted in jsdom. The
// nav-e2e entry (`src/main.nav-e2e.ts`) bootstraps the SAME app shell + real
// `provideRouter`, then exposes the bootstrapped `Router` on `globalThis.treatyNav`
// so we navigate to each route and assert the outlet renders the right view. The
// route graph is the app's, minus the greeter route (blocked by the reported Rust
// `server {}` gap, recorded above).
// ---------------------------------------------------------------------------
const outDir = join(here, 'dist', 'nav-e2e')
const navEntryHtml = join(here, 'index.nav-e2e.html')
let buildError = null

async function buildNavApp() {
	rmSync(outDir, { recursive: true, force: true })
	try {
		await build({
			root: here,
			logLevel: 'warn',
			configFile: join(here, 'vite.config.ts'),
			// Standard Angular production defines: tree-shake the dev-only JIT facade
			// (whose side-effect `import "@angular/compiler"` is JIT) so the build never
			// imports @angular/compiler.
			define: { ngDevMode: false, ngI18nClosureMode: false },
			build: {
				outDir,
				minify: false,
				emptyOutDir: true,
				rollupOptions: { input: navEntryHtml },
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

async function driveRouterHeadless() {
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

	// The emitted entry chunk Rollup names after the nav-e2e html entry.
	const js = collectJs()
	const entry =
		js.find((f) => /index\.nav-e2e[^/\\]*\.js$/.test(f)) ??
		js.find((f) => /(?:main|index)[^/\\]*\.js$/.test(f)) ??
		js[0]
	let importError = null
	try {
		await import(pathToFileURL(entry).href)
	} catch (e) {
		importError = e
	}

	// Wait for bootstrap + the nav seam to be exposed.
	const waitFor = async (pred, timeoutMs) => {
		const deadline = Date.now() + timeoutMs
		while (Date.now() < deadline) {
			if (pred()) return true
			await new Promise((r) => setTimeout(r, 30))
		}
		return pred()
	}
	const nav = () => globalThis.treatyNav
	const ready = await waitFor(() => globalThis.treatyNavReady && nav(), 6000)

	console.error = origError

	const combined = `${importError ? String(importError.stack ?? importError) : ''}\n${consoleError}`
	const jitError =
		/needs to be compiled using the JIT compiler|@angular\/compiler|JIT compilation failed|Runtime compiler is not loaded/i.test(
			combined,
		)
	check(
		'render: nav-e2e bundle booted with NO JIT / @angular/compiler error (partial @angular linked to AOT)',
		!jitError,
		jitError ? combined.trim().split('\n').slice(0, 3).join(' | ') : '',
	)
	if (!check('render: app bootstrapped and exposed the real Router (globalThis.treatyNav)', ready, importError ? String(importError.message ?? importError).slice(0, 160) : '')) {
		return
	}

	const root = window.document.querySelector('app-root')
	const outletText = () => {
		const mainEl = root?.querySelector('.app-main') ?? root
		return (mainEl?.textContent ?? '').replace(/\s+/g, ' ').trim()
	}

	// The AppRoot shell itself must have painted (header + nav links + outlet).
	check(
		'render: AppRoot shell painted (header + nav links + <router-outlet>)',
		/Treaty everything-app/.test(root?.textContent ?? '') &&
			Boolean(window.document.querySelector('router-outlet')) &&
			/logs/.test(root?.textContent ?? '') &&
			/greeter/.test(root?.textContent ?? ''),
		(root?.textContent ?? '').replace(/\s+/g, ' ').trim().slice(0, 100),
	)

	// Navigate to each route and assert the OUTLET content changes to that route's
	// view — the nav links actually work end to end through the real Router.
	const { router, appRef } = nav()
	const settle = async () => {
		appRef.tick()
		// Let the lazy `import()` + route activation + first render flush.
		for (let i = 0; i < 12; i++) await new Promise((r) => setTimeout(r, 50))
		appRef.tick()
	}

	/** Navigate to `url` and return the settled outlet text + any navigation error. */
	const navigateTo = async (url) => {
		let navErr = null
		try {
			await router.navigateByUrl(url)
		} catch (e) {
			navErr = e
		}
		await settle()
		return { text: outletText(), navErr }
	}

	// The routes whose components lower to Ivy cleanly AND render at runtime — the
	// hard nav matrix. (metrics + greeter are recorded as Rust-blocked below.)
	const routeViews = [
		["'' (logs)", '/', /Server logs|waiting for stream/i],
		['dashboard', '/dashboard', /Dashboard|active widgets/i],
		['profile', '/profile', /Profile|name:/i],
		// metrics joins the HARD matrix now that the standalone @Pipe (`percent01`)
		// and @Directive (`HighlightDelta`) lower to real Ivy defs: the route renders
		// `Metrics` and the pipe formats the 0..1 load ratio to `load: 0%` (proving
		// the pipe ran — a missing pipe def would throw before any text painted).
		...(METRICS_BLOCKED ? [] : [['metrics', '/metrics', /Metrics[\s\S]*load:\s*0%/i]]),
	]
	// `prev` tracks the last successfully-rendered outlet so the "changed" check
	// proves each live nav link swaps the view (not a static page).
	let prev = ''
	for (const [route, url, viewRe] of routeViews) {
		const { text, navErr } = await navigateTo(url)
		check(
			`render: nav → ${route} (${url}) RENDERS its view in the outlet (lazy route component loaded + activated)`,
			!navErr && viewRe.test(text),
			navErr ? `navigation threw: ${String(navErr.message ?? navErr).slice(0, 120)}` : `outlet="${text.slice(0, 90)}"`,
		)
		check(
			`render: nav → ${route} changed the outlet view (nav link is live, not dead)`,
			text.length > 0 && text !== prev,
			`prev="${prev.slice(0, 40)}" now="${text.slice(0, 40)}"`,
		)
		prev = text
	}

	// METRICS render: blocked by the reported Rust pipe/directive Ivy-lowering gap
	// (see METRICS_BLOCKED above). The metrics component module + the `.treaty` Gauge
	// it hosts LOAD + lower to Ivy cleanly (proven over HTTP in Step 2); only the
	// runtime render is blocked because the consumed standalone `@Pipe`/`@Directive`
	// ship without Ivy defs, so `ɵɵpipe` throws at render time.
	if (METRICS_BLOCKED) {
		const { text, navErr } = await navigateTo('/metrics')
		const isPipeDirGap =
			Boolean(navErr) && /onDestroy|ɵɵpipe|ɵpipe|undefined/i.test(String(navErr?.message ?? navErr))
		console.log(
			'SKIP  render: nav → metrics (/metrics) RENDER is BLOCKED by the reported Rust pipe/directive Ivy-lowering gap' +
				`${isPipeDirGap ? ' (confirmed: ɵɵpipe read an undefined pipe def → `Cannot read properties of undefined (reading \'onDestroy\')`)' : ''}` +
				' — the metrics component + the .treaty Gauge LOAD + lower to Ivy fine over the real HTTP dev server (Step 2); the consumed standalone ' +
				'@Pipe (percent01) / @Directive (HighlightDelta) are PASS-THROUGH (no Ivy ɵpipe/ɵdir def), owned by libs/treaty-ivy / libs/authoring/node (out of scope). ' +
				'Flip METRICS_BLOCKED to false when the Rust compiler lowers standalone @Pipe/@Directive to Ivy.',
		)
		if (!isPipeDirGap && !navErr) {
			// metrics rendered unexpectedly: surface it so the gap-closing is noticed.
			check(
				'metrics render block is the documented Rust pipe/directive gap (it rendered — flip METRICS_BLOCKED to false)',
				false,
				`outlet="${text.slice(0, 90)}"`,
			)
		}
		// Re-home the router on a route that DOES render so the greeter probe + any
		// later assertions see a clean outlet (metrics left it in an errored state).
		await navigateTo('/dashboard')
		prev = outletText()
	}

	// GREETER render: blocked by the reported Rust `server {}` gap. Record it
	// (the MIME fix is proven over HTTP in Step 2; only the render is blocked).
	if (GREETER_BLOCKED) {
		console.log(
			'SKIP  render: nav → greeter (/greeter) is BLOCKED by the reported Rust `server {}` extraction gap in greeter.treaty ' +
				'(the .treaty MIME fix is proven over the real HTTP dev server in Step 2; greeter.treaty fails to TRANSFORM because its ' +
				'inline `server {}` block is emitted verbatim instead of being extracted — owned by libs/treaty-ivy / libs/authoring/node, out of scope to edit). ' +
				'Flip GREETER_BLOCKED to false when that extraction lands in Rust.',
		)
	} else {
		let navErr = null
		try {
			await router.navigateByUrl('/greeter')
		} catch (e) {
			navErr = e
		}
		await settle()
		const text = outletText()
		check(
			'render: nav → greeter (/greeter) RENDERS its view in the outlet',
			!navErr && /Greeter/.test(text),
			navErr ? String(navErr.message ?? navErr).slice(0, 120) : `outlet="${text.slice(0, 90)}"`,
		)
	}

	// STYLES in the built output: the global base theme is emitted as a CSS asset
	// (design tokens + app-shell selectors) and the gauge's component-scoped style
	// rides in a lowered Ivy JS chunk.
	const cssFiles = existsSync(outDir)
		? readdirSync(outDir, { recursive: true }).filter((f) => typeof f === 'string' && f.endsWith('.css'))
		: []
	const cssText = cssFiles.map((f) => readFileSync(join(outDir, f), 'utf-8')).join('\n')
	check(
		'styles: global base theme emitted as a CSS asset (design tokens + app-shell selectors)',
		cssFiles.length > 0 && /--color-accent/.test(cssText) && /\.app-nav/.test(cssText) && /\.app-main/.test(cssText),
		`${cssFiles.length} .css asset(s)`,
	)
	const jsText = collectJs().map((f) => readFileSync(f, 'utf-8')).join('\n')
	check(
		'styles: component-scoped style applied (gauge.treaty <style> → Ivy styles[] in a JS chunk)',
		/\.gauge\b/.test(jsText) && /\.track\b/.test(jsText),
		/\.gauge\b/.test(jsText) ? '' : 'gauge scoped style not found in any JS chunk',
	)
}

// ---------------------------------------------------------------------------
async function main() {
	console.log('== Step 0: wire local node_modules (incl @treaty/jsx + jsdom) ==')
	wireNodeModules()
	const wired = (name) => {
		try {
			return Boolean(lstatSync(join(here, 'node_modules', name)))
		} catch {
			return false
		}
	}
	check(
		'local node_modules wired (@treaty/jsx + @treaty/vite + @angular/router + jsdom present)',
		wired('@treaty/jsx') && wired('@treaty/vite') && wired('@angular/router') && wired('jsdom'),
	)

	console.log('== Step 1: build @treaty/ts-vite + @treaty/vite plugin dists ==')
	buildPluginDists()
	check('@treaty/ts-vite dist built', existsSync(tsViteDist))
	check('@treaty/vite dist built', existsSync(treatyViteDist))

	console.log('== Step 2: per-route module-load + MIME + styles over a REAL HTTP dev server ==')
	await assertPerRouteModuleLoadOverHttp()

	console.log('== Step 3: real `vite build` of the nav app (router render driver) ==')
	const built = await buildNavApp()
	check(
		'real `vite build` of the nav-e2e app (every route except the Rust-blocked greeter) exits 0',
		built,
		buildError ? String(buildError?.message ?? buildError).split('\n').slice(0, 3).join(' | ') : '',
	)

	console.log('== Step 4: drive the REAL Angular Router across every route + assert per-route render ==')
	if (built) await driveRouterHeadless()

	console.log('')
	if (failures.length) {
		console.error(`NAV E2E FAILED: ${failures.length} assertion(s): ${failures.join('; ')}`)
		process.exit(1)
	}
	console.log(
		'NAV E2E PASSED: every route LOADS its lazy component module over a REAL HTTP dev server with a JavaScript content-type ' +
			'(the reported NS_ERROR_CORRUPTED_CONTENT / "disallowed MIME type ()" is GONE for .treaty/.tjsx — the bare greeter.treaty ' +
			'request now reaches the JS transform branch instead of being served raw with an empty content-type), and EVERY route ' +
			'— `` index/logs, dashboard, profile, metrics, and greeter — RENDERS its view in the outlet when navigated to ' +
			'through the real Angular Router (the nav links work). The global base stylesheet is served + emitted and component-scoped ' +
			'styles apply. The two previously Rust-blocked routes now render: greeter — its greeter.treaty inline `server {}` block ' +
			'is extracted to a typed binding so the SFC lowers to valid client JS; metrics — its consumed standalone @Pipe ' +
			'(percent01) lowers to \u0275\u0275definePipe and @Directive (HighlightDelta) to \u0275\u0275defineDirective AOT, both ' +
			'listed in the component `dependencies`, so the pipe formats `load: 0%` and the directive applies with no JIT error.',
	)
}

main().catch((err) => {
	console.error('NAV E2E ERROR:', err)
	process.exit(1)
})
