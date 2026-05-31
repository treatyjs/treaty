// @ts-check
/**
 * @module
 *
 * esbuild bundler for the Treaty VS Code extension. It bundles
 * `src/extension.ts` into `dist/extension.js` as a Node CommonJS module with
 * `vscode` left external (provided by the editor at runtime).
 *
 * The `@treaty/lsp` server is loaded at runtime via `require.resolve`, not
 * bundled, so its NAPI authoring addon (`@treaty/authoring-node`, a native
 * `.node` binary) is never pulled into the bundle.
 *
 * Flags:
 *   --watch    rebuild on change
 *   --minify   minify the output (for `vsce package`)
 */

import * as esbuild from 'esbuild'
import { fileURLToPath } from 'node:url'
import { dirname, resolve } from 'node:path'

const here = dirname(fileURLToPath(import.meta.url))
const projectRoot = resolve(here, '..')

const watch = process.argv.includes('--watch')
const minify = process.argv.includes('--minify')

/** @type {import('esbuild').BuildOptions} */
const options = {
	entryPoints: [resolve(projectRoot, 'src/extension.ts')],
	outfile: resolve(projectRoot, 'dist/extension.js'),
	bundle: true,
	platform: 'node',
	format: 'cjs',
	target: 'node20',
	sourcemap: true,
	minify,
	// `vscode` is provided by the editor. The Treaty LSP server is resolved at
	// runtime via require.resolve and launched as a child process, so it (and
	// its native NAPI authoring addon) must stay external — never bundled.
	external: ['vscode', '@treaty/lsp', '@treaty/lsp/*', 'treaty-lsp'],
	logLevel: 'info',
}

if (watch) {
	const ctx = await esbuild.context(options)
	await ctx.watch()
	console.log('[treaty-vscode] watching for changes...')
} else {
	await esbuild.build(options)
}
