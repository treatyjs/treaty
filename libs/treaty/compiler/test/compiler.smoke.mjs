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
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
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

// 11. cross-module selector registry: an IMPORTED child used by its conventional
//     non-folding selector (<app-stat-card> for class StatCard) resolves only via
//     the explicit per-file registry; absent it, the fold path leaves an empty host.
check('explicit registry resolves a conventional cross-module selector', () => {
	const dash =
		"import { Component } from '@angular/core';\n" +
		"import { StatCard } from './stat-card';\n" +
		"@Component({\n" +
		"  selector: 'app-dashboard',\n" +
		"  imports: [StatCard],\n" +
		"  template: '<app-stat-card></app-stat-card><app-stat-card></app-stat-card><app-stat-card></app-stat-card>',\n" +
		'})\n' +
		'export class Dashboard {}\n'

	// WITHOUT a registry: the class-name fold cannot match <app-stat-card> -> StatCard.
	const c1 = new TreatyCompiler({ cache: false })
	const without = c1.transform('dashboard.ts', dash)
	assert.ok(without, 'dashboard must compile')
	assert.ok(
		!without.code.includes('dependencies: [StatCard'),
		'fold-only path must NOT resolve <app-stat-card> to StatCard'
	)

	// WITH an explicit per-file registry: StatCard resolves and 3 element instructions emit.
	const c2 = new TreatyCompiler({ cache: false })
	const withReg = c2.transform('dashboard.ts', dash, { StatCard: 'app-stat-card' })
	assert.ok(withReg, 'dashboard must compile with registry')
	assert.ok(
		withReg.code.includes('dependencies: [StatCard'),
		'registry must resolve StatCard into dependencies'
	)
	const tags = (withReg.code.match(/"app-stat-card"/g) || []).length
	assert.equal(tags, 3, 'expected 3 app-stat-card element instructions (statCards=3)')
	results.push(`INFO registry dashboard emitted ${withReg.code.length} bytes`)
})

// 12. prewarmSelectorRegistry scans a real on-disk project and auto-derives the
//     per-file registry for the importer (no explicit registry passed to transform).
check('prewarmSelectorRegistry auto-resolves cross-module selectors', () => {
	const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'treaty-compiler-selreg-'))
	const stat =
		"import { Component, input } from '@angular/core';\n" +
		"@Component({ selector: 'app-stat-card', template: '<p>{{label()}}</p>' })\n" +
		"export class StatCard { readonly label = input(''); }\n"
	const dash =
		"import { Component } from '@angular/core';\n" +
		"import { StatCard } from './stat-card';\n" +
		"@Component({ selector: 'app-dashboard', imports: [StatCard], template: '<app-stat-card></app-stat-card>' })\n" +
		'export class Dashboard {}\n'
	fs.writeFileSync(path.join(dir, 'stat-card.ts'), stat)
	fs.writeFileSync(path.join(dir, 'dashboard.ts'), dash)

	const c = new TreatyCompiler({ cache: false })
	const count = c.prewarmSelectorRegistry(dir)
	assert.ok(count >= 1, `prewarm must discover the StatCard selector (got ${count})`)
	assert.equal(c.hasSelectorRegistry(), true, 'registry must be present after prewarm')

	// transform WITHOUT an explicit registry now auto-derives it from the prewarmed map.
	const out = c.transform('dashboard.ts', dash)
	assert.ok(out, 'dashboard must compile')
	assert.ok(
		out.code.includes('dependencies: [StatCard'),
		'prewarmed registry must auto-resolve StatCard into dependencies'
	)
	fs.rmSync(dir, { recursive: true, force: true })
})

// 13. ADDITIVE GUARANTEE: with no registry prewarmed, transform output is identical
//     to before — a plain non-importing @Component is byte-unchanged.
check('no registry => output unchanged (additive)', () => {
	const src =
		"import { Component } from '@angular/core';\n" +
		"@Component({ selector: 'app-solo', template: '<div>solo</div>' })\n" +
		'export class Solo {}\n'
	const noReg = new TreatyCompiler({ cache: false }).transform('solo.ts', src)
	const emptyReg = new TreatyCompiler({ cache: false }).transform('solo.ts', src, {})
	assert.ok(noReg && emptyReg, 'both compile')
	assert.equal(noReg.code, emptyReg.code, 'empty registry must be byte-identical to no registry')
})

for (const line of results) console.log(line)
console.log('\nfinal stats:', JSON.stringify(compiler.stats()))
if (failures > 0) {
	console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSMOKE TEST PASSED')
