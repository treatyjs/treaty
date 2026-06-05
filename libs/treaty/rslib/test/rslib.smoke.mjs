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
import {
	defineTreatyLib,
	treatyRsbuildPlugin,
	ANGULAR_EXTERNAL,
	TREATY_PLUGIN_NAME,
	ServerChunkCollector,
	emitServerChunks,
	serverChunkFileName,
	SERVER_FN_MANIFEST_NAME,
	SERVER_FN_BARREL_NAME,
} from '../dist/index.js'
import { createTreatyCompiler, splitServerModule } from '@treaty/compiler'

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

// ---------------------------------------------------------------------------
// FUNCTION CHUNKING for libraries: each extracted server fn becomes its OWN
// separately-exported chunk entry — a `<id>.server.js` chunk (the body), a
// `server/index.js` barrel re-exporting each fn from its own chunk, and a
// manifest. The Rust addon's server-module extraction is owned by another
// workflow, so we drive the wiring deterministically with synthesized server
// chunks (the shape @treaty/compiler attaches as TransformResult.serverChunks).
// ---------------------------------------------------------------------------

const SAVE_TOKEN = 'PERSIST_TO_DB'
const LOAD_TOKEN = 'READ_FROM_DB'
const SERVER_MODULE = [
	"const express = require('express');",
	'const app = express();',
	'async function save(record) {',
	`  return ${SAVE_TOKEN}(record);`,
	'}',
	'async function loadUser(id) {',
	`  return ${LOAD_TOKEN}(id);`,
	'}',
	"app.post('/__server/save', async (req, res) => { res.json(await save(req.body)); });",
	"app.post('/__server/loadUser', async (req, res) => { res.json(await loadUser(req.body)); });",
	'',
].join('\n')

const SERVER_CHUNKS = splitServerModule('src/data.treaty', SERVER_MODULE)
const serverResult = { code: 'export const x = 1\n', sideEffects: false, serverChunks: SERVER_CHUNKS }

/** A minimal Rspack `sources`/`compilation` pair capturing emitAsset calls. */
function fakeAssetArgs() {
	const assets = {}
	return {
		compilation: {
			assets,
			emitAsset(name, source) {
				assets[name] = source
			},
		},
		sources: {
			RawSource: class {
				#v
				constructor(value) {
					this.#v = value
				}
				source() {
					return this.#v
				}
				size() {
					return this.#v.length
				}
			},
		},
		_assets: assets,
	}
}

// 6. each server fn becomes its own chunk; a barrel re-exports each from its chunk.
check('library emits one chunk per fn + a re-export barrel + manifest', () => {
	const collector = new ServerChunkCollector()
	collector.add(serverResult)
	const assets = collector.assets()
	const names = assets.map((a) => a.name)
	for (const c of SERVER_CHUNKS) {
		assert.ok(names.includes(serverChunkFileName(c.id)), `chunk file for ${c.exportName}`)
	}
	assert.ok(names.includes(SERVER_FN_BARREL_NAME), 'a server-fn barrel must be emitted')
	assert.ok(names.includes(SERVER_FN_MANIFEST_NAME), 'a manifest must be emitted')
	// one chunk per fn + barrel + manifest.
	assert.equal(assets.length, SERVER_CHUNKS.length + 2, 'chunks + barrel + manifest')

	// The barrel re-exports each fn from ITS OWN chunk (separately code-splittable).
	const barrel = assets.find((a) => a.name === SERVER_FN_BARREL_NAME).source
	for (const c of SERVER_CHUNKS) {
		assert.ok(
			barrel.includes(`export { ${c.exportName} }`),
			`barrel re-exports ${c.exportName}`
		)
		assert.ok(barrel.includes(serverChunkFileName(c.id)), `barrel points ${c.exportName} at its own chunk`)
	}
	// The barrel must never inline a server fn body — only re-exports.
	assert.ok(!barrel.includes(SAVE_TOKEN) && !barrel.includes(LOAD_TOKEN), 'barrel carries no fn body')
})

