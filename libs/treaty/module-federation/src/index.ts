/**
 * @module
 *
 * `@treaty/module-federation` — automatic, zero-config Module Federation for
 * Treaty apps. The developer never writes a `ModuleFederationPlugin`: they
 * describe their app declaratively (or pass nothing at all) and Treaty's
 * bundler plugins call into this package to wire federation for them.
 *
 * Three layers, smallest API surface first:
 *   - {@link generateMfConfig} — the framework-agnostic core. Turns simple
 *     {@link MfOptions} into a normalized config with the Angular runtime
 *     shared as eager singletons by default. When given the app's `routes`
 *     and/or `libs`, it auto-derives the `exposes` map (via
 *     {@link deriveExposesFromRoutes} / {@link deriveExposesFromLibs}) so every
 *     lazy feature route and library becomes an independently deployable remote
 *     with no hand-written exposes.
 *   - {@link toRspackModuleFederation} — adapter to `@module-federation/enhanced`
 *     `ModuleFederationPlugin` options (Rspack/webpack).
 *   - {@link toViteFederation} — adapter to `@module-federation/vite`
 *     `federation()` options (Vite).
 *
 * The bundler peers (`@module-federation/enhanced`, `@module-federation/vite`)
 * are referenced structurally, so this package typechecks and is usable for
 * config generation without either installed.
 */

export {
	generateMfConfig,
	DEFAULT_HOST_NAME,
	DEFAULT_FILENAME,
	DEFAULT_ANGULAR_VERSION,
	DEFAULT_ANGULAR_PACKAGES,
	DEFAULT_SINGLETON_PACKAGES,
} from './config.js'
export type {
	MfOptions,
	NormalizedMfConfig,
	SharedConfig,
	RemoteEntry,
} from './config.js'

export {
	deriveExposesFromRoutes,
	deriveExposesFromLibs,
	DEFAULT_ROUTE_KEY_PREFIX,
	DEFAULT_ROUTE_PATH_BASE,
	DEFAULT_LIB_KEY_PREFIX,
} from './routes.js'
export type {
	RouteLike,
	LibEntry,
	DeriveRoutesOptions,
	DeriveLibsOptions,
} from './routes.js'

export { toRspackModuleFederation } from './rspack.js'
export type {
	RspackModuleFederationOptions,
	RspackSharedConfig,
} from './rspack.js'

export { toViteFederation } from './vite.js'
export type { ViteFederationOptions, ViteSharedConfig } from './vite.js'
