/**
 * @module
 *
 * The **render-time data seam** for `@treaty/ssg`. Treaty components may carry
 * top-of-file render-time logic — a fenced ``` macro or an RSC-style
 * `async` data function — that must execute at BUILD time to produce the data a
 * route's template interpolates. That execution is the job of the **Nova
 * runtime** (`libs/runtime`, Rust), whose `run_macro(ts_source, input_json)`
 * transpiles the TS macro to JS and runs it in a fresh Nova isolate, returning
 * the JSON value the template binds against.
 *
 * Treaty is a compiler, not a host, so this package does not embed Nova. The
 * Nova `run_macro` entry is reachable from Rust and is *intended* to be surfaced
 * to Node through the `@treaty/authoring-node` NAPI addon. At the time of
 * writing that addon exposes only the compile entry points
 * (`compile`/`compileMany`/…) and NOT `run_macro` — see
 * `libs/authoring/node/index.d.ts`. Rather than depend on an addon symbol that
 * is not yet there, `@treaty/ssg` defines the clean {@link RenderRuntime}
 * interface here. A Nova-backed implementation that simply forwards to the
 * addon's `runMacro` (once exported) satisfies it verbatim — see
 * {@link createNovaRenderRuntime} for the exact structural binding — and the
 * built-in {@link StubRenderRuntime} lets the pipeline (and its smoke test) run
 * end-to-end today without the native macro entry.
 */

/** A JSON value — the only thing that crosses the runtime boundary. */
export type JsonValue =
	| null
	| boolean
	| number
	| string
	| readonly JsonValue[]
	| { readonly [key: string]: JsonValue }

/** A plain JSON object: the render data a template interpolates against. */
export type RenderData = { readonly [key: string]: JsonValue }

/** A render-time macro/RSC data unit extracted from a route's component. */
export interface RenderMacro {
	/**
	 * The TypeScript macro source to execute (the body of the top-of-file ```
	 * fence or RSC data function). Run verbatim by the Nova runtime; its return
	 * value becomes the component's render data.
	 */
	readonly source: string
	/**
	 * The JSON input injected as the macro's `input` (route params, query, build
	 * env, …). Defaults to `{}` when omitted.
	 */
	readonly input?: RenderData
}

/**
 * The execute-render-time-data seam. Exactly mirrors Nova's
 * `run_macro(ts_source, input_json) -> MacroOutput` shape: a single call that
 * takes the macro TS source plus a JSON input and returns the JSON value the
 * template binds against. A Nova-backed implementation forwards straight to the
 * addon; the {@link StubRenderRuntime} interprets a tiny safe subset so the
 * pipeline runs without the native entry.
 */
export interface RenderRuntime {
	/**
	 * Execute one render-time macro and return its JSON result as the render
	 * data for a route's component. Implementations must be deterministic for a
	 * given `(source, input)` so prerender output is reproducible.
	 */
	runMacro(macro: RenderMacro): RenderData
}

/**
 * The structural shape `@treaty/authoring-node` is expected to expose for the
 * Nova `run_macro` entry. Declared here (rather than imported) precisely so this
 * package compiles whether or not the addon currently exports it: a Nova-backed
 * runtime binds to this shape at the call site. When the addon adds
 * `export declare function runMacro(tsSource: string, inputJson: string): string`
 * (JSON in, JSON out — matching the Rust `run_macro`), it satisfies
 * {@link NovaMacroAddon} with no change here.
 */
export interface NovaMacroAddon {
	/**
	 * Transpile `tsSource` to JS, run it in a fresh Nova isolate with `inputJson`
	 * injected as the macro input, and return the macro's result as a JSON string
	 * (mirroring `libs/runtime`'s `run_macro`, marshalled across NAPI as JSON).
	 */
	runMacro(tsSource: string, inputJson: string): string
}

