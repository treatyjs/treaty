// Treaty FULL-APP build-tool benchmark.
//
// Builds the SAME realistic, standard-Angular application (`examples/ng-bench-app` — 6 components, a
// service layer, a pipe, an attribute directive, an eager route + three lazy routes) through EVERY
// build tool Treaty ships a plugin for, plus Angular's own builder, and measures wall-clock build
// time + emitted dist size at MATCHED production optimization, then e2e-BOOTs each output headlessly
// (jsdom) to a PASS/FAIL `works` verdict.
//
// Unlike `buildtool-bench.mjs` (which builds the tiny hand-authored-Ivy `linker-smoke` app, so the
// only per-tool variable is bundling + @angular/* partial linking), THIS benchmark builds a real
// `@Component`/`@Directive`/`@Pipe` `.ts` app — so it exercises the FULL Treaty compile path
// (decorator lowering + template codegen) through each bundler, not just the linker.
//
// Tools (build the app, time best-of-N, sum dist bytes, then boot the output):
//   - vite     (@treaty/vite)      — Rollup-based, the known-good primary backend.
//   - rspack   (@treaty/rspack)    — @rspack/core + TreatyRspackPlugin + builtin:swc-loader (TS).
//   - rsbuild  (@treaty/rsbuild)   — @rsbuild/core + pluginTreaty (api.transform path).
//   - rslib    (@treaty/rslib)     — @rslib/core library build (see the rslib note below).
//   - rolldown (@treaty/rolldown)  — the Rolldown plugin.
//   - ng       (@angular/build:application) — Angular-native baseline, driven via the Architect API
//                                    (the @angular/cli bin trips a Node-version floor; the builder
//                                    itself runs fine — same approach the ng-bench-app verification
//                                    used).
//
// MATCHED OPTIMIZATION: every tool builds in PRODUCTION mode with minify + tree-shake ON, so the
// dist sizes are apples-to-apples. The one tool that is NOT a like-for-like app-size comparison is
// `rslib` — it is a LIBRARY builder and by design EXTERNALIZES `@angular/*` (a library never bundles
// its peer framework). So its dist excludes the Angular runtime: smaller bytes, and it boots only
// because the externalized `@angular/*` bare imports resolve (pre-linked to AOT) from the symlink
// farm at import time. That divergence is reported on its row, never hidden.
//
// e2e-of-output: a fast build is worthless if it ships output that does not run. After timing+sizing,
// every tool's dist is BOOTED headlessly (jsdom, a fresh child process per tool, reusing the proven
// linker-smoke e2e Step-4 boot pattern via `boot-fullapp.mjs`) and assigned a `works` verdict by
// asserting (a) bootstrap throws no JIT / "@angular/compiler not available" error and (b) the eager
// Dashboard route renders (<h2>Inventory dashboard</h2>) — plus a component-resolution completeness
// signal (how many <stat-card> children + nav links rendered). A fast build with works:"FAIL" is
// flagged, never rewarded.
//
// This is a standalone .mjs benchmark: it MAY use performance.now() freely.
//
// Usage:  node tools/treaty-bench/fullapp-bench.mjs [--runs N]
// Writes: tools/treaty-bench/results/fullapp.json

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
	cpSync,
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

