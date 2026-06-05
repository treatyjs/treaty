/**
 * Lazy feature: Metrics. A selectorless Angular `@Component` (`.ts`) that wires
 * up the new authoring surfaces of this feature:
 *
 *   - the `.treaty` `Gauge` signals-heavy component (computed + effect + three
 *     signal inputs), referenced SELECTORLESSLY (`<Gauge [value]="..." />`);
 *   - the `Percent01Pipe` PIPE, used in this component's own template
 *     (`{{ value() | percent01 }}`);
 *   - the `HighlightDelta` SELECTORLESS directive, applied by NAME on the
 *     readout (`<strong HighlightDelta [delta]="...">`).
 *
 * It is default-exported, so it is the lazy `loadComponent` target for the
 * `metrics` route -- a lazy boundary Treaty's `deriveExposesFromRoutes` turns
 * into an independently deployable Module Federation remote. No `selector`,
 * `standalone: true`, or change-detection boilerplate: the compiler fills the
 * selectorless + standalone + signal defaults in.
 */
import { Component, computed, signal } from '@angular/core'
import Gauge from './gauge.treaty'
import { Percent01Pipe } from './percent.pipe'
import { HighlightDelta } from './highlight-delta.directive'

@Component({
	// Children referenced by VALUE (selectorless auto-import), not by string
	// selector: the `.treaty` component, the pipe class, and the directive class.
	imports: [Gauge, Percent01Pipe, HighlightDelta],
	template: `
		<section class="metrics-panel">
			<h1>Metrics</h1>

			<div class="controls">
				<button type="button" (click)="bump(-10)">-10</button>
				<strong HighlightDelta [delta]="step()">{{ value() }}</strong>
				<button type="button" (click)="bump(10)">+10</button>
				<span class="load">load: {{ load() | percent01 }}</span>
			</div>

			<Gauge [min]="0" [max]="100" [value]="value()" />
		</section>
	`,
})
export default class MetricsPanelComponent {
	readonly value = signal(0)
	readonly step = signal(0)

	/** A 0..1 ratio fed straight to the pipe in this template. */
	readonly load = computed(() => this.value() / 100)

	bump(by: number): void {
		this.step.set(by)
		this.value.update((v) => Math.min(100, Math.max(0, v + by)))
	}
}
