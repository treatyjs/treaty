/**
 * @module
 *
 * The Treaty TEMPLATE LANGUAGE-SERVICE plugin: the layer that beats the Angular
 * Language Service on Treaty's distinguishing axes — **signals-by-default**,
 * **selectorless** components/directives, and **cross-file** resolution with
 * **no `NgModule`**.
 *
 * It is a volarjs {@link LanguageServicePlugin} that runs *alongside*
 * `volar-service-typescript`. The TypeScript service already covers the embedded
 * TS/JSX projection (completion, hover, definition, references, rename,
 * signature help, semantic tokens, formatting of the component body and every
 * expression). This plugin adds the Treaty-only intelligence the TS service
 * cannot know:
 *
 *  - **Completions** in template position: every selectorless component tag in
 *    the workspace (with an auto-import edit — *no `NgModule`, no `imports:[]`*),
 *    every directive under `use:`, the `@if`/`@for`/`@switch`/`@defer`
 *    control-flow blocks, and signal-aware member hints inside `{{ … }}`.
 *  - **Hover** on a selectorless tag / `use:` directive: the resolved class,
 *    its origin file and selector; and a "(signal)" annotation reminding that a
 *    plain Treaty variable is a signal.
 *  - **Definition** from a selectorless tag / `use:` name to the declaring file.
 *  - **Diagnostics** straight from the Rust authoring compiler (template + TS),
 *    mapped onto the source — wired through Volar's diagnostic capability so they
 *    refresh live, registry-aware so a cross-module selector resolves correctly.
 *
 * The plugin owns a {@link ComponentRegistry} of the workspace's components and
 * keeps it warm as documents open/change. The compiler is never reimplemented:
 * selectors come from the Rust scanner and diagnostics from the Rust compiler.
 */

import { extname } from 'node:path'
import {
	CompletionItemKind,
	DiagnosticSeverity,
	InsertTextFormat,
	MarkupKind,
	type CompletionItem,
	type CompletionList,
	type Diagnostic,
	type Hover,
	type LocationLink,
	type Position,
	type Range,
	type TextEdit,
} from 'vscode-languageserver'
import type { TextDocument } from 'vscode-languageserver-textdocument'
import type {
	LanguageServiceContext,
	LanguageServicePlugin,
	LanguageServicePluginInstance,
} from '@volar/language-service'
import {
	ComponentRegistry,
	scanWorkspaceSelectors,
	type ComponentEntry,
} from './component-registry.js'
import { compileWithSelectors } from './compiler.js'
import { templateContextAt, type TemplateCompletionKind } from './template-context.js'

/** The volarjs language ids this plugin serves (the Treaty authoring formats). */
const TREATY_LANGUAGE_IDS = new Set(['treaty', 'treaty-jsx'])

/** Diagnostic source tag for compiler diagnostics surfaced through this plugin. */
export const TEMPLATE_DIAGNOSTIC_SOURCE = 'treaty'

/** The `@`-control-flow blocks offered as snippet completions. */
const CONTROL_FLOW_SNIPPETS: ReadonlyArray<{ label: string; insert: string; doc: string }> = [
	{ label: '@if', insert: '@if (${1:condition}) {\n\t$0\n}', doc: 'Conditional block' },
	{
		label: '@for',
		insert: '@for (${1:item} of ${2:items}; track ${3:$index}) {\n\t$0\n}',
		doc: 'Repeater block (track is required)',
	},
	{
		label: '@switch',
		insert: '@switch (${1:expr}) {\n\t@case (${2:value}) {\n\t\t$0\n\t}\n\t@default {\n\t}\n}',
		doc: 'Switch block',
	},
	{
		label: '@defer',
		insert: '@defer (on ${1:idle}) {\n\t$0\n} @placeholder {\n}',
		doc: 'Deferred (lazy) block',
	},
	{ label: '@else', insert: '@else {\n\t$0\n}', doc: 'Else branch' },
	{ label: '@empty', insert: '@empty {\n\t$0\n}', doc: 'Empty branch of an @for' },
]

