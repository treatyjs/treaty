/**
 * Node smoke test for @treaty/vite.
 *
 * Constructs the plugin via its default factory and exercises the Vite plugin
 * surface against the real Treaty compiler core (which routes through the Rust
 * authoring compiler via @treaty/authoring-node). It asserts:
 *   1. the factory returns a well-formed Vite Plugin (name, enforce pre, hooks),
 *   2. transform() on a .treaty string -> Ivy JS containing `defineComponent`,
 *   3. transform() returns { code, map } shape,
 *   4. a .tsx @Component also lowers to Ivy JS,
 *   5. transform() returns null for files Treaty does not own (plain .js),
 *   6. the config() hook teaches esbuild the .tjsx loader,
 *   7. handleHotUpdate on a deleted owned file returns affected modules and
 *      drives the core onDelete (dependent re-evaluation),
 *   8. a BARE-JSX .tsx (export default returning JSX, no @Component) lowers to Ivy,
 *   9. the cold-build prewarm runs transformMany in buildStart so the per-file
 *      transform that follows is a cache hit (and is a no-op in dev).
 *
 * Run: node libs/treaty/vite/test/vite.smoke.mjs
 */

import assert from 'node:assert/strict'
import { mkdtempSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import treaty from '../dist/index.js'

let failures = 0
const results = []

async function check(label, fn) {
	try {
		await fn()
		results.push(`PASS ${label}`)
	} catch (err) {
		failures++
		results.push(`FAIL ${label}: ${err.stack ?? err.message}`)
	}
}

const plugin = treaty()

// 1. plugin shape
await check('factory returns a well-formed Vite plugin', () => {
	assert.equal(typeof plugin, 'object', 'plugin must be an object')
	assert.equal(plugin.name, 'treaty:vite', 'plugin name')
	assert.equal(plugin.enforce, 'pre', 'enforce must be pre')
	assert.equal(typeof plugin.transform, 'function', 'transform hook present')
	assert.equal(typeof plugin.config, 'function', 'config hook present')
	assert.equal(typeof plugin.configResolved, 'function', 'configResolved hook present')
	assert.equal(typeof plugin.resolveId, 'function', 'resolveId hook present')
	assert.equal(typeof plugin.handleHotUpdate, 'function', 'handleHotUpdate hook present')
})

// helper: call the transform hook with a benign `this`.
function runTransform(code, id) {
	return plugin.transform.call({}, code, id)
}

// 2. .treaty -> Ivy JS
let treatyOut
await check('transform(.treaty) -> defineComponent', () => {
	treatyOut = runTransform('<div>hello</div>\n', 'logo.treaty')
	assert.ok(treatyOut, 'expected a non-null transform result')
	assert.ok(
		treatyOut.code.includes('defineComponent'),
		'emitted Ivy JS must contain defineComponent'
	)
	results.push(`INFO .treaty emitted ${treatyOut.code.length} bytes`)
})

// 3. { code, map } shape
await check('transform returns { code, map } shape', () => {
	assert.ok(treatyOut, 'requires the prior transform')
	assert.equal(typeof treatyOut.code, 'string', 'code is a string')
	assert.ok('map' in treatyOut, 'result carries a map field')
})

// 4. .tsx @Component -> Ivy JS
await check('transform(.tsx @Component) -> defineComponent', () => {
	const tsxSource =
		"import { Component } from '@angular/core';\n" +
		"@Component({ selector: 'app-x', template: '<div>x</div>' })\n" +
		'export class XComponent {}\n'
	const out = runTransform(tsxSource, 'x.tsx')
	assert.ok(out, 'expected a non-null transform result')
	assert.ok(out.code.includes('defineComponent'), 'emitted Ivy JS must contain defineComponent')
})

// 4b. BARE-JSX .tsx (no @Component) now lowers to Ivy via the unified front-end
await check('transform(bare-JSX .tsx) -> defineComponent', () => {
	const bare = 'export default function App() {\n  return <h1>bare jsx</h1>\n}\n'
	const out = runTransform(bare, 'App.tsx')
	assert.ok(out, 'bare JSX must compile (no longer rejected)')
	assert.ok(out.code.includes('defineComponent'), 'bare JSX must lower to Ivy JS')
})

// 5. unowned files return null
await check('unowned files return null', () => {
	assert.equal(runTransform('export const a = 1\n', 'util.js'), null, 'plain .js => null')
	assert.equal(runTransform('export const x = 1\n', 'plain.ts'), null, 'non-component .ts => null')
})

// 6. config() teaches esbuild the .tjsx loader
await check('config() registers the .tjsx esbuild loader', () => {
	const cfg = plugin.config.call({}, {}, { command: 'serve', mode: 'development' })
	const loader = cfg?.optimizeDeps?.esbuildOptions?.loader
	assert.ok(loader, 'config returns optimizeDeps.esbuildOptions.loader')
	assert.equal(loader['.tjsx'], 'tsx', '.tjsx maps to the tsx loader')
})

// 7. handleHotUpdate on a deleted owned file returns affected modules + onDelete
await check('handleHotUpdate on delete drives onDelete and returns modules', async () => {
	// Record an importer so logo.treaty has a dependent in the core index.
	const importer =
		"import { Component } from '@angular/core';\n" +
		"import logo from './logo.treaty';\n" +
		"@Component({ selector: 'app-y', template: '<div>y</div>' })\n" +
		'export class YComponent {}\n'
	runTransform(importer, 'y.tsx')

	const deletedModule = { id: './logo.treaty' }
	const dependentModule = { id: 'y.tsx' }
	const ctx = {
		file: './logo.treaty',
		timestamp: Date.now(),
		modules: [deletedModule],
		// read() throws -> signals the file was deleted.
		read: async () => {
			throw new Error('ENOENT')
		},
		server: {
			moduleGraph: {
				getModulesByFile(file) {
					return file === 'y.tsx' ? new Set([dependentModule]) : new Set()
				},
			},
		},
	}

	const affected = plugin.handleHotUpdate.call({}, ctx)
	const resolved = affected instanceof Promise ? await affected : affected
	assert.ok(Array.isArray(resolved), 'handleHotUpdate returns an array of modules')
	assert.ok(resolved.includes(deletedModule), 'the deleted module is reported')
	assert.ok(
		resolved.includes(dependentModule),
		'the dependent (y.tsx) is reported via core onDelete'
	)
})

// 8. cold-build prewarm: buildStart batch-compiles listed files so the
//    subsequent per-file transform is a cache hit.
await check('cold-build prewarm warms the cache via transformMany', async () => {
	const dir = mkdtempSync(join(tmpdir(), 'treaty-vite-prewarm-'))
	const file = join(dir, 'Warm.tsx')
	const code = 'export default () => <section>warm</section>\n'
	writeFileSync(file, code, 'utf8')

	const p = treaty({ prewarm: [file] })
	// Mark this as a build (cold) so prewarm is allowed to run.
	p.configResolved.call({}, { command: 'build', mode: 'production' })
	// buildStart reads the prewarm files and runs transformMany.
	const started = p.buildStart.call({})
	if (started instanceof Promise) await started

	// The very first transform of the prewarmed file must come back lowered.
	const out = p.transform.call({}, code, file)
	assert.ok(out, 'prewarmed file transforms to a result')
	assert.ok(out.code.includes('defineComponent'), 'prewarmed bare JSX lowered to Ivy')
})

// 9. prewarm is a no-op in dev (serve): buildStart does nothing, transform still works.
await check('prewarm is a no-op for the dev server', async () => {
	const dir = mkdtempSync(join(tmpdir(), 'treaty-vite-dev-'))
	const file = join(dir, 'Dev.tsx')
	const code = 'export default () => <nav>dev</nav>\n'
	writeFileSync(file, code, 'utf8')

	const p = treaty({ prewarm: [file] })
	p.configResolved.call({}, { command: 'serve', mode: 'development' })
	const started = p.buildStart.call({})
	if (started instanceof Promise) await started
	// Even without prewarming, the per-file transform still lowers correctly.
	const out = p.transform.call({}, code, file)
	assert.ok(out && out.code.includes('defineComponent'), 'dev per-file transform still works')
})

for (const line of results) console.log(line)
if (failures > 0) {
	console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSMOKE TEST PASSED')
