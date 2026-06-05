/**
 * @module
 *
 * Dead-code / tree-shaking metadata helpers. These post-process the Ivy JS
 * emitted by the Rust compiler with bundler-friendly annotations; they do not
 * alter program semantics, only what an optimizing bundler is allowed to drop.
 *
 * Framework-agnostic: no bundler is imported here.
 */

/** The pure annotation bundlers (Rollup, esbuild, Terser) recognize. */
export const PURE_ANNOTATION = '/*#__PURE__*/'

/**
 * The `sideEffects` descriptor for a module. Pure component modules declare
 * `false` so bundlers may drop them entirely when their exports are unused.
 */
export interface SideEffectsDescriptor {
	/** `false` for pure component modules. */
	readonly sideEffects: false | readonly string[]
}

/** A pure-module descriptor (`sideEffects: false`). */
export const PURE_MODULE: SideEffectsDescriptor = { sideEffects: false }

/**
 * Annotate the Ivy factory calls in `code` with the pure annotation so a
 * bundler can drop the module's exports when they are unused.
 *
 * Targets the Ivy definition factories emitted by the compiler
 * (`ɵɵdefineComponent`, `ɵɵdefineDirective`, `ɵɵdefinePipe`,
 * `ɵɵdefineNgModule`, `ɵɵdefineInjector`). Only un-annotated occurrences are
 * touched, so the transform is idempotent.
 */
export function annotatePureFactories(code: string): string {
	// Match `i0.ɵɵdefineComponent(` and friends, optionally already prefixed.
	const factory = /(\/\*#__PURE__\*\/\s*)?(\b[\w$]+\.ɵɵdefine(?:Component|Directive|Pipe|NgModule|Injector)\s*\()/g
	return code.replace(factory, (_match, existing: string | undefined, call: string) =>
		existing ? `${PURE_ANNOTATION} ${call}` : `${PURE_ANNOTATION} ${call}`
	)
}

/**
 * Drop unused server-fn client bindings from `code`.
 *
 * A server function authored as `const <name> = createServerFn(...)` (or
 * `ɵɵserverFn`) should never ship its body to the browser when `<name>` is
 * never referenced elsewhere in the module. This rewrites such a binding to a
 * bare client stub so the bundler can eliminate the original closure.
 *
 * Conservative by design: a binding is only dropped when its declared name does
 * not appear again anywhere else in the source. Returns the (possibly
 * unchanged) code.
 */
export function dropUnusedServerFns(code: string): string {
	const decl = /(?:const|let|var)\s+([\w$]+)\s*=\s*(?:createServerFn|ɵɵserverFn)\s*\(/g
	const names: string[] = []
	let m: RegExpExecArray | null
	while ((m = decl.exec(code)) !== null) {
		const name = m[1]
		if (name) names.push(name)
	}
	if (names.length === 0) return code

	let out = code
	for (const name of names) {
		// Count references to the binding name (word-boundary) across the module.
		const ref = new RegExp(`\\b${escapeRegExp(name)}\\b`, 'g')
		const refs = (out.match(ref) ?? []).length
		// One occurrence == only the declaration itself: the binding is unused.
		if (refs <= 1) {
			const declLine = new RegExp(
				`((?:const|let|var)\\s+${escapeRegExp(name)}\\s*=\\s*)(?:createServerFn|\\u0275\\u0275serverFn)\\s*\\([^]*?\\)\\s*;?`
			)
			out = out.replace(declLine, `$1/* dropped unused server-fn client binding */ undefined;`)
		}
	}
	return out
}

/** Escape a string for safe interpolation into a `RegExp`. */
function escapeRegExp(input: string): string {
	return input.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}
