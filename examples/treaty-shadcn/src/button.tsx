/**
 * Button -- a presentational shadcn-style button for the treaty-shadcn showcase.
 *
 * Authored in Treaty JSX (`.tsx`): a plain function returning JSX, lowered
 * straight to a standalone, signal-based, selectorless Ivy component (no
 * @Component, selector, or `standalone: true`). Props are declared as signal
 * INPUTS via `input(...)`; the parent binds them as `<Button [variant]="..." />`.
 *
 * Pure presentational: no handlers. The host element's native click bubbles to
 * the parent, so a consumer wires `(click)` on the `<Button>` itself.
 */
import { input, computed } from '@angular/core'

export default function button() {
	// Signal INPUTS -- bound by the host. Defaults make every prop optional.
	const variant = input<'default' | 'outline' | 'ghost' | 'destructive'>('default')
	const size = input<'sm' | 'md' | 'lg'>('md')
	const disabled = input(false)
	const label = input<string>('')

	// Class built from variant + size, e.g. `btn btn-outline btn-lg`.
	const className = computed(() => `btn btn-${variant()} btn-${size()}`)

	return (
		<button class={className()} disabled={disabled()}>
			{label()}
		</button>
	)
}
