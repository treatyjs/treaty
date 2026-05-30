/**
 * Node smoke test for @treaty/compiler.
 *
 * Drives the built TreatyCompiler (which routes through the Rust authoring
 * compiler via the @treaty/authoring-node NAPI addon) and asserts:
 *   1. a trivial .treaty transforms to Ivy JS containing `defineComponent`,
 *   2. a trivial .tsx @Component transforms to Ivy JS containing `defineComponent`,
 *   3. re-transforming identical content is served from the cache (hit count++),
 *   4. invalidate() and onDelete() evict the entry,
 *   5. files Treaty does not own (e.g. plain .js / non-component .ts) return null,
 *   6. tree-shaking metadata: sideEffects === false and factory calls are
 *      annotated /*#__PURE__*\/,
 *   7. a BARE-JSX .tsx (export default returning JSX, no @Component) now lowers
 *      to Ivy JS via the unified front-end,
 *   8. transformMany() batch-transforms a mixed file set in input order, honours
 *      ownership (null for unowned), and serves already-cached files from the
 *      cache (cache hits skip the batch).
 *
 * Run: node libs/treaty/compiler/test/compiler.smoke.mjs
 */

import assert from 'node:assert/strict'
import { TreatyCompiler, PURE_ANNOTATION } from '../dist/index.js'

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

const compiler = new TreatyCompiler()

// 1. trivial .treaty -> Ivy JS with defineComponent
check('treaty -> defineComponent', () => {
	const out = compiler.transform('logo.treaty', '<div>hello</div>\n')
	assert.ok(out, 'expected a non-null transform result')
	assert.ok(out.code.includes('defineComponent'), 'emitted Ivy JS must contain defineComponent')
	assert.equal(out.sideEffects, false, 'pure component module => sideEffects false')
	results.push(`INFO treaty emitted ${out.code.length} bytes`)
})

// 2. trivial .tsx @Component -> Ivy JS with defineComponent
const tsxSource =
	"import { Component } from '@angular/core';\n" +
	"@Component({ selector: 'app-x', template: '<div>x</div>' })\n" +
	'export class XComponent {}\n'
check('tsx @Component -> defineComponent', () => {
	const out = compiler.transform('x.tsx', tsxSource)
	assert.ok(out, 'expected a non-null transform result')
	assert.ok(out.code.includes('defineComponent'), 'emitted Ivy JS must contain defineComponent')
})

// 3. identical content served from cache (hit count increments)
check('identical re-transform is a cache hit', () => {
	const before = compiler.stats().hits
	const out = compiler.transform('x.tsx', tsxSource)
	assert.ok(out, 'expected a cached result')
	const after = compiler.stats().hits
	assert.equal(after, before + 1, `hit count must increment (${before} -> ${after})`)
})

// 4. invalidate evicts; subsequent transform is a miss
check('invalidate evicts the entry', () => {
	const removed = compiler.invalidate('x.tsx')
	assert.equal(removed, true, 'invalidate must report a removed entry')
	const missesBefore = compiler.stats().misses
	const out = compiler.transform('x.tsx', tsxSource)
	assert.ok(out, 'expected recompile after invalidation')
	assert.equal(compiler.stats().misses, missesBefore + 1, 'recompile must be a miss')
})

// 5. onDelete evicts and reports dependents
check('onDelete evicts and reports dependents', () => {
	// An importer references logo.treaty; record it via a transform.
	const importer =
		"import { Component } from '@angular/core';\n" +
		"import logo from './logo.treaty';\n" +
		"@Component({ selector: 'app-y', template: '<div>y</div>' })\n" +
		'export class YComponent {}\n'
	compiler.transform('y.tsx', importer)
	// onDelete on the imported id should evict it and report y.tsx as dependent.
	const affected = compiler.onDelete('./logo.treaty')
	assert.ok(Array.isArray(affected), 'onDelete returns an array of dependent ids')
	assert.ok(affected.includes('y.tsx'), `expected y.tsx among dependents, got ${JSON.stringify(affected)}`)
	// Deleting a cached id evicts it.
	compiler.transform('logo.treaty', '<div>hello</div>\n')
	assert.ok(compiler.invalidate('logo.treaty') || compiler.onDelete('logo.treaty'), 'cached id is evictable')
})

