/**
 * Plugin-output viewer.
 *
 * A self-contained, standalone Angular component for the REPL that shows what
 * each Treaty authoring plugin actually emits. It drives the framework-agnostic
 * `@treaty/compiler` core directly -- the same `transform` / `transformMany`
 * seam every bundler plugin (`@treaty/vite`, `@treaty/rspack`, ...) builds on --
 * and renders, per input file:
 *
 *   - which authoring plugin owns the file (`.treaty` SFC, JSX, or Angular
 *     `@Component` `.ts`), via the core's own `classify`
 *   - the emitted Ivy JavaScript (`code`)
 *   - the extracted server module, when the file declared server fns, together
 *     with each server fn decomposed into its own loadable chunk
 *   - the tree-shaking hint (`sideEffects`)
 *
 * The viewer ships a built-in sample per authoring surface so it demonstrates
 * EVERY plugin compiling to Ivy out of the box, and exposes a selector to switch
 * which surface's output is shown. It stays decoupled from the editor: pass it
 * your own sources via `compileOne` / `compileMany` and it lowers them the same
 * way, without re-implementing any lowering.
 */
import { Component, computed, signal, type Signal, type WritableSignal } from '@angular/core'
import {
	classify,
	createTreatyCompiler,
	TreatyCompileError,
	type ServerFnChunk,
	type TransformInput,
	type TransformResult,
	type TreatyFileKind,
} from '@treaty/compiler'

/** Human-facing label for each authoring plugin Treaty owns. */
const PLUGIN_LABEL: Record<TreatyFileKind, string> = {
	treaty: '.treaty SFC',
	jsx: 'JSX (.tjsx)',
	component: 'Angular @Component (.ts)',
}

/** One rendered row: the input id plus either its result or an error. */
interface OutputRow {
	readonly id: string
	/** Owning authoring plugin, or `null` when the compiler passes the file through. */
	readonly kind: TreatyFileKind | null
	/** Human-facing plugin label for {@link kind}. */
	readonly plugin: string
	readonly code: string
	readonly serverModule?: string
	readonly serverChunks?: readonly ServerFnChunk[]
	readonly sideEffects: boolean
	readonly error?: string
	readonly owned: boolean
}

/**
 * A built-in showcase input: the source plus the bundler id whose extension
 * routes it to a specific authoring plugin. Selecting one compiles it live.
 */
interface SampleInput extends TransformInput {
	/** Short caption shown in the plugin selector. */
	readonly label: string
}

/**
 * One sample per authoring surface so the viewer demonstrates each plugin
 * lowering to Ivy with no editor input. The Angular `.ts` sample declares a
 * server fn so the extracted server module / per-fn chunks are exercised too.
 *
 * Defaults (standalone, signal, selectorless) are intentionally omitted from
 * the scaffolds -- the compiler fills them in.
 */
const SAMPLES: readonly SampleInput[] = [
	{
		label: 'Counter.treaty',
		id: 'Counter.treaty',
		// No <template>: HTML tags interleave freely with TS; <style lang> block.
		code: [
			'<style lang="scss">',
			'  .count { font-weight: 600; }',
			'</style>',
			'',
			"import { signal } from '@angular/core'",
			'const count = signal(0)',
			'const inc = () => count.set(count() + 1)',
			'',
			'<button (click)="inc()">+</button>',
			'<span class="count">{{ count() }}</span>',
		].join('\n'),
	},
	{
		label: 'Greeting.tjsx',
		id: 'Greeting.tjsx',
		// Lowercase class, Angular control-flow @if, signals-by-default.
		code: [
			"import { input } from '@angular/core'",
			'',
			'export default function Greeting() {',
			"  const name = input('world')",
			'  return (',
			'    <p class="greeting">',
			'      @if (name()) { Hello, {name()}! } @else { Hello! }',
			'    </p>',
			'  )',
			'}',
		].join('\n'),
	},
	{
		label: 'SaveNote (Angular .ts, server fn)',
		id: 'save-note.component.ts',
		// Classic @Component, with a server fn extracted to the backend (axum).
		code: [
			"import { Component, signal } from '@angular/core'",
			'',
			'export async function saveNote(text: string) {',
			"  'use server'",
			'  return { saved: text.length }',
			'}',
			'',
			'@Component({',
			"  selector: 'save-note',",
			"  template: `<button (click)=\"save()\">Save</button>`,",
			'})',
			'export class SaveNoteComponent {',
			"  readonly draft = signal('')",
			'  async save() { await saveNote(this.draft()) }',
			'}',
		].join('\n'),
	},
]

