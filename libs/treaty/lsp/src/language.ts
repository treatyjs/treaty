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
import { typeScriptRegions } from './regions.js'

/**
 * Stable id of the embedded TypeScript virtual code carried by every Treaty
 * root virtual code. The diagnostics layer looks the embedded code up by this
 * id to map compiler ranges back through its {@link VirtualCode.mappings}.
 */
export const EMBEDDED_TS_ID = 'ts'

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
 * Build the volarjs {@link VirtualCode} for a `.treaty` single-file component.
 *
 * The root code maps the whole source 1:1 (so the document is recognized and
 * formatting/structure features see the original text), and carries a single
 * embedded TypeScript code that concatenates the *TypeScript-by-default*
 * regions (everything outside HTML tags, `<style>` blocks, `{{ … }}`
 * interpolations, `@`-control-flow markers, and a leading macro fence). Each
 * concatenated region carries a {@link CodeMapping} back to its original span,
 * so positions reported by the TS service — and by the Rust compiler — map
 * back to the source faithfully.
 */
export function createTreatyVirtualCode(
	languageId: string,
	snapshot: IScriptSnapshot,
): VirtualCode {
	const length = snapshot.getLength()
	const source = snapshot.getText(0, length)

	const regions = typeScriptRegions(source)
	let generated = ''
	const tsMappings: CodeMapping[] = []
	for (const region of regions) {
		const chunk = source.slice(region.start, region.end)
		tsMappings.push({
			sourceOffsets: [region.start],
			generatedOffsets: [generated.length],
			lengths: [chunk.length],
			data: FULL_CODE_INFORMATION,
		})
		// Separate concatenated chunks with a newline so token boundaries from
		// distinct source regions never fuse into one identifier in the TS view.
		generated += chunk + '\n'
	}

	const embedded: VirtualCode = {
		id: EMBEDDED_TS_ID,
		languageId: 'typescript',
		snapshot: new StringSnapshot(generated),
		mappings: tsMappings,
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
		byLanguageId.set(plugin.languageId, plugin)
	}

	return {
		getLanguageId(scriptId: T): string | undefined {
			return resolveByExtension(toFileName(scriptId))?.languageId
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