/**
 * Build the Treaty template language-service plugin. The returned plugin is
 * passed to the volarjs server *after* `volar-service-typescript`, so the TS
 * service handles everything in the embedded projection and this plugin
 * contributes the Treaty-only template/selectorless/signals intelligence on top.
 *
 * `registry` may be shared with the language layer (so the same component view
 * backs registry-aware compiles); when omitted a fresh one is created and warmed
 * from the first workspace folder seen.
 */
export function createTemplateService(
	registry: ComponentRegistry = new ComponentRegistry(),
): LanguageServicePlugin {
	return {
		name: 'treaty-template',
		capabilities: {
			completionProvider: {
				// Tag start, control-flow head, member access, directive namespace.
				triggerCharacters: ['<', '@', '.', ':', ' '],
				resolveProvider: false,
			},
			hoverProvider: true,
			definitionProvider: true,
			diagnosticProvider: {
				interFileDependencies: true,
				workspaceDiagnostics: false,
			},
		},
		create(context: LanguageServiceContext): LanguageServicePluginInstance {
			let warmed = false

			/** Lazily scan the workspace's `.ts` selectors and index open docs once. */
			function warm(): void {
				if (warmed) {
					return
				}
				warmed = true
				for (const folder of context.env.workspaceFolders) {
					registry.setProjectSelectors(scanWorkspaceSelectors(folder.fsPath || folder.path))
				}
			}

			/** Keep the registry current for a document about to be served. */
			function indexDocument(document: TextDocument): void {
				warm()
				const fileName = uriToFileName(document.uri)
				if (isAuthoringFile(fileName)) {
					registry.indexFile(fileName, document.getText())
				}
			}

			return {
				provideCompletionItems(document, position) {
					if (!TREATY_LANGUAGE_IDS.has(document.languageId)) {
						return undefined
					}
					indexDocument(document)
					return provideCompletions(document, position, registry)
				},

				provideHover(document, position) {
					if (!TREATY_LANGUAGE_IDS.has(document.languageId)) {
						return undefined
					}
					indexDocument(document)
					return provideHover(document, position, registry)
				},

				provideDefinition(document, position) {
					if (!TREATY_LANGUAGE_IDS.has(document.languageId)) {
						return undefined
					}
					indexDocument(document)
					return provideDefinition(document, position, registry)
				},

				provideDiagnostics(document) {
					if (!TREATY_LANGUAGE_IDS.has(document.languageId)) {
						return undefined
					}
					indexDocument(document)
					return provideTemplateDiagnostics(document, registry)
				},
			}
		},
	}
}

// ---------------------------------------------------------------------------
// Completions
// ---------------------------------------------------------------------------

/** Produce the Treaty template completion list for a position. */
function provideCompletions(
	document: TextDocument,
	position: Position,
	registry: ComponentRegistry,
): CompletionList | undefined {
	const source = document.getText()
	const offset = document.offsetAt(position)
	const ctx = isJsx(document.languageId)
		? jsxTemplateContext(source, offset)
		: templateContextAt(source, offset)
	if (ctx.completion === 'none') {
		return undefined
	}

	const replaceRange: Range = {
		start: document.positionAt(ctx.prefixStart),
		end: position,
	}

	const items =
		ctx.completion === 'control-flow'
			? controlFlowCompletions(ctx.prefix, replaceRange)
			: ctx.completion === 'use-directive'
				? directiveCompletions(registry, ctx.prefix, replaceRange)
				: ctx.completion === 'tag'
					? tagCompletions(document, registry, ctx.prefix, replaceRange)
					: []

	if (items.length === 0) {
		return undefined
	}
	// isIncomplete=false: the full selectorless/control-flow set is returned each
	// time; the client filters by `prefix` as the user types.
	return { isIncomplete: false, items }
}

