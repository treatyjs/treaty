/**
 * `virtual:treaty-gallery` — a build-time Ivy gallery.
 *
 * The REPL runs in the browser, which cannot load the native `authoring_node`
 * NAPI addon. So instead of compiling samples at runtime in the browser (which
 * pulled the node-only `@treaty/compiler` into the browser bundle and broke the
 * build), this Vite plugin compiles one sample PER AUTHORING SURFACE at BUILD
 * time — in Node, through the REAL Rust addon — and emits the results as a plain
 * data module. The browser imports data, not a native dependency.
 *
 * Each row carries the original `source` and the emitted Ivy `code`, so the
 * gallery shows, side by side, how every authoring surface Treaty supports —
 * normal Angular `@Component`/`@Directive`/`@Injectable`/`@Pipe`/`@NgModule`,
 * `.treaty` SFCs, Treaty JSX, and plain React `.tsx` — lowers to Ivy.
 */
import { readFileSync, readdirSync } from 'node:fs'
import { join, extname } from 'node:path'
import type { Plugin } from 'vite'
import { loadRustCompiler } from './treaty-sfc/rust-compiler-loader'

const VIRTUAL_ID = 'virtual:treaty-gallery'
const RESOLVED_ID = '\0' + VIRTUAL_ID

/** A precompiled gallery row (matches the viewer's render model + a `source`). */
export interface GalleryRow {
	readonly id: string
	readonly label: string
	readonly kind: 'treaty' | 'jsx' | 'component'
	readonly plugin: string
	readonly source: string
	readonly code: string
	readonly serverModule?: string
	readonly sideEffects: boolean
	readonly error?: string
	readonly owned: boolean
}

/** Authoring plugin that owns a file id, by extension. */
function kindFor(file: string): GalleryRow['kind'] {
	const ext = extname(file).toLowerCase()
	if (ext === '.treaty') return 'treaty'
	if (ext === '.tsx' || ext === '.tjsx' || ext === '.jsx') return 'jsx'
	return 'component'
}

const PLUGIN_LABEL: Record<GalleryRow['kind'], string> = {
	treaty: '.treaty SFC',
	jsx: 'JSX / React front-end',
	component: 'Angular @Component (.ts)',
}

/** Friendly captions for the curated sample filenames (fallback: prettified stem). */
const LABELS: Record<string, string> = {
	'angular-component.ts': 'Angular @Component (.ts)',
	'angular-directive.ts': 'Angular @Directive (.ts)',
	'angular-injectable.ts': 'Angular @Injectable (.ts)',
	'angular-pipe.ts': 'Angular @Pipe (.ts)',
	'angular-ngmodule.ts': 'Angular @NgModule (.ts)',
	'counter.treaty': '.treaty SFC',
	'treaty-jsx.tsx': 'Treaty JSX (.tsx)',
	'react-counter.tsx': 'React (.tsx) → Angular',
}

function labelFor(file: string): string {
	return LABELS[file] ?? file.replace(/\.[^.]+$/, '').replace(/[-_]/g, ' ')
}

/** Order the curated surfaces so normal Angular leads, then Treaty, then React. */
const ORDER = [
	'angular-component.ts',
	'angular-directive.ts',
	'angular-injectable.ts',
	'angular-pipe.ts',
	'angular-ngmodule.ts',
	'counter.treaty',
	'treaty-jsx.tsx',
	'react-counter.tsx',
]
function orderKey(file: string): number {
	const i = ORDER.indexOf(file)
	return i === -1 ? ORDER.length + file.charCodeAt(0) : i
}

/**
 * Vite plugin exposing `virtual:treaty-gallery` — the precompiled rows.
 * @param samplesDir absolute path to the directory of sample sources.
 */
export function treatyGallery(samplesDir: string): Plugin {
	return {
		name: 'treaty-gallery',
		resolveId(id) {
			if (id === VIRTUAL_ID) return RESOLVED_ID
			return null
		},
		load(id) {
			if (id !== RESOLVED_ID) return null

			const rust = loadRustCompiler()
			// The unified `compile(code, fileName)` routes by the id's extension and
			// returns { code, serverModule?, errors }. It is present on the addon even
			// though the loader's narrow interface does not type it.
			const compile = (rust.addon as unknown as {
				compile?: (source: string, fileName: string) => {
					code: string
					serverModule?: string
					errors: string[]
				}
			} | null)?.compile

			const files = readdirSync(samplesDir)
				.filter((f) => !f.startsWith('.'))
				.sort((a, b) => orderKey(a) - orderKey(b))

			const rows: GalleryRow[] = files.map((file) => {
				const source = readFileSync(join(samplesDir, file), 'utf8')
				const kind = kindFor(file)
				const base: Omit<GalleryRow, 'code' | 'error'> = {
					id: file,
					label: labelFor(file),
					kind,
					plugin: PLUGIN_LABEL[kind],
					source,
					serverModule: undefined,
					sideEffects: true,
					owned: true,
				}
				if (!compile) {
					return {
						...base,
						code: '',
						error: `addon not loaded: ${rust.error ?? 'unknown'}`,
					}
				}
				try {
					const r = compile(source, file)
					return {
						...base,
						code: r.code,
						serverModule: r.serverModule || undefined,
						error: r.errors && r.errors.length ? r.errors.join('; ') : undefined,
					}
				} catch (e) {
					return { ...base, code: '', error: e instanceof Error ? e.message : String(e) }
				}
			})

			return `export default ${JSON.stringify(rows)}\n`
		},
	}
}
