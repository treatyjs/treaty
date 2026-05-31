/**
 * @module
 *
 * **Static Ivy renderer**: turns the Ivy JS that `@treaty/compiler` emits for a
 * component into a static HTML string at build time, binding interpolations
 * against render data.
 *
 * Ivy lowers a template to a `*_Template(rf, ctx)` function whose body is a
 * deterministic instruction stream: a *create* block (`rf & 1`) of
 * `ɵɵdomElementStart` / `ɵɵtext` / `ɵɵdomElementEnd` / `ɵɵtext("literal")`
 * calls that build the DOM shape, and an *update* block (`rf & 2`) of
 * `ɵɵadvance()` / `ɵɵtextInterpolate(ctx.x)` calls that fill the dynamic text.
 * Because that stream is data — not behaviour — it can be replayed at build time
 * against the route's render data to emit the same HTML the browser would, with
 * no DOM, no zone, and no Angular runtime.
 *
 * This is intentionally a focused interpreter, not a full Ivy VM: it covers the
 * static-content + text-interpolation subset that SSG prerender targets (the
 * shapes the smoke fixture and typical content routes use). Instructions outside
 * that subset are ignored rather than guessed at, so output is always a faithful
 * subset of the live render — never a wrong one — and the hydration marker tells
 * the client runtime to take over for the dynamic remainder.
 */

import type { RenderData } from './runtime.js'

/** Void HTML elements that never get a closing tag. */
const VOID_ELEMENTS = new Set([
	'area',
	'base',
	'br',
	'col',
	'embed',
	'hr',
	'img',
	'input',
	'link',
	'meta',
	'param',
	'source',
	'track',
	'wbr',
])

/** A node accumulated while replaying the Ivy create block. */
interface ElementNode {
	readonly tag: string
	readonly attrs: { name: string; value: string }[]
	/** Index-addressable children: text slots (filled in the update block) + elements. */
	readonly children: RenderNode[]
}

/** A text slot whose content the update block fills via interpolation. */
interface TextNode {
	/** Static literal text (`ɵɵtext(1, "hi")`) or filled interpolation. */
	text: string
}

type RenderNode = ElementNode | TextNode

function isElement(node: RenderNode): node is ElementNode {
	return (node as ElementNode).tag !== undefined
}

/** Escape a text node's content for safe HTML output. */
function escapeText(value: string): string {
	return value.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
}