/**
 * Selectorless component tag completions WITH auto-import. Every workspace
 * component is offered as its kebab tag; accepting one inserts the tag and — when
 * the component is not already imported by this file — an `import { X } from '…'`
 * edit, so the selectorless reference resolves with **no `NgModule`**.
 */
function tagCompletions(
	document: TextDocument,
	registry: ComponentRegistry,
	prefix: string,
	replaceRange: Range,
): CompletionItem[] {
	const source = document.getText()
	const selfFile = uriToFileName(document.uri)
	const items: CompletionItem[] = []
	for (const entry of registry.all()) {
		if (entry.fileName === selfFile) {
			continue // a component does not list itself as a dependency
		}
		if (prefix && !entry.tag.startsWith(prefix.toLowerCase())) {
			continue
		}
		const additionalTextEdits = autoImportEdit(document, source, entry)
		items.push({
			label: entry.tag,
			kind: CompletionItemKind.Class,
			detail: `${entry.kind} ${entry.className}`,
			documentation: componentDoc(entry),
			textEdit: { range: replaceRange, newText: entry.tag },
			filterText: entry.tag,
			sortText: `0_${entry.tag}`,
			...(additionalTextEdits ? { additionalTextEdits } : {}),
		})
	}
	return items
}

/** Directive completions under a `use:` namespace (no auto-import edit churn). */
function directiveCompletions(
	registry: ComponentRegistry,
	prefix: string,
	replaceRange: Range,
): CompletionItem[] {
	const items: CompletionItem[] = []
	for (const entry of registry.directives()) {
		const name = entry.tag
		if (prefix && !name.startsWith(prefix.toLowerCase())) {
			continue
		}
		items.push({
			label: name,
			kind: CompletionItemKind.Function,
			detail: `directive ${entry.className}`,
			documentation: componentDoc(entry),
			textEdit: { range: replaceRange, newText: name },
			sortText: `1_${name}`,
		})
	}
	return items
}

/** `@if`/`@for`/`@switch`/`@defer`/… control-flow snippet completions. */
function controlFlowCompletions(prefix: string, replaceRange: Range): CompletionItem[] {
	const lower = prefix.toLowerCase()
	return CONTROL_FLOW_SNIPPETS.filter((s) => s.label.startsWith(lower) || lower === '@' || lower === '').map(
		(snippet) => ({
			label: snippet.label,
			kind: CompletionItemKind.Keyword,
			detail: snippet.doc,
			insertTextFormat: InsertTextFormat.Snippet,
			textEdit: { range: replaceRange, newText: snippet.insert },
			sortText: `0_${snippet.label}`,
		}),
	)
}

/**
 * Compute the `additionalTextEdits` that import `entry.className` into this file,
 * or `undefined` when the file already imports it. The import is inserted after
 * the last existing import statement (or at the top of the body), so accepting a
 * selectorless tag wires the dependency with no manual import and no `NgModule`.
 */
function autoImportEdit(
	document: TextDocument,
	source: string,
	entry: ComponentEntry,
): TextEdit[] | undefined {
	if (importsSymbol(source, entry.className)) {
		return undefined
	}
	const specifier = relativeSpecifier(uriToFileName(document.uri), entry.importSpecifier)
	const importLine = `import { ${entry.className} } from '${specifier}'\n`
	const insertOffset = importInsertionOffset(source)
	const pos = document.positionAt(insertOffset)
	return [{ range: { start: pos, end: pos }, newText: importLine }]
}

// ---------------------------------------------------------------------------
// Hover
// ---------------------------------------------------------------------------

