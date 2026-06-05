/**
 * @module
 *
 * Lexical region splitter for `.treaty` single-file components.
 *
 * A `.treaty` file is *TypeScript by default*: any text that is not inside an
 * HTML tag region, a `<style>` block, a `{{ … }}` template interpolation, an
 * `@`-prefixed control-flow marker, or a leading ```` ``` ````-fenced macro
 * block is component-body TypeScript. This module scans the source into those
 * regions so the language layer can surface exactly the TS spans to the
 * TypeScript service while leaving the non-TS spans untouched.
 *
 * This is a *region scanner*, not a compiler: it reproduces the region
 * boundaries of the Rust treaty lexer (`apps/rust/authoring/src/treaty/lexer.rs`)
 * so positions map faithfully, but it never parses or lowers the embedded code.
 * Actual compilation and diagnostics always come from the Rust authoring
 * compiler via the NAPI addon.
 */

/** The kind of a scanned `.treaty` region. */
export type TreatyRegionKind =
	/** Component-body TypeScript (the TS-by-default region). */
	| 'ts'
	/** An HTML template tag region. */
	| 'html'
	/** A `<style …>…</style>` block. */
	| 'style'
	/** A `{{ … }}` template interpolation. */
	| 'expression'
	/** An `@if` / `@for` / `@switch` / `@defer` (etc.) control-flow marker. */
	| 'control-flow'
	/** A leading ```` ``` ````-fenced compile-time macro block. */
	| 'macro'

/** A half-open `[start, end)` byte/char span of one region within the source. */
export interface TreatyRegion {
	readonly kind: TreatyRegionKind
	/** Inclusive start offset into the original source (UTF-16 code units). */
	readonly start: number
	/** Exclusive end offset into the original source (UTF-16 code units). */
	readonly end: number
}

const CONTROL_FLOW_KEYWORDS = [
	'@placeholder',
	'@default',
	'@loading',
	'@switch',
	'@else if',
	'@empty',
	'@defer',
	'@error',
	'@else',
	'@case',
	'@for',
	'@if',
] as const

/**
 * Scan a `.treaty` source into ordered, non-overlapping {@link TreatyRegion}s
 * covering the entire input. Adjacent TypeScript runs are emitted as one `ts`
 * region; every offset in `[0, source.length)` belongs to exactly one region.
 */
export function scanTreatyRegions(source: string): TreatyRegion[] {
	const scanner = new RegionScanner(source)
	return scanner.scan()
}

/** Convenience: just the TypeScript-by-default regions, in source order. */
export function typeScriptRegions(source: string): TreatyRegion[] {
	return scanTreatyRegions(source).filter((r) => r.kind === 'ts')
}

/** A half-open `[start, end)` span of one region's INNER content (delimiters stripped). */
export interface RegionInner {
	/** Inclusive start offset of the inner content into the original source. */
	readonly start: number
	/** Exclusive end offset of the inner content into the original source. */
	readonly end: number
}

/**
 * The inner expression span of a `{{ … }}` interpolation region — the text
 * between the opening `{{` and the closing `}}`, with the surrounding
 * whitespace trimmed off. Returns `undefined` when the region is not a
 * well-formed interpolation (no closing `}}`).
 *
 * This is the span the language layer projects into the embedded TypeScript
 * code so a `{{ count }}` expression shares the component-body scope and the
 * TypeScript service drives completion/hover inside it.
 */
export function interpolationInner(source: string, region: TreatyRegion): RegionInner | undefined {
	if (region.kind !== 'expression') {
		return undefined
	}
	// `{{` … `}}`. Guard a malformed region missing the closing braces.
	const open = region.start + 2
	const hasClose = source.startsWith('}}', region.end - 2) && region.end - 2 >= open
	const close = hasClose ? region.end - 2 : region.end
	return trimInner(source, open, close)
}

/**
 * The inner CSS-body span of a `<style …>…</style>` region — the text between
 * the opening tag's `>` and the closing `</style>`. Returns `undefined` when no
 * opening `>` is found inside the region.
 *
 * This is the span the language layer projects into an embedded `css` code so
 * the CSS language service drives completion/hover/validation inside a `<style>`
 * block.
 */
