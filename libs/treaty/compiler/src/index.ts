/**
 * @module
 *
 * Public API of `@treaty/compiler`: the file-by-file Treaty compiler core that
 * every bundler plugin (Vite, Rspack, …) builds on. It routes authoring files
 * to the Rust authoring compiler, caches results incrementally, and exposes
 * dead-code / tree-shaking metadata — all without importing any bundler.
 *
 * Treaty is a compiler, not a host: the actual lowering to Ivy JS lives in the
 * Rust authoring compiler reached through the `@treaty/authoring-node` addon.
 */

export { TreatyCompiler, createTreatyCompiler, TreatyCompileError, classify } from './compiler.js'

export type {
	ImportedSelectorMap,
	ProjectSelectors,
	ServerFnChunk,
	TransformInput,
	TransformResult,
	TreatyCompilerOptions,
	TreatyFileKind,
} from './types.js'

export type {
	ServerBodyMapAudit,
	ServerFnManifest,
	ServerFnManifestEntry,
} from './server-chunks.js'
export {
	assertNoServerBodyInMap,
	buildServerFnManifest,
	isValidSourceMapV3,
	serverFnChunkId,
	splitServerModule,
} from './server-chunks.js'

export type { CacheStats } from './cache.js'
export { contentHash, IncrementalCache } from './cache.js'

export type { SideEffectsDescriptor } from './treeshake.js'
export {
	PURE_ANNOTATION,
	PURE_MODULE,
	annotatePureFactories,
	dropUnusedServerFns,
} from './treeshake.js'

export type {
	CompiledComponent,
	CompiledAuthoring,
	CompiledAuthoringEntry,
	AuthoringFile,
} from './addon.js'
export {
	compileTreaty,
	compileSource,
	compileUnifiedSource,
	compileMany,
	compileTemplate,
	buildSelectorRegistry,
	buildImportedSelectors,
} from './addon.js'
