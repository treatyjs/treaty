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
import { fileURLToPath } from 'node:url'
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
import { registerLinkPartialTransform } from './link-partial.js'
import {
	ROUTES_SENTINEL_TEST,
	routesLoaderPath,
	routesSentinelPath,
	TREATY_ROUTES_ID,
} from './routes-virtual.js'
import { devBackendConfigModifier, type DevServerFn } from './dev-backend.js'

/**
 * The `processAssets` pipeline stage to emit server-fn chunks at. `'additional'`
 * runs after the normal asset graph is built, so adding the per-fn chunk files
 * and the manifest does not race the modules they were extracted from.
 */
const SERVER_CHUNK_STAGE = 'additional'

/** Stable plugin name, also asserted by the smoke test. */
export const PLUGIN_NAME = 'treaty:rsbuild'

/**
 * Resolve the loader module path used by the `tools.rspack` fallback. Uses
 * `fileURLToPath` (not `.pathname`) so the path is a real OS path on Windows too —
 * `.pathname` yields an unresolvable leading-slash `/C:/...`.
 */
function loaderPath(): string {
	return fileURLToPath(new URL('./loader.js', import.meta.url))
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

				// File routing as a virtual module, generated during the build (no
				// prebuilt routes.ts). Alias the `virtual:treaty-routes` import to the
				// in-package sentinel so Rspack can resolve it; the routes loader (wired
				// via tools.rspack below) replaces the sentinel source with the
				// Rust-generated route graph. Only wired when the app opted in.
				if (options.fileRoutes !== undefined) {
					const alias = (resolve['alias'] ?? (resolve['alias'] = {})) as Record<
						string,
						unknown
					>
					alias[TREATY_ROUTES_ID] = routesSentinelPath
				}
			})

			// Add the routes-sentinel loader rule through tools.rspack when file
			// routing is enabled. Rsbuild has no synthetic-module load hook, so the
			// route graph is produced by a loader matched to the aliased sentinel,
			// mirroring @treaty/rspack.
			if (options.fileRoutes !== undefined) {
				const fileRoutes = options.fileRoutes
				api.modifyRsbuildConfig((config: RsbuildConfig) => {
					const tools = config.tools ?? (config.tools = {})
					const rule: RspackModuleRule = {
						test: ROUTES_SENTINEL_TEST,
						use: [{ loader: routesLoaderPath, options: fileRoutes }],
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
			}

			// Link published partial-compiled Angular libraries (node_modules `ɵɵngDeclare*`) to AOT
			// via the SHARED Rust linker, so the build needs NO JIT and NO `@angular/compiler`. This
			// is independent of how first-party authoring files are routed below; it only touches
			// partial `node_modules` modules and runs whenever the host exposes `api.transform`.
			if (typeof api.transform === 'function') {
				registerLinkPartialTransform(api)
			}

			// Strategy 1: the first-class transform hook.
			if (typeof api.transform === 'function') {
				const compiler = createTreatyCompiler(coreOptions)
				// Accumulates the per-fn server chunks discovered across the build so
				// they can be code-split into their own files at asset-emit time.
				const serverChunks = new ServerChunkCollector()
				// DEV BACKEND registry: export name -> the ORIGINAL module whose SSR-loaded
				// export is the real server-fn body. Populated lazily below as authoring
				// files declaring server fns are transformed, and read at request time by
				// the `/__server/<name>` dev middleware (mirroring @treaty/vite). Registered
				// on Rsbuild's dev middleware chain so the client RPC stub's
				// `fetch('/__server/<name>')` gets a genuine response in dev instead of a 404.
				const devServerFns = new Map<string, DevServerFn>()
				api.modifyRsbuildConfig(devBackendConfigModifier(devServerFns))
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
					// Register each extracted server fn for the dev backend, routing
					// `/__server/<exportName>` to the ORIGINAL module so its export runs
					// the real body server-side (the client only ever sees the RPC stub).
					if (result.serverChunks) {
						for (const chunk of result.serverChunks) {
							devServerFns.set(chunk.exportName, {
								exportName: chunk.exportName,
								moduleId: resourcePath,
							})
						}
					}
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
