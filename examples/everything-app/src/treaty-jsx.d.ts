/**
 * Ambient declarations for the Treaty JSX authoring dialect.
 *
 * Treaty's `.tsx` authoring is NOT React: the compiler lowers JSX directly to
 * Ivy. These declarations exist only so the showcase `.tsx` sources are
 * `tsgo`-clean while still exercising Treaty-specific JSX features the standard
 * React typings would reject:
 *
 *   - lowercase component classes used as elements (`<counter />`)
 *   - `use:<directive>` attributes (structural/attribute directive application)
 *   - `class` instead of React's `className`
 *   - `{signal()}` interpolation (signals are called in the body, not in JSX)
 *
 * The real types come from the compiler output; this is purely an authoring-time
 * shim local to the example so the dialect typechecks without a host runtime.
 */

declare namespace JSX {
	/** A Treaty JSX element resolves to Ivy at compile time; opaque here. */
	type Element = unknown

	/** Every intrinsic/host element accepts arbitrary attributes plus `use:`. */
	interface TreatyAttributes {
		/** Treaty uses HTML's `class`, never React's `className`. */
		class?: string
		/** Inline style as a string (HTML semantics). */
		style?: string
		/** DOM-style click handler; lowered to an Ivy `(click)` binding. */
		onClick?: (event: unknown) => void
		/**
		 * `use:<name>` directive application. The compiler matches the suffix to
		 * a directive in scope; any boolean/value is accepted at authoring time.
		 */
		[useDirective: `use:${string}`]: unknown
		/** Allow any other attribute / event / property binding. */
		[attr: string]: unknown
	}

	type IntrinsicElements = {
		[elemName: string]: TreatyAttributes
	}

	/** Class components author as lowercase too; the compiler is case-insensitive. */
	interface ElementClass {
		// Marker only; Treaty components need no `render` method.
		readonly __treatyComponent?: true
	}

	interface ElementAttributesProperty {
		// Props are derived from the class' signal inputs by the compiler.
		props: unknown
	}

	interface IntrinsicAttributes extends TreatyAttributes {}
}
