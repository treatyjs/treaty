/**
 * @module
 *
 * Shared option types for `@treaty/rspack`. The loader and the plugin accept the
 * same Treaty compiler knobs, forwarded verbatim to `@treaty/compiler`. Keeping
 * them here means neither `loader.ts` nor `plugin.ts` has to import the other.
 */

import type { TreatyCompilerOptions } from '@treaty/compiler'

/**
 * Options understood by both the Treaty Rspack loader and plugin. These are the
 * {@link TreatyCompilerOptions} (cache, pure annotation, server-fn dropping)
 * exposed through Rspack's `loader.options` / plugin constructor.
 */
export type TreatyLoaderOptions = TreatyCompilerOptions

/** Options accepted by {@link TreatyRspackPlugin}. */
export interface TreatyPluginOptions extends TreatyCompilerOptions {
	/**
	 * Authoring extensions to add to `resolve.extensions` so bare imports of
	 * Treaty modules resolve. Defaults to `['.treaty', '.tsx', '.tjsx']`.
	 */
	readonly extensions?: readonly string[]
	/**
	 * The `test` regular expression the generated module rule matches against. By
	 * default it matches Treaty's owned extensions; override to narrow or widen it.
	 */
	readonly test?: RegExp
}

/** The authoring extensions Treaty owns and the plugin resolves by default. */
export const DEFAULT_EXTENSIONS: readonly string[] = ['.treaty', '.tsx', '.tjsx']

/** Default `test` matcher for the loader rule: any Treaty authoring extension. */
export const DEFAULT_TEST: RegExp = /\.(treaty|tsx|tjsx)$/
