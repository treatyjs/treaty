/**
 * @module
 *
 * The Rsbuild plugin: {@link pluginTreaty}. It wires the shared
 * `@treaty/compiler` core into an Rsbuild build so that `.treaty`, `.tsx`, and
 * `.tjsx` authoring modules are lowered to Ivy JS.
 *
 * Two wiring strategies, preferred in order:
 *   1. `api.transform` — Rsbuild's first-class code-transform hook. We register
 *      one transform matched to the Treaty extensions and call the core
 *      compiler directly.
 *   2. `tools.rspack` — the escape hatch used when `api.transform` is absent.
 *      We add an Rspack module rule pointing at {@link ./loader}, which calls
 *      the same core.
 *
 * In both cases `resolve.extensions` is extended (via `modifyRsbuildConfig`) so
 * bare imports of Treaty modules resolve.
 *
 * Treaty is a compiler, not a host: this plugin never reimplements compilation
 * — it only routes matched modules to `@treaty/compiler`.
 */

import { createTreatyCompiler } from '@treaty/compiler'
import type {
	RsbuildConfig,
	RsbuildPlugin,
	RsbuildPluginAPI,
	RspackConfig,
	RspackModuleRule,
} from '@rsbuild/core'
import {
	DEFAULT_TEST,
	TREATY_EXTENSIONS,
	toCompilerOptions,
	type TreatyPluginOptions,
} from './options.js'

/** Stable plugin name, also asserted by the smoke test. */
export const PLUGIN_NAME = 'treaty:rsbuild'

/** Resolve the loader module path used by the `tools.rspack` fallback. */
function loaderPath(): string {
	return new URL('./loader.js', import.meta.url).pathname
}

/** Merge Treaty extensions into an existing `resolve.extensions` array. */
function mergeExtensions(existing: string[] | undefined, add: readonly string[]): string[] {
	const out = existing ? [...existing] : []
	for (const ext of add) if (!out.includes(ext)) out.push(ext)
	return out
}

/**
 * Create the Treaty Rsbuild plugin.
 *
 * @param options - Typed plugin options (see {@link TreatyPluginOptions}). All
 *   fields are optional; sensible defaults match the rest of the Treaty
 *   toolchain (cache on, pure annotation on, unused server fns dropped).
 */
export function pluginTreaty(options: TreatyPluginOptions = {}): RsbuildPlugin {
	const test = options.include ?? DEFAULT_TEST
	const extensions = options.extensions ?? TREATY_EXTENSIONS
	const coreOptions = toCompilerOptions(options)

	return {
		name: PLUGIN_NAME,
		setup(api: RsbuildPluginAPI): void {
			// Always extend resolve.extensions so bare Treaty imports resolve.
			api.modifyRsbuildConfig((config: RsbuildConfig) => {
				const resolve = config.resolve ?? (config.resolve = {})
				resolve.extensions = mergeExtensions(resolve.extensions, extensions)
			})

			// Strategy 1: the first-class transform hook.
			if (typeof api.transform === 'function') {
				const compiler = createTreatyCompiler(coreOptions)
				api.transform({ test }, ({ code, resourcePath }) => {
					const result = compiler.transform(resourcePath, code)
					return result ? { code: result.code, map: result.map } : { code }
				})
				return
			}

			// Strategy 2: fall back to an Rspack module rule -> our loader.
			api.modifyRsbuildConfig((config: RsbuildConfig) => {
				const tools = config.tools ?? (config.tools = {})
				const rule: RspackModuleRule = {
					test,
					use: [{ loader: loaderPath(), options: coreOptions }],
				}
				const prev = tools.rspack
				const addRule = (rspack: RspackConfig): void => {
					const mod = rspack.module ?? (rspack.module = {})
					const rules = mod.rules ?? (mod.rules = [])
					rules.push(rule)
				}
				if (prev === undefined) {
					tools.rspack = (rspack: RspackConfig) => {
						addRule(rspack)
					}
				} else {
					const list = Array.isArray(prev) ? prev : [prev]
					tools.rspack = [
						...list,
						(rspack: RspackConfig) => {
							addRule(rspack)
						},
					]
				}
			})
		},
	}
}
