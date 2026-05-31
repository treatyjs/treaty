/**
 * @module
 *
 * Typed bridge to the Rust authoring compiler, exposed to Node via the
 * `@treaty/authoring-node` NAPI addon. Every lowering in this package routes
 * through this single seam; the compiler is never reimplemented in TypeScript.
 *
 * The addon's entry points map onto the file kinds Treaty owns:
 *   - `compileTreatyFile`      -> `.treaty` single-file components
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
	compileComponent,
	compileComponentSource,
	compileMany as compileManyNative,
	compileTreatyFile,
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

/** One result from {@link compileMany}: the file id plus its compiled output. */
export interface CompiledAuthoringEntry extends CompiledAuthoring {
	/** The id of the input file this entry was produced from. */
	readonly id: string
}

/** Compile a `.treaty` single-file component by name. */
export function compileTreaty(source: string, fileName: string): CompiledComponent {
	return compileTreatyFile(source, fileName)
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
 * @param source   Full module source text.
 * @param fileName File id used for diagnostics and extension-aware handling.
 */
export function compileUnifiedSource(source: string, fileName: string): CompiledAuthoring {
	return compileUnified(source, fileName)
}

/**
 * Compile a batch of authoring files through the unified front-end in one call.
 * The Rust addon lowers the files in parallel (rayon); results are returned in
 * the same order as the inputs, each tagged with its source id.
 */
export function compileMany(files: readonly AuthoringFile[]): CompiledAuthoringEntry[] {
	return compileManyNative(files as AuthoringFile[])
}

/** Compile a component from a template, selector, and class name. */
export function compileTemplate(
	template: string,
	selector: string,
	className: string
): CompiledComponent {
	return compileComponent(template, selector, className)
}
