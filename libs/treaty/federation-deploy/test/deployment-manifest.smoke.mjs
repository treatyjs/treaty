/**
 * Node smoke test for the @treaty/federation-deploy D2 deployment-manifest layer.
 *
 * Exercises the versioned per-remote ledger against the built dist:
 *   1. recordDeployment adds a remote with single-entry history,
 *   2. a second deploy appends to history and advances currentVersion,
 *   3. recordDeployment changes ONLY the targeted remote (others untouched),
 *   4. rollbackTo flips currentVersion+entry to a prior history version, preserving history,
 *   5. rollbackTo rejects a version never deployed (not in history),
 *   6. rollbackTo to the current version is a no-op,
 *   7. serialize/parse round-trips to a deep-equal manifest (sorted, stable),
 *   8. MemoryDeploymentStore load/save round-trips through serialize/parse,
 *   9. the deployment runtime plugin resolves remotes' current version+entry from a manifest,
 *  10. the runtime plugin loads lazily + once from an injected store.
 *
 * Run: node libs/treaty/federation-deploy/test/deployment-manifest.smoke.mjs
 */

import assert from 'node:assert/strict'
import {
	createDeploymentManifest,
	getRemote,
	hasRemote,
	recordDeployment,
	rollbackTo,
	serializeDeploymentManifest,
	parseDeploymentManifest,
	MemoryDeploymentStore,
	createTreatyDeploymentRuntimePlugin,
	DEPLOYMENT_MANIFEST_SCHEMA,
} from '../dist/index.js'

let failures = 0
const results = []

async function check(label, fn) {
	try {
		await fn()
		results.push(`PASS ${label}`)
	} catch (err) {
		failures++
		results.push(`FAIL ${label}: ${err.stack ?? err.message}`)
	}
}

// 1. first deploy of a remote -> single-entry history
await check('recordDeployment seeds a new remote with single-entry history', () => {
	const m0 = createDeploymentManifest({ app: 'shell' })
	const m1 = recordDeployment(m0, 'routes/dashboard', '1.0.0', 'https://cdn/routes/dashboard/1.0.0/remoteEntry.js', { kind: 'route' })
	assert.equal(m1.schema, DEPLOYMENT_MANIFEST_SCHEMA, 'schema stamped')
	const r = getRemote(m1, 'routes/dashboard')
	assert.equal(r.currentVersion, '1.0.0', 'current version set')
	assert.equal(r.entry, 'https://cdn/routes/dashboard/1.0.0/remoteEntry.js', 'entry url set')
	assert.deepEqual([...r.history], ['1.0.0'], 'history seeded with the one version')
	assert.equal(r.kind, 'route', 'kind recorded')
	assert.ok(hasRemote(m1, 'routes/dashboard'))
	// input not mutated
	assert.equal(hasRemote(m0, 'routes/dashboard'), false, 'input manifest untouched')
})

// 2. second deploy appends history + advances current
await check('recordDeployment appends to history and advances currentVersion', () => {
	let m = createDeploymentManifest()
	m = recordDeployment(m, 'routes/dashboard', '1.0.0', 'https://cdn/routes/dashboard/1.0.0/remoteEntry.js')
	m = recordDeployment(m, 'routes/dashboard', '2.0.0', 'https://cdn/routes/dashboard/2.0.0/remoteEntry.js')
	const r = getRemote(m, 'routes/dashboard')
	assert.equal(r.currentVersion, '2.0.0', 'advanced to v2')
	assert.deepEqual([...r.history], ['1.0.0', '2.0.0'], 'history grew, oldest first, current last')
	// idempotent re-record of current does not grow history
	const m2 = recordDeployment(m, 'routes/dashboard', '2.0.0', 'https://cdn/routes/dashboard/2.0.0/remoteEntry.js')
	assert.deepEqual([...getRemote(m2, 'routes/dashboard').history], ['1.0.0', '2.0.0'], 're-deploy of current is idempotent in history')
})

// 3. deploy of remote A leaves remote B untouched (deployment granularity)
await check('recordDeployment changes ONLY the targeted remote', () => {
	let m = createDeploymentManifest()
	m = recordDeployment(m, 'A', '1.0.0', 'https://cdn/A/1.0.0/remoteEntry.js')
	m = recordDeployment(m, 'B', '1.0.0', 'https://cdn/B/1.0.0/remoteEntry.js')
	const before = getRemote(m, 'B')
	const next = recordDeployment(m, 'A', '2.0.0', 'https://cdn/A/2.0.0/remoteEntry.js')
	assert.equal(getRemote(next, 'A').currentVersion, '2.0.0', 'A advanced')
	assert.deepEqual(getRemote(next, 'B'), before, 'B byte-for-byte untouched')
})

// 4. rollback flips to a prior history version, preserving full history
await check('rollbackTo flips currentVersion+entry to a prior version, preserves history', () => {
	let m = createDeploymentManifest()
	m = recordDeployment(m, 'A', '1.0.0', 'https://cdn/A/1.0.0/remoteEntry.js')
	m = recordDeployment(m, 'A', '2.0.0', 'https://cdn/A/2.0.0/remoteEntry.js')
	const rolled = rollbackTo(m, 'A', '1.0.0')
	const r = getRemote(rolled, 'A')
	assert.equal(r.currentVersion, '1.0.0', 'reverted to v1')
	assert.equal(r.entry, 'https://cdn/A/1.0.0/remoteEntry.js', 'entry url swapped to v1 (version-segment derivation)')
	assert.deepEqual([...r.history], ['1.0.0', '2.0.0'], 'history preserved in full (rollback is reversible)')
	// can roll "forward" again because v2 is still in history
	const forward = rollbackTo(rolled, 'A', '2.0.0')
	assert.equal(getRemote(forward, 'A').currentVersion, '2.0.0', 'rollback is reversible via history')
})

