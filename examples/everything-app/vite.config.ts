/**
 * Vite build for the everything-app, wired to the Treaty compiler.
 *
 * Treaty is a compiler, not a host: this config does not start anything. The
 * developer runs Vite over it (`vite build` / `vite dev`). All Treaty does here
 * is contribute the authoring plugin: `treaty(...)` returns the `@treaty/vite`
 * plugin, which lowers every `.treaty` / `.tsx` / `.tjsx` / `@Component` `.ts`
 * file in the module graph to Ivy JS via `@treaty/compiler` -> the Rust addon.
 *
 * AUTO MODULE FEDERATION. Treaty derives the federation config from the app's
 * route graph with zero hand-written `exposes` (see `mfConfig` below, computed by
 * `generateMfConfig`). The federation runtime plugin
 * (`@module-federation/vite`) is an OPTIONAL peer; this example does not install
 * it, so the buildable default uses the plain authoring plugin and surfaces the
 * derived federation config for inspection/coverage. To turn the runtime wiring
 * on, install `@module-federation/vite` and swap `treaty(...)` for
 * `treatyWithFederation({ ..., moduleFederation: mfOptions })` — every lazy route
 * then becomes a deployable remote with no other change. `mfConfig` is the exact
 * config that path would generate.
 */
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'

import { defineConfig } from 'vite'
import treaty, { generateMfConfig } from '@treaty/vite'
import type { MfOptions } from '@treaty/module-federation'

import { appRoutes } from './federation/routes-bridge'

const here = dirname(fileURLToPath(import.meta.url))
// The Treaty compiler rewrites lifted server-fn call sites to typed client resource bindings that
// `import { edenPromiseResource } from '@treaty/httpclient/resources'` — the real published runtime.
// A consumer app resolves that through the installed `@treaty/httpclient` package's `exports` map; in
// this monorepo example the runtime package is not built, so we resolve the subpath to its TypeScript
// source (the same mapping `tsconfig.base.json` already declares for the typechecker). Vite's default
// esbuild pipeline transforms the plain `.ts` runtime; the Treaty plugin owns only authoring formats.
const edenSrc = join(here, '../../libs/treaty/edenclient/src')
const httpclientAliases = [
	{ find: '@treaty/httpclient/resources', replacement: join(edenSrc, 'resources.ts') },
	{ find: '@treaty/httpclient/client', replacement: join(edenSrc, 'client.ts') },
	{ find: '@treaty/httpclient', replacement: join(edenSrc, 'index.ts') },
]

/**
 * The auto-MF options for this app: a federation HOST that consumes the standalone
 * `profile` remote and auto-exposes each lazy route. `routes` is what drives
 * `deriveExposesFromRoutes`; the developer writes no `exposes` map by hand.
 */
const mfOptions: MfOptions = {
	name: 'everything_app',
	routes: appRoutes,
	// A remote this host consumes at runtime (the lazy `profile` feature, served
	// standalone). The URL is the bootstrap default; the federation-deploy runtime
	// plugin repoints it from the manifest at load (see federation.manifest.ts).
	remotes: {
		profile: 'profile@http://localhost:4301/remoteEntry.js',
	},
}

// The normalized federation config Treaty derives from the route graph. Computing
// it here keeps the host's exposes/remotes coherent with `app.routes.ts` and is
// the precise object `treatyWithFederation({ moduleFederation: mfOptions })` hands
// to `@module-federation/vite`. Eager `''` stays in the host; each lazy route is
// auto-exposed as `./routes/<path>`.
export const mfConfig = generateMfConfig(mfOptions)

export default defineConfig({
	// `<app-root>` lives in index.html; Vite uses it as the build entry and
	// bundles `src/main.ts` (the bootstrap) from there.
	build: {
		target: 'es2022',
		outDir: 'dist',
	},
	resolve: {
		// `@treaty/httpclient` is a workspace TS package with no built dist in this example; resolve the
		// resource-client subpath the compiler imports to its source so the lifted server-fn client
		// bindings (edenPromiseResource) link in the real bundler build (mirrors tsconfig.base.json).
		alias: httpclientAliases,
	},
	plugins: [
		treaty({
			// Treaty compiler knobs are forwarded verbatim to `@treaty/compiler`.
			sourceMap: true,
			// Cold-build prewarm: batch-compile the authoring entry points up front in
			// one parallel round trip through the Rust addon, so the per-module
			// transforms during the build are served from the incremental cache.
			prewarm: [
				'src/app/app-root.component.ts',
				'src/components/log-viewer.component.ts',
				'src/components/counter.tsx',
				'src/components/todo-list.treaty',
				'src/features/dashboard/dashboard.component.ts',
				'src/features/greeter/greeter-page.component.ts',
				'src/features/greeter/greeter.treaty',
				'src/features/greeter/greeting-card.tjsx',
				'src/features/metrics/metrics-panel.component.ts',
				'src/features/metrics/gauge.treaty',
				'src/features/profile/profile.component.ts',
				'src/features/profile/profile-settings.component.ts',
			],
		}),
	],
})
