/**
 * @module
 *
 * Cursor-context analysis over a `.treaty` source: given an offset, decide which
 * authoring REGION the cursor is in and what kind of template intelligence
 * applies there (a tag-name completion, an attribute, a `use:` directive, a
 * control-flow block, an interpolation, …).
 *
 * This is the front-half of the Treaty {@link import('./template-service.js')
 * language-service plugin}: it never produces edits, it only classifies a
 * position so the service can offer the right completions / hover. It reuses the
 * lexical {@link scanTreatyRegions region scanner} so its notion of "inside the
 * template" is exactly the compiler's, then refines the local syntax (open tag,
 * attribute, control-flow head) with a tiny backward scan.
 */

import { scanTreatyRegions, type TreatyRegion } from './regions.js'

/** The kind of template completion a position invites. */
export type TemplateCompletionKind =
	/** A tag name is being typed (`<pa|` or `<|`): offer selectorless components. */
	| 'tag'
	/** A `use:` directive name (`use:hi|`): offer directives. */
	| 'use-directive'
	/** An `@`-control-flow head (`@i|`): offer `@if`/`@for`/… blocks. */
	| 'control-flow'
	/** Inside a `{{ … }}` interpolation: offer component-scope members. */
	| 'interpolation'
	/** Not a template-completion position (plain TS body, style, …). */
	| 'none'

/** The classified context at a cursor offset within a `.treaty` source. */
export interface TemplateContext {
	/** The region the offset falls in. */
	readonly region: TreatyRegion
	/** The completion kind that applies at the offset. */
	readonly completion: TemplateCompletionKind
	/**
	 * The partial word already typed at the cursor (the tag/directive/keyword
	 * prefix), used to filter and to compute the replacement range. Empty when
	 * the cursor is at a fresh position.
	 */
	readonly prefix: string
	/** Source offset where {@link prefix} begins (the replacement-range start). */
	readonly prefixStart: number
}

/** Find the region containing `offset` (the last region whose span covers it). */
export function regionAt(source: string, offset: number): TreatyRegion | undefined {
	const regions = scanTreatyRegions(source)
	for (const region of regions) {
		if (offset >= region.start && offset <= region.end) {
			// Prefer a non-`ts` region when offset sits exactly on a boundary so an
			// interpolation/template edge classifies as template, not body TS.
			if (offset < region.end || region.kind !== 'ts') {
				return region
			}
		}
	}
	// Fall back to the region the offset is within, inclusive of the end edge.
	return regions.find((r) => offset >= r.start && offset <= r.end)
}

/**
 * Classify the completion context at `offset` in a `.treaty` source.
 *
 * The region scan decides the coarse location; a short backward scan over the
 * characters before the cursor refines it into a tag / `use:` / control-flow /
 * interpolation context and recovers the partial word being typed.
 */
export function templateContextAt(source: string, offset: number): TemplateContext {
	const region = regionAt(source, offset) ?? {
		kind: 'ts',
		start: 0,
		end: source.length,
	}

	// Control-flow heads and interpolations are their own regions.
	if (region.kind === 'control-flow') {
		const word = wordBefore(source, offset, /[@A-Za-z]/)
		return {
			region,
			completion: 'control-flow',
			prefix: word.text,
			prefixStart: word.start,
		}
	}
	if (region.kind === 'expression') {
		const word = wordBefore(source, offset, /[\w$.]/)
		return {
			region,
			completion: 'interpolation',
			prefix: word.text,
			prefixStart: word.start,
		}
	}

	if (region.kind === 'html') {
		return refineHtmlContext(source, offset, region)
	}

	// A bare `@` typed in the TS body at a statement boundary is the start of a
	// control-flow block the lexer has not yet split into its own region.
	if (region.kind === 'ts') {
		const atWord = controlFlowHeadBefore(source, offset)
		if (atWord) {
			return {
				region,
				completion: 'control-flow',
				prefix: atWord.text,
				prefixStart: atWord.start,
			}
		}
		// A `<` typed in the body opens a template tag (TS-by-default → template).
		const tag = openTagBefore(source, offset)
		if (tag) {
			return {
				region,
				completion: 'tag',
				prefix: tag.text,
				prefixStart: tag.start,
			}
		}
	}

	return { region, completion: 'none', prefix: '', prefixStart: offset }
}

/** Refine the completion context for an offset inside an HTML template region. */
function refineHtmlContext(
	source: string,
	offset: number,
	region: TreatyRegion,
): TemplateContext {
	// `use:` directive application: `use:hi|`.
	const useWord = useDirectiveBefore(source, offset)
	if (useWord) {
		return {
			region,
			completion: 'use-directive',
			prefix: useWord.text,
			prefixStart: useWord.start,
		}
	}

	// A control-flow head typed in the template body: `@i|` between tags. These
	// live inside the HTML region until a full keyword is recognized, so detect a
	// partial `@`-word here too.
	const atWord = controlFlowHeadBefore(source, offset)
	if (atWord) {
		return {
			region,
			completion: 'control-flow',
			prefix: atWord.text,
			prefixStart: atWord.start,
		}
	}

	// An open tag name: `<pa|` or `<|`.
	const tag = openTagBefore(source, offset)
	if (tag) {
		return {
			region,
			completion: 'tag',
			prefix: tag.text,
			prefixStart: tag.start,
		}
	}

	return { region, completion: 'none', prefix: '', prefixStart: offset }
}

/** A word recovered by a backward scan: its text and start offset. */
interface Word {
	readonly text: string
	readonly start: number
}

/** Scan backward from `offset` collecting characters matching `charClass`. */
function wordBefore(source: string, offset: number, charClass: RegExp): Word {
	let start = offset
	while (start > 0 && charClass.test(source[start - 1]!)) {
		start--
	}
	return { text: source.slice(start, offset), start }
}

/**
 * Recover an open-tag prefix when the cursor sits in tag-name position: a `<`
 * immediately followed (back to the cursor) by tag-name characters, with no
 * intervening `>` / whitespace that would close or end the tag name. Returns the
 * partial tag name (possibly empty for a bare `<|`) and the offset just after
 * the `<`, or `undefined` when the cursor is not in tag-name position.
 */
function openTagBefore(source: string, offset: number): Word | undefined {
	let i = offset
	while (i > 0) {
		const ch = source[i - 1]!
		if (/[A-Za-z0-9-]/.test(ch)) {
			i--
			continue
		}
		if (ch === '<') {
			return { text: source.slice(i, offset), start: i }
		}
		return undefined
	}
	return undefined
}

/** Recover a `use:<name>` directive prefix when the cursor is on the name. */
function useDirectiveBefore(source: string, offset: number): Word | undefined {
	const word = wordBefore(source, offset, /[A-Za-z0-9-]/)
	const before = word.start
	// Require the literal `use:` immediately before the name.
	if (before >= 4 && source.slice(before - 4, before) === 'use:') {
		return word
	}
	return undefined
}

/** Recover an `@`-control-flow head (`@if`, `@fo…`) at a statement-ish boundary. */
function controlFlowHeadBefore(source: string, offset: number): Word | undefined {
	const word = wordBefore(source, offset, /[A-Za-z]/)
	const at = word.start - 1
	if (at >= 0 && source[at] === '@') {
		// Only treat it as control flow when the `@` is not a decorator: a decorator
		// `@` is preceded by a newline + indentation and followed by a known
		// decorator name. We keep it permissive — the service still offers the
		// control-flow snippets, which a user can ignore in decorator position.
		return { text: '@' + word.text, start: at }
	}
	return undefined
}
