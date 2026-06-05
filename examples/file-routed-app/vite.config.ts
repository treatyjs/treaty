/**
 * Vite build for the file-routed-app, wired to the Treaty compiler and the
 * file-system route generator — produced DURING the build as a VIRTUAL MODULE.
 *
 * Treaty is a compiler, not a host: this config does not start anything. The one
 * `treaty(...)` plugin (`@treaty/vite`) contributes both halves of the
 * file-routing story, with the routing logic living ONCE in Rust:
 *
 *   - it lowers every route authoring file (`.treaty` / `.tjsx`) the route graph
 *     lazily imports to Ivy JS via `@treaty/compiler` -> the Rust addon; and
 *   - via `fileRoutes`, it serves `import { routes } from 'virtual:treaty-routes'`
 *     by driving the Rust file-routing core (`@treaty/authoring-node`.
 *     `generateRoutes`, the shim over `treaty_file_routing`) over the on-disk
 *     `routes/` + `api/` tree on every load. There is NO checked-in / prebuilt
 *     `routes.ts` and NO prebuild step — the route graph is generated from the
 *     filesystem at build time, and editing/adding/removing a route file
 *     regenerates the virtual module in dev (the referenced route files are
 *     registered as Vite watch dependencies).
 *
 * `routesRoot` is this app's directory (it contains `routes/` and `api/`).
 * `dynamicSegmentStyle: 'colon'` makes dynamic segments Angular-router-native
 * (`[slug]` -> `:slug`), so the generated graph is directly consumable by
 * `provideRouter`. `importBase` is the absolute app root: because the virtual
 * module has no on-disk location, the emitted lazy `import('<root>/routes/…')`
 * loaders use an absolute base so Vite can resolve each real route file (and the
 * Treaty plugin then lowers it).
 */
import { fileURLToPath } from 'node:url'
import { dirname } from 'node:path'

import { defineConfig } from 'vite'
import treaty from '@treaty/vite'

const appRoot = dirname(fileURLToPath(import.meta.url))
// Vite resolves module ids with POSIX separators, so the absolute import base the
// emitted lazy loaders use must be forward-slashed even on Windows.
const importBase = appRoot.replace(/\\/g, '/')

export default defineConfig({
	build: {
		target: 'es2022',
		outDir: 'dist',
	},
	plugins: [
		treaty({
			sourceMap: true,
			// File routing as a build-time virtual module (no prebuilt routes.ts).
			fileRoutes: {
				routesRoot: appRoot,
				// Angular-router-native dynamic segments (:slug) for provideRouter.
				dynamicSegmentStyle: 'colon',
				// The virtual module has no on-disk path, so the lazy loaders need an
				// absolute base to resolve the real route files.
				importBase,
			},
			// Cold-build prewarm: batch-compile the route authoring files up front in
			// one parallel round trip through the Rust addon so the per-module
			// transforms during the build are served from the incremental cache.
			prewarm: [
				'src/app/routed-root.component.ts',
				'routes/layout.treaty',
				'routes/index.treaty',
				'routes/not-found.treaty',
				'routes/(marketing)/index.treaty',
				'routes/(marketing)/about.tjsx',
				'routes/blog/layout.treaty',
				'routes/blog/index.treaty',
				'routes/blog/[slug]/index.treaty',
				'routes/blog/[...path]/index.treaty',
				'routes/docs/[category]/[page]/index.tjsx',
			],
		}),
	],
})
