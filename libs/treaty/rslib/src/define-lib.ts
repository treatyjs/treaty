/**
 * @module
 *
 * {@link defineTreatyLib}: the ergonomic entry point of `@treaty/rslib`. It
 * produces a complete `RslibConfig` for building a Treaty library — the Treaty
 * transform plugin wired in, plus sensible library defaults (ESM + `.d.ts`,
 * `@angular/*` externalized, file-by-file transpile so tree-shaking metadata
 * survives to the consumer).
 *
 * The result is a plain config object, so callers can spread it into their own
 * `rslib.config.ts` and override any field.
 */

import { treatyRsbuildPlugin } from './plugin.js'
import type {
	DefineTreatyLibOptions,
	TreatyLibFormatEntry,
	TreatyRslibConfig,
} from './types.js'

/**
 * Match every `@angular/*` scoped package. Angular is always a peer dependency
 * of a Treaty library and must never be bundled into it.
 */
export const ANGULAR_EXTERNAL = /^@angular\//

/** Default output formats for a Treaty library. */
const DEFAULT_FORMATS = ['esm'] as const

/**
 * Produce an rslib config for a Treaty library.
 *
 * @param options Library build options; every field has a library-friendly
 *   default, so `defineTreatyLib()` with no arguments is valid.
 * @returns A structural {@link TreatyRslibConfig} assignable to `@rslib/core`'s
 *   `RslibConfig`.
 */
export function defineTreatyLib(
	options: DefineTreatyLibOptions = {}
): TreatyRslibConfig {
	const formats = options.formats ?? DEFAULT_FORMATS
	const dts = options.dts ?? true
	const bundle = options.bundle ?? false
	const target = options.target ?? 'node'

	const lib: TreatyLibFormatEntry[] = formats.map((format) => ({
		format,
		dts,
		bundle,
	}))

	const externals: (string | RegExp)[] = [ANGULAR_EXTERNAL, ...(options.externals ?? [])]

	return {
		lib,
		plugins: [treatyRsbuildPlugin(options.compiler ?? {})],
		output: {
			externals,
			target,
		},
	}
}
