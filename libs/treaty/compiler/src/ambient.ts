/**
 * @module
 *
 * Ambient authoring-module declaration shipped by `@treaty/compiler` for the
 * `.treaty` single-file-component format it owns.
 *
 * A `.treaty` file is lowered to a standalone Ivy component by the Treaty
 * compiler at build time. tsgo cannot parse the extension directly, so a plain
 * `.ts` / `.tsx` host that does `import C from "./x.treaty"` would otherwise see
 * an unresolved module. This ambient `declare module` shim restates exactly what
 * the compiler emits: the authoring file default-exports the lowered,
 * selectorless standalone component VALUE.
 *
 * It lives in `@treaty/compiler` (not `@treaty/jsx`) because `.treaty` is the
 * SFC authoring format, NOT JSX — each authoring format's module shim ships from
 * the package that owns the format (`.tjsx` ships from `@treaty/jsx/ambient`).
 *
 * An app picks it up with a SINGLE reference — either
 *
 *   /// <reference types="@treaty/compiler/ambient" />
 *
 * once in the project (e.g. a top-level `env.d.ts`), or by adding
 * `"@treaty/compiler/ambient"` to `compilerOptions.types` in `tsconfig.json`.
 * No per-app `declare module "*.treaty"` blocks are needed.
 *
 * The component is typed as Angular's `Type<unknown>` so it is assignable to
 * `imports: Type<unknown>[]` and usable as a `loadComponent` target; the
 * concrete synthesized component is an assignable superset. Nothing here runs —
 * these are type-only declarations.
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
