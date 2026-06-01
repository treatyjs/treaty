/**
 * Node smoke test for @treaty/rsbuild's Angular partial-declaration linker.
 *
 * Does NOT run a full Rsbuild build (the bundler is a peer dependency and is not installed). Instead
 * it asserts the contracts this package owns:
 *   1. pluginTreatyLinkPartial registers an api.transform whose test matches node_modules .mjs/.js
 *      (and not first-party .mjs);
 *   2. the registered transform handler, fed a REAL @angular/common fesm chunk, links it to ZERO
 *      residual ɵɵngDeclare and pulls in NO @angular/compiler;
 *   3. pluginTreaty (the main plugin) ALSO registers the linker transform, so a normal Treaty
 *      rsbuild build links partial Angular automatically;
 *   4. a non-partial / first-party module passes through unchanged.
 *
 * The linking itself is the SHARED Rust-backed core from @treaty/ts-vite — this only proves the
 * rsbuild-native wiring delivers a real partial Angular chunk into that core and returns the link.
 *
 * Run: node libs/treaty/rsbuild/test/link-partial.smoke.mjs
 */

import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import {
	pluginTreaty,
	pluginTreatyLinkPartial,
	LINK_PARTIAL_PLUGIN_NAME,
	LINK_PARTIAL_TEST,
	isPartialModule,
} from '../dist/index.js'

const PARTIAL_MARKER = 'ɵɵngDeclare'

/** A real published partial Angular fesm chunk to link end-to-end. */
const FIXTURE = fileURLToPath(
	new URL('../../../../node_modules/@angular/common/fesm2022/_location-chunk.mjs', import.meta.url)
)

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

/**
 * Drive a plugin's setup with a fake api that records every transform registration, returning the
 * descriptor + handler of the transform matched to the link-partial test.
 */
function captureLinkTransform(plugin) {
	const transforms = []
	const api = {
		transform(descriptor, handler) {
			transforms.push({ descriptor, handler })
		},
		modifyRsbuildConfig() {},
		processAssets() {},
		onBeforeBuild() {},
	}
	plugin.setup(api)
	return transforms.find((t) => t.descriptor.test === LINK_PARTIAL_TEST) ?? null
}

// 1. standalone linker plugin shape + transform registration.
check('pluginTreatyLinkPartial registers the linker transform', () => {
	const plugin = pluginTreatyLinkPartial()
	assert.equal(plugin.name, LINK_PARTIAL_PLUGIN_NAME, 'plugin name must be stable')
	const link = captureLinkTransform(plugin)
	assert.ok(link, 'a transform matched to the link-partial test must be registered')
	assert.ok(
		link.descriptor.test.test('/x/node_modules/@angular/common/fesm2022/common.mjs'),
		'test must match node_modules .mjs'
	)
	assert.ok(!link.descriptor.test.test('/src/app.mjs'), 'test must not match first-party .mjs')
	assert.equal(typeof link.handler, 'function', 'handler must be a function')
})

// 2. the registered handler links a REAL @angular/common fesm chunk to zero residual ngDeclare.
check('linker transform links a real @angular/common chunk to zero residual ngDeclare', () => {
	const source = readFileSync(FIXTURE, 'utf8')
	assert.ok(source.includes(PARTIAL_MARKER), 'fixture must actually be partial-compiled')
	assert.ok(isPartialModule(FIXTURE, source), 'isPartialModule must flag the real chunk')

	const link = captureLinkTransform(pluginTreatyLinkPartial())
	const out = link.handler({ code: source, resourcePath: FIXTURE, resource: FIXTURE })
	const linked = typeof out === 'string' ? out : out.code
	assert.ok(typeof linked === 'string' && linked.length > 0, 'handler must emit linked source')
	assert.ok(linked !== source, 'partial source must be rewritten')
	assert.equal(
		(linked.match(/ɵɵngDeclare/g) || []).length,
		0,
		'linked output must contain ZERO residual ɵɵngDeclare'
	)
	assert.ok(/ɵɵdefine/.test(linked), 'linked output must contain AOT ɵɵdefine* calls')
	assert.ok(!linked.includes('@angular/compiler'), 'linked output must NOT import @angular/compiler')
})

// 3. the MAIN plugin (pluginTreaty) also wires the linker transform.
check('pluginTreaty wires the linker transform automatically', () => {
	const link = captureLinkTransform(pluginTreaty())
	assert.ok(link, 'pluginTreaty must register the link-partial transform alongside authoring')
	const source = readFileSync(FIXTURE, 'utf8')
	const out = link.handler({ code: source, resourcePath: FIXTURE, resource: FIXTURE })
	const linked = typeof out === 'string' ? out : out.code
	assert.equal(
		(linked.match(/ɵɵngDeclare/g) || []).length,
		0,
		'pluginTreaty must link partial Angular to zero residual ɵɵngDeclare'
	)
})

// 4. a non-partial / first-party module passes through unchanged.
check('non-partial module passes through unchanged', () => {
	const link = captureLinkTransform(pluginTreatyLinkPartial())
	const code = 'export const answer = 42\n'
	// node_modules but no ɵɵngDeclare:
	const vendor = link.handler({
		code,
		resourcePath: '/x/node_modules/lodash/index.mjs',
		resource: '/x/node_modules/lodash/index.mjs',
	})
	assert.equal(typeof vendor === 'string' ? vendor : vendor.code, code, 'plain vendor unchanged')
})

for (const line of results) console.log(line)
if (failures > 0) {
	console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSMOKE TEST PASSED')
