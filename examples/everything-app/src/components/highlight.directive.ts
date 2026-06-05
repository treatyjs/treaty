/**
 * SELECTORLESS attribute directive for the JSX `counter.tsx` showcase.
 *
 * It is the REAL directive behind `counter.tsx`'s `use:highlight` application: a
 * genuine, visible host affordance (NOT a no-op). Authored the Treaty way --
 * WITHOUT a `selector` and WITHOUT `standalone: true` (the compiler fills those
 * defaults in) -- and consumed SELECTORLESSLY: `counter.tsx` imports the class by
 * value and applies it with `use:highlight`, which the JSX front-end resolves to
 * this `Highlight` class and lists in the component's `dependencies`. The Treaty
 * `.ts` decorator path lowers this `@Directive` to a real Ivy `ɵɵdefineDirective`
 * (+ `ɵfac`) AOT, so no raw `@Directive` decorator survives to fall to Angular's
 * JIT at runtime.
 *
 * Behavior: paints an accent left-border and a soft tinted background on its host
 * element, and reflects an `data-highlight` hook for styling/testing -- a small,
 * real "this region is interactive" affordance for the counter card. The host
 * styling is applied through an `effect` over the directive's own signals, so it
 * is reactive and tears down with the element.
 */
import { Directive, ElementRef, computed, effect, inject, signal } from '@angular/core'

@Directive({
	// No `selector` -- selectorless; referenced by `use:highlight` in counter.tsx.
	// No `standalone: true` -- the compiler fills the default in.
	host: {
		'[style.borderLeft]': 'accentBorder()',
		'[style.background]': 'tint()',
		'[style.paddingLeft]': '"0.75rem"',
		'[attr.data-highlight]': '"on"',
	},
})
export class Highlight {
	/** The accent hue the affordance paints with (a real signal the host binds to). */
	readonly accent = signal('#6366f1')

	/** The host element, so the directive can also drive imperative style on init. */
	private readonly host = inject(ElementRef<HTMLElement>)

	/** Derived host bindings: a solid accent border and a translucent tinted wash. */
	readonly accentBorder = computed(() => `3px solid ${this.accent()}`)
	readonly tint = computed(() => `color-mix(in srgb, ${this.accent()} 8%, transparent)`)

	constructor() {
		// Reflect the accent as a CSS custom property on the host too, so descendant
		// rules (e.g. the bump button) can pick the same hue up. Runs reactively.
		effect(() => {
			this.host.nativeElement.style.setProperty('--counter-accent', this.accent())
		})
	}
}
