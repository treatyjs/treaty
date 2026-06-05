// Full-app boot probe — headless jsdom boot of ONE built bundle (one tool, one process).
//
// Sibling of `boot-headless.mjs` (the linker-smoke probe), but asserts against the REAL ng-bench-app
// shell instead of the linker-smoke fixture: the root element is <app-root> and the eager Dashboard
// route renders an <h2>Inventory dashboard</h2>. Booting each tool's bundle in a FRESH child process
// keeps Angular's module-level platform/injector singletons and jsdom's globalThis DOM install from
// bleeding between tools (same isolation rationale as boot-headless.mjs).
//
// It reuses the EXACT jsdom global-install + import + flush routine from examples/linker-smoke/e2e.mjs
// Step 4, so the e2e-of-output layer asserts the same thing the proven linker-smoke e2e does, but
// against EACH build tool's emitted bundle of the full app.
//
// Verdict (single JSON line, prefixed BOOT_RESULT:):
//   works:        "PASS" | "FAIL"
//   reason:       human-readable explanation
//   jitError:     bootstrap hit a JIT / "@angular/compiler not available" error
//   rendered:     the routed Dashboard's <h2> rendered
//   statCards:    how many <app-stat-card> children the Dashboard INSTANTIATED (component-resolution
//                 completeness signal — a cross-file child used by its CONVENTIONAL @Component selector
//                 `app-stat-card`, which does NOT fold to the class name `StatCard` and so resolves only
//                 through the project selector registry the @treaty build threads into the compiler)
//
// Usage (internal):  node boot-fullapp.mjs <tool> <entryJsAbsPath> <nodeModulesDir>
// Always exits 0 (the verdict is in the JSON; a non-zero exit would mean the probe itself crashed).

import { createRequire } from 'node:module'
import { pathToFileURL } from 'node:url'

const [, , tool, entry, nodeModulesDir] = process.argv

function emit(verdict) {
	process.stdout.write('BOOT_RESULT:' + JSON.stringify(verdict) + '\n')
}

if (!tool || !entry) {
	emit({ tool: tool ?? '?', works: 'FAIL', reason: 'boot-fullapp invoked without <tool> <entry> args' })
	process.exit(0)
}

const req = createRequire(nodeModulesDir ? nodeModulesDir + '/' : import.meta.url)

async function main() {
	let JSDOM
	try {
		;({ JSDOM } = req('jsdom'))
	} catch (e) {
		emit({ tool, works: 'FAIL', reason: `jsdom not resolvable for boot: ${String(e?.message ?? e)}` })
		return
	}

	// The DOM shell mirrors the app's own index.html: a body containing <app-root>. The built bundle's
	// main self-executes `bootstrapApplication(App, appConfig)`, which mounts into <app-root> and (via
	// provideRouter) renders the eager Dashboard at the `''` route.
	const dom = new JSDOM(`<!doctype html><html><head><base href="/"></head><body><app-root></app-root></body></html>`, {
		url: 'http://localhost/',
		pretendToBeVisual: true,
		runScripts: 'outside-only',
	})
	const { window } = dom

	const setGlobal = (key, value) => {
		try {
			Object.defineProperty(globalThis, key, { value, configurable: true, writable: true })
		} catch {
			/* read-only Node global: Angular reads it from window anyway */
		}
	}
	setGlobal('window', window)
	setGlobal('document', window.document)
	setGlobal('navigator', window.navigator)
	setGlobal('location', window.location)
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

	// Capture console.error (Angular routes bootstrap failures + the JIT/"@angular/compiler not
	// available" diagnostic through it).
	let consoleError = ''
	const origError = console.error
	console.error = (...args) => {
		consoleError += args.map(String).join(' ') + '\n'
	}

	let importError = null
	try {
		await import(pathToFileURL(entry).href)
	} catch (e) {
		importError = e
	}

	// Let Angular's async bootstrap + lazy route resolution flush. The eager Dashboard route resolves
	// synchronously, but allow a generous budget for the zoneless scheduler + toSignal stream.
	await new Promise((r) => setTimeout(r, 500))
	console.error = origError

	const combined = `${importError ? String(importError.stack ?? importError) : ''}\n${consoleError}`

	// (a) The JIT / "@angular/compiler not available" guard — the load-bearing "fast build shipped
	//     broken output" signal: a bundle whose @angular/* partials were not linked throws this at
	//     bootstrap (or the runtime falls back to JIT, which is the failure this whole pipeline avoids).
	const jitError =
		/needs to be compiled using the JIT compiler|@angular\/compiler|JIT compilation failed|Runtime compiler is not loaded|Component .* is not resolved/i.test(
			combined,
		)

	// (b) The app actually rendered: the eager Dashboard route writes <h2>Inventory dashboard</h2>.
	//     Asserting that heading rendered proves bootstrap + provideRouter + the eager route + the
	//     OnPush/zoneless render all completed against the linked @angular/* libs.
	const root = window.document.querySelector('app-root')
	const rootHtml = (root?.innerHTML ?? '').trim()
	const headings = Array.from(window.document.querySelectorAll('h2')).map((h) => h.textContent?.trim() ?? '')
	const renderedDashboard = headings.some((t) => /Inventory dashboard/i.test(t))
	const renderedAnything = rootHtml.length > 0

	// (c) Component-resolution completeness: the Dashboard template instantiates three <app-stat-card>
	//     children. StatCard uses the CONVENTIONAL Angular-CLI selector (`class StatCard` ↔ selector
	//     "app-stat-card"), which does NOT fold to the class name — so it resolves as a cross-file
	//     dependency ONLY through the project SELECTOR REGISTRY (the @treaty/vite build scans the
	//     project's `.ts` and threads each importer's per-file registry into the compiler). Count the
	//     children StatCard actually INSTANTIATED — proven by its inner "Refresh" button — so the
	//     verdict distinguishes a fully-resolved render (statCards=3) from three empty hosts (=0).
	const statCards = window.document.querySelectorAll('app-stat-card button').length
	// Also count the router nav links the App shell renders (proves the App component + RouterLink).
	const navLinks = window.document.querySelectorAll('nav a').length

	let works
	let reason
	if (jitError) {
		works = 'FAIL'
		reason = `bootstrap hit a JIT / @angular/compiler error: ${combined.trim().split('\n').slice(0, 3).join(' | ')}`
	} else if (importError) {
		works = 'FAIL'
		reason = `bundle threw on import/bootstrap: ${String(importError.message ?? importError).split('\n')[0]}`
	} else if (!renderedAnything) {
		works = 'FAIL'
		reason = 'bootstrap did not throw, but nothing was rendered into <app-root>'
	} else if (!renderedDashboard) {
		works = 'FAIL'
		reason = `rendered into the DOM but not the eager Dashboard route (no "Inventory dashboard" <h2>); root="${rootHtml.slice(0, 120)}"`
	} else {
		works = 'PASS'
		reason = `eager Dashboard route rendered ("Inventory dashboard") with no JIT / @angular/compiler error; statCards=${statCards} navLinks=${navLinks}`
	}

	emit({
		tool,
		works,
		reason,
		jitError,
		importError: importError ? String(importError.message ?? importError).split('\n')[0] : null,
		rendered: renderedDashboard,
		renderedAnything,
		statCards,
		navLinks,
		headings: headings.filter(Boolean).slice(0, 4),
	})
}

main().catch((err) => {
	emit({ tool, works: 'FAIL', reason: `boot probe crashed: ${String(err?.stack ?? err).split('\n').slice(0, 3).join(' | ')}` })
	process.exit(0)
})
