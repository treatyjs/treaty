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

export type { TreatyPluginOptions } from './options.js'
export { TREATY_EXTENSIONS, DEFAULT_TEST } from './options.js'

export type { EmittableAsset } from './server-chunks.js'
export {
	ServerChunkCollector,
	serverChunkFileName,
	SERVER_FN_MANIFEST_NAME,
} from './server-chunks.js'

export { default as treatyLoader } from './loader.js'
