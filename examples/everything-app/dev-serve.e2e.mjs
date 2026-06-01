// Treaty everything-app FULL DEV-SERVE end-to-end harness (Phase 2).
//
// The dev-serve counterpart to `full-build.e2e.mjs` (which proves a real `vite build`). Here we boot a
// REAL `vite` dev server over the ACTUAL everything-app source (its own `vite.config.ts` — the real
// `treaty()` plugin chain) and FETCH every authoring surface the app ships THROUGH the running dev
// server, asserting each is served CLEAN and lowered to Ivy with no JIT / no foreign React runtime:
//
//   (1) `.tsx` + `.tjsx` Treaty/Angular JSX  (counter.tsx, greeting-card.tjsx)
//         -> served module contains `ɵɵdefineComponent`, NO unresolved `@treaty/jsx/jsx-dev-runtime`,
//            and NO React `createElement(`/`jsxDEV(` call (Treaty JSX is Ivy, not React).
//   (2) a `.treaty` SFC                       (gauge.treaty)        -> `ɵɵdefineComponent`.
//   (3) a `@Component` `.ts`                  (log-viewer, app-root) -> `ɵɵdefineComponent`.
//   (4) a partial `@angular/*` dep served via the linker             -> 0 residual `ɵɵngDeclare*`,
//            no `@angular/compiler`, no `@angular/compiler-cli`/`@babel/core`.
//   (5) the dev `index.html` injects NO `@angular/compiler` script (no JIT in dev).
//
// THE EXACT USER FAILURE (this harness's primary regression guard):
//   `bun run dev:vite` aborted its dependency scan with
//       "@treaty/jsx/jsx-dev-runtime (imported by .../counter.tsx) could not be resolved"
//   because esbuild's automatic-JSX dev transform read the app tsconfig (`jsxImportSource:"@treaty/jsx"`)
//   and INJECTED that import. The `treaty()` `config()` fix forces esbuild `jsx:'preserve'` on both
//   esbuild surfaces so the phantom import is never injected. This harness boots a REAL `vite` dev
//   server (subprocess) over the WHOLE app and asserts it serves WITHOUT any "could not be resolved"
//   dep-scan failure — the user's exact error, gone — then shuts the server down cleanly.
//
// REPORTED OUT-OF-SCOPE RUST-COMPILER GAP (does NOT affect dev-serve):
//   `greeting-card.tjsx` (and `counter.tsx`) use a `use:<name>` template directive that the Rust JSX
//   lowering emits into the Ivy `dependencies: […]` array as an undefined class reference (`Autofocus`/
//   `Highlight`), with no import/definition. That is a runtime ReferenceError at BOOT — it does NOT
//   affect the dev-serve TRANSFORM, which lowers every JSX file to a single `ɵɵdefineComponent` and
//   serves it clean (verified below). Booting the full app in jsdom is therefore out of scope for this
//   dev-serve harness (the build harness already gates that boot on the documented gap); here we prove
//   the dev SERVE of every surface, which is exactly the user-reported failure mode.
//
// Usage:  node examples/everything-app/dev-serve.e2e.mjs
// Exit code 0 on success, 1 on any failed assertion.

