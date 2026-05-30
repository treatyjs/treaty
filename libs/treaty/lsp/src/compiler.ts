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
	compileComponent,
	compileComponentSource,
	compileTreatyFile,
} from '@treaty/authoring-node'

/** Result of a single compilation: emitted code plus any compiler errors. */
export interface CompiledComponent {
	readonly code: string
	readonly errors: readonly string[]
}

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