export function styleInner(source: string, region: TreatyRegion): RegionInner | undefined {
	if (region.kind !== 'style') {
		return undefined
	}
	const gt = source.indexOf('>', region.start)
	if (gt === -1 || gt >= region.end) {
		return undefined
	}
	const bodyStart = gt + 1
	const closeIdx = source.lastIndexOf('</style>', region.end)
	const bodyEnd = closeIdx > bodyStart ? closeIdx : region.end
	return { start: bodyStart, end: bodyEnd }
}

/**
 * Find every `{{ … }}` interpolation WITHIN a region's span and return each
 * one's inner expression span (delimiters stripped, whitespace trimmed). Used to
 * project interpolations that live INSIDE an `html` region (the common case —
 * `<div>{{ count }}</div>` is one `html` region, not a separate `expression`
 * region) into the embedded TypeScript code so TS completion works inside them.
 *
 * Strings inside the interpolation are skipped so a `}}` inside a `"…"` literal
 * never closes the interpolation early. Returns spans in source order.
 */
export function findInterpolationsIn(source: string, region: TreatyRegion): RegionInner[] {
	const out: RegionInner[] = []
	let i = region.start
	const end = region.end
	while (i < end) {
		if (source.startsWith('{{', i)) {
			const open = i + 2
			let j = open
			let depth = 0
			while (j < end) {
				const ch = source[j]
				if (ch === '"' || ch === "'" || ch === '`') {
					j = skipString(source, j, end)
					continue
				}
				if (ch === '{') {
					depth++
				} else if (ch === '}') {
					if (depth === 0 && source.startsWith('}}', j)) {
						break
					}
					depth--
				}
				j++
			}
			out.push(trimInner(source, open, j))
			i = j + 2
			continue
		}
		i++
	}
	return out
}

/** Skip a quoted string starting at `i` (the opening quote); returns the index past the close. */
function skipString(source: string, i: number, end: number): number {
	const quote = source[i]
	let j = i + 1
	while (j < end) {
		const ch = source[j]
		if (ch === '\\') {
			j += 2
			continue
		}
		if (ch === quote) {
			return j + 1
		}
		j++
	}
	return end
}

/** Trim leading/trailing ASCII whitespace from a `[start, end)` span. */
function trimInner(source: string, start: number, end: number): RegionInner {
	let s = start
	let e = end
	while (s < e && isWhitespace(source[s]!)) {
		s++
	}
	while (e > s && isWhitespace(source[e - 1]!)) {
		e--
	}
	return { start: s, end: e }
}

class RegionScanner {
	private readonly src: string
	private readonly len: number
	private pos = 0
	private atFileTop = true
	private readonly regions: TreatyRegion[] = []
	/** Start of the current pending TypeScript run, or -1 when none is open. */
	private tsStart = -1

	constructor(source: string) {
		this.src = source
		this.len = source.length
	}

	scan(): TreatyRegion[] {
		while (this.pos < this.len) {
			const before = this.pos
			this.consumeWhitespace()
			if (this.pos >= this.len) {
				break
			}

			// A ```-fenced block at the very top of the file is a macro block. It
			// is only recognized here, mirroring the Rust lexer's `at_file_top`.
			if (this.atFileTop && this.startsWith('```')) {
				this.flushTs()
				this.scanMacro()
				this.atFileTop = false
				continue
			}

			const ch = this.src[this.pos]
			if (ch === '<' && this.startsWithStyleOpen()) {
				this.flushTs()
				this.scanStyle()
				this.atFileTop = false
				continue
			}
			if (ch === '<' && !this.startsWith('</')) {
				this.flushTs()
				this.scanHtml()
				this.atFileTop = false
				continue
			}
			if (ch === '{' && this.startsWith('{{')) {
				this.flushTs()
				this.scanExpression()
				this.atFileTop = false
				continue
			}
			if (ch === '@' && this.matchControlFlowKeyword() !== undefined) {
				this.flushTs()
				this.scanControlFlow()
				this.atFileTop = false
				continue
			}

			// Otherwise this is TypeScript-by-default. Open a TS run at the first
			// non-whitespace char (so leading whitespace is not attributed to TS),
			// then advance one statement-ish chunk the way the Rust lexer does.
			if (this.tsStart === -1) {
				this.tsStart = this.pos
			}
			this.atFileTop = false
			this.scanJavaScript()

			// Defensive: guarantee forward progress so a malformed source can never
			// spin forever.
			if (this.pos === before) {
				this.pos++
			}
		}

		this.flushTs()
		return this.regions
	}

