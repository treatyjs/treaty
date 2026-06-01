/**
 * SELECTORLESS attribute directive (`.ts`, `@Directive`).
 *
 * Like every Treaty surface this is authored WITHOUT a `selector` and WITHOUT
 * `standalone: true` -- the compiler fills those defaults in. It is consumed
 * SELECTORLESSLY: the host (`metrics-panel.component.ts`) lists the directive
 * CLASS by value in its `imports` and references it by NAME in the template
 * (`<strong HighlightDelta ...>`), so there is no string selector to match. The
 * Treaty compiler lowers this `@Directive` source to a real Ivy `ɵɵdefineDirective`
 * (+ `ɵfac`) AOT -- no raw `@Directive` decorator survives, so it never falls to
 * Angular's JIT at runtime -- and the host lists it in its component
 * `dependencies` so the selectorless `HighlightDelta` reference binds.
 *
 * Behavior: paints its host green when the bound `delta` is positive, red when
 * negative, and clears the color at zero -- a tiny, real "trend" affordance.
 */
import { Directive, ElementRef, computed, effect, inject, input } from '@angular/core'

@Directive({
	// No `selector` -- selectorless; referenced by class name in the host.
	// No `standalone: true` -- the compiler fills the default in.
	host: { '[style.color]': 'tint()' },
})
export class HighlightDelta {
	/** Signal input -- the value to react to (matched by name in the template). */
	readonly delta = input(0)

	private readonly host = inject(ElementRef<HTMLElement>)

	/** Derived tint: green up, red down, inherit at zero. */
	readonly tint = computed(() =>
		this.delta() > 0 ? '#059669' : this.delta() < 0 ? '#dc2626' : 'inherit'
	)

	constructor() {
		// Reflect the sign as a data attribute too, for styling/testing hooks.
		effect(() => {
			this.host.nativeElement.dataset['trend'] =
				this.delta() > 0 ? 'up' : this.delta() < 0 ? 'down' : 'flat'
		})
	}
}
