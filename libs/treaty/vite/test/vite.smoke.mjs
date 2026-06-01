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
 *  10. FUNCTION CHUNKING: a transformed file carrying two server fns emits TWO
 *      code-split chunks (one body each) + a manifest asset, replaces the
 *      component code with the per-fn client bindings, redirects each binding's
 *      `./<id>.server.js` import to a client RPC stub (so the server BODY never
 *      enters the client graph), and the body lives only in its emitted chunk.
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

/**
 * `treaty(...)` returns an ARRAY of plugins (the authoring plugin plus the shared
 * Angular partial-declaration linker plugins). Select the authoring plugin
 * (`treaty:vite`) — the one this suite exercises — from that array.
 */
function authoringPluginOf(plugins) {
	return plugins.find((p) => p && p.name === 'treaty:vite')
}

const plugin = authoringPluginOf(treaty())

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
	assert.equal(typeof plugin.configureServer, 'function', 'configureServer hook present')
})

// helper: drive the plugin's configureServer middleware and return the (possibly
// rewritten) req.url for a given incoming request URL. Captures the single
// middleware the plugin registers and runs it against a minimal connect-style req.
function runDevMiddleware(p, url) {
	let middleware
	const server = { middlewares: { use(fn) { middleware = fn } } }
	p.configureServer.call({}, server)
	assert.equal(typeof middleware, 'function', 'configureServer registers a middleware')
	const req = { url }
	let nexted = false
	middleware(req, {}, () => { nexted = true })
	assert.ok(nexted, 'middleware always calls next()')
	return req.url
}

// 1b. DEV-SERVE MIME FIX: the configureServer middleware forces Vite to serve
//     `.treaty`/`.tjsx` modules with a JS content-type. Vite only labels a served
//     module `text/javascript` when the request reaches its transform branch,
//     which (for these Treaty-only extensions, absent from Vite's known-JS regex)
//     requires an `?import` marker. The middleware injects that marker into a bare
//     owned-extension request URL, so the request goes through transform (-> Ivy
//     JS, JS content-type) instead of falling through to the static/fs middleware
//     that serves the RAW source with an EMPTY content-type (the reported bug:
//     "disallowed MIME type ()" / NS_ERROR_CORRUPTED_CONTENT on a lazy import).
await check('configureServer injects ?import into a bare .treaty request URL', () => {
	assert.equal(
		runDevMiddleware(plugin, '/src/features/greeter/greeter.treaty'),
		'/src/features/greeter/greeter.treaty?import',
		'a bare .treaty request gains the ?import marker so Vite transforms + JS-labels it'
	)
})

await check('configureServer injects ?import into a bare .tjsx request URL', () => {
	assert.equal(
		runDevMiddleware(plugin, '/src/x.tjsx'),
		'/src/x.tjsx?import',
		'a bare .tjsx request gains the ?import marker'
	)
})

await check('configureServer preserves an existing query when injecting ?import', () => {
	assert.equal(
		runDevMiddleware(plugin, '/src/x.treaty?t=123'),
		'/src/x.treaty?import&t=123',
		'?import is prepended ahead of an existing query string (and the query is kept)'
	)
})

await check('configureServer leaves an already-?import .treaty request untouched', () => {
	assert.equal(
		runDevMiddleware(plugin, '/src/x.treaty?import'),
		'/src/x.treaty?import',
		'a request that already carries ?import is not rewritten'
	)
})

await check('configureServer does NOT rewrite .tsx/.ts (Vite already JS-labels them)', () => {
	assert.equal(runDevMiddleware(plugin, '/src/x.tsx'), '/src/x.tsx', '.tsx is a known JS request to Vite')
	assert.equal(runDevMiddleware(plugin, '/src/x.ts'), '/src/x.ts', '.ts is a known JS request to Vite')
})

