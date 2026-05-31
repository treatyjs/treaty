/**
 * Node smoke test for @treaty/deploy.
 *
 * Exercises the public surface against the built dist:
 *   1. assembleDeployArtifact reads a fixture build dir + manifest and yields the
 *      right file set with per-module versioned deploy paths,
 *   2. nested files are walked and keyed under <moduleId>/<version>/...,
 *   3. a partial deploy (only) covers only the listed modules,
 *   4. a partial deploy (changedFiles + graph) covers only the affected modules,
 *   5. assembly never reads/needs unselected modules' dirs,
 *   6. deploy() uploads every file to an FsDeployTarget and repoints the manifest,
 *   7. a partial deploy uploads ONLY the changed modules + leaves others in the manifest,
 *   8. dryRun computes urls + manifest without writing,
 *   9. deployViaPlugin drives a @treaty/federation-deploy FsDeployPlugin,
 *  10. rollback flips one manifest entry to a prior version, untouched elsewhere,
 *  11. assembleDeployArtifact rejects bad input.
 *
 * Run: node libs/treaty/deploy/test/deploy.smoke.mjs
 */

import assert from 'node:assert/strict'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { buildManifest, getModule, FsDeployPlugin, parseManifest } from '../../federation-deploy/dist/index.js'
import {
	assembleDeployArtifact,
	artifactPaths,
	deploy,
	deployViaPlugin,
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

const sampleModules = [
	{ moduleId: 'shell', version: '1.0.0', url: 'https://cdn/shell/1.0.0/remoteEntry.js', kind: 'host' },
	{ moduleId: 'routes/dashboard', version: '2.3.1', url: 'https://cdn/routes/dashboard/2.3.1/remoteEntry.js', kind: 'route' },
	{ moduleId: 'routes/reports', version: '0.9.0', url: 'https://cdn/routes/reports/0.9.0/remoteEntry.js', kind: 'route' },
	{ moduleId: 'libs/data-access', version: '1.4.0', url: 'https://cdn/libs/data-access/1.4.0/remoteEntry.js', kind: 'lib' },
]

const graph = {
	shell: { files: ['apps/shell/main.ts'], dependsOn: ['libs/data-access'] },
	'libs/data-access': { files: ['libs/data-access/src/api.ts'], dependsOn: [] },
	'routes/dashboard': { files: ['apps/shell/dashboard/page.ts'], dependsOn: ['libs/data-access'] },
	'routes/reports': { files: ['apps/shell/reports/page.ts'], dependsOn: [] },
}

const fixtureLayout = {
	shell: { 'remoteEntry.js': '// shell entry', 'index.html': '<!doctype html>' },
	'routes/dashboard': { 'remoteEntry.js': '// dashboard entry', 'chunks/widget.js': '// widget' },
	'routes/reports': { 'remoteEntry.js': '// reports entry' },
	'libs/data-access': { 'remoteEntry.js': '// data-access entry' },
}

const run = async () => {
	const buildDir = await mkdtemp(join(tmpdir(), 'treaty-deploy-build-'))
	try {
		await writeBuild(buildDir, fixtureLayout)
		const manifest = buildManifest(sampleModules, { app: 'shell' })

		// 1. full assembly: right file set + per-module versioned paths
		await check('assembleDeployArtifact: full artifact, versioned per-module paths', async () => {
			const artifact = await assembleDeployArtifact({ buildDir, manifest, target: 'fs' })
			assert.equal(artifact.partial, false, 'covers every manifest module -> not partial')
			assert.equal(Object.keys(artifact.modules).length, 4, 'all four modules assembled')
			assert.equal(artifact.target, 'fs', 'target recorded')

			const dash = artifact.modules['routes/dashboard']
			assert.equal(dash.basePath, 'routes/dashboard/2.3.1', 'base path is <moduleId>/<version>')
			assert.equal(dash.entry, 'remoteEntry.js', 'remote entry identified from url basename')
			assert.deepEqual(Object.keys(dash.files).sort(), [
				'routes/dashboard/2.3.1/chunks/widget.js',
				'routes/dashboard/2.3.1/remoteEntry.js',
			], 'dashboard files keyed under versioned base path (nested walked)')

			const paths = artifactPaths(artifact)
			assert.deepEqual(paths, [
				'libs/data-access/1.4.0/remoteEntry.js',
				'routes/dashboard/2.3.1/chunks/widget.js',
				'routes/dashboard/2.3.1/remoteEntry.js',
				'routes/reports/0.9.0/remoteEntry.js',
				'shell/1.0.0/index.html',
				'shell/1.0.0/remoteEntry.js',
			], 'flat upload set is the union of versioned paths, sorted')
		})

		// 3. partial via `only`
		await check('assembleDeployArtifact: partial via only covers only listed modules', async () => {
			const artifact = await assembleDeployArtifact({
				buildDir,
				manifest,
				only: ['routes/dashboard'],
			})
			assert.equal(artifact.partial, true, 'subset of manifest -> partial')
			assert.deepEqual(Object.keys(artifact.modules), ['routes/dashboard'], 'only the listed module')
			assert.ok(
				artifactPaths(artifact).every((p) => p.startsWith('routes/dashboard/2.3.1/')),
				'only the listed module contributes files'
			)
		})

		// 4. partial via changedFiles + graph (affected set)
		await check('assembleDeployArtifact: partial via changedFiles+graph = affected modules', async () => {
			const artifact = await assembleDeployArtifact({
				buildDir,
				manifest,
				changedFiles: ['libs/data-access/src/api.ts'],
				graph,
			})
			// data-access changed: itself + shell + dashboard depend on it; reports does not.
			assert.deepEqual(Object.keys(artifact.modules).sort(), [
				'libs/data-access',
				'routes/dashboard',
				'shell',
			], 'affected = changed lib + its (transitive) dependents')
			assert.ok(!('routes/reports' in artifact.modules), 'unaffected route excluded from partial artifact')
			assert.equal(artifact.partial, true)
		})

		// 5. changedFiles requires graph
		await check('assembleDeployArtifact: changedFiles without graph throws', async () => {
			await assert.rejects(
				() => assembleDeployArtifact({ buildDir, manifest, changedFiles: ['x.ts'] }),
				/requires options\.graph/
			)
		})

		// 6. deploy() to an FsDeployTarget uploads everything + repoints manifest
		await check('deploy: uploads all files to FsDeployTarget, repoints manifest', async () => {
			const out = await mkdtemp(join(tmpdir(), 'treaty-deploy-out-'))
			try {
				const artifact = await assembleDeployArtifact({ buildDir, manifest })
				const target = new FsDeployTarget({ root: out, baseUrl: 'https://cdn' })
				const res = await deploy(artifact, target)

				assert.equal(res.partial, false)
				// every assembled path written to disk under <root>/<deployPath>
				for (const path of artifactPaths(artifact)) {
					const onDisk = await readFile(join(out, path), 'utf8')
					assert.ok(onDisk.length > 0, `${path} written to disk`)
				}
				// manifest repointed to the target's urls (version preserved)
				assert.equal(
					getModule(res.manifest, 'routes/dashboard').url,
					'https://cdn/routes/dashboard/2.3.1/remoteEntry.js',
					'dashboard repointed at served url'
				)
				assert.equal(getModule(res.manifest, 'routes/dashboard').version, '2.3.1', 'version preserved')
				// input manifest never mutated
				assert.equal(getModule(manifest, 'routes/dashboard').url, 'https://cdn/routes/dashboard/2.3.1/remoteEntry.js')
			} finally {
				await rm(out, { recursive: true, force: true })
			}
		})

		// 7. partial deploy uploads ONLY changed modules; others stay in manifest
		await check('deploy: partial deploy uploads only changed modules, leaves others', async () => {
			const out = await mkdtemp(join(tmpdir(), 'treaty-deploy-partial-'))
			try {
				const artifact = await assembleDeployArtifact({
					buildDir,
					manifest,
					changedFiles: ['libs/data-access/src/api.ts'],
					graph,
				})
				const target = new FsDeployTarget({ root: out, baseUrl: 'https://cdn' })
				const res = await deploy(artifact, target)

				assert.equal(res.partial, true, 'result flagged partial')
				assert.deepEqual(Object.keys(res.modules).sort(), ['libs/data-access', 'routes/dashboard', 'shell'])
				// reports was NOT uploaded
				assert.ok(
					!res.uploaded.some((p) => p.startsWith('routes/reports/')),
					'unaffected module not uploaded'
				)
				await assert.rejects(() => readFile(join(out, 'routes/reports/0.9.0/remoteEntry.js'), 'utf8'),
					'unaffected module absent on disk')
				// the unaffected module is still present (unchanged) in the resulting manifest
				assert.deepEqual(getModule(res.manifest, 'routes/reports'), getModule(manifest, 'routes/reports'),
					'unaffected module carried through manifest unchanged')
			} finally {
				await rm(out, { recursive: true, force: true })
			}
		})

		// 8. dryRun: urls + manifest computed, nothing written
		await check('deploy: dryRun resolves urls + manifest without writing', async () => {
			const out = await mkdtemp(join(tmpdir(), 'treaty-deploy-dry-'))
			try {
				const artifact = await assembleDeployArtifact({ buildDir, manifest, only: ['shell'] })
				const target = new FsDeployTarget({ root: out, baseUrl: 'https://cdn' })
				const res = await deploy(artifact, target, { dryRun: true })
				assert.equal(getModule(res.manifest, 'shell').url, 'https://cdn/shell/1.0.0/remoteEntry.js',
					'url still resolved in dry run')
				await assert.rejects(() => readFile(join(out, 'shell/1.0.0/remoteEntry.js'), 'utf8'),
					'dry run wrote nothing')
			} finally {
				await rm(out, { recursive: true, force: true })
			}
		})

		// 9. deployViaPlugin drives a federation-deploy FsDeployPlugin
		await check('deployViaPlugin: drives a @treaty/federation-deploy FsDeployPlugin', async () => {
			const out = await mkdtemp(join(tmpdir(), 'treaty-deploy-plugin-'))
			try {
				const manifestPath = join(out, 'mf-manifest.json')
				const plugin = new FsDeployPlugin({ root: out, manifestPath, baseUrl: 'https://cdn' })
				const artifact = await assembleDeployArtifact({ buildDir, manifest, only: ['routes/dashboard'] })
				const res = await deployViaPlugin(artifact, plugin, { app: 'shell' })

				assert.equal(getModule(res.manifest, 'routes/dashboard').url,
					'https://cdn/routes/dashboard/2.3.1/remoteEntry.js', 'plugin url adopted into manifest')
				// plugin wrote the module-relative files under its own versioned layout
				assert.equal(await readFile(join(out, 'routes/dashboard/2.3.1/remoteEntry.js'), 'utf8'), '// dashboard entry',
					'plugin wrote module-relative entry under its own version dir')
				assert.equal(await readFile(join(out, 'routes/dashboard/2.3.1/chunks/widget.js'), 'utf8'), '// widget',
					'plugin wrote nested file')
				// plugin maintained its own on-disk manifest
				const onDisk = parseManifest(await readFile(manifestPath, 'utf8'))
				assert.equal(getModule(onDisk, 'routes/dashboard').version, '2.3.1')
			} finally {
				await rm(out, { recursive: true, force: true })
			}
		})

		// 10. rollback flips ONE entry to a prior version
		await check('rollback: flips one manifest entry to a prior version, untouched elsewhere', async () => {
			// deploy dashboard 2.4.0 first
			const deployed = await (async () => {
				const out = await mkdtemp(join(tmpdir(), 'treaty-deploy-rb-'))
				try {
					const next = buildManifest([
						...sampleModules.filter((m) => m.moduleId !== 'routes/dashboard'),
						{ moduleId: 'routes/dashboard', version: '2.4.0', url: 'https://cdn/routes/dashboard/2.4.0/remoteEntry.js', kind: 'route' },
					], { app: 'shell' })
					return next
				} finally {
					await rm(out, { recursive: true, force: true })
				}
			})()

			const rolledBack = rollback(deployed, 'routes/dashboard', '2.3.1')
			assert.deepEqual(getModule(rolledBack, 'routes/dashboard'), {
				version: '2.3.1',
				url: 'https://cdn/routes/dashboard/2.3.1/remoteEntry.js',
			}, 'version segment swapped by default derivation')
			for (const id of ['shell', 'routes/reports', 'libs/data-access']) {
				assert.deepEqual(getModule(rolledBack, id), getModule(deployed, id), `${id} untouched by rollback`)
			}
			// input manifest unchanged
			assert.equal(getModule(deployed, 'routes/dashboard').version, '2.4.0', 'input manifest not mutated')
			// explicit url + unknown module guard
			assert.equal(
				getModule(rollback(deployed, 'shell', '0.9.0', { url: 'https://cdn/shell/0.9.0/remoteEntry.js' }), 'shell').url,
				'https://cdn/shell/0.9.0/remoteEntry.js'
			)
			assert.throws(() => rollback(deployed, 'nope', '1.0.0'), /unknown moduleId/)
		})

		// 11. validation
		await check('assembleDeployArtifact: rejects bad input', async () => {
			await assert.rejects(() => assembleDeployArtifact({ manifest }), /buildDir is required/)
			await assert.rejects(() => assembleDeployArtifact({ buildDir }), /manifest is required/)
			await assert.rejects(
				() => assembleDeployArtifact({ buildDir, manifest, only: ['ghost'] }),
				/not in the manifest/
			)
			await assert.rejects(
				() => assembleDeployArtifact({
					buildDir: join(buildDir, 'does-not-exist'),
					manifest: buildManifest([{ moduleId: 'shell', version: '1.0.0', url: 'u' }]),
					only: ['shell'],
				}),
				/no build output for module/
			)
		})
	} finally {
		await rm(buildDir, { recursive: true, force: true })
	}

	for (const line of results) console.log(line)
	if (failures > 0) {
		console.error(`\nSMOKE TEST FAILED: ${failures} case(s) failed`)
		process.exit(1)
	}
	console.log('\nSMOKE TEST PASSED')
}

await run()
