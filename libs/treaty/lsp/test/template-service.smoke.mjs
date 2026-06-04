/**
 * Node smoke test for the @treaty/lsp Treaty TEMPLATE language-service plugin.
 *
 * Drives the plugin's create() against a minimal LanguageServiceContext and
 * exercises the Treaty-only intelligence the TS service cannot provide:
 *  - selectorless component tag completions WITH an auto-import edit (no NgModule)
 *  - `use:` directive completions
 *  - @if/@for/… control-flow snippet completions
 *  - hover on a selectorless tag (resolved class + signals reminder)
 *  - go-to-definition from a tag to the declaring file
 *  - registry-aware compiler diagnostics
 * for BOTH .treaty and JSX (.tsx) documents.
 *
 * Run: node libs/treaty/lsp/test/template-service.smoke.mjs
 */

import assert from 'node:assert/strict'
import { mkdtempSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { TextDocument } from 'vscode-languageserver-textdocument'
// Import the specific dist modules (not ../dist/index.js) so the test does not
// transitively load server.js — whose `@volar/language-server/node` import only
// resolves under the server's own module conditions, not a bare `node` run.
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

/** A minimal LanguageServiceContext: no workspace folders, no TS project. */
function fakeContext() {
	return {
		env: { workspaceFolders: [] },
		language: { scripts: { get: () => undefined } },
	}
}

/** Instantiate the plugin with a pre-seeded registry of two child components. */
function instance() {
	const registry = new ComponentRegistry()
	// A .treaty component (filename fold → <stat-card>) and a directive.
	registry.indexFile('/proj/src/StatCard.treaty', 'const total = 0\n<div>{{ total }}</div>')
	registry.indexFile(
		'/proj/src/Highlight.treaty',
		"host { '[class.on]': 'on' }\nconst on = true",
	)
	const plugin = createTemplateService(registry)
	return { registry, inst: plugin.create(fakeContext()) }
}

function docOf(uri, languageId, text) {
	return TextDocument.create(uri, languageId, 1, text)
}

const noCtx = { triggerKind: 1 }
const noToken = { isCancellationRequested: false, onCancellationRequested: () => ({ dispose() {} }) }

// 1. Selectorless tag completion WITH auto-import in a .treaty template.
test('.treaty: selectorless tag completion offers <stat-card> with auto-import', () => {
	const { inst } = instance()
	const text = 'import { signal } from "@angular/core"\nconst a = 1\n<div><st</div>'
	const doc = docOf('file:///proj/src/Parent.treaty', 'treaty', text)
	const offset = text.indexOf('<st') + 3 // cursor right after `<st`
	const pos = doc.positionAt(offset)
	const list = inst.provideCompletionItems(doc, pos, noCtx, noToken)
	assert.ok(list && Array.isArray(list.items), 'a completion list is returned')
	const item = list.items.find((i) => i.label === 'stat-card')
	assert.ok(item, 'the selectorless <stat-card> tag is offered')
	assert.ok(item.detail.includes('StatCard'), 'detail names the StatCard class')
	assert.ok(
		Array.isArray(item.additionalTextEdits) && item.additionalTextEdits.length === 1,
		'an auto-import edit accompanies the tag',
	)
	assert.ok(
		item.additionalTextEdits[0].newText.includes("import { StatCard }"),
		'the auto-import brings in StatCard',
	)
})

// 2. A tag the file ALREADY imports gets no duplicate import edit.
test('.treaty: tag completion omits the import edit when already imported', () => {
	const { inst } = instance()
	const text = "import { StatCard } from './StatCard'\n<div><st</div>"
	const doc = docOf('file:///proj/src/Parent.treaty', 'treaty', text)
	const offset = text.indexOf('<st') + 3
	const list = inst.provideCompletionItems(doc, doc.positionAt(offset), noCtx, noToken)
	const item = list.items.find((i) => i.label === 'stat-card')
	assert.ok(item, 'the tag is still offered')
	assert.ok(!item.additionalTextEdits, 'no duplicate import edit when already imported')
})

// 3. Control-flow snippet completion after `@`.
test('.treaty: @ offers control-flow block snippets (@if/@for/…)', () => {
	const { inst } = instance()
	const text = 'const a = 1\n<div>\n@i\n</div>'
	const doc = docOf('file:///proj/src/Parent.treaty', 'treaty', text)
	const offset = text.indexOf('@i') + 2
	const list = inst.provideCompletionItems(doc, doc.positionAt(offset), noCtx, noToken)
	assert.ok(list, 'a list is returned for an @-head')
	const ifItem = list.items.find((i) => i.label === '@if')
	assert.ok(ifItem, '@if is offered')
	assert.equal(ifItem.insertTextFormat, 2, '@if is a snippet (InsertTextFormat.Snippet)')
	assert.ok(ifItem.textEdit.newText.includes('@if ('), 'the snippet expands an @if block')
})

// 4. `use:` directive completion.
test('.treaty: use: offers selectorless directives', () => {
	const { inst } = instance()
	const text = 'const a = 1\n<div use:h></div>'
	const doc = docOf('file:///proj/src/Parent.treaty', 'treaty', text)
	const offset = text.indexOf('use:h') + 'use:h'.length
	const list = inst.provideCompletionItems(doc, doc.positionAt(offset), noCtx, noToken)
	assert.ok(list, 'a list is returned in use: position')
	const dir = list.items.find((i) => i.label === 'highlight')
	assert.ok(dir, 'the highlight directive is offered under use:')
	assert.ok(dir.detail.includes('Highlight'), 'detail names the Highlight class')
})

// 5. Hover on a selectorless tag.
test('.treaty: hover on a selectorless tag resolves the class + signals note', () => {
	const { inst } = instance()
	const text = 'const a = 1\n<stat-card></stat-card>'
	const doc = docOf('file:///proj/src/Parent.treaty', 'treaty', text)
	const offset = text.indexOf('stat-card') + 2 // inside the tag name
	const hover = inst.provideHover(doc, doc.positionAt(offset), noToken)
	assert.ok(hover, 'a hover is returned')
	assert.ok(hover.contents.value.includes('StatCard'), 'hover names the resolved class')
	assert.ok(/signal/i.test(hover.contents.value), 'hover carries the signals reminder')
	assert.ok(/NgModule/.test(hover.contents.value), 'hover notes selectorless / no NgModule')
})

// 6. Go-to-definition from a selectorless tag to its declaring file.
test('.treaty: definition on a selectorless tag points at the declaring file', () => {
	const { inst } = instance()
	const text = 'const a = 1\n<stat-card></stat-card>'
	const doc = docOf('file:///proj/src/Parent.treaty', 'treaty', text)
	const offset = text.indexOf('stat-card') + 2
	const defs = inst.provideDefinition(doc, doc.positionAt(offset), noToken)
	assert.ok(Array.isArray(defs) && defs.length === 1, 'one definition link is returned')
	assert.ok(/StatCard\.treaty$/.test(defs[0].targetUri), 'it targets StatCard.treaty')
})

// 7. Registry-aware compiler diagnostics over a broken .treaty.
test('.treaty: provideDiagnostics surfaces compiler errors', () => {
	const { inst } = instance()
	const text = 'const a = 1\n<div>{{ }}</div>'
	const doc = docOf('file:///proj/src/Broken.treaty', 'treaty', text)
	const diags = inst.provideDiagnostics(doc, noToken)
	assert.ok(Array.isArray(diags), 'an array of diagnostics is returned')
	assert.ok(diags.length >= 1, 'the blank interpolation is reported')
	assert.equal(diags[0].source, 'treaty', 'the diagnostic is tagged treaty')
})

// 8. JSX: selectorless lowercase tag completion in a .tsx file.
test('.tsx: selectorless lowercase tag completion offers <stat-card>', () => {
	const { inst } = instance()
	const text = 'export default function App() {\n  return <div><st</div>\n}'
	const doc = docOf('file:///proj/src/App.tsx', 'treaty-jsx', text)
	const offset = text.indexOf('<st') + 3
	const list = inst.provideCompletionItems(doc, doc.positionAt(offset), noCtx, noToken)
	assert.ok(list, 'a list is returned in JSX tag position')
	const item = list.items.find((i) => i.label === 'stat-card')
	assert.ok(item, 'the selectorless tag is offered in JSX too')
	assert.ok(item.additionalTextEdits, 'JSX tag also carries an auto-import edit')
})

// 9. A non-Treaty document is ignored by the plugin.
test('plugin ignores non-Treaty documents', () => {
	const { inst } = instance()
	const doc = docOf('file:///proj/src/plain.ts', 'typescript', 'const a = 1')
	const list = inst.provideCompletionItems(doc, doc.positionAt(0), noCtx, noToken)
	assert.equal(list, undefined, 'plain .ts gets no Treaty template completions')
})

// 10. A .ts Angular component contributes its real selector to the registry.
test('registry indexes a .ts @Component selector for cross-file tags', () => {
	const registry = new ComponentRegistry()
	registry.setProjectSelectors({ AppCard: 'app-card' })
	registry.indexFile(
		'/proj/src/app-card.component.ts',
		'@Component({ selector: "app-card", template: "" })\nexport class AppCard {}',
	)
	const entry = registry.getByTag('app-card')
	assert.ok(entry, 'the .ts component is indexed by its real selector tag')
	assert.equal(entry.className, 'AppCard', 'class name recorded')
	assert.equal(entry.origin, 'angular', 'origin is angular')
})

// 11. Go-to-definition resolves a NON-ZERO target range (the class on a later
//     line of the real declaring file), not the hard-zeroed file top.
test('.treaty: definition lands on the class/symbol range, not {0,0}', () => {
	const dir = mkdtempSync(join(tmpdir(), 'treaty-lsp-def-'))
	// An Angular-style .ts component whose `class Panel` sits well below the top:
	// two import lines + the decorator precede it, so a correct range is non-zero.
	const file = join(dir, 'panel.component.ts')
	const text =
		"import { Component } from '@angular/core'\n" +
		"import { signal } from '@angular/core'\n" +
		'\n' +
		"@Component({ selector: 'app-panel', template: '' })\n" +
		'export class Panel {}\n'
	writeFileSync(file, text)

	const registry = new ComponentRegistry()
	registry.setProjectSelectors({ Panel: 'app-panel' })
	registry.indexFile(file, text)
	const inst = createTemplateService(registry).create(fakeContext())

	const parent = 'const a = 1\n<app-panel></app-panel>'
	const doc = docOf('file:///proj/src/Parent.treaty', 'treaty', parent)
	const offset = parent.indexOf('app-panel') + 2
	const defs = inst.provideDefinition(doc, doc.positionAt(offset), noToken)
	assert.ok(Array.isArray(defs) && defs.length === 1, 'one definition link is returned')
	const link = defs[0]
	assert.ok(/panel\.component\.ts$/.test(link.targetUri), 'targets the declaring .ts file')
	const sel = link.targetSelectionRange
	// `export class Panel` is on line 4 (0-based), char 13 → a real, non-zero range.
	assert.ok(
		sel.start.line > 0 || sel.start.character > 0,
		`selection range must be non-zero, got ${JSON.stringify(sel)}`,
	)
	assert.equal(sel.start.line, 4, 'selection lands on the `class Panel` line')
	assert.ok(sel.start.character > 0, 'selection skips the `export class ` prefix')
	assert.equal(link.targetRange.start.line, 4, 'target range also lands on the class line')
})

// 12. JSX gets `use:` directive completions (the selectorless directive set),
//     same as .treaty — proving the JSX completion-context detects use: too.
test('.tsx: use: offers selectorless directives in JSX', () => {
	const { inst } = instance()
	const text = 'export default function App() {\n  return <div use:h></div>\n}'
	const doc = docOf('file:///proj/src/App.tsx', 'treaty-jsx', text)
	const offset = text.indexOf('use:h') + 'use:h'.length
	const list = inst.provideCompletionItems(doc, doc.positionAt(offset), noCtx, noToken)
	assert.ok(list, 'a list is returned in JSX use: position')
	const dir = list.items.find((i) => i.label === 'highlight')
	assert.ok(dir, 'the highlight directive is offered under use: in JSX')
	assert.ok(dir.detail.includes('Highlight'), 'detail names the Highlight class')
})

// 13. JSX gets @-control-flow snippet completions, same as .treaty.
test('.tsx: @ offers control-flow block snippets in JSX', () => {
	const { inst } = instance()
	const text = 'export default function App() {\n  return <div>@i</div>\n}'
	const doc = docOf('file:///proj/src/App.tsx', 'treaty-jsx', text)
	const offset = text.indexOf('@i') + 2
	const list = inst.provideCompletionItems(doc, doc.positionAt(offset), noCtx, noToken)
	assert.ok(list, 'a list is returned for an @-head in JSX')
	const ifItem = list.items.find((i) => i.label === '@if')
	assert.ok(ifItem, '@if is offered in JSX')
	assert.equal(ifItem.insertTextFormat, 2, '@if is a snippet')
})

// 14. A template diagnostic anchors on a NON-ZERO range when the embedded TS
//     region starts below the document top (the rich, anchored path — not the
//     old fixed whole-document range at offset 0).
test('.treaty: diagnostics anchor on the embedded-TS region, not {0,0}', () => {
	const { registry } = instance()
	// Place the broken interpolation after a leading HTML region so the embedded
	// TS-by-default region (the `const a` line) begins on a non-zero line.
	const text = '<div>{{ }}</div>\nconst a = 1\n'
	const uri = 'file:///proj/src/BrokenAnchored.treaty'
	const doc = docOf(uri, 'treaty', text)

	// A context whose script-store yields a root virtual code whose embedded TS
	// code maps from a non-zero source offset (the start of `const a`), so the
	// anchored fallback lands there rather than at the document top.
	const tsStart = text.indexOf('const a')
	const rootVirtualCode = {
		id: 'root',
		languageId: 'treaty',
		embeddedCodes: [
			{
				id: 'ts',
				languageId: 'typescript',
				mappings: [
					{ sourceOffsets: [tsStart], generatedOffsets: [0], lengths: [11], data: {} },
				],
				embeddedCodes: [],
			},
		],
	}
	const ctx = {
		env: { workspaceFolders: [] },
		language: { scripts: { get: (id) => (String(id) === uri ? { generated: { root: rootVirtualCode } } : undefined) } },
	}
	const inst = createTemplateService(registry).create(ctx)
	const diags = inst.provideDiagnostics(doc, noToken)
	assert.ok(Array.isArray(diags) && diags.length >= 1, 'at least one diagnostic is returned')
	const r = diags[0].range
	assert.ok(
		r.start.line > 0 || r.start.character > 0,
		`diagnostic range must be non-zero, got ${JSON.stringify(r)}`,
	)
	// The embedded TS region begins on line 1 (the `const a` line).
	assert.equal(r.start.line, 1, 'diagnostic anchors on the embedded-TS region line')
})

for (const line of results) console.log(line)
if (failures > 0) {
	console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSMOKE TEST PASSED')
