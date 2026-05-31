/**
 * Profile feature root component. Selectorless Angular `@Component` with a
 * signal. One decorated class per file (the unified front-end compiles a single
 * decorated class at a time).
 */
import { Component, signal } from '@angular/core'

@Component({
	template: `<h1>Profile</h1>
		<p>name: {{ name() }}</p>`,
})
export class ProfileComponent {
	readonly name = signal('Ada Lovelace')
}
