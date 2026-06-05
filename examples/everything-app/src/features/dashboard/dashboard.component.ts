/**
 * Lazy feature: Dashboard. Selectorless Angular `@Component`, default-exported
 * so a route's `loadComponent` can import it. Being a lazy boundary, Treaty's
 * Module Federation derivation turns this into its own deployable remote.
 */
import { Component, signal } from '@angular/core'

@Component({
	template: `
		<h1>Dashboard</h1>
		<p>active widgets: {{ widgets() }}</p>
	`,
})
export default class DashboardComponent {
	readonly widgets = signal(3)
}
