/**
 * JSX authoring (`.tsx`) -- Treaty's React-flavored dialect lowered straight to
 * Ivy (NOT React). This single file exercises the JSX-specific features:
 *
 *   - a LOWERCASE component (`counter`) used as an element (`<counter />`)
 *   - `use:class` and a custom `use:highlight` directive (directive application)
 *   - `{count()}` signal interpolation (signals are called inline)
 *   - an `onClick` handler (lowered to an Ivy `(click)` binding)
 *   - an `items.map(...)` list and a conditional (lowered to Ivy `@if`)
 *
 * Treaty's JSX components author as plain functions returning JSX; the compiler
 * lowers them to standalone, signal-based, selectorless Ivy components -- no
 * @Component decorator, selector, or `standalone: true` boilerplate.
 */
import { signal, computed } from '@angular/core'
import { wsPresence, type PresenceEvent } from '../server/presence.ws'

// A custom attribute directive referenced via `use:highlight` below. In Treaty
// JSX, `use:` applies a directive to the element; the compiler matches it by name.
export function highlight(): void {
	// Directive behavior lives in the lowered output; this stub keeps it in scope.
}

// Lowercase component function -- Treaty is case-insensitive for component names.
export default function counter() {
	const count = signal(0)
	const step = signal(1)
	const doubled = computed(() => count() * 2)
	const events = signal<PresenceEvent[]>([])

	// WebSocket-transport call site: opens a live channel. The body of
	// `wsPresence` stays on the server; here we only hold the client binding.
	const socket = wsPresence('me', (event) => {
		events.update((list) => [...list, event])
	})
	void socket

	const increment = (): void => {
		count.update((n) => n + step())
	}

	return (
		<section class="counter" use:highlight>
			<button class="bump" use:class={{ active: count() > 0 }} onClick={increment}>
				count is {count()}
			</button>
			<p>doubled: {doubled()}</p>

			{/* Conditional + list. The ternary lowers to an Ivy `@if`/`@else`. */}
			{events().length ? (
				<ul class="presence">
					{events().map((event) => (
						<li class="peer">
							{event.userId} is {event.status}
						</li>
					))}
				</ul>
			) : (
				<p class="idle">no presence yet</p>
			)}
		</section>
	)
}
