/**
 * @module
 *
 * Typed bridge to the Rust authoring compiler, exposed to Node via the
 * `@treaty/authoring-node` NAPI addon. Every lowering in this package routes
 * through this single seam; the compiler is never reimplemented in TypeScript.
 *
 * The addon's entry points map onto the file kinds Treaty owns:
 *   - `compile` (unified)      -> `.treaty` single-file components (server-block
 *     aware) as well as the `.tsx`/`.tjsx`/`.ts` extensions below
 *   - `compileComponentSource` -> `.ts` `@Component` and legacy authoring
 *   - `compileComponent`       -> template/selector/className triples
 *   - `compile`                -> the *unified* authoring front-end: full source
 *     text for any owned extension (`.tsx`/`.tjsx`/`.ts`), including BARE JSX
 *     (`export default` function/arrow returning JSX with no `@Component`)
 *   - `compileMany`            -> the same unified front-end over a batch of
 *     files, lowered in parallel inside the Rust addon (rayon)
 *
 * Every entry point returns Ivy JS plus a list of compiler errors; the unified
 * paths additionally surface an optional extracted server module.
 */

import {
	compile as compileUnified,
	compileWithRegistry as compileUnifiedWithRegistry,
	compileComponent,
	compileComponentSource,
	compileMany as compileManyNative,
	compileManyWithRegistry as compileManyWithRegistryNative,
	buildSelectorRegistry as buildSelectorRegistryNative,
	buildImportedSelectors as buildImportedSelectorsNative,
} from '@treaty/authoring-node'

/** Result of a single compilation: emitted Ivy JS plus any compiler errors. */
export interface CompiledComponent {
	readonly code: string
	readonly errors: readonly string[]
	/**
	 * The additive Source Map v3 JSON mapping `code` back to the original source,
	 * or `undefined` when the routed front-end produced no map. CLIENT PRIVACY:
	 * when the source declared a `server { … }` block, every lifted server-fn body
	 * has already been redacted from the map's `sourcesContent` by the Rust addon
	 * before it reaches this seam.
	 */
	readonly map?: string
}

/**
 * Result of a unified {@link compileUnifiedSource} compilation. Adds the
 * optional extracted server module to {@link CompiledComponent}: when the source
 * declares server functions, the Rust front-end splits their bodies into a
 * separate module emitted here.
 */
export interface CompiledAuthoring extends CompiledComponent {
	/** Extracted server-side module source, when the file declared server fns. */
	readonly serverModule?: string
}

/** One input file for {@link compileMany}: its id (path/url) and source text. */
export interface AuthoringFile {
	readonly id: string
	readonly code: string
}

/**
 * The project-wide `className -> selector` map: every first-party
 * `@Component`/`@Directive` class's real `selector`, keyed by class name. Built
 * ONCE per build by {@link buildSelectorRegistry} (the Rust scanner) and reused to
 * derive each file's {@link ImportedSelectorMap}. Crosses the NAPI boundary as a
 * plain object.
 */
export type ProjectSelectors = Record<string, string>

/**
 * A per-file `{ localImportName -> selector }` map: for the file being compiled,
 * each imported `@Component`/`@Directive`'s LOCAL binding name resolved to its real
 * selector. The compiler consumes this (instead of its class-name↔tag fold) so an
 * IMPORTED child used by its conventional selector (`<app-stat-card>` for
 * `class StatCard`) resolves as a real dependency. `undefined` ⇒ no cross-module
 * selectors for this file ⇒ the byte-identical fold path.
 */
export type ImportedSelectorMap = Record<string, string>

/**
 * Scan every first-party `.ts` source under `rootDir` and return the project-wide
 * `className -> selector` map. The bundler-plugin COLD-BUILD PREWARM entry: a
 * plugin calls this once in `buildStart` and holds the result for the build, then
 * derives each file's {@link ImportedSelectorMap} via {@link buildImportedSelectors}.
 * Drives the SAME Rust scanner the native `treaty build` uses, so cross-module
 * selectors resolve identically. `node_modules`/dot-dirs are skipped; a missing
 * root yields an empty map.
 */
export function buildSelectorRegistry(rootDir: string): ProjectSelectors {
	return buildSelectorRegistryNative(rootDir)
}

/**
 * Build the per-file {@link ImportedSelectorMap} for `source` from a project-wide
 * map produced by {@link buildSelectorRegistry}. Returns `undefined` when no import
 * resolves to a known selector (the file then folds byte-identically). Type-only
 * imports are excluded.
 */
