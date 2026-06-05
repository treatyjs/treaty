/**
 * Plain Angular `@Component` authoring (`.ts`).
 *
 * This is base Angular -- a class with a `@Component` decorator -- but it still
 * benefits from the same Treaty compiler: it is SELECTORLESS (no `selector`),
 * the compiler fills in `standalone` and signal defaults, and it consumes a
 * STREAM-transport server fn. It uses signals + `@for`/`@if` control flow in an
 * inline template.
 */
import { Component, signal } from '@angular/core'
import { streamLogs, type LogLine } from '../server/logs.stream'

@Component({
	// No `selector` -- Treaty is selectorless; the compiler synthesizes one.
	template: `
		<section class="logs">
			<h2>Server logs</h2>
			@if (lines().length) {
				<ol>
					@for (line of lines(); track line.seq) {
						<li [attr.data-level]="line.level">
							<code>[{{ line.level }}]</code> {{ line.message }}
						</li>
					}
				</ol>
			} @else {
				<p>waiting for stream...</p>
			}
		</section>
	`,
})
export class LogViewer {
	readonly lines = signal<LogLine[]>([])

	constructor() {
		void this.tail()
	}

	/**
	 * Stream-transport call site: `streamLogs` is an async generator on the
	 * server. The client binding is an async iterable; each yielded chunk pushes
	 * a new line into the signal, re-rendering the list incrementally.
	 */
	private async tail(): Promise<void> {
		for await (const line of streamLogs(10)) {
			this.lines.update((current) => [...current, line])
		}
	}
}
