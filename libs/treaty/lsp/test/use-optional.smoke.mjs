/**
 * Node smoke test for use:-OPTIONAL selector-directive completion + the new
 * authoring-syntax intelligence:
 *
 *  - a BARE `routerLink` (a built-in Angular attribute-selector directive)
 *    completes in attribute position WITHOUT `use:`, and accepting it
 *    auto-imports `RouterLink` from `@angular/router` (no NgModule) — recognized
 *    by its attribute selector, the way the SelectorRegistry resolves it;
 *  - an IMPORTED `@Directive({ selector: '[appHighlight]' })` likewise completes
 *    bare as `appHighlight`;
 *  - the same works for a `.tsx` Treaty JSX file (bare attribute in a JSX tag);
 *  - `use:routerLink` still completes (explicit selectorless is still valid);
 *  - hover annotates a destructured-props input binding as an `input()` and the
 *    inline `server { … }` block as "runs on the server".
 *
 * Run: node libs/treaty/lsp/test/use-optional.smoke.mjs
 */

import assert from 'node:assert/strict'
import { TextDocument } from 'vscode-languageserver-textdocument'
import { ComponentRegistry } from '../dist/component-registry.js'
import { createTemplateService } from '../dist/template-service.js'

let failures = 0
const results = []
function test(label, fn) {
	try {
		fn()
		results.push(`PASS ${label}`)
	} catch (err) {
		failures++
		results.push(`FAIL ${label}: ${err.message}`)
	}
}

function fakeContext() {
	return {
		env: { workspaceFolders: [] },
		language: { scripts: { get: () => undefined } },
	}
}

const noCtx = { triggerKind: 1 }
const noToken = {
	isCancellationRequested: false,
	onCancellationRequested: () => ({ dispose() {} }),
}

function docOf(uri, languageId, text) {
	return TextDocument.create(uri, languageId, 1, text)
}

/** A template service whose registry knows an imported `[appHighlight]` directive. */
function instance() {
	const registry = new ComponentRegistry()
	// An imported attribute-selector directive (its selector comes from the Rust
	// scanner, set here directly): className AppHighlight, selector [appHighlight].
	registry.setProjectSelectors({ AppHighlight: '[appHighlight]' })
	registry.indexFile(
		'/proj/src/highlight.directive.ts',
		'@Directive({ selector: "[appHighlight]" })\nexport class AppHighlight {}',
	)
	return { registry, inst: createTemplateService(registry).create(fakeContext()) }
}

// 1. Bare `routerLink` completes in attribute position WITHOUT use:, and
//    auto-imports RouterLink from @angular/router.
test('.treaty: bare routerLink completes + auto-imports RouterLink (no use:)', () => {
	const { inst } = instance()
	const text = 'const a = 1\n<a rout></a>'
	const doc = docOf('file:///proj/src/Nav.treaty', 'treaty', text)
	const offset = text.indexOf('rout') + 'rout'.length
	const list = inst.provideCompletionItems(doc, doc.positionAt(offset), noCtx, noToken)
	assert.ok(list && Array.isArray(list.items), 'a completion list is returned in attribute position')
	const item = list.items.find((i) => i.label === 'routerLink')
	assert.ok(item, 'bare `routerLink` is offered (no use: needed)')
	assert.ok(item.detail.includes('RouterLink'), 'detail names the RouterLink class')
	assert.ok(
		Array.isArray(item.additionalTextEdits) && item.additionalTextEdits.length === 1,
		'an auto-import edit accompanies the bare directive',
	)
	const imp = item.additionalTextEdits[0].newText
	assert.ok(imp.includes('import { RouterLink }'), 'the auto-import brings in RouterLink')
	assert.ok(imp.includes('@angular/router'), 'RouterLink is imported from @angular/router (bare specifier)')
})

// 2. The documentation hints that use: is optional/redundant for a selector directive.
test('.treaty: bare routerLink documentation hints use: is optional', () => {
	const { inst } = instance()
	const text = 'const a = 1\n<a rout></a>'
	const doc = docOf('file:///proj/src/Nav.treaty', 'treaty', text)
	const offset = text.indexOf('rout') + 'rout'.length
	const list = inst.provideCompletionItems(doc, doc.positionAt(offset), noCtx, noToken)
	const item = list.items.find((i) => i.label === 'routerLink')
	const doc2 = item.documentation.value || item.documentation
	assert.ok(/optional|redundant/i.test(doc2), 'documentation hints use: is optional/redundant')
})

