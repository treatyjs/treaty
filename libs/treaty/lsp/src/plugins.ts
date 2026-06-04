/**
 * @module
 *
 * Plugin-extensible architecture for the Treaty language service.
 *
 * The service is extensible along two orthogonal axes:
 *
 *  - **Per authoring format** ({@link AuthoringLanguagePlugin}): how a given
 *    source file (by extension, e.g. `.treaty`, `.tsx`) is parsed into a
 *    volarjs {@link VirtualCode} tree. This is the volarjs `LanguagePlugin`
 *    surface, narrowed to the pieces an authoring format needs to implement.
 *
 *  - **Per server language** ({@link ServerLanguagePlugin}): which downstream
 *    language toolchain produces semantic diagnostics for the embedded code
 *    inside a virtual file (e.g. `server:rust`, `server:ts`, `server:php`).
 *
 * Both axes are backed by mutable registries so additional formats and
 * languages can be contributed without modifying the server entry point.
 */

import type {
	CodegenContext,
	IScriptSnapshot,
	VirtualCode,
} from '@volar/language-core'
import type { Diagnostic } from 'vscode-languageserver'
import {
	createAngularHtmlVirtualCode,
	createAngularSourceVirtualCode,
	createJsxVirtualCode,
	createTreatyVirtualCode,
} from './language.js'
import { provideDiagnostics, type DiagnosticDocument } from './diagnostics.js'

/**
 * Stable identifier of a server-side language toolchain. New toolchains can be
 * registered with arbitrary `server:*` ids; the unions below are the ones the
 * scaffold ships with by default.
 */
export type ServerLanguageId = 'server:rust' | 'server:ts' | 'server:php' | (string & {})

/**
 * Contributes support for a single authoring format (one set of file
 * extensions) to the language service.
 *
 * Implementations turn a source snapshot into a volarjs {@link VirtualCode}
 * tree whose embedded codes are handed off to {@link ServerLanguagePlugin}s for
 * semantic analysis.
 */
export interface AuthoringLanguagePlugin<
	K extends VirtualCode = VirtualCode,
> {
	/** Unique, stable identifier for the authoring format (e.g. `"treaty"`). */
	readonly id: string

	/**
	 * File extensions handled by this plugin, including the leading dot
	 * (e.g. `[".treaty"]`, `[".tsx", ".tjsx"]`). Used by
	 * {@link resolveByExtension} to route files to the right plugin.
	 */
	readonly extensions: readonly string[]

	/**
	 * volarjs language id assigned to source scripts of this format. Mirrors
	 * `LanguagePlugin.getLanguageId`. For a format spanning more than one
	 * extension/language id (e.g. Angular's `.html` template + `.ts` source),
	 * this is the default; {@link languageIdFor} refines it per file and
	 * {@link languageIds} enumerates the full set the plugin can emit.
	 */
	readonly languageId: string

	/**
	 * Every volarjs language id this plugin can emit from {@link languageIdFor}.
	 * Defaults to `[languageId]` when omitted. The volarjs adapter indexes each
	 * entry so {@link createVirtualCode} is routed for any of them.
	 */
	readonly languageIds?: readonly string[]

	/**
	 * Refine the volarjs language id for a specific file (by path/extension),
	 * for formats that map distinct extensions to distinct language ids. Returns
	 * `undefined` to fall back to {@link languageId}.
	 */
	languageIdFor?(fileName: string): string | undefined

	/**
	 * The {@link ServerLanguageId}s whose diagnostics this format expects to
	 * surface for its embedded code. Lets the server pre-resolve the relevant
	 * {@link ServerLanguagePlugin}s for a given file.
	 */
	readonly serverLanguages: readonly ServerLanguageId[]

	/**
	 * Parse a source snapshot into a volarjs virtual code tree. Mirrors
	 * `LanguagePlugin.createVirtualCode`; returns `undefined` when this plugin
	 * does not handle the given `languageId`.
	 */
	createVirtualCode(
		scriptId: string,
		languageId: string,
		snapshot: IScriptSnapshot,
		ctx: CodegenContext<string>
	): K | undefined

	/**
	 * Produce compiler diagnostics for a document of this format by invoking the
	 * Rust authoring compiler and mapping its errors back onto the source.
	 * `rootVirtualCode` is the document's root {@link VirtualCode} (as returned
	 * by {@link createVirtualCode}), used to anchor diagnostic ranges.
	 */
	provideDiagnostics(
		document: DiagnosticDocument,
		rootVirtualCode?: VirtualCode
	): Diagnostic[]
}

/**
 * Contributes semantic diagnostics (and, in later phases, completions, hovers,
 * etc.) for a single server-side language. The actual language-service wiring
 * is provided in a subsequent phase; this interface fixes the contract the
 * registry resolves against.
 */
export interface ServerLanguagePlugin {
	/** Unique, stable identifier (typically equal to {@link lang}). */
	readonly id: string

	/** The server language this plugin handles (e.g. `"server:rust"`). */
	readonly lang: ServerLanguageId
}

// ---------------------------------------------------------------------------
// Registries
// ---------------------------------------------------------------------------

const authoringByExtension = new Map<string, AuthoringLanguagePlugin>()
const authoringById = new Map<string, AuthoringLanguagePlugin>()
const serverByLang = new Map<string, ServerLanguagePlugin>()

/**
 * Register an authoring-format plugin. Each of its {@link
 * AuthoringLanguagePlugin.extensions} is indexed (case-insensitively, with a
 * normalized leading dot) for {@link resolveByExtension}.
 */
export function registerAuthoringLanguage(plugin: AuthoringLanguagePlugin): void {
	authoringById.set(plugin.id, plugin)
	for (const ext of plugin.extensions) {
		authoringByExtension.set(normalizeExtension(ext), plugin)
	}
}

