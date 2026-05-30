/**
 * @module
 *
 * `@treaty/vite` — the Vite plugin for Treaty authoring formats. It wires the
 * framework-agnostic {@link TreatyCompiler} core from `@treaty/compiler` into
 * Vite's plugin lifecycle so that `.treaty`, `.tsx`, `.tjsx`, and Angular
 * `@Component` `.ts` files are lowered to Ivy JS during dev and build.
 *
 * Treaty is a compiler, not a host: this plugin does not reimplement any
 * lowering. It delegates every owned file to the core's `transform`, which in
 * turn routes through the Rust authoring compiler. The plugin's job is purely
 * Vite integration: extension ownership, esbuild/resolve configuration, the
 * incremental cache, and hot-update / deletion handling.
 */

import { readFile } from 'node:fs/promises'
import {
	createTreatyCompiler,
	classify,
	type TransformInput,
	type TreatyCompiler,
	type TreatyCompilerOptions,
} from '@treaty/compiler'
import {
	toViteFederation,
	type MfOptions,
	type ViteFederationOptions,
} from '@treaty/module-federation'
import type { Plugin } from 'vite'

/** Public options for {@link treaty}. */
export interface PluginOptions extends TreatyCompilerOptions {
	/**
	 * Emit a JSON source map alongside the transformed code when the core
	 * produces one. Defaults to `true`. When `false`, a null map is returned so
	 * Vite skips source-map work for Treaty modules.
	 */
	readonly sourceMap?: boolean
	/**
	 * Force `esbuild` to treat the listed extensions with the given loader so
	 * Vite's dependency optimizer and esbuild passes do not choke on the JSX
	 * authoring extensions this plugin owns. Defaults to mapping `.tjsx` to the
	 * `tsx` loader (`.tsx` is already known to esbuild).
	 */
	readonly esbuildLoaders?: Readonly<Record<string, 'ts' | 'tsx' | 'js' | 'jsx'>>
	/**
	 * Cold-build prewarm: a list of absolute paths to owned authoring files to
	 * batch-compile up front via the core's `transformMany` (one parallel round
	 * trip through the Rust addon). Only runs for a one-shot `build` (not dev),
	 * during `buildStart`. The results populate the incremental cache, so the
	 * per-module `transform` calls Vite makes during the build are served as cache
	 * hits instead of re-entering the compiler one file at a time.
	 *
	 * Vite/Rollup is a pull-based pipeline with no hook that hands the plugin the
	 * full owned-file set, so this batch path is opt-in: pass the entry/owned
	 * authoring files you want compiled eagerly. When omitted, the plugin uses
	 * per-file `transform` only (the default, and the path used for incremental
	 * dev rebuilds regardless of this option).
	 */
	readonly prewarm?: readonly string[]
	/**
	 * Automatic Module Federation. Every Treaty app is a Module Federation host
	 * by default — Treaty generates the federation config from these options so
	 * the developer writes no `federation()`/`ModuleFederationPlugin` by hand.
	 *
	 *   - `true` / omitted via {@link treatyWithFederation}: enable with defaults
	 *     (the app is a host that shares the Angular runtime as eager singletons).
	 *   - an {@link MfOptions} object: configure the app name, the remotes it
	 *     consumes, the modules it exposes, and extra shared deps.
	 *   - `false`: disable federation entirely.
	 *
	 * The base {@link treaty} factory does not apply federation (so existing
	 * single-plugin usage is unchanged); use {@link treatyWithFederation} to get
	 * the Treaty plugin and the auto-generated federation plugin together.
	 */
	readonly moduleFederation?: MfOptions | boolean
}

/**
 * The `@module-federation/vite` `federation()` factory, declared structurally so
 * `@treaty/vite` typechecks (and the base plugin runs) without the peer package
 * installed. The real default export is assignable to this.
 */
type ViteFederationFactory = (options: ViteFederationOptions) => Plugin

/** The plugin name surfaced in Vite logs and the plugin pipeline. */
const PLUGIN_NAME = 'treaty:vite'

