/**
 * Smoke test for @treaty/build.
 *
 * Verifies:
 *   1. builders.json registers both builders and each resolves to a loadable
 *      architect Builder implementation (a function-like with the architect
 *      builder symbol) and an on-disk schema.
 *   2. The application build schema validates a minimal config and applies the
 *      documented defaults via the architect CoreSchemaRegistry.
 *
 * Does NOT run a full ng workspace build (no @rspack/core peer installed).
 */

import { readFile } from 'node:fs/promises'
import { fileURLToPath, pathToFileURL } from 'node:url'
import path from 'node:path'
import assert from 'node:assert/strict'
import { schema } from '@angular-devkit/core'

const here = path.dirname(fileURLToPath(import.meta.url))
const pkgRoot = path.resolve(here, '..')

function fail(msg) {
	console.error(`SMOKE FAIL: ${msg}`)
	process.exit(1)
}

// --- 1. builders.json resolves both builders ---------------------------------
const buildersJson = JSON.parse(await readFile(path.join(pkgRoot, 'builders.json'), 'utf8'))
const builders = buildersJson.builders ?? {}

const expected = ['application', 'dev-server']
for (const name of expected) {
	if (!builders[name]) fail(`builders.json is missing the "${name}" builder`)
}

const architectBuilderSymbol = Symbol.for('@angular-devkit/architect:builder')

for (const name of expected) {
	const entry = builders[name]
	const implPath = path.resolve(pkgRoot, entry.implementation)
	const schemaPath = path.resolve(pkgRoot, entry.schema)

	// Implementation loads and is an architect Builder (createBuilder tags the
	// default export with the architect builder symbol).
	const mod = await import(pathToFileURL(implPath).href)
	const builder = mod.default
	if (!builder || typeof builder !== 'object') fail(`${name}: implementation has no default Builder export`)
	if (!builder[architectBuilderSymbol]) fail(`${name}: default export is not an architect Builder`)

	// Schema file exists and is valid JSON.
	const builderSchema = JSON.parse(await readFile(schemaPath, 'utf8'))
	if (builderSchema.type !== 'object') fail(`${name}: schema is not an object schema`)

	console.log(`ok: @treaty/build:${name} resolves (impl + schema)`)
}

// --- 2. application schema validates a minimal config + applies defaults -----
// Register the same default-applying transform architect uses, so the registry
// behaves as it would inside a real `ng build` run.
const registry = new schema.CoreSchemaRegistry()
registry.addPostTransform(schema.transforms.addUndefinedDefaults)
const appSchema = JSON.parse(
	await readFile(path.resolve(pkgRoot, builders.application.schema), 'utf8')
)

const minimalConfig = { entry: 'src/main.ts', outputPath: 'dist/app' }
const validator = await registry.compile(appSchema)
const result = await validator(minimalConfig)

if (!result.success) {
	fail(`application schema rejected a minimal config: ${JSON.stringify(result.errors)}`)
}
// Defaults applied by the registry.
assert.equal(result.data.optimization, false, 'optimization default should be false')
console.log('ok: application schema validates minimal config and applies defaults')

// --- 3. dev-server schema validates a minimal config -------------------------
const devSchema = JSON.parse(
	await readFile(path.resolve(pkgRoot, builders['dev-server'].schema), 'utf8')
)
const devValidator = await registry.compile(devSchema)
const devResult = await devValidator({ entry: 'src/main.ts', port: 4200 })
if (!devResult.success) {
	fail(`dev-server schema rejected a minimal config: ${JSON.stringify(devResult.errors)}`)
}
assert.equal(devResult.data.host, 'localhost', 'host default should be localhost')
console.log('ok: dev-server schema validates minimal config and applies defaults')

// --- 4. application schema rejects an invalid config -------------------------
// A config missing the required `entry`/`outputPath` must fail validation, so a
// misconfigured angular.json target surfaces a clear error rather than building.
const badResult = await validator({ port: 'not-a-number' })
if (badResult.success) {
	fail('application schema accepted a config missing required `entry`/`outputPath`')
}
console.log('ok: application schema rejects an invalid config')

console.log('\nSMOKE PASS: @treaty/build builders resolve and schemas validate')
