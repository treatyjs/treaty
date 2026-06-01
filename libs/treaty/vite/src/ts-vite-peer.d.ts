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
}
