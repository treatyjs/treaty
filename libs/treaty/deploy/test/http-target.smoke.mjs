/**
 * Node smoke test for the CONCRETE cloud deploy target: HttpDeployTarget.
 *
 * Unlike deploy.smoke.mjs (which drives the reference FsDeployTarget), this stands
 * up a REAL in-process HTTP object store — a node:http server that stores PUT
 * bodies in a map keyed by url path, serves them on GET, and removes them on
 * DELETE — and drives a full build-to-deploy through HttpDeployTarget against it.
 * It is the credential-free analogue of an S3/GCS/R2/static-host upload API.
 *
 * Cases:
 *   1. deploy(artifact, HttpDeployTarget) PUTs every assembled file to its
 *      versioned <moduleId>/<version>/<file> path; each is then fetchable back from
 *      the server with the exact bytes (proves a real round-trip, not a mock).
 *   2. the returned manifest is repointed at each module's served url; the input
 *      manifest is never mutated.
 *   3. content-type is inferred from the path's extension.
 *   4. publicBaseUrl (a CDN host distinct from the upload origin) drives urlFor.
 *   5. partial deploy (changedFiles + graph) PUTs ONLY the affected modules; an
 *      unaffected module's object is never created on the server.
 *   6. a non-2xx upload response throws (a failed deploy is never reported live).
 *   7. rollback(manifest, moduleId, toVersion) flips EXACTLY one manifest entry to
 *      an already-published prior version (still served by the store) and leaves
 *      every other module untouched.
 *   8. dryRun resolves urls without issuing any request.
 *
 * Run: node libs/treaty/deploy/test/http-target.smoke.mjs
 */

import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { buildManifest, getModule } from '../../federation-deploy/dist/index.js'
import { assembleDeployArtifact, artifactPaths, deploy, rollback, HttpDeployTarget } from '../dist/index.js'

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

/**
 * A real HTTP object store: PUT stores the body under the request path, GET serves
 * it, DELETE removes it, and a `fail` set lets a test force a 500 on a given path.
 * Returns { baseUrl, store, contentTypes, close }.
 */
