// Treaty CLI vs Angular CLI benchmark.
//
// Compares the two developer-facing CLIs on the SAME realistic standard-Angular app
// (`examples/ng-bench-app` — 6 components, a service layer, a pure pipe, an attribute directive, an
// eager route + 3 lazy routes), on the two operations a developer actually waits on:
//
//   (1) BUILD  — `treaty build` vs `ng build` (production):
//         * wall-clock build time (best of N clean builds),
//         * emitted dist size (sum of all output bytes),
//         * e2e-boot WORKS verdict (the emitted bundle is booted headlessly in jsdom and must render
//           the eager Dashboard route with no JIT / "@angular/compiler not available" error — a
//           fast-but-broken build is flagged FAIL, never rewarded).
//
//   (2) SERVE  — `treaty serve` vs `ng serve` (dev cold start):
//         * COLD START to first byte (time from kicking off the dev server in a cold process until it
//           answers the first HTTP request for `/`),
//         * for Treaty: time to COMPILE + SERVE the first component module (the dev server is asked
//           for the Dashboard component's `.ts` over HTTP; the Treaty plugin lowers that `@Component`
//           -> Ivy on demand and the elapsed transform time is measured — the dev-serve JIT gap this
//           migration closed). Angular's dev-server does a full app prebundle/compile BEFORE the first
//           byte, so it has no separable "first module compile" number; that asymmetry is reported
//           honestly rather than faked.
//
// WHAT IS DRIVEN — the REAL CLIs, not a re-implementation:
//   * Treaty build/serve drive the standalone Treaty CLI's OWN command functions
//     (`@treaty/cli` dist: `resolveConfig` + `runBuild` / `runDev`), i.e. the exact code path
//     `treaty build` / `treaty serve` execute (Vite default bundler + the Treaty plugin). Module
//     Federation is opted OUT (`moduleFederation:false`, a first-class CLI option) so the bench needs
//     no MF peer and stays a like-for-like app build.
//   * Angular build/serve drive `@angular/build:application` / `@angular/build:dev-server` through the
//     Architect programmatic API. This is what `ng build` / `ng serve` run; we bypass the `@angular/cli`
//     BIN only because it trips a hard Node-version floor (a guard in ng.js — this Node runs the builder
//     itself fine), exactly as the existing fullapp/buildtool benches do.
//
// This is a standalone .mjs benchmark: it MAY use performance.now() freely (the workflow-script clock
// restriction does not apply to benchmark files).
//
// Usage:  node tools/treaty-bench/cli-bench.mjs [--runs N] [--serve-runs M]
// Writes: tools/treaty-bench/results/cli.json

import { createRequire } from 'node:module'
import { execFileSync } from 'node:child_process'
import { request as httpRequest, createServer as createHttpServer } from 'node:http'
import {
	readFileSync,
	writeFileSync,
	readdirSync,
	existsSync,
	mkdirSync,
	rmSync,
	symlinkSync,
	lstatSync,
	statSync,
} from 'node:fs'
import { join, dirname } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(here, '..', '..')
const req = createRequire(import.meta.url)

const RUNS = (() => {
	const i = process.argv.indexOf('--runs')
	if (i >= 0 && process.argv[i + 1]) return Math.max(1, Number(process.argv[i + 1]) | 0)
	return 3
})()
// Cold-start serve is expensive (a fresh child process per measurement), so default to fewer runs.
const SERVE_RUNS = (() => {
	const i = process.argv.indexOf('--serve-runs')
	if (i >= 0 && process.argv[i + 1]) return Math.max(1, Number(process.argv[i + 1]) | 0)
	return 3
})()

const appDir = join(repoRoot, 'examples', 'ng-bench-app')
const appMain = join(appDir, 'src', 'main.ts')
// The first component module a developer's first navigation compiles: the eager Dashboard route's
// @Component. Requesting it through the dev server exercises on-demand @Component -> Ivy lowering.
const firstComponentRel = 'src/app/features/dashboard/dashboard.ts'
const resultsDir = process.env.TREATY_BENCH_RESULTS || join(here, 'results')
const benchRoot = join(appDir, 'dist', 'cli-bench') // all outputs land under dist/ (gitignored)
const appNodeModules = join(appDir, 'node_modules')

