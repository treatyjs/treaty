/**
 * @module
 *
 * Diagnostics provider for Treaty authoring formats.
 *
 * Diagnostics come straight from the Rust authoring compiler (via the
 * `@treaty/authoring-node` NAPI addon): the `.treaty` front-end through
 * {@link compileTreaty}, and `.tsx` / Angular components through
 * {@link compileSource}. The compiler returns descriptive error strings; this
 * module turns each one into a {@link Diagnostic} whose range is mapped back
 * onto the original source — through the embedded virtual-code mappings where a
 * position can be recovered, and onto the relevant region (or the whole
 * document) otherwise. The compiler is never reimplemented here.
 */

import type { CodeMapping, VirtualCode } from '@volar/language-core'
import {
	DiagnosticSeverity,
	type Diagnostic,
	type Position,
	type Range,
} from 'vscode-languageserver'
import { compileSource, compileTreaty, type CompiledComponent } from './compiler.js'
import { EMBEDDED_TS_ID } from './language.js'

/** Diagnostic source tag attached to every Treaty compiler diagnostic. */
export const DIAGNOSTIC_SOURCE = 'treaty'

/**
 * A document to diagnose: its file name (used by the compiler for `.treaty`
 * naming/diagnostics), its full text, and the authoring `languageId` that
 * selects the compiler entry point.
 */
export interface DiagnosticDocument {
	readonly fileName: string
	readonly languageId: string
	readonly text: string
}

/** Line-start offset index over a source string for offset→position mapping. */
class LineIndex {
	private readonly lineStarts: number[]
	private readonly length: number

	constructor(text: string) {
		this.length = text.length
		const starts = [0]
		for (let i = 0; i < text.length; i++) {
			const ch = text.charCodeAt(i)
			if (ch === 10 /* \n */) {
				starts.push(i + 1)
			} else if (ch === 13 /* \r */) {
				// Treat \r\n as a single break; a lone \r also starts a line.
				if (text.charCodeAt(i + 1) === 10) {
					i++
				}
				starts.push(i + 1)
			}
		}
		this.lineStarts = starts
	}

	/** Convert a clamped character offset into a zero-based LSP {@link Position}. */
	positionAt(offset: number): Position {
		const clamped = offset < 0 ? 0 : offset > this.length ? this.length : offset
		let lo = 0
		let hi = this.lineStarts.length - 1
		while (lo < hi) {
			const mid = (lo + hi + 1) >> 1
			if (this.lineStarts[mid]! <= clamped) {
				lo = mid
			} else {
				hi = mid - 1
			}
		}
		return { line: lo, character: clamped - this.lineStarts[lo]! }
	}
}

/**
 * Produce diagnostics for a Treaty document by invoking the Rust compiler and
 * converting its errors into LSP {@link Diagnostic}s.
 *
 * `rootVirtualCode` is the document's root {@link VirtualCode} (from the
 * language layer). Its embedded TypeScript code's mappings are used to anchor
 * messages that carry a recoverable position; messages without one fall back to
 * the start of the embedded TypeScript region, then to the document start.
 */
export function provideDiagnostics(
	document: DiagnosticDocument,
	rootVirtualCode?: VirtualCode,
): Diagnostic[] {
	const compiled = compile(document)
	if (compiled.errors.length === 0) {
		return []
	}

	const lineIndex = new LineIndex(document.text)
	const fallback = fallbackRange(document, rootVirtualCode, lineIndex)

	return compiled.errors.map((message) =>
		toDiagnostic(message, document, lineIndex, fallback),
	)
}

/** Route a document to the right compiler entry point by authoring language id. */
function compile(document: DiagnosticDocument): CompiledComponent {
	if (document.languageId === 'treaty') {
		return compileTreaty(document.text, document.fileName)
	}
	// `.tsx` / `.tjsx` and Angular component sources compile through the source
	// front-end, which parses the decorated class out of the full text.
	return compileSource(document.text)
}

/**
 * Convert one compiler error message into a {@link Diagnostic}. Sass errors
 * (`sass: …`) embed a `path:line:col` suffix that pins the range inside the
 * `<style>` region; other messages anchor to the provided `fallback` range.
 */
function toDiagnostic(
	message: string,
	document: DiagnosticDocument,
	lineIndex: LineIndex,
	fallback: Range,
): Diagnostic {
	const range = sassRange(message, document, lineIndex) ?? fallback
	return {
		range,
		severity: DiagnosticSeverity.Error,
		source: DIAGNOSTIC_SOURCE,
		message: cleanMessage(message),
	}
}

/**
 * Recover a range from a sass diagnostic. `grass` formats errors with a trailing
 * `./stdin:<line>:<col>` locator (1-based) relative to the style block's CSS
 * body; the body's offset within the document is found by locating the matching
 * `<style …>` region so the range lands on the real source text.
 */
function sassRange(
	message: string,
	document: DiagnosticDocument,
	lineIndex: LineIndex,
): Range | undefined {
	if (!message.startsWith('sass:')) {
		return undefined
	}
	const match = /(?:^|\s)(?:\.\/stdin|stdin|[^\s:]+):(\d+):(\d+)\s*$/m.exec(message)
	if (!match) {
		return undefined
	}
	const line = Number.parseInt(match[1]!, 10) - 1
	const column = Number.parseInt(match[2]!, 10) - 1
	if (!Number.isFinite(line) || !Number.isFinite(column) || line < 0 || column < 0) {
		return undefined
	}

	// Locate the CSS body start: just after the first `<style …>` opening tag.
	const styleOpen = /<style\b[^>]*>/i.exec(document.text)
	const bodyStart = styleOpen ? styleOpen.index + styleOpen[0].length : 0
	const bodyStartPos = lineIndex.positionAt(bodyStart)

	// The locator is relative to the CSS body; line 0 stays on the body's line.
	const start: Position =
		line === 0
			? { line: bodyStartPos.line, character: bodyStartPos.character + column }
			: { line: bodyStartPos.line + line, character: column }
	return { start, end: { line: start.line, character: start.character + 1 } }
}

/**
 * Choose the fallback diagnostic range: the start of the embedded TypeScript
 * region (its first source mapping) when available, otherwise the document
 * start. The range spans a single character so editors render a visible marker.
 */
function fallbackRange(
	document: DiagnosticDocument,
	rootVirtualCode: VirtualCode | undefined,
	lineIndex: LineIndex,
): Range {
	const offset = embeddedTsStart(rootVirtualCode) ?? 0
	const start = lineIndex.positionAt(offset)
	const end = lineIndex.positionAt(Math.min(offset + 1, document.text.length))
	return { start, end }
}

/** First source offset covered by the embedded TypeScript code, if any. */
function embeddedTsStart(rootVirtualCode: VirtualCode | undefined): number | undefined {
	const embedded = rootVirtualCode?.embeddedCodes?.find((c) => c.id === EMBEDDED_TS_ID)
	if (!embedded) {
		return undefined
	}
	let min: number | undefined
	for (const mapping of embedded.mappings as CodeMapping[]) {
		for (const offset of mapping.sourceOffsets) {
			if (min === undefined || offset < min) {
				min = offset
			}
		}
	}
	return min
}

/** Strip a redundant trailing locator line from a sass message for display. */
function cleanMessage(message: string): string {
	return message.replace(/\s+$/, '')
}
