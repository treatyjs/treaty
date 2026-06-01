/**
 * @module
 *
 * `treaty generate <app|lib|component> <name>` — scaffold standalone,
 * federation-ready building blocks for a Treaty project. Everything generated is
 * standalone (no NgModule) and federation-ready by default, consistent with the
 * principle that every Treaty app is a Module Federation host with zero config.
 *
 * The generator is pure filesystem work — no bundler peer is involved — so it
 * runs with nothing installed but the CLI itself.
 *
 *   - `app`       → a runnable standalone app: `treaty.config`, `index.html`,
 *                   `src/main.ts` bootstrapping a root standalone component.
 *   - `lib`       → a sharable library with a public `src/index.ts` barrel and a
 *                   sample standalone component to expose as a remote.
 *   - `component` → a single standalone component file under `src/app`.
 */

import { mkdir, writeFile } from 'node:fs/promises'
import { existsSync } from 'node:fs'
import { dirname, join, resolve as resolvePath } from 'node:path'

/** The kinds of artifact {@link runGenerate} can scaffold. */
export type GenerateKind = 'app' | 'lib' | 'component'

/** Options controlling a scaffold run. */
export interface GenerateOptions {
	/** The artifact kind. */
	readonly kind: GenerateKind
	/** The artifact name (kebab-case recommended; used for paths + class names). */
	readonly name: string
	/** The directory the artifact is written under. Defaults to the cwd. */
	readonly cwd: string
	/** When `true`, plan the files but do not write them (used by tests/dry-runs). */
	readonly dryRun?: boolean
	/** When `true`, overwrite existing files instead of skipping them. */
	readonly force?: boolean
}

/** A single file the generator plans to (or did) write. */
export interface GeneratedFile {
	/** Absolute path of the file. */
	readonly path: string
	/** The file's full contents. */
	readonly contents: string
}

/** The outcome of a scaffold run. */
export interface GenerateResult {
	readonly kind: GenerateKind
	readonly name: string
	/** Every file produced by the plan (whether or not it was written). */
	readonly files: readonly GeneratedFile[]
	/** Paths actually written to disk (empty for a dry run). */
	readonly written: readonly string[]
	/** Paths skipped because they already existed (and `force` was off). */
	readonly skipped: readonly string[]
}

/** Convert a kebab/snake/space name to a PascalCase identifier base. */
function toPascalCase(name: string): string {
	return name
		.split(/[^a-zA-Z0-9]+/)
		.filter(Boolean)
		.map((part) => part.charAt(0).toUpperCase() + part.slice(1))
		.join('')
}

/** Convert a name to a kebab-case selector/path-safe token. */
function toKebabCase(name: string): string {
	return name
		.replace(/([a-z0-9])([A-Z])/g, '$1-$2')
		.replace(/[^a-zA-Z0-9]+/g, '-')
		.toLowerCase()
		.replace(/^-+|-+$/g, '')
}

/** Render a standalone component source file body. */
function componentSource(name: string): string {
	const className = `${toPascalCase(name)}Component`
	const kebab = toKebabCase(name)
	// MINIMAL TEMPLATE: Treaty is selectorless, standalone, and signal by default —
	// the compiler fills the selector, `standalone`, and signal wrapping in during
	// compilation. A scaffolded component therefore carries none of that ceremony;
	// it is still federation-ready (exposable as a Module Federation remote) with no
	// extra configuration.
	return `import { Component } from '@angular/core'

/**
 * ${className} — a standalone, federation-ready Treaty component. The compiler
 * fills in the selector and standalone metadata, so none is written here.
 */
@Component({
	template: \`<p>${kebab} works</p>\`,
})
export class ${className} {}
`
}

/** Render the root standalone bootstrap for a generated app. */
function mainSource(name: string): string {
	const className = `${toPascalCase(name)}Component`
	return `import { bootstrapApplication } from '@angular/platform-browser'
import { ${className} } from './app/${toKebabCase(name)}.component'

// Standalone bootstrap — no NgModule. The compiled app is automatically a
// Module Federation host (Treaty wires this; you configure nothing).
void bootstrapApplication(${className})
`
}

/** Render the `index.html` shell for a generated app. */
function indexHtmlSource(name: string): string {
	const selector = `app-${toKebabCase(name)}`
	return `<!doctype html>
<html lang="en">
	<head>
		<meta charset="utf-8" />
		<title>${name}</title>
		<meta name="viewport" content="width=device-width, initial-scale=1" />
	</head>
	<body>
		<${selector}></${selector}>
		<script type="module" src="/src/main.ts"></script>
	</body>
</html>
`
}

/** Render a minimal `treaty.config.mjs` for a generated app. */
function treatyConfigSource(name: string): string {
	return `import { defineConfig } from '@treaty/cli'

// Treaty apps are convention-based: this config is optional. The app is a
// Module Federation host automatically — declare remotes/exposes here only if
// you need them.
export default defineConfig({
	moduleFederation: {
		name: '${toKebabCase(name)}',
	},
})
`
}

/** Render a library public-API barrel that re-exports the sample component. */
function libIndexSource(name: string): string {
	const className = `${toPascalCase(name)}Component`
	return `export { ${className} } from './lib/${toKebabCase(name)}.component'
`
}

/**
 * Plan the set of files for a scaffold request without touching the filesystem.
 * Exposed separately from {@link runGenerate} so callers (and the smoke test) can
 * inspect or dry-run a scaffold deterministically.
 */
export function planGenerate(options: GenerateOptions): readonly GeneratedFile[] {
	const root = resolvePath(options.cwd, toKebabCase(options.name))
	const fileName = toKebabCase(options.name)

	switch (options.kind) {
		case 'app':
			return [
				{ path: join(root, 'treaty.config.mjs'), contents: treatyConfigSource(options.name) },
				{ path: join(root, 'index.html'), contents: indexHtmlSource(options.name) },
				{ path: join(root, 'src', 'main.ts'), contents: mainSource(options.name) },
				{
					path: join(root, 'src', 'app', `${fileName}.component.ts`),
					contents: componentSource(options.name),
				},
			]
		case 'lib':
			return [
				{ path: join(root, 'src', 'index.ts'), contents: libIndexSource(options.name) },
				{
					path: join(root, 'src', 'lib', `${fileName}.component.ts`),
					contents: componentSource(options.name),
				},
			]
		case 'component':
			// A bare component is written into the *current* project's src/app.
			return [
				{
					path: resolvePath(options.cwd, 'src', 'app', `${fileName}.component.ts`),
					contents: componentSource(options.name),
				},
			]
	}
}

/**
 * Scaffold the requested artifact. Plans the files via {@link planGenerate}, then
 * writes each (creating parent directories) unless `dryRun` is set. Existing
 * files are skipped unless `force` is set, so a generate never clobbers work by
 * accident. Returns a {@link GenerateResult} describing exactly what happened.
 */
export async function runGenerate(options: GenerateOptions): Promise<GenerateResult> {
	const files = planGenerate(options)
	const written: string[] = []
	const skipped: string[] = []

	if (!options.dryRun) {
		for (const file of files) {
			if (existsSync(file.path) && !options.force) {
				skipped.push(file.path)
				continue
			}
			await mkdir(dirname(file.path), { recursive: true })
			await writeFile(file.path, file.contents, 'utf8')
			written.push(file.path)
		}
	}

	return { kind: options.kind, name: options.name, files, written, skipped }
}

/** Validate that a raw token is a supported {@link GenerateKind}. */
export function isGenerateKind(value: string | undefined): value is GenerateKind {
	return value === 'app' || value === 'lib' || value === 'component'
}