export function buildImportedSelectors(
	source: string,
	projectSelectors: ProjectSelectors
): ImportedSelectorMap | undefined {
	return buildImportedSelectorsNative(source, projectSelectors) ?? undefined
}

/** One result from {@link compileMany}: the file id plus its compiled output. */
export interface CompiledAuthoringEntry extends CompiledAuthoring {
	/** The id of the input file this entry was produced from. */
	readonly id: string
}

/**
 * Compile a `.treaty` single-file component by name.
 *
 * Routes through the *unified* authoring front-end ({@link compileUnifiedSource})
 * rather than the raw `compileTreatyFile` entry. The unified path is server-block
 * aware: it lifts a top-level `server { … }` block out of the `.treaty` source
 * before lowering, so the emitted client module never carries a raw `server { … }`
 * statement (nor leaks the body's `import` declarations into the synthesized
 * component function — which is illegal JS and made esbuild fail with
 * `Unexpected "{"`), and the extracted backend module is surfaced as
 * {@link CompiledAuthoring.serverModule}. Behaviour for a `.treaty` file WITHOUT a
 * server block is identical to the raw entry (the source compiles unchanged), so
 * this is a strict superset reached by every owned-extension caller via
 * {@link compileUnifiedSource}.
 */
export function compileTreaty(source: string, fileName: string): CompiledAuthoring {
	return compileUnified(source, fileName)
}

/**
 * Compile a component from full source text via the legacy `@Component`-only
 * path. Retained for callers that specifically need the component-source entry
 * point; new code should prefer {@link compileUnifiedSource}, which also accepts
 * bare JSX.
 */
export function compileSource(source: string): CompiledComponent {
	return compileComponentSource(source)
}

/**
 * Compile full source text through the unified authoring front-end. Unlike
 * {@link compileSource}, this accepts BARE JSX (an `export default` function or
 * arrow returning JSX with no `@Component` decorator) in addition to
 * `@Component` classes, lowering either to Ivy JS.
 *
 * `importedSelectors` is the OPTIONAL per-file cross-module selector registry
 * ({@link ImportedSelectorMap}): when present (and non-empty) the source is lowered
 * through the registry-aware Rust entry so an IMPORTED child used by its
 * conventional `@Component` selector resolves as a real dependency. ADDITIVE
 * GUARANTEE: when it is `undefined`/empty, the output is byte-for-byte identical to
 * the no-registry path, so existing callers are unaffected.
 *
 * @param source            Full module source text.
 * @param fileName          File id used for diagnostics and extension-aware handling.
 * @param importedSelectors Optional per-file `{ importName -> selector }` registry.
 */
export function compileUnifiedSource(
	source: string,
	fileName: string,
	importedSelectors?: ImportedSelectorMap
): CompiledAuthoring {
	// Empty map ⇒ no cross-module selectors ⇒ take the fold path (byte-identical to `compile`).
	if (importedSelectors === undefined || isEmptyRecord(importedSelectors)) {
		return compileUnified(source, fileName)
	}
	return compileUnifiedWithRegistry(source, fileName, importedSelectors)
}

/** Whether a record has no own enumerable keys (treated as "no registry"). */
function isEmptyRecord(record: Record<string, string>): boolean {
	for (const _ in record) return false
	return true
}

/**
 * Compile a batch of authoring files through the unified front-end in one call.
 * The Rust addon lowers the files in parallel (rayon); results are returned in
 * the same order as the inputs, each tagged with its source id.
 *
 * `registries` (when given) is matched POSITIONALLY to `files` — entry `i` is the
 * per-file {@link ImportedSelectorMap} for `files[i]`, or `undefined`/`null` for a
 * file with no cross-module selectors. A batch with no registries (or all empty) is
 * byte-for-byte identical to the no-registry batch (ADDITIVE GUARANTEE).
 */
export function compileMany(
	files: readonly AuthoringFile[],
	registries?: readonly (ImportedSelectorMap | undefined)[]
): CompiledAuthoringEntry[] {
	if (registries === undefined || !registries.some((r) => r !== undefined && !isEmptyRecord(r))) {
		// No real per-file registry ⇒ the byte-identical parallel fold path.
		return compileManyNative(files as AuthoringFile[])
	}
	const normalized = files.map((_, i) => {
		const reg = registries[i]
		return reg !== undefined && !isEmptyRecord(reg) ? reg : null
	})
	return compileManyWithRegistryNative(files as AuthoringFile[], normalized)
}

/** Compile a component from a template, selector, and class name. */
export function compileTemplate(
	template: string,
	selector: string,
	className: string
): CompiledComponent {
	return compileComponent(template, selector, className)
}
