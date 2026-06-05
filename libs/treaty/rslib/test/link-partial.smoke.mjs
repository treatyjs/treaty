/**
 * Node smoke test for @treaty/rslib's Angular partial-declaration linker.
 *
 * rslib builds on rsbuild, so the linker is the SAME shared Rust-backed core wired through an
 * rsbuild transform. Does NOT run a full rslib build (the bundler is a peer dependency, not
 * installed). It asserts the contracts this package owns:
 *   1. the wired plugin (treatyRsbuildPlugin / defineTreatyLib) registers a transform whose test
 *      matches node_modules .mjs/.js (and not first-party .mjs) alongside the authoring transform;
 *   2. that linker transform, fed a REAL @angular/common fesm chunk, links it to ZERO residual
 *      ɵɵngDeclare and pulls in NO @angular/compiler;
 *   3. a non-partial / first-party module passes through unchanged.
 *
 * Run: node libs/treaty/rslib/test/link-partial.smoke.mjs
 */

import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import {
	defineTreatyLib,
	treatyRsbuildPlugin,
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
 * Drive a plugin's setup with a fake api recording every transform registration, returning the
 * descriptor + handler of the transform matched to the link-partial test.
 */
function captureLinkTransform(plugin) {
	const transforms = []
	const api = {
		transform(descriptor, handler) {
			transforms.push({ descriptor, handler })
		},
		processAssets() {},
		onBeforeBuild() {},
	}
	plugin.setup(api)
	return transforms.find((t) => t.descriptor.test === LINK_PARTIAL_TEST) ?? null
}

// 1. the wired plugin registers the linker transform alongside the authoring transform.
check('treatyRsbuildPlugin registers the linker transform', () => {
	const link = captureLinkTransform(treatyRsbuildPlugin())
	assert.ok(link, 'a transform matched to the link-partial test must be registered')
	assert.ok(
		link.descriptor.test.test('/x/node_modules/@angular/common/fesm2022/common.mjs'),
		'test must match node_modules .mjs'
	)
	assert.ok(!link.descriptor.test.test('/src/lib.mjs'), 'test must not match first-party .mjs')
	assert.equal(typeof link.handler, 'function', 'handler must be a function')
})

// 2. the linker transform links a REAL @angular/common fesm chunk to zero residual ngDeclare.
check('linker transform links a real @angular/common chunk to zero residual ngDeclare', () => {
	const source = readFileSync(FIXTURE, 'utf8')
	assert.ok(source.includes(PARTIAL_MARKER), 'fixture must actually be partial-compiled')
	assert.ok(isPartialModule(FIXTURE, source), 'isPartialModule must flag the real chunk')

	const link = captureLinkTransform(treatyRsbuildPlugin())
	const out = link.handler({ code: source, resource: FIXTURE })
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

// 3. defineTreatyLib's wired plugin also links partial Angular.
check('defineTreatyLib plugin links partial Angular', () => {
	const cfg = defineTreatyLib()
	const link = captureLinkTransform(cfg.plugins[0])
	assert.ok(link, 'the library preset must wire the linker transform')
	const source = readFileSync(FIXTURE, 'utf8')
	const out = link.handler({ code: source, resource: FIXTURE })
	const linked = typeof out === 'string' ? out : out.code
	assert.equal(
		(linked.match(/ɵɵngDeclare/g) || []).length,
		0,
		'defineTreatyLib must link partial Angular to zero residual ɵɵngDeclare'
	)
})

// 4. a non-partial / first-party module passes through unchanged.
check('non-partial module passes through unchanged', () => {
	const link = captureLinkTransform(treatyRsbuildPlugin())
	const code = 'export const answer = 42\n'
	const vendor = link.handler({ code, resource: '/x/node_modules/lodash/index.mjs' })
	assert.equal(typeof vendor === 'string' ? vendor : vendor.code, code, 'plain vendor unchanged')
})

for (const line of results) console.log(line)
if (failures > 0) {
	console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSMOKE TEST PASSED')
