/**
 * Vite build for the everything-app, wired to the Treaty compiler with
 * automatic Module Federation.
 *
 * Treaty is a compiler, not a host: this config does not start anything. The
 * developer runs Vite over it (`vite build` / `vite dev`). All Treaty does here
 * is contribute plugins:
 *
 *   - `treatyWithFederation(...)` returns BOTH the Treaty authoring plugin
 *     (lowers `.treaty` / `.tsx` / `@Component` `.ts` to Ivy JS via
 *     `@treaty/compiler` -> the Rust addon) AND the auto-generated
 *     `@module-federation/vite` `federation()` plugin. The developer writes no
 *     `federation()` call by hand -- passing the app's `routes` is enough for
 *     every lazy boundary to become a deployable remote.
 *
 * The `defineConfig` from `vite` and the structural `UserConfig` shape are the
 * only Vite surface this file touches; the federation plugin is referenced
 * exactly as the `@treaty/vite` plugin produces it (an array that may contain a
 * `Promise<Plugin>`, which Vite resolves).
 */
import { defineConfig } from 'vite'
import { treatyWithFederation } from '@treaty/vite'

import { appRoutes } from './federation/routes-bridge'

// `treatyWithFederation` returns `Array<Plugin | Promise<Plugin>>`. Passing the
// app's `routes` turns on zero-config auto-MF: `deriveExposesFromRoutes` (called
// inside `@treaty/module-federation`) walks the route graph and exposes every
// lazy route as a remote -- no hand-written `exposes` map.
export default defineConfig({
	plugins: [
		treatyWithFederation({
			// Auto-MF: this app is a federation HOST by default and additionally
			// exposes each lazy route. `routes` drives `deriveExposesFromRoutes`.
			moduleFederation: {
				name: 'everything_app',
				routes: appRoutes,
				// A remote this host consumes at runtime (the lazy `profile`
				// feature, served standalone). Demonstrates host<-remote wiring;
				// the URL is resolved by the federation-deploy manifest at load
				// (see federation.manifest.ts).
				remotes: {
					profile: 'profile@http://localhost:4301/remoteEntry.js',
				},
			},
			// Treaty compiler knobs are forwarded verbatim to `@treaty/compiler`.
			sourceMap: true,
		}),
	],
})
