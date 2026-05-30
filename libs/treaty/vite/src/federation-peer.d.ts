/**
 * Ambient declaration for the optional `@module-federation/vite` peer so
 * `@treaty/vite` typechecks (and the base plugin runs) without it installed.
 * The dynamic `import('@module-federation/vite')` in `index.ts` resolves to
 * this shape; the real package's exports are assignable to it.
 */
declare module '@module-federation/vite' {
	const federation: (options: unknown) => unknown
	export { federation }
	export default federation
}
