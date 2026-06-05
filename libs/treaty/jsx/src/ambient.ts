/**
 * @module
 *
 * Ambient authoring-module declaration shipped by `@treaty/jsx` for the JSX
 * authoring surface it owns: the `.tjsx` extension.
 *
 * A `.tjsx` file is lowered to a standalone Ivy component by the Treaty compiler
 * at build time. tsgo cannot parse the extension directly, so a plain `.ts` /
 * `.tsx` host that does `import C from "./x.tjsx"` would otherwise see an
 * unresolved module. This ambient `declare module` shim restates exactly what
 * the compiler emits: the authoring file default-exports the lowered,
 * selectorless standalone component VALUE.
 *
 * Shipping the shim FROM this package (rather than hand-writing a
 * `declare module "*.tjsx"` block per app) is the whole point: an app picks it
 * up with a SINGLE reference — either
 *
 *   /// <reference types="@treaty/jsx/ambient" />
 *
 * once in the project (e.g. a top-level `env.d.ts`), or by adding
 * `"@treaty/jsx/ambient"` to `compilerOptions.types` in `tsconfig.json`.
 *
 * Scope: `@treaty/jsx` owns ONLY the JSX surface — the `.tjsx` module shim here,
 * plus the global `JSX` namespace and `jsxImportSource` / `jsx-runtime` element
 * typing in `./index` / `./jsx-runtime`. The `.treaty` single-file-component
 * module shim is NOT a JSX concern: it ships from `@treaty/compiler` (the
 * package that owns the `.treaty` format) at `@treaty/compiler/ambient`.
 *
 * The component is typed as Angular's `Type<unknown>` so it is assignable to
 * `imports: Type<unknown>[]` and usable as a `loadComponent` target; the
 * concrete synthesized component is an assignable superset. Nothing here runs —
 * these are type-only declarations.
 */

declare module '*.tjsx' {
	/**
	 * The lowered, selectorless standalone component the Treaty compiler emits
	 * for a `.tjsx` JSX authoring file. Typed as Angular's `Type<unknown>` so it
	 * slots into `imports: Type<unknown>[]` and `loadComponent` without a cast;
	 * the synthesized component is an assignable superset.
	 */
	const component: import('@angular/core').Type<unknown>
	export default component
}
