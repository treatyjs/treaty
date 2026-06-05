/**
 * Node smoke test for @treaty/rsbuild's server-fn DEV BACKEND — the `/__server/*`
 * route that runs the real server-fn body in dev, brought to PARITY with @treaty/vite.
 *
 * Does NOT run a full Rsbuild dev server (the bundler is a peer dependency and is not
 * installed). It drives the contracts this package owns:
 *   1. the route/arg helpers reused from the shared @treaty/vite backend behave;
 *   2. pluginTreaty registers a `dev.setupMiddlewares` entry that unshifts the
 *      server-fn middleware onto Rsbuild's dev middleware chain;
 *   3. the plugin's transform POPULATES the dev registry from a real authoring file's
 *      server fns (export name -> original module id);
 *   4. END TO END: the registered dev middleware, fed a `/__server/<name>` request,
 *      loads a REAL on-disk `.ts` server module through the Node-loader ssrLoadModule
 *      adapter, runs the genuine body, and replies with its JSON result — and streams
 *      an async-generator export as Server-Sent Events;
 *   5. an unregistered `/__server/*` name 404s and a non-server path falls through.
 *
 * Run: node libs/treaty/rsbuild/test/dev-backend.smoke.mjs
 */

import assert from 'node:assert/strict'
import { mkdtempSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import {
	pluginTreaty,
	devBackendConfigModifier,
	createServerFnMiddleware,
	parseServerFnRoute,
	decodeArgs,
	SERVER_ROUTE_PREFIX,
} from '../dist/index.js'

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

// ---------------------------------------------------------------------------
// 1. Shared route/arg helpers (reused from @treaty/vite).
// ---------------------------------------------------------------------------

await check('shared route + arg helpers behave', () => {
	assert.equal(SERVER_ROUTE_PREFIX, '/__server/', 'route prefix is /__server/')
	assert.equal(parseServerFnRoute('/__server/listTodos'), 'listTodos', 'plain route parses')
	assert.equal(parseServerFnRoute('/__server/streamLogs?t=1'), 'streamLogs', 'query stripped')
	assert.equal(parseServerFnRoute('/src/x.ts'), null, 'non-server path is null')
	assert.equal(parseServerFnRoute('/__server/a/b'), null, 'nested path rejected')
	assert.deepEqual(decodeArgs(''), [], 'empty body -> no args')
	assert.deepEqual(decodeArgs('5'), [5], 'bare value -> one positional arg')
	assert.deepEqual(decodeArgs('[1,2]'), [1, 2], 'array -> spread positional args')
})

// ---------------------------------------------------------------------------
// 2. pluginTreaty wires a dev.setupMiddlewares entry on the api.transform path.
// ---------------------------------------------------------------------------

await check('pluginTreaty wires a dev.setupMiddlewares entry', () => {
	const plugin = pluginTreaty()
	const configMods = []
	plugin.setup({
		transform() {},
		modifyRsbuildConfig(modifier) {
			configMods.push(modifier)
		},
	})
	const config = {}
	for (const mod of configMods) mod(config)
	assert.ok(config.dev && Array.isArray(config.dev.setupMiddlewares), 'dev.setupMiddlewares array set')
	assert.ok(config.dev.setupMiddlewares.length >= 1, 'at least one setupMiddlewares entry')
	// The entry unshifts a middleware onto the dev chain.
	const stack = { unshifted: [], unshift(fn) { this.unshifted.push(fn) }, push() {} }
	config.dev.setupMiddlewares[config.dev.setupMiddlewares.length - 1](stack, {})
	assert.equal(stack.unshifted.length, 1, 'one server-fn middleware unshifted onto the chain')
	assert.equal(typeof stack.unshifted[0], 'function', 'the unshifted entry is a middleware fn')
})

// ---------------------------------------------------------------------------
// 3. The plugin transform populates the dev registry from a real authoring file's
//    server fns. We capture the SAME registry instance the dev modifier closed over by
//    reproducing the plugin's wiring against a fake api, then run a server-fn-bearing
//    authoring file through the registered transform and assert the registry filled.
// ---------------------------------------------------------------------------

/**
 * A `.tsx` authoring component declaring one `'use server'` function. The real
 * `@treaty/compiler` front-end lifts the body into a server chunk and leaves the client
 * an RPC binding — so the plugin's transform sees `result.serverChunks` and must
 * register the fn into the dev registry.
 */
const SERVER_COMPONENT = [
	'export async function addThing(n: number) {',
	"  'use server'",
	'  return n + 1',
	'}',
	'export default () => <button>add</button>',
	'',
].join('\n')

await check('plugin transform registers discovered server fns into the dev registry', () => {
	const plugin = pluginTreaty()
	let transformHandler = null
	const configMods = []
	plugin.setup({
		transform(_d, h) {
			transformHandler = h
		},
		modifyRsbuildConfig(modifier) {
			configMods.push(modifier)
		},
	})
	// Apply every modifier against one config so the dev modifier's setupMiddlewares
	// captures the SAME registry the transform handler writes to.
	const config = {}
	for (const mod of configMods) mod(config)

	// Drive the registered transform over the REAL authoring file: it lowers to Ivy AND,
	// because the front-end lifted a server fn, registers that fn into the dev registry.
	const out = transformHandler({
		code: SERVER_COMPONENT,
		resourcePath: '/app/src/widget.tsx',
		resource: '/app/src/widget.tsx',
	})
	assert.ok(out && out.code.includes('defineComponent'), 'authoring file lowers to Ivy JS')

	// Capture the middleware the dev modifier registered (over the SAME registry the
	// transform just wrote to) and prove the fn route is now registered: a registered
	// name is CLAIMED (never falls through to next()), whereas an unregistered name would
	// 404 here. The fn's original module id is the `.tsx` authoring file (which Node
	// cannot natively import), so the body load fails into a 500 — but a 500, not a 404,
	// is exactly what proves the route WAS registered by the transform.
	const stack = { mw: null, unshift(fn) { this.mw = fn }, push() {} }
	config.dev.setupMiddlewares[config.dev.setupMiddlewares.length - 1](stack, {})
	const middleware = stack.mw
	assert.equal(typeof middleware, 'function', 'a middleware was registered')

	return new Promise((resolve, reject) => {
		const req = makeReq('/__server/addThing')
		const res = makeRes((status) => {
			try {
				assert.notEqual(status, 404, 'a REGISTERED fn must not 404 (transform registered it)')
				resolve()
			} catch (e) {
				reject(e)
			}
		})
		middleware(req, res, () =>
			reject(new Error('a registered /__server route must be claimed, not passed to next()'))
		)
	})
})

// ---------------------------------------------------------------------------
// 4. END TO END: the dev middleware runs a REAL on-disk .ts server module body.
// ---------------------------------------------------------------------------

/** Build a fake connect req carrying a POST body and the `on('data'|'end')` stream. */
function makeReq(url, body = '') {
	const handlers = {}
	const req = {
		url,
		method: 'POST',
		on(event, cb) {
			handlers[event] = cb
			return req
		},
	}
	// Drive the body stream on next tick so `readBody` can attach its listeners first.
	queueMicrotask(() => {
		if (body) handlers.data?.(body)
		handlers.end?.()
	})
	return req
}

/** Build a fake connect res that captures status + body and notifies on `end`. */
function makeRes(onEnd) {
	const chunks = []
	return {
		statusCode: 0,
		headers: {},
		setHeader(k, v) {
			this.headers[k.toLowerCase()] = v
		},
		write(chunk) {
			chunks.push(chunk)
			return true
		},
		end(body) {
			if (body !== undefined) chunks.push(body)
			this.body = chunks.join('')
			onEnd(this.statusCode, this.body, this.headers)
		},
	}
}

await check('dev middleware runs a real .ts server fn body and replies with JSON', () => {
	const dir = mkdtempSync(join(tmpdir(), 'treaty-rsbuild-srv-'))
	const modPath = join(dir, 'todos.server.ts')
	// A genuine TS server module: Node (>=22) strips the type annotations on import.
	writeFileSync(
		modPath,
		'export async function addTodo(title: string): Promise<{ id: number; title: string }> {\n' +
			'  return { id: 7, title }\n' +
			'}\n'
	)
	const registry = new Map([['addTodo', { exportName: 'addTodo', moduleId: modPath }]])
	const config = {}
	devBackendConfigModifier(registry)(config)
	const stack = { mw: null, unshift(fn) { this.mw = fn }, push() {} }
	config.dev.setupMiddlewares[0](stack, {})
	const middleware = stack.mw

	return new Promise((resolve, reject) => {
		const req = makeReq('/__server/addTodo', JSON.stringify('hello'))
		const res = makeRes((status, body, headers) => {
			try {
				assert.equal(status, 200, 'a successful server fn replies 200')
				assert.match(headers['content-type'] ?? '', /application\/json/, 'JSON content-type')
				assert.deepEqual(JSON.parse(body), { id: 7, title: 'hello' }, 'the REAL body ran server-side')
				resolve()
			} catch (e) {
				reject(e)
			}
		})
		middleware(req, res, (err) => reject(err ?? new Error('server-fn request must be claimed')))
	})
})

await check('dev middleware streams an async-generator server fn as SSE', () => {
	const dir = mkdtempSync(join(tmpdir(), 'treaty-rsbuild-sse-'))
	const modPath = join(dir, 'ticks.server.ts')
	writeFileSync(
		modPath,
		'export async function* ticks(n: number) {\n' +
			'  for (let i = 0; i < n; i++) yield i\n' +
			'}\n'
	)
	const registry = new Map([['ticks', { exportName: 'ticks', moduleId: modPath }]])
	const config = {}
	devBackendConfigModifier(registry)(config)
	const stack = { mw: null, unshift(fn) { this.mw = fn }, push() {} }
	config.dev.setupMiddlewares[0](stack, {})
	const middleware = stack.mw

	return new Promise((resolve, reject) => {
		const req = makeReq('/__server/ticks', JSON.stringify(3))
		const res = makeRes((status, body, headers) => {
			try {
				assert.equal(status, 200, 'streaming reply is 200')
				assert.match(headers['content-type'] ?? '', /text\/event-stream/, 'SSE content-type')
				assert.ok(body.includes('data: 0'), 'first yield streamed')
				assert.ok(body.includes('data: 2'), 'last yield streamed')
				assert.ok(body.includes('event: end'), 'stream terminated with an end frame')
				resolve()
			} catch (e) {
				reject(e)
			}
		})
		middleware(req, res, (err) => reject(err ?? new Error('streaming request must be claimed')))
	})
})

// ---------------------------------------------------------------------------
// 5. Unregistered name 404s; a non-server path falls through to next().
// ---------------------------------------------------------------------------

await check('unregistered /__server name 404s; non-server path falls through', () => {
	const middleware = createServerFnMiddleware(
		{ middlewares: { use() {} }, ssrLoadModule: async () => ({}) },
		new Map()
	)
	return new Promise((resolve, reject) => {
		const req = makeReq('/__server/doesNotExist')
		const res = makeRes((status) => {
			try {
				assert.equal(status, 404, 'an unregistered fn 404s explicitly')
			} catch (e) {
				return reject(e)
			}
			// A non-server path must fall through to next().
			let nexted = false
			const req2 = { url: '/index.html', method: 'GET', on() {} }
			const res2 = makeRes(() => reject(new Error('a non-server path must not be answered here')))
			middleware(req2, res2, () => {
				nexted = true
			})
			try {
				assert.equal(nexted, true, 'a non-server path calls next()')
				resolve()
			} catch (e) {
				reject(e)
			}
		})
		middleware(req, res, () => reject(new Error('a /__server/* path must be claimed, not passed through')))
	})
})

for (const line of results) console.log(line)
if (failures > 0) {
	console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSMOKE TEST PASSED')