// ---------------------------------------------------------------------------
// helpers (shared shape with fullapp-bench.mjs)
// ---------------------------------------------------------------------------
function resolvePkgDir(name) {
	try {
		return dirname(req.resolve(`${name}/package.json`, { paths: [repoRoot] }))
	} catch {
		return null
	}
}
function isInstalled(name) {
	return Boolean(resolvePkgDir(name))
}
function pkgVersion(name) {
	const dir = resolvePkgDir(name)
	if (!dir) return null
	try {
		return JSON.parse(readFileSync(join(dir, 'package.json'), 'utf-8')).version ?? null
	} catch {
		return null
	}
}
function link(nodeModules, name, target) {
	if (!target) return false
	const dest = join(nodeModules, name)
	mkdirSync(dirname(dest), { recursive: true })
	if (existsSync(dest)) {
		try {
			const st = lstatSync(dest)
			if (st.isSymbolicLink() || st.isDirectory()) return true
		} catch {
			/* recreate */
		}
		rmSync(dest, { recursive: true, force: true })
	}
	symlinkSync(target, dest, 'junction')
	return true
}
function dirBytes(dir) {
	if (!existsSync(dir)) return 0
	let total = 0
	for (const entry of readdirSync(dir, { recursive: true })) {
		if (typeof entry !== 'string') continue
		const p = join(dir, entry)
		try {
			const st = statSync(p)
			if (st.isFile()) total += st.size
		} catch {
			/* ignore vanished temp files */
		}
	}
	return total
}

