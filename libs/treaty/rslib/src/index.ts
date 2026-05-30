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
} from './plugin.js'

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
} from './types.js'