/**
 * Build a {@link RenderRuntime} backed by the Nova `run_macro` NAPI entry. Pass
 * the `@treaty/authoring-node` addon (or any object satisfying
 * {@link NovaMacroAddon}) once it surfaces `runMacro`:
 *
 * ```ts
 * import * as addon from '@treaty/authoring-node'
 * const runtime = createNovaRenderRuntime(addon) // when addon.runMacro exists
 * ```
 *
 * This is the documented plug-in point: the rest of the pipeline only ever sees
 * {@link RenderRuntime}, so swapping the {@link StubRenderRuntime} for this
 * Nova-backed one is a one-line change at the call site.
 */
export function createNovaRenderRuntime(addon: NovaMacroAddon): RenderRuntime {
	return {
		runMacro(macro: RenderMacro): RenderData {
			const inputJson = JSON.stringify(macro.input ?? {})
			const resultJson = addon.runMacro(macro.source, inputJson)
			const value: unknown = JSON.parse(resultJson)
			if (value === null || typeof value !== 'object' || Array.isArray(value)) {
				throw new RenderRuntimeError(
					`Nova run_macro must return a JSON object of render data, got ${describe(value)}`
				)
			}
			return value as RenderData
		},
	}
}

/** Error raised when a render runtime cannot execute or marshal a macro. */
export class RenderRuntimeError extends Error {
	constructor(message: string) {
		super(message)
		this.name = 'RenderRuntimeError'
	}
}

/**
 * A dependency-free {@link RenderRuntime} for builds without the native Nova
 * entry. It does NOT run arbitrary TS — it understands a small, explicit,
 * side-effect-free macro subset sufficient to drive prerender and its smoke
 * test, and is deterministic:
 *
 *   - `export default { … }` / `export default ({ … })` — a literal JSON object
 *     of render data (the common static case): parsed and returned.
 *   - `return { … }` — the trailing object literal of a function-body macro.
 *   - `input.foo` references inside those literals resolve against the injected
 *     {@link RenderMacro.input}.
 *
 * Anything outside this subset throws {@link RenderRuntimeError} — the stub
 * never silently mis-renders. The documented path for real macros is to swap in
 * {@link createNovaRenderRuntime}; the stub exists so the pipeline is exercisable
 * today and so static (no-macro) routes need no native dependency at all.
 */
export class StubRenderRuntime implements RenderRuntime {
	runMacro(macro: RenderMacro): RenderData {
		const input = macro.input ?? {}
		const literal = extractObjectLiteral(macro.source)
		if (literal === null) {
			throw new RenderRuntimeError(
				'StubRenderRuntime supports only a literal-object macro ' +
					'(`export default { … }` or `return { … }`); ' +
					'use createNovaRenderRuntime for arbitrary macros.'
			)
		}
		const json = literalToJson(literal, input)
		return json
	}
}

/**
 * Extract the source text of the single object literal a stub-supported macro
 * resolves to: the operand of `export default` (with or without wrapping
 * parens), or the operand of the macro body's `return`. Returns the `{ … }`
 * slice (braces included) or `null` when no such literal is present.
 */