	/** Close any open TypeScript run as a `ts` region. */
	private flushTs(): void {
		if (this.tsStart !== -1 && this.pos > this.tsStart) {
			this.regions.push({ kind: 'ts', start: this.tsStart, end: this.pos })
		}
		this.tsStart = -1
	}

	/**
	 * Advance over a TypeScript chunk, stopping at the boundaries the Rust lexer
	 * uses to leave the JavaScript state: a statement terminator (`;`/newline), a
	 * `<style`/`</`/`{{`/`@` transition, or end of input. The open TS run is left
	 * open so a following TS chunk coalesces into the same region.
	 */
	private scanJavaScript(): void {
		while (this.pos < this.len) {
			const ch = this.src[this.pos]
			if (ch === '"' || ch === "'" || ch === '`') {
				this.consumeString(ch)
				continue
			}
			if (ch === '/' && this.startsWith('//')) {
				this.consumeLineComment()
				continue
			}
			if (ch === '/' && this.startsWith('/*')) {
				this.consumeBlockComment()
				continue
			}
			if (ch === '\n' || ch === '\r' || ch === '\f' || ch === ';') {
				this.pos++
				return
			}
			if (ch === '<' && (this.startsWithStyleOpen() || this.startsWith('</'))) {
				return
			}
			if (ch === '{' && this.startsWith('{{')) {
				return
			}
			if (ch === '@') {
				return
			}
			this.pos++
		}
	}

	/** Scan a `{{ … }}` interpolation, balancing inner braces and skipping strings. */
	private scanExpression(): void {
		const start = this.pos
		this.pos += 2 // skip '{{'
		let depth = 0
		while (this.pos < this.len) {
			const ch = this.src[this.pos]
			if (ch === '"' || ch === "'" || ch === '`') {
				this.consumeString(ch)
				continue
			}
			if (ch === '{') {
				depth++
			} else if (ch === '}') {
				if (depth === 0 && this.startsWith('}}')) {
					this.pos += 2
					break
				}
				depth--
			}
			this.pos++
		}
		this.regions.push({ kind: 'expression', start, end: this.pos })
	}

	/** Scan an `@`-prefixed control-flow marker token (just the keyword). */
	private scanControlFlow(): void {
		const start = this.pos
		const kw = this.matchControlFlowKeyword()
		if (kw === undefined) {
			// Not actually control flow — treat the '@' as TypeScript.
			if (this.tsStart === -1) {
				this.tsStart = this.pos
			}
			this.pos++
			return
		}
		this.pos += kw.length
		this.regions.push({ kind: 'control-flow', start, end: this.pos })
	}

	/** Scan a top-of-file ```` ``` ````-fenced macro block. */
	private scanMacro(): void {
		const start = this.pos
		this.pos += 3 // opening fence
		// Skip the rest of the opening line (the info string).
		while (this.pos < this.len && this.src[this.pos] !== '\n' && this.src[this.pos] !== '\r') {
			this.pos++
		}
		if (this.src[this.pos] === '\r') {
			this.pos++
		}
		if (this.src[this.pos] === '\n') {
			this.pos++
		}
		while (this.pos < this.len && !this.startsWith('```')) {
			this.pos++
		}
		if (this.startsWith('```')) {
			this.pos += 3
		}
		this.regions.push({ kind: 'macro', start, end: this.pos })
	}

	/** Scan a `<style …>…</style>` block. */
	private scanStyle(): void {
		const start = this.pos
		this.pos += '<style'.length
		// Consume the rest of the opening tag.
		while (this.pos < this.len) {
			const ch = this.src[this.pos]
			if (ch === '>') {
				this.pos++
				break
			}
			if (ch === '"' || ch === "'" || ch === '`') {
				this.consumeString(ch)
				continue
			}
			this.pos++
		}
		while (this.pos < this.len && !this.startsWith('</style>')) {
			this.pos++
		}
		if (this.startsWith('</style>')) {
			this.pos += '</style>'.length
		}
		this.regions.push({ kind: 'style', start, end: this.pos })
	}

