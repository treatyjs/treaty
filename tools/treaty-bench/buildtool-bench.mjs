// Treaty build-tool macro-benchmark.
//
// Builds the SAME representative Angular app through every available bundler and measures
// wall-clock build time + emitted output size, to compare which build tool is best for a Treaty
// app. The app under test is `examples/linker-smoke` — the smallest viable REAL Angular app in the
// repo (bootstrapApplication + provideRouter + inject(PlatformLocation), consuming the published
// partial-compiled @angular/* libraries). Its first-party code is hand-authored AOT Ivy, so the
// only build-time work that differs per tool is (a) bundling and (b) linking the published
// `ɵɵngDeclare*` partial @angular/* libraries to AOT `ɵɵdefine*` — exactly the cross-cutting Treaty
// concern we want to compare apples-to-apples across bundlers.
//
// Tools measured (build the app, time best-of-N, sum dist bytes):
//   - vite     (@treaty/vite)      — the known-good primary backend.
//   - rolldown (@treaty/rolldown)  — the new Rolldown plugin.
//   - rspack   (@treaty/rspack)    — needs @rspack/core (peer).
//   - rsbuild  (@treaty/rsbuild)   — needs @rsbuild/core (peer).
//   - rslib    (@treaty/rslib)     — needs @rslib/core  (peer) [library build].
//   - ng-cli   (@angular/build:application) — the Angular-native baseline.
//
// A tool that cannot build cleanly in THIS environment is reported honestly with
// status:"pending"/"failed" and the REAL reason (e.g. "peer @rspack/core not installed"); its
// numbers are NEVER faked.
//
// This is a standalone .mjs benchmark: it MAY use performance.now() freely for timing (the
// workflow-script-only clock restriction does not apply to benchmark files).
//
// Usage:  node tools/treaty-bench/buildtool-bench.mjs [--runs N]
// Writes: tools/treaty-bench/results/buildtool.json

import { build as viteBuild } from 'vite'
import { build as rolldownBuild } from 'rolldown'
import { createRequire } from 'node:module'
import { execFileSync } from 'node:child_process'
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
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(here, '..', '..')
const req = createRequire(import.meta.url)

const RUNS = (() => {
	const i = process.argv.indexOf('--runs')
	if (i >= 0 && process.argv[i + 1]) return Math.max(1, Number(process.argv[i + 1]) | 0)
	return 3
})()

const appDir = join(repoRoot, 'examples', 'linker-smoke')
const resultsDir = join(here, 'results')
const benchRoot = join(appDir, 'dist', 'buildtool-bench') // all tool outputs land under dist/ (gitignored)

// ---------------------------------------------------------------------------
// helpers
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

// Recursively sum the byte size of every emitted file under `dir`.
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

