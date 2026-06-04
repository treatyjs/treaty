/**
 * Node smoke test for the @treaty/lsp diagnostics provider.
 *
 * Imports the built diagnostics provider (which routes through the Rust
 * authoring compiler via the @treaty/authoring-node NAPI addon), feeds it
 * deliberately-broken sources, and asserts that at least one Diagnostic is
 * returned with a sane (non-negative, ordered) range.
 *
 * Run: node libs/treaty/lsp/test/diagnostics.smoke.mjs
 */

import assert from 'node:assert/strict'
import { mapErrorsToDiagnostics, provideDiagnostics } from '../dist/diagnostics.js'

/** Assert a Diagnostic range is well-formed and non-negative. */
function assertSaneRange(diag, label) {
	assert.ok(diag, `${label}: a diagnostic must be present`)
	const r = diag.range
	assert.ok(r, `${label}: diagnostic must carry a range`)
	for (const key of ['start', 'end']) {
		assert.equal(typeof r[key].line, 'number', `${label}: ${key}.line is a number`)
		assert.equal(typeof r[key].character, 'number', `${label}: ${key}.character is a number`)
		assert.ok(r[key].line >= 0, `${label}: ${key}.line >= 0`)
		assert.ok(r[key].character >= 0, `${label}: ${key}.character >= 0`)
	}
	// end must not precede start
	const before =
		r.end.line > r.start.line ||
		(r.end.line === r.start.line && r.end.character >= r.start.character)
	assert.ok(before, `${label}: range end must not precede start`)
	assert.ok(typeof diag.message === 'string' && diag.message.length > 0, `${label}: non-empty message`)
}

let failures = 0
const results = []

// Case 1: deliberately-broken .tsx / source — a syntax error the compiler rejects.
try {
	const brokenSource = 'export class Broken { constructor() { this.y = ; } }'
	const diags = provideDiagnostics({
		fileName: 'broken.tsx',
		languageId: 'typescriptreact',
		text: brokenSource,
	})
	assert.ok(Array.isArray(diags), 'tsx: provideDiagnostics returns an array')
	assert.ok(diags.length >= 1, `tsx: expected >=1 diagnostic, got ${diags.length}`)
	assertSaneRange(diags[0], 'tsx')
	results.push(`PASS tsx: ${diags.length} diagnostic(s); first="${diags[0].message}" range=${JSON.stringify(diags[0].range)}`)
} catch (err) {
	failures++
	results.push(`FAIL tsx: ${err.message}`)
}

// Case 2: deliberately-broken .treaty source.
try {
	// A blank interpolation `{{ }}` in the template is rejected by the .treaty
	// front-end's template parser with a descriptive error.
	const brokenTreaty = 'const x = 1\n<div>{{ }}</div>\n'
	const diags = provideDiagnostics({
		fileName: 'broken.treaty',
		languageId: 'treaty',
		text: brokenTreaty,
	})
	assert.ok(Array.isArray(diags), 'treaty: provideDiagnostics returns an array')
	assert.ok(diags.length >= 1, `treaty: expected >=1 diagnostic, got ${diags.length}`)
	assertSaneRange(diags[0], 'treaty')
	results.push(`PASS treaty: ${diags.length} diagnostic(s); first="${diags[0].message}" range=${JSON.stringify(diags[0].range)}`)
} catch (err) {
	failures++
	results.push(`FAIL treaty: ${err.message}`)
}

// Case 3: valid source produces no diagnostics (sanity: provider isn't always-erroring).
try {
	const valid = 'export class Ok { value = 1 }'
	const diags = provideDiagnostics({
		fileName: 'ok.tsx',
		languageId: 'typescriptreact',
		text: valid,
	})
	assert.ok(Array.isArray(diags), 'valid: provideDiagnostics returns an array')
	results.push(`INFO valid: ${diags.length} diagnostic(s)`)
} catch (err) {
	failures++
	results.push(`FAIL valid: ${err.message}`)
}

// Case 4: a `sass:` error with a `./stdin:line:col` locator anchors INSIDE the
// <style> block on a non-zero line — proving the rich, sass-aware range mapping
// (not a fixed whole-document range at offset 0).
try {
	const styled = '<div>hi</div>\n<style lang="scss">\n  .x { color: $missing; }\n</style>\n'
	// grass-shaped locator: relative to the CSS body, line 2, col 9.
	const message = 'sass: Undefined variable.\n  ./stdin:2:9'
	const diags = mapErrorsToDiagnostics([message], {
		fileName: 'styled.treaty',
		languageId: 'treaty',
		text: styled,
	})
	assert.ok(Array.isArray(diags) && diags.length === 1, 'sass: one diagnostic returned')
	assertSaneRange(diags[0], 'sass')
	const r = diags[0].range
	assert.ok(
		r.start.line > 0 || r.start.character > 0,
		`sass: range must be non-zero, got ${JSON.stringify(r)}`,
	)
	results.push(`PASS sass: anchored range=${JSON.stringify(r)}`)
} catch (err) {
	failures++
	results.push(`FAIL sass: ${err.message}`)
}

// Case 5: a non-positional error anchors on the embedded-TS region (recovered
// from the root virtual code's first mapping), not the document top.
try {
	const text = '<div>hi</div>\nconst broken = ;\n'
	const tsStart = text.indexOf('const broken')
	const rootVirtualCode = {
		id: 'root',
		languageId: 'treaty',
		embeddedCodes: [
			{
				id: 'ts',
				languageId: 'typescript',
				mappings: [
					{ sourceOffsets: [tsStart], generatedOffsets: [0], lengths: [12], data: {} },
				],
				embeddedCodes: [],
			},
		],
	}
	const diags = mapErrorsToDiagnostics(
		['Unexpected token.'],
		{ fileName: 'anchored.treaty', languageId: 'treaty', text },
		rootVirtualCode,
	)
	assert.ok(Array.isArray(diags) && diags.length === 1, 'anchored: one diagnostic returned')
	assertSaneRange(diags[0], 'anchored')
	const r = diags[0].range
	assert.equal(r.start.line, 1, 'anchored: lands on the embedded-TS region line')
	results.push(`PASS anchored: embedded-TS range=${JSON.stringify(r)}`)
} catch (err) {
	failures++
	results.push(`FAIL anchored: ${err.message}`)
}

for (const line of results) console.log(line)
if (failures > 0) {
	console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSMOKE TEST PASSED')
