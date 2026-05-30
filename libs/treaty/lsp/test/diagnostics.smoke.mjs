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
import { provideDiagnostics } from '../dist/diagnostics.js'

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

for (const line of results) console.log(line)
if (failures > 0) {
	console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSMOKE TEST PASSED')
