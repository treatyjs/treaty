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

import { createTreatyCompiler, type TreatyCompilerOptions } from '@treaty/compiler'
import type {
	TreatyRsbuildPlugin,
	TreatyRsbuildPluginApi,
	TreatyTransformContext,
	TreatyTransformOutput,
} from './types.js'

/** Stable plugin name reported to rsbuild/rslib. */
export const TREATY_PLUGIN_NAME = 'treaty:transform'

/**
 * Resource paths the transform should be invoked for. rsbuild matches the
 * registered `test` against the resource before calling the handler; we still
 * re-check inside the handler via the compiler's own classifier so a plain
 * `.ts` without `@Component` is passed through.
 */
export const TREATY_TRANSFORM_TEST = /\.(treaty|tsx|tjsx|ts)$/

/**
 * Build the Treaty rsbuild plugin. The returned object is structurally an
 * `RsbuildPlugin` (`{ name, setup(api) }`) and is reused by rslib.
 *
 * @param options Forwarded to the underlying {@link TreatyCompiler}.
 */
export function treatyRsbuildPlugin(
	options: TreatyCompilerOptions = {}
): TreatyRsbuildPlugin {
	const compiler = createTreatyCompiler(options)

	return {
		name: TREATY_PLUGIN_NAME,
		setup(api: TreatyRsbuildPluginApi): void {
			api.transform(
				{ test: TREATY_TRANSFORM_TEST },
				(context: TreatyTransformContext): TreatyTransformOutput | string | null => {
					// Let the compiler decide ownership: it returns null for files it
					// does not own (e.g. a non-`@Component` `.ts`), so we pass those back
					// unchanged for rsbuild's normal pipeline to handle.
					const result = compiler.transform(context.resource, context.code)
					if (result === null) return context.code
					return result.map !== undefined
						? { code: result.code, map: result.map }
						: { code: result.code }
				}
			)
		},
	}
}
