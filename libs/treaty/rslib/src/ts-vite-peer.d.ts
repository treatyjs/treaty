/**
 * Ambient declaration for the bundler-agnostic `@treaty/ts-vite` linker surface that
 * `@treaty/rslib` consumes.
 *
 * `@treaty/rslib` reuses the Angular partial-declaration linker from `@treaty/ts-vite` (one source
 * of truth, Rust-backed via `@treaty/authoring-node`.`linkPartial`) rather than re-implementing it.
 * `@treaty/ts-vite` is a CommonJS package whose published types are not part of `@treaty/rslib`'s
 * composite program, so we declare only the two bundler-agnostic exports this package calls. The
 * Vite-shaped `createLinkPartialPlugins` is intentionally NOT declared here — it returns
 * `import('vite').Plugin[]`, which is unusable by rslib; rslib (which builds on rsbuild) wires the
 * linker through its own rsbuild `transform` registration instead.
 */
declare module '@treaty/ts-vite' {
	/**
	 * Cheap detector for a partial-compiled module that must be linked: true only when `id` lives
	 * under `node_modules` AND `code` contains a `ɵɵngDeclare*` call.
	 */
	export function isPartialModule(id: string, code: string): boolean
	/**
	 * Link one partial-compiled Angular module's source to AOT (`ɵɵngDeclare*` → `ɵɵdefine*`) via the
	 * Rust linker. Returns `null` when the module is not partial or the addon is unavailable (the
	 * caller serves the source unchanged); throws when the linker reports diagnostics.
	 */
	export function linkPartialCode(code: string, id: string): string | null
}