/** Default esbuild loader assignments for Treaty's JSX authoring extensions. */
const DEFAULT_ESBUILD_LOADERS: Readonly<Record<string, 'ts' | 'tsx' | 'js' | 'jsx'>> = {
	'.tjsx': 'tsx',
}

/** Strip a bundler-appended query/hash suffix (`?foo`, `#bar`) from an id. */
function cleanId(id: string): string {
	return id.replace(/[?#].*$/, '')
}

/**
 * Whether the resolved id is one this plugin should attempt to transform. We
 * rely on the core's {@link classify} so ownership stays in one place; a plain
 * `.ts` is only fully claimed by the core's `transform` (which screens for an
 * `@Component` decorator and returns `null` otherwise).
 */
function isCandidate(id: string): boolean {
	return classify(cleanId(id)) !== null
}

/**
 * Create the Treaty Vite plugin. Returns a single {@link Plugin} object that
 * delegates all lowering to the shared {@link TreatyCompiler} core.
 */
export default function treaty(options: PluginOptions = {}): Plugin {
	const emitSourceMap = options.sourceMap ?? true
	const esbuildLoaders = options.esbuildLoaders ?? DEFAULT_ESBUILD_LOADERS
	const prewarmFiles = options.prewarm ?? []

	const compiler: TreatyCompiler = createTreatyCompiler(options)
	// Set by configResolved; gates the cold-build-only prewarm in buildStart.
	let isColdBuild = false

	return {
		name: PLUGIN_NAME,
		// Run before Vite's core TS/esbuild handling so authoring files reach the
		// Treaty compiler as their original source rather than esbuild output.
		enforce: 'pre',

		/**
		 * Teach esbuild about Treaty's JSX authoring extensions. Without this the
		 * dependency optimizer / esbuild transform pass would not know how to read
		 * `.tjsx` files; `.treaty` files are never handed to esbuild because this
		 * plugin transforms them first.
		 */
		config() {
			return {
				optimizeDeps: {
					esbuildOptions: {
						loader: { ...esbuildLoaders },
					},
				},
			}
		},

		/**
		 * Capture whether we are building so the cache can be left enabled in dev
		 * (where re-transforms are common) and the core's defaults otherwise.
		 */
		configResolved(resolved) {
			// One-shot production builds gain nothing from a stale in-memory cache;
			// clear it so a fresh build never serves a stale dev entry. Also record
			// that this is a cold build so `buildStart` may batch-prewarm.
			isColdBuild = resolved.command === 'build'
			if (isColdBuild) compiler.clearCache()
		},

		/**
		 * Cold-build batch prewarm. On a one-shot `build`, read the configured
		 * {@link PluginOptions.prewarm} files and lower them in a single
		 * `transformMany` round trip so the per-module `transform` calls Vite makes
		 * during the build are cache hits. No-op in dev or when nothing is listed —
		 * incremental rebuilds always use per-file `transform`.
		 */
		async buildStart() {
			if (!isColdBuild || prewarmFiles.length === 0) return
			const inputs: TransformInput[] = []
			for (const file of prewarmFiles) {
				const id = cleanId(file)
				if (!isCandidate(id)) continue
				try {
					inputs.push({ id, code: await readFile(file, 'utf8') })
				} catch {
					// A missing/unreadable prewarm entry is skipped; the per-file
					// transform (or Vite's own resolver) will surface any real error.
				}
			}
			if (inputs.length > 0) compiler.transformMany(inputs)
		},

		/**
		 * Resolve bare/relative `.treaty` (and other owned) imports so that an
		 * importing module's `import x from './foo.treaty'` keeps a stable id that
		 * this plugin's `transform` then owns. We only intervene for ids that carry
		 * an owned extension and are not already absolute/virtual, deferring the
		 * actual path resolution to Vite via `this.resolve`.
		 */
		async resolveId(source, importer, resolveOptions) {
			if (!isCandidate(source)) return null
			// Avoid infinite recursion: skip ids we have already resolved.
			const resolved = await this.resolve(source, importer, {
				...resolveOptions,
				skipSelf: true,
			})
			return resolved ? resolved.id : null
		},

		/**
		 * The heart of the plugin: hand owned files to the core compiler and return
		 * Vite's `{ code, map }` shape. Files the core does not own (it returns
		 * `null`) fall through to Vite's normal pipeline untouched.
		 */
		transform(code, id) {
			if (!isCandidate(id)) return null
			const result = compiler.transform(cleanId(id), code)
			if (result === null) return null
			return {
				code: result.code,
				map: emitSourceMap && result.map !== undefined ? result.map : null,
			}
		},

		/**
		 * Re-transform changed authoring files and propagate deletions through the
		 * core's `onDelete`. On a normal change we invalidate the incremental cache
		 * entry so the next `transform` recompiles; on a delete we evict the file
		 * and additionally invalidate every module that imported it so Vite picks
		 * up the now-broken (or changed) reference.
		 */
		async handleHotUpdate(ctx) {
			const file = cleanId(ctx.file)
			if (!isCandidate(file)) return

			let exists = true
			try {
				await ctx.read()
			} catch {
				// `read()` throwing signals the file is gone (deleted/renamed).
				exists = false
			}

			if (!exists) {
				const dependents = compiler.onDelete(file)
				const affected = [...ctx.modules]
				const graph = ctx.server.moduleGraph
				for (const depId of dependents) {
					for (const mod of graph.getModulesByFile(depId) ?? []) {
						affected.push(mod)
					}
				}
				return affected
			}

			// Changed-in-place: drop the stale cache entry so the reload recompiles.
			compiler.invalidate(file)
			return ctx.modules
		},
	}
}

/**
 * Resolve the user's `moduleFederation` option to the concrete
 * {@link MfOptions} when federation is enabled, or `null` when it is disabled.
 * `true` (and the default within {@link treatyWithFederation}) ⇒ defaults `{}`.
 */
function resolveMfOptions(value: MfOptions | boolean | undefined): MfOptions | null {
	if (value === false) return null
	if (value === true || value === undefined) return {}
	return value
}

/**
 * Build the auto-generated `@module-federation/vite` plugin for the given
 * Treaty options. Returns a promise so the optional peer is loaded lazily — the
 * Treaty plugin itself never depends on `@module-federation/vite` being present
 * unless federation is actually used. Vite accepts a `Promise<Plugin>` entry in
 * its `plugins` array, so the returned value can be placed there directly.
 */
async function createFederationPlugin(mf: MfOptions): Promise<Plugin> {
	const options = toViteFederation(mf)
	// Loaded by specifier so bundlers do not eagerly require the optional peer.
	const mod: { default?: unknown; federation?: unknown } = await import(
		'@module-federation/vite'
	)
	const factory = (mod.federation ?? mod.default) as ViteFederationFactory | undefined
	if (typeof factory !== 'function') {
		throw new Error(
			'@treaty/vite: Module Federation is enabled but "@module-federation/vite" ' +
				'did not export a federation() factory. Install @module-federation/vite to use auto-MF.'
		)
	}
	return factory(options)
}

/**
 * Treaty's Vite integration **with automatic Module Federation**: returns the
 * Treaty authoring plugin plus the auto-generated `@module-federation/vite`
 * plugin, so every Treaty app is a federation host with zero config. Pass
 * `moduleFederation` to declare remotes/exposes/shared; omit it to get the
 * defaults (host that shares the Angular runtime as eager singletons). Set
 * `moduleFederation: false` to opt out (equivalent to plain {@link treaty}).
 *
 * The federation plugin entry is a `Promise<Plugin>` (Vite supports this), so
 * the optional `@module-federation/vite` peer is only loaded when MF is on.
 */
export function treatyWithFederation(
	options: PluginOptions = {}
): Array<Plugin | Promise<Plugin>> {
	const mf = resolveMfOptions(options.moduleFederation ?? true)
	const plugins: Array<Plugin | Promise<Plugin>> = [treaty(options)]
	if (mf !== null) plugins.push(createFederationPlugin(mf))
	return plugins
}

export { toViteFederation, generateMfConfig } from '@treaty/module-federation'
export type { MfOptions, NormalizedMfConfig } from '@treaty/module-federation'
export { createTreatyCompiler } from '@treaty/compiler'
export type { TreatyCompilerOptions } from '@treaty/compiler'
