/**
 * Plugin-output viewer.
 *
 * A self-contained, standalone Angular component for the REPL that shows what
 * the Treaty bundler plugins actually emit. It drives the framework-agnostic
 * `@treaty/compiler` core directly -- the same `transform` / `transformMany`
 * seam every bundler plugin (`@treaty/vite`, `@treaty/rspack`, ...) builds on --
 * and renders, per input file:
 *
 *   - the emitted Ivy JavaScript (`code`)
 *   - the extracted server module, when the file declared server fns
 *   - the tree-shaking hint (`sideEffects`)
 *
 * It is intentionally decoupled from the editor: pass it sources and it lowers
 * them. This makes it a faithful preview of plugin output for the
 * everything-app showcase without re-implementing any lowering.
 */
import { Component, signal, type WritableSignal } from '@angular/core'
import {
	createTreatyCompiler,
	TreatyCompileError,
	type TransformInput,
	type TransformResult,
} from '@treaty/compiler'

/** One rendered row: the input id plus either its result or an error. */
interface OutputRow {
	readonly id: string
	readonly code: string
	readonly serverModule?: string
	readonly sideEffects: boolean
	readonly error?: string
	readonly owned: boolean
}

@Component({
	// Selectorless -- the Treaty compiler synthesizes the selector.
	template: `
		<section class="plugin-output">
			<h2>Treaty plugin output</h2>
			@if (!rows().length) {
				<p class="hint">Call compile() with authoring sources to preview emitted Ivy.</p>
			}
			@for (row of rows(); track row.id) {
				<article class="row" [class.error]="!!row.error" [class.skipped]="!row.owned">
					<header>
						<code class="id">{{ row.id }}</code>
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
						<pre class="code">{{ row.code }}</pre>
						@if (row.serverModule) {
							<h3>server module</h3>
							<pre class="server">{{ row.serverModule }}</pre>
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
			.badge.muted {
				color: #888;
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
	/** Rendered rows, one per compiled input. */
	readonly rows: WritableSignal<OutputRow[]> = signal<OutputRow[]>([])

	// One compiler instance with the incremental cache enabled, exactly like a
	// bundler plugin would hold.
	private readonly compiler = createTreatyCompiler({ cache: true })

	/**
	 * Compile a batch of authoring files through the core's `transformMany`
	 * (the cold-build path) and render each result. Files the compiler does not
	 * own yield a `null` result -- shown as "passed through".
	 */
	compileMany(files: readonly TransformInput[]): void {
		const results = this.compiler.transformMany(files)
		this.rows.set(files.map((file, index) => this.toRow(file, results[index] ?? null)))
	}

	/**
	 * Compile a single authoring file via the per-file `transform` (the
	 * incremental dev path) and append it to the rendered rows.
	 */
	compileOne(id: string, code: string): void {
		let result: TransformResult | null
		try {
			result = this.compiler.transform(id, code)
		} catch (error) {
			this.rows.update((current) => [...current, this.errorRow(id, error)])
			return
		}
		this.rows.update((current) => [...current, this.toRow({ id, code }, result)])
	}

	/** Clear all rendered rows. */
	clear(): void {
		this.rows.set([])
	}

	private toRow(file: TransformInput, result: TransformResult | null): OutputRow {
		if (result === null) {
			return { id: file.id, code: '', sideEffects: true, owned: false }
		}
		return {
			id: file.id,
			code: result.code,
			serverModule: result.serverModule,
			sideEffects: result.sideEffects,
			owned: true,
		}
	}

	private errorRow(id: string, error: unknown): OutputRow {
		const message =
			error instanceof TreatyCompileError
				? error.message
				: error instanceof Error
					? error.message
					: String(error)
		return { id, code: '', sideEffects: true, owned: true, error: message }
	}
}
