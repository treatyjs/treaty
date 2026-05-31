/**
 * Node smoke test for @treaty/federation-deploy.
 *
 * Exercises the public surface against the built dist:
 *   1. buildManifest produces a schema-1 manifest keyed by moduleId -> { version, url },
 *   2. serialize/parse round-trips to a deep-equal manifest (and is sorted/stable),
 *   3. setModuleVersion (deploy) changes ONLY the targeted module,
 *   4. rollbackModule reverts ONE module to a prior version and leaves others,
 *   5. deploy/rollback never mutate the input manifest,
 *   6. buildManifest / setModuleVersion reject bad input,
 *   7. the runtime plugin factory returns a named plugin with the expected hooks,
 *   8. beforeRequest resolves each remote's url+version from a manifest object,
 *   9. resolveRemote repoints a single remote from the manifest,
 *  10. unknown remotes pass through by default and throw when passthrough is off.
 *
 * Run: node libs/treaty/federation-deploy/test/federation-deploy.smoke.mjs
 */

import assert from 'node:assert/strict'
import { mkdtemp, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import {
	buildManifest,
	serializeManifest,
	parseManifest,
	getModule,
	setModuleVersion,
	rollbackModule,
	createTreatyMfRuntimePlugin,
	computeAffectedModules,
	DeployPluginRegistry,
	NoopDeployPlugin,
	FsDeployPlugin,
	affectedDeployPlan,
	MANIFEST_SCHEMA,
} from '../dist/index.js'

let failures = 0
const results = []

function check(label, fn) {
	try {
		const r = fn()
		if (r instanceof Promise) {
			return r.then(
				() => results.push(`PASS ${label}`),
				(err) => {
					failures++
					results.push(`FAIL ${label}: ${err.stack ?? err.message}`)
				}
			)
		}
		results.push(`PASS ${label}`)
	} catch (err) {
		failures++
		results.push(`FAIL ${label}: ${err.stack ?? err.message}`)
	}
	return undefined
}

const sampleModules = [
	{ moduleId: 'shell', version: '1.0.0', url: 'https://cdn/app/1.0.0/remoteEntry.js', kind: 'host' },
	{ moduleId: 'routes/dashboard', version: '2.3.1', url: 'https://cdn/dashboard/2.3.1/remoteEntry.js', kind: 'route' },
	{ moduleId: 'routes/reports', version: '0.9.0', url: 'https://cdn/reports/0.9.0/remoteEntry.js', kind: 'route' },
	{ moduleId: 'libs/data-access', version: '1.4.0', url: 'https://cdn/data-access/1.4.0/remoteEntry.js', kind: 'lib' },
]

// 1. buildManifest shape
check('buildManifest -> schema-1 moduleId map', () => {
	const m = buildManifest(sampleModules, { app: 'shell' })
	assert.equal(m.schema, MANIFEST_SCHEMA, 'schema stamped')
	assert.equal(m.app, 'shell', 'app recorded')
	assert.equal(Object.keys(m.modules).length, 4, 'four modules')
	assert.deepEqual(getModule(m, 'routes/dashboard'), {
		version: '2.3.1',
		url: 'https://cdn/dashboard/2.3.1/remoteEntry.js',
	})
	assert.equal(m.kinds['libs/data-access'], 'lib', 'kind carried')
})

// 2. serialize/parse round-trip + sorted
check('serializeManifest/parseManifest round-trip (sorted, stable)', () => {
	const m = buildManifest(sampleModules, { app: 'shell' })
	const json = serializeManifest(m)
	const back = parseManifest(json)
	assert.deepEqual(back, m, 'round-trips to a deep-equal manifest')

	const keys = Object.keys(JSON.parse(json).modules)
	assert.deepEqual(keys, [...keys].sort(), 'module keys serialized in sorted order')

	// stable: serializing twice (and a reparse) is byte-identical
	assert.equal(serializeManifest(back), json, 'serialization is stable')
})

// 3. setModuleVersion changes only the one entry
check('setModuleVersion (deploy) changes ONLY the targeted module', () => {
	const m = buildManifest(sampleModules)
	const next = setModuleVersion(m, 'routes/dashboard', '2.4.0', {
		url: 'https://cdn/dashboard/2.4.0/remoteEntry.js',
	})
	assert.deepEqual(getModule(next, 'routes/dashboard'), {
		version: '2.4.0',
		url: 'https://cdn/dashboard/2.4.0/remoteEntry.js',
	}, 'targeted module repointed')

	for (const id of ['shell', 'routes/reports', 'libs/data-access']) {
		assert.deepEqual(getModule(next, id), getModule(m, id), `${id} untouched`)
	}
})

// 3b. urlFor derives the new url
check('setModuleVersion derives url via urlFor', () => {
	const m = buildManifest(sampleModules)
	const next = setModuleVersion(m, 'routes/reports', '1.0.0', {
		urlFor: (v, prev) => prev.url.replace('0.9.0', v),
	})
	assert.equal(getModule(next, 'routes/reports').url, 'https://cdn/reports/1.0.0/remoteEntry.js')
})

// 4. rollbackModule reverts one and leaves others
check('rollbackModule reverts ONE module, leaves others', () => {
	const m = buildManifest(sampleModules)
	const deployed = setModuleVersion(m, 'routes/dashboard', '2.4.0', {
		url: 'https://cdn/dashboard/2.4.0/remoteEntry.js',
	})
	const rolledBack = rollbackModule(deployed, 'routes/dashboard', '2.3.1', {
		url: 'https://cdn/dashboard/2.3.1/remoteEntry.js',
	})
	assert.deepEqual(getModule(rolledBack, 'routes/dashboard'), getModule(m, 'routes/dashboard'),
		'dashboard reverted to the original deployment')
	for (const id of ['shell', 'routes/reports', 'libs/data-access']) {
		assert.deepEqual(getModule(rolledBack, id), getModule(m, id), `${id} left intact through deploy+rollback`)
	}
})

// 5. immutability
check('deploy/rollback never mutate the input manifest', () => {
	const m = buildManifest(sampleModules)
	const before = serializeManifest(m)
	setModuleVersion(m, 'shell', '1.1.0', { url: 'https://cdn/app/1.1.0/remoteEntry.js' })
	rollbackModule(m, 'shell', '0.9.0', { url: 'https://cdn/app/0.9.0/remoteEntry.js' })
	assert.equal(serializeManifest(m), before, 'input manifest unchanged')
	assert.throws(() => { m.modules['shell'] = { version: 'x', url: 'y' } }, 'modules map is frozen')
})

// 6. validation
check('builders reject bad input', () => {
	assert.throws(() => buildManifest([{ moduleId: '', version: '1', url: 'u' }]), /moduleId/)
	assert.throws(() => buildManifest([{ moduleId: 'x', version: '', url: 'u' }]), /version/)
	assert.throws(() => buildManifest([{ moduleId: 'x', version: '1', url: '' }]), /url/)
	assert.throws(() => parseManifest('not json'), /invalid JSON/)
	assert.throws(() => parseManifest(JSON.stringify({ schema: 99, modules: {} })), /schema/)
	const m = buildManifest(sampleModules)
	assert.throws(() => setModuleVersion(m, 'nope', '1.0.0', { url: 'u' }), /unknown moduleId/)
	assert.throws(() => setModuleVersion(m, 'shell', '1.1.0'), /needs a url/)
	assert.throws(() => rollbackModule(m, 'nope', '1.0.0', { url: 'u' }), /unknown moduleId/)
})

// 7. runtime plugin factory shape
check('createTreatyMfRuntimePlugin returns a named plugin with hooks', () => {
	const factory = createTreatyMfRuntimePlugin(buildManifest(sampleModules))
	assert.equal(typeof factory, 'function', 'factory is a function')
	const plugin = factory()
	assert.equal(plugin.name, 'treaty-federation-deploy', 'default plugin name')
	assert.equal(typeof plugin.beforeRequest, 'function', 'beforeRequest hook present')
	assert.equal(typeof plugin.resolveRemote, 'function', 'resolveRemote hook present')
})

// 8. beforeRequest resolves remote url+version from a manifest object
await check('beforeRequest resolves remote url+version from manifest', async () => {
	const manifest = buildManifest(sampleModules)
	const plugin = createTreatyMfRuntimePlugin(manifest)()
	const args = {
		id: 'routes/dashboard/Widget',
		options: {
			remotes: [
				{ name: 'routes/dashboard', entry: 'https://STALE/remoteEntry.js' },
				{ name: 'libs/data-access', entry: 'https://STALE/remoteEntry.js' },
			],
		},
	}
	const out = await plugin.beforeRequest(args)
	const dash = out.options.remotes.find((r) => r.name === 'routes/dashboard')
	assert.equal(dash.entry, 'https://cdn/dashboard/2.3.1/remoteEntry.js', 'entry repointed to current url')
	assert.equal(dash.version, '2.3.1', 'version stamped from manifest')
	// input not mutated
	assert.equal(args.options.remotes[0].entry, 'https://STALE/remoteEntry.js', 'original args untouched')
})

// 9. resolveRemote repoints a single remote (and reflects a deploy)
await check('resolveRemote reflects the current manifest deployment', async () => {
	const manifest = setModuleVersion(buildManifest(sampleModules), 'routes/dashboard', '2.4.0', {
		url: 'https://cdn/dashboard/2.4.0/remoteEntry.js',
	})
	const plugin = createTreatyMfRuntimePlugin(manifest)()
	const resolved = await plugin.resolveRemote({
		remote: { name: 'routes/dashboard', entry: 'https://STALE/remoteEntry.js' },
	})
	assert.equal(resolved.entry, 'https://cdn/dashboard/2.4.0/remoteEntry.js', 'resolved to deployed url')
	assert.equal(resolved.version, '2.4.0', 'resolved to deployed version')
})

// 9b. string-url source resolves via injected fetchManifest
await check('string-url manifest source fetched + parsed via fetchManifest', async () => {
	const json = serializeManifest(buildManifest(sampleModules))
	let fetched = 0
	const plugin = createTreatyMfRuntimePlugin('https://cdn/mf-manifest.json', {
		fetchManifest: async () => { fetched++; return json },
	})()
	const out = await plugin.beforeRequest({
		id: 'x',
		options: { remotes: [{ name: 'routes/reports', entry: 'https://STALE' }] },
	})
	assert.equal(out.options.remotes[0].entry, 'https://cdn/reports/0.9.0/remoteEntry.js', 'fetched manifest applied')
	// second call uses the cache, not a second fetch
	await plugin.resolveRemote({ remote: { name: 'shell', entry: 'https://STALE' } })
	assert.equal(fetched, 1, 'manifest fetched once and cached')
})

// 10. unknown remote: passthrough vs throw
await check('unknown remote passes through by default, throws when off', async () => {
	const manifest = buildManifest(sampleModules)
	const passthrough = createTreatyMfRuntimePlugin(manifest)()
	const out = await passthrough.resolveRemote({ remote: { name: 'mystery', entry: 'https://keep' } })
	assert.equal(out.entry, 'https://keep', 'unknown remote left untouched by default')

	const strict = createTreatyMfRuntimePlugin(manifest, { passthroughUnknown: false })()
	await assert.rejects(
		() => Promise.resolve(strict.resolveRemote({ remote: { name: 'mystery', entry: 'https://keep' } })),
		/no manifest entry/,
		'strict mode throws on unknown remote'
	)
})

// ----------------------------------------------------------------------------
// CI deployment layer: affected-change detection, deploy plugins, deploy plan.
// ----------------------------------------------------------------------------

// A small federated graph: a shared lib that two routes depend on (one of them
// through an intermediate ui lib), plus an isolated route the lib never reaches.
const graph = {
	shell: { files: ['apps/shell/main.ts'], dependsOn: ['libs/data-access'] },
	'libs/data-access': { files: ['libs/data-access/src/api.ts', 'libs/data-access/src/index.ts'], dependsOn: [] },
	'libs/ui': { files: ['libs/ui/src/button.ts'], dependsOn: ['libs/data-access'] },
	'routes/dashboard': { files: ['apps/shell/dashboard/page.ts'], dependsOn: ['libs/data-access', 'libs/ui'] },
	'routes/reports': { files: ['apps/shell/reports/page.ts'], dependsOn: ['libs/ui'] },
	'routes/settings': { files: ['apps/shell/settings/page.ts'], dependsOn: [] },
}

// 11. shared-lib file change fans out to every (transitive) dependent
check('computeAffectedModules: shared-lib change marks ALL dependents affected', () => {
	const affected = computeAffectedModules(['libs/data-access/src/api.ts'], graph)
	// data-access itself + everything that reaches it: shell (direct), ui (direct),
	// dashboard (direct + via ui), reports (via ui). settings never depends on it.
	assert.deepEqual(affected, [
		'libs/data-access',
		'libs/ui',
		'routes/dashboard',
		'routes/reports',
		'shell',
	], 'fan-out reaches direct and transitive dependents, sorted')
	assert.ok(!affected.includes('routes/settings'), 'isolated route not dragged in')
})

// 12. isolated route change affects only that route
check('computeAffectedModules: isolated route change affects ONLY that route', () => {
	const affected = computeAffectedModules(['apps/shell/settings/page.ts'], graph)
	assert.deepEqual(affected, ['routes/settings'], 'only the edited route is affected')
})

// 12b. no relevant change -> empty; deterministic + de-duplicated
check('computeAffectedModules: unrelated change affects nothing; dedup + sorted', () => {
	assert.deepEqual(computeAffectedModules(['README.md'], graph), [], 'unrelated file affects nothing')
	// a route file + one of its deps' files should not double-count the route
	const affected = computeAffectedModules(
		['apps/shell/reports/page.ts', 'libs/ui/src/button.ts'],
		graph
	)
	assert.deepEqual(affected, ['libs/ui', 'routes/dashboard', 'routes/reports'], 'union, deduped, sorted')
})

// 12c. error modes: dangling edge + cycle
check('computeAffectedModules: dangling edge ignored by default, errors on demand; cycle throws', () => {
	const dangling = { a: { files: ['a.ts'], dependsOn: ['ghost'] } }
	assert.deepEqual(computeAffectedModules(['a.ts'], dangling), ['a'], 'dangling edge ignored by default')
	assert.throws(
		() => computeAffectedModules(['a.ts'], dangling, { onMissingDependency: 'error' }),
		/unknown module "ghost"/,
		'dangling edge errors when asked'
	)
	const cyclic = {
		a: { files: ['a.ts'], dependsOn: ['b'] },
		b: { files: ['b.ts'], dependsOn: ['a'] },
	}
	assert.throws(() => computeAffectedModules(['x.ts'], cyclic), /cycle/, 'cycle is reported, not spun on')
})

// 12d. custom matcher: directory-prefix ownership
check('computeAffectedModules: custom matchFile enables prefix ownership', () => {
	const prefixGraph = { 'libs/data-access': { files: ['libs/data-access/'], dependsOn: [] } }
	const affected = computeAffectedModules(['libs/data-access/src/deep/nested.ts'], prefixGraph, {
		matchFile: (changed, file) => changed.startsWith(file),
	})
	assert.deepEqual(affected, ['libs/data-access'], 'prefix matcher attributes nested files')
})

// 13. registry resolves a deploy plugin by name
check('DeployPluginRegistry registers, resolves, lists, and guards duplicates', () => {
	const registry = new DeployPluginRegistry([new NoopDeployPlugin(), new FsDeployPlugin({ name: 'fs' })])
	assert.deepEqual(registry.list(), ['fs', 'noop'], 'lists registered names sorted')
	assert.equal(registry.get('noop').name, 'noop', 'resolves a plugin by name')
	assert.ok(registry.has('fs'), 'has() reports registration')
	assert.equal(registry.tryGet('missing'), undefined, 'tryGet returns undefined for unknown')
	assert.throws(() => registry.get('missing'), /no plugin named "missing"/, 'get throws for unknown')
	assert.throws(() => registry.register(new NoopDeployPlugin()), /already registered/, 'duplicate name rejected')
	registry.register(new NoopDeployPlugin(), true) // override allowed
})

// 13b. NoopDeployPlugin reports the deployment it would make
await check('NoopDeployPlugin reports a deployment without I/O', async () => {
	const plugin = new NoopDeployPlugin()
	const dep = await plugin.deploy(
		{ moduleId: 'routes/dashboard', version: '2.4.0', kind: 'route' },
		{},
		{ params: { baseUrl: 'https://cdn' } }
	)
	assert.deepEqual(dep, { version: '2.4.0', url: 'https://cdn/routes/dashboard/2.4.0/remoteEntry.js' })
	const rb = await plugin.rollback(
		{ moduleId: 'routes/dashboard', version: '2.4.0', kind: 'route' },
		'2.3.1',
		{ params: { baseUrl: 'https://cdn' } }
	)
	assert.equal(rb.version, '2.3.1', 'rollback reports the target version')
})

// 13c. FsDeployPlugin writes artifacts + maintains an on-disk manifest, rolls back
await check('FsDeployPlugin writes artifacts, updates manifest, rolls back', async () => {
	const root = await mkdtemp(join(tmpdir(), 'treaty-fs-deploy-'))
	try {
		const manifestPath = join(root, 'mf-manifest.json')
		const plugin = new FsDeployPlugin({ root, manifestPath, baseUrl: 'https://cdn', name: 'fs' })

		// deploy v1 of a route
		const d1 = await plugin.deploy(
			{ moduleId: 'routes/dashboard', version: '1.0.0', kind: 'route' },
			{ files: { 'remoteEntry.js': '// v1' } },
			{ app: 'shell' }
		)
		assert.equal(d1.url, 'https://cdn/routes/dashboard/1.0.0/remoteEntry.js', 'deploy reports stamped url')
		assert.equal(await readFile(join(root, 'routes/dashboard/1.0.0/remoteEntry.js'), 'utf8'), '// v1',
			'artifact written under <root>/<id>/<version>/')

		let m = parseManifest(await readFile(manifestPath, 'utf8'))
		assert.deepEqual(getModule(m, 'routes/dashboard'), d1, 'manifest file points at v1')

		// deploy v2 (writes new dir, repoints manifest)
		await plugin.deploy(
			{ moduleId: 'routes/dashboard', version: '2.0.0', kind: 'route' },
			{ files: { 'remoteEntry.js': '// v2' } },
			{ app: 'shell' }
		)
		m = parseManifest(await readFile(manifestPath, 'utf8'))
		assert.equal(getModule(m, 'routes/dashboard').version, '2.0.0', 'manifest advanced to v2')

		// rollback to the still-published v1 (no rebuild)
		const rb = await plugin.rollback({ moduleId: 'routes/dashboard', version: '2.0.0', kind: 'route' }, '1.0.0', {})
		assert.equal(rb.url, 'https://cdn/routes/dashboard/1.0.0/remoteEntry.js', 'rollback resolves the v1 url')
		m = parseManifest(await readFile(manifestPath, 'utf8'))
		assert.equal(getModule(m, 'routes/dashboard').version, '1.0.0', 'manifest reverted to v1')

		// rollback to a never-published version fails (artifact missing)
		await assert.rejects(
			() => plugin.rollback({ moduleId: 'routes/dashboard', version: '1.0.0', kind: 'route' }, '9.9.9', {}),
			/no published artifact/,
			'cannot roll back to an unpublished version'
		)
	} finally {
		await rm(root, { recursive: true, force: true })
	}
})

// 14. affectedDeployPlan: right module set + manifest deltas for a shared-lib change
check('affectedDeployPlan: shared-lib change -> dependents compiled/tested/deployed + manifest deltas', () => {
	const manifest = buildManifest([
		{ moduleId: 'shell', version: '1.0.0', url: 'https://cdn/shell/1.0.0/remoteEntry.js', kind: 'host' },
		{ moduleId: 'libs/data-access', version: '1.0.0', url: 'https://cdn/libs/data-access/1.0.0/remoteEntry.js', kind: 'lib' },
		{ moduleId: 'libs/ui', version: '1.0.0', url: 'https://cdn/libs/ui/1.0.0/remoteEntry.js', kind: 'lib' },
		{ moduleId: 'routes/dashboard', version: '1.0.0', url: 'https://cdn/routes/dashboard/1.0.0/remoteEntry.js', kind: 'route' },
		{ moduleId: 'routes/reports', version: '1.0.0', url: 'https://cdn/routes/reports/1.0.0/remoteEntry.js', kind: 'route' },
		{ moduleId: 'routes/settings', version: '1.0.0', url: 'https://cdn/routes/settings/1.0.0/remoteEntry.js', kind: 'route' },
	], { app: 'shell' })

	const plan = affectedDeployPlan(['libs/data-access/src/api.ts'], graph, manifest, {
		version: '2.0.0',
		urlFor: (id, v) => `https://cdn/${id}/${v}/remoteEntry.js`,
	})

	const expected = ['libs/data-access', 'libs/ui', 'routes/dashboard', 'routes/reports', 'shell']
	assert.deepEqual([...plan.compile], expected, 'compiles the full affected set')
	assert.deepEqual([...plan.test], expected, 'tests the full affected set')
	assert.deepEqual(plan.deploy.map((d) => d.moduleId), expected, 'deploys the affected set')
	assert.ok(!plan.empty, 'plan is not empty')
	assert.ok(plan.deploy.every((d) => d.version === '2.0.0'), 'version policy applied to each')

	// manifest deltas: exactly the affected modules moved 1.0.0 -> 2.0.0
	assert.deepEqual(plan.manifestChanges.map((c) => c.moduleId), expected, 'one delta per affected module')
	for (const c of plan.manifestChanges) {
		assert.equal(c.from.version, '1.0.0', 'delta from old version')
		assert.equal(c.to.version, '2.0.0', 'delta to new version')
	}
	// settings (unaffected) is untouched in nextManifest
	assert.deepEqual(getModule(plan.nextManifest, 'routes/settings'), getModule(manifest, 'routes/settings'),
		'unaffected module untouched in nextManifest')
	assert.equal(getModule(plan.nextManifest, 'libs/data-access').version, '2.0.0', 'affected module advanced')
	// input manifest never mutated
	assert.equal(getModule(manifest, 'libs/data-access').version, '1.0.0', 'input manifest unchanged')
})

// 14b. isolated route change -> single-module plan
check('affectedDeployPlan: isolated route change -> single module + single delta', () => {
	const manifest = buildManifest([
		{ moduleId: 'routes/settings', version: '3.1.0', url: 'https://cdn/routes/settings/3.1.0/remoteEntry.js', kind: 'route' },
		{ moduleId: 'libs/data-access', version: '1.0.0', url: 'https://cdn/libs/data-access/1.0.0/remoteEntry.js', kind: 'lib' },
	])
	const plan = affectedDeployPlan(['apps/shell/settings/page.ts'], graph, manifest, {
		version: (id, prev) => `${prev.version}-${id === 'routes/settings' ? 'next' : 'x'}`,
		urlFor: (id, v) => `https://cdn/${id}/${v}/remoteEntry.js`,
	})
	assert.deepEqual([...plan.compile], ['routes/settings'], 'only the route compiled')
	assert.equal(plan.manifestChanges.length, 1, 'a single manifest delta')
	assert.equal(plan.manifestChanges[0].to.version, '3.1.0-next', 'derived version applied')
	assert.deepEqual(getModule(plan.nextManifest, 'libs/data-access'), getModule(manifest, 'libs/data-access'),
		'untouched lib carried through unchanged')
})

// 14c. nothing affected -> empty plan, nextManifest === input
check('affectedDeployPlan: no affected modules -> empty plan, manifest unchanged', () => {
	const manifest = buildManifest([
		{ moduleId: 'routes/settings', version: '1.0.0', url: 'https://cdn/routes/settings/1.0.0/remoteEntry.js' },
	])
	const plan = affectedDeployPlan(['docs/CHANGELOG.md'], graph, manifest)
	assert.ok(plan.empty, 'plan reports empty')
	assert.deepEqual([...plan.compile], [], 'nothing to compile')
	assert.deepEqual([...plan.deploy], [], 'nothing to deploy')
	assert.deepEqual([...plan.manifestChanges], [], 'no manifest deltas')
	assert.equal(plan.nextManifest, manifest, 'nextManifest is the untouched input when nothing changed')
})

// 14d. end-to-end: plan an affected set, then deploy each via the registry's plugin
await check('affectedDeployPlan + DeployPluginRegistry: plan then deploy each module', async () => {
	const root = await mkdtemp(join(tmpdir(), 'treaty-plan-deploy-'))
	try {
		const manifestPath = join(root, 'mf-manifest.json')
		const manifest = buildManifest([
			{ moduleId: 'libs/data-access', version: '1.0.0', url: 'https://cdn/libs/data-access/1.0.0/remoteEntry.js', kind: 'lib' },
			{ moduleId: 'routes/dashboard', version: '1.0.0', url: 'https://cdn/routes/dashboard/1.0.0/remoteEntry.js', kind: 'route' },
			{ moduleId: 'routes/reports', version: '1.0.0', url: 'https://cdn/routes/reports/1.0.0/remoteEntry.js', kind: 'route' },
			{ moduleId: 'routes/settings', version: '1.0.0', url: 'https://cdn/routes/settings/1.0.0/remoteEntry.js', kind: 'route' },
			{ moduleId: 'libs/ui', version: '1.0.0', url: 'https://cdn/libs/ui/1.0.0/remoteEntry.js', kind: 'lib' },
			{ moduleId: 'shell', version: '1.0.0', url: 'https://cdn/shell/1.0.0/remoteEntry.js', kind: 'host' },
		], { app: 'shell' })

		const registry = new DeployPluginRegistry([new FsDeployPlugin({ root, manifestPath, baseUrl: 'https://cdn' })])
		const plugin = registry.get('fs')

		const plan = affectedDeployPlan(['libs/data-access/src/api.ts'], graph, manifest, {
			version: '2.0.0',
			urlFor: (id, v) => `https://cdn/${id}/${v}/remoteEntry.js`,
		})

		const kindOf = (id) => manifest.kinds[id]
		for (const planned of plan.deploy) {
			const dep = await plugin.deploy(
				{ moduleId: planned.moduleId, version: planned.version, kind: kindOf(planned.moduleId) },
				{ files: { 'remoteEntry.js': `// ${planned.moduleId}@${planned.version}` } },
				{ app: 'shell' }
			)
			assert.equal(dep.url, planned.url, `${planned.moduleId} deployed to the planned url`)
		}

		// the on-disk manifest now matches the plan's nextManifest for affected modules,
		// and the unaffected route stays at 1.0.0 (never touched).
		const onDisk = parseManifest(await readFile(manifestPath, 'utf8'))
		assert.equal(getModule(onDisk, 'libs/data-access').version, '2.0.0', 'affected lib advanced on disk')
		assert.equal(getModule(onDisk, 'routes/dashboard').version, '2.0.0', 'dependent route advanced on disk')
		assert.equal(getModule(onDisk, 'routes/settings'), undefined, 'unaffected route never deployed by this plan')
	} finally {
		await rm(root, { recursive: true, force: true })
	}
})

for (const line of results) console.log(line)
if (failures > 0) {
	console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSMOKE TEST PASSED')
