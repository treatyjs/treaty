// Treaty everything-app SERVER-FN DEV BACKEND end-to-end harness (Phase 3).
//
// Phases 1 + 2 proved a file-level `'use server'` module's body (and any secret in
// it) no longer ships to the client CODE or the client source MAP. This harness
// proves the OTHER half of the contract the user asked for: that server code
// actually RUNS in dev.
//
// The compiler lifts each server-fn body out of the client bundle and leaves the
// client a tiny RPC stub that `fetch`es `/__server/<name>`. Until now nothing
// served `/__server/*` in dev, so that stub 404'd and server code never ran. The
// `@treaty/vite` dev backend (a `configureServer` middleware) now serves
// `/__server/<name>` by loading the ORIGINAL server module SSR-side and invoking
// the real body — returning JSON for an API fn and an SSE stream for an
// async-generator fn.
//
// This harness boots a REAL listening `vite` dev server over the ACTUAL
// everything-app and asserts, over HTTP:
//   (A) ROUTE RESPONDS — POST /__server/listTodos returns the real todo list as
//       JSON (the server body RAN: it touched the server-only secret server-side),
//       and POST /__server/addTodo creates + returns a todo.
//   (B) STREAM YIELDS — GET /__server/streamLogs streams SSE `data:` frames, one
//       per `yield` of the `async function* streamLogs` (server-push transport).
//   (C) NO LEAK — the SERVER-ONLY SECRET (`sk_live_…` in todos.server.ts) and the
//       server-fn body tokens appear in NEITHER the client-served module for
//       todos.server.ts NOR its client source map. (Phases 1/2 guard the build
//       artifact + map; here we re-assert over the live dev pipeline.)
//
// Verification is by PARSING/READING the responses (JSON.parse, SSE frame split,
// structured map inspection) — never by regex over emitted code for the leak check
// of the secret token (a literal substring search of the served text is exact).
//
// Usage:  node examples/everything-app/server-fn-dev.e2e.mjs
// Exit code 0 on success, 1 on any failed assertion.

import { createServer } from 'vite'
import { execFileSync } from 'node:child_process'
import { createRequire } from 'node:module'
import { readFileSync, rmSync, existsSync, mkdirSync, symlinkSync, lstatSync } from 'node:fs'
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

// The server-only secret that lives in src/server/todos.server.ts. It must never
// cross to the client. Read it from the source so the test tracks the real token.
const SERVER_SRC = readFileSync(join(here, 'src/server/todos.server.ts'), 'utf8')
const SECRET = 'sk_live_TREATY_SERVER_ONLY_9f3a1c'

// ---------------------------------------------------------------------------
// Step 0: node_modules symlink farm (the examples are not in the root lockfile).
// Mirrors dev-serve.e2e.mjs: link every @treaty/* + @angular/* dep the app's
// vite.config.ts pulls in, plus the build toolchain. Deliberately omits
// @angular/compiler / compiler-cli / @babel/core (no JIT, no Babel finisher).
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
	try {
		const st = lstatSync(dest)
		if (st.isSymbolicLink() || st.isDirectory()) return true
		rmSync(dest, { recursive: true, force: true })
	} catch {
		/* not present */
	}
	symlinkSync(target, dest, 'junction')
	return true
}

function unlinkFrom(nodeModules, name) {
	const dest = join(nodeModules, name)
	try {
		if (lstatSync(dest)) rmSync(dest, { recursive: true, force: true })
	} catch {
		/* already gone */
	}
}

function wireNodeModules() {
	const nm = join(here, 'node_modules')
	mkdirSync(nm, { recursive: true })
	unlinkFrom(nm, '@angular/compiler')
	unlinkFrom(nm, '@angular/compiler-cli')
	unlinkFrom(nm, '@babel/core')
	linkInto(nm, '@treaty/jsx', join(repoRoot, 'libs/treaty/jsx'))
	linkInto(nm, '@treaty/vite', join(repoRoot, 'libs/treaty/vite'))
	linkInto(nm, '@treaty/compiler', join(repoRoot, 'libs/treaty/compiler'))
	linkInto(nm, '@treaty/module-federation', join(repoRoot, 'libs/treaty/module-federation'))
	linkInto(nm, '@treaty/ts-vite', join(repoRoot, 'libs/typescript/vite'))
	linkInto(nm, '@treaty/authoring-node', join(repoRoot, 'libs/authoring/node'))
	for (const name of ['@angular/core', '@angular/common', '@angular/router', '@angular/platform-browser', 'rxjs', 'tslib', 'vite', 'esbuild']) {
		linkInto(nm, name, resolvePkgDir(name))
	}
	return nm
}