// 3. An imported `[appHighlight]` directive completes bare as `appHighlight`.
test('.treaty: an imported [appHighlight] directive completes bare', () => {
	const { inst } = instance()
	const text = 'const a = 1\n<div appH></div>'
	const doc = docOf('file:///proj/src/Box.treaty', 'treaty', text)
	const offset = text.indexOf('appH') + 'appH'.length
	const list = inst.provideCompletionItems(doc, doc.positionAt(offset), noCtx, noToken)
	const item = list.items.find((i) => i.label === 'appHighlight')
	assert.ok(item, 'bare `appHighlight` is offered for the imported directive')
	assert.ok(item.detail.includes('AppHighlight'), 'detail names AppHighlight')
	assert.ok(item.additionalTextEdits, 'an auto-import edit accompanies the imported directive')
})

// 4. The same bare-attribute directive completion works in a .tsx Treaty file.
test('.tsx: bare routerLink completes + auto-imports in JSX', () => {
	const { inst } = instance()
	const text = 'export default function Nav() {\n  return <a rout></a>\n}'
	const doc = docOf('file:///proj/src/Nav.tsx', 'treaty-jsx', text)
	const offset = text.indexOf('rout') + 'rout'.length
	const list = inst.provideCompletionItems(doc, doc.positionAt(offset), noCtx, noToken)
	assert.ok(list, 'a list is returned in JSX attribute position')
	const item = list.items.find((i) => i.label === 'routerLink')
	assert.ok(item, 'bare routerLink is offered in JSX too')
	assert.ok(item.additionalTextEdits, 'JSX bare directive also carries an auto-import edit')
})

// 5. `use:routerLink` STILL completes (explicit selectorless is valid + optional).
test('.treaty: use:routerLink still completes (use: stays valid)', () => {
	const { inst } = instance()
	const text = 'const a = 1\n<a use:rout></a>'
	const doc = docOf('file:///proj/src/Nav.treaty', 'treaty', text)
	const offset = text.indexOf('use:rout') + 'use:rout'.length
	const list = inst.provideCompletionItems(doc, doc.positionAt(offset), noCtx, noToken)
	assert.ok(list, 'a list is returned in use: position')
	const item = list.items.find((i) => i.label === 'routerLink')
	assert.ok(item, 'use:routerLink completes (by attribute name)')
})

// 6. A .tsx file yields Treaty (selectorless) completions — proving .tsx is a
//    first-class Treaty JSX authoring format.
test('.tsx: yields Treaty selectorless tag completions', () => {
	const registry = new ComponentRegistry()
	registry.indexFile('/proj/src/StatCard.treaty', 'const total = 0\n<div>{{ total }}</div>')
	const inst = createTemplateService(registry).create(fakeContext())
	const text = 'export default function App() {\n  return <div><st</div>\n}'
	const doc = docOf('file:///proj/src/App.tsx', 'treaty-jsx', text)
	const offset = text.indexOf('<st') + 3
	const list = inst.provideCompletionItems(doc, doc.positionAt(offset), noCtx, noToken)
	assert.ok(
		list && list.items.some((i) => i.label === 'stat-card'),
		'.tsx offers the selectorless <stat-card> tag',
	)
})

// 7. Hover annotates a destructured-props input binding as an input().
test('hover: a destructured-props binding is annotated as an input()', () => {
	const { inst } = instance()
	const text = 'function Greeting({ label, count = 5 }) {\n  return <h1>{{ label }}</h1>\n}'
	const doc = docOf('file:///proj/src/Greeting.tsx', 'treaty-jsx', text)
	const offset = text.indexOf('label,') // hover the `label` prop in the destructure
	const hover = inst.provideHover(doc, doc.positionAt(offset), noToken)
	assert.ok(hover, 'a hover is returned on a destructured input')
	assert.ok(/input\(\)/.test(hover.contents.value), 'hover annotates it as an input()')
})

// 8. Hover annotates the inline `server { … }` block keyword as "runs on the server".
test('hover: the inline server { } block is annotated runs-on-server', () => {
	const { inst } = instance()
	const text = 'const a = 1\nserver {\n  function save() {}\n}\n<div></div>'
	const doc = docOf('file:///proj/src/Save.treaty', 'treaty', text)
	const offset = text.indexOf('server') + 2 // inside the `server` keyword
	const hover = inst.provideHover(doc, doc.positionAt(offset), noToken)
	assert.ok(hover, 'a hover is returned on the server keyword')
	assert.ok(/server/i.test(hover.contents.value), 'hover mentions the server')
	assert.ok(/runs on the server/i.test(hover.contents.value), 'hover says it runs on the server')
})

for (const line of results) console.log(line)
if (failures > 0) {
	console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSMOKE TEST PASSED')
