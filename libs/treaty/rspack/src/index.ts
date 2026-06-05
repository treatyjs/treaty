/**
 * @module
 *
 * Public API of `@treaty/rspack`: the Rspack/webpack integration for Treaty.
 *
 * Two entry points, both building on `@treaty/compiler` (which routes to the
 * Rust authoring compiler — Treaty is a compiler, not a host):
 *   - {@link treatyLoader}: a webpack/rspack loader that lowers Treaty's owned
 *     extensions (`.treaty`, `.tsx`, `.tjsx`, and `@Component` `.ts`) to Ivy JS.
 *   - {@link TreatyRspackPlugin}: a thin plugin that registers the loader rule
 *     and adds the authoring extensions to `resolve.extensions`.
 *
 * `@rspack/core` is a peer dependency; the loader-context and compiler shapes
 * are declared structurally so this package typechecks without it installed.
 */

export { treatyLoader, loaderPath } from './loader.js'
export { default } from './loader.js'
export type { TreatyLoader, TreatyLoaderContext } from './loader.js'

export { TreatyRspackPlugin, treatyRule, linkPartialRule, routesRule } from './plugin.js'
export type { TreatyCompilerHost, TreatyCompilation } from './plugin.js'

// File routing as a virtual module, generated during the build (no prebuilt routes.ts). Reuses the
// shared, Rust-backed generator from `@treaty/ts-vite` (one source of truth). The plugin wires the
// alias + `routesRule()` automatically when `fileRoutes` is set; these exports let callers assemble
// their own config or run the loader standalone.
export {
	routesLoader,
	routesLoaderPath,
	routesSentinelPath,
	ROUTES_SENTINEL_TEST,
	TREATY_ROUTES_ID,
} from './routes-virtual.js'
export type { RoutesLoaderOptions, RoutesLoaderContext } from './routes-virtual.js'

// The Angular partial-declaration linker loader. Reuses the shared, Rust-backed linker core from
// `@treaty/ts-vite` (one source of truth) so published partial Angular libraries link to AOT with
// NO JIT and NO `@angular/compiler`. The plugin registers `linkPartialRule()` automatically; this
// export lets callers wire the loader into their own config.
export {
	linkPartialLoader,
	linkPartialLoaderPath,
	LINK_PARTIAL_TEST,
	isPartialModule,
} from './link-partial-loader.js'
export type { LinkPartialLoader, LinkPartialLoaderContext } from './link-partial-loader.js'

export {
	emitServerFnChunks,
	registryFor,
	serverFnAssetName,
	serverFnManifestAsset,
	TreatyServerFnRegistry,
	SERVER_FN_MANIFEST_ASSET,
} from './server-chunks.js'
export type {
	RspackServerFnManifest,
	ServerChunkEmitter,
	ServerFnManifestRecord,
} from './server-chunks.js'

export {
	DEFAULT_EXTENSIONS,
	DEFAULT_TEST,
} from './options.js'
export type { TreatyLoaderOptions, TreatyPluginOptions } from './options.js'

// Re-export the Module Federation surface so callers can generate or inspect the
// federation config the plugin wires automatically (Treaty is a compiler: this
// is generated, never hand-written).
export {
	toRspackModuleFederation,
	generateMfConfig,
} from '@treaty/module-federation'
export type {
	MfOptions,
	NormalizedMfConfig,
	RspackModuleFederationOptions,
} from '@treaty/module-federation'
