/**
 * @module
 *
 * Adapts the {@link AuthoringLanguagePlugin} registry into the volarjs
 * {@link LanguagePlugin} that the language server consumes. A single adapter
 * fans out to every registered authoring format: `getLanguageId` routes by file
 * extension and `createVirtualCode` delegates to the format that owns the
 * resolved `languageId`.
 *
 * The adapter is generic over the volarjs script-id type (`T`). The server
 * runs with `T = URI`, while in-process / TS-plugin callers may use `string`.
 * A `scriptIdToFileName` mapper turns the opaque id into a path so the registry
 * can resolve the owning format by extension.
 */

import type {
	CodegenContext,
	CodeInformation,
	CodeMapping,
	IScriptSnapshot,
	LanguagePlugin,
	VirtualCode,
} from '@volar/language-core'
import {
	listAuthoringLanguages,
	resolveByExtension,
	type AuthoringLanguagePlugin,
} from './plugins.js'
import {
	findInterpolationsIn,
	interpolationInner,
	scanTreatyRegions,
	styleInner,
	type TreatyRegion,
} from './regions.js'

/**
 * Stable id of the embedded TypeScript virtual code carried by every Treaty
 * root virtual code. The diagnostics layer looks the embedded code up by this
 * id to map compiler ranges back through its {@link VirtualCode.mappings}.
 */
export const EMBEDDED_TS_ID = 'ts'

/**
 * Stable id of the embedded Angular-template (HTML) virtual code carried by an
 * external `.html` Angular template's root virtual code.
 */
export const EMBEDDED_HTML_ID = 'html'

/**
 * Id prefix of an embedded CSS virtual code carried by a `.treaty` root virtual
 * code for each `<style>` block, suffixed with the block's index (`style_0`,
 * `style_1`, …). The CSS language service serves completion/validation over
 * these so editing inside a `<style>` block behaves like editing CSS.
 */
export const EMBEDDED_CSS_ID_PREFIX = 'style_'

/**
 * Full {@link CodeInformation} capability set: the embedded TypeScript is a
 * faithful projection of the source spans, so every language feature
 * (verification, completion, semantic, navigation, structure, format) maps
 * through it.
 */
const FULL_CODE_INFORMATION: CodeInformation = {
	verification: true,
	completion: true,
	semantic: true,
	navigation: true,
	structure: true,
	format: true,
}

/**
 * Capabilities for an embedded `{{ … }}` interpolation expression projected into
 * the TypeScript code. Completion, hover (semantic), navigation and type
 * verification all map through — so TS completion works INSIDE the interpolation
 * against the component scope — but `format` and `structure` are off: the
 * interpolation is a fragment spliced into the body view, so letting the TS
 * formatter or document-symbol pass reach across the `{{ }}` boundary would
 * corrupt the source. The same fragment-safety the Angular/Vue template
 * projections use for interpolation expressions.
 */
const EXPRESSION_CODE_INFORMATION: CodeInformation = {
	verification: true,
	completion: true,
	semantic: true,
	navigation: true,
	structure: false,
	format: false,
}

/** A snapshot over a fixed string, used for generated embedded codes. */
class StringSnapshot implements IScriptSnapshot {
	private readonly text: string
	constructor(text: string) {
		this.text = text
	}
	getText(start: number, end: number): string {
		return this.text.slice(start, end)
	}
	getLength(): number {
		return this.text.length
	}
	getChangeRange(): undefined {
		return undefined
	}
}

/**
 * Build the volarjs {@link VirtualCode} for a `.treaty` single-file component as
 * a REGION-AWARE projection: each authoring region is projected into the
 * embedded language that owns it, so completion/hover/validation follow the
 * cursor.
 *
 * The root code maps the whole source 1:1 (so the document is recognized and
 * formatting/structure features see the original text, and the Treaty template
 * service can read the raw source for the template region), and carries:
 *
 *  - one embedded **TypeScript** code (`id: 'ts'`) that concatenates the
 *    *TypeScript-by-default* body regions AND the inner expression text of every
 *    `{{ … }}` interpolation, each with a {@link CodeMapping} back to its span.
 *    Because the interpolations share this single embedded code with the body,
 *    an identifier in `{{ count }}` resolves against the `const count` declared
 *    in the body — so the TypeScript service drives completion/hover INSIDE the
 *    interpolation, against the component scope; and
 *  - one embedded **CSS** code per `<style>` block (`id: 'style_<n>'`) covering
 *    the block's CSS body, so the CSS language service drives completion and
 *    validation inside the block.
 *
 * The HTML/control-flow template regions are intentionally NOT projected: the
 * Treaty template service serves them (selectorless tags, `use:` directives,
 * control-flow) directly over the root document, beating a plain HTML service.
 */