const appDir = join(repoRoot, 'examples', 'ng-bench-app')
const appMain = join(appDir, 'src', 'main.ts')
const resultsDir = join(here, 'results')
const benchRoot = join(appDir, 'dist', 'fullapp-bench') // all tool outputs land under dist/ (gitignored)
const appNodeModules = join(appDir, 'node_modules') // jsdom + @angular/* symlink farm

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
// Step 0: build the @treaty plugin dists from current source.
// vite + rolldown ship a build.mjs; rspack/rsbuild/rslib are esbuild-bundled per-file (JS only — the
// .d.ts emit is irrelevant to the runtime benchmark). The committed source is what is measured.
// ---------------------------------------------------------------------------
function buildPluginDists() {
	for (const pkg of ['libs/treaty/vite', 'libs/treaty/rolldown']) {
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
	// rspack/rsbuild/rslib: esbuild each src/*.ts -> dist/*.js (esm, packages external).
	const esbuild = req.resolve('esbuild/bin/esbuild', { paths: [repoRoot] })
	for (const pkg of ['libs/treaty/rspack', 'libs/treaty/rsbuild', 'libs/treaty/rslib']) {
		const srcDir = join(repoRoot, pkg, 'src')
		const distDir = join(repoRoot, pkg, 'dist')
		mkdirSync(distDir, { recursive: true })
		for (const f of readdirSync(srcDir)) {
			if (!f.endsWith('.ts') || f.endsWith('.d.ts') || f.endsWith('.spec.ts')) continue
			const base = f.slice(0, -3)
			execFileSync(
				process.execPath,
				[
					esbuild,
					join(srcDir, f),
					'--format=esm',
					'--platform=node',
					'--target=node20',
					'--packages=external',
					`--outfile=${join(distDir, base + '.js')}`,
				],
				{ stdio: ['ignore', 'ignore', 'inherit'] },
			)
		}
	}
}

// ---------------------------------------------------------------------------
// Step 1: wire a self-contained node_modules symlink farm for the app (the examples are not in the
// root lockfile). Includes the @treaty workspace packages, the Angular runtime + Architect builder
// deps, and jsdom (for the boot layer).
// ---------------------------------------------------------------------------
function wireNodeModules() {
	const nm = appNodeModules
	mkdirSync(nm, { recursive: true })
	// @treaty workspace packages the plugins resolve.
	link(nm, '@treaty/vite', join(repoRoot, 'libs/treaty/vite'))
	link(nm, '@treaty/rolldown', join(repoRoot, 'libs/treaty/rolldown'))
	link(nm, '@treaty/rspack', join(repoRoot, 'libs/treaty/rspack'))
	link(nm, '@treaty/rsbuild', join(repoRoot, 'libs/treaty/rsbuild'))
	link(nm, '@treaty/rslib', join(repoRoot, 'libs/treaty/rslib'))
	link(nm, '@treaty/ts-vite', resolvePkgDir('@treaty/ts-vite'))
	link(nm, '@treaty/authoring-node', join(repoRoot, 'libs/authoring/node'))
	link(nm, '@treaty/compiler', join(repoRoot, 'libs/treaty/compiler'))
	link(nm, '@treaty/module-federation', join(repoRoot, 'libs/treaty/module-federation'))
	for (const name of [
		'@angular/core',
		'@angular/common',
		'@angular/router',
		'@angular/forms',
		'@angular/platform-browser',
		'rxjs',
		'tslib',
		'jsdom',
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
		// `selectorRoot: true` scans the app's first-party `.ts` ONCE at buildStart so the conventional
		// non-folding child selector (`class StatCard` ↔ `app-stat-card`) resolves cross-file — the
		// Dashboard then instantiates 3 real <app-stat-card> children instead of 3 empty hosts.
		plugins: [treaty({ sourceMap: false, selectorRoot: true })],
		build: {
			outDir,
			target: 'es2022',
			minify: true,
			emptyOutDir: true,
			reportCompressedSize: false,
			rollupOptions: { input: appMain },
		},
	})
}

async function buildRolldown(outDir) {
	const treaty = rolldownTreaty()
	await rolldownBuild({
		input: appMain,
		cwd: appDir,
		// `selectorRoot` scans the app's `.ts` so the conventional non-folding child selector
		// (`class StatCard` ↔ `app-stat-card`) resolves cross-file (see the Vite build note).
		plugins: [treaty({ sourceMap: false, functionChunking: false, selectorRoot: appDir })],
		output: { dir: outDir, format: 'es', minify: true },
	})
}

// rspack: @rspack/core programmatic build. The Treaty rspack plugin lowers `@Component`/`@Directive`/
// `@Pipe` `.ts` to Ivy and links the @angular/* partials; `builtin:swc-loader` transpiles the
// remaining TS (the Treaty loader emits TS, exactly as the Vite/esbuild path does). publicPath is set
// to '' so the runtime does not attempt the document.currentScript auto-detect (which jsdom rejects).
// moduleFederation:false keeps it a plain app build (the MF peer is optional).
async function buildRspack(outDir) {
	const { rspack } = req('@rspack/core')
	const { TreatyRspackPlugin } = req(join(repoRoot, 'libs/treaty/rspack/dist/plugin.js'))
	const treatyLoaderPath = join(repoRoot, 'libs/treaty/rspack/dist/loader.js')
	rmSync(outDir, { recursive: true, force: true })
	const config = {
		context: appDir,
		mode: 'production',
		entry: appMain,
		output: { path: outDir, filename: 'main.js', chunkFilename: '[name].chunk.js', clean: true, publicPath: '' },
		resolve: { extensions: ['.ts', '.js', '.mjs'], modules: [appNodeModules, join(repoRoot, 'node_modules')] },
		module: {
			rules: [
				{
					test: /(?<!\.d)\.ts$/,
					use: [
						{
							loader: 'builtin:swc-loader',
							options: { jsc: { parser: { syntax: 'typescript', decorators: true }, target: 'es2022' } },
						},
						{ loader: treatyLoaderPath, options: {} },
					],
				},
			],
		},
		plugins: [new TreatyRspackPlugin({ moduleFederation: false })],
		optimization: { minimize: true },
		target: 'web',
		infrastructureLogging: { level: 'error' },
		stats: 'none',
	}
	await new Promise((resolve, reject) => {
		rspack(config, (err, stats) => {
			if (err) return reject(err)
			if (stats.hasErrors()) {
				const first = stats.toJson({ errors: true }).errors?.[0]
				return reject(new Error(String(first?.message ?? first ?? 'rspack build error').split('\n').slice(0, 4).join(' ')))
			}
			resolve()
		})
	})
}

// rsbuild: @rsbuild/core programmatic build via createRsbuild + pluginTreaty (the api.transform path
// lowers Treaty `.ts` and links the partials; rsbuild's own SWC transpiles the rest). distPath is
// pinned to outDir; assetPrefix '' so the runtime skips the publicPath auto-detect under jsdom.
async function buildRsbuild(outDir) {
	const { createRsbuild } = req('@rsbuild/core')
	const { pluginTreaty } = req(join(repoRoot, 'libs/treaty/rsbuild/dist/plugin.js'))
	rmSync(outDir, { recursive: true, force: true })
	const rsbuild = await createRsbuild({
		cwd: appDir,
		rsbuildConfig: {
			root: appDir,
			source: { entry: { index: appMain } },
			plugins: [pluginTreaty()],
			mode: 'production',
			dev: { progressBar: false },
			// `chunkSplit: 'all-in-one'` keeps the whole app (runtime + vendor + entry) in ONE entry
			// chunk, so the emitted output boots from a single ESM import in jsdom (rsbuild's default
			// webpack-style vendor split needs the HTML's ordered <script> tags + jsonp chunk loading,
			// which a single-file import does not trigger). Lazy routes still split into async chunks.
			performance: { printFileSize: false, chunkSplit: { strategy: 'all-in-one' } },
			html: { template: join(appDir, 'src', 'index.html') },
			output: {
				distPath: { root: outDir },
				cleanDistPath: true,
				minify: true,
				target: 'web',
				assetPrefix: '',
				sourceMap: false,
			},
			resolve: { alias: {} },
			tools: {
				rspack: (config) => {
					// Resolve the app's @angular/* + workspace deps from the symlink farm.
					config.resolve ??= {}
					config.resolve.modules = [appNodeModules, join(repoRoot, 'node_modules'), 'node_modules']
				},
			},
			logLevel: 'error',
		},
	})
	const res = await rsbuild.build()
	if (res && typeof res.close === 'function') await res.close()
}

// rslib: @rslib/core LIBRARY build via defineTreatyLib + createRslib. NOTE (reported on the row): a
// library build EXTERNALIZES `@angular/*` by design, so its dist excludes the Angular runtime — it is
// NOT a like-for-like app-size comparison with the app bundlers. It still boots because the
// externalized bare `@angular/*` imports resolve (pre-linked AOT) from the symlink farm at import.
async function buildRslib(outDir) {
	const { createRslib } = req('@rslib/core')
	const { defineTreatyLib } = req(join(repoRoot, 'libs/treaty/rslib/dist/define-lib.js'))
	rmSync(outDir, { recursive: true, force: true })
	const libConfig = defineTreatyLib({ formats: ['esm'], dts: false, bundle: true })
	// `createRslib` reads `options.config` (NOT `rslibConfig`); each `lib[]` entry carries its own
	// `source.entry`. `target: 'web'` so the lib targets the browser (the boot runs in jsdom). Angular
	// stays EXTERNAL — the @treaty/rslib library plugin externalizes `@angular/*` by design (see the
	// row note); the boot layer supplies an AOT-linked Angular next to the chunks.
	const rslib = await createRslib({
		cwd: appDir,
		config: {
			root: appDir,
			lib: libConfig.lib.map((l) => ({
				...l,
				source: { entry: { main: appMain } },
				output: { target: 'web' },
			})),
			plugins: libConfig.plugins,
			mode: 'production',
			source: { entry: { main: appMain } },
			output: {
				...libConfig.output,
				target: 'web',
				distPath: { root: outDir },
				cleanDistPath: true,
				minify: true,
			},
			tools: {
				rspack: (config) => {
					config.resolve ??= {}
					config.resolve.modules = [appNodeModules, join(repoRoot, 'node_modules'), 'node_modules']
				},
			},
			logLevel: 'error',
		},
	})
	const res = await rslib.build()
	if (res && typeof res.close === 'function') await res.close()
}

// ---------------------------------------------------------------------------
// Angular-native baseline: drive `@angular/build:application` through the Architect programmatic API
// (the @angular/cli bin trips a hard Node-version GATE; the builder itself runs fine). Scaffolds an
// Angular-CLI project view over the SAME src/ and runs the production build (AOT, optimization on).
// ---------------------------------------------------------------------------
function scaffoldNgProject() {
	const ngRoot = join(benchRoot, 'ng-project')
	rmSync(ngRoot, { recursive: true, force: true })
	mkdirSync(join(ngRoot, 'src'), { recursive: true })

	// Reuse the app's existing src/ verbatim. The CLI main re-imports the app's bootstrap entry.
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
	]) {
		link(ngNm, name, resolvePkgDir(name))
	}
	return { ngRoot }
}

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
// e2e-of-output: boot one tool's emitted dist in a fresh child process; return the verdict record.
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
	// (1) Resolve the module entry from an emitted HTML's module <script src>. Prefer index.html, then
	//     any HTML (rsbuild names the page after the entry, e.g. `main.html`). The src may be
	//     root-absolute (`/static/js/main.x.js`) or relative; resolve against the HTML's own dir.
	const orderedHtml = [
		...htmlFiles.filter((h) => /(^|[\\/])index\.html$/.test(h)),
		...htmlFiles.filter((h) => !/(^|[\\/])index\.html$/.test(h)),
	]
	for (const htmlRel of orderedHtml) {
		const htmlDir = dirname(join(distDir, htmlRel))
		const html = readFileSync(join(distDir, htmlRel), 'utf-8')
		// The LAST module script is the entry (preload/vendor scripts may precede it).
		const matches = [...html.matchAll(/<script[^>]*type=["']module["'][^>]*\bsrc=["']([^"']+)["']/gi)]
		for (const m of matches.reverse()) {
			const src = m[1].replace(/^\//, '')
			const candidate = existsSync(join(htmlDir, src)) ? join(htmlDir, src) : join(distDir, src)
			if (existsSync(candidate)) return candidate
		}
	}
	// (2) A `main`-named entry chunk, NOT inside an `async/` lazy-chunk dir (rspack emits `main.js`;
	//     rsbuild emits `static/js/main.<hash>.js` with lazy routes under `static/js/async/`).
	const mainEntry = jsFiles.find(
		(f) => !/(^|[\\/])async[\\/]/i.test(f) && /(^|[\\/])main([.-][^\\/]*)?\.m?js$/i.test(f),
	)
	if (mainEntry) return join(distDir, mainEntry)
	return join(distDir, jsFiles[0])
}

// Build a PRE-LINKED (AOT) @angular/* farm: copy each published Angular package and run the SHARED
// Rust linker (`@treaty/authoring-node`.linkPartial) over every partial fesm `.mjs` IN PLACE, so the
// copy carries AOT `ɵɵdefine*` (zero residual `ɵɵngDeclare*`, no `@angular/compiler`). This is what a
// real CONSUMER app provides for a LIBRARY whose `@angular/*` deps are externalized: the rslib output
// externalizes Angular by design, so to boot it we co-locate this AOT Angular next to the chunks (the
// app bundlers, which bundle + link Angular themselves, never need this). Built once, reused per run.
let _prelinkedFarm = null
function buildPrelinkedAngularFarm() {
	if (_prelinkedFarm) return _prelinkedFarm
	const farm = join(benchRoot, 'prelinked-nm')
	rmSync(farm, { recursive: true, force: true })
	mkdirSync(farm, { recursive: true })
	const { linkPartial } = req('@treaty/authoring-node')
	for (const pkg of ['@angular/core', '@angular/common', '@angular/router', '@angular/forms', '@angular/platform-browser']) {
		const src = resolvePkgDir(pkg)
		if (!src) continue
		const dest = join(farm, pkg)
		mkdirSync(dirname(dest), { recursive: true })
		cpSync(src, dest, { recursive: true, dereference: true })
		const fesm = join(dest, 'fesm2022')
		if (!existsSync(fesm)) continue
		for (const f of readdirSync(fesm, { recursive: true })) {
			if (typeof f !== 'string' || !f.endsWith('.mjs')) continue
			const p = join(fesm, f)
			const code = readFileSync(p, 'utf-8')
			if (!/ɵɵngDeclare/.test(code)) continue
			const idForLinker = join('node_modules', pkg, 'fesm2022', f) // the linker's partial guard wants a node_modules segment
			writeFileSync(p, linkPartial(code, idForLinker).code)
		}
	}
	for (const name of ['rxjs', 'tslib', 'jsdom']) link(farm, name, resolvePkgDir(name))
	_prelinkedFarm = farm
	return farm
}

function bootDist(tool, distDir) {
	const entry = findBootEntry(distDir)
	if (!entry) {
		return { tool, works: 'SKIPPED', reason: `no bootable JS entry found in dist (${distDir ?? 'no dist'})` }
	}
	// rslib emits a LIBRARY (externalized @angular/*, chained chunks). Co-locate the pre-linked AOT
	// Angular farm as a node_modules NEXT TO the chunks so Node resolves the externalized bare
	// `@angular/*` imports to AOT modules (a consumer's job) — then the library output boots with no
	// JIT. The app bundlers bundle+link Angular themselves and resolve jsdom from `appNodeModules`.
	let bootNodeModules = appNodeModules
	if (tool === 'rslib') {
		const farm = buildPrelinkedAngularFarm()
		const coLocated = join(dirname(entry), 'node_modules')
		rmSync(coLocated, { recursive: true, force: true })
		cpSync(farm, coLocated, { recursive: true, dereference: true })
		bootNodeModules = coLocated
	}
	let stdout = ''
	try {
		stdout = execFileSync(process.execPath, [bootScript, tool, entry, bootNodeModules], {
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
// Run one tool: clean build best-of-RUNS, then size the LAST successful output dir.
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
		times.push(performance.now() - t0)
		lastOut = outDir
	}
	if (firstError || times.length === 0) {
		return {
			tool,
			status: 'failed',
			note: String(firstError?.message ?? firstError ?? 'no successful run').split('\n').slice(0, 4).join(' '),
		}
	}
	const buildMs = Math.round(Math.min(...times))
	const distBytes = dirBytes(lastOut)
	const rec = { tool, status: 'measured', buildMs, distBytes }
	Object.defineProperty(rec, 'distDir', { value: lastOut, enumerable: false })
	rec.note = `best of ${times.length} run(s); times(ms)=[${times.map((t) => Math.round(t)).join(', ')}]`
	if (inspect) {
		try {
			const ins = inspectBundle(lastOut)
			rec.note += `; jsFiles=${ins.jsFiles} residualNgDeclare=${ins.residual} importsCompiler=${ins.compiler} linkedOk=${ins.linkedOk}`
		} catch {
			/* best-effort */
		}
	}
	return rec
}

// ---------------------------------------------------------------------------
async function main() {
	console.log(`== Treaty FULL-APP build-tool benchmark (app: examples/ng-bench-app, runs=${RUNS}) ==`)
	mkdirSync(resultsDir, { recursive: true })
	rmSync(benchRoot, { recursive: true, force: true })
	mkdirSync(benchRoot, { recursive: true })

	console.log('-- building @treaty plugin dists from source --')
	buildPluginDists()
	console.log('-- wiring app node_modules symlink farm --')
	wireNodeModules()

	const results = []

	console.log('-- vite (@treaty/vite) --')
	results.push(await measure('vite', (i) => join(benchRoot, `vite-${i}`), buildVite))

	console.log('-- rspack (@treaty/rspack) --')
	results.push(await measure('rspack', (i) => join(benchRoot, `rspack-${i}`), buildRspack))

	console.log('-- rsbuild (@treaty/rsbuild) --')
	results.push(await measure('rsbuild', (i) => join(benchRoot, `rsbuild-${i}`), buildRsbuild))

	console.log('-- rslib (@treaty/rslib) [library build: externalizes @angular/*] --')
	const rslibRec = await measure('rslib', (i) => join(benchRoot, `rslib-${i}`), buildRslib)
	rslibRec.note =
		`LIBRARY build — EXTERNALIZES @angular/* (not bundled), so dist excludes the Angular runtime and is NOT a like-for-like app-size comparison with the app bundlers. ` +
		rslibRec.note
	results.push(rslibRec)

	console.log('-- rolldown (@treaty/rolldown) --')
	results.push(await measure('rolldown', (i) => join(benchRoot, `rolldown-${i}`), buildRolldown))

	console.log('-- ng (@angular/build:application via Architect) --')
	if (!isInstalled('@angular/build') || !isInstalled('@angular-devkit/architect')) {
		results.push({ tool: 'ng', status: 'failed', note: '@angular/build or @angular-devkit/architect not installed' })
	} else {
		let scaffold = null
		try {
			scaffold = scaffoldNgProject()
		} catch (err) {
			results.push({ tool: 'ng', status: 'failed', note: `scaffold failed: ${String(err?.message ?? err)}` })
		}
		if (scaffold) {
			const ngOut = join(scaffold.ngRoot, 'dist')
			results.push(await measure('ng', () => ngOut, () => buildNg(scaffold.ngRoot)))
		}
	}

	// ---------------------------------------------------------------------------
	// e2e-of-output: boot every measured dist and fold a works verdict into its row.
	// ---------------------------------------------------------------------------
	console.log('\n-- e2e-of-output: booting each built dist headlessly (jsdom) --')
	const e2eResults = []
	for (const r of results) {
		if (r.status !== 'measured') {
			r.works = 'SKIPPED'
			r.worksReason = `not booted: build status="${r.status}" (${r.note ?? 'no dist produced'})`
			e2eResults.push({ tool: r.tool, works: 'SKIPPED', reason: r.worksReason })
			continue
		}
		const verdict = bootDist(r.tool, r.distDir)
		r.works = verdict.works
		r.worksReason = verdict.reason
		if (verdict.statCards != null) r.statCards = verdict.statCards
		if (verdict.navLinks != null) r.navLinks = verdict.navLinks
		console.log(`  ${r.tool.padEnd(9)} works=${String(verdict.works).padEnd(7)} ${verdict.reason}`)
		e2eResults.push(verdict)
	}

	const out = {
		generatedAt: new Date().toISOString(),
		app: 'examples/ng-bench-app',
		appNote:
			'Realistic standard-Angular application (6 components, a service layer, a pure pipe, an attribute directive, an eager route + 3 lazy routes). Pure @Component/@Directive/@Pipe .ts — zero Treaty-only features — so it builds with BOTH the Angular CLI (@angular/build:application) and every Treaty bundler plugin, exercising the FULL Treaty compile path (decorator lowering + template codegen), not just the linker. StatCard uses the CONVENTIONAL Angular-CLI selector (class StatCard ↔ selector "app-stat-card", used as <app-stat-card>), which does NOT fold to the class name — so the Dashboard resolves its three cross-file StatCard children ONLY through the project SELECTOR REGISTRY (each Treaty plugin scans the app .ts at buildStart via selectorRoot and threads the per-file importName→selector map into the compiler). The attribute directive (class ThemeToggle ↔ "[themeToggle]") resolves the same way.',
		matchedOptimization:
			'Every tool builds in PRODUCTION mode with minify + tree-shake ON, so dist sizes are apples-to-apples — EXCEPT rslib, which is a LIBRARY builder that externalizes @angular/* by design (its dist excludes the Angular runtime; flagged on its row).',
		host: { platform: process.platform, arch: process.arch, node: process.version },
		runsPerTool: RUNS,
		versions: {
			'@angular/core': pkgVersion('@angular/core'),
			vite: pkgVersion('vite'),
			rolldown: pkgVersion('rolldown'),
			'@rspack/core': pkgVersion('@rspack/core'),
			'@rsbuild/core': pkgVersion('@rsbuild/core'),
			'@rslib/core': pkgVersion('@rslib/core'),
			'@angular/build': pkgVersion('@angular/build'),
		},
		metric:
			'best (min) wall-clock build time over N clean builds; distBytes = sum of all emitted output files; works = headless jsdom boot verdict (renders the eager Dashboard route with no JIT / @angular/compiler error).',
		e2e: {
			layer: 'e2e-of-output',
			method:
				'After timing+sizing each build tool, boot its emitted bundle headlessly in jsdom (a fresh child process per tool, reusing the linker-smoke e2e Step-4 boot pattern via boot-fullapp.mjs) and assert: (a) bootstrap throws NO JIT / "@angular/compiler not available" error, and (b) the eager Dashboard route renders (<h2>Inventory dashboard</h2>). statCards/navLinks report cross-file component-resolution completeness.',
			results: e2eResults,
		},
		results,
	}
	const outPath = join(resultsDir, 'fullapp.json')
	writeFileSync(outPath, JSON.stringify(out, null, 2) + '\n')

	console.log('\n== results ==')
	for (const r of results) {
		const size = r.distBytes != null ? `${(r.distBytes / 1024).toFixed(1)} KiB` : '—'
		const ms = r.buildMs != null ? `${r.buildMs} ms` : '—'
		console.log(
			`  ${r.tool.padEnd(9)} ${r.status.padEnd(9)} ${ms.padStart(9)}  ${size.padStart(11)}  works=${r.works ?? '—'}`,
		)
	}
	console.log(`\nwrote ${outPath}`)
	return out
}

main().catch((err) => {
	console.error('BENCH ERROR:', err)
	process.exit(1)
})