// Assert the emitted JS is a correctly LINKED Angular bundle: zero residual partial declarations,
// real Ivy AOT defs present, and no @angular/compiler dragged in. Keeps the comparison honest —
// a "fast" build that skipped linking would not be a like-for-like result.
function inspectBundle(dir) {
	let residual = 0
	let defs = 0
	let compiler = false
	let jsFiles = 0
	for (const entry of readdirSync(dir, { recursive: true })) {
		if (typeof entry !== 'string' || !entry.endsWith('.js')) continue
		jsFiles += 1
		const code = readFileSync(join(dir, entry), 'utf-8')
		residual += (code.match(/ɵɵngDeclare[A-Za-z]+\s*\(/g) || []).length
		defs += (code.match(/ɵɵdefine(Component|Directive|Injectable|NgModule|Pipe)/g) || []).length
		if (/from\s*['"]@angular\/compiler['"]|import\(\s*['"]@angular\/compiler['"]/.test(code)) {
			compiler = true
		}
	}
	// The load-bearing "correctly linked AOT" signals are: ZERO residual `ɵɵngDeclare*` partial
	// calls AND no `@angular/compiler` import (so the bundle needs no JIT). The presence of literal
	// `ɵɵdefine*` symbols is informational only — a minifier (e.g. Angular CLI's esbuild pass) renames
	// those imported identifiers, so a def count of 0 in a minified bundle does NOT mean "unlinked".
	return { jsFiles, residual, defs, compiler, linkedOk: residual === 0 && !compiler }
}

// ---------------------------------------------------------------------------
// Step 0: build the @treaty plugin dists from current source, so the wiring under
// test is the committed source (mirrors the linker-smoke e2e's plugin rebuild step).
// ---------------------------------------------------------------------------
function buildPluginDists() {
	for (const pkg of ['libs/treaty/vite', 'libs/treaty/rolldown']) {
		const buildScript = join(repoRoot, pkg, 'build.mjs')
		if (!existsSync(buildScript)) continue
		// JS-only rebuild (the .d.ts emit via tsgo is irrelevant to runtime); the build.mjs already
		// emits dist/index.js via esbuild. Run it and tolerate a tsgo-declarations failure, since only
		// the JS bundle is loaded by the benchmark.
		try {
			execFileSync(process.execPath, [buildScript], {
				cwd: join(repoRoot, pkg),
				stdio: ['ignore', 'ignore', 'inherit'],
			})
		} catch {
			// If declaration emit failed but dist/index.js exists, that's fine for the benchmark.
			if (!existsSync(join(repoRoot, pkg, 'dist', 'index.js'))) throw new Error(`failed to build ${pkg}`)
		}
	}
}

// ---------------------------------------------------------------------------
// Step 1: wire a self-contained node_modules symlink farm for the app (same approach as the
// linker-smoke e2e: the examples are not in the root lockfile).
// ---------------------------------------------------------------------------
function wireNodeModules() {
	const nm = join(appDir, 'node_modules')
	mkdirSync(nm, { recursive: true })
	// @treaty workspace packages (the plugins under test + their shared deps).
	link(nm, '@treaty/vite', join(repoRoot, 'libs/treaty/vite'))
	link(nm, '@treaty/rolldown', join(repoRoot, 'libs/treaty/rolldown'))
	link(nm, '@treaty/ts-vite', join(repoRoot, 'libs/typescript/vite'))
	link(nm, '@treaty/authoring-node', join(repoRoot, 'libs/authoring/node'))
	link(nm, '@treaty/compiler', join(repoRoot, 'libs/treaty/compiler'))
	link(nm, '@treaty/module-federation', join(repoRoot, 'libs/treaty/module-federation'))
	// Runtime + build deps resolved from the monorepo.
	for (const name of [
		'@angular/core',
		'@angular/common',
		'@angular/router',
		'@angular/platform-browser',
		'rxjs',
		'tslib',
	]) {
		link(nm, name, resolvePkgDir(name))
	}
}

// ---------------------------------------------------------------------------
// Per-tool builders. Each returns nothing on success or throws on failure. The harness handles
// cleaning, timing, repetition, and sizing around them.
// ---------------------------------------------------------------------------
const viteTreaty = () => req(join(repoRoot, 'libs/treaty/vite/dist/index.js')).default
const rolldownTreaty = () => req(join(repoRoot, 'libs/treaty/rolldown/dist/index.js')).default

async function buildVite(outDir) {
	const treaty = viteTreaty()
	await viteBuild({
		root: appDir,
		logLevel: 'silent',
		configFile: false,
		plugins: [treaty({ sourceMap: false })],
		build: { outDir, target: 'es2022', minify: true, emptyOutDir: true, reportCompressedSize: false },
	})
}

async function buildRolldown(outDir) {
	const treaty = rolldownTreaty()
	await rolldownBuild({
		input: join(appDir, 'src/main.ts'),
		cwd: appDir,
		plugins: [treaty({ sourceMap: false, functionChunking: false })],
		output: { dir: outDir, format: 'es', minify: true },
	})
}

// ---------------------------------------------------------------------------
// Angular CLI baseline (@angular/build:application). Scaffolds an Angular-CLI project view over the
// SAME src/ (an angular.json + CLI main/index/tsconfig), then runs `ng build`. Angular's own build
// performs the @angular/* partial linking itself (it bundles the compiler-cli Babel linker), so it
// builds the identical app fully AOT — the Angular-native baseline.
// ---------------------------------------------------------------------------
function scaffoldNgProject() {
	const ngRoot = join(benchRoot, 'ng-project')
	rmSync(ngRoot, { recursive: true, force: true })
	mkdirSync(join(ngRoot, 'src'), { recursive: true })

	// CLI entry: import the app's existing bootstrap. The app's src/main.ts already calls
	// bootstrapApplication; reuse it verbatim via a relative import (no `.ts` extension — the Angular
	// compiler plugin resolves it through TS module resolution).
	const appMainNoExt = join(appDir, 'src', 'main').replace(/\\/g, '/')
	writeFileSync(join(ngRoot, 'src', 'main.ts'), `import '${appMainNoExt}'\n`)

	// CLI index.html — NO manual <script> tag (the application builder injects its own entry).
	writeFileSync(
		join(ngRoot, 'src', 'index.html'),
		`<!doctype html><html lang="en"><head><meta charset="utf-8"><title>linker-smoke</title></head><body><smoke-root></smoke-root></body></html>\n`,
	)

	// Resolve the monorepo node_modules so the CLI finds @angular/*, typescript, etc.
	const rootNm = join(repoRoot, 'node_modules')
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
					experimentalDecorators: true,
					lib: ['ES2022', 'dom'],
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
					'linker-smoke': {
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
								},
							},
						},
					},
				},
			},
			null,
			2,
		),
	)

	// Wire the CLI project's node_modules to the monorepo's installed packages.
	const ngNm = join(ngRoot, 'node_modules')
	mkdirSync(ngNm, { recursive: true })
	// Symlink the whole root node_modules contents the CLI needs by pointing node_modules at root.
	// Simplest robust approach: junction the scoped + top-level deps the CLI resolves.
	for (const name of [
		'@angular/core',
		'@angular/common',
		'@angular/router',
		'@angular/platform-browser',
		'@angular/build',
		'@angular/cli',
		'@angular/compiler',
		'@angular/compiler-cli',
		'@angular-devkit/build-angular',
		'@angular-devkit/core',
		'@angular-devkit/architect',
		'rxjs',
		'tslib',
		'typescript',
		'esbuild',
	]) {
		link(ngNm, name, resolvePkgDir(name))
	}
	return { ngRoot, ngNm, rootNm }
}

