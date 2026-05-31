/**
 * Node smoke test for @treaty/ssg.
 *
 * Exercises the public surface against the built dist (what a build tool would
 * consume). It asserts:
 *   1. discoverRoutes enumerates static routes (one each), skips wildcards and
 *      pure redirects, and excludes componentless layout shells by default,
 *   2. discoverRoutes fans a parameterized route (`blog/:slug`) out into one
 *      DiscoveredRoute per supplied param set, with params substituted into url,
 *   3. StubRenderRuntime executes a literal-object macro, resolving input.* refs,
 *   4. renderIvyToHtml statically interprets emitted Ivy (from @treaty/compiler)
 *      for an interpolation component into HTML containing the bound text,
 *   5. prerenderRoute produces a full hydration-ready document for a route whose
 *      component renders an interpolation -> static HTML contains the rendered
 *      text + the hydration marker + serialized state,
 *   6. prerenderAll (THE entry point) prerenders a fixture route to static HTML
 *      via an injected writeFile sink, returns a route -> output-file manifest,
 *      and the emitted document contains the macro-rendered interpolation text,
 *   7. a parameterized route prerenders one document per param set, each with its
 *      own substituted url + per-route render data,
 *   8. (when the prebuilt @treaty/authoring-node addon is present) prerenderAll
 *      drives a route through the REAL Nova runtime via createNovaRenderRuntime:
 *      a macro that COMPUTES its render data (a reduce + input ref the stub can
 *      NOT interpret) executes in Nova and the emitted static HTML contains the
 *      computed value.
 *
 * Cases 1–7 use the built-in StubRenderRuntime so they run without the native
 * addon. Case 8 plugs the Nova-backed RenderRuntime (run_macro via
 * @treaty/authoring-node) in verbatim through createNovaRenderRuntime; it is
 * skipped (not failed) when the prebuilt addon is absent.
 *
 * Run: node libs/treaty/ssg/test/ssg.smoke.mjs
 */

import assert from 'node:assert/strict'
import {
	discoverRoutes,
	renderIvyToHtml,
	prerenderRoute,
	prerenderAll,
	StubRenderRuntime,
	createNovaRenderRuntime,
	HYDRATION_MARKER_ATTR,
	HYDRATION_STATE_ID,
} from '../dist/index.js'
import { TreatyCompiler } from '@treaty/compiler'

let failures = 0
const results = []

function check(label, fn) {
	return Promise.resolve()
		.then(fn)
		.then(() => results.push(`PASS ${label}`))
		.catch((err) => {
			failures++
			results.push(`FAIL ${label}: ${err.stack ?? err.message}`)
		})
}

// Try to load the prebuilt Nova addon (@treaty/authoring-node). Returns an
// object satisfying NovaMacroAddon (it exposes runMacro) when the native binary
// is present, or null so the Nova case is skipped rather than failing on a box
// without the prebuilt addon.
async function loadNovaAddon() {
	try {
		const addon = await import('@treaty/authoring-node')
		const runMacro = addon.runMacro ?? addon.default?.runMacro
		return typeof runMacro === 'function' ? { runMacro } : null
	} catch {
		return null
	}
}

// A trivial @Component whose template interpolates a single binding. The
// compiler lowers `{{ title }}` to ɵɵtextInterpolate(ctx.title); the static
// renderer fills it from the route's render data.
const interpolationComponent =
	"import { Component } from '@angular/core';\n" +
	"@Component({ selector: 'app-hello', template: '<h1>{{ title }}</h1>' })\n" +
	'export class HelloComponent {}\n'

// A render-time macro (literal-object form the stub understands). Its result is
// the render data the template binds against. The Nova runtime would execute an
// arbitrary TS macro here; the stub resolves the literal + input.* refs.
const titleMacro = { source: 'export default { title: "Hello from SSG" }' }

await check('discoverRoutes enumerates static routes, skips wildcard/redirect', () => {
	const routes = discoverRoutes([
		{ path: '', component: {} },
		{ path: 'about', component: {} },
		{ path: 'login', redirectTo: 'about', pathMatch: 'full' },
		{ path: '**', component: {} },
		{ path: 'layout', children: [{ path: 'inner', component: {} }] },
	])
	const urls = routes.map((r) => r.url).sort()
	assert.deepEqual(urls, ['/', '/about', '/layout/inner'], 'static + nested only')
	const wild = routes.find((r) => r.routePath === '**')
	assert.equal(wild, undefined, 'wildcard not prerenderable')
	const redirect = routes.find((r) => r.url === '/login')
	assert.equal(redirect, undefined, 'pure redirect not prerenderable')
})

await check('discoverRoutes fans out a parameterized route per param set', () => {
	const routes = discoverRoutes(
		[{ path: 'blog/:slug', component: {} }],
		{ params: { 'blog/:slug': [{ slug: 'hello-world' }, { slug: 'second-post' }] } }
	)
	assert.equal(routes.length, 2, 'one DiscoveredRoute per param set')
	assert.deepEqual(routes.map((r) => r.url).sort(), ['/blog/hello-world', '/blog/second-post'])
	assert.equal(routes[0].parameterized, true, 'flagged parameterized')
	assert.equal(routes[0].params.slug, 'hello-world', 'params carried on the route')

	// No params supplied => the parameterized route is skipped (cannot materialize).
	const none = discoverRoutes([{ path: 'blog/:slug', component: {} }])
	assert.equal(none.length, 0, 'parameterized route with no params yields nothing')
})

await check('StubRenderRuntime executes a literal-object macro with input refs', () => {
	const rt = new StubRenderRuntime()
	const data = rt.runMacro({ source: 'export default { title: input.slug }', input: { slug: 'abc' } })
	assert.equal(data.title, 'abc', 'input.* reference resolved against injected input')
})

