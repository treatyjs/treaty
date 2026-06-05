/**
 * Rspack build for the everything-app, wired to the Treaty compiler with
 * automatic Module Federation turned ON.
 *
 * Treaty is a compiler, not a host: this is a plain Rspack configuration object.
 * The developer runs Rspack over it (`rspack build` / `rspack serve`). The one
 * Treaty entry is `new TreatyRspackPlugin({...})`, which on `apply(compiler)`:
 *
 *   1. registers the Treaty loader rule (lowers `.treaty` / `.tsx` / `.tjsx` /
 *      `@Component` `.ts` to Ivy JS via `@treaty/compiler` -> the Rust addon),
 *   2. adds the authoring extensions to `resolve.extensions`, and
 *   3. -- because `moduleFederation` is not `false` -- adds the auto-generated
 *      `@module-federation/enhanced` `ModuleFederationPlugin`. Passing the app's
 *      `routes` makes `deriveExposesFromRoutes` expose every lazy route as a
 *      remote with zero hand-written `exposes`.
 *
 * `@rspack/core` is a peer dependency provided by the developer's build. We do
 * not import its `Configuration` type (it is not installed in this repo); we
 * reference the config surface structurally, exactly as `@treaty/rspack`
 * references the `Compiler` it mutates.
 */
import { TreatyRspackPlugin } from '@treaty/rspack'

import { appRoutes } from './federation/routes-bridge'

/**
 * The slice of an Rspack/webpack `Configuration` this file populates. The real
 * `Configuration` from `@rspack/core` is a structural superset and remains
 * assignable to this. Declared locally so the config typechecks without the
 * peer installed -- the same structural-peer discipline the Treaty plugins use.
 */
interface RspackConfigLike {
	readonly mode?: 'development' | 'production' | 'none'
	readonly entry?: Readonly<Record<string, string>> | string
	readonly output?: {
		readonly publicPath?: string
		readonly uniqueName?: string
		readonly [key: string]: unknown
	}
	readonly resolve?: {
		readonly extensions?: readonly string[]
		readonly [key: string]: unknown
	}
	readonly module?: {
		readonly rules?: readonly unknown[]
		readonly [key: string]: unknown
	}
	readonly plugins?: readonly unknown[]
	readonly [key: string]: unknown
}

const config: RspackConfigLike = {
	mode: 'production',
	entry: { main: './src/main.ts' },
	output: {
		// `uniqueName` keeps this host's federation runtime from clashing with a
		// consumed remote's runtime in the same page.
		uniqueName: 'everything_app',
		publicPath: 'auto',
	},
	plugins: [
		new TreatyRspackPlugin({
			// Auto-MF on (the default). `routes` drives `deriveExposesFromRoutes`
			// inside `@treaty/module-federation`, so every lazy route is exposed
			// as an independently deployable remote -- no `exposes` map by hand.
			moduleFederation: {
				name: 'everything_app',
				routes: appRoutes,
				// Host consumes the standalone `profile` remote. The concrete URL
				// is repointed at load by the federation-deploy runtime plugin
				// (see federation.manifest.ts) so deploy/rollback needs no rebuild.
				remotes: {
					profile: 'profile@http://localhost:4301/remoteEntry.js',
				},
			},
		}),
	],
}

export default config
