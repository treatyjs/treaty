/**
 * Vite build for the treaty-shadcn-demo, wired to the Treaty compiler.
 *
 * Treaty is a compiler, not a host: this config does not start anything. The
 * developer runs Vite over it (`vite build` / `vite dev`). Treaty's only
 * contribution is the authoring plugin `treaty(...)` (the `@treaty/vite` plugin),
 * which lowers the gallery's app-root `@Component` `.ts` to Ivy JS via
 * `@treaty/compiler` → the Rust addon. The six library components are consumed
 * PRE-COMPILED (already Ivy) from `examples/treaty-shadcn/dist`, so they ride
 * through Vite as plain ES modules.
 *
 * The library is not installed in node_modules (it is a sibling example, packaged
 * to its own `dist/`), so we resolve the `treaty-shadcn` package + every
 * per-component subpath to its built `.mjs` via a workspace ALIAS — the bundler
 * equivalent of the package's `exports` map. This is the consumer path the task
 * asks for: import the components from the BUILT library.
 */
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'

import { defineConfig, type Plugin } from 'vite'
import { transform } from 'esbuild'
import treaty from '@treaty/vite'

const here = dirname(fileURLToPath(import.meta.url))
const shadcnDist = join(here, '..', 'treaty-shadcn', 'dist')

/**
 * Type-strip the consumed library's published ESM.
 *
 * The packagr emits each component's lowered Ivy as `dist/<name>/index.mjs`, but
 * the body still carries some authored TypeScript that survives lowering — signal
 * `input<T>()` generics, a `type` alias, an `as` cast (e.g. `alert/index.mjs`).
 * Vite/Rollup treat a `.mjs` as plain JavaScript, so those TS tokens are a parse
 * error in the consumer build. `@treaty/vite` claims only the authoring extensions
 * (`.treaty`/`.tjsx`/`.tsx`/`@Component` `.ts`), NOT a dependency's `.mjs`, so the
 * consumer owns this strip — exactly as the Treaty plugin strips a `.treaty`/
 * `.tjsx` body itself. We run esbuild's `ts` loader over the library's `.mjs`
 * (value-preserving: only types are removed) so the real built artifact links
 * unchanged. Scoped to the `treaty-shadcn/dist` tree so no app code is touched.
 */
function stripLibraryTypes(): Plugin {
	// Vite/Rollup module ids are POSIX-slashed even on Windows; normalize the dist
	// root the same way so the prefix match works cross-platform.
	const distPrefix = shadcnDist.replace(/\\/g, '/') + '/'
	return {
		name: 'treaty-shadcn-demo:strip-library-types',
		enforce: 'pre',
		async transform(code, id) {
			const path = id.split('?')[0].replace(/\\/g, '/')
			if (!path.startsWith(distPrefix) || !path.endsWith('.mjs')) return null
			const out = await transform(code, { loader: 'ts', sourcemap: true, sourcefile: id })
			return { code: out.code, map: out.map }
		},
	}
}

// Map the library's package specifier + each per-component subpath export to its
// compiled `dist/<name>/index.mjs`. Mirrors the `exports` map in
// `examples/treaty-shadcn/dist/package.json` (the publishable artifact). The
// per-subpath aliases come BEFORE the bare-package alias so `treaty-shadcn/button`
// does not get swallowed by the `treaty-shadcn` → `index.mjs` rule.
const shadcnAliases = [
	{ find: 'treaty-shadcn/button', replacement: join(shadcnDist, 'button', 'index.mjs') },
	{ find: 'treaty-shadcn/badge', replacement: join(shadcnDist, 'badge', 'index.mjs') },
	{ find: 'treaty-shadcn/card', replacement: join(shadcnDist, 'card', 'index.mjs') },
	{ find: 'treaty-shadcn/alert', replacement: join(shadcnDist, 'alert', 'index.mjs') },
	{ find: 'treaty-shadcn/input', replacement: join(shadcnDist, 'input', 'index.mjs') },
	{ find: 'treaty-shadcn/switch', replacement: join(shadcnDist, 'switch', 'index.mjs') },
	{ find: 'treaty-shadcn', replacement: join(shadcnDist, 'index.mjs') },
]

export default defineConfig({
	// `<app-root>` lives in index.html; Vite uses it as the build entry and bundles
	// `src/main.ts` (the bootstrap) from there.
	build: {
		target: 'es2022',
		outDir: 'dist',
	},
	resolve: {
		alias: shadcnAliases,
	},
	plugins: [
		// Strip residual TS from the consumed library `.mjs` BEFORE Rollup parses them.
		stripLibraryTypes(),
		treaty({
			sourceMap: true,
			// Cold-build prewarm: batch-compile the one authoring entry up front through
			// the Rust addon so the per-module transform is served from the cache.
			prewarm: ['src/app/app-root.component.ts'],
		}),
	],
})