import { createServer } from 'vite'
import { execFileSync, spawn } from 'node:child_process'
import { createRequire } from 'node:module'
import { readFileSync, rmSync, readdirSync, existsSync, mkdirSync, symlinkSync, lstatSync } from 'node:fs'
import { join, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(here, '..', '..')
const req = createRequire(import.meta.url)

const failures = []
function check(label, condition, detail) {
	const ok = Boolean(condition)
	console.log(`${ok ? 'PASS' : 'FAIL'}  ${label}${detail ? ` - ${detail}` : ''}`)
	if (!ok) failures.push(label)
	return ok
}

// ---------------------------------------------------------------------------
// Step 0: wire a local node_modules symlink farm.
//
// The examples are not in the root lockfile, so the app has no node_modules of its own. CRUCIAL: this
// links `@treaty/jsx` (the JSX authoring-format types + automatic-runtime shim) into node_modules
// alongside the other @treaty/* packages — the user's bug was precisely that `@treaty/jsx` was NOT
// linked, so an injected `@treaty/jsx/jsx-dev-runtime` import could not resolve. The farm links every
// @treaty workspace package the app's vite.config.ts pulls in transitively, plus the real partial
// @angular libs + rxjs/tslib + the build toolchain (vite, esbuild). It deliberately does NOT link
// @angular/compiler / @angular/compiler-cli / @babel/core, so a build that needed JIT or the Babel
// finisher would fail to resolve them (the no-JIT / Rust-only guarantee).
// ---------------------------------------------------------------------------
function resolvePkgDir(name) {
	try {
		return dirname(req.resolve(`${name}/package.json`, { paths: [repoRoot] }))
	} catch {
		return null
	}
}

function linkInto(nodeModules, name, target) {
	if (!target) return false
	const dest = join(nodeModules, name)
	mkdirSync(dirname(dest), { recursive: true })
	let present = false
	try {
		const st = lstatSync(dest)
		present = Boolean(st)
		if (st.isSymbolicLink() || st.isDirectory()) return true
	} catch {
		present = false
	}
	if (present) rmSync(dest, { recursive: true, force: true })
	symlinkSync(target, dest, 'junction')
	return true
}

/** Remove a stale entry so the no-JIT / no-Babel build-graph guarantee holds run-to-run. */
function unlinkFrom(nodeModules, name) {
	const dest = join(nodeModules, name)
	if (
		existsSync(dest) ||
		(() => {
			try {
				return Boolean(lstatSync(dest))
			} catch {
				return false
			}
		})()
	) {
		rmSync(dest, { recursive: true, force: true })
	}
}

function wireNodeModules() {
	const nm = join(here, 'node_modules')
	mkdirSync(nm, { recursive: true })
	// The Rust-only linker must NEVER drag JIT or the Babel finisher into the build graph.
	unlinkFrom(nm, '@angular/compiler')
	unlinkFrom(nm, '@angular/compiler-cli')
	unlinkFrom(nm, '@babel/core')
	// @treaty/jsx is NOT in the root lockfile workspace map; link it by its known monorepo path. This is
	// the package whose absence caused the user's "@treaty/jsx/jsx-dev-runtime could not be resolved".
	linkInto(nm, '@treaty/jsx', join(repoRoot, 'libs/treaty/jsx'))
	// @treaty workspace packages the app's vite.config.ts pulls in transitively.
	linkInto(nm, '@treaty/vite', join(repoRoot, 'libs/treaty/vite'))
	linkInto(nm, '@treaty/compiler', join(repoRoot, 'libs/treaty/compiler'))
	linkInto(nm, '@treaty/module-federation', join(repoRoot, 'libs/treaty/module-federation'))
	linkInto(nm, '@treaty/ts-vite', join(repoRoot, 'libs/typescript/vite'))
	linkInto(nm, '@treaty/authoring-node', join(repoRoot, 'libs/authoring/node'))
	// Runtime + build deps resolved from the monorepo.
	for (const name of [
		'@angular/core',
		'@angular/common',
		'@angular/router',
		'@angular/platform-browser',
		'rxjs',
		'tslib',
		'vite',
		'esbuild',
	]) {
		linkInto(nm, name, resolvePkgDir(name))
	}
	return nm
}

// ---------------------------------------------------------------------------
// Step 1: (re)build the @treaty/ts-vite + @treaty/vite plugin dists from current source, so the wiring
// under test is the committed source. Both are bundled with deps EXTERNAL so the runtime cross-package
// imports resolve through the symlink farm (not an inlined copy). @treaty/vite is ESM (its package
// declares "type": "module"); @treaty/ts-vite is CJS. See full-build.e2e.mjs for the external-deps
// rationale (inlining @treaty/ts-vite would break its native `require('@treaty/authoring-node')`).
// ---------------------------------------------------------------------------
const tsViteDist = join(repoRoot, 'libs/typescript/vite/dist/index.js')
const treatyViteDist = join(repoRoot, 'libs/treaty/vite/dist/index.js')
const pluginDistPath = tsViteDist

function buildPluginDists() {
	const bin = req.resolve('esbuild/bin/esbuild', { paths: [repoRoot] })
	mkdirSync(dirname(tsViteDist), { recursive: true })
	execFileSync(
		process.execPath,
		[
			bin,
			join(repoRoot, 'libs/typescript/vite/src/index.ts'),
			'--bundle',
			'--platform=node',
			'--format=cjs',
			'--target=node20',
			'--packages=external',
			`--outfile=${tsViteDist}`,
		],
		{ stdio: ['ignore', 'ignore', 'inherit'] },
	)
	mkdirSync(dirname(treatyViteDist), { recursive: true })
	execFileSync(
		process.execPath,
		[
			bin,
			join(repoRoot, 'libs/treaty/vite/src/index.ts'),
			'--bundle',
			'--platform=node',
			'--format=esm',
			'--target=node20',
			'--packages=external',
			'--external:@treaty/ts-vite',
			'--external:@treaty/authoring-node',
			'--external:@treaty/compiler',
			'--external:@treaty/module-federation',
			`--outfile=${treatyViteDist}`,
		],
		{ stdio: ['ignore', 'ignore', 'inherit'] },
	)
}

// ---------------------------------------------------------------------------
// Step 2: boot a REAL `vite` dev server (subprocess) over the WHOLE everything-app via its own
// vite.config.ts and assert it serves WITHOUT the user's dep-scan "could not be resolved" failure.
//
// This is the user's exact reproduction: `vite` (dev) over a `.tsx`/`.tjsx` app whose tsconfig sets
// `jsxImportSource:"@treaty/jsx"`. We DO NOT pass `optimizeDeps.noDiscovery` here, so the real
// dependency scanner runs over the actual entry graph (the failing path). The server must come up and
// serve index.html with no resolution error in its output, then we shut it down cleanly.
// ---------------------------------------------------------------------------
function viteBin() {
	const pkgDir = dirname(req.resolve('vite/package.json', { paths: [repoRoot] }))
	const pkg = req(join(pkgDir, 'package.json'))
	const binRel = typeof pkg.bin === 'string' ? pkg.bin : pkg.bin.vite
	return join(pkgDir, binRel)
}

async function assertRealDevServerNoDepScanFailure() {
	const port = 5391
	const child = spawn(
		process.execPath,
		[viteBin(), '-c', join(here, 'vite.config.ts'), '--port', String(port), '--strictPort'],
		{ cwd: here, stdio: ['ignore', 'pipe', 'pipe'] },
	)
	let stderr = ''
	let stdout = ''
	child.stderr?.on('data', (d) => (stderr += d))
	child.stdout?.on('data', (d) => (stdout += d))

	const base = `http://localhost:${port}`
	async function waitReady(timeoutMs) {
		const deadline = Date.now() + timeoutMs
		while (Date.now() < deadline) {
			// If the dep scan aborted, Vite prints the resolution error and never serves; bail early so
			// the assertion reports the real failure rather than just timing out.
			if (/could not be resolved/.test(stderr) || /could not be resolved/.test(stdout)) return false
			try {
				const r = await fetch(`${base}/index.html`)
				if (r.ok) return true
			} catch {
				/* not up yet */
			}
			await new Promise((r) => setTimeout(r, 200))
		}
		return false
	}

	let servedHtml = ''
	try {
		const ready = await waitReady(60_000)
		if (ready) {
			try {
				servedHtml = await (await fetch(`${base}/index.html`)).text()
			} catch {
				/* leave empty */
			}
		}
		const log = `${stderr}\n${stdout}`.slice(-800)
		check(
			'REAL `vite` dev server serves the whole everything-app WITHOUT a dep-scan resolution failure',
			ready,
			ready ? '' : log,
		)
		check(
			'DEV server did NOT report "@treaty/jsx/jsx-dev-runtime could not be resolved" (the EXACT user error)',
			!/could not be resolved/.test(log),
			/could not be resolved/.test(log) ? log : '',
		)
		// (5) The dev-served index.html must inject NO @angular/compiler script (no JIT in dev).
		check(
			'dev-served index.html injects NO @angular/compiler script (no JIT in dev)',
			servedHtml.length > 0 && !/@angular\/compiler/.test(servedHtml),
			servedHtml.length === 0 ? 'index.html not served' : '',
		)
	} finally {
		await new Promise((resolve) => {
			let done = false
			const finish = () => {
				if (!done) {
					done = true
					resolve()
				}
			}
			child.once('exit', () => {
				clearTimeout(killTimer)
				finish()
			})
			child.kill()
			const killTimer = setTimeout(() => {
				try {
					child.kill('SIGKILL')
				} catch {
					/* already gone */
				}
				finish()
			}, 3000)
		})
	}
}

// ---------------------------------------------------------------------------
// Step 3: drive each authoring surface THROUGH the dev server's real transform pipeline.
//
// An in-process dev server (middleware mode) lets us call `transformRequest(id)` — the exact pipeline
// the browser hits when it requests a module URL — for each surface and assert the served code. This is
// the dev-serve equivalent of building each module: it runs the `treaty()` pre-transform (JSX/.treaty/
// @Component lowering) followed by Vite's import-analysis/rewrite, exactly as a browser fetch would.
// `noDiscovery` skips the dependency PRE-bundle scan here (Step 2 already proved the real scanner does
// not fail); this server is for fetching the lowered authoring modules deterministically.
// ---------------------------------------------------------------------------
async function withDevServer(fn) {
	const server = await createServer({
		root: here,
		logLevel: 'warn',
		configFile: join(here, 'vite.config.ts'),
		server: { middlewareMode: true, hmr: false },
		optimizeDeps: { noDiscovery: true, include: [] },
	})
	try {
		return await fn(server)
	} finally {
		await server.close()
	}
}

/** Fetch a module's served code through the dev server transform pipeline (browser-equivalent). */
async function serve(server, id) {
	const result = await server.transformRequest(id)
	return result ? result.code : ''
}

// The Ivy emitter always prepends `import * as i0 from "<angular core>"`; Vite rewrites the bare
// `@angular/core` specifier to a `/@fs/.../core.mjs` (or prebundled) URL, so match the rewritten form.
const IVY_I0 = /import\s*\*\s*as\s+i0\s+from\s*["'][^"']*(?:@angular[/_]core|angular_core|core\.mjs)[^"']*["']/

async function assertAuthoringSurfaces(server) {
	// (1) Treaty/Angular JSX — .tsx and .tjsx.
	for (const [label, id] of [
		['counter.tsx (JSX .tsx)', '/src/components/counter.tsx'],
		['greeting-card.tjsx (JSX .tjsx)', '/src/features/greeter/greeting-card.tjsx'],
	]) {
		const code = await serve(server, id)
		if (!check(`dev-serve served ${label}`, code.length > 0)) continue
		check(
			`${label}: served module is Treaty-lowered Ivy (i0 @angular/core + ɵɵdefineComponent)`,
			IVY_I0.test(code) && /ɵɵdefineComponent/.test(code),
			code.replace(/\s+/g, ' ').slice(0, 140),
		)
		check(
			`${label}: served module injects NO @treaty/jsx/jsx-dev-runtime import (the user bug is gone)`,
			!/@treaty\/jsx\/jsx-dev-runtime/.test(code) && !/@treaty\/jsx/.test(code),
		)
		check(
			`${label}: served module has NO React-runtime jsxDEV( / createElement( (Treaty JSX is Ivy, not React)`,
			!/\bjsxDEV\s*\(/.test(code) && !/\bcreateElement\s*\(/.test(code),
		)
		check(`${label}: served module has ZERO residual ɵɵngDeclare*`, !/ɵɵngDeclare[A-Za-z]+\s*\(/.test(code))
	}

	// (2) .treaty SFC.
	{
		const id = '/src/features/metrics/gauge.treaty'
		const code = await serve(server, id)
		if (check('dev-serve served gauge.treaty (.treaty SFC)', code.length > 0)) {
			check(
				'gauge.treaty: served module is Treaty-lowered Ivy (i0 @angular/core + ɵɵdefineComponent)',
				IVY_I0.test(code) && /ɵɵdefineComponent/.test(code),
				code.replace(/\s+/g, ' ').slice(0, 140),
			)
			check('gauge.treaty: served module has ZERO residual ɵɵngDeclare*', !/ɵɵngDeclare[A-Za-z]+\s*\(/.test(code))
		}
	}

	// (3) @Component .ts (the eager index route component + the app shell).
	for (const [label, id] of [
		['log-viewer.component.ts (@Component .ts)', '/src/components/log-viewer.component.ts'],
		['app-root.component.ts (@Component .ts)', '/src/app/app-root.component.ts'],
	]) {
		const code = await serve(server, id)
		if (!check(`dev-serve served ${label}`, code.length > 0)) continue
		check(
			`${label}: served module is Treaty-lowered Ivy (i0 @angular/core + ɵɵdefineComponent)`,
			IVY_I0.test(code) && /ɵɵdefineComponent/.test(code),
			code.replace(/\s+/g, ' ').slice(0, 140),
		)
		check(`${label}: served module has ZERO residual ɵɵngDeclare*`, !/ɵɵngDeclare[A-Za-z]+\s*\(/.test(code))
	}
}

// (4) A partial @angular dep served via the linker -> 0 residual ɵɵngDeclare, no @angular/compiler.
//
// The on-the-fly LinkPartialPlugin is the dev transform for partial node_modules deps Vite does not
// pre-bundle. Run a real partial @angular/common fesm module through it exactly as the dev server does.
async function assertPartialAngularLinker(server) {
	const optimize = server.config.optimizeDeps ?? {}
	check(
		'dev-serve excludes @angular/compiler from prebundling (no JIT in dev)',
		(optimize.exclude ?? []).includes('@angular/compiler'),
	)
	check(
		'dev-serve wires the esbuild optimizeDeps linker (de-partials prebundled deps on the fly)',
		(optimize.esbuildOptions?.plugins ?? []).some((p) => p?.name === 'treaty-link-partial-deps'),
	)

	const linkPlugin = (server.config.plugins ?? []).find((p) => p?.name === 'vite-plugin-treaty-link-partial')
	if (!check('dev-serve has the on-the-fly partial linker (LinkPartialPlugin) active', Boolean(linkPlugin))) return

	const commonDir = resolvePkgDir('@angular/common')
	const fesm = commonDir
		? readdirSync(join(commonDir, 'fesm2022'), { recursive: true }).find(
				(f) => typeof f === 'string' && f.endsWith('.mjs'),
			)
		: null
	if (!fesm || typeof linkPlugin.transform !== 'function') {
		check('dev-serve links a partial @angular dep on the fly', false, 'no @angular/common fesm / linker transform')
		return
	}
	const id = join(commonDir, 'fesm2022', fesm)
	const idForLinker = id.includes('node_modules') ? id : join('node_modules', '@angular/common', 'fesm2022', fesm)
	const source = readFileSync(id, 'utf-8')
	const transformed = await linkPlugin.transform.call({}, source, idForLinker)
	const code = transformed?.code ?? ''
	check(
		'(4) dev-serve links a partial @angular dep via the linker (ZERO residual ɵɵngDeclare)',
		code.length > 0 && !/ɵɵngDeclare[A-Za-z]+\s*\(/.test(code),
		`len=${code.length} residual=${(code.match(/ɵɵngDeclare[A-Za-z]+\s*\(/g) || []).length}`,
	)
	check(
		'(4) dev-serve linked @angular dep does NOT import @angular/compiler (no JIT)',
		!/from\s*['"]@angular\/compiler['"]|import\(\s*['"]@angular\/compiler['"]/.test(code),
	)
	check(
		'(4) dev-serve linked @angular dep contains no @angular/compiler-cli / @babel/core (Rust-only)',
		!/@angular\/compiler-cli|@babel\/core/.test(code),
	)
	// Backend attribution is observability-only (cache-hit returns undefined); the post-Phase-1 shim
	// only ever records 'rust' (no fallback exists). Guarantee: recorded ⇒ "rust", NEVER "babel".
	const distMod = req(pluginDistPath)
	const backend =
		typeof distMod.getLinkBackend === 'function' ? distMod.getLinkBackend(idForLinker) : undefined
	check(
		'(4) dev-serve attributes the linked @angular dep to "rust" (or cache-hit; never a fallback)',
		backend === 'rust' || backend === undefined,
		`backend=${backend ?? 'none(cache-hit)'}`,
	)
}

// (5b) Also assert index.html through the in-process dev server's real transformIndexHtml pipeline,
// belt-and-suspenders alongside the Step-2 real-server fetch.
async function assertDevHtml(server) {
	const html = await server.transformIndexHtml('/index.html', readFileSync(join(here, 'index.html'), 'utf-8'))
	check(
		'(5) dev-serve index.html transform injects NO @angular/compiler script (no JIT in dev)',
		!/@angular\/compiler/.test(html),
		/@angular\/compiler/.test(html) ? 'compiler script present' : '',
	)
}

// ---------------------------------------------------------------------------
async function main() {
	console.log('== Step 0: wire local node_modules (incl @treaty/jsx) ==')
	wireNodeModules()
	const wired = (name) => {
		try {
			return Boolean(lstatSync(join(here, 'node_modules', name)))
		} catch {
			return false
		}
	}
	check(
		'local node_modules wired (@treaty/jsx + @treaty/vite + @angular/router present)',
		wired('@treaty/jsx') && wired('@treaty/vite') && wired('@angular/router'),
	)

	console.log('== Step 1: build @treaty/ts-vite + @treaty/vite plugin dists ==')
	buildPluginDists()
	check('@treaty/ts-vite dist built', existsSync(tsViteDist))
	check('@treaty/vite dist built', existsSync(treatyViteDist))

	console.log('== Step 2: REAL `vite` dev server boots the whole app with NO dep-scan failure (the user bug) ==')
	await assertRealDevServerNoDepScanFailure()

	console.log('== Step 3: each authoring surface served + lowered to Ivy through the dev pipeline ==')
	await withDevServer(async (server) => {
		await assertAuthoringSurfaces(server)
		await assertPartialAngularLinker(server)
		await assertDevHtml(server)
	})

	console.log('')
	if (failures.length) {
		console.error(`E2E FAILED: ${failures.length} assertion(s): ${failures.join('; ')}`)
		process.exit(1)
	}
	console.log(
		'E2E PASSED: the everything-app dev-serves cleanly — every authoring surface (.tsx/.tjsx/.treaty/@Component .ts) is lowered to Ivy, partial @angular is linked to AOT (no JIT), index.html injects no @angular/compiler, and the user-reported "@treaty/jsx/jsx-dev-runtime could not be resolved" dep-scan failure is gone.',
	)
}

main().catch((err) => {
	console.error('E2E ERROR:', err)
	process.exit(1)
})
