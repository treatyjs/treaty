/**
 * @module
 *
 * Typed options and the minimal structural shapes of the rslib / rsbuild config
 * surface this preset produces. `@rslib/core` is a peer dependency that may not
 * be installed at typecheck time, so rather than importing its types we model
 * just the slice we populate. The objects we emit are structurally compatible
 * with `RslibConfig` from `@rslib/core` and `RsbuildPlugin` from `@rsbuild/core`.
 *
 * Treaty is a compiler, not a host: the actual lowering to Ivy JS is performed
 * by the Rust authoring compiler reached through `@treaty/compiler`. This module
 * only describes how that transform is wired into a library build.
 */

import type { TreatyCompilerOptions } from '@treaty/compiler'

/**
 * The rsbuild transform-handler context. rsbuild calls a registered transform
 * with the module source plus its resource path; we only need those two fields.
 * Declared structurally so we do not depend on `@rsbuild/core` at build time.
 */
export interface TreatyTransformContext {
	/** Source text of the module being transformed. */
	readonly code: string
	/** Absolute path of the resource (used to classify the file kind). */
	readonly resource: string
}

/** A transformed module: emitted code plus an optional serialized source map. */
export interface TreatyTransformOutput {
	readonly code: string
	readonly map?: string
}

/**
 * An rsbuild transform handler. Returns the rewritten module (or `null`/the
 * original to opt out). rsbuild allows sync or async handlers; we are sync.
 */
export type TreatyTransformHandler = (
	context: TreatyTransformContext
) => TreatyTransformOutput | string | null

/**
 * The `transform` descriptor an rsbuild plugin registers via
 * `api.transform(descriptor, handler)`. We narrow it to the `test` field we set.
 */
export interface TreatyTransformDescriptor {
	/** RegExp matching the resource paths this transform should handle. */
	readonly test: RegExp
}

/**
 * The minimal `RsbuildPluginAPI` slice we use: registering a transform. The real
 * type accepts additional hook registrars we do not touch.
 */
export interface TreatyRsbuildPluginApi {
	transform(
		descriptor: TreatyTransformDescriptor,
		handler: TreatyTransformHandler
	): void
}

/**
 * A structural `RsbuildPlugin`. `@rsbuild/core` defines this with the same
 * `name` + `setup(api)` shape; ours assigns cleanly to it.
 */
export interface TreatyRsbuildPlugin {
	readonly name: string
	setup(api: TreatyRsbuildPluginApi): void
}

/**
 * The slice of an rslib library entry (`RslibConfig['lib'][number]`) this preset
 * populates: output format, declaration emission, and the bundling toggle.
 */
export interface TreatyLibFormatEntry {
	/** Output module format. Treaty libraries default to ESM. */
	readonly format: 'esm' | 'cjs' | 'umd' | 'mf'
	/** Whether to emit `.d.ts` declarations for this entry. */
	readonly dts: boolean
	/** Whether rslib should bundle (`true`) or transpile file-by-file (`false`). */
	readonly bundle?: boolean
}

/**
 * The slice of `RsbuildConfig` this preset populates: the Treaty transform
 * plugin and the externals/output tuning for a library build.
 */
export interface TreatyRsbuildConfigSlice {
	readonly plugins: readonly TreatyRsbuildPlugin[]
	readonly output: {
		/** Dependencies left out of the bundle (peer/runtime deps). */
		readonly externals: readonly (string | RegExp)[]
		/** Build target — `node` keeps Angular browser polyfills out of libs. */
		readonly target?: 'web' | 'node'
	}
}

/**
 * The structural `RslibConfig` this preset produces. Assignable to the real
 * `RslibConfig` from `@rslib/core`, which is `RsbuildConfig & { lib: Lib[] }`.
 */
export interface TreatyRslibConfig extends TreatyRsbuildConfigSlice {
	readonly lib: readonly TreatyLibFormatEntry[]
}

/** Options accepted by {@link defineTreatyLib}. */
export interface DefineTreatyLibOptions {
	/**
	 * Output formats to emit. Defaults to `['esm']` — the recommended format for
	 * Angular/Treaty libraries consumed by downstream bundlers.
	 */
	readonly formats?: readonly ('esm' | 'cjs' | 'umd' | 'mf')[]
	/**
	 * Emit `.d.ts` declarations alongside the JS. Defaults to `true`; libraries
	 * almost always want their public types published.
	 */
	readonly dts?: boolean
	/**
	 * Whether rslib should bundle the library (`true`) or transpile module-by-
	 * module (`false`). Defaults to `false` so the Treaty transform runs per file
	 * and tree-shaking metadata is preserved for the downstream consumer.
	 */
	readonly bundle?: boolean
	/**
	 * Extra package names or patterns to externalize on top of the always-on
	 * `@angular/*` externals. Use for additional peer dependencies.
	 */
	readonly externals?: readonly (string | RegExp)[]
	/**
	 * Build target. Defaults to `'node'` for libraries (no browser polyfills);
	 * the consuming app sets its own target.
	 */
	readonly target?: 'web' | 'node'
	/**
	 * Options forwarded to the underlying {@link TreatyCompiler} (cache,
	 * tree-shaking annotations, server-fn dropping).
	 */
	readonly compiler?: TreatyCompilerOptions
}