function extractObjectLiteral(source: string): string | null {
	const exportMatch = /export\s+default\s*\(?\s*(\{)/.exec(source)
	const returnMatch = /\breturn\s*\(?\s*(\{)/.exec(source)
	const open = exportMatch
		? exportMatch.index + exportMatch[0].length - 1
		: returnMatch
			? returnMatch.index + returnMatch[0].length - 1
			: -1
	if (open < 0) return null
	const close = matchBrace(source, open)
	if (close < 0) return null
	return source.slice(open, close + 1)
}

/** Index of the `}` matching the `{` at `open`, or -1 if unbalanced. */
function matchBrace(source: string, open: number): number {
	let depth = 0
	for (let i = open; i < source.length; i++) {
		const ch = source[i]
		if (ch === '{') depth++
		else if (ch === '}') {
			depth--
			if (depth === 0) return i
		}
	}
	return -1
}

/**
 * Convert a stub object-literal to JSON. Three narrow, predictable steps:
 *   1. resolve bare `input.key` member reads against the injected `input`,
 *      inlining each as a JSON value;
 *   2. coerce the relaxed JS literal into strict JSON — quote bare identifier
 *      keys and rewrite single-quoted strings as double-quoted — so a natural
 *      `{ title: 'x' }` macro parses;
 *   3. `JSON.parse` the result.
 * Anything richer (computed keys, expressions, method calls) is intentionally
 * unsupported and surfaces as a parse error: that is the Nova runtime's job.
 */
function literalToJson(literal: string, input: RenderData): RenderData {
	const resolved = literal.replace(/\binput\.([A-Za-z_$][\w$]*)/g, (_match, key: string) => {
		const value = input[key]
		return JSON.stringify(value === undefined ? null : value)
	})
	const substituted = relaxedLiteralToJson(resolved)
	let parsed: unknown
	try {
		parsed = JSON.parse(substituted)
	} catch (cause) {
		throw new RenderRuntimeError(
			`StubRenderRuntime could not parse the macro object literal as JSON: ${
				cause instanceof Error ? cause.message : String(cause)
			}`
		)
	}
	if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) {
		throw new RenderRuntimeError('StubRenderRuntime macro literal must be a JSON object')
	}
	return parsed as RenderData
}

/**
 * Coerce a relaxed JS object literal into strict JSON via a single forward scan.
 * String literals (single- or double-quoted) are normalized to double-quoted
 * JSON strings and copied verbatim; outside strings, a bare identifier
 * immediately followed by `:` is treated as an object key and wrapped in
 * quotes; trailing commas before `}`/`]` are dropped. This is the small,
 * deterministic relaxation the stub needs — not a JS parser.
 */
function relaxedLiteralToJson(literal: string): string {
	let out = ''
	let i = 0
	const n = literal.length
	while (i < n) {
		const ch = literal[i]!
		if (ch === '"' || ch === "'") {
			const { text, next } = readString(literal, i, ch)
			out += text
			i = next
			continue
		}
		// A bare identifier that is an object key (next non-space char is `:`).
		if (/[A-Za-z_$]/.test(ch)) {
			let j = i + 1
			while (j < n && /[\w$]/.test(literal[j]!)) j++
			const ident = literal.slice(i, j)
			let k = j
			while (k < n && /\s/.test(literal[k]!)) k++
			if (literal[k] === ':') {
				out += `"${ident}"`
				i = j
				continue
			}
			// A non-key bare word (true/false/null pass through; anything else is
			// left for JSON.parse to reject — the stub never guesses identifiers).
			out += ident
			i = j
			continue
		}
		if (ch === ',') {
			let k = i + 1
			while (k < n && /\s/.test(literal[k]!)) k++
			if (literal[k] === '}' || literal[k] === ']') {
				// Drop the trailing comma.
				i++
				continue
			}
		}
		out += ch
		i++
	}
	return out
}

/**
 * Read a quoted string starting at `start` (whose quote char is `quote`) and
 * return its JSON double-quoted form plus the index just past the closing quote.
 * Handles escapes and re-escapes embedded double quotes when converting from a
 * single-quoted source.
 */
function readString(src: string, start: number, quote: string): { text: string; next: number } {
	let i = start + 1
	let value = ''
	while (i < src.length) {
		const ch = src[i]!
		if (ch === '\\') {
			value += ch + (src[i + 1] ?? '')
			i += 2
			continue
		}
		if (ch === quote) {
			i++
			break
		}
		value += ch
		i++
	}
	// Re-escape any bare double quotes that were legal inside a single-quoted source.
	const json = quote === '"' ? value : value.replace(/"/g, '\\"')
	return { text: `"${json}"`, next: i }
}

/** A short human description of an unexpected runtime value, for diagnostics. */
function describe(value: unknown): string {
	if (value === null) return 'null'
	if (Array.isArray(value)) return 'an array'
	return typeof value
}
