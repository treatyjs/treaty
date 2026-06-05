/**
 * @module
 *
 * The Angular partial-declaration linker, wired as a webpack/rspack loader.
 *
 * Published Angular libraries (`node_modules/@angular/* /fesm2022/*.mjs`) ship *partial*-compiled:
 * every decorated class emits a `ɵɵngDeclare*({...})` call. Left un-linked, Angular falls back to
 * the JIT compiler at runtime and throws "needs JIT / `@angular/compiler` not available" the moment
 * `@angular/compiler` is excluded. This loader rewrites those declarations to their AOT
 * `ɵɵdefine*` form so NO JIT and NO `@angular/compiler` are needed.
 *
 * It reuses the EXACT shared linker core from `@treaty/ts-vite`
 * ({@link isPartialModule} + {@link linkPartialCode}) — the same bundler-agnostic, Rust-backed
 * (`@treaty/authoring-node`.`linkPartial`) functions `@treaty/vite` consumes. Only the per-bundler
 * registration differs: Vite uses a `transform` plugin, rspack/webpack uses this loader. No linking
 * logic is re-implemented here.
 *
 * Treaty is a compiler, not a host: this loader is a thin shim (guard + delegate). For modules that
 * are not partial-compiled (or when the linker addon is unavailable) it passes the source through
 * unchanged so the rest of the loader chain still runs.
 */

import { fileURLToPath } from 'node:url'
import { isPartialModule, linkPartialCode } from '@treaty/ts-vite'

/**
 * The slice of the webpack/rspack loader context this loader relies on. Declared structurally so
 * the package typechecks without the (peer-only) `@rspack/core` types installed; the real Rspack
 * `LoaderContext` is assignable to this.
 */
export interface LinkPartialLoaderContext {
	/** Absolute path of the module being loaded (without query/fragment). */
	readonly resourcePath: string
	/** Full resource string including any `?query#fragment`, when present. */
	readonly resource?: string
	/**
	 * Synchronous result callback. Present on real loader contexts; the loader uses it when
	 * available and otherwise returns the value directly.
	 */
	callback?: (error: Error | null | undefined, content?: string, sourceMap?: unknown) => void
}

/**
 * Loader signature compatible with webpack/rspack: called with the source (and an optional incoming
 * source map) and `this` bound to the loader context.
 */
export type LinkPartialLoader = (
	this: LinkPartialLoaderContext,
	source: string,
	map?: unknown
) => string | void

/**
 * The linker loader. For a partial-compiled `node_modules` Angular module it returns the linked
 * (AOT) source; for everything else it returns the source unchanged. Linker diagnostics surface as
 * a loader error so the build fails loudly rather than shipping un-linked (JIT-dependent) output.
 *
 * Linking is a span rewrite that preserves byte offsets outside the rewritten `ɵɵngDeclare*` calls,
 * so an incoming source map (`map`) is forwarded as-is rather than invalidated.
 */
export const linkPartialLoader: LinkPartialLoader = function linkPartialLoader(
	this: LinkPartialLoaderContext,
	source: string,
	map?: unknown
): string | void {
	const id = this.resource ?? this.resourcePath

	let linked: string | null
	try {
		linked = linkPartialCode(source, id)
	} catch (error) {
		return report(this, error instanceof Error ? error : new Error(String(error)), source, map)
	}

	// Not a partial module (or the linker addon is unavailable): pass through unchanged.
	const out = linked === null ? source : linked
	return report(this, null, out, map)
}

/**
 * Deliver a loader result through the context's result callback when present; otherwise fall back
 * to the loader's synchronous return / throw contract (for a minimal/test context).
 */
function report(
	ctx: LinkPartialLoaderContext,
	error: Error | null,
	content?: string,
	map?: unknown
): string | void {
	if (typeof ctx.callback === 'function') {
		ctx.callback(error, content, map)
		return
	}
	if (error) throw error
	return content
}

export default linkPartialLoader

/**
 * Absolute path to this loader module, for use in a `rules[].use.loader` entry.
 * Uses `fileURLToPath` (not `new URL(...).pathname`) so the path is a real OS path
 * on Windows too — `.pathname` would yield an unresolvable leading-slash `/C:/...`.
 */
export const linkPartialLoaderPath = fileURLToPath(import.meta.url)

/**
 * The module-rule `test` for the linker loader: `node_modules` `.mjs`/`.js`/`.cjs` files. The cheap
 * per-module {@link isPartialModule} substring guard inside {@link linkPartialCode} keeps the
 * (more expensive) link off any matched file that is not actually partial-compiled.
 *
 * Re-exported alongside the loader so {@link isPartialModule} stays the single shared partial
 * detector across every Treaty bundler.
 */
export const LINK_PARTIAL_TEST: RegExp = /[\\/]node_modules[\\/].*\.[cm]?js$/

export { isPartialModule }