export function createTreatyVirtualCode(
	languageId: string,
	snapshot: IScriptSnapshot,
): VirtualCode {
	const length = snapshot.getLength()
	const source = snapshot.getText(0, length)

	const regions = scanTreatyRegions(source)
	const embeddedCodes: VirtualCode[] = [
		buildEmbeddedTs(source, regions),
		...buildEmbeddedStyles(source, regions),
	]

	return {
		id: 'root',
		languageId,
		snapshot,
		mappings: [wholeDocumentMapping(length)],
		embeddedCodes,
	}
}

/**
 * Build the embedded TypeScript code: the TS-by-default body regions plus the
 * inner expression text of each `{{ … }}` interpolation, concatenated (each
 * chunk newline-separated so distinct source spans never fuse into one token)
 * and each carrying a {@link CodeMapping} back to its source span.
 */
function buildEmbeddedTs(source: string, regions: readonly TreatyRegion[]): VirtualCode {
	let generated = ''
	const tsMappings: CodeMapping[] = []
	const push = (start: number, end: number, data: CodeInformation): void => {
		if (end <= start) {
			return
		}
		tsMappings.push({
			sourceOffsets: [start],
			generatedOffsets: [generated.length],
			lengths: [end - start],
			data,
		})
		generated += source.slice(start, end) + '\n'
	}
	for (const region of regions) {
		if (region.kind === 'ts') {
			push(region.start, region.end, FULL_CODE_INFORMATION)
			continue
		}
		if (region.kind === 'expression') {
			// A standalone `{{ … }}` region (rare — an interpolation not inside an
			// HTML tag): project its inner expression as TypeScript.
			const inner = interpolationInner(source, region)
			if (inner) {
				push(inner.start, inner.end, EXPRESSION_CODE_INFORMATION)
			}
			continue
		}
		if (region.kind === 'html' || region.kind === 'control-flow') {
			// Interpolations almost always live INSIDE an HTML region
			// (`<div>{{ count }}</div>` is one `html` region) or a control-flow head
			// (`@if (cond)`). Project each `{{ … }}` inner expression as TypeScript so
			// member completion / hover work inside it, sharing the body scope.
			for (const inner of findInterpolationsIn(source, region)) {
				push(inner.start, inner.end, EXPRESSION_CODE_INFORMATION)
			}
		}
	}
	return {
		id: EMBEDDED_TS_ID,
		languageId: 'typescript',
		snapshot: new StringSnapshot(generated),
		mappings: tsMappings,
		embeddedCodes: [],
	}
}

/**
 * Build one embedded CSS code per `<style>` block, each covering the block's CSS
 * body (between the opening tag's `>` and the closing `</style>`) and mapped 1:1
 * so positions inside the block round-trip to the source. The CSS language
 * service serves these.
 */
function buildEmbeddedStyles(
	source: string,
	regions: readonly TreatyRegion[],
): VirtualCode[] {
	const codes: VirtualCode[] = []
	let index = 0
	for (const region of regions) {
		if (region.kind !== 'style') {
			continue
		}
		const inner = styleInner(source, region)
		if (!inner || inner.end <= inner.start) {
			index++
			continue
		}
		const body = source.slice(inner.start, inner.end)
		codes.push({
			id: `${EMBEDDED_CSS_ID_PREFIX}${index}`,
			languageId: 'css',
			snapshot: new StringSnapshot(body),
			mappings: [
				{
					sourceOffsets: [inner.start],
					generatedOffsets: [0],
					lengths: [body.length],
					data: FULL_CODE_INFORMATION,
				},
			],
			embeddedCodes: [],
		})
		index++
	}
	return codes
}

/**
 * Build the volarjs {@link VirtualCode} for a Treaty `.tsx` / `.tjsx`
 * component: the whole file is TypeScript + JSX, so the embedded code is the
 * entire source mapped 1:1 under the `typescriptreact` language id.
 */
