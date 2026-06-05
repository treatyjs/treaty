/// <reference path="../../libs/treaty/compiler/dist/ambient.d.ts" />
/// <reference path="../../libs/treaty/jsx/dist/ambient.d.ts" />

/**
 * Ambient module declarations for this example.
 *
 * The two triple-slash references above bring in the authoring-format module
 * shims from the packages that OWN each format — `*.treaty` from
 * `@treaty/compiler` (its `./ambient` entry) and `*.tjsx` / `*.tsx` from
 * `@treaty/jsx` — so the generated route module's lazy `import('…/foo.treaty')`
 * loaders typecheck without a per-app `declare module` block. This workspace
 * consumes the libs from their built `dist`, so the references target the
 * shipped `dist/ambient.d.ts` files directly; an app that installs the packages
 * as real dependencies uses the package-name form instead
 * (`/// <reference types="@treaty/compiler/ambient" />`) or `compilerOptions.types`.
 *
 * Treaty is a compiler, not a host: nothing here runs; these declarations only
 * keep the authoring-time route graph type-clean.
 */

/**
 * The build-time file-routing virtual module served by `@treaty/vite`'s
 * `fileRoutes` option. There is no on-disk `routes.ts`: `@treaty/vite` generates
 * this module from the `routes/` + `api/` tree via the Rust file-routing core on
 * every load. This ambient declaration types the import so the app bootstrap
 * typechecks without the module existing on disk; the emitted shape matches the
 * `treaty_file_routing` TS emitter (`routes` / default export plus the
 * `federationRemotes` descriptor list).
 */
declare module 'virtual:treaty-routes' {
	/** The Angular route graph generated from the `routes/` directory tree. */
	export const routes: import('@angular/router').Routes
	export default routes

	/** Module-Federation remotes derived from the lazy route + layout boundaries. */
	export const federationRemotes: ReadonlyArray<{
		readonly name: string
		readonly exposedModule: string
		readonly entryFile: string
		readonly routePath: string
	}>
}
