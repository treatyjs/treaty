/**
 * Ambient declaration for the `@treaty/ts-vite` surface that `@treaty/vite` consumes.
 *
 * `@treaty/vite` reuses the Angular partial-declaration linker plugins from `@treaty/ts-vite`
 * (one source of truth, Rust-backed) rather than re-implementing them. `@treaty/ts-vite` is a
 * CommonJS package whose published types are not part of `@treaty/vite`'s composite program, so we
 * declare only the single consumed export here. The real `createLinkPartialPlugins` from
 * `@treaty/ts-vite` returns `import('vite').Plugin[]`, which is assignable to this shape; the runtime
 * `import { createLinkPartialPlugins } from '@treaty/ts-vite'` in `index.ts` resolves to the real
 * package.
 */
declare module '@treaty/ts-vite' {
	import type { Plugin } from 'vite'
	/**
	 * Build the Angular partial-declaration linker Vite plugins (the `enforce: 'pre'` transform, the
	 * `optimizeDeps` esbuild prebundle link plugin + `@angular/compiler` exclusion, and the dev-serve
	 * `index.html` guard) so a host plugin can spread them into its own plugin array.
	 */
	export function createLinkPartialPlugins(): Plugin[]

	// File routing as a virtual module, generated DURING the build by the SHARED Rust-backed helper
	// in `@treaty/ts-vite` (the shim over `treaty_file_routing` via `@treaty/authoring-node`).
	// `@treaty/vite` reuses this one helper and only adds the Vite resolveId/load wiring.

	/** The bare virtual module specifier (`virtual:treaty-routes`) a Treaty app imports for its routes. */
	export const TREATY_ROUTES_ID: string
	/** The resolved virtual id (`\0virtual:treaty-routes`) bundlers map {@link TREATY_ROUTES_ID} to. */
	export const RESOLVED_TREATY_ROUTES_ID: string
	/** Whether `id` is the Treaty routes virtual module (bare or resolved, query/hash ignored). */
	export function isTreatyRoutesId(id: string): boolean

	/** Knobs forwarded to the Rust file-routing core; only `routesRoot` is required. */
	export interface RoutesVirtualModuleOptions {
		readonly routesRoot: string
		readonly cwd?: string
		readonly routesDir?: string
		readonly apiDir?: string
		readonly dynamicSegmentStyle?: 'bracket' | 'colon'
		readonly federation?: boolean
		readonly importBase?: string
	}
	/** The emitted routes module plus the route entry files it references (watch deps). */
	export interface GeneratedRoutesModule {
		code: string
		files: string[]
		watchFiles: string[]
	}
	/**
	 * Generate the Treaty file-routing virtual module DURING a build: resolve the routes root, drive
	 * the Rust `generateRoutes` core, and return the emitted TypeScript module plus its route-file
	 * watch dependencies. Throws when the `@treaty/authoring-node` addon is unavailable.
	 */
	export function generateRoutesModule(options: RoutesVirtualModuleOptions): GeneratedRoutesModule
}