// Assert the emitted JS is correctly LINKED AOT: zero residual `ɵɵngDeclare*` partial calls and no
// `@angular/compiler` import (so the bundle needs no JIT). Informational — folded into the row note.
function inspectBundle(dir) {
	let residual = 0
	let compiler = false
	let jsFiles = 0
	for (const entry of readdirSync(dir, { recursive: true })) {
		if (typeof entry !== 'string' || !(entry.endsWith('.js') || entry.endsWith('.mjs'))) continue
		jsFiles += 1
		const code = readFileSync(join(dir, entry), 'utf-8')
		residual += (code.match(/ɵɵngDeclare[A-Za-z]+\s*\(/g) || []).length
		if (/from\s*['"]@angular\/compiler['"]|import\(\s*['"]@angular\/compiler['"]/.test(code)) {
			compiler = true
		}
	}
	return { jsFiles, residual, compiler, linkedOk: residual === 0 && !compiler }
}

// ---------------------------------------------------------------------------
// Step 0: build the @treaty plugin dists from current source, so the wiring under test is the
// committed source (vite + rolldown ship a build.mjs; the CLI uses vite).
// ---------------------------------------------------------------------------
function buildPluginDists() {
	for (const pkg of ['libs/treaty/vite']) {
		const buildScript = join(repoRoot, pkg, 'build.mjs')
		if (!existsSync(buildScript)) continue
		try {
			execFileSync(process.execPath, [buildScript], {
				cwd: join(repoRoot, pkg),
				stdio: ['ignore', 'ignore', 'inherit'],
			})
		} catch {
			if (!existsSync(join(repoRoot, pkg, 'dist', 'index.js'))) throw new Error(`failed to build ${pkg}`)
		}
	}
}

// ---------------------------------------------------------------------------
// Step 1: wire a self-contained node_modules symlink farm for the app (the examples are not in the
// root lockfile). Includes the @treaty workspace packages, the Angular runtime, and jsdom.
// ---------------------------------------------------------------------------
function wireNodeModules() {
	const nm = appNodeModules
	mkdirSync(nm, { recursive: true })
	link(nm, '@treaty/vite', join(repoRoot, 'libs/treaty/vite'))
	link(nm, '@treaty/cli', join(repoRoot, 'libs/treaty/cli'))
	link(nm, '@treaty/ts-vite', resolvePkgDir('@treaty/ts-vite'))
	link(nm, '@treaty/authoring-node', join(repoRoot, 'libs/authoring/node'))
	link(nm, '@treaty/compiler', join(repoRoot, 'libs/treaty/compiler'))
	link(nm, '@treaty/module-federation', join(repoRoot, 'libs/treaty/module-federation'))
	link(nm, '@treaty/rspack', join(repoRoot, 'libs/treaty/rspack'))
	for (const name of [
		'@angular/core',
		'@angular/common',
		'@angular/router',
		'@angular/forms',
		'@angular/platform-browser',
		'rxjs',
		'tslib',
		'jsdom',
		'vite',
	]) {
		link(nm, name, resolvePkgDir(name))
	}
}

// ---------------------------------------------------------------------------
// Treaty CLI driver — drive the standalone CLI's OWN command functions (the exact code path
// `treaty build` / `treaty serve` run). Module Federation is opted out so no MF peer is needed.
//
// The Treaty CLI follows its OWN project convention: a root `index.html` + `src/main.ts` entry (see
// libs/treaty/cli/src/config.ts). The fixture app is an Angular-CLI app (its index.html lives under
// src/), so — exactly as we scaffold an Angular-CLI project VIEW over the same source for `ng` — we
// scaffold a TREATY project view: a root `index.html` that loads `src/main.ts`, whose main re-imports
// the real app bootstrap by absolute path (Vite follows the absolute fs path into the app's own src/,
// so the WHOLE real app is compiled). Same source, each CLI given the layout IT expects.
// ---------------------------------------------------------------------------
const cliConfigUrl = pathToFileURL(join(repoRoot, 'libs/treaty/cli/dist/config.js')).href
const cliBuildUrl = pathToFileURL(join(repoRoot, 'libs/treaty/cli/dist/commands/build.js')).href
const cliDevUrl = pathToFileURL(join(repoRoot, 'libs/treaty/cli/dist/commands/dev.js')).href

// A treaty-project root (its own index.html + src/main.ts) viewing the same app source. Built once.
let _treatyRoot = null
function scaffoldTreatyProject() {
	if (_treatyRoot) return _treatyRoot
	const root = join(benchRoot, 'treaty-project')
	rmSync(root, { recursive: true, force: true })
	mkdirSync(join(root, 'src'), { recursive: true })
	// Root index.html (the Treaty convention): a module <script> to the entry, mounting <app-root>.
	writeFileSync(
		join(root, 'index.html'),
		`<!doctype html><html lang="en"><head><meta charset="utf-8"><base href="/"><title>ng-bench-app</title></head><body><app-root></app-root><script type="module" src="/src/main.ts"></script></body></html>\n`,
	)
	// Entry re-imports the real app bootstrap by absolute path; Vite follows it into the app's own src/.
	writeFileSync(join(root, 'src', 'main.ts'), `import '${appMain.replace(/\\/g, '/').replace(/\.ts$/, '')}'\n`)
	// Co-locate a node_modules so Vite + the Treaty plugin + @angular/* resolve from one place; reuse the
	// app's already-wired symlink farm by junctioning the whole dir.
	link(root, 'node_modules', appNodeModules)
	_treatyRoot = root
	return root
}

async function resolveTreatyConfig(overrides) {
	const { resolveConfig } = await import(cliConfigUrl)
	const root = scaffoldTreatyProject()
	const cfg = await resolveConfig(root, { root, ...overrides })
	// `resolveConfig` defaults moduleFederation:true; force it OFF for a like-for-like app build with
	// no MF peer. (A spread copy because ResolvedConfig fields are readonly.)
	return { ...cfg, moduleFederation: false }
}

async function treatyBuild(outDir) {
	const { runBuild } = await import(cliBuildUrl)
	const config = await resolveTreatyConfig({ outDir })
	await runBuild(config)
}

// ---------------------------------------------------------------------------
// Angular CLI driver — scaffold an Angular-CLI project view over the SAME src/, then drive
// `@angular/build:application` (build) / `@angular/build:dev-server` (serve) through Architect.
// ---------------------------------------------------------------------------
function scaffoldNgProject() {
	const ngRoot = join(benchRoot, 'ng-project')
	rmSync(ngRoot, { recursive: true, force: true })
	mkdirSync(join(ngRoot, 'src'), { recursive: true })

	writeFileSync(join(ngRoot, 'src', 'main.ts'), `import '${appMain.replace(/\\/g, '/').replace(/\.ts$/, '')}'\n`)
	writeFileSync(
		join(ngRoot, 'src', 'index.html'),
		`<!doctype html><html lang="en"><head><meta charset="utf-8"><base href="/"><title>ng-bench-app</title></head><body><app-root></app-root></body></html>\n`,
	)
	writeFileSync(
		join(ngRoot, 'tsconfig.json'),
		JSON.stringify(
			{
				compilerOptions: {
					target: 'ES2022',
					module: 'ESNext',
					moduleResolution: 'bundler',
					strict: false,
					skipLibCheck: true,
					experimentalDecorators: false,
					importHelpers: true,
					lib: ['ES2022', 'dom', 'dom.iterable'],
					types: [],
				},
				angularCompilerOptions: { strictTemplates: false },
				files: ['src/main.ts'],
			},
			null,
			2,
		),
	)
	writeFileSync(
		join(ngRoot, 'angular.json'),
		JSON.stringify(
			{
				$schema: './node_modules/@angular/cli/lib/config/schema.json',
				version: 1,
				newProjectRoot: 'projects',
				projects: {
					'ng-bench-app': {
						projectType: 'application',
						root: '',
						sourceRoot: 'src',
						architect: {
							build: {
								builder: '@angular/build:application',
								options: {
									outputPath: 'dist',
									index: 'src/index.html',
									browser: 'src/main.ts',
									tsConfig: 'tsconfig.json',
									optimization: true,
									aot: true,
									sourceMap: false,
									namedChunks: false,
									progress: false,
									outputHashing: 'none',
								},
							},
							serve: {
								builder: '@angular/build:dev-server',
								options: { buildTarget: 'ng-bench-app:build' },
							},
						},
					},
				},
			},
			null,
			2,
		),
	)

	const ngNm = join(ngRoot, 'node_modules')
	mkdirSync(ngNm, { recursive: true })
	for (const name of [
		'@angular/core',
		'@angular/common',
		'@angular/router',
		'@angular/forms',
		'@angular/platform-browser',
		'@angular/build',
		'@angular/cli',
		'@angular/compiler',
		'@angular/compiler-cli',
		'@angular-devkit/core',
		'@angular-devkit/architect',
		'rxjs',
		'tslib',
		'typescript',
		'esbuild',
		'vite',
	]) {
		link(ngNm, name, resolvePkgDir(name))
	}
	return { ngRoot }
}

// Open an Architect against a scaffolded ng project root.
async function openArchitect(ngRoot) {
	const { Architect } = req('@angular-devkit/architect')
	const { WorkspaceNodeModulesArchitectHost } = req('@angular-devkit/architect/node')
	const { NodeJsSyncHost } = req('@angular-devkit/core/node')
	const { workspaces, logging } = req('@angular-devkit/core')

	const wsHost = workspaces.createWorkspaceHost(new NodeJsSyncHost())
	const { workspace } = await workspaces.readWorkspace(join(ngRoot, 'angular.json'), wsHost)
	const archHost = new WorkspaceNodeModulesArchitectHost(workspace, ngRoot)
	const architect = new Architect(archHost)
	return { architect, logging }
}

async function ngBuild(ngRoot) {
	const { architect, logging } = await openArchitect(ngRoot)
	let firstError = ''
	const logger = new logging.Logger('ng')
	logger.subscribe((e) => {
		if (e.level === 'error' && !firstError) firstError = e.message
	})
	const run = await architect.scheduleTarget({ project: 'ng-bench-app', target: 'build' }, {}, { logger })
	const result = await run.result
	await run.stop()
	if (!result.success) {
		throw new Error(
			`ng build failed: ${(firstError || String(result.error || 'unknown')).replace(/\[[0-9;]*m/g, '').split('\n').slice(0, 4).join(' ')}`,
		)
	}
}

// ---------------------------------------------------------------------------
// HTTP helper — GET a URL, resolving on first response with the time-to-first-byte (ms from the GET
// being issued to the first response byte/headers) and the body. Used to probe a running dev server.
// ---------------------------------------------------------------------------
// Grab a currently-free TCP port by binding an ephemeral listener and reading the assigned port, then
// releasing it. We pass an EXPLICIT port to the Treaty CLI because its RunningServer.url echoes
// `config.port` rather than the underlying Vite server's actual bound port (so port:0 would surface as
// ":0"); an explicit free port keeps the returned URL correct.
function freePort() {
	return new Promise((resolve, reject) => {
		const srv = createHttpServer()
		srv.on('error', reject)
		srv.listen(0, '127.0.0.1', () => {
			const port = srv.address().port
			srv.close(() => resolve(port))
		})
	})
}

function httpGet(url, timeoutMs = 60000) {
	return new Promise((resolve, reject) => {
		const t0 = performance.now()
		const reqObj = httpRequest(url, { method: 'GET' }, (res) => {
			const ttfb = performance.now() - t0
			let body = ''
			res.setEncoding('utf8')
			res.on('data', (c) => {
				body += c
			})
			res.on('end', () => resolve({ status: res.statusCode, ttfb, body, totalMs: performance.now() - t0 }))
		})
		reqObj.setTimeout(timeoutMs, () => {
			reqObj.destroy(new Error(`GET ${url} timed out after ${timeoutMs}ms`))
		})
		reqObj.on('error', reject)
		reqObj.end()
	})
}

// ---------------------------------------------------------------------------
// SERVE measurement — Treaty.
//
// Cold start: this whole .mjs is the "cold process" for the FIRST run; for repeat runs we still start
// a brand-new Vite dev server (a fresh module graph + plugin container) so each measurement is a cold
// server start, then tear it down. We measure:
//   * coldToFirstByteMs: from kicking off `runDev` (the CLI's `treaty serve`) to the first byte of the
//     response to `GET /` (index.html).
//   * firstModuleCompileMs: from issuing `GET /<firstComponentRel>` (which makes Vite pull that module
//     through the Treaty plugin's transform — lowering the @Component to Ivy on demand) to its first
//     byte. This is the dev-serve "compile + serve the first component module" number.
// ---------------------------------------------------------------------------
async function serveTreatyOnce() {
	const { runDev } = await import(cliDevUrl)
	// A fresh, explicit free port per server so repeat cold starts never collide AND the CLI's returned
	// URL (which echoes config.port) is correct.
	const port = await freePort()
	const config = await resolveTreatyConfig({ port, host: '127.0.0.1' })
	const t0 = performance.now()
	const server = await runDev(config)
	const startedMs = performance.now() - t0 // server listening (post-createServer + listen)

	const url = server.url
	const base = url.replace(/\/$/, '')
	try {
		// `GET /` — cold start to first byte (the root index.html served by Vite's HTML middleware). With
		// the scaffolded treaty-project root this is a 200 SPA index; the metric is server-readiness +
		// first round-trip in a cold start.
		const t1 = performance.now()
		const idx = await httpGet(base + '/')
		const coldToFirstByteMs = performance.now() - t1 + startedMs

		// First component module compile: the eager Dashboard @Component lives in the app's own src/
		// (OUTSIDE the treaty-project root), so Vite serves it via the `/@fs/<abs>` path. Requesting it
		// pulls the module through the Treaty plugin transform, lowering the @Component -> Ivy on demand.
		// Time to first byte = compile + serve the first component module (the dev-serve JIT path).
		const firstAbs = join(appDir, firstComponentRel).replace(/\\/g, '/')
		const modUrl = base + '/@fs/' + firstAbs.replace(/^\//, '')
		const mod = await httpGet(modUrl)
		const firstModuleCompileMs = mod.ttfb
		const moduleLooksCompiled = /ɵɵdefineComponent/.test(mod.body)

		await closeWithTimeout(server)
		return {
			ok: true,
			startedMs,
			coldToFirstByteMs,
			indexStatus: idx.status,
			firstModuleCompileMs,
			moduleStatus: mod.status,
			moduleBytes: mod.body.length,
			moduleLooksCompiled,
		}
	} catch (err) {
		try {
			await closeWithTimeout(server)
		} catch {
			/* ignore */
		}
		throw err
	}
}

// Close a Vite dev server, but never hang the bench on it: Vite's `close()` can keep lingering
// keep-alive sockets/HMR ws handles open, so race it against a short timeout and move on.
function closeWithTimeout(server, ms = 4000) {
	return Promise.race([
		Promise.resolve(server.close()).catch(() => {}),
		new Promise((r) => setTimeout(r, ms)),
	])
}

// ---------------------------------------------------------------------------
// SERVE measurement — Angular dev-server (`@angular/build:dev-server`).
//
// Schedule the dev-server target through Architect; it emits a result carrying `baseUrl`/`port` once
// it is listening (after the initial prebundle/compile). We measure cold-start-to-first-byte: from
// scheduling the target to the first byte of `GET /` against that URL. Angular's dev-server compiles
// the whole app BEFORE serving the first byte, so there is no separable "first component module
// compile" number — reported honestly as N/A rather than faked.
// ---------------------------------------------------------------------------
async function serveNgOnce(ngRoot) {
	const { architect, logging } = await openArchitect(ngRoot)
	const logger = new logging.Logger('ng-serve')
	let firstError = ''
	logger.subscribe((e) => {
		if (e.level === 'error' && !firstError) firstError = e.message
	})

	const t0 = performance.now()
	const run = await architect.scheduleTarget(
		{ project: 'ng-bench-app', target: 'serve' },
		{ port: 0, host: '127.0.0.1' },
		{ logger },
	)

	// The dev-server builder yields results as it (re)builds; the first one carrying a URL means it is
	// listening. Wait for that result with a generous timeout.
	const listenInfo = await new Promise((resolve, reject) => {
		const timer = setTimeout(() => reject(new Error('ng dev-server did not report a URL within 120s')), 120000)
		const sub = run.output.subscribe({
			next: (out) => {
				const baseUrl = out.baseUrl || (out.info && out.info.baseUrl) || null
				const port = out.port || (out.info && out.info.port) || null
				if (out.success && (baseUrl || port)) {
					clearTimeout(timer)
					sub.unsubscribe()
					resolve({ baseUrl, port, listeningMs: performance.now() - t0 })
				} else if (out.success === false && firstError) {
					clearTimeout(timer)
					sub.unsubscribe()
					reject(new Error(`ng dev-server build failed: ${firstError.replace(/\[[0-9;]*m/g, '').split('\n').slice(0, 3).join(' ')}`))
				}
			},
			error: reject,
		})
	})

	const url = listenInfo.baseUrl || `http://127.0.0.1:${listenInfo.port}/`
	try {
		const t1 = performance.now()
		const idx = await httpGet(url.replace(/\/$/, '') + '/')
		const coldToFirstByteMs = performance.now() - t1 + listenInfo.listeningMs
		await run.stop()
		return { ok: true, listeningMs: listenInfo.listeningMs, coldToFirstByteMs, indexStatus: idx.status, url }
	} catch (err) {
		try {
			await run.stop()
		} catch {
			/* ignore */
		}
		throw err
	}
}

// ---------------------------------------------------------------------------
// e2e-of-output (build) — boot one tool's emitted dist headlessly; return the WORKS verdict.
// ---------------------------------------------------------------------------
const bootScript = join(here, 'boot-fullapp.mjs')

function findBootEntry(distDir) {
	if (!distDir || !existsSync(distDir)) return null
	const jsFiles = []
	const htmlFiles = []
	for (const entry of readdirSync(distDir, { recursive: true })) {
		if (typeof entry !== 'string') continue
		if (entry.endsWith('.js') || entry.endsWith('.mjs')) jsFiles.push(entry)
		else if (entry.endsWith('.html')) htmlFiles.push(entry)
	}
	if (jsFiles.length === 0) return null
	const orderedHtml = [
		...htmlFiles.filter((h) => /(^|[\\/])index\.html$/.test(h)),
		...htmlFiles.filter((h) => !/(^|[\\/])index\.html$/.test(h)),
	]
	for (const htmlRel of orderedHtml) {
		const htmlDir = dirname(join(distDir, htmlRel))
		const html = readFileSync(join(distDir, htmlRel), 'utf-8')
		const matches = [...html.matchAll(/<script[^>]*type=["']module["'][^>]*\bsrc=["']([^"']+)["']/gi)]
		for (const m of matches.reverse()) {
			const src = m[1].replace(/^\//, '')
			const candidate = existsSync(join(htmlDir, src)) ? join(htmlDir, src) : join(distDir, src)
			if (existsSync(candidate)) return candidate
		}
	}
	const mainEntry = jsFiles.find(
		(f) => !/(^|[\\/])async[\\/]/i.test(f) && /(^|[\\/])main([.-][^\\/]*)?\.m?js$/i.test(f),
	)
	if (mainEntry) return join(distDir, mainEntry)
	return join(distDir, jsFiles[0])
}

function bootDist(tool, distDir) {
	const entry = findBootEntry(distDir)
	if (!entry) {
		return { tool, works: 'SKIPPED', reason: `no bootable JS entry found in dist (${distDir ?? 'no dist'})` }
	}
	let stdout = ''
	try {
		stdout = execFileSync(process.execPath, [bootScript, tool, entry, appNodeModules], {
			encoding: 'utf-8',
			stdio: ['ignore', 'pipe', 'inherit'],
			timeout: 120000,
		})
	} catch (err) {
		stdout = String(err?.stdout ?? '')
		const tail = String(err?.message ?? err).split('\n').slice(0, 2).join(' ')
		if (!/BOOT_RESULT:/.test(stdout)) {
			return { tool, works: 'FAIL', reason: `boot child process crashed: ${tail}`, entry }
		}
	}
	const line = stdout.split('\n').find((l) => l.startsWith('BOOT_RESULT:'))
	if (!line) return { tool, works: 'FAIL', reason: 'boot probe produced no BOOT_RESULT verdict', entry }
	try {
		const verdict = JSON.parse(line.slice('BOOT_RESULT:'.length))
		return { ...verdict, entry }
	} catch (e) {
		return { tool, works: 'FAIL', reason: `unparseable boot verdict: ${String(e?.message ?? e)}`, entry }
	}
}

// ---------------------------------------------------------------------------
// measure one BUILD: clean build best-of-RUNS, size the last output, boot it.
// ---------------------------------------------------------------------------
async function measureBuild(tool, outDirFor, runFn) {
	const times = []
	let lastOut = null
	let firstError = null
	for (let i = 0; i < RUNS; i++) {
		const outDir = outDirFor(i)
		rmSync(outDir, { recursive: true, force: true })
		const t0 = performance.now()
		try {
			await runFn(outDir)
		} catch (err) {
			firstError = err
			break
		}
		times.push(performance.now() - t0)
		lastOut = outDir
	}
	if (firstError || times.length === 0) {
		return {
			cli: tool,
			op: 'build',
			status: 'failed',
			note: String(firstError?.message ?? firstError ?? 'no successful run').split('\n').slice(0, 4).join(' '),
		}
	}
	const buildMs = Math.round(Math.min(...times))
	const distBytes = dirBytes(lastOut)
	const rec = { cli: tool, op: 'build', status: 'measured', buildMs, distBytes }
	Object.defineProperty(rec, 'distDir', { value: lastOut, enumerable: false })
	rec.note = `best of ${times.length} clean build(s); times(ms)=[${times.map((t) => Math.round(t)).join(', ')}]`
	try {
		const ins = inspectBundle(lastOut)
		rec.note += `; jsFiles=${ins.jsFiles} residualNgDeclare=${ins.residual} importsCompiler=${ins.compiler} linkedOk=${ins.linkedOk}`
	} catch {
		/* best-effort */
	}
	return rec
}

// ---------------------------------------------------------------------------
// measure SERVE best-of-M: run the per-CLI once-fn M times, keep the min cold-start. The once-fn
// itself starts + tears down a fresh dev server each call.
// ---------------------------------------------------------------------------
async function measureServe(cli, onceFn) {
	const runs = []
	let firstError = null
	for (let i = 0; i < SERVE_RUNS; i++) {
		try {
			runs.push(await onceFn())
		} catch (err) {
			firstError = err
			break
		}
	}
	if (runs.length === 0) {
		return {
			cli,
			op: 'serve',
			status: 'failed',
			note: String(firstError?.message ?? firstError ?? 'no successful run').split('\n').slice(0, 4).join(' '),
		}
	}
	const best = (key) => Math.round(Math.min(...runs.map((r) => r[key]).filter((v) => typeof v === 'number')))
	const rec = {
		cli,
		op: 'serve',
		status: 'measured',
		coldToFirstByteMs: best('coldToFirstByteMs'),
	}
	if (runs.some((r) => typeof r.firstModuleCompileMs === 'number')) {
		rec.firstModuleCompileMs = best('firstModuleCompileMs')
	}
	const sample = runs[runs.length - 1]
	rec.note =
		`best of ${runs.length} cold start(s); coldToFirstByte(ms)=[${runs.map((r) => Math.round(r.coldToFirstByteMs)).join(', ')}]` +
		(rec.firstModuleCompileMs != null
			? `; firstModuleCompile(ms)=[${runs.map((r) => Math.round(r.firstModuleCompileMs ?? NaN)).join(', ')}]`
			: '') +
		(sample.indexStatus != null ? `; GET / -> ${sample.indexStatus}` : '') +
		(sample.moduleStatus != null
			? `; GET ${firstComponentRel} -> ${sample.moduleStatus} (${sample.moduleBytes}B, compiled=${sample.moduleLooksCompiled})`
			: '')
	if (firstError) rec.note += `; subsequent run failed: ${String(firstError.message ?? firstError).split('\n')[0]}`
	return rec
}

// ---------------------------------------------------------------------------
async function main() {
	console.log(`== Treaty CLI vs Angular CLI benchmark (app: examples/ng-bench-app, build-runs=${RUNS}, serve-runs=${SERVE_RUNS}) ==`)
	mkdirSync(resultsDir, { recursive: true })
	rmSync(benchRoot, { recursive: true, force: true })
	mkdirSync(benchRoot, { recursive: true })

	console.log('-- building @treaty/vite plugin dist from source --')
	buildPluginDists()
	console.log('-- wiring app node_modules symlink farm --')
	wireNodeModules()

	const haveNg = isInstalled('@angular/build') && isInstalled('@angular-devkit/architect')
	let ngScaffold = null
	if (haveNg) {
		try {
			ngScaffold = scaffoldNgProject()
		} catch (err) {
			console.error('ng scaffold failed:', err)
		}
	}

	const build = []
	const serve = []

	// ---- BUILD: treaty build vs ng build ----
	console.log('\n-- BUILD: treaty build (treaty CLI: vite + Treaty plugin, MF off) --')
	const treatyBuildRec = await measureBuild('treaty', (i) => join(benchRoot, `treaty-build-${i}`), treatyBuild)
	build.push(treatyBuildRec)

	console.log('-- BUILD: ng build (@angular/build:application via Architect) --')
	if (!ngScaffold) {
		build.push({
			cli: 'ng',
			op: 'build',
			status: 'failed',
			note: haveNg ? 'ng project scaffold failed' : '@angular/build or @angular-devkit/architect not installed',
		})
	} else {
		const ngOut = join(ngScaffold.ngRoot, 'dist')
		build.push(await measureBuild('ng', () => ngOut, () => ngBuild(ngScaffold.ngRoot)))
	}

	// ---- BUILD e2e-of-output: boot each emitted dist ----
	console.log('\n-- e2e-of-output: booting each built dist headlessly (jsdom) --')
	for (const r of build) {
		if (r.status !== 'measured') {
			r.works = 'SKIPPED'
			r.worksReason = `not booted: build status="${r.status}" (${r.note ?? 'no dist produced'})`
			continue
		}
		const verdict = bootDist(r.cli, r.distDir)
		r.works = verdict.works
		r.worksReason = verdict.reason
		if (verdict.statCards != null) r.statCards = verdict.statCards
		if (verdict.navLinks != null) r.navLinks = verdict.navLinks
		console.log(`  ${r.cli.padEnd(8)} works=${String(verdict.works).padEnd(7)} ${verdict.reason}`)
	}

	// ---- SERVE: treaty serve vs ng serve ----
	console.log('\n-- SERVE: treaty serve (cold start to first byte + first component module compile) --')
	serve.push(await measureServe('treaty', serveTreatyOnce))

	console.log('-- SERVE: ng serve (@angular/build:dev-server via Architect; cold start to first byte) --')
	if (!ngScaffold) {
		serve.push({
			cli: 'ng',
			op: 'serve',
			status: 'failed',
			note: haveNg ? 'ng project scaffold failed' : '@angular/build or @angular-devkit/architect not installed',
		})
	} else {
		const ngServeRec = await measureServe('ng', () => serveNgOnce(ngScaffold.ngRoot))
		// Angular's dev-server prebundles/compiles the whole app before the first byte, so there is no
		// separable "first component module compile" number. Report that honestly.
		ngServeRec.firstModuleCompileNote =
			'N/A — the Angular dev-server (@angular/build:dev-server) prebundles + compiles the whole app BEFORE serving the first byte, so per-module on-demand compile is not separately observable. The cold-start-to-first-byte already INCLUDES the full app compile.'
		serve.push(ngServeRec)
	}

	// ---------------------------------------------------------------------------
	const out = {
		generatedAt: new Date().toISOString(),
		app: 'examples/ng-bench-app',
		appNote:
			'Realistic standard-Angular application (6 components, a service layer, a pure pipe, an attribute directive, an eager route + 3 lazy routes). Pure @Component/@Directive/@Pipe .ts — zero Treaty-only features — so the SAME app drives both CLIs.',
		host: { platform: process.platform, arch: process.arch, node: process.version },
		buildRunsPerCli: RUNS,
		serveRunsPerCli: SERVE_RUNS,
		versions: {
			'@angular/core': pkgVersion('@angular/core'),
			'@angular/build': pkgVersion('@angular/build'),
			'@angular/cli': pkgVersion('@angular/cli'),
			vite: pkgVersion('vite'),
		},
		drivers: {
			treaty:
				"the standalone Treaty CLI's own command functions (@treaty/cli dist resolveConfig + runBuild/runDev), i.e. the exact `treaty build` / `treaty serve` code path (Vite bundler + Treaty plugin). Module Federation opted out (moduleFederation:false) for a like-for-like app build with no MF peer.",
			ng: 'the Angular builders `@angular/build:application` (build) and `@angular/build:dev-server` (serve) driven through the Architect programmatic API — exactly what `ng build` / `ng serve` run; only the @angular/cli BIN is bypassed (it trips a hard Node-version floor), not the builder.',
		},
		metrics: {
			build: 'best (min) wall-clock build time over N clean builds; distBytes = sum of all emitted output files; works = headless jsdom boot verdict (eager Dashboard route renders, no JIT / @angular/compiler error).',
			serve: 'coldToFirstByteMs = time from kicking off the dev server in a cold start to the first byte of GET /; firstModuleCompileMs (Treaty only) = time to compile + serve the first component module (GET of the Dashboard @Component .ts, lowered to Ivy on demand by the Treaty plugin). Angular has no separable first-module number (it compiles the whole app before first byte).',
		},
		build,
		serve,
	}
	const outPath = join(resultsDir, 'cli.json')
	writeFileSync(outPath, JSON.stringify(out, null, 2) + '\n')

	console.log('\n== BUILD results ==')
	for (const r of build) {
		const size = r.distBytes != null ? `${(r.distBytes / 1024).toFixed(1)} KiB` : '—'
		const ms = r.buildMs != null ? `${r.buildMs} ms` : '—'
		console.log(`  ${r.cli.padEnd(8)} ${r.status.padEnd(9)} ${ms.padStart(9)}  ${size.padStart(11)}  works=${r.works ?? '—'}`)
	}
	console.log('\n== SERVE results ==')
	for (const r of serve) {
		const cold = r.coldToFirstByteMs != null ? `${r.coldToFirstByteMs} ms` : '—'
		const fm = r.firstModuleCompileMs != null ? `${r.firstModuleCompileMs} ms` : 'N/A'
		console.log(`  ${r.cli.padEnd(8)} ${r.status.padEnd(9)} cold->firstByte=${cold.padStart(9)}  firstModule=${fm.padStart(9)}`)
	}
	console.log(`\nwrote ${outPath}`)
	return out
}

main().catch((err) => {
	console.error('BENCH ERROR:', err)
	process.exit(1)
})
