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

/** Minimal shape of the loader `this` context we depend on. */
interface LoaderContext {
	readonly resourcePath: string
	getOptions?: () => TreatyPluginOptions
	readonly query?: TreatyPluginOptions | string
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
	return result ? result.code : source
}
