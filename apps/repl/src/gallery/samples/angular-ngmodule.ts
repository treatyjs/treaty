// Angular @NgModule (.ts) — non-standalone module with declarations, imports, and exports.
// Demonstrates idiomatic NgModule pattern exercising ɵɵdefineNgModule + ɵinj Ivy definitions.
import { NgModule } from '@angular/core'
import { CommonModule } from '@angular/common'
import { Component } from '@angular/core'

/** A simple component scoped to this module. */
@Component({
	selector: 'app-widget',
	template: `
		<div class="widget">
			<h3>Module Widget</h3>
			<p>This component is declared and exported by WidgetModule.</p>
		</div>
	`,
	styles: [`
		.widget {
			padding: 1rem;
			border: 1px solid #ccc;
			border-radius: 4px;
			font-family: system-ui;
		}
	`],
})
export class WidgetComponent {}

/** A non-standalone module that declares a component, imports CommonModule, and exports it. */
@NgModule({
	declarations: [WidgetComponent],
	imports: [CommonModule],
	exports: [WidgetComponent],
})
export class WidgetModule {}