@Component({
	// Explicit selector so the component matches whether it is reached through the
	// `.treaty` shell (which lowers a `<PluginOutputViewer>` tag to `plugin-output-viewer`)
	// or any other Ivy host that declares it as a dependency.
	selector: 'plugin-output-viewer',
	template: `
		<section class="plugin-output">
			<h2>Treaty plugin output</h2>

			<nav class="plugins" aria-label="authoring plugins">
				@for (sample of samples; track sample.id) {
					<button
						type="button"
						class="plugin-tab"
						[class.active]="selected() === sample.id"
						(click)="select(sample.id)"
					>
						{{ pluginLabel(sample.id) }}
						<small>{{ sample.label }}</small>
					</button>
				}
				<button type="button" class="plugin-tab all" [class.active]="!selected()" (click)="showAll()">
					All plugins
				</button>
			</nav>

			@if (!visibleRows().length) {
				<p class="hint">Select a plugin above, or call compile() with your own authoring sources.</p>
			}
			@for (row of visibleRows(); track row.id) {
				<article class="row" [class.error]="!!row.error" [class.skipped]="!row.owned">
					<header>
						<code class="id">{{ row.id }}</code>
						<span class="badge plugin">{{ row.plugin }}</span>
						@if (row.owned && !row.error) {
							<span class="badge">sideEffects: {{ row.sideEffects }}</span>
						}
						@if (!row.owned) {
							<span class="badge muted">not owned (passed through)</span>
						}
					</header>

					@if (row.error) {
						<pre class="diag">{{ row.error }}</pre>
					} @else if (row.owned) {
						<h3>emitted Ivy</h3>
						<pre class="code">{{ row.code }}</pre>
						@if (row.serverModule) {
							<h3>server module</h3>
							<pre class="server">{{ row.serverModule }}</pre>
						}
						@if (row.serverChunks?.length) {
							<h3>server fn chunks</h3>
							@for (chunk of row.serverChunks; track chunk.id) {
								<details class="chunk">
									<summary>
										<code>{{ chunk.exportName }}</code>
										<span class="badge muted">{{ chunk.id }}</span>
									</summary>
									<h4>client binding</h4>
									<pre class="binding">{{ chunk.clientBinding }}</pre>
									<h4>server chunk</h4>
									<pre class="server">{{ chunk.code }}</pre>
								</details>
							}
						}
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
			.row.skipped {
				opacity: 0.6;
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
			.badge.muted {
				color: #888;
			}
			h3 {
				font-size: 13px;
				margin: 0.6rem 0 0.2rem;
				color: #9da5b4;
			}
			h4 {
				font-size: 12px;
				margin: 0.4rem 0 0.2rem;
				color: #9da5b4;
			}
			.chunk {
				border: 1px solid #333;
				border-radius: 4px;
				padding: 0.3rem 0.5rem;
				margin-bottom: 0.4rem;
			}
			.chunk summary {
				cursor: pointer;
				display: flex;
				gap: 0.5rem;
				align-items: center;
			}
			pre {
				overflow-x: auto;
				background: #161b22;
				padding: 0.5rem;
				border-radius: 4px;
				white-space: pre;
			}
			pre.diag {
				color: #ff8a80;
			}
		`,
	],
})
export class PluginOutputViewer {
	/** Built-in samples, one per authoring surface, exposed to the template. */
	readonly samples: readonly SampleInput[] = SAMPLES

	/** Rendered rows, one per compiled input. */
	readonly rows: WritableSignal<OutputRow[]> = signal<OutputRow[]>([])

	/** Currently selected sample id, or `null` to show every compiled row. */
	readonly selected: WritableSignal<string | null> = signal<string | null>(null)

	/** Rows filtered to the current selection (all rows when nothing selected). */
	readonly visibleRows: Signal<OutputRow[]> = computed(() => {
		const id = this.selected()
		const all = this.rows()
		return id === null ? all : all.filter((row) => row.id === id)
	})

	// One compiler instance with the incremental cache enabled, exactly like a
	// bundler plugin would hold.
	private readonly compiler = createTreatyCompiler({ cache: true })

	constructor() {
		// Compile every showcase sample up front so each authoring plugin's Ivy
		// output is available the moment the viewer mounts.
		this.compileMany(this.samples)
	}

	/** Friendly plugin label for a sample id, via the compiler's own routing. */
	pluginLabel(id: string): string {
		const kind = classify(id)
		return kind === null ? 'not owned' : PLUGIN_LABEL[kind]
	}

	/** Show only the row for `id` (the sample bound to a plugin tab). */
	select(id: string): void {
		this.selected.set(id)
	}

	/** Clear the selection and show every compiled row. */
	showAll(): void {
		this.selected.set(null)
	}

	/**
	 * Compile a batch of authoring files through the core's `transformMany`
	 * (the cold-build path) and render each result. Files the compiler does not
	 * own yield a `null` result -- shown as "passed through". A compile diagnostic
	 * surfaces as an error row rather than throwing out of the batch.
	 */
	compileMany(files: readonly TransformInput[]): void {
		this.rows.set(files.map((file) => this.compileRow(file)))
	}

	/**
	 * Compile a single authoring file via the per-file `transform` (the
	 * incremental dev path) and append it to the rendered rows.
	 */
	compileOne(id: string, code: string): void {
		this.rows.update((current) => [...current, this.compileRow({ id, code })])
	}

	/** Clear all rendered rows. */
	clear(): void {
		this.rows.set([])
	}

	/**
	 * Lower one input and shape it into a row. `transform` returns `null` for
	 * files this compiler does not own and throws {@link TreatyCompileError} on a
	 * diagnostic; both are captured here so a single bad input never breaks the
	 * surrounding batch render.
	 */
	private compileRow(file: TransformInput): OutputRow {
		try {
			const result = this.compiler.transform(file.id, file.code)
			return this.toRow(file, result)
		} catch (error) {
			return this.errorRow(file.id, error)
		}
	}

	private toRow(file: TransformInput, result: TransformResult | null): OutputRow {
		const kind = classify(file.id)
		const plugin = kind === null ? 'not owned' : PLUGIN_LABEL[kind]
		if (result === null) {
			return { id: file.id, kind, plugin, code: '', sideEffects: true, owned: false }
		}
		return {
			id: file.id,
			kind,
			plugin,
			code: result.code,
			serverModule: result.serverModule,
			serverChunks: result.serverChunks,
			sideEffects: result.sideEffects,
			owned: true,
		}
	}

	private errorRow(id: string, error: unknown): OutputRow {
		const kind = classify(id)
		const plugin = kind === null ? 'not owned' : PLUGIN_LABEL[kind]
		const message =
			error instanceof TreatyCompileError
				? error.message
				: error instanceof Error
					? error.message
					: String(error)
		return { id, kind, plugin, code: '', sideEffects: true, owned: true, error: message }
	}
}
