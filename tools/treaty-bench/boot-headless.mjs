// Treaty build-tool benchmark — headless boot probe (one tool, one process).
//
// Boots a SINGLE built bundle headlessly in jsdom and reports a machine-readable verdict on stdout.
// Run as a child process (one process per tool) by `buildtool-bench.mjs` so each tool's boot is fully
// isolated: Angular's runtime caches platform/injector state in module-level singletons, and jsdom
// installs DOM globals on `globalThis`, so booting several tools' bundles in one process would let
// state from one tool bleed into the next. A fresh child process per tool removes that hazard.
//
// This reuses the EXACT boot pattern from `examples/linker-smoke/e2e.mjs` Step 4 (the proven jsdom
// global-install + import + flush + assert routine), so the e2e-of-output layer asserts the same
// thing the linker-smoke e2e does, but against EACH build tool's emitted bundle.
//
// Verdict (single JSON line, prefixed by BOOT_RESULT:):
//   works:   "PASS" | "FAIL"
//   reason:  human-readable explanation
//   jitError, importError, bootError, rendered: the underlying signals
//
// Usage (internal):  node boot-headless.mjs <tool> <entryJsAbsPath> <nodeModulesDir>
// Always exits 0 (the verdict is in the JSON; a non-zero exit would mean the probe itself crashed).

import { createRequire } from 'node:module'
import { readFileSync } from 'node:fs'
import { pathToFileURL } from 'node:url'

const [, , tool, entry, nodeModulesDir] = process.argv

function emit(verdict) {
	process.stdout.write('BOOT_RESULT:' + JSON.stringify(verdict) + '\n')
}

if (!tool || !entry) {
	emit({ tool: tool ?? '?', works: 'FAIL', reason: 'boot-headless invoked without <tool> <entry> args' })
	process.exit(0)
}

// Resolve `jsdom` from the app's symlink farm (the bench wires it there alongside @angular/*).
const req = createRequire(nodeModulesDir ? nodeModulesDir + '/' : import.meta.url)

async function main() {
	let JSDOM
	try {
		;({ JSDOM } = req('jsdom'))
	} catch (e) {
		emit({ tool, works: 'FAIL', reason: `jsdom not resolvable for boot: ${String(e?.message ?? e)}` })
		return
	}

	// Same DOM shell the linker-smoke e2e boots: a body containing the app's root element. The built
	// bundle's `main` self-executes `bootstrapApplication(AppRoot, ...)`, which mounts into <smoke-root>.
	const dom = new JSDOM(`<!doctype html><html><body><smoke-root></smoke-root></body></html>`, {
		url: 'http://localhost/',
		pretendToBeVisual: true,
		runScripts: 'outside-only',
	})
	const { window } = dom

	// Install the browser globals Angular expects (verbatim approach from e2e.mjs Step 4): assign via
	// defineProperty and tolerate read-only Node globals (e.g. navigator) which Angular reads off window.
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

	// Let Angular's async bootstrap flush (same 300ms budget the e2e uses).
	await new Promise((r) => setTimeout(r, 300))
	console.error = origError

	const bootErrEl = window.document.getElementById('bootstrap-error')
	const bootErrText = bootErrEl ? bootErrEl.textContent : ''
	const combined = `${importError ? String(importError.stack ?? importError) : ''}\n${bootErrText}\n${consoleError}`

	// (a) The JIT / "@angular/compiler not available" guard — the load-bearing "fast build shipped
	//     broken output" signal: a bundle that skipped/failed linking throws this at bootstrap.
	const jitError =
		/needs to be compiled using the JIT compiler|@angular\/compiler|JIT compilation failed|Runtime compiler is not loaded|Component .* is not resolved/i.test(
			combined,
		)

	// (b) The app actually rendered something: the routed HomeComponent writes "Linker smoke" into an
	//     <h1 id="smoke-heading">. We assert that heading rendered (proves bootstrap + routing + the
	//     inject(PlatformLocation) path all completed against the linked @angular/* libs).
	const heading = window.document.getElementById('smoke-heading')
	const headingText = heading?.textContent ?? ''
	const renderedHeading = Boolean(heading) && /Linker smoke/.test(headingText)
	// Fallback "rendered SOMETHING" signal: any non-whitespace text/markup inside <smoke-root> beyond
	// the empty placeholder. Used only to distinguish "rendered the wrong thing" from "rendered nothing".
	const root = window.document.querySelector('smoke-root')
	const rootHtml = (root?.innerHTML ?? '').trim()
	const renderedAnything = rootHtml.length > 0

	let works
	let reason
	if (jitError) {
		works = 'FAIL'
		reason = `bootstrap hit a JIT / @angular/compiler error: ${combined.trim().split('\n').slice(0, 3).join(' | ')}`
	} else if (importError) {
		works = 'FAIL'
		reason = `bundle threw on import/bootstrap: ${String(importError.message ?? importError).split('\n')[0]}`
	} else if (bootErrText) {
		works = 'FAIL'
		reason = `app reported a bootstrap error: ${bootErrText.split('\n')[0]}`
	} else if (!renderedAnything) {
		works = 'FAIL'
		reason = 'bootstrap did not throw, but nothing was rendered into <smoke-root>'
	} else if (!renderedHeading) {
		works = 'FAIL'
		reason = `rendered into the DOM but not the expected routed component (no "Linker smoke" heading); root="${rootHtml.slice(0, 80)}"`
	} else {
		works = 'PASS'
		reason = `routed component rendered ("${headingText.trim()}") with no JIT / @angular/compiler error`
	}

	emit({
		tool,
		works,
		reason,
		jitError,
		importError: importError ? String(importError.message ?? importError).split('\n')[0] : null,
		bootError: bootErrText ? bootErrText.split('\n')[0] : null,
		rendered: renderedHeading,
		renderedAnything,
		heading: headingText.trim() || null,
	})
}

main().catch((err) => {
	emit({ tool, works: 'FAIL', reason: `boot probe crashed: ${String(err?.stack ?? err).split('\n').slice(0, 3).join(' | ')}` })
	process.exit(0)
})
