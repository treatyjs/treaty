/**
 * End-to-end D2 smoke test: the WHOLE deployment-granularity chain, wired across
 * all three packages (@treaty/module-federation -> @treaty/federation-deploy ->
 * @treaty/deploy) against their built dist.
 *
 * It proves the four D2 goals compose into one pipeline, with NO hand-written
 * exposes anywhere:
 *   1. route-graph pass: a lazy route auto-becomes a deployable module, a lib is a
 *      module, eager routes are NOT modules (federatedModules over a fixture graph);
 *   3. a versioned manifest (module -> version -> url) is GENERATED from that route
 *      graph (federatedModuleInputs -> buildManifest), no manual module list;
 *   - build-to-deploy: a deploy artifact is EMITTED from a built dir + the manifest
 *     (assembleDeployArtifact) and uploaded to a pluggable target (deploy + FsDeployTarget);
 *   4. partial deploy + single-entry ROLLBACK: flipping ONE manifest entry rolls
 *      back ONE route, every other module untouched (rollback);
 *   - the @module-federation/enhanced runtime plugin resolves the flipped entry at
 *     load (createTreatyMfRuntimePlugin) — a manifest flip takes effect next load;
 *   - disabled federation skips the whole chain (no modules, no deploy).
 *
 * Run: node libs/treaty/deploy/test/d2-flow.smoke.mjs
 */