// 7. per-fn chunk bodies are isolated; the manifest maps each id -> export name.
check('library per-fn chunks isolate bodies; manifest maps id -> export', () => {
	const collector = new ServerChunkCollector()
	collector.add(serverResult)
	const assets = collector.assets()
	const save = SERVER_CHUNKS.find((c) => c.exportName === 'save')
	const load = SERVER_CHUNKS.find((c) => c.exportName === 'loadUser')
	const saveAsset = assets.find((a) => a.name === serverChunkFileName(save.id))
	const loadAsset = assets.find((a) => a.name === serverChunkFileName(load.id))
	assert.ok(saveAsset.source.includes(SAVE_TOKEN) && !saveAsset.source.includes(LOAD_TOKEN), 'save body isolated')
	assert.ok(loadAsset.source.includes(LOAD_TOKEN) && !loadAsset.source.includes(SAVE_TOKEN), 'loadUser body isolated')

	const manifest = JSON.parse(assets.find((a) => a.name === SERVER_FN_MANIFEST_NAME).source)
	assert.equal(Object.keys(manifest).length, 2, 'manifest has one entry per fn')
	for (const c of SERVER_CHUNKS) {
		assert.equal(manifest[c.id].exportName, c.exportName, 'manifest export name matches chunk')
		assert.equal(manifest[c.id].chunkRef, c.id, 'manifest chunkRef is the stable id')
	}
})

// 8. emitServerChunks writes the chunk + barrel + manifest into the compilation.
check('emitServerChunks emits library server assets via compilation', () => {
	const collector = new ServerChunkCollector()
	collector.add(serverResult)
	const args = fakeAssetArgs()
	emitServerChunks(collector, args)
	const emitted = Object.keys(args._assets)
	for (const c of SERVER_CHUNKS) {
		assert.ok(emitted.includes(serverChunkFileName(c.id)), `chunk file emitted for ${c.exportName}`)
	}
	assert.ok(emitted.includes(SERVER_FN_BARREL_NAME), 'barrel emitted into compilation')
	assert.ok(emitted.includes(SERVER_FN_MANIFEST_NAME), 'manifest emitted into compilation')
	// No server fn body ever appears in a client binding.
	for (const c of SERVER_CHUNKS) {
		assert.ok(
			!c.clientBinding.includes(SAVE_TOKEN) && !c.clientBinding.includes(LOAD_TOKEN),
			'no server fn body leaks into a client binding'
		)
	}
})

// 9. the wired plugin registers processAssets (the emit hook for library chunks).
check('wired plugin registers a processAssets hook', () => {
	const plugin = treatyRsbuildPlugin()
	let assetsHandler = null
	let stage = null
	plugin.setup({
		transform() {},
		processAssets(descriptor, handler) {
			stage = descriptor.stage
			assetsHandler = handler
		},
	})
	assert.equal(typeof assetsHandler, 'function', 'processAssets handler must be registered')
	assert.equal(typeof stage, 'string', 'processAssets descriptor carries a stage')
})

// 10. defineTreatyLib's wired plugin also exposes the processAssets emit hook.
check('defineTreatyLib plugin exposes the processAssets emit hook', () => {
	const cfg = defineTreatyLib()
	const plugin = cfg.plugins[0]
	let registered = false
	plugin.setup({
		transform() {},
		processAssets() {
			registered = true
		},
	})
	assert.equal(registered, true, 'the library preset wires per-fn chunk emission')
})

// 11. a pure library (no server fns) emits no extra assets.
check('pure library emits no server assets', () => {
	const collector = new ServerChunkCollector()
	collector.add({ code: 'export const y = 2\n', sideEffects: false })
	assert.ok(collector.isEmpty, 'collector empty for a result with no server chunks')
	const args = fakeAssetArgs()
	emitServerChunks(collector, args)
	assert.equal(Object.keys(args._assets).length, 0, 'pure library emits no server assets')
})

for (const line of results) console.log(line)
if (failures > 0) {
	console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSMOKE TEST PASSED')
