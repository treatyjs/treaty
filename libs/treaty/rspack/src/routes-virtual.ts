/**
 * @module
 *
 * File-system routing as a VIRTUAL MODULE for Rspack/webpack, generated DURING the
 * build (no checked-in / prebuilt `routes.ts`).
 *
 * The routing logic lives ONCE in Rust (`@treaty/authoring-node`.`generateRoutes`,
 * the shim over `treaty_file_routing`), surfaced for every bundler through the
 * SHARED `@treaty/ts-vite` helper ({@link generateRoutesModule} +
 * {@link isTreatyRoutesId}) — the exact same module `@treaty/vite` (Vite) serves.
 * Only the per-bundler registration differs.
 *
 * Rspack/webpack has no `load(id)` hook for a synthetic specifier, so the virtual
 * module is wired with the standard webpack idiom: a `resolve.alias` redirects the
 * bare `virtual:treaty-routes` import to a tiny on-disk SENTINEL entry shipped in
 * this package, and a loader matched to that sentinel REPLACES its (empty) source
 * with the freshly generated routes module. No file is written into the user's
 * project; the sentinel is a stable resolve target only. Each referenced route
 * entry file is added as a loader dependency (`this.addDependency`) so editing a
 * route re-runs the loader in watch mode.
 */

import { generateRoutesModule, type RoutesVirtualModuleOptions } from '@treaty/ts-vite'

export { TREATY_ROUTES_ID } from '@treaty/ts-vite'

/**
 * Absolute path to the on-disk sentinel module the `virtual:treaty-routes` import
 * is aliased to. Its contents are irrelevant — the loader below overwrites them —
 * but it must exist so Rspack/webpack can resolve the import to a real module.
 */
export const routesSentinelPath: string = new URL('./routes-virtual-entry.js', import.meta.url)
	.pathname

/** Absolute path to the routes-generating loader module, for a `rules[].use.loader` entry. */
export const routesLoaderPath: string = new URL('./routes-virtual-loader.js', import.meta.url)
	.pathname

/**
 * The module-rule `test` that selects the sentinel so the routes loader runs over
 * it (and ONLY it). Matched against the resolved sentinel path.
 */
export const ROUTES_SENTINEL_TEST: RegExp = /routes-virtual-entry\.js$/

/**
 * The loader options carrying the file-routing knobs the loader forwards to the
 * shared {@link generateRoutesModule}. Serialised onto the rule's `use.options` by
 * the plugin and read back via `getOptions()` in the loader.
 */
export type RoutesLoaderOptions = RoutesVirtualModuleOptions

/**
 * The slice of the webpack/rspack loader context the routes loader relies on.
 * Declared structurally so the package typechecks without the peer-only
 * `@rspack/core` types; the real `LoaderContext` is assignable to this.
 */
export interface RoutesLoaderContext {
	/** Parse and return the loader's configured {@link RoutesLoaderOptions}. */
	getOptions(): RoutesLoaderOptions
	/** Register a file as a build dependency so a change re-runs this loader (watch). */
	addDependency?(file: string): void
	/** Synchronous result callback present on real loader contexts. */
	callback?: (error: Error | null | undefined, content?: string, sourceMap?: unknown) => void
}

/**
 * The routes loader. Ignores the sentinel's (empty) source and returns the freshly
 * generated routes module produced by the shared Rust-backed core, registering each
 * referenced route entry file as a watch dependency. Generation errors surface as a
 * loader error so the build fails loudly rather than emitting an empty route graph.
 */
export const routesLoader = function routesLoader(
	this: RoutesLoaderContext,
): string | void {
	const options = typeof this.getOptions === 'function' ? this.getOptions() : undefined
	if (options === undefined || typeof options.routesRoot !== 'string') {
		return report(
			this,
			new Error('[treaty] routes loader requires a routesRoot option'),
		)
	}

	let code: string
	let watchFiles: string[]
	try {
		const generated = generateRoutesModule(options)
		code = generated.code
		watchFiles = generated.watchFiles
	} catch (error) {
		return report(this, error instanceof Error ? error : new Error(String(error)))
	}

	if (typeof this.addDependency === 'function') {
		for (const file of watchFiles) this.addDependency(file)
	}
	return report(this, null, code)
}

/**
 * Deliver a loader result through the context callback when present; otherwise fall
 * back to the loader's synchronous return/throw contract (for a minimal/test context).
 */
function report(
	ctx: RoutesLoaderContext,
	error: Error | null,
	content?: string,
): string | void {
	if (typeof ctx.callback === 'function') {
		ctx.callback(error, content)
		return
	}
	if (error) throw error
	return content
}

export default routesLoader