await check('configureServer does NOT rewrite unowned (.css/.js) requests', () => {
	assert.equal(runDevMiddleware(plugin, '/src/x.css'), '/src/x.css', '.css is untouched')
	assert.equal(runDevMiddleware(plugin, '/src/x.js'), '/src/x.js', '.js is untouched')
})

// helper: call the (async) transform hook with a benign `this` and await it. The
// plugin's `transform` is async (it may run an esbuild type-strip pass on the
// lowered output), so callers must await the result before asserting on it.
async function runTransform(code, id) {
	return plugin.transform.call({}, code, id)
}

// 2. .treaty -> Ivy JS
let treatyOut
await check('transform(.treaty) -> defineComponent', async () => {
	treatyOut = await runTransform('<div>hello</div>\n', 'logo.treaty')
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

// 3b. SOURCE MAP forwarding: a `.ts` @Component yields a v3 map from the core,
//     and the plugin forwards it on transform()'s { code, map } unchanged.
await check('transform forwards the v3 source map when the core produces one', async () => {
	const tsComponent =
		"import { Component } from '@angular/core';\n" +
		"@Component({ selector: 'app-sm', template: '<div>sm</div>' })\n" +
		'export class SmComponent {}\n'
	const out = await runTransform(tsComponent, 'Sm.ts')
	assert.ok(out, 'a .ts @Component is owned and transforms')
	assert.ok(out.code.includes('defineComponent'), 'lowered to Ivy JS')
	assert.equal(typeof out.map, 'string', 'the v3 source map is forwarded as a JSON string')
	const map = JSON.parse(out.map)
	assert.equal(map.version, 3, 'forwarded map is Source Map v3')
	assert.equal(typeof map.mappings, 'string', 'map carries a mappings field')
})

// 3c. sourceMap:false makes the plugin return a null map so Vite skips map work.
await check('sourceMap:false returns a null map', async () => {
	const p = authoringPluginOf(treaty({ sourceMap: false }))
	const tsComponent =
		"import { Component } from '@angular/core';\n" +
		"@Component({ selector: 'app-nm', template: '<div>nm</div>' })\n" +
		'export class NmComponent {}\n'
	const out = await p.transform.call({}, tsComponent, 'Nm.ts')
	assert.ok(out, 'still transforms with sourceMap disabled')
	assert.equal(out.map, null, 'map is null when sourceMap is off')
})

// 4. .tsx @Component -> Ivy JS
await check('transform(.tsx @Component) -> defineComponent', async () => {
	const tsxSource =
		"import { Component } from '@angular/core';\n" +
		"@Component({ selector: 'app-x', template: '<div>x</div>' })\n" +
		'export class XComponent {}\n'
	const out = await runTransform(tsxSource, 'x.tsx')
	assert.ok(out, 'expected a non-null transform result')
	assert.ok(out.code.includes('defineComponent'), 'emitted Ivy JS must contain defineComponent')
})

// 4b. BARE-JSX .tsx (no @Component) now lowers to Ivy via the unified front-end
await check('transform(bare-JSX .tsx) -> defineComponent', async () => {
	const bare = 'export default function App() {\n  return <h1>bare jsx</h1>\n}\n'
	const out = await runTransform(bare, 'App.tsx')
	assert.ok(out, 'bare JSX must compile (no longer rejected)')
	assert.ok(out.code.includes('defineComponent'), 'bare JSX must lower to Ivy JS')
})

// 4c. IDEMPOTENCY: feeding a first pass's lowered Ivy output back through the
//     transform (same owned `.tjsx` id) must NOT recompile — it returns null so the
//     already-lowered JS passes through untouched, so each module compiles exactly once.
await check('transform is idempotent: a second pass over lowered Ivy is skipped', async () => {
	const bare = 'export default function About() {\n  return <div>hi</div>\n}\n'
	const first = await runTransform(bare, 'about.tjsx')
	assert.ok(first && first.code.includes('@angular/core'), 'first pass lowers bare JSX to Ivy')
	// Without the guard, this second pass throws "no component … returning JSX … found".
	const second = await runTransform(first.code, 'about.tjsx')
	assert.equal(second, null, 'lowered Ivy is detected and passed through (no recompile)')
})

// 5. unowned files return null
await check('unowned files return null', async () => {
	assert.equal(await runTransform('export const a = 1\n', 'util.js'), null, 'plain .js => null')
	assert.equal(await runTransform('export const x = 1\n', 'plain.ts'), null, 'non-component .ts => null')
})

// 6. config() teaches esbuild the .tjsx loader
await check('config() registers the .tjsx esbuild loader', () => {
	const cfg = plugin.config.call({}, {}, { command: 'serve', mode: 'development' })
	const loader = cfg?.optimizeDeps?.esbuildOptions?.loader
	assert.ok(loader, 'config returns optimizeDeps.esbuildOptions.loader')
	assert.equal(loader['.tjsx'], 'tsx', '.tjsx maps to the tsx loader')
})

// 6b. DEV-SERVE JSX FIX: config() forces esbuild jsx:'preserve' on BOTH the main
//     transform pass and the dependency scanner, so esbuild's automatic-JSX dev
//     transform never injects an `@treaty/jsx/jsx-dev-runtime` import that would
//     escape to Vite's dep-scan unresolvable. Treaty JSX is Ivy (lowered by the
//     enforce:'pre' transform), NOT React, so JSX must be preserved for the bundler.
await check("config() forces esbuild jsx:'preserve' (scanner + main pass)", () => {
	const cfg = plugin.config.call({}, {}, { command: 'serve', mode: 'development' })
	assert.equal(
		cfg?.esbuild?.jsx,
		'preserve',
		"Vite's main esbuild transform must preserve JSX (drops tsconfig jsxImportSource)"
	)
	assert.equal(
		cfg?.optimizeDeps?.esbuildOptions?.jsx,
		'preserve',
		"the dependency scanner must preserve JSX so no @treaty/jsx runtime import is injected"
	)
})

// 6c. preserveJsx:false opts out (an embedder wanting real React automatic-runtime
//     on .tsx); the jsx override is then absent and esbuild keeps its default mode.
await check('preserveJsx:false leaves esbuild jsx mode untouched', () => {
	const p = authoringPluginOf(treaty({ preserveJsx: false }))
	const cfg = p.config.call({}, {}, { command: 'serve', mode: 'development' })
	assert.equal(cfg?.esbuild, undefined, 'no esbuild.jsx override when preserveJsx is off')
	assert.equal(
		cfg?.optimizeDeps?.esbuildOptions?.jsx,
		undefined,
		'no scanner jsx override when preserveJsx is off'
	)
	// The loader map is still provided regardless of the jsx mode.
	assert.equal(cfg?.optimizeDeps?.esbuildOptions?.loader['.tjsx'], 'tsx', '.tjsx loader still set')
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

	const p = authoringPluginOf(treaty({ prewarm: [file] }))
	// Mark this as a build (cold) so prewarm is allowed to run.
	p.configResolved.call({}, { command: 'build', mode: 'production' })
	// buildStart reads the prewarm files and runs transformMany.
	const started = p.buildStart.call({})
	if (started instanceof Promise) await started

	// The very first transform of the prewarmed file must come back lowered.
	const out = await p.transform.call({}, code, file)
	assert.ok(out, 'prewarmed file transforms to a result')
	assert.ok(out.code.includes('defineComponent'), 'prewarmed bare JSX lowered to Ivy')
})

// 9. prewarm is a no-op in dev (serve): buildStart does nothing, transform still works.
await check('prewarm is a no-op for the dev server', async () => {
	const dir = mkdtempSync(join(tmpdir(), 'treaty-vite-dev-'))
	const file = join(dir, 'Dev.tsx')
	const code = 'export default () => <nav>dev</nav>\n'
	writeFileSync(file, code, 'utf8')

	const p = authoringPluginOf(treaty({ prewarm: [file] }))
	p.configResolved.call({}, { command: 'serve', mode: 'development' })
	const started = p.buildStart.call({})
	if (started instanceof Promise) await started
	// Even without prewarming, the per-file transform still lowers correctly.
	const out = await p.transform.call({}, code, file)
	assert.ok(out && out.code.includes('defineComponent'), 'dev per-file transform still works')
})

// 10. FUNCTION CHUNKING: two server fns -> two emitted chunks + a manifest, with
//     the client code carrying only bindings (no server-fn body).
await check('function chunking emits per-fn chunks + manifest, body never in client', async () => {
	const { splitServerModule } = await import('@treaty/compiler')

	// Unique tokens prove a server-fn body never leaks into client code/stubs.
	const SAVE_BODY = 'PERSIST_TO_DB'
	const LOAD_BODY = 'READ_FROM_DB'
	const FILE = 'dashboard.tsx'
	const serverModule = [
		"const express = require('express');",
		'const app = express();',
		'async function save(record) {',
		`  return ${SAVE_BODY}(record);`,
		'}',
		'async function loadUser(id) {',
		`  return ${LOAD_BODY}(id);`,
		'}',
		"app.post('/__server/save', async (req, res) => { res.json(await save(req.body)); });",
		"app.post('/__server/loadUser', async (req, res) => { res.json(await loadUser(req.body)); });",
		'',
	].join('\n')
	const serverChunks = splitServerModule(FILE, serverModule)
	assert.equal(serverChunks.length, 2, 'fixture must yield two server-fn chunks')

	const CLIENT_CODE = 'export const XComponent = defineComponent();\n'

	// Stub compiler: returns Ivy client code + the two server chunks for our file,
	// null for anything else. Injected via the `compilerFactory` seam so the test
	// drives the plugin's real Vite wiring deterministically.
	const stubCompiler = {
		transform(id, _code) {
			if (id !== FILE) return null
			return { code: CLIENT_CODE, serverChunks, sideEffects: false }
		},
		transformMany() {
			return []
		},
		invalidate() {
			return false
		},
		onDelete() {
			return []
		},
		clearCache() {},
	}

	const p = authoringPluginOf(treaty({ compilerFactory: () => stubCompiler }))

	// Build PluginContext capturing emitted chunks/assets.
	const emitted = []
	const ctx = {
		emitFile(file) {
			emitted.push(file)
			return file.fileName ?? 'ref'
		},
	}

	const out = await p.transform.call(ctx, 'source-ignored', FILE)
	assert.ok(out, 'transform returns a result for the server-fn file')

	// Two server fns -> two emitted CHUNKS, one per fn, with stable file names.
	const chunkFiles = emitted.filter((f) => f.type === 'chunk')
	assert.equal(chunkFiles.length, 2, `expected 2 emitted chunks, got ${chunkFiles.length}`)
	const chunkNames = chunkFiles.map((f) => f.fileName).sort()
	for (const c of serverChunks) {
		assert.ok(
			chunkNames.includes(`${c.id}.server.js`),
			`a chunk must be emitted for ${c.exportName} (${c.id})`
		)
	}

	// The client component code carries ONLY the per-fn bindings; no server body.
	assert.ok(out.code.includes(CLIENT_CODE.trim()), 'client code retains the Ivy component')
	for (const c of serverChunks) {
		assert.ok(out.code.includes(c.clientBinding), `client code carries the ${c.exportName} binding`)
	}
	assert.ok(
		!out.code.includes(SAVE_BODY) && !out.code.includes(LOAD_BODY),
		'NO server-fn body may appear in the client component code'
	)

	// resolveId: the binding's `./<id>.server.js` import is redirected to the
	// per-fn CLIENT stub virtual id (NOT the server body), keeping the body out.
	const save = serverChunks.find((c) => c.exportName === 'save')
	const clientId = await p.resolveId.call(ctx, `./${save.id}.server.js`, FILE, {})
	assert.ok(typeof clientId === 'string', 'client import resolves to a virtual id')
	assert.ok(
		clientId.startsWith('\0treaty-server-fn-client:'),
		'client import redirects to the RPC stub, not the server body'
	)

	// load: the client stub is a fetch-based binding with NO server body in it.
	const stub = p.load.call(ctx, clientId)
	assert.ok(typeof stub === 'string' && stub.includes('fetch('), 'client stub is an RPC binding')
	assert.ok(stub.includes('/__server/save'), 'client stub targets the fn route')
	assert.ok(
		!stub.includes(SAVE_BODY) && !stub.includes(LOAD_BODY),
		'client stub must NOT contain any server-fn body'
	)

	// load: the SERVER body virtual id serves the real chunk body (server side).
	const serverVirtual = `\0treaty-server-fn:${save.id}`
	const body = p.load.call(ctx, serverVirtual)
	assert.ok(typeof body === 'string' && body.includes(SAVE_BODY), 'server chunk carries its body')

	// generateBundle: a manifest asset maps every fn id -> chunk file + export name.
	const genCtx = {
		emitFile(file) {
			emitted.push(file)
			return file.fileName ?? 'ref'
		},
	}
	p.generateBundle.call(genCtx, {}, {})
	const manifestAsset = emitted.find(
		(f) => f.type === 'asset' && f.fileName === 'treaty-server-fns.json'
	)
	assert.ok(manifestAsset, 'a treaty-server-fns.json manifest asset is emitted')
	const manifest = JSON.parse(manifestAsset.source)
	for (const c of serverChunks) {
		const entry = manifest[c.id]
		assert.ok(entry, `manifest must contain fn id ${c.id}`)
		assert.equal(entry.exportName, c.exportName, 'manifest export name matches fn')
		assert.equal(entry.chunkFile, `${c.id}.server.js`, 'manifest points at the emitted chunk file')
	}
	assert.ok(
		!manifestAsset.source.includes(SAVE_BODY) && !manifestAsset.source.includes(LOAD_BODY),
		'manifest must NOT embed any server-fn body'
	)
	results.push(`INFO chunking emitted ${chunkFiles.length} chunks + manifest`)
})

// 11. function chunking can be disabled: no chunks/manifest, code unchanged.
await check('functionChunking:false leaves server fns as a single blob', async () => {
	const { splitServerModule } = await import('@treaty/compiler')
	const FILE = 'plain.tsx'
	const serverModule =
		"const app = require('express')();\n" +
		'async function ping() { return PONG_BODY(); }\n' +
		"app.post('/__server/ping', async (_q, r) => r.json(await ping()));\n"
	const serverChunks = splitServerModule(FILE, serverModule)
	const CLIENT = 'export const P = defineComponent();\n'
	const stub = {
		transform: (id) => (id === FILE ? { code: CLIENT, serverChunks, sideEffects: false } : null),
		transformMany: () => [],
		invalidate: () => false,
		onDelete: () => [],
		clearCache() {},
	}
	const p = authoringPluginOf(treaty({ functionChunking: false, compilerFactory: () => stub }))
	const emitted = []
	const ctx = { emitFile: (f) => (emitted.push(f), 'ref') }
	const out = await p.transform.call(ctx, 'x', FILE)
	assert.equal(out.code, CLIENT, 'code unchanged when chunking is off')
	assert.equal(emitted.length, 0, 'no chunks emitted when chunking is off')
	p.generateBundle.call(ctx, {}, {})
	assert.equal(emitted.length, 0, 'no manifest emitted when chunking is off')
})

for (const line of results) console.log(line)
if (failures > 0) {
	console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSMOKE TEST PASSED')