async function startObjectStore({ fail = new Set() } = {}) {
	/** @type {Map<string, Buffer>} */
	const store = new Map()
	/** @type {Map<string, string>} */
	const contentTypes = new Map()
	const requests = []

	const server = createServer((req, res) => {
		const key = decodeURIComponent(new URL(req.url, 'http://localhost').pathname.replace(/^\//, ''))
		requests.push(`${req.method} ${key}`)
		if (fail.has(key) && req.method === 'PUT') {
			res.writeHead(500, 'Forced Failure')
			res.end('forced failure')
			return
		}
		if (req.method === 'PUT') {
			const chunks = []
			req.on('data', (c) => chunks.push(c))
			req.on('end', () => {
				store.set(key, Buffer.concat(chunks))
				const ct = req.headers['content-type']
				if (typeof ct === 'string') contentTypes.set(key, ct)
				res.writeHead(201, 'Created')
				res.end()
			})
			return
		}
		if (req.method === 'GET') {
			const body = store.get(key)
			if (!body) {
				res.writeHead(404, 'Not Found')
				res.end()
				return
			}
			res.writeHead(200, 'OK')
			res.end(body)
			return
		}
		if (req.method === 'DELETE') {
			store.delete(key)
			res.writeHead(204, 'No Content')
			res.end()
			return
		}
		res.writeHead(405, 'Method Not Allowed')
		res.end()
	})

	await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
	const { port } = server.address()
	return {
		baseUrl: `http://127.0.0.1:${port}`,
		store,
		contentTypes,
		requests,
		close: () => new Promise((resolve) => server.close(resolve)),
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

async function fetchText(url) {
	const res = await fetch(url)
	if (!res.ok) throw new Error(`GET ${url} -> ${res.status}`)
	return res.text()
}

const run = async () => {
	const buildDir = await mkdtemp(join(tmpdir(), 'treaty-http-build-'))
	try {
		await writeBuild(buildDir, fixtureLayout)
		const manifest = buildManifest(sampleModules, { app: 'shell' })

		// 1 + 2 + 3: full deploy through a real HTTP object store.
		await check('deploy: HttpDeployTarget PUTs every file to its versioned path; fetchable back; manifest repointed', async () => {
			const srv = await startObjectStore()
			try {
				const artifact = await assembleDeployArtifact({ buildDir, manifest, target: 'http' })
				const target = new HttpDeployTarget({ baseUrl: srv.baseUrl })
				const res = await deploy(artifact, target, { env: 'prod' })

				assert.equal(res.partial, false, 'full artifact -> not partial')

				// every assembled path landed on the server at <baseUrl>/<deployPath>, with the exact bytes.
				const expectedBytes = {
					'libs/data-access/1.4.0/remoteEntry.js': '// data-access entry',
					'routes/dashboard/2.3.1/chunks/widget.js': '// widget',
					'routes/dashboard/2.3.1/remoteEntry.js': '// dashboard entry',
					'routes/reports/0.9.0/remoteEntry.js': '// reports entry',
					'shell/1.0.0/index.html': '<!doctype html>',
					'shell/1.0.0/remoteEntry.js': '// shell entry',
				}
				assert.deepEqual(artifactPaths(artifact), Object.keys(expectedBytes).sort(), 'assembled paths are the versioned set')
				for (const [path, body] of Object.entries(expectedBytes)) {
					const got = await fetchText(`${srv.baseUrl}/${path}`)
					assert.equal(got, body, `${path} fetched back from the store with exact bytes`)
				}
				// res.uploaded mirrors the versioned paths.
				assert.deepEqual([...res.uploaded], Object.keys(expectedBytes).sort(), 'uploaded set = versioned paths')

				// manifest repointed at the served (http) urls; version preserved.
				assert.equal(
					getModule(res.manifest, 'routes/dashboard').url,
					`${srv.baseUrl}/routes/dashboard/2.3.1/remoteEntry.js`,
					'dashboard repointed at the http served url'
				)
				assert.equal(getModule(res.manifest, 'routes/dashboard').version, '2.3.1', 'version preserved')
				assert.equal(getModule(res.manifest, 'shell').url, `${srv.baseUrl}/shell/1.0.0/remoteEntry.js`, 'host repointed too')

				// input manifest never mutated.
				assert.equal(getModule(manifest, 'routes/dashboard').url, 'https://cdn/routes/dashboard/2.3.1/remoteEntry.js', 'input manifest untouched')

				// content type inferred from extension.
				assert.equal(srv.contentTypes.get('shell/1.0.0/index.html'), 'text/html; charset=utf-8', 'html content-type inferred')
				assert.equal(srv.contentTypes.get('shell/1.0.0/remoteEntry.js'), 'application/javascript', 'js content-type inferred')
			} finally {
				await srv.close()
			}
		})

		// 4: publicBaseUrl (CDN host distinct from the upload origin) drives urlFor.
		await check('deploy: publicBaseUrl repoints the manifest at the CDN host, not the upload origin', async () => {
			const srv = await startObjectStore()
			try {
				const artifact = await assembleDeployArtifact({ buildDir, manifest, only: ['shell'] })
				const target = new HttpDeployTarget({ baseUrl: srv.baseUrl, publicBaseUrl: 'https://assets.example.com/app' })
				const res = await deploy(artifact, target)
				// uploaded to the origin...
				assert.equal(await fetchText(`${srv.baseUrl}/shell/1.0.0/remoteEntry.js`), '// shell entry', 'still PUT to the upload origin')
				// ...but served from the CDN host in the manifest.
				assert.equal(
					getModule(res.manifest, 'shell').url,
					'https://assets.example.com/app/shell/1.0.0/remoteEntry.js',
					'manifest url uses publicBaseUrl'
				)
			} finally {
				await srv.close()
			}
		})

		// 5: partial deploy PUTs only affected modules; unaffected object never created.
		await check('deploy: partial (changedFiles+graph) PUTs only affected modules', async () => {
			const srv = await startObjectStore()
			try {
				const artifact = await assembleDeployArtifact({
					buildDir,
					manifest,
					changedFiles: ['libs/data-access/src/api.ts'],
					graph,
				})
				const target = new HttpDeployTarget({ baseUrl: srv.baseUrl })
				const res = await deploy(artifact, target)

				assert.equal(res.partial, true, 'result flagged partial')
				assert.deepEqual(Object.keys(res.modules).sort(), ['libs/data-access', 'routes/dashboard', 'shell'])

				// reports was NOT uploaded -> not on the server.
				assert.equal((await fetch(`${srv.baseUrl}/routes/reports/0.9.0/remoteEntry.js`)).status, 404, 'unaffected module absent on the store')
				assert.ok(!res.uploaded.some((p) => p.startsWith('routes/reports/')), 'unaffected module not in uploaded set')
				// affected ones are present.
				assert.equal(await fetchText(`${srv.baseUrl}/libs/data-access/1.4.0/remoteEntry.js`), '// data-access entry', 'changed lib uploaded')
				// unaffected module carried through the manifest unchanged.
				assert.deepEqual(getModule(res.manifest, 'routes/reports'), getModule(manifest, 'routes/reports'), 'unaffected manifest entry untouched')
			} finally {
				await srv.close()
			}
		})

		// 6: a non-2xx upload throws.
		await check('deploy: a non-2xx upload response throws (failed deploy is not reported live)', async () => {
			const srv = await startObjectStore({ fail: new Set(['shell/1.0.0/remoteEntry.js']) })
			try {
				const artifact = await assembleDeployArtifact({ buildDir, manifest, only: ['shell'] })
				const target = new HttpDeployTarget({ baseUrl: srv.baseUrl })
				await assert.rejects(() => deploy(artifact, target), /PUT .*failed: 500/)
			} finally {
				await srv.close()
			}
		})

		// 7: rollback flips EXACTLY one entry to an already-published prior version.
		await check('rollback: flips exactly one entry to a prior, still-served version', async () => {
			const srv = await startObjectStore()
			try {
				// Publish v2.3.1 of every module (the live release).
				const live = await deploy(
					await assembleDeployArtifact({ buildDir, manifest }),
					new HttpDeployTarget({ baseUrl: srv.baseUrl })
				)

				// Now publish a NEW dashboard version 2.4.0 (a partial deploy of just dashboard).
				const v240Manifest = buildManifest(
					[
						...sampleModules.filter((m) => m.moduleId !== 'routes/dashboard'),
						{ moduleId: 'routes/dashboard', version: '2.4.0', url: 'https://cdn/routes/dashboard/2.4.0/remoteEntry.js', kind: 'route' },
					],
					{ app: 'shell' }
				)
				// Lay down a 2.4.0 build under the same module dir; the version comes from the manifest.
				await writeBuild(buildDir, { 'routes/dashboard': { 'remoteEntry.js': '// dashboard entry v2.4.0', 'chunks/widget.js': '// widget v2.4.0' } })
				const target = new HttpDeployTarget({ baseUrl: srv.baseUrl })
				const after = await deploy(
					await assembleDeployArtifact({ buildDir, manifest: v240Manifest, only: ['routes/dashboard'] }),
					target,
					{}
				)
				// dashboard now live at 2.4.0; both versions are present on the store.
				assert.equal(getModule(after.manifest, 'routes/dashboard').version, '2.4.0', 'dashboard rolled forward to 2.4.0')
				assert.equal(await fetchText(`${srv.baseUrl}/routes/dashboard/2.4.0/remoteEntry.js`), '// dashboard entry v2.4.0', '2.4.0 published')
				assert.equal(await fetchText(`${srv.baseUrl}/routes/dashboard/2.3.1/remoteEntry.js`), '// dashboard entry', '2.3.1 still served (immutable versioned paths)')

				// ROLLBACK to 2.3.1: flips exactly one manifest entry, uploads nothing.
				const rolledBack = rollback(after.manifest, 'routes/dashboard', '2.3.1', { url: `${srv.baseUrl}/routes/dashboard/2.3.1/remoteEntry.js` })
				assert.deepEqual(getModule(rolledBack, 'routes/dashboard'), {
					version: '2.3.1',
					url: `${srv.baseUrl}/routes/dashboard/2.3.1/remoteEntry.js`,
				}, 'dashboard flipped back to the still-served 2.3.1 objects')
				// the prior 2.3.1 url is genuinely live on the store.
				assert.equal(await fetchText(getModule(rolledBack, 'routes/dashboard').url), '// dashboard entry', 'rollback url resolves on the store')

				// EXACTLY one entry changed: every other module identical to the pre-rollback manifest.
				let changed = 0
				for (const id of Object.keys(after.manifest.modules)) {
					if (JSON.stringify(getModule(rolledBack, id)) !== JSON.stringify(getModule(after.manifest, id))) changed++
				}
				assert.equal(changed, 1, 'rollback changed exactly one manifest entry')
				for (const id of ['shell', 'routes/reports', 'libs/data-access']) {
					assert.deepEqual(getModule(rolledBack, id), getModule(after.manifest, id), `${id} untouched by rollback`)
				}
				// input manifest unchanged.
				assert.equal(getModule(after.manifest, 'routes/dashboard').version, '2.4.0', 'pre-rollback manifest not mutated')
				// default derivation (swap the version segment) also resolves to the served 2.3.1.
				const derived = rollback(after.manifest, 'routes/dashboard', '2.3.1')
				assert.equal(getModule(derived, 'routes/dashboard').url, `${srv.baseUrl}/routes/dashboard/2.3.1/remoteEntry.js`, 'default url derivation swaps the version segment')
				void live
			} finally {
				await srv.close()
			}
		})

		// 8: dryRun issues no request but still resolves urls.
		await check('deploy: dryRun resolves urls without issuing any request', async () => {
			const srv = await startObjectStore()
			try {
				const artifact = await assembleDeployArtifact({ buildDir, manifest, only: ['shell'] })
				const target = new HttpDeployTarget({ baseUrl: srv.baseUrl })
				const res = await deploy(artifact, target, { dryRun: true })
				assert.equal(getModule(res.manifest, 'shell').url, `${srv.baseUrl}/shell/1.0.0/remoteEntry.js`, 'url resolved in dry run')
				assert.deepEqual(srv.requests, [], 'no HTTP request issued in dry run')
				assert.equal((await fetch(`${srv.baseUrl}/shell/1.0.0/remoteEntry.js`)).status, 404, 'nothing uploaded in dry run')
			} finally {
				await srv.close()
			}
		})

		// constructor validation
		await check('HttpDeployTarget: requires a baseUrl', async () => {
			assert.throws(() => new HttpDeployTarget({}), /baseUrl is required/)
			assert.throws(() => new HttpDeployTarget({ baseUrl: '' }), /baseUrl is required/)
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
