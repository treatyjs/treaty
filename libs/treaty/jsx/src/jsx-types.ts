/**
 * @module
 *
 * The shipped ambient JSX types for Treaty's authoring JSX. Loading this file
 * (via `"types": ["@treaty/jsx"]`, `jsxImportSource: "@treaty/jsx"`, or the
 * `@treaty/lsp` auto-provide) installs a global `JSX` namespace that reflects
 * how Treaty JSX actually authors:
 *
 *  - **Selectorless**: components are referenced by value (the class/function
 *    itself), never by a string selector. The Treaty compiler fills in the
 *    `@Component({ standalone: true, … })` decorator, so authors write plain
 *    functions/classes and the type system treats any callable as an element.
 *  - **Signal-aware**: a bound `{signal}` value is accepted wherever the plain
 *    value is, because the compiler unwraps the signal at the binding site. The
 *    same attribute therefore accepts `T`, a `() => T` getter, or a
 *    `{ (): T }`-shaped signal.
 *  - **Standalone**: there is no `NgModule` ceremony in the types; intrinsic
 *    (lowercase) elements model the DOM, and everything else is a component
 *    value.
 *
 * Angular control-flow (`@if` / `@for` / `@switch`) is handled structurally by
 * the compiler and intentionally does not appear in these types.
 *
 * This module carries no runtime values — only the ambient `global { … }`
 * `JSX` namespace, which builds on the `TreatyJsx` machinery in
 * `./treaty-jsx.ts`. The automatic-runtime entry `./jsx-runtime.ts`
 * re-publishes this `JSX` namespace under its own `JSX` export so
 * `jsxImportSource: "@treaty/jsx"` resolves element typing.
 */

import './treaty-jsx.js'

export {}

declare global {
	/**
	 * Treaty's global JSX contract. TypeScript consults this namespace for every
	 * JSX expression in a `.tsx` / `.tjsx` file compiled with the classic factory
	 * or, when re-exported by the automatic runtime, with
	 * `jsxImportSource: "@treaty/jsx"`.
	 */
	namespace JSX {
		/**
		 * The type produced by evaluating a JSX expression. Treaty lowers every
		 * element to an Ivy view node; from the type system's perspective it is an
		 * opaque element handle.
		 */
		interface Element extends TreatyJsx.Element {}

		/**
		 * Names the prop through which children flow. Treaty components receive
		 * their projected content as `children`, mirroring the de-facto JSX
		 * convention so editors complete it.
		 */
		interface ElementChildrenAttribute {
			children: {}
		}

		/**
		 * The attribute Treaty reads to name an element's children-bearing prop on
		 * the *class* side. Components expose `children` (see
		 * {@link ElementChildrenAttribute}); this keeps the two consistent.
		 */
		interface ElementAttributesProperty {
			props: {}
		}

		/**
		 * The set of lowercase, built-in DOM elements. Each entry is typed with the
		 * HTML attributes valid for that element, the shared global attributes,
		 * Treaty's `use:` directive attributes, and the full DOM event surface.
		 */
		interface IntrinsicElements extends TreatyJsx.IntrinsicElements {}

		/**
		 * Treaty is **selectorless**: a component is any callable or constructable
		 * value, so the element class can be a function component or an Angular-
		 * style class. Its instance/return type is irrelevant to JSX (the compiler
		 * owns instantiation), hence the permissive shape.
		 */
		interface ElementClass extends TreatyJsx.ElementClass {}

		/**
		 * Attribute typing for an intrinsic element. Signal-aware: see
		 * {@link TreatyJsx.Bindable}.
		 */
		type IntrinsicAttributes = TreatyJsx.IntrinsicAttributes

		/**
		 * Attribute typing for a class/function component, including the `key`-style
		 * framework attributes Treaty understands.
		 */
		type LibraryManagedAttributes<C, P> = TreatyJsx.LibraryManagedAttributes<C, P>
	}
}
