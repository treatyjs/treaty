/**
 * Vite build for the file-routed-app, wired to the Treaty compiler and the
 * file-system route generator.
 *
 * Treaty is a compiler, not a host: this config does not start anything. Two
 * contributions:
 *
 *   - `treaty(...)` — the `@treaty/vite` plugin. Lowers every route authoring
 *     file (`.treaty` / `.tjsx`) the generated route graph lazily imports to Ivy
 *     JS via `@treaty/compiler` -> the Rust addon.
 *   - `fileRoutesPlugin()` — a tiny local plugin that runs the route generator
 *     (`scripts/generate-routes.mjs`) on `buildStart`, so `src/generated/routes.ts`
 *     is always regenerated from the on-disk `routes/` tree before the bundle is
 *     built. This is the JS surface for the `treaty_file_routing` convention: the
 *     same `routes/` -> Angular-routes + Module-Federation-remotes lowering the
 *     crate performs, wired into the build.
 *
 * The route generator and the Treaty plugin are decoupled: the generator only
 * emits a plain `.ts` route module (lazy `import('…/foo.treaty')` loaders); the
 * Treaty plugin then owns lowering each `.treaty` / `.tjsx` target as Vite pulls
 * it into the graph.
 */
import { execFileSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'

import { defineConfig, type Plugin } from 'vite'
import treaty from '@treaty/vite'

const here = dirname(fileURLToPath(import.meta.url))
const generator = join(here, 'scripts', 'generate-routes.mjs')

/**
 * Regenerate `src/generated/routes.ts` from the `routes/` tree before each
 * build/dev start. Running the generator as a child process keeps the file-route
 * lowering in one place (`scripts/generate-routes.mjs`, also runnable by hand)
 * and guarantees the generated module is current with the directory tree.
 */
function fileRoutesPlugin(): Plugin {
	return {
		name: 'treaty:file-routes',
		enforce: 'pre',
		buildStart() {
			execFileSync(process.execPath, [generator], { cwd: here, stdio: 'inherit' })
		},
	}
}

export default defineConfig({
	build: {
		target: 'es2022',
		outDir: 'dist',
	},
	plugins: [
		fileRoutesPlugin(),
		treaty({
			sourceMap: true,
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