export function createJsxVirtualCode(
	languageId: string,
	snapshot: IScriptSnapshot,
): VirtualCode {
	const length = snapshot.getLength()
	const embedded: VirtualCode = {
		id: EMBEDDED_TS_ID,
		languageId: 'typescriptreact',
		snapshot,
		mappings: [wholeDocumentMapping(length)],
		embeddedCodes: [],
	}
	return {
		id: 'root',
		languageId,
		snapshot,
		mappings: [wholeDocumentMapping(length)],
		embeddedCodes: [embedded],
	}
}

/**
 * Build the volarjs {@link VirtualCode} for an external Angular template
 * (`.html`). The whole file is an Angular HTML template, so the embedded code
 * is the entire source mapped 1:1 under the `html` language id (the closest
 * built-in volarjs/TS service language for an Angular template), giving it
 * syntax highlighting and — where the downstream service supports it — checks.
 */
export function createAngularHtmlVirtualCode(
	languageId: string,
	snapshot: IScriptSnapshot,
): VirtualCode {
	const length = snapshot.getLength()
	const embedded: VirtualCode = {
		id: EMBEDDED_HTML_ID,
		languageId: 'html',
		snapshot,
		mappings: [wholeDocumentMapping(length)],
		embeddedCodes: [],
	}
	return {
		id: 'root',
		languageId,
		snapshot,
		mappings: [wholeDocumentMapping(length)],
		embeddedCodes: [embedded],
	}
}

/**
 * Build the volarjs {@link VirtualCode} for a plain Angular component source
 * (`.ts`): the whole file is TypeScript, so the embedded code is the entire
 * source mapped 1:1 under the `typescript` language id, letting the standard
 * TypeScript service cover it directly.
 */
export function createAngularSourceVirtualCode(
	languageId: string,
	snapshot: IScriptSnapshot,
): VirtualCode {
	const length = snapshot.getLength()
	const embedded: VirtualCode = {
		id: EMBEDDED_TS_ID,
		languageId: 'typescript',
		snapshot,
		mappings: [wholeDocumentMapping(length)],
		embeddedCodes: [],
	}
	return {
		id: 'root',
		languageId,
		snapshot,
		mappings: [wholeDocumentMapping(length)],
		embeddedCodes: [embedded],
	}
}

/** A single mapping covering `[0, length)` 1:1 with full capabilities. */
function wholeDocumentMapping(length: number): CodeMapping {
	return {
		sourceOffsets: [0],
		generatedOffsets: [0],
		lengths: [length],
		data: FULL_CODE_INFORMATION,
	}
}

/** Options controlling how a {@link createTreatyLanguagePlugin} adapter behaves. */
export interface TreatyLanguagePluginOptions<T> {
	/**
	 * Turn a volarjs script id into a file path/name for extension resolution.
	 * Defaults to `String(scriptId)`, which is correct when `T = string` and for
	 * `URI` values whose `toString()` ends in the relevant extension.
	 */
	scriptIdToFileName?: (scriptId: T) => string
	/**
	 * Authoring plugins to serve; defaults to every plugin currently registered
	 * via {@link registerAuthoringLanguage}.
	 */
	plugins?: readonly AuthoringLanguagePlugin[]
}

/**
 * Build the volarjs {@link LanguagePlugin} backed by the authoring registry.
 */
export function createTreatyLanguagePlugin<T = string>(
	options: TreatyLanguagePluginOptions<T> = {}
): LanguagePlugin<T> {
	const plugins = options.plugins ?? listAuthoringLanguages()
	const toFileName = options.scriptIdToFileName ?? ((scriptId: T) => String(scriptId))

	const byLanguageId = new Map<string, AuthoringLanguagePlugin>()
	for (const plugin of plugins) {
		for (const id of plugin.languageIds ?? [plugin.languageId]) {
			byLanguageId.set(id, plugin)
		}
	}

	return {
		getLanguageId(scriptId: T): string | undefined {
			const fileName = toFileName(scriptId)
			const owner = resolveByExtension(fileName)
			if (!owner) {
				return undefined
			}
			return owner.languageIdFor?.(fileName) ?? owner.languageId
		},
		createVirtualCode(
			scriptId: T,
			languageId: string,
			snapshot: IScriptSnapshot,
			ctx: CodegenContext<T>
		): VirtualCode | undefined {
			const owner = byLanguageId.get(languageId)
			if (!owner) {
				return undefined
			}
			return owner.createVirtualCode(
				toFileName(scriptId),
				languageId,
				snapshot,
				ctx as unknown as CodegenContext<string>
			)
		},
	}
}
