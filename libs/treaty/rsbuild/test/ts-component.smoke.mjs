/**
 * Node smoke test for @treaty/rsbuild's plain-`.ts` Angular-decorator lowering —
 * the feature this phase brings to PARITY with @treaty/vite.
 *
 * Does NOT run a full Rsbuild build (the bundler is a peer dependency and is not
 * installed). It drives the plugin's registered `api.transform` handler (Strategy 1)
 * and the `tools.rspack` loader (Strategy 2) directly and asserts:
 *   1. DEFAULT_TEST now matches `.ts` (and `.treaty`/`.tsx`/`.tjsx`) but never `.d.ts`
 *      or `.js`/`.mjs`;
 *   2. the transform handler lowers a `.ts` `@Component`, `@Directive`, `@Pipe`,
 *      `@Injectable`, and `@NgModule` to its Ivy definition;
 *   3. a non-Angular `.ts` is passed through unchanged (so Rsbuild's own TS pipeline
 *      handles it) — proving ownership is decided by the core compiler, not the regex;
 *   4. the loader (Strategy 2 fallback) lowers a `.ts` `@Component` and passes a
 *      plain `.ts` through, matching the transform path.
 *
 * Run: node libs/treaty/rsbuild/test/ts-component.smoke.mjs
 */

import assert from 'node:assert/strict'
import { pluginTreaty, treatyLoader, DEFAULT_TEST } from '../dist/index.js'

let failures = 0
const results = []

function check(label, fn) {
	try {
		fn()
		results.push(`PASS ${label}`)
	} catch (err) {
		failures++
		results.push(`FAIL ${label}: ${err.stack ?? err.message}`)
	}
}

/** Capture the api.transform handler the plugin registers (Strategy 1). */
function transformHandlerOf(plugin) {
	let handler = null
	plugin.setup({
		transform(_descriptor, h) {
			handler = h
		},
		modifyRsbuildConfig() {},
	})
	return handler
}

/** The five Angular decorator kinds, each as a minimal `.ts` source. */
const DECORATOR_SOURCES = {
	Component:
		"import { Component } from '@angular/core'\n@Component({ selector: 'a-x', template: '<i>x</i>' })\nexport class XComponent {}\n",
	Directive:
		"import { Directive } from '@angular/core'\n@Directive({ selector: '[appX]' })\nexport class XDir {}\n",
	Pipe: "import { Pipe } from '@angular/core'\n@Pipe({ name: 'x' })\nexport class XPipe { transform(v) { return v } }\n",
	Injectable:
		"import { Injectable } from '@angular/core'\n@Injectable({ providedIn: 'root' })\nexport class XSvc {}\n",
	NgModule: "import { NgModule } from '@angular/core'\n@NgModule({})\nexport class XMod {}\n",
}

// 1. DEFAULT_TEST matches .ts (and the JSX/.treaty exts) but never .d.ts / .js.
check('DEFAULT_TEST matches .ts/.treaty/.tsx/.tjsx but never .d.ts or .js', () => {
	assert.ok(DEFAULT_TEST.test('app.component.ts'), 'matches .ts')
	assert.ok(DEFAULT_TEST.test('x.treaty'), 'matches .treaty')
	assert.ok(DEFAULT_TEST.test('x.tsx'), 'matches .tsx')
	assert.ok(DEFAULT_TEST.test('x.tjsx'), 'matches .tjsx')
	assert.ok(!DEFAULT_TEST.test('app.d.ts'), 'must NOT match .d.ts')
	assert.ok(!DEFAULT_TEST.test('app.js'), 'must NOT match .js')
	assert.ok(!DEFAULT_TEST.test('app.mjs'), 'must NOT match .mjs')
})

// 2. the transform handler lowers every .ts Angular decorator kind to Ivy.
check('transform handler lowers every .ts Angular decorator kind to Ivy', () => {
	const handler = transformHandlerOf(pluginTreaty())
	for (const [kind, src] of Object.entries(DECORATOR_SOURCES)) {
		const out = handler({ code: src, resourcePath: `${kind}.ts`, resource: `${kind}.ts` })
		assert.ok(out && typeof out.code === 'string', `${kind}: handler returns { code }`)
		assert.notEqual(out.code, src, `${kind}: .ts is rewritten (lowered, not passed through)`)
		assert.ok(/ɵɵdefine|ɵfac/.test(out.code), `${kind}: .ts lowers to an Ivy ɵɵdefine*/ɵfac`)
	}
})

// 3. a non-Angular .ts is passed through unchanged (core decides ownership).
check('non-Angular .ts is passed through unchanged', () => {
	const handler = transformHandlerOf(pluginTreaty())
	const plain = 'export const answer = 42\nexport function add(a, b) { return a + b }\n'
	const out = handler({ code: plain, resourcePath: 'util.ts', resource: 'util.ts' })
	const code = typeof out === 'string' ? out : out.code
	assert.equal(code, plain, 'plain .ts must reach Rsbuild’s own TS pipeline untouched')
})

// 4. the loader (Strategy 2 fallback) matches the transform path for .ts.
check('loader lowers a .ts @Component and passes a plain .ts through', () => {
	const lowered = treatyLoader.call(
		{ resourcePath: 'x.component.ts' },
		DECORATOR_SOURCES.Component
	)
	assert.ok(lowered.includes('defineComponent'), 'loader lowers .ts @Component to Ivy')
	const plain = 'export const x = 1\n'
	assert.equal(treatyLoader.call({ resourcePath: 'util.ts' }, plain), plain, 'plain .ts unchanged')
})

for (const line of results) console.log(line)
if (failures > 0) {
	console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSMOKE TEST PASSED')
