import './jsx-types.js'
/**
 * @module
 *
 * Automatic-runtime entry for Treaty JSX, so projects can set
 * `"jsx": "react-jsx"` + `"jsxImportSource": "@treaty/jsx"` and have TypeScript
 * resolve element typing and the `jsx` / `jsxs` / `Fragment` factory imports
 * from `@treaty/jsx/jsx-runtime`.
 *
 * Treaty's real compiler lowers JSX straight to Ivy view instructions; it does
 * **not** call these factories at runtime. They exist so:
 *
 *  - TypeScript's automatic-runtime check finds `jsx` / `jsxs` / `Fragment`
 *    (and, in dev, `jsxDEV`) and type-checks element creation against the
 *    {@link JSX} namespace re-exported below, and
 *  - any non-Treaty toolchain that *does* emit `jsx(...)` calls (e.g. a plain
 *    `tsc` run over a fixture) still has callable, correctly-typed factories.
 *
 * The side-effecting `./jsx-types.js` import pulls in the ambient global `JSX`
 * / `TreatyJsx` namespaces; the `export import JSX` re-export below is what the
 * automatic runtime consults for element typing under `jsxImportSource`.
 */

/**
 * Re-publish the global `JSX` namespace from the runtime module. Under
 * `jsxImportSource: "@treaty/jsx"`, TypeScript looks up `JSX` as a member of
 * the resolved `jsx-runtime` module before falling back to the global, so
 * exporting it here makes element typing resolve with or without the global.
 */
export import JSX = globalThis.JSX

/** Props bag passed to {@link jsx} / {@link jsxs}: the element's attributes. */
export type JsxProps = Record<string, unknown> & {
	children?: JSX.Element | readonly unknown[] | unknown
}

/**
 * Fragment marker. Treaty lowers `<>…</>` structurally; this symbol stands in
 * for the fragment type/key the automatic runtime expects.
 */
export const Fragment: unique symbol = Symbol.for('@treaty/jsx.Fragment')
export type Fragment = typeof Fragment

/**
 * Create an element with zero or one child. Mirrors the React automatic-runtime
 * signature so `jsxImportSource: "@treaty/jsx"` type-checks. Treaty's compiler
 * normally replaces this call; the runtime implementation returns an opaque
 * element handle for the rare toolchain that actually invokes it.
 */
export function jsx(
	type: string | Function | Fragment,
	props: JsxProps,
	key?: string | number,
): JSX.Element {
	return createElement(type, props, key)
}

/**
 * Create an element with static children (more than one child). The automatic
 * runtime emits `jsxs` instead of `jsx` when the children list is a static
 * array; the runtime behavior is identical here.
 */
export function jsxs(
	type: string | Function | Fragment,
	props: JsxProps,
	key?: string | number,
): JSX.Element {
	return createElement(type, props, key)
}

/**
 * Development-runtime factory. TypeScript emits `jsxDEV` under
 * `jsxImportSource` + `"jsx": "react-jsxdev"`; the extra source/self arguments
 * are accepted and ignored.
 */
export function jsxDEV(
	type: string | Function | Fragment,
	props: JsxProps,
	key?: string | number,
	_isStaticChildren?: boolean,
	_source?: unknown,
	_self?: unknown,
): JSX.Element {
	return createElement(type, props, key)
}

/**
 * Shared element constructor. Returns an opaque, frozen handle carrying the
 * resolved type/props/key. This path runs only when a non-Treaty toolchain
 * actually invokes the factories; the Treaty compiler bypasses it.
 */
function createElement(
	type: string | Function | Fragment,
	props: JsxProps,
	key: string | number | undefined,
): JSX.Element {
	const node = { type, props, key }
	// Cast through unknown: the public element type is intentionally opaque.
	return Object.freeze(node) as unknown as JSX.Element
}
