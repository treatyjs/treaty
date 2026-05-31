/**
 * @module
 *
 * The Treaty rsbuild/rslib plugin. rslib builds on rsbuild, so a plugin that
 * registers an rsbuild `transform` handler works for both an app build and a
 * library build — this is the same transform-wiring approach the other Treaty
 * bundler integrations use, shared through `@treaty/compiler`.
 *
 * The plugin owns one job: route every authoring file Treaty owns (`.treaty`,
 * `.tsx`/`.tjsx`, and `.ts` `@Component`) through the {@link TreatyCompiler}
 * (which calls the Rust authoring compiler) and hand the emitted Ivy JS back to
 * rsbuild. Files Treaty does not own are passed through untouched.
 */

import { readFile } from 'node:fs/promises'
import { createTreatyCompiler, type TransformInput, type TreatyCompiler } from '@treaty/compiler'
import type {
	TreatyProcessAssetsArgs,
	TreatyRsbuildPlugin,
	TreatyRsbuildPluginApi,
	TreatyRslibPluginOptions,
	TreatyTransformContext,
	TreatyTransformOutput,
} from './types.js'
import { ServerChunkCollector } from './server-chunks.js'

/** Stable plugin name reported to rsbuild/rslib. */
export const TREATY_PLUGIN_NAME = 'treaty:transform'

/**
 * The `processAssets` stage to emit the library's server-fn chunks at.
 * `'additional'` runs after the normal module graph is built, so the extra
 * per-fn chunks + barrel + manifest do not race the modules they came from.
 */
const SERVER_CHUNK_STAGE = 'additional'

/**
 * Emit a collector's library server-fn chunks (each fn's body), the re-export
 * barrel, and the manifest into a `processAssets` pass via `compilation.emitAsset`
 * (wrapped in a `RawSource`). Skips any name the build already carries so a
 * re-run is idempotent; a no-op when nothing was collected.
 */
export function emitServerChunks(
	collector: ServerChunkCollector,
	args: TreatyProcessAssetsArgs
): void {
	if (collector.isEmpty) return
	const { compilation, sources } = args
	for (const asset of collector.assets()) {
		if (asset.name in compilation.assets) continue
		compilation.emitAsset(asset.name, new sources.RawSource(asset.source))
	}
}

/**
 * Resource paths the transform should be invoked for. rsbuild matches the
 * registered `test` against the resource before calling the handler; we still
 * re-check inside the handler via the compiler's own classifier so a plain
 * `.ts` without `@Component` is passed through.
 */
export const TREATY_TRANSFORM_TEST = /\.(treaty|tsx|tjsx|ts)$/

/**
 * Batch-prewarm the configured cold-build files through `transformMany` so the
 * per-module transforms that follow are cache hits. Unreadable entries are
 * skipped; the per-module transform surfaces any real error.
 */
async function prewarm(compiler: TreatyCompiler, files: readonly string[]): Promise<void> {
	const inputs: TransformInput[] = []
	for (const file of files) {
		try {
			inputs.push({ id: file, code: await readFile(file, 'utf8') })
		} catch {
			// Missing/unreadable prewarm entry: skip.
		}
	}
	if (inputs.length > 0) compiler.transformMany(inputs)
}

/**
 * Build the Treaty rsbuild plugin. The returned object is structurally an
 * `RsbuildPlugin` (`{ name, setup(api) }`) and is reused by rslib.
 *
 * @param options Forwarded to the underlying {@link TreatyCompiler}, plus an
 *   optional cold-build `prewarm` list wired to `onBeforeBuild` when available.
 */
export function treatyRsbuildPlugin(
	options: TreatyRslibPluginOptions = {}
): TreatyRsbuildPlugin {
	const { prewarm: prewarmFiles = [], ...coreOptions } = options
	const compiler = createTreatyCompiler(coreOptions)

	return {
		name: TREATY_PLUGIN_NAME,
		setup(api: TreatyRsbuildPluginApi): void {
			// Accumulates the per-fn server chunks discovered across the library build
			// so each server fn becomes its own separately-exported chunk entry.
			const serverChunks = new ServerChunkCollector()
			// Cold-build batch prewarm (opt-in), when the host exposes the hook.
			if (prewarmFiles.length > 0 && typeof api.onBeforeBuild === 'function') {
				api.onBeforeBuild(() => prewarm(compiler, prewarmFiles))
			}
			api.transform(
				{ test: TREATY_TRANSFORM_TEST },
				(context: TreatyTransformContext): TreatyTransformOutput | string | null => {
					// Let the compiler decide ownership: it returns null for files it
					// does not own (e.g. a non-`@Component` `.ts`), so we pass those back
					// unchanged for rsbuild's normal pipeline to handle.
					const result = compiler.transform(context.resource, context.code)
					if (result === null) return context.code
					// Collect this module's server fns for per-fn chunk emission; the
					// returned `code` is the CLIENT module (bodies already replaced by
					// the compiler with their client bindings).
					serverChunks.add(result)
					return result.map !== undefined
						? { code: result.code, map: result.map }
						: { code: result.code }
				}
			)
			// Emit each server fn as its own loadable chunk, the re-export barrel, and
			// the manifest, when the host exposes the asset hook. A server fn body
			// therefore never enters the library's client output — only its binding.
			if (typeof api.processAssets === 'function') {
				api.processAssets({ stage: SERVER_CHUNK_STAGE }, (assetArgs) => {
					emitServerChunks(serverChunks, assetArgs)
				})
			}
		},
	}
}