import assert from 'node:assert/strict'
import { mkdtemp, readFile, rm, writeFile, mkdir } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import {
	federatedModules,
	federatedModuleInputs,
	generateMfConfig,
} from '../../module-federation/dist/index.js'
import {
	buildManifest,
	getModule,
	createTreatyMfRuntimePlugin,
} from '../../federation-deploy/dist/index.js'
import {
	assembleDeployArtifact,
	artifactPaths,
	deploy,
	rollback,
	FsDeployTarget,
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

/**
 * Build a manifest with exactly ONE module rolled forward to `version` (its url
 * version-stamped to match), every other module carried through unchanged — the
 * single-module deploy this test rolls back from.
 */
function deployOneForward(manifest, moduleId, version) {
	return buildManifest(
		Object.entries(manifest.modules).map(([id, dep]) => ({
			moduleId: id,
			version: id === moduleId ? version : dep.version,
			url: id === moduleId ? `https://cdn/${id}/${version}/remoteEntry.js` : dep.url,
			kind: manifest.kinds[id],
		})),
		{ app: 'shell' }
	)
}

/** Lay down a fixture build dir: <buildDir>/<moduleId>/<files...>. */
async function writeBuild(buildDir, layout) {
	for (const [moduleId, files] of Object.entries(layout)) {
		for (const [rel, contents] of Object.entries(files)) {
			const dest = join(buildDir, moduleId, rel)
			await mkdir(join(dest, '..'), { recursive: true })
			await writeFile(dest, contents)
		}
	}
}

// A real-shaped Angular route graph for a "shell" app. NO exposes are written by
// hand anywhere in this test — every deployable module is derived from this graph.
const appOptions = {
	name: 'shell',
	routes: [
		{ path: '', redirectTo: 'dashboard', pathMatch: 'full' }, // eager redirect: not a module
		{ path: 'home', component: {} }, // eager component: not a module
		{ path: 'dashboard', loadComponent: () => ({}) }, // lazy: a module
		{ path: 'reports', loadChildren: () => ({}) }, // lazy children: a module
		{
			path: 'admin',
			component: {}, // eager layout shell
			children: [{ path: 'users', loadComponent: () => ({}) }], // nested lazy: a module
		},
	],
	libs: ['./libs/data-access'], // a lib: a module
}

const run = async () => {
	// 1 + 2: the route-graph pass yields exactly the host + lazy routes + libs.
	await check('D2(1/2): route graph -> deployable modules (host + lazy routes + libs, no eager)', () => {
		const modules = federatedModules(appOptions)
		const byId = Object.fromEntries(modules.map((m) => [m.moduleId, m]))

		assert.equal(byId['shell'].kind, 'host', 'host container is a module')
		assert.equal(byId['./routes/dashboard'].kind, 'route', 'lazy loadComponent route is a module')
		assert.equal(byId['./routes/reports'].kind, 'route', 'lazy loadChildren route is a module')
		assert.equal(byId['./routes/admin/users'].kind, 'route', 'nested lazy route under eager layout is a module')
		assert.equal(byId['./libs/data-access'].kind, 'lib', 'lib is a module')

		assert.equal(byId['./routes/home'], undefined, 'eager component route is NOT a module')
		assert.equal(byId['./routes/index'], undefined, 'eager redirect is NOT a module')

		// the host module path is its remote-entry filename; a route's path is its exposed source.
		assert.equal(byId['shell'].path, 'remoteEntry.js', 'host backed by its remote entry filename')
		assert.equal(byId['./routes/dashboard'].path, './src/app/dashboard', 'route backed by its exposed source path')

		// exactly the five modules above, sorted, deterministic.
		assert.deepEqual(modules.map((m) => m.moduleId), [
			'./libs/data-access',
			'./routes/admin/users',
			'./routes/dashboard',
			'./routes/reports',
			'shell',
		], 'exactly the host + every lazy route + every lib, sorted')

		// includeHost:false drops the host.
		const noHost = federatedModules(appOptions, { includeHost: false })
		assert.ok(!noHost.some((m) => m.moduleId === 'shell'), 'includeHost:false omits the host')
	})

	// 3: a versioned manifest is generated straight from the route graph.
	const manifest = buildManifest(
		federatedModuleInputs(appOptions, {
			version: '1.0.0',
			urlFor: (m, v) => `https://cdn/${m.moduleId}/${v}/remoteEntry.js`,
		}),
		{ app: 'shell' }
	)

	await check('D2(3): versioned manifest GENERATED from the route graph (module -> version -> url)', () => {
		assert.equal(manifest.app, 'shell', 'manifest names the app')
		assert.deepEqual(Object.keys(manifest.modules).sort(), [
			'./libs/data-access',
			'./routes/admin/users',
			'./routes/dashboard',
			'./routes/reports',
			'shell',
		], 'manifest carries exactly the derived modules')
		assert.deepEqual(getModule(manifest, './routes/dashboard'), {
			version: '1.0.0',
			url: 'https://cdn/./routes/dashboard/1.0.0/remoteEntry.js',
		}, 'each module pins a version + url')
		assert.equal(manifest.kinds['shell'], 'host', 'host kind preserved into the manifest')
		assert.equal(manifest.kinds['./libs/data-access'], 'lib', 'lib kind preserved into the manifest')
		assert.equal(manifest.kinds['./routes/dashboard'], 'route', 'route kind preserved into the manifest')
	})

	// build-to-deploy: emit an artifact from a built dir + the generated manifest,
	// then deploy it to a pluggable target.
	await check('build-to-deploy: EMIT a deploy artifact from the manifest + deploy it to a target', async () => {
		const buildDir = await mkdtemp(join(tmpdir(), 'treaty-d2-build-'))
		const out = await mkdtemp(join(tmpdir(), 'treaty-d2-out-'))
		try {
			// the compiler's output: one dir per moduleId, each with a remote entry.
			await writeBuild(buildDir, {
				shell: { 'remoteEntry.js': '// shell host' },
				'./routes/dashboard': { 'remoteEntry.js': '// dashboard', 'chunks/a.js': '// chunk' },
				'./routes/reports': { 'remoteEntry.js': '// reports' },
				'./routes/admin/users': { 'remoteEntry.js': '// admin users' },
				'./libs/data-access': { 'remoteEntry.js': '// data-access' },
			})

			const artifact = await assembleDeployArtifact({ buildDir, manifest, target: 'fs' })
			assert.equal(artifact.partial, false, 'covers every module -> a full artifact')
			assert.equal(Object.keys(artifact.modules).length, 5, 'all five modules assembled')
			// files laid out under versioned deploy paths <moduleId>/<version>/<file>.
			assert.ok(
				artifactPaths(artifact).includes('routes/dashboard/1.0.0/remoteEntry.js'),
				'dashboard entry keyed under its versioned deploy path'
			)
			assert.ok(
				artifactPaths(artifact).includes('routes/dashboard/1.0.0/chunks/a.js'),
				'nested chunk keyed under the versioned deploy path'
			)

			const target = new FsDeployTarget({ root: out, baseUrl: 'https://cdn' })
			const res = await deploy(artifact, target)
			assert.equal(res.partial, false)
			// every file landed on disk under the served layout.
			for (const path of artifactPaths(artifact)) {
				const onDisk = await readFile(join(out, path), 'utf8')
				assert.ok(onDisk.length > 0, `${path} uploaded to the target`)
			}
			// the published manifest repoints each module at the target's served url.
			assert.equal(
				getModule(res.manifest, './routes/dashboard').url,
				'https://cdn/routes/dashboard/1.0.0/remoteEntry.js',
				'manifest repointed at the deployed url'
			)
			assert.equal(getModule(res.manifest, './routes/dashboard').version, '1.0.0', 'version carried through')
		} finally {
			await rm(buildDir, { recursive: true, force: true })
			await rm(out, { recursive: true, force: true })
		}
	})

	// 4: deploy a NEW version of ONE route, then flip a SINGLE manifest entry back —
	// only that route rolls back, every other module is byte-for-byte untouched.
	await check('D2(4): a single-entry flip rolls back ONE route; every other module untouched', () => {
		// roll dashboard forward to 2.0.0 (one entry changes) to set up the rollback.
		const deployed = deployOneForward(manifest, './routes/dashboard', '2.0.0')

		assert.equal(getModule(deployed, './routes/dashboard').version, '2.0.0', 'dashboard rolled forward to 2.0.0')

		// ROLLBACK: flip exactly one manifest entry back to the prior version.
		const rolledBack = rollback(deployed, './routes/dashboard', '1.0.0')
		assert.deepEqual(getModule(rolledBack, './routes/dashboard'), {
			version: '1.0.0',
			url: 'https://cdn/./routes/dashboard/1.0.0/remoteEntry.js',
		}, 'dashboard flipped back to 1.0.0 (version segment swapped in the url)')

		// EVERY other module is untouched by the single-entry flip.
		for (const id of ['shell', './routes/reports', './routes/admin/users', './libs/data-access']) {
			assert.deepEqual(getModule(rolledBack, id), getModule(deployed, id), `${id} untouched by the rollback`)
		}
		// the input manifest was not mutated.
		assert.equal(getModule(deployed, './routes/dashboard').version, '2.0.0', 'input manifest not mutated by rollback')
	})

	// the enhanced runtime plugin resolves the manifest's current entry at load, so
	// a flip takes effect on the next load with no rebuild.
	await check('runtime: the enhanced plugin resolves the flipped entry at load', async () => {
		const deployed = deployOneForward(manifest, './routes/dashboard', '2.0.0')
		const rolledBack = rollback(deployed, './routes/dashboard', '1.0.0')

		const plugin = createTreatyMfRuntimePlugin(rolledBack)()
		const resolved = await plugin.resolveRemote({
			remote: { name: './routes/dashboard', entry: 'https://STALE/remoteEntry.js' },
		})
		assert.equal(resolved.entry, 'https://cdn/./routes/dashboard/1.0.0/remoteEntry.js', 'runtime resolves the rolled-back url')
		assert.equal(resolved.version, '1.0.0', 'runtime stamps the rolled-back version')

		// an untouched module still resolves to its live url.
		const reports = await plugin.resolveRemote({
			remote: { name: './routes/reports', entry: 'https://STALE/remoteEntry.js' },
		})
		assert.equal(reports.version, '1.0.0', 'untouched module unaffected by the flip')
	})

	// disabled federation: the route graph produces no route/lib modules to deploy.
	await check('toggle: federation off -> only the host remains, no route/lib modules', () => {
		const off = { ...appOptions, enabled: false }
		const modules = federatedModules(off)
		assert.deepEqual(modules.map((m) => m.moduleId), ['shell'], 'disabled config exposes nothing (host only)')
		assert.deepEqual(federatedModules(off, { includeHost: false }), [], 'and nothing at all without the host')

		// and the generated config itself is inert.
		const cfg = generateMfConfig(off)
		assert.equal(cfg.enabled, false, 'config reports disabled')
		assert.deepEqual(cfg.exposes, {}, 'no exposes when disabled')
	})

	for (const line of results) console.log(line)
	if (failures > 0) {
		console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
		process.exit(1)
	}
	console.log('\nSMOKE TEST PASSED')
}

await run()