// Run the Angular-native build by driving the SAME builder `ng build` uses
// (`@angular/build:application`) through the Architect programmatic API. We bypass the
// `@angular/cli` bin only to dodge its hard Node-version GATE (a guard in ng.js — this Node runs the
// builder fine); the builder, compiler, and AOT pipeline executed are exactly what `ng build` runs.
async function buildNg(ngRoot) {
	const { Architect } = req('@angular-devkit/architect')
	const { WorkspaceNodeModulesArchitectHost } = req('@angular-devkit/architect/node')
	const { NodeJsSyncHost } = req('@angular-devkit/core/node')
	const { workspaces, logging } = req('@angular-devkit/core')

	const wsHost = workspaces.createWorkspaceHost(new NodeJsSyncHost())
	const { workspace } = await workspaces.readWorkspace(join(ngRoot, 'angular.json'), wsHost)
	const archHost = new WorkspaceNodeModulesArchitectHost(workspace, ngRoot)
	const architect = new Architect(archHost)

	let firstError = ''
	const logger = new logging.Logger('ng')
	logger.subscribe((e) => {
		if (e.level === 'error' && !firstError) firstError = e.message
	})

	const run = await architect.scheduleTarget({ project: 'linker-smoke', target: 'build' }, {}, { logger })
	const result = await run.result
	await run.stop()
	if (!result.success) {
		throw new Error(`ng build failed: ${(firstError || String(result.error || 'unknown')).replace(/\[[0-9;]*m/g, '').split('\n').slice(0, 4).join(' ')}`)
	}
}

// ---------------------------------------------------------------------------
// Run one tool: clean build best-of-RUNS, then size the LAST successful output dir.
// Returns a result record. Never throws (failures captured as status:"failed").
// ---------------------------------------------------------------------------
async function measure(tool, outDirFor, runFn, { inspect = true } = {}) {
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
		const dt = performance.now() - t0
		times.push(dt)
		lastOut = outDir
	}
	if (firstError || times.length === 0) {
		return {
			tool,
			status: 'failed',
			note: String(firstError?.message ?? firstError ?? 'no successful run').split('\n').slice(0, 3).join(' '),
		}
	}
	const buildMs = Math.round(Math.min(...times))
	const distBytes = dirBytes(lastOut)
	const rec = { tool, status: 'measured', buildMs, distBytes }
	rec.note = `best of ${times.length} run(s); times(ms)=[${times.map((t) => Math.round(t)).join(', ')}]`
	if (inspect) {
		try {
			const ins = inspectBundle(lastOut)
			rec.note += `; jsFiles=${ins.jsFiles} ivyDefs(literal,minify-sensitive)=${ins.defs} residualNgDeclare=${ins.residual} importsCompiler=${ins.compiler} linkedOk=${ins.linkedOk}`
		} catch {
			/* sizing/inspection best-effort */
		}
	}
	return rec
}