/** Register a server-language plugin, keyed by its {@link ServerLanguagePlugin.lang}. */
export function registerServerLanguage(plugin: ServerLanguagePlugin): void {
	serverByLang.set(plugin.lang, plugin)
}

/**
 * Resolve the authoring plugin responsible for a file path or bare extension.
 * Accepts `"foo.treaty"`, `".treaty"`, or `"treaty"`.
 */
export function resolveByExtension(
	pathOrExtension: string
): AuthoringLanguagePlugin | undefined {
	return authoringByExtension.get(normalizeExtension(extractExtension(pathOrExtension)))
}

/** Resolve the server-language plugin for a {@link ServerLanguageId}. */
export function resolveByLang(lang: string): ServerLanguagePlugin | undefined {
	return serverByLang.get(lang)
}

/** Snapshot of all registered authoring plugins (insertion order). */
export function listAuthoringLanguages(): AuthoringLanguagePlugin[] {
	return [...authoringById.values()]
}

/** Snapshot of all registered server-language plugins (insertion order). */
export function listServerLanguages(): ServerLanguagePlugin[] {
	return [...serverByLang.values()]
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function extractExtension(pathOrExtension: string): string {
	const dot = pathOrExtension.lastIndexOf('.')
	return dot === -1 ? pathOrExtension : pathOrExtension.slice(dot)
}

function normalizeExtension(ext: string): string {
	const lower = ext.toLowerCase()
	return lower.startsWith('.') ? lower : `.${lower}`
}

// ---------------------------------------------------------------------------
// Seed registry
// ---------------------------------------------------------------------------

const treatyPlugin: AuthoringLanguagePlugin = {
	id: 'treaty',
	extensions: ['.treaty'],
	languageId: 'treaty',
	serverLanguages: ['server:rust'],
	createVirtualCode(_scriptId, languageId, snapshot) {
		if (languageId !== 'treaty') {
			return undefined
		}
		return createTreatyVirtualCode(languageId, snapshot)
	},
	provideDiagnostics(document, rootVirtualCode) {
		return provideDiagnostics(document, rootVirtualCode)
	},
}

const jsxPlugin: AuthoringLanguagePlugin = {
	id: 'jsx',
	// `.tsx` and `.jsx` are FIRST-CLASS Treaty JSX authoring formats (signals,
	// selectorless, region completion); `.tjsx` is the historical stopgap alias,
	// kept for back-compat. All three resolve to the `treaty-jsx` language id.
	extensions: ['.tsx', '.jsx', '.tjsx'],
	languageId: 'treaty-jsx',
	serverLanguages: ['server:ts'],
	createVirtualCode(_scriptId, languageId, snapshot) {
		if (languageId !== 'treaty-jsx') {
			return undefined
		}
		return createJsxVirtualCode(languageId, snapshot)
	},
	provideDiagnostics(document, rootVirtualCode) {
		return provideDiagnostics(document, rootVirtualCode)
	},
}

/**
 * Plain (non-Treaty) Angular support: external `.html` Angular templates and
 * ordinary Angular component `.ts` sources, so users can import and edit
 * regular Angular alongside Treaty.
 *
 *  - `.html` → languageId `angular-html`, projected to an `html`-flavoured
 *    embedded code mapped 1:1 (highlighting + best-effort checks).
 *  - `.ts` → languageId `typescript`, passed through as a 1:1 TypeScript
 *    embedded code so the standard TS service covers it. The branch is gated on
 *    `languageId` so it never shadows volarjs' own TS handling for non-Angular
 *    `.ts` that the registry did not route here.
 *
 * Diagnostics delegate to the shared {@link provideDiagnostics} path (the NAPI
 * compile entry handles `.ts` via the Rust `AngularSourcePlugin`); a bare
 * `.html` template is not a compilable module, so it surfaces no compiler
 * diagnostics of its own.
 */
const ANGULAR_HTML_LANGUAGE_ID = 'angular-html'

const angularPlugin: AuthoringLanguagePlugin = {
	id: 'angular',
	extensions: ['.html', '.ts'],
	languageId: ANGULAR_HTML_LANGUAGE_ID,
	languageIds: [ANGULAR_HTML_LANGUAGE_ID, 'typescript'],
	languageIdFor(fileName) {
		return normalizeExtension(extractExtension(fileName)) === '.ts'
			? 'typescript'
			: ANGULAR_HTML_LANGUAGE_ID
	},
	serverLanguages: ['server:ts'],
	createVirtualCode(_scriptId, languageId, snapshot) {
		if (languageId === ANGULAR_HTML_LANGUAGE_ID) {
			return createAngularHtmlVirtualCode(languageId, snapshot)
		}
		if (languageId === 'typescript') {
			return createAngularSourceVirtualCode(languageId, snapshot)
		}
		return undefined
	},
	provideDiagnostics(document, rootVirtualCode) {
		// A bare external template is not a compilable module; only the embedded
		// HTML highlighting applies. Angular component `.ts` delegates to the
		// shared compiler path (compileSource → Rust AngularSourcePlugin).
		if (document.languageId === ANGULAR_HTML_LANGUAGE_ID) {
			return []
		}
		return provideDiagnostics(document, rootVirtualCode)
	},
}

registerAuthoringLanguage(treatyPlugin)
registerAuthoringLanguage(jsxPlugin)
registerAuthoringLanguage(angularPlugin)

const rustServerPlugin: ServerLanguagePlugin = { id: 'server:rust', lang: 'server:rust' }
const tsServerPlugin: ServerLanguagePlugin = { id: 'server:ts', lang: 'server:ts' }

registerServerLanguage(rustServerPlugin)
registerServerLanguage(tsServerPlugin)