/** Escape an attribute value for safe double-quoted output. */
function escapeAttr(value: string): string {
	return value.replace(/&/g, '&amp;').replace(/"/g, '&quot;')
}

/** Stringify a render-data value the way a template interpolation would. */
function stringifyBinding(value: unknown): string {
	if (value === null || value === undefined) return ''
	if (typeof value === 'string') return value
	if (typeof value === 'number' || typeof value === 'boolean') return String(value)
	return JSON.stringify(value)
}

/** Resolve a `ctx.a.b` member path against the render data. */
function resolvePath(data: RenderData, path: readonly string[]): unknown {
	let cur: unknown = data
	for (const key of path) {
		if (cur === null || typeof cur !== 'object') return undefined
		cur = (cur as Record<string, unknown>)[key]
	}
	return cur
}

/**
 * Isolate the body of the `*_Template(rf, ctx)` function from emitted Ivy JS.
 * Returns the source between the function's first `{` and its matching `}`, or
 * `null` when no template function is present (e.g. a pass-through module).
 */
function extractTemplateBody(code: string): string | null {
	const sig = /function\s+[A-Za-z_$][\w$]*_Template\s*\([^)]*\)\s*\{/.exec(code)
	if (!sig) return null
	const open = sig.index + sig[0].length - 1
	let depth = 0
	for (let i = open; i < code.length; i++) {
		const ch = code[i]
		if (ch === '{') depth++
		else if (ch === '}') {
			depth--
			if (depth === 0) return code.slice(open + 1, i)
		}
	}
	return null
}

/** One parsed Ivy instruction: the bare name and its raw argument list text. */
interface Instruction {
	readonly name: string
	readonly args: string
}

/**
 * Tokenize the Ivy instruction calls (`iN.ɵɵfoo(args)` or `ɵɵfoo(args)`) in a
 * block of template-function source, in source order. Argument text is captured
 * raw (balanced parens) for the per-instruction parsers to interpret.
 */
function parseInstructions(block: string): Instruction[] {
	const out: Instruction[] = []
	const callRe = /(?:[A-Za-z_$][\w$]*\.)?(ɵɵ[A-Za-z]+)\s*\(/g
	let m: RegExpExecArray | null
	while ((m = callRe.exec(block)) !== null) {
		const name = m[1]!
		const argsStart = m.index + m[0].length
		let depth = 1
		let i = argsStart
		for (; i < block.length && depth > 0; i++) {
			const ch = block[i]
			if (ch === '(') depth++
			else if (ch === ')') depth--
		}
		out.push({ name, args: block.slice(argsStart, i - 1) })
		callRe.lastIndex = i
	}
	return out
}

/**
 * Split an instruction's raw argument text on top-level commas (ignoring commas
 * inside nested parens, brackets, braces, or string literals).
 */
function splitArgs(args: string): string[] {
	const parts: string[] = []
	let depth = 0
	let quote: string | null = null
	let start = 0
	for (let i = 0; i < args.length; i++) {
		const ch = args[i]
		if (quote) {
			if (ch === quote && args[i - 1] !== '\\') quote = null
			continue
		}
		if (ch === '"' || ch === "'" || ch === '`') quote = ch
		else if (ch === '(' || ch === '[' || ch === '{') depth++
		else if (ch === ')' || ch === ']' || ch === '}') depth--
		else if (ch === ',' && depth === 0) {
			parts.push(args.slice(start, i).trim())
			start = i + 1
		}
	}
	const tail = args.slice(start).trim()
	if (tail !== '' || parts.length > 0) parts.push(tail)
	return parts.filter((p) => p !== '')
}

/** Unquote a string-literal argument (`"x"`, `'x'`); returns null otherwise. */
function asStringLiteral(arg: string): string | null {
	const t = arg.trim()
	if (t.length >= 2 && (t[0] === '"' || t[0] === "'" || t[0] === '`') && t[t.length - 1] === t[0]) {
		return t.slice(1, -1)
	}
	return null
}

/** Parse a `ctx.a.b` reference to its member path, or null for other exprs. */
function asCtxPath(arg: string): string[] | null {
	const t = arg.trim()
	const m = /^ctx\.((?:[A-Za-z_$][\w$]*)(?:\.[A-Za-z_$][\w$]*)*)$/.exec(t)
	return m ? m[1]!.split('.') : null
}

/**
 * Replay the create block to build the node tree. The create instructions form
 * a flat, slot-indexed, depth-first description of the DOM; `*Start`/`*End`
 * pairs push/pop the current parent, single-shot element/text instructions add a
 * leaf. Slot indices are recorded so the update block can address text nodes.
 */
function buildCreateTree(instructions: readonly Instruction[]): {
	roots: RenderNode[]
	slots: (RenderNode | undefined)[]
} {
	const roots: RenderNode[] = []
	const slots: (RenderNode | undefined)[] = []
	const stack: ElementNode[] = []

	const attach = (node: RenderNode): void => {
		const parent = stack[stack.length - 1]
		if (parent) parent.children.push(node)
		else roots.push(node)
	}

	for (const ins of instructions) {
		switch (ins.name) {
			case 'ɵɵelementStart':
			case 'ɵɵdomElementStart': {
				const parts = splitArgs(ins.args)
				const slot = Number(parts[0])
				const tag = asStringLiteral(parts[1] ?? '') ?? 'div'
				const node: ElementNode = { tag, attrs: parseAttrs(parts[2]), children: [] }
				attach(node)
				stack.push(node)
				if (Number.isInteger(slot)) slots[slot] = node
				break
			}
			case 'ɵɵelementEnd':
			case 'ɵɵdomElementEnd': {
				stack.pop()
				break
			}
			case 'ɵɵelement':
			case 'ɵɵdomElement': {
				const parts = splitArgs(ins.args)
				const slot = Number(parts[0])
				const tag = asStringLiteral(parts[1] ?? '') ?? 'div'
				const node: ElementNode = { tag, attrs: parseAttrs(parts[2]), children: [] }
				attach(node)
				if (Number.isInteger(slot)) slots[slot] = node
				break
			}
			case 'ɵɵtext': {
				const parts = splitArgs(ins.args)
				const slot = Number(parts[0])
				const literal = parts.length > 1 ? (asStringLiteral(parts[1] ?? '') ?? '') : ''
				const node: TextNode = { text: literal }
				attach(node)
				if (Number.isInteger(slot)) slots[slot] = node
				break
			}
			default:
				// Other create instructions (listeners, projection, …) carry no
				// static HTML we can faithfully emit; skip rather than guess.
				break
		}
	}
	return { roots, slots }
}

/**
 * Parse a `ɵɵelementStart` consts attribute array (`["id", "main"]`) into
 * name/value pairs. Returns `[]` for the common no-attrs case (a numeric const
 * index or absent argument), since we do not resolve the consts table here.
 */
function parseAttrs(arg: string | undefined): { name: string; value: string }[] {
	if (arg === undefined) return []
	const t = arg.trim()
	if (!t.startsWith('[')) return []
	const inner = t.slice(1, -1)
	const items = splitArgs(inner)
		.map((it) => asStringLiteral(it))
		.filter((it): it is string => it !== null)
	const attrs: { name: string; value: string }[] = []
	for (let i = 0; i + 1 < items.length; i += 2) {
		attrs.push({ name: items[i]!, value: items[i + 1]! })
	}
	return attrs
}

/**
 * Replay the update block, filling text slots from interpolation instructions.
 * `ɵɵadvance(n)` moves a virtual cursor across the slot table; the various
 * `ɵɵtextInterpolate*` instructions write the interpolated string into the slot
 * at the cursor. Bindings that are `ctx.path` resolve against `data`; quoted
 * literals are written verbatim; anything else resolves to the empty string
 * (the dynamic remainder hydration fills in).
 */
function applyUpdateBlock(
	instructions: readonly Instruction[],
	slots: (RenderNode | undefined)[],
	data: RenderData
): void {
	let cursor = 0
	const writeText = (value: string): void => {
		const node = slots[cursor]
		if (node && !isElement(node)) node.text = value
	}
	for (const ins of instructions) {
		switch (ins.name) {
			case 'ɵɵadvance': {
				const parts = splitArgs(ins.args)
				const by = parts.length > 0 ? Number(parts[0]) : 1
				cursor += Number.isInteger(by) ? by : 1
				break
			}
			case 'ɵɵtextInterpolate':
			case 'ɵɵtextInterpolate1': {
				writeText(interpolate(ins.name, splitArgs(ins.args), data))
				break
			}
			default: {
				// Higher-arity interpolations (textInterpolate2..8) and property
				// bindings: render what we can, else leave the slot for hydration.
				if (ins.name.startsWith('ɵɵtextInterpolate')) {
					writeText(interpolate(ins.name, splitArgs(ins.args), data))
				}
				break
			}
		}
	}
}

/**
 * Compute the interpolated string for a `ɵɵtextInterpolate*` instruction.
 * `ɵɵtextInterpolate(expr)` is the single-binding form; the `N`-suffixed forms
 * interleave string literals and bindings (`prefix, e0, i0, e1, …, suffix`). Any
 * argument that is a `ctx.path` resolves against `data`, a quoted literal is
 * taken verbatim, and an unrecognized expression contributes the empty string.
 */
function interpolate(name: string, args: readonly string[], data: RenderData): string {
	if (name === 'ɵɵtextInterpolate') {
		return evalArg(args[0] ?? '', data)
	}
	// Interleaved form: literals at even indices, bindings at odd indices.
	let out = ''
	for (let i = 0; i < args.length; i++) {
		const literal = asStringLiteral(args[i] ?? '')
		out += literal !== null ? literal : evalArg(args[i] ?? '', data)
	}
	return out
}

/** Evaluate one interpolation argument to its string contribution. */
function evalArg(arg: string, data: RenderData): string {
	const literal = asStringLiteral(arg)
	if (literal !== null) return literal
	const path = asCtxPath(arg)
	if (path) return stringifyBinding(resolvePath(data, path))
	return ''
}

/** Serialize a built node tree to an HTML string. */
function serialize(nodes: readonly RenderNode[]): string {
	let html = ''
	for (const node of nodes) {
		if (!isElement(node)) {
			html += escapeText(node.text)
			continue
		}
		const attrs = node.attrs.map((a) => ` ${a.name}="${escapeAttr(a.value)}"`).join('')
		if (VOID_ELEMENTS.has(node.tag.toLowerCase())) {
			html += `<${node.tag}${attrs}>`
		} else {
			html += `<${node.tag}${attrs}>${serialize(node.children)}</${node.tag}>`
		}
	}
	return html
}

/**
 * Render the emitted Ivy JS for a component to a static HTML fragment, binding
 * interpolations against `data`. Returns the empty string when `code` carries no
 * recognizable template function (a pass-through module), so callers can treat
 * "nothing to prerender" uniformly.
 *
 * @param code Ivy JS emitted by `@treaty/compiler` for one component.
 * @param data Render data (from the component's render-time macro, or `{}`).
 */
export function renderIvyToHtml(code: string, data: RenderData = {}): string {
	const body = extractTemplateBody(code)
	if (body === null) return ''

	// The create block is everything the compiler emits under `rf & 1`; the
	// update block under `rf & 2`. Splitting on the `rf & 2` guard keeps the two
	// instruction streams apart so cursor/slot semantics match Ivy's.
	const updateGuard = /if\s*\(\s*rf\s*&\s*2\s*\)/.exec(body)
	const createSrc = updateGuard ? body.slice(0, updateGuard.index) : body
	const updateSrc = updateGuard ? body.slice(updateGuard.index) : ''

	const { roots, slots } = buildCreateTree(parseInstructions(createSrc))
	if (updateSrc !== '') applyUpdateBlock(parseInstructions(updateSrc), slots, data)

	return serialize(roots)
}