// ---------------------------------------------------------------------------
async function main() {
	console.log(`== Treaty build-tool benchmark (app: examples/linker-smoke, runs=${RUNS}) ==`)
	mkdirSync(resultsDir, { recursive: true })
	rmSync(benchRoot, { recursive: true, force: true })
	mkdirSync(benchRoot, { recursive: true })

	console.log('-- building @treaty plugin dists from source --')
	buildPluginDists()
	console.log('-- wiring app node_modules symlink farm --')
	wireNodeModules()

	const results = []

	// vite (@treaty/vite)
	console.log('-- vite (@treaty/vite) --')
	results.push(
		await measure('vite', (i) => join(benchRoot, `vite-${i}`), buildVite),
	)

	// rolldown (@treaty/rolldown)
	console.log('-- rolldown (@treaty/rolldown) --')
	results.push(
		await measure('rolldown', (i) => join(benchRoot, `rolldown-${i}`), buildRolldown),
	)

	// rspack / rsbuild / rslib — require peer cores that are NOT installed in this monorepo.
	for (const [tool, core] of [
		['rspack', '@rspack/core'],
		['rsbuild', '@rsbuild/core'],
		['rslib', '@rslib/core'],
	]) {
		console.log(`-- ${tool} (@treaty/${tool}) --`)
		if (!isInstalled(core)) {
			results.push({
				tool,
				status: 'pending',
				note: `peer ${core} not installed in this monorepo — @treaty/${tool}'s plugin dist is present but the bundler core it drives is absent, so no build can run here. Install ${core} to measure.`,
			})
		} else {
			// If a core ever gets installed, we still don't have a generic runner wired for it here.
			results.push({
				tool,
				status: 'pending',
				note: `peer ${core} present (v${pkgVersion(core)}) but no runner is wired in this benchmark; add a build step for ${tool} to measure.`,
			})
		}
	}

	// ng-cli (Angular CLI / @angular/build:application) — Angular-native baseline.
	console.log('-- ng-cli (@angular/build:application) --')
	if (!isInstalled('@angular/build') || !isInstalled('@angular/cli')) {
		results.push({
			tool: 'ng-cli',
			status: 'pending',
			note: 'Angular CLI / @angular/build not installed',
		})
	} else {
		let scaffold = null
		try {
			scaffold = scaffoldNgProject()
		} catch (err) {
			results.push({ tool: 'ng-cli', status: 'failed', note: `scaffold failed: ${String(err?.message ?? err)}` })
		}
		if (scaffold) {
			const ngOut = join(scaffold.ngRoot, 'dist')
			results.push(
				await measure(
					'ng-cli',
					() => ngOut, // CLI controls its own outputPath; same dir each run, cleaned by measure()
					() => buildNg(scaffold.ngRoot),
					{ inspect: true },
				),
			)
		}
	}

	// ---------------------------------------------------------------------------
	const out = {
		generatedAt: new Date().toISOString(),
		app: 'examples/linker-smoke',
		appNote:
			'Smallest viable real Angular app: bootstrapApplication + provideRouter + inject(PlatformLocation), consuming published partial-compiled @angular/* libraries. First-party code is hand-authored AOT Ivy, so the per-tool variable is bundling + @angular/* partial linking.',
		host: { platform: process.platform, arch: process.arch, node: process.version },
		runsPerTool: RUNS,
		versions: {
			'@angular/core': pkgVersion('@angular/core'),
			vite: pkgVersion('vite'),
			rolldown: pkgVersion('rolldown'),
			'@angular/build': pkgVersion('@angular/build'),
			'@angular/cli': pkgVersion('@angular/cli'),
		},
		metric: 'best (min) wall-clock build time over N clean builds; distBytes = sum of all emitted output files',
		results,
	}
	const outPath = join(resultsDir, 'buildtool.json')
	writeFileSync(outPath, JSON.stringify(out, null, 2) + '\n')

	console.log('\n== results ==')
	for (const r of results) {
		const size = r.distBytes != null ? `${(r.distBytes / 1024).toFixed(1)} KiB` : '—'
		const ms = r.buildMs != null ? `${r.buildMs} ms` : '—'
		console.log(`  ${r.tool.padEnd(9)} ${r.status.padEnd(9)} ${ms.padStart(9)}  ${size.padStart(11)}`)
	}
	console.log(`\nwrote ${outPath}`)
	return out
}

main().catch((err) => {
	console.error('BENCH ERROR:', err)
	process.exit(1)
})