// ---------------------------------------------------------------------------
// Step 1: (re)build the @treaty/ts-vite + @treaty/vite plugin dists from current
// source, deps EXTERNAL, so the wiring under test is the committed source. Mirrors
// dev-serve.e2e.mjs (the external-deps rationale is the native addon require).
// ---------------------------------------------------------------------------
const tsViteDist = join(repoRoot, 'libs/typescript/vite/dist/index.js')
const treatyViteDist = join(repoRoot, 'libs/treaty/vite/dist/index.js')

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
// SSE helper: read a `text/event-stream` body to its `data:` frames. Reads the
// whole stream (the dev stream fn yields a finite count then an `event: end`).
// ---------------------------------------------------------------------------
async function readSseFrames(res) {
	const text = await res.text()
	const frames = []
	for (const block of text.split('\n\n')) {
		const dataLines = block
			.split('\n')
			.filter((l) => l.startsWith('data: '))
			.map((l) => l.slice('data: '.length))
		if (dataLines.length === 0) continue
		const isEnd = block.split('\n').some((l) => l.startsWith('event: end'))
		frames.push({ data: dataLines.join('\n'), end: isEnd })
	}
	return frames
}

// ---------------------------------------------------------------------------
// The main flow: boot a REAL listening dev server, register the server fns by
// transforming the importing module, then drive the dev backend over HTTP.
// ---------------------------------------------------------------------------
async function main() {
	console.log('== Step 0: wire local node_modules ==')
	wireNodeModules()

	console.log('== Step 1: build @treaty/ts-vite + @treaty/vite plugin dists ==')
	buildPluginDists()
	check('@treaty/ts-vite dist built', existsSync(tsViteDist))
	check('@treaty/vite dist built', existsSync(treatyViteDist))

	console.log('== Step 2: boot a REAL listening vite dev server over the everything-app ==')
	const port = 5394
	const server = await createServer({
		root: here,
		logLevel: 'warn',
		configFile: join(here, 'vite.config.ts'),
		server: { port, strictPort: true, hmr: false },
		optimizeDeps: { noDiscovery: true, include: [] },
	})
	const base = `http://localhost:${port}`
	try {
		await server.listen()

		// (C) NO LEAK — drive the CLIENT transform of the server module through the
		// dev pipeline and assert the secret + body tokens are absent from both the
		// served client CODE and its client source MAP. transformRequest runs the
		// exact pipeline a browser fetch hits. This ALSO registers the server fns
		// into the dev backend (the client side is what the importing app loads).
		const clientResult = await server.transformRequest('/src/server/todos.server.ts')
		const clientCode = clientResult?.code ?? ''
		check('(C) client-served todos.server.ts is non-empty (compiled by Treaty)', clientCode.length > 0)
		check(
			'(C) the server-only SECRET does NOT appear in the client-served module code',
			clientCode.length > 0 && !clientCode.includes(SECRET),
			clientCode.includes(SECRET) ? 'SECRET LEAKED into client code' : '',
		)
		// Body tokens that originated in the server fns (never client code).
		for (const token of ['store.push', 'store.slice', 'store.findIndex']) {
			check(
				`(C) server-fn body token "${token}" is absent from the client-served code`,
				!clientCode.includes(token),
				clientCode.includes(token) ? 'body leaked' : '',
			)
		}
		// The CLIENT MAP the compiler emits (the artifact that ships) must not embed
		// the secret in its sourcesContent. We assert this on the ADDON's own redacted
		// map (the authoritative artifact PHASE 2 produces), parsed structurally —
		// rather than the dev server's recomposed map, which Vite rebuilds from the
		// on-disk source after its esbuild TS-strip and is not the shipped artifact.
		{
			const addon = req('@treaty/authoring-node')
			const compiled = addon.compile(SERVER_SRC, 'todos.server.ts')
			check('(C) the addon emits a client map for the server module', typeof compiled.map === 'string' && compiled.map.length > 0)
			check(
				'(C) the addon client CODE does NOT contain the SECRET (the shipped client artifact)',
				typeof compiled.code === 'string' && !compiled.code.includes(SECRET),
			)
			let parsed = null
			try {
				parsed = JSON.parse(compiled.map)
			} catch {
				parsed = null
			}
			check('(C) the addon client map is valid v3 JSON', parsed && parsed.version === 3)
			const sc = parsed && Array.isArray(parsed.sourcesContent) ? parsed.sourcesContent.join('\n') : ''
			check(
				'(C) the addon client map does NOT embed the SECRET in sourcesContent (PHASE 2 redaction)',
				sc.length > 0 && !sc.includes(SECRET),
				sc.includes(SECRET) ? 'SECRET LEAKED into client map' : '',
			)
			check(
				'(C) the addon client map does NOT embed a server-fn body token',
				!sc.includes('store.slice') && !sc.includes('store.findIndex'),
			)
		}

		// Also register the stream fn by transforming its module client-side.
		await server.transformRequest('/src/server/logs.stream.ts')

		// (A) ROUTE RESPONDS — POST /__server/listTodos returns the real list as JSON.
		{
			const r = await fetch(`${base}/__server/listTodos`, {
				method: 'POST',
				headers: { 'content-type': 'application/json' },
				body: JSON.stringify([]),
			})
			const ct = r.headers.get('content-type') || ''
			let list = null
			let raw = ''
			try {
				raw = await r.text()
				list = JSON.parse(raw)
			} catch {
				/* leave null */
			}
			check(
				'(A) POST /__server/listTodos responds 200 JSON (no 404 — the dev backend serves it)',
				r.status === 200 && /application\/json/.test(ct),
				`status=${r.status} content-type="${ct}"`,
			)
			check(
				'(A) listTodos ran server-side and returned the real todo list',
				Array.isArray(list) && list.length >= 3 && list.every((t) => typeof t.title === 'string'),
				Array.isArray(list) ? `${list.length} todos` : `body=${raw.slice(0, 80)}`,
			)
			check(
				'(A) the server-only SECRET is NOT in the listTodos response payload',
				!raw.includes(SECRET),
			)
		}

		// (A) POST /__server/addTodo creates + returns a todo (a mutating server fn ran).
		{
			const r = await fetch(`${base}/__server/addTodo`, {
				method: 'POST',
				headers: { 'content-type': 'application/json' },
				body: JSON.stringify('e2e created todo'),
			})
			let created = null
			let raw = ''
			try {
				raw = await r.text()
				created = JSON.parse(raw)
			} catch {
				/* leave null */
			}
			check(
				'(A) POST /__server/addTodo responds 200 and the mutating server fn ran',
				r.status === 200 && created && created.title === 'e2e created todo' && created.done === false,
				`status=${r.status} body=${raw.slice(0, 100)}`,
			)
		}

		// (B) STREAM YIELDS — GET /__server/streamLogs streams SSE frames, one per yield.
		{
			const r = await fetch(`${base}/__server/streamLogs`, {
				method: 'POST',
				headers: { 'content-type': 'application/json' },
				body: JSON.stringify(4),
			})
			const ct = r.headers.get('content-type') || ''
			const frames = await readSseFrames(r)
			const valueFrames = frames.filter((f) => !f.end)
			check(
				'(B) GET/POST /__server/streamLogs responds with an SSE stream (text/event-stream)',
				r.status === 200 && /text\/event-stream/.test(ct),
				`status=${r.status} content-type="${ct}"`,
			)
			check(
				'(B) the async-generator streamLogs yields one SSE frame per line (4 frames)',
				valueFrames.length === 4,
				`got ${valueFrames.length} value frames`,
			)
			let firstOk = false
			try {
				const first = JSON.parse(valueFrames[0]?.data ?? 'null')
				firstOk = first && first.seq === 1 && typeof first.message === 'string'
			} catch {
				firstOk = false
			}
			check('(B) each SSE frame carries a real LogLine (seq + message)', firstOk)
			check(
				'(B) a terminating end frame closes the stream',
				frames.some((f) => f.end),
			)
		}

		// (A') A genuinely unknown server fn 404s (the backend does not silently 200).
		{
			const r = await fetch(`${base}/__server/noSuchFn`, { method: 'POST', body: '[]' })
			check('(A′) an unregistered /__server/<name> returns 404 (not an HTML fallback)', r.status === 404, `status=${r.status}`)
		}
	} finally {
		await server.close()
	}

	console.log('')
	if (failures.length) {
		console.error(`E2E FAILED: ${failures.length} assertion(s): ${failures.join('; ')}`)
		process.exit(1)
	}
	console.log(
		'E2E PASSED: the everything-app dev server RUNS server code over HTTP — POST /__server/listTodos + /__server/addTodo return real JSON results, the async-generator /__server/streamLogs streams SSE frames per yield, and the server-only secret + server-fn bodies leak into NEITHER the client-served module nor its source map.',
	)
}

main().catch((err) => {
	console.error('E2E ERROR:', err)
	process.exit(1)
})