// 5. rollback to a never-deployed version is rejected
await check('rollbackTo rejects a version not in history', () => {
	let m = createDeploymentManifest()
	m = recordDeployment(m, 'A', '1.0.0', 'https://cdn/A/1.0.0/remoteEntry.js')
	assert.throws(() => rollbackTo(m, 'A', '9.9.9'), /was never deployed/, 'cannot roll back to an unpublished version')
	assert.throws(() => rollbackTo(m, 'nope', '1.0.0'), /unknown remote/, 'cannot roll back unknown remote')
})

// 6. rollback to current is a no-op
await check('rollbackTo to the current version is a no-op', () => {
	let m = createDeploymentManifest()
	m = recordDeployment(m, 'A', '1.0.0', 'https://cdn/A/1.0.0/remoteEntry.js')
	assert.equal(rollbackTo(m, 'A', '1.0.0'), m, 'returns the same manifest unchanged')
})

// 7. serialize/parse round-trip, sorted + stable
await check('serialize/parse round-trips to a deep-equal manifest (sorted, stable)', () => {
	let m = createDeploymentManifest({ app: 'shell' })
	m = recordDeployment(m, 'routes/reports', '0.9.0', 'https://cdn/routes/reports/0.9.0/remoteEntry.js', { kind: 'route' })
	m = recordDeployment(m, 'libs/data-access', '1.4.0', 'https://cdn/libs/data-access/1.4.0/remoteEntry.js', { kind: 'lib' })
	const json = serializeDeploymentManifest(m)
	const back = parseDeploymentManifest(json)
	assert.deepEqual(back, m, 'round-trips deep-equal')
	const keys = Object.keys(JSON.parse(json).remotes)
	assert.deepEqual(keys, [...keys].sort(), 'remote keys serialized sorted')
	assert.equal(serializeDeploymentManifest(back), json, 'serialization is stable')
	// parse rejects a currentVersion not in history
	assert.throws(
		() => parseDeploymentManifest(JSON.stringify({ schema: 1, remotes: { A: { name: 'A', currentVersion: 'x', entry: 'u', history: ['y'], kind: 'route' } } })),
		/not in its history/
	)
})

// 8. MemoryDeploymentStore round-trips via serialize/parse
await check('MemoryDeploymentStore load/save round-trips through serialize/parse', async () => {
	const store = new MemoryDeploymentStore()
	assert.equal(await store.load(), undefined, 'empty store loads undefined (first deploy)')
	let m = createDeploymentManifest({ app: 'shell' })
	m = recordDeployment(m, 'A', '1.0.0', 'https://cdn/A/1.0.0/remoteEntry.js')
	await store.save(m)
	const loaded = await store.load()
	assert.deepEqual(loaded, m, 'persisted manifest round-trips deep-equal')
	assert.ok(typeof store.raw === 'string' && store.raw.includes('"A"'), 'raw JSON exposes the persisted ledger')
})

// 9. runtime plugin resolves remotes' current version+entry from a manifest
await check('deployment runtime plugin resolves remotes from a manifest object', async () => {
	let m = createDeploymentManifest()
	m = recordDeployment(m, 'routes/dashboard', '1.0.0', 'https://cdn/routes/dashboard/1.0.0/remoteEntry.js')
	m = recordDeployment(m, 'routes/dashboard', '2.0.0', 'https://cdn/routes/dashboard/2.0.0/remoteEntry.js')
	const plugin = createTreatyDeploymentRuntimePlugin(m)()
	assert.equal(plugin.name, 'treaty-deployment', 'default plugin name')
	const out = await plugin.beforeRequest({
		id: 'routes/dashboard/Widget',
		options: { remotes: [{ name: 'routes/dashboard', entry: 'https://STALE/remoteEntry.js' }] },
	})
	const r = out.options.remotes[0]
	assert.equal(r.entry, 'https://cdn/routes/dashboard/2.0.0/remoteEntry.js', 'entry repointed to current deployed url')
	assert.equal(r.version, '2.0.0', 'version stamped from ledger currentVersion')
	// unknown remote passes through by default; throws in strict mode
	const passed = await plugin.resolveRemote({ remote: { name: 'mystery', entry: 'https://keep' } })
	assert.equal(passed.entry, 'https://keep', 'unknown remote passes through by default')
	const strict = createTreatyDeploymentRuntimePlugin(m, { passthroughUnknown: false })()
	await assert.rejects(() => Promise.resolve(strict.resolveRemote({ remote: { name: 'mystery', entry: 'x' } })), /no deployment manifest entry/)
})

// 10. runtime plugin lazily loads (once) from an injected store
await check('deployment runtime plugin loads lazily + once from an injected store', async () => {
	let m = createDeploymentManifest()
	m = recordDeployment(m, 'A', '3.0.0', 'https://cdn/A/3.0.0/remoteEntry.js')
	let loads = 0
	const store = {
		load: () => { loads++; return m },
		save: () => {},
	}
	const plugin = createTreatyDeploymentRuntimePlugin(store)()
	assert.equal(loads, 0, 'no load until first resolution (lazy)')
	const out = await plugin.beforeRequest({ id: 'x', options: { remotes: [{ name: 'A', entry: 'https://STALE' }] } })
	assert.equal(out.options.remotes[0].entry, 'https://cdn/A/3.0.0/remoteEntry.js', 'resolved from the store')
	await plugin.resolveRemote({ remote: { name: 'A', entry: 'https://STALE' } })
	assert.equal(loads, 1, 'store loaded once and cached')
})

for (const line of results) console.log(line)
if (failures > 0) {
	console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSMOKE TEST PASSED')
