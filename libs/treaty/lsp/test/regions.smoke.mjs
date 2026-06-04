/**
 * Node smoke test for REGION-AWARE projection of a `.treaty` single-file
 * component: each authoring region is projected to the embedded language that
 * owns it, so completion follows the cursor —
 *
 *  - the TypeScript body AND every `{{ … }}` interpolation expression project
 *    into ONE embedded `typescript` code (sharing the component scope), each span
 *    completion-enabled, so the TS service drives completion in the body and
 *    INSIDE interpolations;
 *  - every `<style>` block projects into its own embedded `css` code, and the
 *    real CSS language service (volar-service-css) offers a CSS property
 *    completion inside it;
 *  - the HTML/control-flow template region is served by the Treaty template
 *    service (selectorless tags / use: / control-flow) — asserted via the
 *    template-service completion at a tag position here too.
 *
 * Run: node libs/treaty/lsp/test/regions.smoke.mjs
 */

import assert from 'node:assert/strict'
import { TextDocument } from 'vscode-languageserver-textdocument'
import { createTreatyVirtualCode } from '../dist/language.js'
import { ComponentRegistry } from '../dist/component-registry.js'
import { createTemplateService } from '../dist/template-service.js'
import { create as createCssService } from 'volar-service-css'

let failures = 0
const results = []

/** Run an async test case, recording PASS/FAIL. */
async function test(label, fn) {
	try {
		await fn()
		results.push(`PASS ${label}`)
	} catch (err) {
		failures++
		results.push(`FAIL ${label}: ${err.message}`)
	}
}

/** A fixed-string IScriptSnapshot. */
function snapshotOf(text) {
	return {
		getText: (s, e) => text.slice(s, e),
		getLength: () => text.length,
		getChangeRange: () => undefined,
	}
}

/** The embedded code whose mapping covers `offset`, plus that mapping's data. */
function embeddedAt(vc, offset, languageId) {
	for (const code of vc.embeddedCodes) {
		if (languageId && code.languageId !== languageId) continue
		for (const mp of code.mappings) {
			const so = mp.sourceOffsets[0]
			const len = mp.lengths[0]
			if (offset >= so && offset <= so + len) {
				return { code, data: mp.data }
			}
		}
	}
	return undefined
}

const noToken = {
	isCancellationRequested: false,
	onCancellationRequested: () => ({ dispose() {} }),
}

const SOURCE =
	'import { signal } from "@angular/core"\n' +
	'const count = signal(0)\n' +
	'<style>.box { color: red }</style>\n' +
	'<div>{{ count }}</div>'

await test('region: the TS body projects to a completion-enabled typescript code', () => {
	const vc = createTreatyVirtualCode('treaty', snapshotOf(SOURCE))
	const bodyOffset = SOURCE.indexOf('count = signal')
	const hit = embeddedAt(vc, bodyOffset, 'typescript')
	assert.ok(hit, 'the body offset maps into the embedded typescript code')
	assert.equal(hit.code.id, 'ts', 'it is the shared TS code')
	assert.equal(hit.data.completion, true, 'the body span is completion-enabled')
})

await test('region: a {{ }} interpolation projects to the typescript code (TS completion inside {{ }})', () => {
	const vc = createTreatyVirtualCode('treaty', snapshotOf(SOURCE))
	const interpOffset = SOURCE.indexOf('count }}') + 1 // inside `count` in `{{ count }}`
	const hit = embeddedAt(vc, interpOffset, 'typescript')
	assert.ok(hit, 'the interpolation offset maps into the embedded typescript code')
	assert.equal(hit.code.id, 'ts', 'interpolation shares the body TS code (same scope)')
	assert.equal(hit.data.completion, true, 'the interpolation span is completion-enabled')
	const tsText = hit.code.snapshot.getText(0, hit.code.snapshot.getLength())
	assert.ok(/\bcount\b/.test(tsText), 'the TS view carries the interpolation identifier')
})

await test('region: a <style> block projects to a css embedded code', () => {
	const vc = createTreatyVirtualCode('treaty', snapshotOf(SOURCE))
	const css = vc.embeddedCodes.find((c) => c.languageId === 'css')
	assert.ok(css, 'a css embedded code is produced for the <style> block')
	const body = css.snapshot.getText(0, css.snapshot.getLength())
	assert.ok(body.includes('.box'), 'the css code carries the style body')
	assert.ok(!body.includes('<style'), 'the css code excludes the <style> tag itself')
})

await test('region: the CSS service offers a CSS property completion inside <style>', async () => {
	const ctx = {
		env: { workspaceFolders: [] },
		language: { scripts: { get: () => undefined } },
		documents: { get: () => undefined },
		project: {},
	}
	const inst = createCssService().create(ctx)
	// A partial property inside the rule: `.box { col| }`.
	const cssDoc = TextDocument.create('file:///proj/embedded.css', 'css', 1, '.box { col }')
	const pos = { line: 0, character: '.box { col'.length }
	const list = await inst.provideCompletionItems(cssDoc, pos, { triggerKind: 1 }, noToken)
	const items = Array.isArray(list) ? list : (list?.items ?? [])
	assert.ok(items.length > 0, 'the CSS service returns completions inside a rule')
	assert.ok(
		items.some((i) => i.label === 'color' || /^color\b/.test(String(i.label))),
		'a CSS property like `color` is offered inside the <style> block',
	)
})

await test('region: the template region offers selectorless tag completion', () => {
	const registry = new ComponentRegistry()
	registry.indexFile('/proj/src/StatCard.treaty', 'const total = 0\n<div>{{ total }}</div>')
	const inst = createTemplateService(registry).create({
		env: { workspaceFolders: [] },
		language: { scripts: { get: () => undefined } },
	})
	const text = 'const a = 1\n<div><st</div>'
	const doc = TextDocument.create('file:///proj/src/Parent.treaty', 'treaty', 1, text)
	const offset = text.indexOf('<st') + 3
	const list = inst.provideCompletionItems(doc, doc.positionAt(offset), { triggerKind: 1 }, noToken)
	assert.ok(
		list && list.items.some((i) => i.label === 'stat-card'),
		'template tag completion offers <stat-card>',
	)
})

await test('region: a {{ }} inside an @if control-flow block projects to completion-enabled TS', () => {
	const src = 'const ok = true\n<div>\n@if (ok) {\n  <span>{{ ok }}</span>\n}\n</div>'
	const vc = createTreatyVirtualCode('treaty', snapshotOf(src))
	const interpOffset = src.indexOf('ok }}') + 1
	const hit = embeddedAt(vc, interpOffset, 'typescript')
	assert.ok(
		hit && hit.data.completion === true,
		'the {{ ok }} inside @if projects to completion-enabled TS',
	)
})

for (const line of results) console.log(line)
if (failures > 0) {
	console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSMOKE TEST PASSED')
