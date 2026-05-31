/**
 * Module Federation REMOTE config -- the lazy feature routes served standalone.
 *
 * This is the demonstration of zero-config auto-MF: the remote writes NO
 * `exposes` map by hand. It passes the app's route graph to
 * `deriveExposesFromRoutes` and every LAZY boundary
 * (`loadComponent` / `loadChildren`) becomes an exposed, independently
 * deployable remote module. In `app.routes.ts` that is `dashboard` (a lazy
 * `loadComponent`) and `profile` (a lazy `loadChildren`); the eager index route
 * stays in the host and is correctly NOT exposed.
 *
 * Treaty is a compiler, not a host: this config only describes the federation
 * surface. The developer runs Rspack/Vite over it to serve `remoteEntry.js`.
 * `@treaty/rspack`'s `TreatyRspackPlugin` derives this for you automatically;
 * we spell out `deriveExposesFromRoutes` explicitly to show what auto-MF does.
 */
import { deriveExposesFromRoutes, generateMfConfig } from '@treaty/module-federation'

import { appRoutes } from './routes-bridge'

// Auto-derived exposes from the lazy routes -- NO hand-written exposes map.
// Each lazy route becomes `./routes/<path>` pointing at a stable, deployable
// module handle the serving platform versions. Eager routes are skipped.
//   => { './routes/dashboard': './src/app/dashboard',
//        './routes/profile':   './src/app/profile' }
export const exposes: Record<string, string> = deriveExposesFromRoutes(appRoutes)

// The full normalized remote config. `generateMfConfig` runs the same
// `deriveExposesFromRoutes` internally when handed `routes`, and shares the
// Angular runtime as eager singletons so the host and this remote share one
// framework copy at runtime.
export const remoteConfig = generateMfConfig({
	name: 'profile',
	// This remote's own entry filename so the host can consume it.
	filename: 'remoteEntry.js',
	routes: appRoutes,
})

export default remoteConfig
