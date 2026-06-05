/**
 * Input -- a presentational text input for the treaty-shadcn showcase.
 *
 * Authored in Treaty's JSX dialect (`.tsx`), lowered straight to a standalone,
 * selectorless Ivy component (NOT React). Props are declared as signal `input()`s
 * (the `.tsx` prop form); each becomes an Angular signal input the parent binds,
 * and a bare `{type}` / `{placeholder}` read in the JSX auto-calls the signal.
 *
 * Purely presentational: it renders a single `<input>` with its attributes bound
 * from the inputs. No two-way binding -- the showcase only demonstrates the
 * surface, so there is no `(input)` handler or local state.
 */
import { input } from '@angular/core'

// Signal INPUTS -- bound by the host: <Input type="email" placeholder="you@x.com" />.
const type = input<'text' | 'email' | 'password'>('text')
const placeholder = input('')
const disabled = input(false)
const value = input('')

export default function input_() {
	// `[type]`, `[placeholder]`, `[disabled]` and `[value]` are property bindings to
	// the signal inputs (the `{expr}` JSX form lowers to Angular `[prop]="expr"`).
	return (
		<input
			type={type}
			placeholder={placeholder}
			disabled={disabled}
			value={value}
			class="input"
		/>
	)
}
