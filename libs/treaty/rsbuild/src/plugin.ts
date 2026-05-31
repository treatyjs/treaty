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

import { readFile } from 'node:fs/promises'
import { createTreatyCompiler, type TransformInput, type TreatyCompiler } from '@treaty/compiler'
import type {
	ProcessAssetsArgs,
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
import { ServerChunkCollector } from './server-chunks.js'

/**
 * The `processAssets` pipeline stage to emit server-fn chunks at. `'additional'`
 * runs after the normal asset graph is built, so adding the per-fn chunk files
 * and the manifest does not race the modules they were extracted from.
 */
const SERVER_CHUNK_STAGE = 'additional'

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
 * Batch-prewarm the configured cold-build files through `transformMany` so the
 * per-module transform/loader passes that follow are cache hits. Unreadable
 * entries are skipped — the per-module path surfaces any real error.
 */
async function prewarm(compiler: TreatyCompiler, files: readonly string[]): Promise<void> {
	const inputs: TransformInput[] = []
	for (const file of files) {
		try {
			inputs.push({ id: file, code: await readFile(file, 'utf8') })
		} catch {
			// Missing/unreadable prewarm entry: skip; per-module transform will error.
		}
	}
	if (inputs.length > 0) compiler.transformMany(inputs)
}

/**
 * Emit a collector's server-fn chunks + manifest into a `processAssets` pass.
 * Each `<chunkId>.server.js` and the `treaty-server-fns.json` manifest is added
 * via `compilation.emitAsset` (wrapped in a `RawSource`), skipping any name the
 * build already carries so a re-run is idempotent. A no-op when nothing was
 * collected, so a pure client build emits no extra files.
 */
export function emitServerChunks(collector: ServerChunkCollector, args: ProcessAssetsArgs): void {
	if (collector.isEmpty) return
	const { compilation, sources } = args
	for (const asset of collector.assets()) {
		if (asset.name in compilation.assets) continue
		compilation.emitAsset(asset.name, new sources.RawSource(asset.source))
	}
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
	const prewarmFiles = options.prewarm ?? []

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
				// Accumulates the per-fn server chunks discovered across the build so
				// they can be code-split into their own files at asset-emit time.
				const serverChunks = new ServerChunkCollector()
				// Cold-build batch prewarm (opt-in), when the host exposes the hook.
				if (prewarmFiles.length > 0 && typeof api.onBeforeBuild === 'function') {
					api.onBeforeBuild(() => prewarm(compiler, prewarmFiles))
				}
				api.transform({ test }, ({ code, resourcePath }) => {
					const result = compiler.transform(resourcePath, code)
					if (result === null) return { code }
					// Collect this file's server fns for later per-fn chunk emission;
					// the returned `code` is the CLIENT module (fn bodies already
					// replaced by the compiler with their client bindings).
					serverChunks.add(result)
					return { code: result.code, map: result.map }
				})
				// Emit each server fn as its own `<id>.server.js` chunk plus the
				// manifest, when the host exposes the asset hook. The fn body therefore
				// never enters the client/Ivy bundle — only the client binding does.
				if (typeof api.processAssets === 'function') {
					api.processAssets({ stage: SERVER_CHUNK_STAGE }, (assetArgs) => {
						emitServerChunks(serverChunks, assetArgs)
					})
				}
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
