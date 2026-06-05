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
import { readFileSync } from 'node:fs'
import { URI } from 'vscode-uri'
import {
	CompletionItemKind,
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
import { mapErrorsToDiagnostics, type DiagnosticDocument } from './diagnostics.js'
import {
	jsxContextAt,
	templateContextAt,
	type TemplateCompletionKind,
} from './template-context.js'
import type { VirtualCode } from '@volar/language-core'

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
				const fileName = uriToFileName(sourceUriOf(context, document.uri))
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
					return provideCompletions(document, position, registry, context)
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
					return provideDefinition(document, position, registry, context)
				},

				provideDiagnostics(document) {
					if (!TREATY_LANGUAGE_IDS.has(document.languageId)) {
						return undefined
					}
					indexDocument(document)
					return provideTemplateDiagnostics(document, registry, context)
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
	context: LanguageServiceContext,
): CompletionList | undefined {
	const source = document.getText()
	const offset = document.offsetAt(position)
	const ctx = isJsx(document.languageId)
		? jsxContextAt(source, offset)
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
				: ctx.completion === 'attribute'
					? attributeDirectiveCompletions(document, registry, ctx.prefix, replaceRange, context)
					: ctx.completion === 'tag'
						? tagCompletions(document, registry, ctx.prefix, replaceRange, context)
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
	context: LanguageServiceContext,
): CompletionItem[] {
	const source = document.getText()
	const selfFile = uriToFileName(sourceUriOf(context, document.uri))
	const items: CompletionItem[] = []
	for (const entry of registry.all()) {
		if (entry.fileName === selfFile) {
			continue // a component does not list itself as a dependency
		}
		if (prefix && !entry.tag.startsWith(prefix.toLowerCase())) {
			continue
		}
		const additionalTextEdits = autoImportEdit(document, source, entry, context)
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
		// Under `use:`, an attribute-selector directive is referenced by its
		// attribute name (`use:routerLink`), not the kebab class fold; a plain
		// selectorless directive uses its filename-fold tag.
		const name = entry.attributeSelector ?? entry.tag
		if (prefix && !name.toLowerCase().startsWith(prefix.toLowerCase())) {
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

/**
 * Bare attribute-selector directive completions WITH auto-import. Every
 * attribute-selector directive (built-in Angular like `routerLink`, plus any
 * imported `@Directive({ selector: '[x]' })`) is offered under its bare attribute
 * name — NO `use:` required, recognized via its attribute selector the way the
 * SelectorRegistry resolves it. Accepting one inserts the attribute and — when
 * the directive's class is not already imported — an import edit, so the bare
 * `routerLink` resolves with no `NgModule`. The documentation hints that `use:`
 * is valid but optional/redundant for a selector-bearing directive.
 */
function attributeDirectiveCompletions(
	document: TextDocument,
	registry: ComponentRegistry,
	prefix: string,
	replaceRange: Range,
	context: LanguageServiceContext,
): CompletionItem[] {
	const source = document.getText()
	const lower = prefix.toLowerCase()
	const items: CompletionItem[] = []
	for (const entry of registry.attributeDirectives()) {
		const name = entry.attributeSelector!
		if (lower && !name.toLowerCase().startsWith(lower)) {
			continue
		}
		const additionalTextEdits = autoImportEdit(document, source, entry, context)
		items.push({
			label: name,
			kind: CompletionItemKind.Property,
			detail: `directive ${entry.className} (selector [${name}])`,
			documentation: attributeDirectiveDoc(entry),
			textEdit: { range: replaceRange, newText: name },
			filterText: name,
			sortText: `0_${name}`,
			...(additionalTextEdits ? { additionalTextEdits } : {}),
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
	context: LanguageServiceContext,
): TextEdit[] | undefined {
	if (importsSymbol(source, entry.className)) {
		return undefined
	}
	// A built-in directive imports from a bare package specifier (`@angular/router`);
	// everything else imports from a project-relative sibling path.
	const specifier =
		entry.origin === 'builtin'
			? entry.importSpecifier
			: relativeSpecifier(uriToFileName(sourceUriOf(context, document.uri)), entry.importSpecifier)
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
	const wordRange: Range = {
		start: document.positionAt(word.start),
		end: document.positionAt(word.end),
	}

	// The `server` keyword that opens an inline `server { … }` / `server:lang { … }`
	// block: surface that the block runs on the server and is lifted out of the
	// client bundle.
	if (word.text === 'server' && opensServerBlock(source, word.start)) {
		return { contents: { kind: MarkupKind.Markdown, value: serverBlockHover(source, word.start) }, range: wordRange }
	}

	// A destructured-props input binding (`function C({ label, count = 5 })`): the
	// compiler lowers each destructured prop to a signal `input()`, so annotate the
	// hovered name as an input.
	if (isDestructuredInput(source, word.text)) {
		return { contents: { kind: MarkupKind.Markdown, value: destructuredInputHover(word.text) }, range: wordRange }
	}

	const entry =
		registry.getByTag(word.text) ??
		registry.getByClass(word.text) ??
		registry.getByAttribute(word.text)
	if (!entry) {
		return undefined
	}
	return {
		contents: {
			kind: MarkupKind.Markdown,
			value: hoverMarkdown(entry),
		},
		range: wordRange,
	}
}

/**
 * Whether the `server` word at `start` opens an inline `server { … }` /
 * `server:lang { … }` block: it must be a standalone identifier (not `x.server`
 * or `serverFoo`) followed by an optional `:lang` tag and then a `{`.
 */
function opensServerBlock(source: string, start: number): boolean {
	const before = source[start - 1]
	if (before !== undefined && /[.$\w]/.test(before)) {
		return false
	}
	const rest = source.slice(start + 'server'.length)
	// optional whitespace, optional `:ident`, optional whitespace, then `{`.
	return /^\s*(?::\s*[A-Za-z_]\w*\s*)?\{/.test(rest)
}

/** The `server { … }` hover, naming the language tag when present. */
function serverBlockHover(source: string, start: number): string {
	const rest = source.slice(start + 'server'.length)
	const tag = /^\s*:\s*([A-Za-z_]\w*)/.exec(rest)
	const lang = tag ? tag[1] : 'rust'
	return [
		'**`server` block** — _runs on the server_.',
		'',
		`Code inside this block is lifted OUT of the client bundle and emitted as a server module (target language: \`${lang}\`).`,
		'',
		'Each function inside becomes a callable endpoint; the client calls it over the wire. Secrets here never ship to the browser.',
	].join('\n')
}

/**
 * Whether `name` is a destructured-props input binding of the component function
 * — a name listed in a `function C({ a, b = 5 }: …)` / `(props: …) => …` object
 * destructuring of the FIRST parameter. The compiler lowers each such prop to a
 * signal `input()`, so the LSP annotates it as an input. Best-effort: a light
 * scan for the first `({ … })` parameter list and a word-boundary match of the
 * name inside it.
 */
function isDestructuredInput(source: string, name: string): boolean {
	if (!/^[A-Za-z_$][\w$]*$/.test(name)) {
		return false
	}
	for (const params of destructuredParamLists(source)) {
		// The destructured key is `name` at the start of the pattern or after a `,`
		// (allowing surrounding whitespace), optionally followed by `=default`,
		// `: alias` or `,`/end — match it as a property name, not a default-value
		// reference. `destructuredParamLists` returns the `{ … }` INTERIOR, so the
		// first key sits after leading whitespace at string start.
		const re = new RegExp(`(?:^|,)\\s*${escapeRegExp(name)}\\s*(?:[,=:]|$)`)
		if (re.test(params)) {
			return true
		}
	}
	return false
}

/**
 * The inner text of every component-function FIRST-parameter object pattern
 * `({ … })` in a source — the destructured-props parameter lists. Scans for
 * `function NAME({ … })` and `({ … }) =>` / `({ … }:` arrow forms.
 */
function destructuredParamLists(source: string): string[] {
	const out: string[] = []
	// `function Name ( { … } ` and `( { … } ) =>` — capture the brace interior.
	const re = /(?:function\s+[A-Za-z_$][\w$]*\s*\(|\(\s*)\{([^{}]*)\}/g
	for (let m = re.exec(source); m; m = re.exec(source)) {
		out.push(m[1]!)
	}
	return out
}

/** Hover for a destructured-props input binding. */
function destructuredInputHover(name: string): string {
	return [
		`**${name}** — _component \`input()\`_.`,
		'',
		'A destructured prop of the component function is lowered to a signal `input()`.',
		'',
		`Read it as \`${name}()\` (it is a signal). Bind it from a parent with \`<this-component ${name}="…" />\`.`,
	].join('\n')
}

// ---------------------------------------------------------------------------
// Definition
// ---------------------------------------------------------------------------

/** Go-to-definition from a selectorless tag / `use:` name to its declaring file. */
function provideDefinition(
	document: TextDocument,
	position: Position,
	registry: ComponentRegistry,
	context: LanguageServiceContext,
): LocationLink[] | undefined {
	const source = document.getText()
	const offset = document.offsetAt(position)
	const word = wordAround(source, offset)
	if (!word) {
		return undefined
	}
	const entry =
		registry.getByTag(word.text) ??
		registry.getByClass(word.text) ??
		registry.getByAttribute(word.text)
	// A built-in directive resolves to a bare package specifier, not a workspace
	// file, so there is no in-project declaration to navigate to.
	if (!entry || entry.origin === 'builtin' || entry.fileName === uriToFileName(sourceUriOf(context, document.uri))) {
		return undefined
	}
	const targetUri = fileNameToUri(entry.fileName)
	const { targetRange, selectionRange } = resolveTargetRanges(entry)
	return [
		{
			targetUri,
			targetRange,
			targetSelectionRange: selectionRange,
			originSelectionRange: {
				start: document.positionAt(word.start),
				end: document.positionAt(word.end),
			},
		},
	]
}

/**
 * Resolve the real go-to-definition ranges for a registry entry: the full span
 * of the declaration (`targetRange`) and the identifier to highlight
 * (`targetSelectionRange`). The declaring file is read and the class/symbol
 * located so navigation lands on the declaration rather than the file top.
 *
 *  - Angular `.ts` and JSX sources that export an explicit `class <Name>` jump
 *    to that class (selection range over the class name).
 *  - `.treaty`/JSX selectorless files have no explicit class (the filename is
 *    the component name); navigation lands on the first meaningful declaration —
 *    the file's first non-import, non-blank statement — or the file top when the
 *    source can't be read.
 */
function resolveTargetRanges(entry: ComponentEntry): {
	targetRange: Range
	selectionRange: Range
} {
	const zero: Range = { start: { line: 0, character: 0 }, end: { line: 0, character: 0 } }
	const source = readTargetSource(entry.fileName)
	if (source === undefined) {
		return { targetRange: zero, selectionRange: zero }
	}
	const lineIndex = new SourceLineIndex(source)
	const located = locateSymbol(source, entry.className) ?? locateFirstDeclaration(source)
	if (!located) {
		return { targetRange: zero, selectionRange: zero }
	}
	const selectionRange: Range = {
		start: lineIndex.positionAt(located.nameStart),
		end: lineIndex.positionAt(located.nameEnd),
	}
	const targetRange: Range = {
		start: lineIndex.positionAt(located.declStart),
		end: lineIndex.positionAt(located.declEnd),
	}
	return { targetRange, selectionRange }
}

/** A located declaration: the statement span plus the identifier span to select. */
interface LocatedSymbol {
	readonly declStart: number
	readonly declEnd: number
	readonly nameStart: number
	readonly nameEnd: number
}

/**
 * Locate an explicit `class <className>` (optionally `export`/`abstract`/`default`)
 * declaration in a source, returning the statement start and the class-name span.
 */
function locateSymbol(source: string, className: string): LocatedSymbol | undefined {
	const re = new RegExp(
		`(?:^|[\\n;])([ \\t]*(?:export\\s+)?(?:default\\s+)?(?:abstract\\s+)?class\\s+)(${escapeRegExp(className)})\\b`,
	)
	const m = re.exec(source)
	if (!m) {
		return undefined
	}
	// m.index points at the leading boundary (newline/`;`/start); skip it so the
	// declaration start sits on the first modifier/keyword. `m[1]` (the
	// modifiers + `class `) begins right after that consumed boundary char.
	const lead = m[0]!.startsWith('\n') || m[0]!.startsWith(';') ? 1 : 0
	const groupStart = m.index + lead
	const declStart = groupStart + leadingWhitespace(m[1]!)
	const nameStart = groupStart + m[1]!.length
	const nameEnd = nameStart + m[2]!.length
	return { declStart, declEnd: nameEnd, nameStart, nameEnd }
}

/**
 * Locate the first meaningful declaration in a selectorless `.treaty`/JSX source
 * that has no explicit `class`: the first non-blank line that is not an import,
 * a comment, or a closing brace — i.e. the component body's first statement.
 */
function locateFirstDeclaration(source: string): LocatedSymbol | undefined {
	const lines = source.split('\n')
	let offset = 0
	for (const line of lines) {
		const trimmed = line.trim()
		const skip =
			trimmed.length === 0 ||
			trimmed.startsWith('import ') ||
			trimmed.startsWith('//') ||
			trimmed.startsWith('/*') ||
			trimmed.startsWith('*') ||
			trimmed.startsWith('}')
		if (!skip) {
			const start = offset + (line.length - line.trimStart().length)
			const end = offset + line.replace(/\s+$/, '').length
			return { declStart: start, declEnd: end, nameStart: start, nameEnd: end }
		}
		offset += line.length + 1 // +1 for the consumed '\n'
	}
	return undefined
}

/** The count of leading whitespace characters in a string. */
function leadingWhitespace(text: string): number {
	return text.length - text.trimStart().length
}

/**
 * Read the declaring file's text for symbol resolution. Best-effort: an
 * unreadable file (deleted, permissions, or a non-file URI) yields `undefined`
 * so navigation falls back to the file top rather than throwing.
 */
function readTargetSource(fileName: string): string | undefined {
	try {
		return readFileSync(fileName, 'utf8')
	} catch {
		return undefined
	}
}

/** Line-start index over a raw source string for offset→{@link Position} mapping. */
class SourceLineIndex {
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

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

/**
 * Compiler diagnostics for a Treaty document, registry-aware: the file's
 * imported selectors are resolved from the workspace registry so a cross-module
 * `<app-card>` compiles the way a bundler build would, then the Rust compiler's
 * errors are converted into LSP diagnostics over the source.
 *
 * The error→range anchoring is the *single* shared path from {@link
 * ./diagnostics.js mapErrorsToDiagnostics}: sass errors are pinned inside the
 * `<style>` block, every other message is anchored to the start of the embedded
 * TypeScript region (recovered from the document's root virtual code), falling
 * back to the document start. There is no longer a separate whole-document
 * range-at-offset-0 path here.
 */
function provideTemplateDiagnostics(
	document: TextDocument,
	registry: ComponentRegistry,
	context: LanguageServiceContext,
): Diagnostic[] {
	const source = document.getText()
	const fileName = uriToFileName(sourceUriOf(context, document.uri))
	const imported = registry.importedSelectorsFor(source)
	const compiled = compileWithSelectors(source, fileName, imported)
	const diagnosticDocument: DiagnosticDocument = {
		fileName,
		languageId: document.languageId,
		text: source,
	}
	const rootVirtualCode = rootVirtualCodeOf(context, sourceUriOf(context, document.uri))
	return mapErrorsToDiagnostics(compiled.errors, diagnosticDocument, rootVirtualCode)
}

/**
 * Resolve a document's root {@link VirtualCode} from the language context, used
 * to anchor non-positional diagnostics on the embedded TypeScript region.
 * Best-effort: returns `undefined` when the script is not (yet) tracked.
 */
function rootVirtualCodeOf(
	context: LanguageServiceContext,
	uri: string,
): VirtualCode | undefined {
	const language = context.language as unknown as {
		scripts?: {
			get?: (id: unknown) => { generated?: { root?: VirtualCode } } | undefined
		}
	}
	const scripts = language.scripts
	const get = scripts?.get
	if (!get) {
		return undefined
	}
	// `scripts.get` keys on the volarjs script id (a `URI` in the server, a string
	// for in-process callers); try the parsed URI first, then the raw string.
	for (const id of [parseUri(uri), uri]) {
		if (id === undefined) {
			continue
		}
		try {
			const root = get.call(scripts, id)?.generated?.root
			if (root) {
				return root
			}
		} catch {
			// Try the next id form.
		}
	}
	return undefined
}

/** Parse a uri string into a {@link URI}, or `undefined` when it is not parseable. */
function parseUri(uri: string): URI | undefined {
	try {
		return URI.parse(uri)
	} catch {
		return undefined
	}
}

/**
 * Resolve the SOURCE-script uri behind a (possibly embedded) document uri.
 *
 * When volarjs serves a plugin over a root virtual code it hands the plugin the
 * EMBEDDED document, whose uri is the encoded `volar-embedded-content://…` form,
 * NOT the original `file://…/Foo.treaty`. Deriving a filename from that encoded
 * uri yields garbage — corrupting the registry's class/tag fold and the
 * self-file exclusion so a real prefix like `<st` matches nothing. This decodes
 * the embedded uri back to its source-script uri via
 * {@link LanguageServiceContext.decodeEmbeddedDocumentUri}, so every
 * `uriToFileName` call site works on the real path. A non-embedded (plain
 * `file://`) uri — as in the in-process unit smokes — is returned unchanged.
 */
function sourceUriOf(context: LanguageServiceContext, uri: string): string {
	const parsed = parseUri(uri)
	if (parsed === undefined) {
		return uri
	}
	// `decodeEmbeddedDocumentUri` is present on a real server context; an
	// in-process / unit-test caller may pass a minimal context without it (and a
	// plain `file://` uri that needs no decoding), so guard before calling.
	const decode = context.decodeEmbeddedDocumentUri
	if (typeof decode !== 'function') {
		return uri
	}
	const decoded = decode.call(context, parsed)
	return decoded ? decoded[0].toString() : uri
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
	const handle = entry.attributeSelector
		? `Attribute selector: \`[${entry.attributeSelector}]\` — applied bare (no \`use:\` needed)`
		: `Selectorless tag: \`<${entry.tag}>\``
	const lines = [
		`**${entry.className}** _(${entry.kind})_`,
		'',
		handle,
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

/**
 * Documentation for a bare attribute-selector directive completion. Names the
 * directive class + its attribute selector, and HINTS that `use:` is valid but
 * optional/redundant for a selector-bearing directive (it is applied by its
 * attribute selector directly).
 */
function attributeDirectiveDoc(entry: ComponentEntry): {
	kind: typeof MarkupKind.Markdown
	value: string
} {
	const attr = entry.attributeSelector!
	return {
		kind: MarkupKind.Markdown,
		value: [
			`Attribute directive **${entry.className}** (selector \`[${attr}]\`) from \`${entry.importSpecifier}\`.`,
			'',
			`Applied by its attribute selector — \`use:\` is **optional/redundant** here (\`use:${attr}\` also works).`,
			'',
			'Auto-imported on accept — no `NgModule`.',
		].join('\n'),
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