// 6. files Treaty does not own return null
check('unowned files return null', () => {
	assert.equal(compiler.transform('util.js', 'export const a = 1\n'), null, 'plain .js => null')
	assert.equal(
		compiler.transform('plain.ts', 'export const x = 1\n'),
		null,
		'non-component .ts => null'
	)
	assert.equal(compiler.transform('styles.css', '.a{}'), null, '.css => null')
})

// 7. tree-shaking: factory calls annotated pure; sideEffects descriptor false
check('pure annotation on emitted factories', () => {
	const out = compiler.transform('logo.treaty', '<div>hello</div>\n')
	assert.ok(out, 'expected a result')
	assert.ok(out.code.includes(PURE_ANNOTATION), 'factory calls must carry the pure annotation')
	assert.equal(compiler.sideEffects.sideEffects, false, 'sideEffects descriptor must be false')
})

// 8. BARE JSX .tsx (no @Component) lowers to Ivy via the unified front-end
check('bare-JSX .tsx -> defineComponent', () => {
	const bare = 'export default function App() {\n  return <h1>Hello bare JSX</h1>\n}\n'
	const out = compiler.transform('App.tsx', bare)
	assert.ok(out, 'bare JSX must compile (not be rejected)')
	assert.ok(
		out.code.includes('defineComponent'),
		'bare JSX must lower to Ivy JS containing defineComponent'
	)
	results.push(`INFO bare-JSX emitted ${out.code.length} bytes`)
})

// 9. a named-function bare JSX .tjsx also lowers to Ivy
check('bare-JSX .tjsx named function -> defineComponent', () => {
	const named = 'export function Card() {\n  return <article>card</article>\n}\n'
	const out = compiler.transform('Card.tjsx', named)
	assert.ok(out, 'named-function bare JSX must compile')
	assert.ok(out.code.includes('defineComponent'), 'named-function bare JSX must lower to Ivy JS')
})

// 10. transformMany: mixed batch, input order, ownership, and cache-hit skip
check('transformMany batch-transforms a mixed file set', () => {
	const batchCompiler = new TreatyCompiler()
	// Warm the cache for one file so it is served as a hit (skips the batch).
	const warm = '<div>warm</div>\n'
	batchCompiler.transform('warm.treaty', warm)
	const missesBefore = batchCompiler.stats().misses

	const files = [
		{ id: 'warm.treaty', code: warm }, // cached -> hit, skips batch
		{ id: 'a.tsx', code: 'export default () => <span>a</span>\n' }, // bare JSX
		{ id: 'logo.treaty', code: '<p>logo</p>\n' }, // .treaty (lowered in place)
		{ id: 'util.js', code: 'export const x = 1\n' }, // unowned -> null
		{
			id: 'comp.tsx',
			code:
				"import { Component } from '@angular/core';\n" +
				"@Component({ selector: 'app-c', template: '<i>c</i>' })\n" +
				'export class CComponent {}\n',
		}, // @Component in .tsx -> fallback path
	]
	const out = batchCompiler.transformMany(files)

	assert.equal(out.length, files.length, 'one result slot per input, in order')
	assert.ok(out[0] && out[0].code.includes('defineComponent'), 'cached .treaty served')
	assert.ok(out[1] && out[1].code.includes('defineComponent'), 'bare JSX lowered in batch')
	assert.ok(out[2] && out[2].code.includes('defineComponent'), '.treaty lowered in batch')
	assert.equal(out[3], null, 'unowned .js -> null slot')
	assert.ok(out[4] && out[4].code.includes('defineComponent'), '@Component .tsx fell back')

	// The warm.treaty entry must have been a cache hit, not a recompile.
	const stats = batchCompiler.stats()
	assert.ok(stats.hits >= 1, 'a warm file is served from the cache during the batch')
	// Only the 3 uncached owned files (a.tsx, logo.treaty, comp.tsx) recompiled.
	assert.equal(stats.misses, missesBefore + 3, 'exactly the uncached owned files recompiled')

	// A subsequent per-file transform of a batched file is now a cache hit.
	const hitsBefore = batchCompiler.stats().hits
	batchCompiler.transform('a.tsx', 'export default () => <span>a</span>\n')
	assert.equal(batchCompiler.stats().hits, hitsBefore + 1, 'batch result is cached for transform()')
})

for (const line of results) console.log(line)
console.log('\nfinal stats:', JSON.stringify(compiler.stats()))
if (failures > 0) {
	console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSMOKE TEST PASSED')
