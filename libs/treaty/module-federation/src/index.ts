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
 *   - {@link resolveFederation} — the canonical reader of the on/off
 *     {@link FederationConfig} toggle (`federation: false` / `{ enabled: false }`
 *     ⇒ off; default ⇒ on). Every surface that wires federation funnels through
 *     it so the toggle is honored in exactly one place.
 *   - {@link exportMfConfig} / {@link writeMfConfig} — the optional eject path:
 *     serialize the generated config to a human-readable object or file so a dev
 *     can customize it (the file re-imports Treaty). Never required.
 *   - {@link exportFederationConfig} / {@link writeFederationConfig} — eject to a
 *     **standalone** `@module-federation/enhanced`-compatible config the user
 *     OWNS: an import-free options object/file that drops straight into a vanilla
 *     `ModuleFederationPlugin`, so an app can take over MF without Treaty.
 *
 * The bundler peers (`@module-federation/enhanced`, `@module-federation/vite`)
 * are referenced structurally, so this package typechecks and is usable for
 * config generation without either installed.
 */

export {
	generateMfConfig,
	resolveFederation,
	isFederationEnabled,
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
	FederationConfig,
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

export { exportMfConfig, writeMfConfig, renderMfConfigFile } from './export.js'
export type { ExportedMfConfig } from './export.js'

export {
	exportFederationConfig,
	writeFederationConfig,
	renderFederationConfigFile,
} from './eject.js'
export type {
	AppGraph,
	StandaloneFederationConfig,
	StandaloneSharedConfig,
} from './eject.js'
