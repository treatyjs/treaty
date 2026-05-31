/**
 * @module
 *
 * Rspack/webpack-compatible loader that routes a single module through the
 * shared `@treaty/compiler` core. This is the fallback path used when the
 * Rsbuild `api.transform` hook is unavailable: {@link pluginTreaty} registers
 * a module rule whose `use.loader` resolves to this file.
 *
 * Treaty is a compiler, not a host — all lowering to Ivy JS happens in the
 * Rust authoring compiler reached through `@treaty/compiler`. This loader only
 * adapts the bundler's loader contract to a single `compiler.transform(...)`
 * call, sharing one compiler instance across invocations for cache reuse.
 */

import { createTreatyCompiler, type TreatyCompiler } from '@treaty/compiler'
import type { TreatyPluginOptions } from './options.js'
import { toCompilerOptions } from './options.js'
import { serverChunkFileName } from './server-chunks.js'

/** Minimal shape of the loader `this` context we depend on. */
interface LoaderContext {
	readonly resourcePath: string
	getOptions?: () => TreatyPluginOptions
	readonly query?: TreatyPluginOptions | string
	/**
	 * Webpack/Rspack additional-asset emit. Present on the real loader context;
	 * declared optional so the loader still works under the minimal fake context
	 * the smoke tests use. Used to code-split each server fn into its own chunk
	 * file (the body never enters the client bundle).
	 */
	emitFile?(name: string, content: string): void
}

/**
 * One compiler per distinct options signature, keyed by a stable serialization
 * of the core options. Loaders are invoked once per module with no shared
 * lifecycle, so caching the instance here preserves the incremental cache
 * across the whole build.
 */
const compilers = new Map<string, TreatyCompiler>()

function compilerFor(options: TreatyPluginOptions): TreatyCompiler {
	const core = toCompilerOptions(options)
	const key = JSON.stringify([core.cache, core.annotatePure, core.dropUnusedServerFns])
	let compiler = compilers.get(key)
	if (!compiler) {
		compiler = createTreatyCompiler(core)
		compilers.set(key, compiler)
	}
	return compiler
}

function readOptions(ctx: LoaderContext): TreatyPluginOptions {
	if (typeof ctx.getOptions === 'function') return ctx.getOptions() ?? {}
	if (ctx.query && typeof ctx.query === 'object') return ctx.query
	return {}
}

/**
 * The loader entry point. Synchronous: the underlying compiler is synchronous,
 * so returning the transformed source directly is both correct and fastest.
 *
 * @returns Emitted Ivy JS, or the original `source` for modules the compiler
 *   does not own (lets the bundler's normal pipeline handle them).
 */
export default function treatyLoader(this: LoaderContext, source: string): string {
	const options = readOptions(this)
	const compiler = compilerFor(options)
	const result = compiler.transform(this.resourcePath, source)
	if (result === null) return source
	// Code-split each extracted server fn into its own `<id>.server.js` chunk so
	// its body is separately loadable and never ships in the client bundle. The
	// returned `code` is the client module (bodies replaced by client bindings).
	if (result.serverChunks && typeof this.emitFile === 'function') {
		for (const chunk of result.serverChunks) {
			this.emitFile(serverChunkFileName(chunk.id), chunk.code)
		}
	}
	return result.code
}
