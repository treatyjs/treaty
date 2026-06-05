/**
 * @module
 *
 * Shared, framework-agnostic types for `@treaty/compiler`. Nothing here imports
 * a bundler (Vite/Rspack/etc.); bundler plugins build on top of these.
 */

/** The authoring file kinds Treaty owns and lowers to Ivy JS. */
export type TreatyFileKind = 'treaty' | 'jsx' | 'component'

/**
 * The project-wide `className -> selector` map: every first-party
 * `@Component`/`@Directive` class's real `selector`, keyed by class name. Built
 * ONCE per build by the Rust selector scanner and reused to derive each file's
 * {@link ImportedSelectorMap}.
 */
export type ProjectSelectors = Record<string, string>

/**
 * A per-file `{ localImportName -> selector }` map: for one file, each imported
 * `@Component`/`@Directive`'s LOCAL binding name resolved to its real selector. The
 * compiler consumes this (instead of its class-name↔tag fold) so an IMPORTED child
 * used by its conventional selector resolves as a real dependency.
 */
export type ImportedSelectorMap = Record<string, string>

/**
 * One extracted server function, exposed as its OWN chunk unit so a bundler can
 * code-split it into a separately-loadable chunk. The function BODY lives only
 * in {@link ServerFnChunk.code} (the server-side module) and never in the client
 * bundle; the client keeps only the {@link ServerFnChunk.clientBinding} shim.
 *
 * This is the per-function decomposition of {@link TransformResult.serverModule}:
 * concatenating every chunk's `code` reconstructs (modulo a shared preamble) the
 * back-compat `serverModule` blob.
 */
export interface ServerFnChunk {
	/**
	 * Stable, content-independent identity for this server fn across builds, of
	 * the form `<file>#<fnName>` hashed to a short hex token. Used as the manifest
	 * key and as the chunk reference a bundler emits the fn under, so the same fn
	 * in the same file always lands in the same chunk.
	 */
	readonly id: string
	/** The author's exported name for this server fn (e.g. `save`, `loadUser`). */
	readonly exportName: string
	/**
	 * The server-side module source for THIS fn alone: its declaration plus the
	 * route/handler registration that targets it. Emitted as its own chunk; never
	 * shipped to the client.
	 */
	readonly code: string
	/**
	 * The client-side shim that replaces the fn body in the component: an
	 * import-and-call binding pointing at this fn's chunk. This is all the client
	 * bundle ever sees of the server fn.
	 */
	readonly clientBinding: string
}

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
	 * Extracted server-side module source, when the authoring file declared
	 * server functions. Produced by the unified front-end; absent for files with
	 * no server fns. Bundler plugins may emit this as a sibling module.
	 *
	 * Retained for back-compat: it is the concatenation of every entry in
	 * {@link TransformResult.serverChunks}. New code should prefer `serverChunks`
	 * so each fn can be code-split into its own loadable chunk.
	 */
	readonly serverModule?: string
	/**
	 * The extracted server functions exposed as INDIVIDUAL chunk units — one per
	 * exported server fn — so a bundler can code-split each into a separately
	 * loadable chunk and ship only the per-fn client binding to the browser.
	 * Absent (and `serverModule` likewise absent) for files with no server fns.
	 *
	 * `serverModule` remains the back-compat single-blob concatenation of these.
	 */
	readonly serverChunks?: readonly ServerFnChunk[]
	/**
	 * Tree-shaking hint for bundlers: pure component modules have no
	 * import-time side effects, so unused exports may be dropped.
	 */
	readonly sideEffects: boolean
}

/**
 * One file in a batch transform: its bundler id (path/url) and source text.
 * Mirrors the two arguments of the per-file `transform(id, code)`.
 */
export interface TransformInput {
	/** Module id (path or url), used for routing, diagnostics, and caching. */
	readonly id: string
	/** Full source text of the module. */
	readonly code: string
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
