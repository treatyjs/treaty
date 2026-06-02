/**
 * Plugin-output viewer — the Treaty Ivy gallery.
 *
 * Shows, for EVERY authoring surface Treaty supports, the authoring source and
 * the Ivy JavaScript it compiles to, side by side:
 *
 *   - normal Angular `@Component` / `@Directive` / `@Injectable` / `@Pipe` /
 *     `@NgModule` (`.ts`)
 *   - `.treaty` single-file components
 *   - Treaty JSX (`.tsx`)
 *   - plain React (`.tsx`) lowered to Angular Ivy
 *
 * The REPL runs in the browser, which cannot load the native `authoring_node`
 * addon. So the compilation happens at BUILD time, in Node, through the REAL
 * Rust compiler, exposed as the `virtual:treaty-gallery` data module (see
 * `apps/repl/src/tools/gallery-plugin.ts`). This component just renders that
 * precompiled data — it imports no compiler, native or otherwise.
 */
import { Component, computed, signal, type Signal, type WritableSignal } from '@angular/core'
import gallery, { type GalleryRow } from 'virtual:treaty-gallery'

@Component({
	// Explicit selector so the component matches whether it is reached through the
	// `.treaty` shell (which lowers a `<PluginOutputViewer>` tag to `plugin-output-viewer`)
	// or any other Ivy host that declares it as a dependency.
	selector: 'plugin-output-viewer',
	template: `
		<section class="plugin-output">
			<h2>Treaty plugin output — authoring source → Ivy</h2>

			<nav class="plugins" aria-label="authoring plugins">
				@for (sample of samples; track sample.id) {
					<button
						type="button"
						class="plugin-tab"
						[class.active]="selected() === sample.id"
						(click)="select(sample.id)"
					>
						{{ sample.plugin }}
						<small>{{ sample.label }}</small>
					</button>
				}
				<button type="button" class="plugin-tab all" [class.active]="!selected()" (click)="showAll()">
					All surfaces
				</button>
			</nav>

			@if (!visibleRows().length) {
				<p class="hint">No samples found.</p>
			}
			@for (row of visibleRows(); track row.id) {
				<article class="row" [class.error]="!!row.error">
					<header>
						<code class="id">{{ row.id }}</code>
						<span class="badge plugin">{{ row.plugin }}</span>
						<span class="badge label">{{ row.label }}</span>
					</header>

					<div class="panels">
						<div class="panel">
							<h3>authoring source</h3>
							<pre class="source">{{ row.source }}</pre>
						</div>
						<div class="panel">
							<h3>emitted Ivy</h3>
							@if (row.error) {
								<pre class="diag">{{ row.error }}</pre>
							} @else {
								<pre class="code">{{ row.code }}</pre>
							}
						</div>
					</div>

					@if (row.serverModule) {
						<h3>extracted server module</h3>
						<pre class="server">{{ row.serverModule }}</pre>
					}
				</article>
			}
		</section>
	`,
	styles: [
		`
			.plugin-output {
				font-family: ui-monospace, monospace;
				color: #d1d5da;
			}
			.plugins {
				display: flex;
				flex-wrap: wrap;
				gap: 0.5rem;
				margin-bottom: 0.75rem;
			}
			.plugin-tab {
				display: flex;
				flex-direction: column;
				align-items: flex-start;
				gap: 2px;
				background: #2c3136;
				color: #d1d5da;
				border: 1px solid #444;
				border-radius: 6px;
				padding: 0.4rem 0.6rem;
				cursor: pointer;
				font: inherit;
			}
			.plugin-tab small {
				color: #888;
				font-size: 11px;
			}
			.plugin-tab.active {
				background: #555;
				border-color: #6b6b6b;
			}
			.plugin-tab.all {
				justify-content: center;
			}
			.row {
				border: 1px solid #444;
				border-radius: 6px;
				margin-bottom: 0.75rem;
				padding: 0.5rem 0.75rem;
			}
			.row.error {
				border-color: #b00020;
			}
			.row header {
				display: flex;
				align-items: center;
				gap: 0.5rem;
				flex-wrap: wrap;
			}
			.id {
				font-weight: 600;
			}
			.badge {
				font-size: 12px;
				background: #2c3136;
				border: 1px solid #444;
				border-radius: 4px;
				padding: 0 0.4rem;
			}
			.badge.plugin {
				background: #1f3a2e;
				border-color: #2f6f4f;
			}
			.badge.label {
				color: #9da5b4;
			}
			.panels {
				display: grid;
				grid-template-columns: 1fr 1fr;
				gap: 0.75rem;
			}
			@media (max-width: 860px) {
				.panels {
					grid-template-columns: 1fr;
				}
			}
			h3 {
				font-size: 13px;
				margin: 0.6rem 0 0.2rem;
				color: #9da5b4;
			}
			pre {
				overflow-x: auto;
				background: #161b22;
				padding: 0.5rem;
				border-radius: 4px;
				white-space: pre;
				margin: 0;
			}
			pre.source {
				background: #11161c;
				color: #c8d1dc;
			}
			pre.diag {
				color: #ff8a80;
			}
		`,
	],
})
export class PluginOutputViewer {
	/** The precompiled gallery rows, one per authoring surface. */
	readonly rows: WritableSignal<readonly GalleryRow[]> = signal<readonly GalleryRow[]>(gallery)

	/** Tabs, one per sample (id + caption). */
	readonly samples: readonly GalleryRow[] = gallery

	/** Currently selected sample id, or `null` to show every row. */
	readonly selected: WritableSignal<string | null> = signal<string | null>(null)

	/** Rows filtered to the current selection (all rows when nothing selected). */
	readonly visibleRows: Signal<readonly GalleryRow[]> = computed(() => {
		const id = this.selected()
		const all = this.rows()
		return id === null ? all : all.filter((row) => row.id === id)
	})

	/** Show only the row for `id` (the sample bound to a plugin tab). */
	select(id: string): void {
		this.selected.set(id)
	}

	/** Clear the selection and show every row. */
	showAll(): void {
		this.selected.set(null)
	}
}