	/**
	 * Scan an HTML tag region, tracking a tag stack so the region closes at the
	 * matching end tag (or after a self-closing tag), mirroring the Rust lexer.
	 */
	private scanHtml(): void {
		const start = this.pos
		const tagStack: string[] = []
		while (this.pos < this.len) {
			this.consumeWhitespace()
			if (this.pos >= this.len) {
				break
			}
			const ch = this.src[this.pos]
			if (ch !== '<') {
				this.pos++
				continue
			}
			if (this.startsWith('<!--')) {
				this.consumeHtmlComment()
				continue
			}
			if (this.startsWith('</')) {
				this.pos += 2
				this.consumeTagName()
				tagStack.pop()
				this.consumeUntil('>')
				if (this.pos < this.len) {
					this.pos++ // skip '>'
				}
				if (tagStack.length === 0) {
					break
				}
				continue
			}
			// Opening tag.
			this.pos++ // skip '<'
			this.consumeTagName()
			tagStack.push('')
			if (this.consumeAttributes()) {
				// Self-closing tag closes immediately.
				tagStack.pop()
				if (tagStack.length === 0) {
					break
				}
			}
		}
		this.regions.push({ kind: 'html', start, end: this.pos })
	}

	// --- low-level consumers -------------------------------------------------

	private consumeString(delimiter: string): void {
		this.pos++ // opening quote
		while (this.pos < this.len) {
			const ch = this.src[this.pos]
			if (ch === '\\') {
				this.pos += 2
				continue
			}
			if (ch === delimiter) {
				this.pos++
				return
			}
			this.pos++
		}
	}

	private consumeLineComment(): void {
		while (this.pos < this.len && this.src[this.pos] !== '\n') {
			this.pos++
		}
	}

	private consumeBlockComment(): void {
		this.pos += 2 // '/*'
		while (this.pos < this.len && !this.startsWith('*/')) {
			this.pos++
		}
		if (this.startsWith('*/')) {
			this.pos += 2
		}
	}

	private consumeHtmlComment(): void {
		this.pos += '<!--'.length
		while (this.pos < this.len && !this.startsWith('-->')) {
			this.pos++
		}
		if (this.startsWith('-->')) {
			this.pos += '-->'.length
		}
	}

	private consumeTagName(): void {
		while (this.pos < this.len && isAlphanumeric(this.src[this.pos]!)) {
			this.pos++
		}
	}

	/** Consume attributes up to the closing `>`; returns true if self-closing. */
	private consumeAttributes(): boolean {
		while (this.pos < this.len) {
			const ch = this.src[this.pos]
			if (ch === '>') {
				this.pos++
				return false
			}
			if (ch === '/' && this.src[this.pos + 1] === '>') {
				this.pos += 2
				return true
			}
			if (ch === '"' || ch === "'" || ch === '`') {
				this.consumeString(ch)
				continue
			}
			this.pos++
		}
		return false
	}

	private consumeUntil(target: string): void {
		while (this.pos < this.len && this.src[this.pos] !== target) {
			this.pos++
		}
	}

	private consumeWhitespace(): void {
		while (this.pos < this.len && isWhitespace(this.src[this.pos]!)) {
			this.pos++
		}
	}

	// --- lookahead helpers ---------------------------------------------------

	private startsWith(s: string): boolean {
		return this.src.startsWith(s, this.pos)
	}

	/**
	 * True when the cursor opens a `<style` tag (`<style>`, `<style …>`, or
	 * `<style/>`) rather than e.g. `<styled>`.
	 */
	private startsWithStyleOpen(): boolean {
		if (!this.startsWith('<style')) {
			return false
		}
		const after = this.src[this.pos + '<style'.length]
		return (
			after === undefined ||
			after === '>' ||
			after === '/' ||
			isWhitespace(after)
		)
	}

	/** The control-flow keyword at the cursor, longest-match first, or undefined. */
	private matchControlFlowKeyword(): string | undefined {
		for (const kw of CONTROL_FLOW_KEYWORDS) {
			if (this.startsWith(kw)) {
				return kw
			}
		}
		return undefined
	}
}

function isWhitespace(ch: string): boolean {
	return ch === ' ' || ch === '\t' || ch === '\n' || ch === '\r' || ch === '\f' || ch === '\v'
}

function isAlphanumeric(ch: string): boolean {
	return /[0-9A-Za-z]/.test(ch)
}
