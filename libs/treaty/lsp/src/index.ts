/**
 * @module
 *
 * Public API of `@treaty/lsp`: the plugin-extensible volarjs language service
 * for Treaty authoring formats. Consumers can register additional authoring
 * formats and server languages, build the volarjs `LanguagePlugin`, and reach
 * the typed Rust compiler bridge.
 *
 * The runnable server lives in `./server.ts` (binary `treaty-lsp`); its
 * {@link createServer} factory is re-exported here for embedding and tests.
 */

export type {
	AuthoringLanguagePlugin,
	ServerLanguagePlugin,
	ServerLanguageId,
} from './plugins.js'
export {
	registerAuthoringLanguage,
	registerServerLanguage,
	resolveByExtension,
	resolveByLang,
	listAuthoringLanguages,
	listServerLanguages,
} from './plugins.js'

export {
	createTreatyLanguagePlugin,
	createTreatyVirtualCode,
	createJsxVirtualCode,
	EMBEDDED_TS_ID,
} from './language.js'

export type { TreatyRegion, TreatyRegionKind } from './regions.js'
export { scanTreatyRegions, typeScriptRegions } from './regions.js'

export type { DiagnosticDocument } from './diagnostics.js'
export { provideDiagnostics, DIAGNOSTIC_SOURCE } from './diagnostics.js'

export type { CompiledComponent } from './compiler.js'
export { compileTreaty, compileSource, compileTemplate } from './compiler.js'

export type { TreatyLanguageServer } from './server.js'
export { createServer, start } from './server.js'

export type { TreatyJsxProjectHost } from './jsx-types.js'
export {
	resolveTreatyJsxTypesEntry,
	treatyJsxCompilerOptions,
	applyTreatyJsxAutoTypes,
	TREATY_JSX_IMPORT_SOURCE,
} from './jsx-types.js'
