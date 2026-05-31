/**
 * Node smoke test for the @treaty/lsp plain-Angular authoring plugin.
 *
 * Asserts that the registry resolves the `angular` plugin for both external
 * Angular templates (`.html`) and ordinary Angular component sources (`.ts`),
 * that it projects each to the right embedded language id, and that a virtual
 * code is produced for an Angular template (highlighting + best-effort checks).
 *
 * Run: node libs/treaty/lsp/test/angular.smoke.mjs
 */

import assert from 'node:assert/strict'
import { resolveByExtension } from '../dist/plugins.js'
import {
	createAngularHtmlVirtualCode,
	createAngularSourceVirtualCode,
	EMBEDDED_HTML_ID,
	EMBEDDED_TS_ID,
} from '../dist/language.js'

/** Minimal IScriptSnapshot over a fixed string. */
function snapshotOf(text) {
	return {
		getText: (start, end) => text.slice(start, end),
		getLength: () => text.length,
		getChangeRange: () => undefined,
	}
}

let failures = 0
const results = []

/** Run a named case, recording PASS/FAIL. */
function test(label, fn) {
	try {
		fn()
		results.push(`PASS ${label}`)
	} catch (err) {
		failures++
		results.push(`FAIL ${label}: ${err.message}`)
	}
}

// The registry routes `.html` to the angular plugin.
test('resolveByExtension(.html) → angular plugin', () => {
	const plugin = resolveByExtension('app/hero.component.html')
	assert.ok(plugin, 'expected a plugin for .html')
	assert.equal(plugin.id, 'angular', 'the .html owner must be the angular plugin')
	assert.equal(
		plugin.languageIdFor?.('app/hero.component.html'),
		'angular-html',
		'.html must refine to the angular-html language id',
	)
})

// The registry routes `.ts` to the angular plugin (base-Angular source path),
// refining to the standard `typescript` language id so the TS service covers it.
test('resolveByExtension(.ts) → angular plugin, typescript language id', () => {
	const plugin = resolveByExtension('app/hero.component.ts')
	assert.ok(plugin, 'expected a plugin for .ts')
	assert.equal(plugin.id, 'angular', 'the .ts owner must be the angular plugin')
	assert.equal(
		plugin.languageIdFor?.('app/hero.component.ts'),
		'typescript',
		'.ts must refine to the typescript language id',
	)
	assert.deepEqual(
		[...plugin.serverLanguages],
		['server:ts'],
		'angular plugin serves server:ts',
	)
})

// An Angular template produces a virtual code whose embedded code is the
// template, mapped 1:1 under the html language id.
test('createVirtualCode produces a virtual code for an Angular template', () => {
	const plugin = resolveByExtension('app/hero.component.html')
	const template = '<h1>{{ title }}</h1>\n<button (click)="onClick()">Go</button>\n'
	const vc = plugin.createVirtualCode(
		'app/hero.component.html',
		'angular-html',
		snapshotOf(template),
		{},
	)
	assert.ok(vc, 'a virtual code must be produced')
	assert.equal(vc.id, 'root', 'root virtual code id')
	assert.equal(vc.languageId, 'angular-html', 'root carries the angular-html language id')
	assert.equal(vc.snapshot.getLength(), template.length, 'root snapshot covers the whole source')
	assert.equal(vc.embeddedCodes.length, 1, 'exactly one embedded code')
	const html = vc.embeddedCodes[0]
	assert.equal(html.id, EMBEDDED_HTML_ID, 'embedded code id is html')
	assert.equal(html.languageId, 'html', 'embedded code is projected as html')
	assert.equal(
		html.snapshot.getText(0, html.snapshot.getLength()),
		template,
		'embedded html is the template text 1:1',
	)
	const m = html.mappings[0]
	assert.deepEqual(m.sourceOffsets, [0], 'mapped from source offset 0')
	assert.deepEqual(m.generatedOffsets, [0], 'mapped to generated offset 0')
	assert.deepEqual(m.lengths, [template.length], 'mapped over the whole length 1:1')
})

// The direct helper and the plugin path agree for templates.
test('createAngularHtmlVirtualCode matches the plugin output', () => {
	const template = '<p>hello</p>'
	const direct = createAngularHtmlVirtualCode('angular-html', snapshotOf(template))
	assert.equal(direct.embeddedCodes[0].languageId, 'html')
	assert.equal(direct.embeddedCodes[0].id, EMBEDDED_HTML_ID)
})

// An Angular component .ts passes through as a 1:1 typescript embedded code.
test('createAngularSourceVirtualCode projects .ts as typescript 1:1', () => {
	const source = '@Component({ template: "" })\nexport class Hero {}\n'
	const vc = createAngularSourceVirtualCode('typescript', snapshotOf(source))
	assert.equal(vc.languageId, 'typescript', 'root carries the typescript language id')
	assert.equal(vc.embeddedCodes.length, 1, 'exactly one embedded code')
	const ts = vc.embeddedCodes[0]
	assert.equal(ts.id, EMBEDDED_TS_ID, 'embedded code id is ts')
	assert.equal(ts.languageId, 'typescript', 'embedded code is projected as typescript')
	assert.equal(
		ts.snapshot.getText(0, ts.snapshot.getLength()),
		source,
		'embedded typescript is the source text 1:1',
	)
	const m = ts.mappings[0]
	assert.deepEqual(m.lengths, [source.length], 'mapped over the whole length 1:1')
})

// A bare external template surfaces no compiler diagnostics of its own.
test('provideDiagnostics on a bare template returns no diagnostics', () => {
	const plugin = resolveByExtension('app/hero.component.html')
	const diags = plugin.provideDiagnostics({
		fileName: 'app/hero.component.html',
		languageId: 'angular-html',
		text: '<h1>{{ title }}</h1>',
	})
	assert.ok(Array.isArray(diags), 'provideDiagnostics returns an array')
	assert.equal(diags.length, 0, 'a bare template is not a compilable module')
})

for (const line of results) console.log(line)
if (failures > 0) {
	console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSMOKE TEST PASSED')
