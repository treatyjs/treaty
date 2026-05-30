/**
 * Node smoke test for @treaty/rslib.
 *
 * Drives the built preset (which wires the Treaty transform through
 * @treaty/compiler -> the Rust authoring compiler via the @treaty/authoring-node
 * NAPI addon) and asserts:
 *   1. defineTreatyLib() returns a config object with library defaults
 *      (ESM + dts, @angular/* externalized) and the Treaty transform plugin wired,
 *   2. options (formats, dts, bundle, target, externals) are honored,
 *   3. the wired rsbuild plugin registers a transform that lowers a .treaty
 *      string to Ivy JS containing `defineComponent`,
 *   3b. the wired transform also lowers BARE JSX (.tsx with no @Component),
 *   3c/3d. the cold-build prewarm wires an onBeforeBuild callback (via the plugin
 *      and via defineTreatyLib),
 *   4. a non-component .ts is passed through unchanged by the transform,
 *   5. a direct core transform of a .treaty string yields Ivy JS.
 *
 * Run: node libs/treaty/rslib/test/rslib.smoke.mjs
 */

import assert from 'node:assert/strict'
import { defineTreatyLib, treatyRsbuildPlugin, ANGULAR_EXTERNAL, TREATY_PLUGIN_NAME } from '../dist/index.js'
import { createTreatyCompiler } from '@treaty/compiler'

let failures = 0
const results = []

function check(label, fn) {
	try {
		fn()
		results.push(`PASS ${label}`)
	} catch (err) {
		failures++
		results.push(`FAIL ${label}: ${err.message}`)
	}
}

const TREATY_SOURCE = '<div>hello</div>\n'

// 1. defineTreatyLib() defaults: ESM + dts, angular externalized, plugin wired.
check('defineTreatyLib returns wired lib config with defaults', () => {
	const cfg = defineTreatyLib()
	assert.ok(cfg && typeof cfg === 'object', 'expected a config object')
	assert.ok(Array.isArray(cfg.lib) && cfg.lib.length === 1, 'one lib entry by default')
	assert.equal(cfg.lib[0].format, 'esm', 'default format is esm')
	assert.equal(cfg.lib[0].dts, true, 'dts on by default')
	assert.equal(cfg.lib[0].bundle, false, 'file-by-file transpile by default')
	assert.equal(cfg.output.target, 'node', 'library target defaults to node')
	assert.ok(
		cfg.output.externals.some((e) => e instanceof RegExp && e.source === ANGULAR_EXTERNAL.source),
		'@angular/* must be externalized'
	)
	assert.ok(Array.isArray(cfg.plugins) && cfg.plugins.length === 1, 'one plugin wired')
	assert.equal(cfg.plugins[0].name, TREATY_PLUGIN_NAME, 'the Treaty transform plugin is wired')
	assert.equal(typeof cfg.plugins[0].setup, 'function', 'plugin exposes setup(api)')
})

// 2. options honored.
check('defineTreatyLib honors options', () => {
	const cfg = defineTreatyLib({
		formats: ['esm', 'cjs'],
		dts: false,
		bundle: true,
		target: 'web',
		externals: ['rxjs'],
	})
	assert.equal(cfg.lib.length, 2, 'two formats => two lib entries')
	assert.deepEqual(cfg.lib.map((l) => l.format), ['esm', 'cjs'], 'formats preserved in order')
	assert.equal(cfg.lib[0].dts, false, 'dts toggled off')
	assert.equal(cfg.lib[0].bundle, true, 'bundle toggled on')
	assert.equal(cfg.output.target, 'web', 'target overridden')
	assert.ok(cfg.output.externals.includes('rxjs'), 'extra externals appended')
	assert.ok(
		cfg.output.externals.some((e) => e instanceof RegExp && e.source === ANGULAR_EXTERNAL.source),
		'@angular/* still externalized alongside extras'
	)
})

// 3. the wired plugin registers a transform that lowers .treaty to Ivy JS.
check('wired transform lowers .treaty to Ivy JS', () => {
	const plugin = treatyRsbuildPlugin()
	let handler = null
	let test = null
	const fakeApi = {
		transform(descriptor, fn) {
			test = descriptor.test
			handler = fn
		},
	}
	plugin.setup(fakeApi)
	assert.ok(handler, 'plugin must register a transform handler')
	assert.ok(test instanceof RegExp && test.test('a.treaty'), 'transform test matches .treaty')
	const out = handler({ code: TREATY_SOURCE, resource: 'logo.treaty' })
	assert.ok(out && typeof out === 'object', 'handler returns a transform output object')
	assert.ok(out.code.includes('defineComponent'), 'emitted Ivy JS must contain defineComponent')
})

// 3b. the wired transform lowers a BARE-JSX .tsx (no @Component) to Ivy JS.
check('wired transform lowers bare-JSX .tsx to Ivy JS', () => {
	const plugin = treatyRsbuildPlugin()
	let handler = null
	plugin.setup({ transform(_d, fn) { handler = fn } })
	const bare = 'export default () => <footer>bare</footer>\n'
	const out = handler({ code: bare, resource: 'App.tsx' })
	assert.ok(out && typeof out === 'object', 'bare JSX returns a transform output object')
	assert.ok(out.code.includes('defineComponent'), 'bare JSX must lower to Ivy JS')
})

// 3c. prewarm wires an onBeforeBuild cold-build callback when the host exposes it.
check('prewarm wires onBeforeBuild when available', () => {
	const plugin = treatyRsbuildPlugin({ prewarm: ['missing.tsx'] })
	let beforeBuild = null
	plugin.setup({
		transform() {},
		onBeforeBuild(cb) {
			beforeBuild = cb
		},
	})
	assert.equal(typeof beforeBuild, 'function', 'onBeforeBuild must be wired when prewarm is set')
	assert.ok(beforeBuild() instanceof Promise, 'callback returns a promise')
})

// 3d. defineTreatyLib forwards prewarm into the wired plugin.
check('defineTreatyLib forwards prewarm to the plugin', () => {
	const cfg = defineTreatyLib({ prewarm: ['x.tsx'] })
	const plugin = cfg.plugins[0]
	let beforeBuild = null
	plugin.setup({
		transform() {},
		onBeforeBuild(cb) {
			beforeBuild = cb
		},
	})
	assert.equal(typeof beforeBuild, 'function', 'prewarm from defineTreatyLib reaches onBeforeBuild')
})

// 4. non-component .ts is passed through unchanged.
check('non-component .ts passes through', () => {
	const plugin = treatyRsbuildPlugin()
	let handler = null
	plugin.setup({ transform(_d, fn) { handler = fn } })
	const src = 'export const x = 1\n'
	const out = handler({ code: src, resource: 'plain.ts' })
	assert.equal(out, src, 'plain .ts is returned unchanged for the normal TS pipeline')
})

// 5. direct core transform of a .treaty string yields Ivy JS.
check('direct core transform yields Ivy JS', () => {
	const compiler = createTreatyCompiler()
	const out = compiler.transform('logo.treaty', TREATY_SOURCE)
	assert.ok(out, 'expected a non-null transform result')
	assert.ok(out.code.includes('defineComponent'), 'core transform must emit defineComponent')
	assert.equal(out.sideEffects, false, 'pure component module => sideEffects false')
	results.push(`INFO core transform emitted ${out.code.length} bytes`)
})

for (const line of results) console.log(line)
if (failures > 0) {
	console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSMOKE TEST PASSED')
