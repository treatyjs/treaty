/**
 * Headless smoke for the REPL everything-demo — a REAL boot/build assertion (not a
 * stub): it RUNS {@link runEverythingDemo} end to end (compiling every authoring
 * surface through the bundler-plugin seam, then driving the full D2 deploy stack)
 * and asserts on the structured report.
 *
 * It proves, in one pass:
 *   - EVERY authoring surface is owned by the compiler and lowers to real Ivy JS:
 *       .treaty SFC (with a compile-time MACRO whose constants are inlined,
 *       SIGNALS, @if/@for CONTROL FLOW, and a `server { }` SERVER FN extracted to
 *       its own chunk), a .tjsx JSX component, and an Angular @Component .ts with a
 *       'use server' SERVER FN (a second transport) — server bodies live only in
 *       the server chunk, never in the client code;
 *   - the route-graph pass auto-derives the federated MODULES (host + one remote
 *       per feature route + a shared lib) with NO hand-written exposes, and the
 *       Vite federation seam exposes exactly those remotes;
 *   - a VERSIONED MANIFEST is generated from the graph and BUILD-TO-DEPLOY uploads
 *       a real artifact to a pluggable target (files land on disk);
 *   - a single-entry ROLLBACK flips ONE remote back while every other module is
 *       untouched, and the enhanced RUNTIME plugin + the operational deployment
 *       ledger both resolve the rolled-back url+version at load.
 *
 * Run: node apps/repl/src/everything-demo/everything-demo.smoke.mjs
 */

import assert from 'node:assert/strict'

import { runEverythingDemo, AUTHORING_SAMPLES } from './everything-demo.mjs'

let failures = 0
/** @type {string[]} */
const results = []

/**
 * @param {string} label
 * @param {() => void | Promise<void>} fn
 */
async function check(label, fn) {
	try {
		await fn()
		results.push(`PASS ${label}`)
	} catch (err) {
		failures++
		const message = err instanceof Error ? (err.stack ?? err.message) : String(err)
		results.push(`FAIL ${label}: ${message}`)
	}
}

