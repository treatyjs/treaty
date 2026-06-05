/**
 * Profile settings component. Selectorless Angular `@Component` with a signal.
 */
import { Component, signal } from '@angular/core'

@Component({
	template: `<h1>Profile settings</h1>
		<p>theme: {{ theme() }}</p>`,
})
export class ProfileSettingsComponent {
	readonly theme = signal('dark')
}
