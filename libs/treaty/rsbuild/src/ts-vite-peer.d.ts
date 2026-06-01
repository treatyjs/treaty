/**
 * Ambient declaration for the bundler-agnostic `@treaty/ts-vite` linker surface that
 * `@treaty/rsbuild` consumes.
 *
 * `@treaty/rsbuild` reuses the Angular partial-declaration linker from `@treaty/ts-vite` (one source
 * of truth, Rust-backed via `@treaty/authoring-node`.`linkPartial`) rather than re-implementing it.
 * `@treaty/ts-vite` is a CommonJS package whose published types are not part of `@treaty/rsbuild`'s
 * composite program, so we declare only the two bundler-agnostic exports this package calls. The
 * Vite-shaped `createLinkPartialPlugins` is intentionally NOT declared here — it returns
 * `import('vite').Plugin[]`, which is unusable by Rsbuild; rsbuild wires the linker through its own
 * `api.transform` hook instead.
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

	// File routing as a virtual module, generated DURING the build by the SHARED Rust-backed helper
	// in `@treaty/ts-vite` (the shim over `treaty_file_routing` via `@treaty/authoring-node`).
	// `@treaty/rsbuild` reuses this one helper and only adds the alias + Rspack loader-rule wiring.

	/** The bare virtual module specifier (`virtual:treaty-routes`) a Treaty app imports for its routes. */
	export const TREATY_ROUTES_ID: string
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