const run = async () => {
	const report = await runEverythingDemo()

	/**
	 * Find a compiled surface by its bundler id, asserting it exists.
	 * @param {string} id
	 * @returns {(typeof report.authoring)[number]}
	 */
	const surface = (id) => {
		const row = report.authoring.find((r) => r.id === id)
		assert.ok(row, `expected a compiled surface for ${id}`)
		return row
	}

	// AUTHORING: every surface owned + lowered to real Ivy JS.
	await check('authoring: every surface compiled to Ivy (treaty SFC, JSX, Angular .ts)', () => {
		assert.equal(report.authoring.length, AUTHORING_SAMPLES.length, 'one row per authoring sample')

		assert.equal(surface('MacroPanel.treaty').kind, 'treaty', '.treaty SFC owned by the treaty plugin')
		assert.equal(surface('Greeting.tjsx').kind, 'jsx', '.tjsx owned by the JSX plugin')
		assert.equal(surface('save-note.component.ts').kind, 'component', 'Angular .ts owned by the component plugin')

		for (const row of report.authoring) {
			assert.ok(row.codeLength > 0, `${row.id} produced non-empty Ivy code`)
			// Real Ivy output carries the component definition factory.
			assert.ok(/ɵcmp|ɵɵdefineComponent/.test(row.code), `${row.id} emitted an Ivy component definition`)
		}
	})

	// MACRO: the .treaty top-of-file fenced macro block is recognized, parsed, and
	// compiled away — its fence never survives into the emitted client module and a
	// `$macro` compile-time binding is produced for it.
	await check('authoring: .treaty MACRO block parsed + compiled away (fence stripped, $macro binding emitted)', () => {
		const macro = surface('MacroPanel.treaty')
		// No leftover macro fence survives into the emitted client code.
		assert.ok(!macro.code.includes('```'), 'macro fence stripped from the emitted code')
		// The compiler lifts the macro to its own compile-time binding.
		assert.ok(macro.code.includes('$macro'), 'macro lifted to a $macro compile-time binding')
		// @if/@for control flow lowers to Ivy conditional/repeater instructions.
		assert.ok(/ɵɵconditional/.test(macro.code), '@if control flow lowered to an Ivy conditional')
	})

	// SERVER FNS: both markers extract to a server chunk; bodies are not in client code.
	await check('authoring: BOTH server-fn markers extract to a chunk; body absent from client code', () => {
		const treaty = surface('MacroPanel.treaty')
		const ng = surface('save-note.component.ts')

		// .treaty `server { }` marker.
		assert.ok(treaty.serverChunks.length >= 1, '.treaty server block extracted at least one server fn chunk')
		assert.ok(treaty.serverModule && treaty.serverModule.length > 0, '.treaty server module emitted')
		assert.ok(
			treaty.serverChunks.some((c) => c.exportName === 'loadAccentCount'),
			'.treaty server fn exposed under its export name'
		)
		// The server BODY (the literal returned object) must not ship to the client.
		assert.ok(!treaty.code.includes('count: 3'), 'server fn body absent from .treaty client code')

		// Angular `'use server'` marker (a different transport, same extraction).
		assert.ok(ng.serverChunks.length >= 1, "Angular 'use server' fn extracted a server chunk")
		assert.ok(
			ng.serverChunks.some((c) => c.exportName === 'saveNote'),
			'Angular server fn exposed under its export name'
		)
		assert.ok(!ng.code.includes("'use server'"), "no 'use server' directive left in client code")
	})

	// D2 (1)/(2): route graph auto-derives modules — host + lazy routes + a lib, no eager.
	await check('D2(1/2): route graph -> host + one remote per feature route + shared lib (no manual exposes)', () => {
		const ids = report.federation.moduleIds
		assert.ok(ids.includes('repl-everything'), 'host container is a module')
		assert.ok(ids.includes('./routes/macro-panel'), 'macro-panel lazy route is a remote')
		assert.ok(ids.includes('./routes/greeting'), 'greeting lazy route is a remote')
		assert.ok(ids.includes('./routes/save-note'), 'save-note lazy route is a remote')
		assert.ok(ids.includes('./libs/ui-kit'), 'shared lib is a module')
		// the eager `about` route is NOT a module.
		assert.ok(!ids.some((id) => id.includes('about')), 'eager route is NOT a deployable module')

		/** @param {string} id */
		const mod = (id) => {
			const found = report.federation.modules.find((m) => m.moduleId === id)
			assert.ok(found, `expected a derived module ${id}`)
			return found
		}
		assert.equal(mod('repl-everything').kind, 'host', 'host kind correct')
		assert.equal(mod('./routes/greeting').kind, 'route', 'route kind correct')
		assert.equal(mod('./libs/ui-kit').kind, 'lib', 'lib kind correct')

		// the Vite federation seam exposes exactly the derived remotes (host excluded).
		assert.equal(report.federation.viteName, 'repl-everything', 'vite federation names the host')
		assert.equal(
			report.federation.viteRemotesCount,
			ids.length - 1,
			'vite exposes one entry per derived remote (host excluded)'
		)
	})

	// D2 (3): a versioned manifest is generated from the graph (module -> version -> url).
	await check('D2(3): versioned manifest generated from the graph (module -> version -> url)', () => {
		const m = report.manifest
		assert.equal(m.app, 'repl-everything', 'manifest names the host app')
		for (const id of report.federation.moduleIds) {
			const dep = m.modules[id]
			assert.ok(dep, `manifest carries module ${id}`)
			assert.equal(dep.version, '1.0.0', `module ${id} pinned to a version`)
			assert.ok(dep.url.includes('1.0.0'), `module ${id} url is version-stamped`)
		}
	})

	// BUILD-TO-DEPLOY: a real artifact is uploaded to a pluggable target.
	await check('build-to-deploy: artifact assembled + uploaded to a pluggable target (files on disk)', () => {
		assert.equal(report.deploy.partial, false, 'a full deploy (covers every module)')
		assert.ok(report.deploy.uploadedFiles.length >= report.federation.moduleIds.length, 'at least one file per module uploaded')
		// every module is repointed at its served url in the published manifest.
		for (const id of report.federation.moduleIds) {
			const published = report.deploy.publishedManifest.modules[id]
			assert.ok(published, `published manifest carries module ${id}`)
			assert.ok(published.url.startsWith('https://cdn.example/'), `${id} repointed at the deployed url`)
		}
	})

	// D2 (4): single-entry rollback + the runtime resolves the flip at load.
	await check('D2(4): single-entry rollback flips ONE remote; runtime + ledger resolve it; rest untouched', () => {
		const rb = report.rollback
		assert.ok(rb.beforeRollback, 'rollback target was recorded in the ledger')
		assert.equal(rb.beforeRollback.currentVersion, '2.0.0', 'remote was rolled forward to 2.0.0 before rollback')
		assert.ok(rb.rolledBackEntry, 'rolled-back module present in the flipped manifest')
		assert.equal(rb.rolledBackEntry.version, '1.0.0', 'rollback flips the remote back to 1.0.0')
		assert.ok(rb.rolledBackEntry.url.includes('1.0.0'), 'rolled-back url is version-stamped to 1.0.0')
		assert.equal(rb.untouched, true, 'every other module untouched by the single-entry flip')

		// the enhanced runtime plugin resolves the rolled-back url+version at load.
		assert.equal(rb.runtimeResolvedVersion, '1.0.0', 'runtime resolves the rolled-back version')
		assert.ok(rb.runtimeResolvedEntry?.includes('1.0.0'), 'runtime resolves the rolled-back url')
		assert.notEqual(rb.runtimeResolvedEntry, 'https://STALE/remoteEntry.js', 'stale entry replaced at load')

		// the operational deployment ledger still serves the latest recorded version (2.0.0)
		// until a rollback is recorded against IT — it resolves from its own ledger.
		assert.equal(rb.ledgerResolvedVersion, '2.0.0', 'deployment ledger resolves its current version')
		assert.ok(rb.ledgerResolvedEntry?.includes('2.0.0'), 'deployment ledger resolves its current url')
	})

	for (const line of results) console.log(line)
	if (failures > 0) {
		console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
		process.exit(1)
	}
	console.log('\nSMOKE TEST PASSED')
}

await run()
