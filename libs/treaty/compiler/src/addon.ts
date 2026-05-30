/**
 * @module
 *
 * Typed bridge to the Rust authoring compiler, exposed to Node via the
 * `@treaty/authoring-node` NAPI addon. Every lowering in this package routes
 * through this single seam; the compiler is never reimplemented in TypeScript.
 *
 * The addon's three entry points map onto the file kinds Treaty owns:
 *   - `compileTreatyFile`     -> `.treaty` single-file components
 *   - `compileComponentSource`-> `.tsx` / `.tjsx` authoring and `.ts` `@Component`
 *   - `compileComponent`      -> template/selector/className triples
 *
 * All three return Ivy JS plus a list of compiler errors.
 */

import {
	compileComponent,
	compileComponentSource,
	compileTreatyFile,
} from '@treaty/authoring-node'

/** Result of a single compilation: emitted Ivy JS plus any compiler errors. */
export interface CompiledComponent {
	readonly code: string
	readonly errors: readonly string[]
}

/** Compile a `.treaty` single-file component by name. */
export function compileTreaty(source: string, fileName: string): CompiledComponent {
	return compileTreatyFile(source, fileName)
}

/** Compile a component from full source text (`.tsx` / `.tjsx` / `.ts` `@Component`). */
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
