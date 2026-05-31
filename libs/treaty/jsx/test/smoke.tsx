/**
 * Type-level smoke fixture for `@treaty/jsx`.
 *
 * Compiled (typecheck-only) with `"jsx": "react-jsx"` +
 * `"jsxImportSource": "@treaty/jsx"` via `tsconfig.smoke.json`. It exercises the
 * load-bearing claims of the shipped ambient types:
 *
 *  - lowercase intrinsic elements (`div`, `button`, `input`),
 *  - `class` as the natural attribute form (plus the `className` alias),
 *  - a `use:autofocus` directive attribute (bare-boolean form),
 *  - a bound `{signal}` value accepted wherever the plain value is,
 *  - standard DOM events (`onClick`, `onInput`).
 *
 * Nothing here is exported for runtime use; the file exists so `tsgo` fails if
 * the JSX contract regresses.
 */

// A minimal signal stand-in: a zero-argument getter, structurally compatible
// with Angular's `Signal<T>` and Treaty's `TreatyJsx.ReadableSignal<T>`.
declare const count: () => number
declare const label: () => string
declare function handleClick(event: MouseEvent): void
declare function handleInput(event: Event): void

// Lowercase `class` (natural form) + bound `{signal}` text + DOM event.
const natural: JSX.Element = (
	<div class="card" onClick={handleClick}>
		{count}
	</div>
)

// `use:autofocus` bare directive + `className` alias + `{signal}` attribute.
const directive: JSX.Element = (
	<input
		className="field"
		use:autofocus
		value={label}
		onInput={handleInput}
	/>
)

// `class` accepting a conditional map, and a plain (unwrapped) number child.
const mapped: JSX.Element = (
	<button class={{ active: true, disabled: false }} type="button">
		{42}
	</button>
)

// Fragment with mixed children, including a bound signal in content position.
const fragment: JSX.Element = (
	<>
		<span class="a">{label}</span>
		<span>{count}</span>
	</>
)

void natural
void directive
void mapped
void fragment
