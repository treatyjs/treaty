// @ts-check
/**
 * @module
 *
 * Library build for `@treaty/vite`.
 *
 * Produces the published `dist/`:
 *   1. `dist/index.js`     — the ESM plugin bundle (esbuild, `src/index.ts` entry).
 *   2. `dist/index.js.map` — its source map.
 *   3. `dist/*.d.ts`(.map) — declarations emitted by tsgo (never tsc).
 *
 * EXTERNALIZATION (the load-bearing part).
 *
 * `@treaty/vite` declares `"type": "module"`, so `dist/index.js` is ESM. Its
 * runtime dependencies — `@treaty/compiler`, `@treaty/module-federation`, and
 * especially `@treaty/ts-vite` (the shared Angular partial-declaration linker) —
 * MUST stay EXTERNAL, i.e. real `import ... from "@treaty/ts-vite"` statements in
 * the published bundle, NOT inlined copies.
 *
 * Why this matters: `@treaty/ts-vite` is CommonJS and loads the native Rust
 * linker addon with `require("@treaty/authoring-node")`. If esbuild INLINES
 * ts-vite into this ESM bundle, that bare `require` is rewritten into esbuild's
 * "Dynamic require of \"@treaty/authoring-node\" is not supported" THROWING shim.
 * A published `@treaty/vite` built that way ships with the partial-Angular linker
 * silently disabled, so it emits un-linked `ɵɵngDeclare*` that crashes at runtime
 * ("needs the JIT compiler / @angular/compiler is not available").
 *
 * `--packages=external` (esbuild's `packages: 'external'`) is NOT sufficient on
 * its own: in this monorepo `@treaty/ts-vite` (etc.) resolves to a path under
 * `libs/`, not `node_modules`, and esbuild only treats *bare, node_modules-style*
 * specifiers as external under that flag — so it would still inline the `libs/`
 * copies. We therefore ALSO list every `@treaty/*` dependency (and the native
 * `@treaty/authoring-node`) explicitly, and add a catch-all plugin that marks any
 * `@treaty/*` specifier external, so the bundle keeps them as real imports of
 * their own (CJS) dist where the native `require` still works.
 */

import * as esbuild from 'esbuild'
import { execFileSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { dirname, resolve } from 'node:path'
import { existsSync, rmSync } from 'node:fs'

const here = dirname(fileURLToPath(import.meta.url))
const repoRoot = resolve(here, '..', '..', '..')

const watch = process.argv.includes('--watch')
const minify = process.argv.includes('--minify')

/**
 * Every `@treaty/*` runtime dependency this plugin imports (directly or
 * transitively from its `dist`) must stay external. `@treaty/authoring-node` is
 * the native Rust addon `@treaty/ts-vite` loads via `require`; keeping ts-vite
 * external keeps that require intact, but we list the addon too so a direct
 * import would also stay external.
 */
const TREATY_EXTERNALS = [
	'@treaty/ts-vite',
	'@treaty/authoring-node',
	'@treaty/compiler',
	'@treaty/module-federation',
]

/**
 * Catch-all: mark ANY `@treaty/*` specifier external, even ones not enumerated
 * above. This is the belt-and-suspenders guard that prevents a future `@treaty/*`
 * import from being silently inlined (and thus turning a transitive native
 * `require` into a throwing dynamic-require shim).
 */
const externalizeTreaty = {
	name: 'externalize-treaty-packages',
	/** @param {import('esbuild').PluginBuild} build */
	setup(build) {
		build.onResolve({ filter: /^@treaty\// }, (args) => ({
			path: args.path,
			external: true,
		}))
	},
}

/** @type {import('esbuild').BuildOptions} */
const options = {
	entryPoints: [resolve(here, 'src/index.ts')],
	outfile: resolve(here, 'dist/index.js'),
	bundle: true,
	platform: 'node',
	format: 'esm',
	target: 'node20',
	sourcemap: true,
	minify,
	// Keep node_modules-style bare deps external (vite, esbuild, rollup, the
	// optional @module-federation/vite peer, node builtins, ...). The explicit
	// list + plugin below cover the @treaty/* deps that resolve under libs/ and
	// would otherwise be inlined despite this flag.
	packages: 'external',
	external: TREATY_EXTERNALS,
	plugins: [externalizeTreaty],
	logLevel: 'info',
}

/**
 * Emit the `.d.ts` (+ maps) with tsgo — never tsc. tsgo is the project's native
 * (Go) TypeScript compiler; it is a platform binary, so we resolve the
 * @typescript/native-preview shim and run it with `--emitDeclarationOnly`
 * (esbuild already produced the JS). The lib tsconfig sets `declaration` and
 * `declarationMap`.
 */
function emitDeclarations() {
	const tsgo = process.platform === 'win32' ? 'tsgo.exe' : 'tsgo'
	const bin = resolve(repoRoot, 'node_modules', '.bin', tsgo)
	const tsconfig = resolve(here, 'tsconfig.lib.json')
	if (!existsSync(bin)) {
		throw new Error(
			`[treaty-vite] tsgo not found at ${bin}; cannot emit declarations (tsc is forbidden).`,
		)
	}
	execFileSync(bin, ['--emitDeclarationOnly', '-p', tsconfig], {
		cwd: here,
		stdio: 'inherit',
	})
}

if (watch) {
	const ctx = await esbuild.context(options)
	await ctx.watch()
	console.log('[treaty-vite] watching for changes...')
} else {
	// Start from a clean dist so an earlier (e.g. legacy tsc) build leaves no
	// orphaned per-module `.js` (server-chunks is bundled INTO index.js; the
	// package `exports` expose only `.`). Declarations are re-emitted below.
	rmSync(resolve(here, 'dist'), { recursive: true, force: true })
	await esbuild.build(options)
	emitDeclarations()
	console.log('[treaty-vite] build complete (dist/index.js ESM + declarations).')
}
