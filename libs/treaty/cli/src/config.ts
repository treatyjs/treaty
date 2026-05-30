/**
 * @module
 *
 * Convention-based project configuration for the standalone Treaty CLI. The CLI
 * assumes there is **no `angular.json`**: a Treaty project is described either by
 * a tiny optional `treaty.config.{js,mjs,ts,json}` file or — when that is absent
 * — purely by conventions (an `index.html` + `src/main.ts` entry, output to
 * `dist/`). Either way the developer configures next to nothing.
 *
 * Treaty is a compiler, not a host: this module only resolves *where* things are
 * and *how* the app federates. It never reimplements the bundler — the dev/build
 * commands hand the resolved config to the `@treaty/{vite,rspack}` plugins, which
 * wire automatic Module Federation.
 */

import { createRequire } from 'node:module'
import { pathToFileURL } from 'node:url'
import { existsSync } from 'node:fs'
import { isAbsolute, resolve as resolvePath } from 'node:path'
import type { MfOptions } from '@treaty/module-federation'
import type { TreatyCompilerOptions } from '@treaty/compiler'

/** The bundler the CLI drives. Defaults to Vite (the zero-config dev choice). */
export type Bundler = 'vite' | 'rspack'

/**
 * The shape of a `treaty.config` file (every field optional). This is the only
 * config a Treaty app ever needs, and most apps need none of it — the defaults
 * and conventions cover a standalone, federation-ready app.
 */
export interface TreatyConfig {
	/** Which bundler `dev`/`build` should use. Defaults to {@link DEFAULT_BUNDLER}. */
	readonly bundler?: Bundler
	/** Project root, relative to the config file (or cwd). Defaults to the cwd. */
	readonly root?: string
	/** The app's entry module. Defaults to {@link DEFAULT_ENTRY}. */
	readonly entry?: string
	/** The dir built output is emitted to. Defaults to {@link DEFAULT_OUT_DIR}. */
	readonly outDir?: string
	/** The public base path for built assets. Defaults to `/`. */
	readonly base?: string
	/** Dev-server host. Defaults to {@link DEFAULT_HOST}. */
	readonly host?: string
	/** Dev-server port. Defaults to {@link DEFAULT_PORT}. */
	readonly port?: number
	/**
	 * Automatic Module Federation options. Every Treaty app is a federation host
	 * automatically; this only declares the app name, the remotes it consumes, the
	 * modules it exposes, and any extra shared deps. Omit it for the zero-config
	 * defaults (a host sharing the Angular runtime as eager singletons). Set to
	 * `false` to opt out of federation entirely.
	 */
	readonly moduleFederation?: MfOptions | boolean
	/** Treaty compiler knobs forwarded verbatim to the plugin. */
	readonly compiler?: TreatyCompilerOptions
}

/**
 * A fully-resolved config: every field present, all paths absolute. This is the
 * single shape the dev/build commands consume so they never re-apply defaults.
 */
export interface ResolvedConfig {
	readonly bundler: Bundler
	readonly root: string
	readonly entry: string
	readonly outDir: string
	readonly base: string
	readonly host: string
	readonly port: number
	readonly moduleFederation: MfOptions | boolean
	readonly compiler: TreatyCompilerOptions
	/** Absolute path of the config file that was loaded, or `null` for conventions. */
	readonly configFile: string | null
}

/** Default bundler when neither config nor `--bundler` selects one. */
export const DEFAULT_BUNDLER: Bundler = 'vite'

/** Default app entry module (a standalone Angular bootstrap). */
export const DEFAULT_ENTRY = 'src/main.ts'

/** Default build output directory. */
export const DEFAULT_OUT_DIR = 'dist'

/** Default dev-server host. */
export const DEFAULT_HOST = 'localhost'

/** Default dev-server port. */
export const DEFAULT_PORT = 4200

/** The config-file basenames the CLI looks for, in resolution order. */
export const CONFIG_FILENAMES: readonly string[] = [
	'treaty.config.ts',
	'treaty.config.mjs',
	'treaty.config.js',
	'treaty.config.json',
]

