/**
 * @module
 *
 * Typed bridge to the Rust authoring compiler, exposed to Node via the
 * `@treaty/authoring-node` NAPI addon. This module re-exports the addon's
 * surface with stable types so the rest of the server depends on a single,
 * well-typed seam rather than the auto-generated addon directly.
 *
 * The compiler is never reimplemented in TypeScript; diagnostics and emitted
 * code come straight from `compileComponent` / `compileComponentSource` /
 * `compileTreatyFile`.
 */

import { createRequire } from 'node:module'
import { join } from 'node:path'
import { pathToFileURL } from 'node:url'
// Types only — erased at build time, so importing them never triggers a runtime
// load of the native addon (which may be absent in a packaged extension).
import type { ImportedSelectorMap, ProjectSelectors } from '@treaty/authoring-node'

/** Result of a single compilation: emitted code plus any compiler errors. */
export interface CompiledComponent {
	readonly code: string
	readonly errors: readonly string[]
}

export type { ImportedSelectorMap, ProjectSelectors } from '@treaty/authoring-node'

/** The subset of the `@treaty/authoring-node` NAPI surface this server calls. */
interface AuthoringAddon {
	compileTreatyFile(source: string, fileName: string): CompiledComponent
	compileComponentSource(source: string): CompiledComponent
	compileComponent(template: string, selector: string, className: string): CompiledComponent
	compileWithRegistry(
		source: string,
		fileName: string,
		importedSelectors?: ImportedSelectorMap,
	): CompiledComponent
	buildSelectorRegistry(rootDir: string): ProjectSelectors
	buildImportedSelectors(
		source: string,
		projectSelectors: ProjectSelectors,
	): ImportedSelectorMap | null | undefined
}

// `undefined` = not yet attempted, `null` = attempted and unavailable.
let cachedAddon: AuthoringAddon | null | undefined
let warnedMissing = false

/**
 * Load the native authoring addon lazily, degrading gracefully when it is not
 * present (e.g. a packaged VSCode extension that did not ship a platform binary).
 * The TS-only intelligence — selectorless / signals / region-aware completion,
 * hover, go-to-definition, the whole template service — does NOT depend on the
 * addon, so the server must still start and serve those features. Only
 * compiler-backed diagnostics and the full cross-module project selector scan
 * need the addon; both no-op until it is available, instead of crashing the
 * whole language server on startup with ERR_MODULE_NOT_FOUND.
 */
function addon(): AuthoringAddon | null {
	if (cachedAddon !== undefined) {
		return cachedAddon
	}
	// Try resolving the addon from this module first (works in a dev install where
	// it is hoisted next to the server), then from the workspace root (the server's
	// cwd — a packaged extension's own dir has no addon, but the open Treaty project
	// usually does). Either success gives full features; total failure degrades.
	const bases = [import.meta.url, pathToFileURL(join(process.cwd(), 'index.js')).href]
	let lastErr: unknown
	for (const base of bases) {
		try {
			cachedAddon = createRequire(base)('@treaty/authoring-node') as AuthoringAddon
			return cachedAddon
		} catch (err) {
			lastErr = err
		}
	}
	cachedAddon = null
	if (!warnedMissing) {
		warnedMissing = true
		console.warn(
			'[treaty-lsp] native compiler addon (@treaty/authoring-node) not found — ' +
				'completion/hover/template intelligence stay enabled; compiler diagnostics ' +
				'and the full project selector scan are disabled until it is installed. ' +
				String((lastErr as { message?: string })?.message ?? lastErr),
		)
	}
	return cachedAddon
}

/** Empty compile result used when the native addon is unavailable. */
const EMPTY_COMPILE: CompiledComponent = { code: '', errors: [] }

/** Compile a `.treaty` source file by name. */
export function compileTreaty(source: string, fileName: string): CompiledComponent {
	const a = addon()
	return a ? a.compileTreatyFile(source, fileName) : EMPTY_COMPILE
}

/** Compile a component from its full source text. */
export function compileSource(source: string): CompiledComponent {
	const a = addon()
	return a ? a.compileComponentSource(source) : EMPTY_COMPILE
}

/** Compile a component from a template, selector, and class name. */
export function compileTemplate(
	template: string,
	selector: string,
	className: string
): CompiledComponent {
	const a = addon()
	return a ? a.compileComponent(template, selector, className) : EMPTY_COMPILE
}

/**
 * Compile an authoring file (`.treaty` / `.tsx` / `.tjsx` / `.ts`) WITH the
 * project's cross-module selector registry, so a selectorless tag resolves to
 * its real imported `@Component`/`@Directive` selector exactly as a bundler
 * build would. When `importedSelectors` is empty/undefined this is byte-for-byte
 * identical to the registry-free path (the Rust additive guarantee), so callers
 * can pass whatever they resolved without a separate branch.
 */
export function compileWithSelectors(
	source: string,
	fileName: string,
	importedSelectors?: ImportedSelectorMap | null
): CompiledComponent {
	const a = addon()
	if (!a) {
		return EMPTY_COMPILE
	}
	const result = a.compileWithRegistry(source, fileName, importedSelectors ?? undefined)
	return { code: result.code, errors: result.errors }
}

/**
 * Scan every first-party `.ts` source under `rootDir` for `@Component` /
 * `@Directive` selectors, returning the project-wide `className → selector` map.
 * The language server holds this for the workspace and refreshes it lazily; it
 * is the same scan a `@treaty/vite` cold build runs in `buildStart`.
 */
export function scanProjectSelectors(rootDir: string): ProjectSelectors {
	const a = addon()
	return a ? a.buildSelectorRegistry(rootDir) : ({} as ProjectSelectors)
}

/**
 * Resolve the per-file `{ localImportName → selector }` map for `source` from a
 * project-wide selector map (from {@link scanProjectSelectors}). Returns
 * `undefined` when no import resolves to a known selector.
 */
export function importedSelectorsFor(
	source: string,
	projectSelectors: ProjectSelectors
): ImportedSelectorMap | undefined {
	const a = addon()
	return (a ? a.buildImportedSelectors(source, projectSelectors) : undefined) ?? undefined
}
