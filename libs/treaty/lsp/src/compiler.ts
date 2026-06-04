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

import {
	buildImportedSelectors,
	buildSelectorRegistry,
	compileComponent,
	compileComponentSource,
	compileTreatyFile,
	compileWithRegistry,
	type ImportedSelectorMap,
	type ProjectSelectors,
} from '@treaty/authoring-node'

/** Result of a single compilation: emitted code plus any compiler errors. */
export interface CompiledComponent {
	readonly code: string
	readonly errors: readonly string[]
}

export type { ImportedSelectorMap, ProjectSelectors } from '@treaty/authoring-node'

/** Compile a `.treaty` source file by name. */
export function compileTreaty(source: string, fileName: string): CompiledComponent {
	return compileTreatyFile(source, fileName)
}

/** Compile a component from its full source text. */
export function compileSource(source: string): CompiledComponent {
	return compileComponentSource(source)
}

/** Compile a component from a template, selector, and class name. */
export function compileTemplate(
	template: string,
	selector: string,
	className: string
): CompiledComponent {
	return compileComponent(template, selector, className)
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
	const result = compileWithRegistry(source, fileName, importedSelectors ?? undefined)
	return { code: result.code, errors: result.errors }
}

/**
 * Scan every first-party `.ts` source under `rootDir` for `@Component` /
 * `@Directive` selectors, returning the project-wide `className → selector` map.
 * The language server holds this for the workspace and refreshes it lazily; it
 * is the same scan a `@treaty/vite` cold build runs in `buildStart`.
 */
export function scanProjectSelectors(rootDir: string): ProjectSelectors {
	return buildSelectorRegistry(rootDir)
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
	return buildImportedSelectors(source, projectSelectors) ?? undefined
}
