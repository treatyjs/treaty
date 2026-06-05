/**
 * Rsbuild build for the everything-app, wired to the Treaty compiler.
 *
 * Treaty is a compiler, not a host: this is a plain Rsbuild configuration. The
 * developer runs Rsbuild over it (`rsbuild build` / `rsbuild dev`). Treaty
 * contributes `pluginTreaty()`, which registers an `api.transform` that lowers
 * the authoring formats (`.treaty` / `.tsx` / `.tjsx`) to Ivy JS via
 * `@treaty/compiler` -> the Rust addon, and extends `resolve.extensions`.
 *
 * `@treaty/rsbuild` deliberately does not bundle Module Federation into its
 * plugin (unlike `@treaty/vite` / `@treaty/rspack`): Rsbuild builds on Rspack +
 * `@module-federation/enhanced`, so MF is wired with the SAME zero-config
 * Treaty source of truth via the `@treaty/module-federation` adapter. We call
 * `generateMfConfig({ routes })` (auto-`exposes` from the lazy routes) and feed
 * it through `toRspackModuleFederation` to get ready-to-use enhanced plugin
 * options -- still no hand-written `exposes` map.
 *
 * `@rsbuild/core` and `@module-federation/enhanced` are peer dependencies the
 * developer's build provides; we reference the config surface structurally
 * (never importing an uninstalled peer's types), exactly as the Treaty plugins
 * do. The Treaty plugin object itself is typed by `@treaty/rsbuild`.
 */
import { pluginTreaty } from '@treaty/rsbuild'
import {
	generateMfConfig,
	toRspackModuleFederation,
	type RspackModuleFederationOptions,
} from '@treaty/module-federation'

import { appRoutes } from './federation/routes-bridge'

/**
 * The slice of `RsbuildConfig` this file populates. The real `RsbuildConfig`
 * from `@rsbuild/core` is a structural superset; declared locally so the config
 * typechecks without the peer installed.
 */
interface RsbuildConfigLike {
	readonly source?: {
		readonly entry?: Readonly<Record<string, string>>
		readonly [key: string]: unknown
	}
	readonly plugins?: readonly unknown[]
	readonly tools?: {
		/** The `@module-federation/enhanced` plugin options live here in Rsbuild. */
		readonly rspack?: unknown
		readonly [key: string]: unknown
	}
	readonly [key: string]: unknown
}

// Auto-MF: one normalized config derived from the route graph. `generateMfConfig`
// runs `deriveExposesFromRoutes` for us, so every lazy route is exposed as a
// remote; `toRspackModuleFederation` shapes it for `@module-federation/enhanced`.
const mfOptions: RspackModuleFederationOptions = toRspackModuleFederation(
	generateMfConfig({
		name: 'everything_app',
		routes: appRoutes,
		remotes: {
			profile: 'profile@http://localhost:4301/remoteEntry.js',
		},
	})
)

const config: RsbuildConfigLike = {
	source: { entry: { index: './src/main.ts' } },
	plugins: [
		// The Treaty authoring transform. Lowers .treaty/.tsx/.tjsx to Ivy JS.
		pluginTreaty(),
	],
	tools: {
		// The developer adds the enhanced MF plugin with Treaty's generated
		// options. In a real config this is:
		//   import { ModuleFederationPlugin } from '@module-federation/enhanced/rspack'
		//   rspack: (cfg, { appendPlugins }) =>
		//     appendPlugins([new ModuleFederationPlugin(mfOptions)])
		// We expose the ready-to-use options object so the wiring is one line.
		rspack: { moduleFederation: mfOptions },
	},
}

export default config
