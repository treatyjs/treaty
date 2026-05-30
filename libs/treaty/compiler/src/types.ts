/**
 * @module
 *
 * Shared, framework-agnostic types for `@treaty/compiler`. Nothing here imports
 * a bundler (Vite/Rspack/etc.); bundler plugins build on top of these.
 */

/** The authoring file kinds Treaty owns and lowers to Ivy JS. */
export type TreatyFileKind = 'treaty' | 'jsx' | 'component'

/**
 * Result of a successful transform. Mirrors the common bundler `transform`
 * contract: emitted `code` plus an optional source map (serialized JSON).
 */
export interface TransformResult {
	/** Emitted Ivy JavaScript. */
	readonly code: string
	/** Serialized source map (JSON string), when one is available. */
	readonly map?: string
	/**
	 * Tree-shaking hint for bundlers: pure component modules have no
	 * import-time side effects, so unused exports may be dropped.
	 */
	readonly sideEffects: boolean
}

/** Options accepted by the {@link TreatyCompiler} factory. */
export interface TreatyCompilerOptions {
	/**
	 * Enable the incremental content-hash cache. Defaults to `true`. Disable for
	 * one-shot builds where caching only adds overhead.
	 */
	readonly cache?: boolean
	/**
	 * When `true`, factory calls in emitted module exports are prefixed with the
	 * `/*#__PURE__*\/` annotation so bundlers can drop them when unused. Defaults
	 * to `true`.
	 */
	readonly annotatePure?: boolean
	/**
	 * When `true`, unused server-fn client bindings (`createServerFn(...)` whose
	 * result is never referenced) are dropped from the emitted module so they do
	 * not ship to the browser. Defaults to `true`.
	 */
	readonly dropUnusedServerFns?: boolean
}
