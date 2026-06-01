/**
 * Node smoke test for @treaty/rspack's Angular partial-declaration linker loader.
 *
 * Does NOT run a full Rspack build (the bundler is a peer dependency and is not installed). Instead
 * it asserts the contracts this package owns:
 *   1. the plugin registers the linker module rule (test matches node_modules .mjs/.js, points at
 *      the link-partial loader) alongside the treaty loader rule;
 *   2. the loader, fed a REAL @angular/common fesm chunk through the rspack loader contract
 *      (this.callback), links it to ZERO residual ɵɵngDeclare and pulls in NO @angular/compiler;
 *   3. the loader passes a non-partial / first-party module through unchanged.
 *
 * The linking itself is the SHARED Rust-backed core from @treaty/ts-vite — this only proves the
 * rspack-native wiring delivers a real partial Angular chunk into that core and returns the linked
 * source.
 *
 * Run: node libs/treaty/rspack/test/link-partial.smoke.mjs
 */

import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import {
	TreatyRspackPlugin,
	linkPartialRule,
	linkPartialLoader,
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

// 1. the plugin registers the linker rule alongside the treaty loader rule.
check('plugin registers the link-partial module rule', () => {
	const plugin = new TreatyRspackPlugin({ moduleFederation: false })
	const config = { module: {}, resolve: {} }
	plugin.apply({ options: config })
	const rules = config.module.rules
	assert.ok(Array.isArray(rules) && rules.length >= 2, 'plugin must add at least two rules')
	const linkRule = rules.find(
		(r) => r.use && r.use[0] && /link-partial-loader\.js$/.test(r.use[0].loader)
	)
	assert.ok(linkRule, 'a rule must point at the link-partial loader')
	assert.ok(linkRule.test instanceof RegExp, 'link rule needs a test regex')
	assert.ok(
		linkRule.test.test('/x/node_modules/@angular/common/fesm2022/common.mjs'),
		'test must match node_modules .mjs'
	)
	assert.ok(!linkRule.test.test('/src/app.mjs'), 'test must not match first-party .mjs')
	assert.equal(linkRule.type, 'javascript/auto', 'link rule must use javascript/auto for .mjs')
})

// 1b. linkPartialRule() standalone matches the same contract.
check('linkPartialRule builder matches node_modules js and points at the loader', () => {
	const rule = linkPartialRule()
	assert.ok(rule.test instanceof RegExp && rule.test === LINK_PARTIAL_TEST, 'exported test reused')
	assert.ok(/link-partial-loader\.js$/.test(rule.use[0].loader), 'rule points at link loader')
	assert.equal(rule.use[0].options, undefined, 'linker loader takes no options')
})

// 2. the loader links a REAL @angular/common fesm chunk to zero residual ngDeclare.
check('loader links a real @angular/common chunk to zero residual ngDeclare', () => {
	const source = readFileSync(FIXTURE, 'utf8')
	assert.ok(source.includes(PARTIAL_MARKER), 'fixture must actually be partial-compiled')
	assert.ok(isPartialModule(FIXTURE, source), 'isPartialModule must flag the real chunk')

	let cbErr
	let linked
	const ctx = {
		resourcePath: FIXTURE,
		resource: FIXTURE,
		callback(err, content) {
			cbErr = err
			linked = content
		},
	}
	const ret = linkPartialLoader.call(ctx, source)
	assert.equal(ret, undefined, 'with a callback the loader returns nothing')
	assert.ok(!cbErr, `loader must not error: ${cbErr && cbErr.message}`)
	assert.ok(typeof linked === 'string' && linked.length > 0, 'loader must emit linked source')
	assert.ok(linked !== source, 'partial source must be rewritten')
	assert.equal(
		(linked.match(/ɵɵngDeclare/g) || []).length,
		0,
		'linked output must contain ZERO residual ɵɵngDeclare'
	)
	assert.ok(/ɵɵdefine/.test(linked), 'linked output must contain AOT ɵɵdefine* calls')
	assert.ok(!linked.includes('@angular/compiler'), 'linked output must NOT import @angular/compiler')
})

// 3. a non-partial / first-party module passes through unchanged.
check('non-partial module passes through unchanged', () => {
	const firstParty = '/src/app.mjs'
	const code = 'export const answer = 42\n'
	let linked
	linkPartialLoader.call({ resourcePath: firstParty, callback: (_e, c) => (linked = c) }, code)
	assert.equal(linked, code, 'first-party source must be returned unchanged')

	// A node_modules module with no ɵɵngDeclare is also a pass-through.
	const vendorPlain = '/x/node_modules/lodash/index.mjs'
	let linked2
	linkPartialLoader.call({ resourcePath: vendorPlain, callback: (_e, c) => (linked2 = c) }, code)
	assert.equal(linked2, code, 'non-partial vendor source must be returned unchanged')
})

// 4. without a callback (minimal context) the loader returns the value directly.
check('minimal context (no callback) returns the linked source directly', () => {
	const source = readFileSync(FIXTURE, 'utf8')
	const out = linkPartialLoader.call({ resourcePath: FIXTURE }, source)
	assert.ok(typeof out === 'string', 'sync return must be a string')
	assert.equal((out.match(/ɵɵngDeclare/g) || []).length, 0, 'sync return is fully linked')
})

for (const line of results) console.log(line)
if (failures > 0) {
	console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSMOKE TEST PASSED')