await check('renderIvyToHtml interprets emitted Ivy interpolation into HTML', () => {
	const compiler = new TreatyCompiler()
	const out = compiler.transform('hello.tsx', interpolationComponent)
	assert.ok(out, 'component compiled to Ivy')
	const html = renderIvyToHtml(out.code, { title: 'Hello from SSG' })
	assert.equal(html, '<h1>Hello from SSG</h1>', 'static render binds ctx.title from data')
})

await check('prerenderRoute emits a hydration-ready document with rendered text', () => {
	const [route] = discoverRoutes([{ path: 'home', component: {} }])
	const doc = prerenderRoute(
		route,
		{ source: interpolationComponent, macro: titleMacro },
		new StubRenderRuntime(),
		new TreatyCompiler(),
		'Home',
		'en'
	)
	assert.ok(doc, 'a document was produced')
	assert.ok(doc.includes('<!doctype html>'), 'full HTML document')
	assert.ok(doc.includes('<h1>Hello from SSG</h1>'), 'macro-rendered interpolation text present')
	assert.ok(doc.includes(`${HYDRATION_MARKER_ATTR}="1"`), 'hydration marker present')
	assert.ok(doc.includes(HYDRATION_STATE_ID), 'serialized render state embedded for hydration')
	assert.ok(doc.includes('<title>Home</title>'), 'document title applied')
})

await check('prerenderAll prerenders a fixture route -> static HTML + manifest', async () => {
	const written = new Map()
	const manifest = await prerenderAll({
		routes: [{ path: 'greeting', component: {} }],
		resolve: () => ({ source: interpolationComponent, macro: titleMacro }),
		outDir: 'dist/ssg',
		writeFile: async (path, contents) => void written.set(path, contents),
	})
	assert.equal(manifest.routes.length, 1, 'one route prerendered')
	const entry = manifest.routes[0]
	assert.equal(entry.url, '/greeting', 'route url in manifest')
	assert.equal(entry.output, 'dist/ssg/greeting/index.html', 'route -> output file mapping')
	assert.ok(entry.bytes > 0, 'document byte length recorded')

	const doc = written.get(entry.output)
	assert.ok(doc, 'the manifest output path was actually written')
	assert.ok(doc.includes('<h1>Hello from SSG</h1>'), 'emitted static HTML contains the rendered text')
})

await check('prerenderAll fans a parameterized route into one document per param', async () => {
	const written = new Map()
	// Macro reads the route param (merged into macro input as input.slug).
	const slugMacro = { source: 'export default { title: input.slug }' }
	const manifest = await prerenderAll({
		routes: [{ path: 'post/:slug', component: {} }],
		params: { 'post/:slug': [{ slug: 'alpha' }, { slug: 'beta' }] },
		resolve: () => ({ source: interpolationComponent, macro: slugMacro }),
		writeFile: async (path, contents) => void written.set(path, contents),
	})
	assert.equal(manifest.routes.length, 2, 'two documents for two param sets')
	const alpha = manifest.routes.find((r) => r.url === '/post/alpha')
	const beta = manifest.routes.find((r) => r.url === '/post/beta')
	assert.ok(alpha && beta, 'both param urls in manifest')
	assert.ok(written.get(alpha.output).includes('<h1>alpha</h1>'), 'alpha rendered with its param')
	assert.ok(written.get(beta.output).includes('<h1>beta</h1>'), 'beta rendered with its param')
})

// Case 8: the REAL Nova runtime, end-to-end through prerenderAll, when the
// prebuilt @treaty/authoring-node addon is present. The macro COMPUTES its
// render data (an array reduce seeded by input.base) — a form the
// StubRenderRuntime explicitly rejects (it only understands literal objects),
// so a pass here proves the value came out of Nova, not the stub.
const novaAddon = await loadNovaAddon()
if (novaAddon) {
	await check('prerenderAll executes a computed macro through the REAL Nova runtime', async () => {
		const runtime = createNovaRenderRuntime(novaAddon)

		// Sanity: the stub canNOT interpret this macro, so it is a genuine Nova path.
		const computedMacro = {
			source: 'const items = [1, 2, 3, 4]; ({ title: items.reduce((a, b) => a + b, input.base) })',
		}
		assert.throws(
			() => new StubRenderRuntime().runMacro({ ...computedMacro, input: { base: 10 } }),
			/StubRenderRuntime supports only a literal-object macro/,
			'the computed macro is beyond the stub — so case 8 is a true Nova execution'
		)

		// 1 + 2 + 3 + 4 + base(10) = 20, computed by Nova and bound into the template.
		const written = new Map()
		const manifest = await prerenderAll({
			routes: [{ path: 'computed', component: {} }],
			resolve: () => ({ source: interpolationComponent, macro: { ...computedMacro, input: { base: 10 } } }),
			runtime,
			outDir: 'dist/ssg',
			writeFile: async (path, contents) => void written.set(path, contents),
		})
		assert.equal(manifest.routes.length, 1, 'one route prerendered through Nova')
		const doc = written.get(manifest.routes[0].output)
		assert.ok(doc, 'the Nova-prerendered document was written')
		assert.ok(
			doc.includes('<h1>20</h1>'),
			'static HTML contains the value COMPUTED by the Nova runtime (1+2+3+4+10)'
		)
	})
} else {
	results.push('SKIP Nova run_macro case (prebuilt @treaty/authoring-node addon not present)')
}

for (const line of results) console.log(line)
if (failures > 0) {
	console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSMOKE TEST PASSED')