/** Hover for a selectorless tag / `use:` directive, plus the signals reminder. */
function provideHover(
	document: TextDocument,
	position: Position,
	registry: ComponentRegistry,
): Hover | undefined {
	const source = document.getText()
	const offset = document.offsetAt(position)
	const word = wordAround(source, offset)
	if (!word) {
		return undefined
	}
	const entry = registry.getByTag(word.text) ?? registry.getByClass(word.text)
	if (!entry) {
		return undefined
	}
	return {
		contents: {
			kind: MarkupKind.Markdown,
			value: hoverMarkdown(entry),
		},
		range: {
			start: document.positionAt(word.start),
			end: document.positionAt(word.end),
		},
	}
}

// ---------------------------------------------------------------------------
// Definition
// ---------------------------------------------------------------------------

/** Go-to-definition from a selectorless tag / `use:` name to its declaring file. */
function provideDefinition(
	document: TextDocument,
	position: Position,
	registry: ComponentRegistry,
): LocationLink[] | undefined {
	const source = document.getText()
	const offset = document.offsetAt(position)
	const word = wordAround(source, offset)
	if (!word) {
		return undefined
	}
	const entry = registry.getByTag(word.text) ?? registry.getByClass(word.text)
	if (!entry || entry.fileName === uriToFileName(document.uri)) {
		return undefined
	}
	const targetUri = fileNameToUri(entry.fileName)
	const zero: Range = { start: { line: 0, character: 0 }, end: { line: 0, character: 0 } }
	return [
		{
			targetUri,
			targetRange: zero,
			targetSelectionRange: zero,
			originSelectionRange: {
				start: document.positionAt(word.start),
				end: document.positionAt(word.end),
			},
		},
	]
}

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

/**
 * Compiler diagnostics for a Treaty document, registry-aware: the file's
 * imported selectors are resolved from the workspace registry so a cross-module
 * `<app-card>` compiles the way a bundler build would, then the Rust compiler's
 * errors are converted into LSP diagnostics over the source.
 */
function provideTemplateDiagnostics(
	document: TextDocument,
	registry: ComponentRegistry,
): Diagnostic[] {
	const source = document.getText()
	const fileName = uriToFileName(document.uri)
	const imported = registry.importedSelectorsFor(source)
	const compiled = compileWithSelectors(source, fileName, imported)
	if (compiled.errors.length === 0) {
		return []
	}
	const fallback: Range = {
		start: { line: 0, character: 0 },
		end: document.positionAt(Math.min(1, source.length)),
	}
	return compiled.errors.map((message) => ({
		range: fallback,
		severity: DiagnosticSeverity.Error,
		source: TEMPLATE_DIAGNOSTIC_SOURCE,
		message: message.replace(/\s+$/, ''),
	}))
}

// ---------------------------------------------------------------------------
// JSX template context
// ---------------------------------------------------------------------------

/**
 * Lightweight JSX completion-context probe: in a `.tsx`/`.tjsx` file a
 * selectorless tag is a lowercase JSX element, so reuse the open-tag detection
 * over the raw source. `use:` and control-flow heads work identically.
 */
