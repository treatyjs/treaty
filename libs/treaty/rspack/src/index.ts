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

export { TreatyRspackPlugin, treatyRule } from './plugin.js'
export type { TreatyCompilerHost } from './plugin.js'

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
