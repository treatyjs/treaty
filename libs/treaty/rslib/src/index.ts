/**
 * @module
 *
 * Public API of `@treaty/rslib`: an Rslib preset for building Treaty libraries.
 *
 * - {@link defineTreatyLib} returns a ready-to-use `RslibConfig` with the Treaty
 *   transform wired in and library defaults applied (ESM + `.d.ts`, `@angular/*`
 *   externalized).
 * - {@link treatyRsbuildPlugin} is the underlying rsbuild/rslib plugin, exposed
 *   for callers who assemble their own config.
 *
 * Treaty is a compiler, not a host: the actual lowering to Ivy JS happens in the
 * Rust authoring compiler reached through `@treaty/compiler`. This package only
 * wires that transform into an rslib library build.
 */

export { defineTreatyLib, ANGULAR_EXTERNAL } from './define-lib.js'
export {
	treatyRsbuildPlugin,
	TREATY_PLUGIN_NAME,
	TREATY_TRANSFORM_TEST,
	emitServerChunks,
} from './plugin.js'

// The Angular partial-declaration linker. Reuses the shared, Rust-backed linker core from
// `@treaty/ts-vite` (one source of truth) so published partial Angular libraries this library
// depends on link to AOT with NO JIT and NO `@angular/compiler`. `treatyRsbuildPlugin` (and so
// `defineTreatyLib`) registers it automatically; these exports let callers reuse the wiring.
export { registerLinkPartialTransform, LINK_PARTIAL_TEST, isPartialModule } from './link-partial.js'

export type { EmittableAsset } from './server-chunks.js'
export {
	ServerChunkCollector,
	serverChunkFileName,
	SERVER_FN_MANIFEST_NAME,
	SERVER_FN_BARREL_NAME,
} from './server-chunks.js'

export type {
	DefineTreatyLibOptions,
	TreatyRslibPluginOptions,
	TreatyRslibConfig,
	TreatyRsbuildConfigSlice,
	TreatyLibFormatEntry,
	TreatyRsbuildPlugin,
	TreatyRsbuildPluginApi,
	TreatyTransformContext,
	TreatyTransformOutput,
	TreatyTransformHandler,
	TreatyTransformDescriptor,
	TreatyProcessAssetsArgs,
	TreatyProcessAssetsDescriptor,
	TreatyRspackCompilation,
	TreatyRspackSource,
	TreatyRspackSourcesNamespace,
} from './types.js'