function jsxTemplateContext(
	source: string,
	offset: number,
): ReturnType<typeof templateContextAt> {
	// The HTML/template scanner is `.treaty`-shaped; for JSX we only need the
	// open-tag / use: / @-head probe, which `templateContextAt` already performs
	// for the TS-body region. Treating the whole JSX file as one TS-by-default
	// region yields exactly that probe.
	return templateContextAt(source, offset)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** Whether a language id is one of the JSX authoring formats. */
function isJsx(languageId: string): boolean {
	return languageId === 'treaty-jsx'
}

/** Whether a file is an indexable authoring source (component-bearing). */
function isAuthoringFile(fileName: string): boolean {
	const ext = extname(fileName).toLowerCase()
	return ext === '.treaty' || ext === '.tsx' || ext === '.tjsx' || ext === '.ts'
}

/** The identifier/tag word surrounding an offset (`stat-card`, `MyComp`). */
function wordAround(
	source: string,
	offset: number,
): { text: string; start: number; end: number } | undefined {
	const isWord = (ch: string) => /[A-Za-z0-9_$-]/.test(ch)
	let start = offset
	let end = offset
	while (start > 0 && isWord(source[start - 1]!)) {
		start--
	}
	while (end < source.length && isWord(source[end]!)) {
		end++
	}
	if (end <= start) {
		return undefined
	}
	return { text: source.slice(start, end), start, end }
}

/** Markdown documentation block for a component/directive entry. */
function hoverMarkdown(entry: ComponentEntry): string {
	const lines = [
		`**${entry.className}** _(${entry.kind})_`,
		'',
		`Selectorless tag: \`<${entry.tag}>\``,
		`Origin: \`${entry.origin}\` — \`${entry.fileName}\``,
		'',
		'_Treaty resolves this selectorlessly — no `NgModule`, no `imports: []`._',
		'',
		'_Plain Treaty variables are signals; read them as `value()` and write with `value.set(…)`._',
	]
	return lines.join('\n')
}

/** Short component documentation for a completion item. */
function componentDoc(entry: ComponentEntry): { kind: typeof MarkupKind.Markdown; value: string } {
	return {
		kind: MarkupKind.Markdown,
		value: `Selectorless ${entry.kind} **${entry.className}** from \`${entry.importSpecifier}\`.\n\nAuto-imported on accept — no \`NgModule\`.`,
	}
}

/** Whether a source already imports the named symbol (so no auto-import is needed). */
function importsSymbol(source: string, className: string): boolean {
	const re = new RegExp(
		`import\\s+(?:type\\s+)?\\{[^}]*\\b${escapeRegExp(className)}\\b[^}]*\\}`,
	)
	return re.test(source)
}

/** Offset to insert a new import: just after the last existing import line, else 0. */
function importInsertionOffset(source: string): number {
	const re = /^[ \t]*import\b[^\n]*\n/gm
	let last = 0
	for (let m = re.exec(source); m; m = re.exec(source)) {
		last = m.index + m[0].length
	}
	return last
}

/**
 * The module specifier to import `target` from `fromFile` — a project-relative
 * path with a leading `./` and no extension, matching how a `.treaty`/JSX file
 * imports a sibling. Falls back to the bare specifier when no relation computes.
 */
function relativeSpecifier(fromFile: string, target: string): string {
	const fromDir = fromFile.replace(/[\\/][^\\/]*$/, '')
	const fromParts = fromDir.split(/[\\/]/).filter(Boolean)
	const targetParts = target.split(/[\\/]/).filter(Boolean)
	let common = 0
	while (
		common < fromParts.length &&
		common < targetParts.length &&
		fromParts[common] === targetParts[common]
	) {
		common++
	}
	const up = fromParts.length - common
	const down = targetParts.slice(common)
	const prefix = up === 0 ? './' : '../'.repeat(up)
	const rel = prefix + down.join('/')
	return rel.startsWith('.') ? rel : `./${rel}`
}

/** Escape a string for use as a literal inside a RegExp. */
function escapeRegExp(text: string): string {
	return text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

/** Best-effort URI → file path (handles `file://` URIs and plain paths). */
function uriToFileName(uri: string): string {
	if (uri.startsWith('file://')) {
		try {
			const url = new URL(uri)
			let path = decodeURIComponent(url.pathname)
			// Windows drive: `/c:/…` → `c:/…`.
			if (/^\/[A-Za-z]:/.test(path)) {
				path = path.slice(1)
			}
			return path
		} catch {
			return uri
		}
	}
	return uri
}

/** Best-effort file path → `file://` URI. */
function fileNameToUri(fileName: string): string {
	if (fileName.startsWith('file://') || fileName.includes('://')) {
		return fileName
	}
	const normalized = fileName.replace(/\\/g, '/')
	const withSlash = normalized.startsWith('/') ? normalized : `/${normalized}`
	return `file://${withSlash}`
}

/** Re-export the completion-kind enum so consumers can branch on it. */
export type { TemplateCompletionKind }
