/**
 * @module
 *
 * The Treaty webpack/rspack loader. A loader is a function invoked with the
 * module source as its argument and the loader context as `this`. For files
 * Treaty owns (`.treaty`, `.tsx`, `.tjsx`, and `@Component` `.ts`) it lowers the
 * source to Ivy JS via `@treaty/compiler`; for everything else it passes the
 * source through unchanged so other loaders in the chain still run.
 *
 * Treaty is a compiler, not a host: the loader never lowers anything itself — it
 * delegates to a {@link TreatyCompiler}, which routes to the Rust authoring
 * compiler. A single compiler instance is memoized per option set so the
 * incremental cache survives across module builds in one Rspack run.
 */

import {
	createTreatyCompiler,
	type TransformResult,
	type TreatyCompiler,
} from '@treaty/compiler'
import type { TreatyLoaderOptions } from './options.js'
import {
	emitServerFnChunks,
	registryFor,
	type ServerChunkEmitter,
} from './server-chunks.js'

/**
 * The slice of the webpack/rspack loader context this loader relies on. Declared
 * structurally so the package typechecks without the (peer-only) `@rspack/core`
 * types installed; the real Rspack `LoaderContext` is assignable to this.
 */
export interface TreatyLoaderContext {
	/** Absolute path of the module being loaded (without query/fragment). */
	readonly resourcePath: string
	/** Full resource string including any `?query#fragment`, when present. */
	readonly resource?: string
	/** Parse and return the loader's configured options (typed by the caller). */
	getOptions(): TreatyLoaderOptions
	/** Switch the loader to async mode; the returned callback reports the result. */
	async(): (
		error: Error | null | undefined,
		content?: string,
		sourceMap?: unknown
	) => void
	/**
	 * Synchronous result callback. Present on real loader contexts; the loader
	 * uses it when available and otherwise returns the value directly.
	 */
	callback?: (
		error: Error | null | undefined,
		content?: string,
		sourceMap?: unknown
	) => void
	/**
	 * Emit a build asset (server-fn chunk). Present on real Rspack/webpack loader
	 * contexts; used to write each extracted server fn's body as its own loadable
	 * output file so it never enters the client chunk. Optional so a minimal test
	 * context (or a build with no server fns) need not provide it.
	 */
	emitFile?: ServerChunkEmitter['emitFile']
	/**
	 * The active compilation, when running inside a real build. Used as the key
	 * for the per-compilation server-fn registry the plugin reads to emit the
	 * manifest. Absent on minimal contexts (then a process-wide registry is used).
	 */
	_compilation?: object
}

/**
 * Loader signature compatible with webpack/rspack: called with the source (and
 * an optional incoming source map) and `this` bound to the loader context.
 */
export type TreatyLoader = (
	this: TreatyLoaderContext,
	source: string,
	map?: unknown
) => string | void

/**
 * Cache one {@link TreatyCompiler} per distinct option set. Loaders are plain
 * functions re-entered for every module, so without this the incremental cache
 * would be thrown away on each call. The key is the serialized options.
 */
const compilers = new Map<string, TreatyCompiler>()

function compilerFor(options: TreatyLoaderOptions): TreatyCompiler {
	const key = stableKey(options)
	let compiler = compilers.get(key)
	if (compiler === undefined) {
		compiler = createTreatyCompiler(options)
		compilers.set(key, compiler)
	}
	return compiler
}

/** Deterministic key for an options object (sorted keys, stable across calls). */
function stableKey(options: TreatyLoaderOptions): string {
	const entries = Object.entries(options as Record<string, unknown>)
		.filter(([, v]) => v !== undefined)
		.sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
	return JSON.stringify(entries)
}

/** A parsed source map, when the compiler produced one. */
function parseMap(result: TransformResult): unknown {
	if (result.map === undefined) return undefined
	try {
		return JSON.parse(result.map) as unknown
	} catch {
		// A non-JSON map string is forwarded as-is rather than dropped.
		return result.map
	}
}

/**
 * The Treaty loader. Lowers owned authoring files to Ivy JS; passes everything
 * else through. Errors from the Rust compiler surface as loader errors so the
 * Rspack build fails with the diagnostics rather than emitting broken output.
 */
export const treatyLoader: TreatyLoader = function treatyLoader(
	this: TreatyLoaderContext,
	source: string
): string | void {
	const options = typeof this.getOptions === 'function' ? this.getOptions() : {}
	const id = this.resource ?? this.resourcePath
	const compiler = compilerFor(options)

	let result: TransformResult | null
	try {
		result = compiler.transform(id, source)
	} catch (error) {
		return report(this, error instanceof Error ? error : new Error(String(error)))
	}

	// Not a Treaty-owned module (or a plain `.ts` without `@Component`): pass through.
	if (result === null) return source

	// When the file declared server fns, emit each fn's body as its own loadable
	// chunk asset and rewrite the client module to carry only the lazy bindings —
	// the body never enters the client (Ivy) chunk. Falls back to `result.code`
	// untouched when there are no server fns or no `emitFile` (minimal context).
	const code = emitClientModule(this, result)

	return report(this, null, code, parseMap(result))
}

/**
 * Produce the client module text for a transform result, splitting out server
 * fns when present. Requires `emitFile` (a real build context) to emit the per-fn
 * chunk assets; without it the body-free `result.code` is returned as-is so a
 * minimal/test context still works.
 */
function emitClientModule(ctx: TreatyLoaderContext, result: TransformResult): string {
	if (!result.serverChunks || result.serverChunks.length === 0) return result.code
	if (typeof ctx.emitFile !== 'function') return result.code
	const emitter: ServerChunkEmitter = { emitFile: ctx.emitFile.bind(ctx) }
	// `_compilation` is Rspack/webpack's own loader-context property; the leading
	// underscore is the framework's API name, not ours.
	// oxlint-disable-next-line no-underscore-dangle
	const registry = registryFor(ctx._compilation)
	return emitServerFnChunks(emitter, registry, result)
}

/**
 * Deliver a loader result through the context's result callback when present.
 * On a minimal context (no `callback`), fall back to returning the emitted
 * content directly / throwing the error, which is the loader's sync contract.
 */
function report(
	ctx: TreatyLoaderContext,
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

export default treatyLoader

/** Absolute path to this loader module, for use in a `rules[].use.loader` entry. */
export const loaderPath = new URL(import.meta.url).pathname
