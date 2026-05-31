/**
 * @module
 *
 * Ambient authoring-module declarations shipped by `@treaty/jsx`.
 *
 * Treaty's authoring formats — the `.treaty` single-file component and the
 * `.tjsx` JSX surface — are lowered to standalone Ivy components by the Treaty
 * compiler at build time. tsgo cannot parse those extensions directly, so a
 * plain `.ts` / `.tsx` host that does `import C from "./x.treaty"` would
 * otherwise see an unresolved module. These ambient `declare module` shims
 * restate exactly what the compiler emits: each authoring file
 * default-exports the lowered, selectorless standalone component VALUE.
 *
 * Shipping the shims FROM this package (rather than hand-writing a
 * `declare module "*.treaty"` block per app) is the whole point: an app picks
 * them up with a SINGLE reference — either
 *
 *   /// <reference types="@treaty/jsx/ambient" />
 *
 * once in the project (e.g. a top-level `env.d.ts`), or by adding
 * `"@treaty/jsx/ambient"` to `compilerOptions.types` in `tsconfig.json`. No
 * per-app `declare module` blocks are needed.
 *
 * The component is typed as Angular's `Type<unknown>` so it is assignable to
 * `imports: Type<unknown>[]` and usable as a `loadComponent` target; the
 * concrete component the compiler synthesizes is an assignable superset.
 * Nothing here runs — these are type-only declarations.
 *
 * This file deliberately ships ONLY the authoring-module shims. The global
 * `JSX` namespace and the `jsxImportSource` / `jsx-runtime` element typing are
 * owned by the package's other entries (`./index`, `./jsx-runtime`) and are
 * unaffected by referencing this one.
 */

declare module '*.treaty' {
	/**
	 * The lowered, selectorless standalone component the Treaty compiler emits
	 * for a `.treaty` single-file component. Typed as Angular's `Type<unknown>`
	 * so it slots into `imports: Type<unknown>[]` and `loadComponent` without a
	 * cast; the synthesized component is an assignable superset.
	 */
	const component: import('@angular/core').Type<unknown>
	export default component
}

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
