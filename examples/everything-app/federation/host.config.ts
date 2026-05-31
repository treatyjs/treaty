/**
 * Module Federation HOST config -- consumes the `profile` remote.
 *
 * OPTIONAL EJECT EXAMPLE. Treaty federation is zero-config: you normally write
 * none of this. These `federation/*.config.ts` files exist only to demonstrate
 * the eject path — what a hand-customized federation config looks like after a
 * developer runs `writeMfConfig` (or hand-authors `generateMfConfig`) to take
 * control. Deleting this directory leaves the app fully auto-federated.
 *
 * Every Treaty app is a federation host automatically; this file shows the host
 * side of a host<-remote pair. The host declares the remotes it consumes (by
 * the local alias used in `import('profile/...')`) and shares the Angular
 * runtime as eager singletons with the remote so there is exactly one framework
 * copy at runtime. It still exposes its OWN lazy routes too (auto-MF), so an app
 * can be a host and a remote at once.
 *
 * Treaty is a compiler, not a host: this config only describes the federation
 * surface. The developer runs Rspack/Vite over it. The concrete remote URL here
 * is the bootstrap default; at load time the federation-deploy runtime plugin
 * (see ../federation.manifest.ts) repoints each remote to the version+url the
 * manifest currently points at, so deploy/rollback needs no rebuild.
 */
import { generateMfConfig } from '@treaty/module-federation'

import { appRoutes } from './routes-bridge'

export const hostConfig = generateMfConfig({
	name: 'everything_app',
	// Remotes this host consumes, keyed by the local import alias. The value is
	// `name@entryUrl`; the runtime plugin overrides the url from the manifest.
	remotes: {
		profile: 'profile@http://localhost:4301/remoteEntry.js',
	},
	// The host also auto-exposes its own lazy routes (host + remote at once).
	routes: appRoutes,
})

export default hostConfig
