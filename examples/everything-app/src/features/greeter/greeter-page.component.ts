/**
 * Plain Angular `@Component` authoring (`.ts`) -- base Angular, the third
 * authoring surface. It is SELECTORLESS (no `selector`) and carries no
 * `standalone: true` or change-detection boilerplate; the Treaty compiler fills
 * those defaults in. It uses a `signal` + `@if`/`@for` control flow inline.
 *
 * Being default-exported, this is the lazy `loadComponent` target for the
 * `greeter` route -- a lazy boundary Treaty's `deriveExposesFromRoutes` turns
 * into an independently deployable Module Federation remote. It hosts the
 * `.treaty` and `.tjsx` components selectorlessly (the compiler auto-imports
 * them by class/function name), so one remote shows all three surfaces interop.
 */
import { Component, signal } from '@angular/core'
import Greeter from './greeter.treaty'
import greetingCard from './greeting-card.tjsx'

@Component({
	// No `selector` -- Treaty is selectorless; the compiler synthesizes one.
	// Children are referenced by value (selectorless auto-import), not strings.
	imports: [Greeter, greetingCard],
	template: `
		<section class="greeter-page">
			<h1>Greeter</h1>
			@if (tabs().length) {
				<nav>
					@for (tab of tabs(); track tab) {
						<button type="button" (click)="active.set(tab)">{{ tab }}</button>
					}
				</nav>
			}
			<Greeter />
			<greetingCard />
		</section>
	`,
})
export default class GreeterPageComponent {
	readonly tabs = signal<readonly string[]>(['sfc', 'jsx'])
	readonly active = signal('sfc')
}