/** Resolve a possibly-relative path against a base directory to an absolute one. */
function toAbsolute(base: string, target: string): string {
	return isAbsolute(target) ? target : resolvePath(base, target)
}

/**
 * Locate the project's config file under `root`, or `null` if none exists. The
 * first matching {@link CONFIG_FILENAMES} entry wins. Pure filesystem probing —
 * it does not load the file.
 */
export function findConfigFile(root: string): string | null {
	for (const name of CONFIG_FILENAMES) {
		const candidate = resolvePath(root, name)
		if (existsSync(candidate)) return candidate
	}
	return null
}

/**
 * Load and return the raw {@link TreatyConfig} authored in a config file. JSON
 * files are required via Node's JSON support; `.js`/`.mjs` are imported as ESM
 * (their default export, or the module namespace, is taken). A `.ts` config is
 * supported only when the runtime can import TS (e.g. under a loader); otherwise
 * the caller should pre-build it. The function unwraps a `default` export and a
 * `defineConfig(...)`-style function export (called with no args).
 */
export async function loadConfigFile(file: string): Promise<TreatyConfig> {
	if (file.endsWith('.json')) {
		const require = createRequire(import.meta.url)
		return require(file) as TreatyConfig
	}
	const mod: Record<string, unknown> = await import(pathToFileURL(file).href)
	const candidate = (mod['default'] ?? mod) as unknown
	// Support `export default defineConfig(() => ({...}))` style factory exports.
	if (typeof candidate === 'function') {
		return (candidate as () => TreatyConfig)()
	}
	return candidate as TreatyConfig
}

/** Options that override file/convention config (typically parsed from argv). */
export interface ConfigOverrides {
	readonly bundler?: Bundler
	readonly root?: string
	readonly outDir?: string
	readonly base?: string
	readonly host?: string
	readonly port?: number
	/** An explicit config-file path; skips auto-discovery when set. */
	readonly configFile?: string
}

/**
 * Resolve the effective {@link ResolvedConfig} for a command. Precedence, lowest
 * to highest: built-in defaults → loaded `treaty.config` → CLI overrides. The
 * `cwd` seeds the root when nothing else specifies it. Paths in the result are
 * absolute; federation/compiler options pass through untouched (their own
 * defaults live in `@treaty/module-federation` / `@treaty/compiler`).
 */
export async function resolveConfig(
	cwd: string,
	overrides: ConfigOverrides = {}
): Promise<ResolvedConfig> {
	const initialRoot = toAbsolute(cwd, overrides.root ?? '.')

	const configFile = overrides.configFile
		? toAbsolute(cwd, overrides.configFile)
		: findConfigFile(initialRoot)
	const fileConfig: TreatyConfig = configFile ? await loadConfigFile(configFile) : {}

	// The config file may itself relocate the root; re-anchor against it.
	const root = fileConfig.root
		? toAbsolute(initialRoot, fileConfig.root)
		: initialRoot

	const bundler = overrides.bundler ?? fileConfig.bundler ?? DEFAULT_BUNDLER

	return {
		bundler,
		root,
		entry: toAbsolute(root, fileConfig.entry ?? DEFAULT_ENTRY),
		outDir: toAbsolute(root, overrides.outDir ?? fileConfig.outDir ?? DEFAULT_OUT_DIR),
		base: overrides.base ?? fileConfig.base ?? '/',
		host: overrides.host ?? fileConfig.host ?? DEFAULT_HOST,
		port: overrides.port ?? fileConfig.port ?? DEFAULT_PORT,
		moduleFederation: fileConfig.moduleFederation ?? true,
		compiler: fileConfig.compiler ?? {},
		configFile,
	}
}

/**
 * Type-only identity helper so a `treaty.config.ts` can be authored with full
 * inference: `export default defineConfig({ ... })`. Mirrors the `defineConfig`
 * convention of Vite/Vitest so the API feels familiar.
 */
export function defineConfig(config: TreatyConfig): TreatyConfig {
	return config
}
