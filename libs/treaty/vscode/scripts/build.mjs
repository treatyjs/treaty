// @ts-check
/**
 * @module
 *
 * esbuild bundler for the Treaty VS Code extension. It produces two artifacts:
 *
 *   - `dist/extension.js` — the extension client (`src/extension.ts`) as a Node
 *     CommonJS module with `vscode` left external (provided by the editor).
 *   - `dist/server.mjs`   — the `@treaty/lsp` volarjs language server, bundled
 *     as a Node ESM module (the server uses `import.meta.url`), launched by the
 *     extension over IPC. Bundling it makes a packaged VSIX self-contained.
 *
 * The `@treaty/authoring-node` NAPI addon (a native `.node` binary) is left
 * external in BOTH bundles: the server loads it at runtime through its own
 * `require`, and the extension's compile/preview commands resolve it the same
 * way, so the native binary is never pulled into a JS bundle.
 *
 * Flags:
 *   --watch    rebuild on change
 *   --minify   minify the output (for `vsce package`)
 */

import * as esbuild from 'esbuild'
import { fileURLToPath } from 'node:url'
import { dirname, resolve } from 'node:path'
import { existsSync } from 'node:fs'

const here = dirname(fileURLToPath(import.meta.url))
const projectRoot = resolve(here, '..')

const watch = process.argv.includes('--watch')
const minify = process.argv.includes('--minify')

/**
 * The native authoring addon must stay external everywhere — it is a `.node`
 * binary the server (and the extension's commands) resolve at runtime.
 */
const NATIVE_EXTERNAL = ['@treaty/authoring-node']

/** @type {import('esbuild').BuildOptions} */
const extensionOptions = {
	entryPoints: [resolve(projectRoot, 'src/extension.ts')],
	outfile: resolve(projectRoot, 'dist/extension.js'),
	bundle: true,
	platform: 'node',
	format: 'cjs',
	target: 'node20',
	sourcemap: true,
	minify,
	// `vscode` is provided by the editor. The bundled LSP server (`dist/server.mjs`)
	// is launched as a child process and resolved at runtime, so it (and the
	// native addon) must stay external — never bundled into the extension client.
	external: ['vscode', ...NATIVE_EXTERNAL, '@treaty/lsp', '@treaty/lsp/*', 'treaty-lsp'],
	logLevel: 'info',
}

// The server's compiled entry. Bundling the compiled `dist/server.js` (not the
// TS source) keeps this build independent of the LSP package's tsconfig.
const serverEntry = resolve(projectRoot, '../lsp/dist/server.js')

/** @type {import('esbuild').BuildOptions} */
const serverOptions = {
	entryPoints: [serverEntry],
	outfile: resolve(projectRoot, 'dist/server.mjs'),
	bundle: true,
	platform: 'node',
	format: 'esm',
	target: 'node20',
	sourcemap: true,
	minify,
	external: [...NATIVE_EXTERNAL],
	// The server uses `createRequire(import.meta.url)`; preserve a working
	// `require` in the ESM output for its runtime addon/tsdk resolution.
	banner: {
		js: "import { createRequire as ___createRequire } from 'node:module'; const require = ___createRequire(import.meta.url);",
	},
	logLevel: 'info',
}

if (!existsSync(serverEntry)) {
	console.warn(
		`[treaty-vscode] @treaty/lsp server entry not found at ${serverEntry}; ` +
			'build the LSP package first (moon run treaty-lsp:build). Building the extension client only.',
	)
}

const builds = existsSync(serverEntry) ? [extensionOptions, serverOptions] : [extensionOptions]

if (watch) {
	for (const options of builds) {
		const ctx = await esbuild.context(options)
		await ctx.watch()
	}
	console.log('[treaty-vscode] watching for changes...')
} else {
	await Promise.all(builds.map((options) => esbuild.build(options)))
}
