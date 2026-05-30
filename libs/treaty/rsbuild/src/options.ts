/**
 * @module
 *
 * Typed options for {@link pluginTreaty}. These are a thin, bundler-facing
 * layer over {@link TreatyCompilerOptions} from `@treaty/compiler`, plus the
 * extension/matching knobs that are specific to wiring the compiler into an
 * Rsbuild build.
 */

import type { TreatyCompilerOptions } from '@treaty/compiler'

/** The authoring file extensions Treaty owns and lowers to Ivy JS. */
export const TREATY_EXTENSIONS = ['.treaty', '.tsx', '.tjsx'] as const

/**
 * Default `test` regex matching every Treaty authoring extension. Plain `.ts`
 * `@Component` sources are intentionally *not* matched here: the core compiler
 * pre-screens `.ts` for an `@Component` decorator, but matching every `.ts`
 * file in an Rsbuild build would intercept the host's ordinary TypeScript
 * pipeline. Callers who want `.ts` `@Component` lowering can widen
 * {@link TreatyPluginOptions.include} explicitly.
 */
export const DEFAULT_TEST = /\.(treaty|tsx|tjsx)$/

/** Options accepted by {@link pluginTreaty}. */
export interface TreatyPluginOptions extends TreatyCompilerOptions {
	/**
	 * Override the module-id matcher. Defaults to {@link DEFAULT_TEST} (every
	 * Treaty authoring extension). Provide a custom {@link RegExp} to broaden or
	 * narrow which modules are routed through the Treaty compiler.
	 */
	readonly include?: RegExp
	/**
	 * Extensions to append to the bundler's `resolve.extensions` so bare imports
	 * of Treaty modules (`import X from './x'`) resolve. Defaults to
	 * {@link TREATY_EXTENSIONS}.
	 */
	readonly extensions?: readonly string[]
}

/** Split plugin options into the compiler core options it forwards. */
export function toCompilerOptions(options: TreatyPluginOptions): TreatyCompilerOptions {
	const { cache, annotatePure, dropUnusedServerFns } = options
	return { cache, annotatePure, dropUnusedServerFns }
}
