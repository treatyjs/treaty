/**
 * Smoke test for @treaty/build.
 *
 * Verifies:
 *   1. builders.json registers both builders and each resolves to a loadable
 *      architect Builder implementation (a function-like with the architect
 *      builder symbol) and an on-disk schema.
 *   2. The application build schema validates a minimal config and applies the
 *      documented defaults via the architect CoreSchemaRegistry.
 *   3. Both builders actually RUN over a tiny project through the real architect
 *      engine (Architect + TestingArchitectHost): architect validates the target
 *      options against the builder schema, invokes the builder, and the builder
 *      assembles its Treaty Rspack config and reaches the `@rspack/core` peer.
 *      That peer is intentionally not installed here, so the builder returns a
 *      handled `{ success: false }` BuilderOutput carrying the documented
 *      actionable error — proving the builder runs end-to-end through architect
 *      and fails cleanly (rather than crashing the run) when the peer is absent.
 *
 * Does NOT run a full Rspack compilation (that needs the @rspack/core peer).
 */

import { readFile } from 'node:fs/promises'
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { fileURLToPath, pathToFileURL } from 'node:url'
import path from 'node:path'
import assert from 'node:assert/strict'
import { schema } from '@angular-devkit/core'
import { Architect } from '@angular-devkit/architect'
import { TestingArchitectHost } from '@angular-devkit/architect/testing/index.js'

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

// --- 5. both builders RUN over a tiny project through the real architect engine ---
// Build a throwaway workspace with a single src/main.ts, register this package's
// builders with a TestingArchitectHost, and schedule each target exactly as
// `ng build` / `ng serve` would. Architect validates the options against the
// builder schema and invokes the builder; the builder assembles its Treaty Rspack
// config and reaches the (intentionally-absent) @rspack/core peer, so it must
// return a handled failing BuilderOutput with the documented actionable message
// instead of throwing. A throw here would crash the architect run — exactly the
// regression this case guards against.
const ws = mkdtempSync(path.join(tmpdir(), 'treaty-build-smoke-'))
try {
	mkdirSync(path.join(ws, 'src'), { recursive: true })
	writeFileSync(path.join(ws, 'src', 'main.ts'), 'export const main = 1\n')

	const runRegistry = new schema.CoreSchemaRegistry()
	runRegistry.addPostTransform(schema.transforms.addUndefinedDefaults)
	const architectHost = new TestingArchitectHost(ws, ws)
	const architect = new Architect(architectHost, runRegistry)
	await architectHost.addBuilderFromPackage(pkgRoot)

	const PEER_HINT = /@rspack\/(core|dev-server)/

	const appRun = await architect.scheduleBuilder('@treaty/build:application', {
		entry: 'src/main.ts',
		outputPath: 'dist/app',
	})
	const appOut = await appRun.result
	await appRun.stop()
	if (appOut.success) {
		fail('application builder unexpectedly succeeded without the @rspack/core peer')
	}
	if (!PEER_HINT.test(String(appOut.error))) {
		fail(`application builder error should name the missing peer, got: ${appOut.error}`)
	}
	console.log('ok: @treaty/build:application runs through architect and fails cleanly without @rspack/core')

	const serveRun = await architect.scheduleBuilder('@treaty/build:dev-server', {
		entry: 'src/main.ts',
		port: 4200,
	})
	const serveOut = await serveRun.result
	await serveRun.stop()
	if (serveOut.success) {
		fail('dev-server builder unexpectedly succeeded without the @rspack/core peer')
	}
	if (!PEER_HINT.test(String(serveOut.error))) {
		fail(`dev-server builder error should name the missing peer, got: ${serveOut.error}`)
	}
	console.log('ok: @treaty/build:dev-server runs through architect and fails cleanly without @rspack/core')
} finally {
	rmSync(ws, { recursive: true, force: true })
}

console.log('\nSMOKE PASS: @treaty/build builders resolve, schemas validate, and run through architect')
