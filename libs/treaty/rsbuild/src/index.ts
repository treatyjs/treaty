/**
 * @module
 *
 * Public API of `@treaty/rsbuild`: an Rsbuild plugin that lowers Treaty
 * authoring formats (`.treaty`, `.tsx`, `.tjsx`) to Ivy JS by routing them
 * through the shared `@treaty/compiler` core. `@rsbuild/core` is a peer
 * dependency — the host application provides the bundler.
 *
 * Treaty is a compiler, not a host: this package never reimplements
 * compilation. It only wires the core into an Rsbuild build.
 */

export { pluginTreaty, PLUGIN_NAME, emitServerChunks } from './plugin.js'

// The Angular partial-declaration linker. Reuses the shared, Rust-backed linker core from
// `@treaty/ts-vite` (one source of truth) so published partial Angular libraries link to AOT with
// NO JIT and NO `@angular/compiler`. `pluginTreaty` registers it automatically; the standalone
// plugin + transform registrar are exported for callers assembling their own Rsbuild config.
export {
	pluginTreatyLinkPartial,
	registerLinkPartialTransform,
	LINK_PARTIAL_PLUGIN_NAME,
	LINK_PARTIAL_TEST,
	isPartialModule,
} from './link-partial.js'

export type { TreatyPluginOptions } from './options.js'
export { TREATY_EXTENSIONS, DEFAULT_TEST } from './options.js'

export type { EmittableAsset } from './server-chunks.js'
export {
	ServerChunkCollector,
	serverChunkFileName,
	SERVER_FN_MANIFEST_NAME,
} from './server-chunks.js'

export { default as treatyLoader } from './loader.js'
