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
 *  - standard DOM events (`onClick`, `onInput`),
 *  - DIRECTIVE authoring as a function returning a host spec, and its
 *    `use:<name>={input}` application + a selectorless component tag,
 *  - PIPE authoring as a transform function.
 *
 * Nothing here is exported for runtime use; the file exists so `tsgo` fails if
 * the JSX contract regresses.
 */

import type { Directive, HostSpec, Pipe, PipeTransform } from '@treaty/jsx'

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

// --- Directive authoring: a function returning a host spec ---------------
// An input-less directive authored the Treaty way: a function whose body runs
// setup and returns a `{ host: { … } }` spec. `Directive` types it with no
// hand-declared interface; the returned object is checked against `HostSpec`.
const highlight: Directive = (): HostSpec => ({
	host: { '[attr.data-hl]': 'on()' },
})

// A directive that accepts an input — `Directive<number>` types the value a
// `use:emphasis={level}` application passes; the function sees `level?: number`.
const emphasis: Directive<number> = (level): HostSpec => ({
	host: { '[style.fontWeight]': `${(level ?? 0) > 0 ? 'bold' : 'normal'}` },
})

// --- Pipe authoring: a transform function --------------------------------
// A pipe authored as a transform function: `Pipe<In, Out, Args>` checks the
// piped value, the result, and the trailing pipe arguments.
const percent: Pipe<number, string, [fractionDigits?: number]> = (ratio, fractionDigits = 0) =>
	`${(ratio * 100).toFixed(fractionDigits)}%`

// The class form satisfies the Angular-shaped `PipeTransform` contract.
const upper: PipeTransform = {
	transform(value: unknown): string {
		return String(value).toUpperCase()
	},
}

// A selectorless component referenced by value as an element tag, and the
// `use:<name>` application of the directives authored above.
function panel(): JSX.Element {
	return <section class="panel">{label}</section>
}

const applied: JSX.Element = (
	<panel use:highlight use:emphasis={2} />
)

void natural
void directive
void mapped
void fragment
void highlight
void emphasis
void percent
void upper
void applied
