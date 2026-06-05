/**
 * Node smoke test for the @treaty/lsp auto-provide of @treaty/jsx types.
 *
 * Asserts that the language-plugin / TypeScript-project configuration the
 * server applies to every project (`applyTreatyJsxAutoTypes`, built on the pure
 * `treatyJsxCompilerOptions`) makes a Treaty `.tsx` document resolve the shipped
 * `@treaty/jsx` ambient JSX types with no per-project tsconfig opt-in:
 *
 *  - the project compilerOptions route the automatic JSX runtime through
 *    `@treaty/jsx` (`jsxImportSource`), lift `jsx` to the automatic runtime when
 *    unset, and keep `@treaty/jsx` in a pinned `types` list, and
 *  - the resolved ambient `.d.ts` is added as an extra project root file so the
 *    global `JSX` namespace loads across the whole project.
 *
 * Run: node libs/treaty/lsp/test/jsx-types.smoke.mjs
 */

import assert from 'node:assert/strict'
import { existsSync } from 'node:fs'
import {
	TREATY_JSX_IMPORT_SOURCE,
	applyTreatyJsxAutoTypes,
	resolveTreatyJsxTypesEntry,
	treatyJsxCompilerOptions,
} from '../dist/jsx-types.js'

// `ts.JsxEmit.ReactJSX` — the automatic runtime mode `jsxImportSource` requires.
const REACT_JSX = 4

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

// The bare specifier is the shipped authoring-plugin package.
test('import source is @treaty/jsx', () => {
	assert.equal(TREATY_JSX_IMPORT_SOURCE, '@treaty/jsx')
})

// treatyJsxCompilerOptions: routes the automatic JSX runtime through @treaty/jsx
// and lifts `jsx` when the project left it unset.
test('compilerOptions: jsxImportSource + jsx default for a .tsx project', () => {
	const base = { target: 99, module: 99 }
	const merged = treatyJsxCompilerOptions(base)
	assert.equal(
		merged.jsxImportSource,
		'@treaty/jsx',
		'jsxImportSource must be @treaty/jsx',
	)
	assert.equal(merged.jsx, REACT_JSX, 'jsx must be lifted to the automatic runtime (ReactJSX)')
	// Pure: input is not mutated.
	assert.equal(base.jsxImportSource, undefined, 'input options must not be mutated')
	assert.equal(base.jsx, undefined, 'input jsx must not be mutated')
})

// A project that already pinned a `types` list keeps its entries and gains
// @treaty/jsx so the ambient declarations are auto-included.
test('compilerOptions: appends @treaty/jsx to a pinned types list', () => {
	const merged = treatyJsxCompilerOptions({ types: ['node'] })
	assert.deepEqual(
		merged.types,
		['node', '@treaty/jsx'],
		'@treaty/jsx must be appended to the pinned types list',
	)
})

// Already-augmented options are stable (idempotent), and an existing `jsx`
// choice is respected.
test('compilerOptions: idempotent and respects an explicit jsx mode', () => {
	const once = treatyJsxCompilerOptions({ jsx: 1, types: ['@treaty/jsx'] })
	assert.equal(once.jsx, 1, 'an explicit jsx mode must be preserved')
	assert.deepEqual(once.types, ['@treaty/jsx'], 'types must not duplicate @treaty/jsx')
	const twice = treatyJsxCompilerOptions(once)
	assert.deepEqual(twice.types, ['@treaty/jsx'], 'second pass must not duplicate @treaty/jsx')
	assert.equal(twice.jsxImportSource, '@treaty/jsx')
})

// The end-to-end wiring the server applies to every project host: a .tsx
// document's project resolves the @treaty/jsx auto-type with no opt-in.
test('applyTreatyJsxAutoTypes: a .tsx project resolves @treaty/jsx types', () => {
	const typesEntry = resolveTreatyJsxTypesEntry()
	assert.ok(
		typesEntry && existsSync(typesEntry),
		`expected to resolve the shipped @treaty/jsx ambient .d.ts, got ${typesEntry}`,
	)

	// A minimal stand-in for the volarjs TypeScript project host, holding a
	// single open `.tsx` document and a baseline (un-opted-in) compilerOptions.
	const tsxDocument = '/workspace/app/Counter.tsx'
	const host = {
		_options: { target: 99, module: 99 },
		_files: [tsxDocument],
		getCompilationSettings() {
			return this._options
		},
		getScriptFileNames() {
			return this._files
		},
	}

	applyTreatyJsxAutoTypes(host, typesEntry)

	const settings = host.getCompilationSettings()
	assert.equal(
		settings.jsxImportSource,
		'@treaty/jsx',
		'project compilerOptions must set jsxImportSource to @treaty/jsx for the .tsx document',
	)
	assert.equal(settings.jsx, REACT_JSX, 'project must use the automatic JSX runtime')

	const fileNames = host.getScriptFileNames()
	assert.ok(
		fileNames.includes(typesEntry),
		'the @treaty/jsx ambient .d.ts must be an extra project root file',
	)
	assert.ok(
		fileNames.includes(tsxDocument),
		'the original .tsx document must still be a project root file',
	)

	// Idempotent: re-applying does not duplicate the root file or change options.
	applyTreatyJsxAutoTypes(host, typesEntry)
	const after = host.getScriptFileNames().filter((f) => f === typesEntry)
	assert.equal(after.length, 1, 'the ambient .d.ts must not be added twice')
	assert.equal(host.getCompilationSettings().jsxImportSource, '@treaty/jsx')
})

// When the ambient .d.ts cannot be resolved, the compiler-option auto-type is
// still applied (bare specifier resolves from the open project), and the
// root-file list is left untouched rather than failing.
test('applyTreatyJsxAutoTypes: degrades gracefully without a resolved .d.ts', () => {
	const host = {
		_options: {},
		_files: ['/workspace/app/Counter.tsx'],
		getCompilationSettings() {
			return this._options
		},
		getScriptFileNames() {
			return this._files
		},
	}
	applyTreatyJsxAutoTypes(host, undefined)
	assert.equal(host.getCompilationSettings().jsxImportSource, '@treaty/jsx')
	assert.deepEqual(
		host.getScriptFileNames(),
		['/workspace/app/Counter.tsx'],
		'root-file list must be untouched when no ambient .d.ts is resolved',
	)
})

for (const line of results) console.log(line)
if (failures > 0) {
	console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSMOKE TEST PASSED')
