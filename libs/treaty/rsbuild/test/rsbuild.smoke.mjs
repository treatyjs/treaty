/**
 * Node smoke test for @treaty/rsbuild.
 *
 * Does NOT run a full Rsbuild build (the bundler is a peer dependency and is
 * not installed). Instead it asserts the two contracts this package owns:
 *   1. the plugin factory returns a well-formed RsbuildPlugin object — correct
 *      `name`, a `setup` function — and that `setup(api)` drives the expected
 *      api hooks (modifyRsbuildConfig + transform, or the tools.rspack
 *      fallback) and that resolve.extensions gets the Treaty extensions;
 *   2. a direct core transform of a trivial `.treaty` string yields Ivy JS
 *      (contains `defineComponent`) — proving the wired-in compiler works;
 *   3. the transform handler lowers BARE JSX (.tsx with no @Component) to Ivy JS;
 *   4. the cold-build prewarm wires an onBeforeBuild callback when the host
 *      exposes it, and is left untouched otherwise.
 *
 * Run: node libs/treaty/rsbuild/test/rsbuild.smoke.mjs
 */

import assert from 'node:assert/strict'
import { pluginTreaty, PLUGIN_NAME, TREATY_EXTENSIONS, treatyLoader } from '../dist/index.js'
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

// 1. plugin object shape + name
check('plugin object has correct name and setup', () => {
	const plugin = pluginTreaty()
	assert.equal(typeof plugin, 'object', 'plugin must be an object')
	assert.equal(plugin.name, PLUGIN_NAME, `name must equal ${PLUGIN_NAME}`)
	assert.equal(plugin.name, 'treaty:rsbuild', 'name must be treaty:rsbuild')
	assert.equal(typeof plugin.setup, 'function', 'plugin must expose a setup function')
})

// 2. setup wires the transform hook + resolve.extensions (api.transform path)
check('setup registers transform + extends resolve.extensions', () => {
	const plugin = pluginTreaty()
	let transformDescriptor = null
	let transformHandler = null
	const configMods = []
	const api = {
		transform(descriptor, handler) {
			transformDescriptor = descriptor
			transformHandler = handler
		},
		modifyRsbuildConfig(modifier) {
			configMods.push(modifier)
		},
	}
	plugin.setup(api)

	assert.ok(transformDescriptor, 'a transform hook must be registered')
	assert.ok(transformDescriptor.test instanceof RegExp, 'transform descriptor needs a test regex')
	assert.ok(transformDescriptor.test.test('x.treaty'), 'test must match .treaty')
	assert.ok(transformDescriptor.test.test('x.tsx'), 'test must match .tsx')
	assert.equal(typeof transformHandler, 'function', 'transform handler must be a function')

	// Apply the config modifiers and verify resolve.extensions gets ours.
	const config = {}
	for (const mod of configMods) mod(config)
	assert.ok(config.resolve, 'resolve must be populated')
	for (const ext of TREATY_EXTENSIONS) {
		assert.ok(
			config.resolve.extensions.includes(ext),
			`resolve.extensions must include ${ext}`
		)
	}
})

// 3. the registered transform handler lowers a .treaty to Ivy JS
check('transform handler lowers .treaty to Ivy JS', () => {
	const plugin = pluginTreaty()
	let handler = null
	plugin.setup({
		transform(_descriptor, h) {
			handler = h
		},
		modifyRsbuildConfig() {},
	})
	const out = handler({
		code: '<div>hello</div>\n',
		resourcePath: 'logo.treaty',
		resource: 'logo.treaty',
	})
	assert.ok(out && typeof out.code === 'string', 'handler must return { code }')
	assert.ok(out.code.includes('defineComponent'), 'emitted Ivy JS must contain defineComponent')
})

// 4. tools.rspack fallback path when api.transform is absent
check('falls back to tools.rspack module rule when transform is absent', () => {
	const plugin = pluginTreaty()
	const configMods = []
	plugin.setup({
		modifyRsbuildConfig(modifier) {
			configMods.push(modifier)
		},
	})
	const config = {}
	for (const mod of configMods) mod(config)
	assert.ok(config.tools && config.tools.rspack, 'tools.rspack must be set in fallback mode')
	const rspackTool =
		typeof config.tools.rspack === 'function'
			? config.tools.rspack
			: config.tools.rspack[config.tools.rspack.length - 1]
	const rspack = {}
	rspackTool(rspack)
	const rules = rspack.module.rules
	assert.ok(Array.isArray(rules) && rules.length === 1, 'one module rule must be added')
	assert.ok(rules[0].test instanceof RegExp, 'rule needs a test regex')
	assert.ok(/loader\.js$/.test(rules[0].use[0].loader), 'rule must point at the treaty loader')
})

// 5. the loader itself lowers a .treaty to Ivy JS through the core
check('loader lowers .treaty to Ivy JS', () => {
	const out = treatyLoader.call({ resourcePath: 'logo.treaty' }, '<div>hi</div>\n')
	assert.ok(out.includes('defineComponent'), 'loader output must contain defineComponent')
})

// 5b. the transform handler lowers a BARE-JSX .tsx (no @Component) to Ivy JS
check('transform handler lowers bare-JSX .tsx to Ivy JS', () => {
	const plugin = pluginTreaty()
	let handler = null
	plugin.setup({
		transform(_descriptor, h) {
			handler = h
		},
		modifyRsbuildConfig() {},
	})
	const bare = 'export default function App() {\n  return <main>bare</main>\n}\n'
	const out = handler({ code: bare, resourcePath: 'App.tsx', resource: 'App.tsx' })
	assert.ok(out && typeof out.code === 'string', 'handler returns { code } for bare JSX')
	assert.ok(out.code.includes('defineComponent'), 'bare JSX must lower to Ivy JS')
})

// 5c. cold-build prewarm registers an onBeforeBuild callback when the host exposes it
check('prewarm registers an onBeforeBuild cold-build callback', () => {
	const plugin = pluginTreaty({ prewarm: ['ignored-missing.tsx'] })
	let beforeBuild = null
	plugin.setup({
		transform() {},
		modifyRsbuildConfig() {},
		onBeforeBuild(cb) {
			beforeBuild = cb
		},
	})
	assert.equal(typeof beforeBuild, 'function', 'onBeforeBuild must be wired when prewarm is set')
	// Running it with a missing file is a safe no-op (unreadable entries are skipped).
	const ran = beforeBuild()
	assert.ok(ran instanceof Promise, 'onBeforeBuild callback returns a promise')
})

// 5d. without prewarm, onBeforeBuild is left untouched
check('no prewarm => onBeforeBuild is not registered', () => {
	const plugin = pluginTreaty()
	let registered = false
	plugin.setup({
		transform() {},
		modifyRsbuildConfig() {},
		onBeforeBuild() {
			registered = true
		},
	})
	assert.equal(registered, false, 'onBeforeBuild must not be used when no prewarm files are given')
})

// 6. direct core transform of a .treaty yields Ivy JS (proves the wiring works)
check('direct core .treaty transform yields Ivy JS', () => {
	const compiler = createTreatyCompiler()
	const out = compiler.transform('hero.treaty', '<h1>hero</h1>\n')
	assert.ok(out, 'expected a non-null transform result')
	assert.ok(out.code.includes('defineComponent'), 'core output must contain defineComponent')
	assert.equal(out.sideEffects, false, 'pure component module => sideEffects false')
})

for (const line of results) console.log(line)
if (failures > 0) {
	console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSMOKE TEST PASSED')
