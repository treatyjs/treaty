/**
 * Node smoke test for the Treaty oxlint plugin (`treaty/no-unused-vars`).
 *
 * Proves the load-bearing DX promise: in a Treaty authoring file an import used
 * ONLY via a `use:<name>` directive application, or via a lowercase selectorless
 * component tag, is NOT flagged as an unused import — so authors never need a
 * `void X` statement to keep a real, used import. It also proves the rule has
 * not gone blind: a genuinely-unused import IS still reported, and the rule
 * stays at parity with the core `no-unused-vars` on a control fixture.
 *
 * The test runs the real `oxlint` binary (the same one the repo lints with) over
 * temporary fixtures, once with the core rule and once with the Treaty rule, and
 * compares the reported identifiers. Nothing is mocked: this is the actual lint
 * the editor / CI runs.
 *
 * Run: node libs/treaty/lsp/test/oxlint-plugin.smoke.mjs
 */

import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const require = createRequire(import.meta.url)
const here = dirname(fileURLToPath(import.meta.url))

// Resolve the oxlint binary (via the package's exported package.json) and the
// plugin source from the workspace.
const oxlintPackageJson = require.resolve('oxlint/package.json')
const oxlintBin = join(dirname(oxlintPackageJson), require('oxlint/package.json').bin.oxlint)
const pluginPath = join(here, '..', 'src', 'oxlint-plugin.ts')

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

/**
 * Run oxlint over a single `.tsx` source with a given rule configuration and
 * return the set of identifiers the run reported as unused. The config is
 * written next to the fixture so oxlint resolves the plugin path the same way a
 * project would.
 */
function lintUnusedNames(source, { treaty }) {
	const dir = mkdtempSync(join(tmpdir(), 'treaty-oxlint-'))
	try {
		const file = join(dir, 'fixture.tsx')
		writeFileSync(file, source, 'utf8')

		const config = treaty
			? {
					plugins: ['typescript'],
					jsPlugins: [pluginPath],
					rules: { 'no-unused-vars': 'off', 'treaty/no-unused-vars': 'warn' },
				}
			: {
					plugins: ['typescript'],
					rules: { 'no-unused-vars': 'warn' },
				}
		const configPath = join(dir, '.oxlintrc.json')
		writeFileSync(configPath, JSON.stringify(config), 'utf8')

		const run = spawnSync(process.execPath, [oxlintBin, '-c', configPath, file], {
			encoding: 'utf8',
		})
		const output = `${run.stdout ?? ''}${run.stderr ?? ''}`
		assert.ok(
			!/Failed to (parse|load)/.test(output),
			`oxlint failed to run/parse plugin:\n${output}`,
		)

		// Pull the quoted identifier out of each "… 'Name' is … but never used" line.
		const names = new Set()
		const pattern =
			/(?:Identifier|Variable|Parameter|Function|Class|Enum|Interface|Type alias|Type) '([A-Za-z_$][A-Za-z0-9_$]*)' is/g
		for (const match of output.matchAll(pattern)) {
			names.add(match[1])
		}
		return names
	} finally {
		rmSync(dir, { recursive: true, force: true })
	}
}

// A fixture whose ONLY use of an imported directive is a `use:<name>`
// application, with no `void X` to silence the import.
const USE_DIRECTIVE_FIXTURE = `import { signal } from '@angular/core'
import { Highlight } from './highlight.directive'

export default function counter() {
	const count = signal(0)
	return (
		<section class="counter" use:highlight>
			count is {count()}
		</section>
	)
}
`

// A fixture whose ONLY use of an imported component is a lowercase selectorless
// tag (`<panel />`), again with no `void X`.
const SELECTORLESS_TAG_FIXTURE = `import { panel } from './panel'

export default function shell() {
	return <panel />
}
`

// A control fixture mixing a genuinely-unused import with real uses, used to
// prove the Treaty rule still reports real problems and matches the core rule.
const CONTROL_FIXTURE = `import { Used } from './used'
import { Unused } from './unused'

export default function view() {
	const local = 1
	const dead = 2
	return <div>{local}<Used /></div>
}
`

// THE PROOF: `use:highlight` keeps `Highlight` alive under the Treaty rule.
test('use:<name> import is not flagged unused (no void X needed)', () => {
	const core = lintUnusedNames(USE_DIRECTIVE_FIXTURE, { treaty: false })
	const treaty = lintUnusedNames(USE_DIRECTIVE_FIXTURE, { treaty: true })
	assert.ok(
		core.has('Highlight'),
		'precondition: the core rule false-positives on the use:<name>-only import',
	)
	assert.ok(
		!treaty.has('Highlight'),
		'the Treaty rule must NOT flag an import used via use:<name>',
	)
	assert.equal(treaty.size, 0, `the Treaty rule must report nothing here, got: ${[...treaty]}`)
})

// A lowercase selectorless component tag keeps its import alive.
test('selectorless lowercase tag import is not flagged unused', () => {
	const core = lintUnusedNames(SELECTORLESS_TAG_FIXTURE, { treaty: false })
	const treaty = lintUnusedNames(SELECTORLESS_TAG_FIXTURE, { treaty: true })
	assert.ok(
		core.has('panel'),
		'precondition: the core rule false-positives on the lowercase selectorless tag import',
	)
	assert.ok(
		!treaty.has('panel'),
		'the Treaty rule must NOT flag an import used via a selectorless lowercase tag',
	)
})

// The rule still reports real unused bindings, at parity with the core rule.
test('genuinely-unused bindings are still reported (parity with core)', () => {
	const core = lintUnusedNames(CONTROL_FIXTURE, { treaty: false })
	const treaty = lintUnusedNames(CONTROL_FIXTURE, { treaty: true })
	assert.deepEqual(
		[...treaty].sort(),
		[...core].sort(),
		'the Treaty rule must match the core rule when no use:/selectorless form is involved',
	)
	assert.ok(treaty.has('Unused'), 'a genuinely-unused import must still be reported')
	assert.ok(treaty.has('dead'), 'a genuinely-unused local must still be reported')
	assert.ok(!treaty.has('Used'), 'a used import must not be reported')
	assert.ok(!treaty.has('local'), 'a used local must not be reported')
})

for (const line of results) console.log(line)
if (failures > 0) {
	console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSMOKE TEST PASSED')
