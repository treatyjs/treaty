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

import { fileURLToPath } from 'node:url'
import { isAbsolute, resolve as resolvePath } from 'node:path'
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
 * would be thrown away on each call. The key is the serialized COMPILER options
 * (the loader-only {@link TreatyLoaderOptions.selectorRoot} is excluded from the
 * key so the loader and the plugin — which forwards the same options — share ONE
 * compiler instance whether or not `selectorRoot` is set, and so the project
 * selector registry the loader prewarms on that instance is the very one every
 * per-file `transform` reads).
 */
const compilers = new Map<string, TreatyCompiler>()

/**
 * Tracks, per shared compiler instance, the absolute selector-scan root that has
 * already been prewarmed — so the one-time project scan runs at most ONCE per root
 * across the thousands of per-file loader invocations a build makes (mirroring the
 * idempotent `buildStart` prewarm `@treaty/vite`/`@treaty/rolldown` run). A
 * `WeakMap` so a discarded compiler (and its remembered root) is collectable.
 */
const prewarmedRoots = new WeakMap<TreatyCompiler, string>()

function compilerFor(options: TreatyLoaderOptions): TreatyCompiler {
	const key = stableKey(options)
	let compiler = compilers.get(key)
	if (compiler === undefined) {
		compiler = createTreatyCompiler(compilerOptions(options))
		compilers.set(key, compiler)
	}
	return compiler
}

/**
 * The {@link TreatyCompilerOptions} subset to pass to the compiler factory — the
 * loader-only {@link TreatyLoaderOptions.selectorRoot} stripped out so it neither
 * reaches `createTreatyCompiler` (which does not understand it) nor perturbs the
 * shared-compiler key.
 */
function compilerOptions(options: TreatyLoaderOptions): TreatyLoaderOptions {
	const { selectorRoot: _selectorRoot, ...rest } = options
	return rest
}

/** Deterministic key for an options object (sorted keys, stable across calls). */
function stableKey(options: TreatyLoaderOptions): string {
	const entries = Object.entries(compilerOptions(options) as Record<string, unknown>)
		.filter(([, v]) => v !== undefined)
		.sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
	return JSON.stringify(entries)
}

/**
 * Resolve the configured {@link TreatyLoaderOptions.selectorRoot} to the absolute
 * directory to scan, or `null` when cross-module selector resolution is off.
 * `true` ⇒ the current working directory; a relative string ⇒ resolved against
 * the cwd; an absolute string ⇒ used as-is; `false`/omitted ⇒ `null`. Mirrors the
 * `@treaty/rolldown` resolution (cwd-relative — the loader has no Vite build root).
 */
function resolveSelectorRoot(selectorRoot: string | boolean | undefined): string | null {
	if (selectorRoot === undefined || selectorRoot === false) return null
	if (selectorRoot === true) return process.cwd()
	return isAbsolute(selectorRoot) ? selectorRoot : resolvePath(process.cwd(), selectorRoot)
}

/**
 * CROSS-MODULE SELECTOR PREWARM. Scan the configured project root's first-party
 * `.ts` ONCE into the shared compiler's project-wide `className -> selector` map,
 * so every subsequent per-file `transform` auto-derives that file's
 * `{ importName -> selector }` registry and resolves an IMPORTED child used by its
 * conventional `@Component` selector (`<app-stat-card>` for `class StatCard`).
 *
 * Idempotent and tolerant — exactly the contract of the `@treaty/vite`/`@treaty/rolldown`
 * `buildStart` prewarm, but driven from the loader because Rspack/webpack is a pull
 * pipeline with no cold-build hook: the scan runs at most once per (compiler, root),
 * and a missing/empty root yields an empty map so every file folds as before. No-op
 * when `selectorRoot` is not configured (strictly ADDITIVE).
 */
function prewarmSelectors(compiler: TreatyCompiler, options: TreatyLoaderOptions): void {
	const root = resolveSelectorRoot(options.selectorRoot)
	if (root === null) return
	if (prewarmedRoots.get(compiler) === root) return
	prewarmedRoots.set(compiler, root)
	compiler.prewarmSelectorRegistry(root)
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
	// CROSS-MODULE SELECTOR PREWARM (idempotent). Scan the project's `.ts` ONCE on
	// the shared compiler so this — and every later — per-file transform resolves an
	// imported child used by its real `@Component` selector. Mirrors the `@treaty/vite`
	// `buildStart` prewarm; here it rides the first owned-file loader call instead,
	// since Rspack has no cold-build hook. No-op when `selectorRoot` is unset.
	prewarmSelectors(compiler, options)

	let result: TransformResult | null
	try {
		// `transform` derives THIS file's `{ importName -> selector }` registry from the
		// prewarmed project map (passing no explicit registry takes the auto-derive path),
		// so an imported child used by its conventional selector resolves cross-file. With
		// no prewarm the derive yields nothing and the output is byte-identical to before.
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

/**
 * Absolute path to this loader module, for use in a `rules[].use.loader` entry.
 * Uses `fileURLToPath` rather than `new URL(...).pathname` so the path is a real
 * OS path on every platform — on Windows `.pathname` yields a leading-slash
 * `/C:/...` string that Rspack cannot resolve as a loader.
 */
export const loaderPath = fileURLToPath(import.meta.url)
