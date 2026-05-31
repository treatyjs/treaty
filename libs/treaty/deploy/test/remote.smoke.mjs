/**
 * Node smoke test for the @treaty/deploy D2 per-remote deploy/rollback layer.
 *
 * Exercises partial deploy + rollback of an INDIVIDUAL remote against the built
 * dist, with ALL IO (upload target + manifest store) behind injected fakes:
 *   1. deployRemote uploads via the injected target + persists the ledger via the store,
 *   2. deploy remote A v1 then v2: ledger advances, history grows, B untouched,
 *   3. rollbackRemote(A, v1) flips A back WITHOUT uploading; B still untouched,
 *   4. rollbackRemote rejects a version never deployed for that remote,
 *   5. deployAffectedRemotes with affected=[A] deploys ONLY A (B not uploaded, not in ledger changes),
 *   6. deployAffectedRemotes ignores an artifact for a non-affected remote,
 *   7. deployRemote dryRun records the ledger but uploads nothing,
 *   8. injected-interface guards (missing store / target) reject.
 *
 * Run: node libs/treaty/deploy/test/remote.smoke.mjs
 */

import assert from 'node:assert/strict'
import {
	deployRemote,
	rollbackRemote,
	deployAffectedRemotes,
	getRemote,
	MemoryDeploymentStore,
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

/** A fake DeployTarget: records uploads in memory, serves under a base url. */
function makeFakeTarget(baseUrl = 'https://cdn') {
	const uploads = new Map()
	let begun = 0
	let finished = 0
	return {
		name: 'fake',
		begin() { begun++ },
		finish() { finished++ },
		upload(path, bytes) { uploads.set(path, bytes) },
		urlFor(path) { return `${baseUrl}/${path}` },
		// test introspection
		uploads,
		get begun() { return begun },
		get finished() { return finished },
	}
}

const bytes = (s) => new TextEncoder().encode(s)
const artifact = (remote, version, kind = 'route', body = `// ${remote}@${version}`) => ({
	remote,
	version,
	kind,
	entry: 'remoteEntry.js',
	files: { 'remoteEntry.js': bytes(body), 'chunks/widget.js': bytes('// widget') },
})

// 1. deployRemote uploads via target + persists via store
await check('deployRemote uploads via injected target and persists via injected store', async () => {
	const store = new MemoryDeploymentStore()
	const target = makeFakeTarget()
	const res = await deployRemote(target, artifact('routes/dashboard', '1.0.0'), { store, app: 'shell' })

	assert.equal(target.begun, 1, 'target.begin called')
	assert.equal(target.finished, 1, 'target.finish called')
	assert.deepEqual([...res.uploaded].sort(), [
		'routes/dashboard/1.0.0/chunks/widget.js',
		'routes/dashboard/1.0.0/remoteEntry.js',
	], 'files uploaded under versioned per-remote paths')
	assert.ok(target.uploads.has('routes/dashboard/1.0.0/remoteEntry.js'), 'entry uploaded to the target')
	assert.equal(res.deployment.entry, 'https://cdn/routes/dashboard/1.0.0/remoteEntry.js', 'served entry url resolved from target')
	assert.equal(res.deployment.currentVersion, '1.0.0')

	// persisted to the store
	const persisted = await store.load()
	assert.deepEqual(getRemote(persisted, 'routes/dashboard'), res.deployment, 'ledger persisted via the store')
})

// 2 + 3. THE CORE SMOKE: deploy A v2 then rollback to v1; B untouched throughout.
await check('deploy remote A v2 then rollback to v1: ledger reflects it, others untouched', async () => {
	const store = new MemoryDeploymentStore()
	const target = makeFakeTarget()

	// Seed two remotes at v1.
	await deployRemote(target, artifact('A', '1.0.0'), { store })
	await deployRemote(target, artifact('B', '1.0.0'), { store })
	const bAfterSeed = getRemote(await store.load(), 'B')

	// Deploy A v2 (partial: only A).
	await deployRemote(target, artifact('A', '2.0.0'), { store })
	let m = await store.load()
	assert.equal(getRemote(m, 'A').currentVersion, '2.0.0', 'A advanced to v2')
	assert.deepEqual([...getRemote(m, 'A').history], ['1.0.0', '2.0.0'], 'A history grew')
	assert.deepEqual(getRemote(m, 'B'), bAfterSeed, 'B untouched by A deploy')
	assert.ok(target.uploads.has('A/2.0.0/remoteEntry.js'), 'A v2 uploaded')

	// Rollback A to v1 — NO upload, flips the ledger, B still untouched.
	const uploadsBefore = target.uploads.size
	const rb = await rollbackRemote('A', '1.0.0', { store })
	assert.equal(target.uploads.size, uploadsBefore, 'rollback uploaded nothing (no rebuild)')
	assert.deepEqual([...rb.uploaded], [], 'rollback result reports no uploads')

	m = await store.load()
	assert.equal(getRemote(m, 'A').currentVersion, '1.0.0', 'ledger reflects rollback to v1')
	assert.equal(getRemote(m, 'A').entry, 'https://cdn/A/1.0.0/remoteEntry.js', 'served entry flipped to v1')
	assert.deepEqual([...getRemote(m, 'A').history], ['1.0.0', '2.0.0'], 'history preserved through rollback')
	assert.deepEqual(getRemote(m, 'B'), bAfterSeed, 'B still untouched after A rollback')
})

// 4. rollback rejects a never-deployed version
await check('rollbackRemote rejects a version never deployed for that remote', async () => {
	const store = new MemoryDeploymentStore()
	const target = makeFakeTarget()
	await deployRemote(target, artifact('A', '1.0.0'), { store })
	await assert.rejects(() => rollbackRemote('A', '9.9.9', { store }), /was never deployed/)
	await assert.rejects(() => rollbackRemote('ghost', '1.0.0', { store }), /unknown remote/)
	const empty = new MemoryDeploymentStore()
	await assert.rejects(() => rollbackRemote('A', '1.0.0', { store: empty }), /no deployment manifest/)
})

// 5. affected=[A] deploys ONLY A
await check('deployAffectedRemotes: affected=[A] deploys ONLY A, leaves the rest', async () => {
	const store = new MemoryDeploymentStore()
	const target = makeFakeTarget()
	// Seed A and B at v1.
	await deployRemote(target, artifact('A', '1.0.0'), { store })
	await deployRemote(target, artifact('B', '1.0.0'), { store })
	const bBefore = getRemote(await store.load(), 'B')
	target.uploads.clear()

	// Build artifacts for BOTH but mark only A affected.
	const res = await deployAffectedRemotes(
		[artifact('A', '2.0.0'), artifact('B', '2.0.0')],
		{ store, target, affected: ['A'] }
	)
	assert.deepEqual([...res.deployed], ['A'], 'only the affected remote deployed')
	assert.ok(target.uploads.has('A/2.0.0/remoteEntry.js'), 'A v2 uploaded')
	assert.ok(![...target.uploads.keys()].some((p) => p.startsWith('B/2.0.0/')), 'B v2 NOT uploaded')

	const m = res.manifest
	assert.equal(getRemote(m, 'A').currentVersion, '2.0.0', 'A advanced')
	assert.deepEqual(getRemote(m, 'B'), bBefore, 'B left at v1, untouched')
	// persisted manifest matches
	assert.deepEqual(await store.load(), m, 'final manifest persisted')
})

// 6. affected set with no matching artifact / extra artifact handling
await check('deployAffectedRemotes: affected without artifact skipped; non-affected artifact ignored', async () => {
	const store = new MemoryDeploymentStore()
	const target = makeFakeTarget()
	const res = await deployAffectedRemotes(
		[artifact('A', '1.0.0')],
		{ store, target, affected: ['A', 'B'] } // B affected but no artifact supplied
	)
	assert.deepEqual([...res.deployed], ['A'], 'only the affected remote that has an artifact ships')
	// an artifact for a remote not in affected is ignored
	const res2 = await deployAffectedRemotes(
		[artifact('A', '2.0.0'), artifact('C', '1.0.0')],
		{ store, target, affected: ['A'] }
	)
	assert.deepEqual([...res2.deployed], ['A'], 'artifact for a non-affected remote is ignored')
	assert.equal(getRemote(await store.load(), 'C'), undefined, 'non-affected remote never entered the ledger')
})

// 7. dryRun records the ledger but uploads nothing
await check('deployRemote dryRun records ledger without uploading', async () => {
	const store = new MemoryDeploymentStore()
	const target = makeFakeTarget()
	const res = await deployRemote(target, artifact('A', '1.0.0'), { store, dryRun: true })
	assert.equal(target.uploads.size, 0, 'nothing uploaded in dry run')
	assert.equal(target.begun, 0, 'begin skipped in dry run')
	assert.equal(res.deployment.currentVersion, '1.0.0', 'url + ledger still resolved')
	assert.equal(getRemote(await store.load(), 'A').currentVersion, '1.0.0', 'ledger persisted in dry run (preview)')
})

// 8. injected-interface guards
await check('deployRemote / rollbackRemote guard their injected interfaces', async () => {
	await assert.rejects(() => deployRemote({}, artifact('A', '1.0.0'), { store: new MemoryDeploymentStore() }), /DeployTarget/)
	await assert.rejects(() => deployRemote(makeFakeTarget(), artifact('A', '1.0.0'), {}), /store/)
	await assert.rejects(() => deployRemote(makeFakeTarget(), { remote: '', version: '1', entry: 'e', files: {} }, { store: new MemoryDeploymentStore() }), /remote/)
	await assert.rejects(() => deployAffectedRemotes([], { store: new MemoryDeploymentStore() }), /target/)
})

for (const line of results) console.log(line)
if (failures > 0) {
	console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSMOKE TEST PASSED')
