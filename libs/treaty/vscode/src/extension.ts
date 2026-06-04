/**
 * @module
 *
 * VS Code extension client for Treaty. It launches the {@link @treaty/lsp}
 * volarjs language server in a child process over IPC and connects a
 * {@link LanguageClient} to it, exposing the server's full feature set
 * (completion, hover, diagnostics, definition, references, rename, signature
 * help, semantic tokens, document formatting) for `.treaty`, `.tjsx`, and the
 * plain Angular files (`.ts`, `.html`) the Treaty LSP now serves directly.
 *
 * On top of the language client it contributes the editor-side commands that
 * drive the Rust authoring compiler — *Compile to Ivy* and *Preview Compiled
 * Output* — plus a *Restart Language Server* command, and it honours the
 * `treaty.format.enable` setting so the server's document formatter participates
 * in VS Code's format-on-save.
 *
 * The grammar, language-configuration and snippet contributions live in
 * `package.json`; this file owns only the runtime wiring: resolving the bundled
 * server, building the client, registering the commands and the Volar Labs
 * hooks plus auto-insertion for Treaty documents.
 */

import * as vscode from 'vscode'
import * as path from 'node:path'
import { createRequire } from 'node:module'
import { activateAutoInsertion, createLabsInfo, getTsdk } from '@volar/vscode'
import * as protocol from '@volar/language-server/protocol'
import {
	LanguageClient,
	type LanguageClientOptions,
	type ServerOptions,
	TransportKind,
} from 'vscode-languageclient/node'

/** Document selector served by the Treaty language server. */
const documentSelector: LanguageClientOptions['documentSelector'] = [
	{ language: 'treaty' },
	{ language: 'treaty-jsx' },
	// Plain Angular files the Treaty LSP now serves directly.
	{ language: 'typescript', pattern: '**/*.ts' },
	{ language: 'html' },
]

/** Language ids whose documents are Treaty authoring formats. */
const TREATY_LANGUAGES = new Set(['treaty', 'treaty-jsx'])

/** Shared output channel for the compile/preview commands. */
let output: vscode.OutputChannel | undefined

let client: LanguageClient | undefined

/**
 * Activate the extension: resolve the bundled `@treaty/lsp` server, start it
 * over IPC, connect a language client, and register the Treaty commands. Returns
 * the Volar Labs export so the Volar Labs view can introspect this client.
 */
export async function activate(context: vscode.ExtensionContext) {
	output = vscode.window.createOutputChannel('Treaty')
	context.subscriptions.push(output)

	// Register the compiler commands first so they are available even if the
	// language server fails to start (e.g. a broken tsdk): compilation goes
	// straight through the native authoring addon, independent of the server.
	registerCommands(context)

	await startClient(context)

	// Restart command: tears down and re-creates the language client. Useful
	// after changing settings or upgrading the native addon.
	context.subscriptions.push(
		vscode.commands.registerCommand('treaty.restartServer', async () => {
			await stopClient()
			await startClient(context)
			void vscode.window.showInformationMessage('Treaty language server restarted.')
		}),
	)

	// Expose the client to the Volar Labs extension for virtual-file inspection.
	const labsInfo = createLabsInfo(protocol)
	if (client) {
		labsInfo.addLanguageClient(client)
	}
	return labsInfo.extensionExports
}

/** Stop the language client on deactivation. */
export async function deactivate(): Promise<void> {
	await stopClient()
}

/**
 * Build, start and wire a fresh language client into `context`. The client's
 * own disposable is tracked so {@link stopClient} (restart / deactivate) tears
 * it down cleanly.
 */
async function startClient(context: vscode.ExtensionContext): Promise<void> {
	const serverModule = resolveServerModule(context)

	const serverOptions: ServerOptions = {
		run: {
			module: serverModule,
			transport: TransportKind.ipc,
		},
		debug: {
			module: serverModule,
			transport: TransportKind.ipc,
			options: { execArgv: ['--nolazy', '--inspect=6009'] },
		},
	}

	const tsdk = await getTsdk(context)

	const clientOptions: LanguageClientOptions = {
		documentSelector,
		initializationOptions: {
			typescript: {
				tsdk: tsdk?.tsdk,
			},
		},
		// Honour `treaty.format.enable`: when formatting is turned off, short-circuit
		// the formatting requests so the server's formatter never runs (Format
		// Document and format-on-save become no-ops for Treaty files).
		middleware: {
			provideDocumentFormattingEdits: (document, options, token, next) =>
				formattingEnabled() ? next(document, options, token) : undefined,
			provideDocumentRangeFormattingEdits: (document, range, options, token, next) =>
				formattingEnabled() ? next(document, range, options, token) : undefined,
			provideOnTypeFormattingEdits: (document, position, ch, options, token, next) =>
				formattingEnabled() ? next(document, position, ch, options, token) : undefined,
		},
	}

	client = new LanguageClient('treaty', 'Treaty Language Server', serverOptions, clientOptions)
	try {
		await client.start()
	} catch (err) {
		client = undefined
		void vscode.window.showErrorMessage(
			`Treaty language server failed to start: ${describeError(err)}`,
		)
		return
	}

	// Auto-insertion (e.g. closing of embedded constructs) for Treaty documents.
	context.subscriptions.push(
		activateAutoInsertion([{ language: 'treaty' }, { language: 'treaty-jsx' }], client),
	)
}

/** Stop and discard the current language client, if any. */
async function stopClient(): Promise<void> {
	const current = client
	client = undefined
	if (current && current.needsStop()) {
		await current.stop()
	}
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/**
 * Register the editor-side commands that drive the Rust authoring compiler:
 *  - `treaty.compile`  — compile the active authoring file and write the Ivy
 *    output (and any extracted server module) next to it.
 *  - `treaty.preview`  — compile the active authoring file and open the Ivy
 *    output in a side-by-side untitled editor.
 *
 * Both reach the ONE Treaty compiler (the Rust/OXC `@treaty/authoring-node`
 * NAPI addon); there is no TypeScript fallback.
 */
function registerCommands(context: vscode.ExtensionContext): void {
	context.subscriptions.push(
		vscode.commands.registerCommand('treaty.preview', () => previewActive(context)),
		vscode.commands.registerCommand('treaty.compile', () => compileActive(context)),
	)
}

/** Compile the active document and open its Ivy output beside it. */
async function previewActive(context: vscode.ExtensionContext): Promise<void> {
	const editor = vscode.window.activeTextEditor
	if (!editor || !isAuthoringDocument(editor.document)) {
		void vscode.window.showWarningMessage('Treaty: open a .treaty or .tjsx file to preview.')
		return
	}
	const compiled = compileDocument(context, editor.document)
	if (!compiled) {
		return
	}
	reportErrors(editor.document, compiled.errors)

	const doc = await vscode.workspace.openTextDocument({
		language: 'javascript',
		content: compiled.code,
	})
	await vscode.window.showTextDocument(doc, {
		viewColumn: vscode.ViewColumn.Beside,
		preview: true,
		preserveFocus: true,
	})
	if (compiled.serverModule) {
		const serverDoc = await vscode.workspace.openTextDocument({
			language: 'typescript',
			content: compiled.serverModule,
		})
		await vscode.window.showTextDocument(serverDoc, {
			viewColumn: vscode.ViewColumn.Beside,
			preview: true,
			preserveFocus: true,
		})
	}
}

/** Compile the active document and write its output next to the source file. */
async function compileActive(context: vscode.ExtensionContext): Promise<void> {
	const editor = vscode.window.activeTextEditor
	if (!editor || !isAuthoringDocument(editor.document)) {
		void vscode.window.showWarningMessage('Treaty: open a .treaty or .tjsx file to compile.')
		return
	}
	const document = editor.document
	const compiled = compileDocument(context, document)
	if (!compiled) {
		return
	}
	reportErrors(document, compiled.errors)
	if (compiled.errors.length > 0) {
		void vscode.window.showErrorMessage(
			`Treaty: compilation reported ${compiled.errors.length} error(s) — see the Treaty output channel.`,
		)
		return
	}

	if (document.uri.scheme !== 'file') {
		void vscode.window.showWarningMessage(
			'Treaty: can only write compiled output for files on disk; use Preview instead.',
		)
		return
	}

	const sourcePath = document.uri.fsPath
	const outPath = sourcePath.replace(/\.(treaty|tjsx|tsx)$/i, '') + '.ivy.js'
	await vscode.workspace.fs.writeFile(
		vscode.Uri.file(outPath),
		new TextEncoder().encode(compiled.code),
	)
	const written: string[] = [outPath]
	if (compiled.serverModule) {
		const serverOut = sourcePath.replace(/\.(treaty|tjsx|tsx)$/i, '') + '.server.js'
		await vscode.workspace.fs.writeFile(
			vscode.Uri.file(serverOut),
			new TextEncoder().encode(compiled.serverModule),
		)
		written.push(serverOut)
	}
	void vscode.window.showInformationMessage(
		`Treaty: wrote ${written.map((p) => path.basename(p)).join(', ')}.`,
	)
}

/** A single authoring compilation result from the native addon. */
interface AuthoringResult {
	readonly code: string
	readonly serverModule?: string
	readonly errors: readonly string[]
}

/**
 * Compile a document through the native authoring addon, surfacing a friendly
 * error (and returning `undefined`) when the addon cannot be loaded.
 */
function compileDocument(
	context: vscode.ExtensionContext,
	document: vscode.TextDocument,
): AuthoringResult | undefined {
	const addon = loadAuthoringAddon(context)
	if (!addon) {
		void vscode.window.showErrorMessage(
			'Treaty: the native authoring compiler (@treaty/authoring-node) could not be loaded.',
		)
		return undefined
	}
	const fileName = document.uri.scheme === 'file' ? document.uri.fsPath : `${document.languageId}.treaty`
	try {
		return addon.compile(document.getText(), fileName)
	} catch (err) {
		void vscode.window.showErrorMessage(`Treaty: compilation failed — ${describeError(err)}`)
		return undefined
	}
}

/** Minimal shape of the native authoring addon used by the commands. */
interface AuthoringAddon {
	compile(source: string, fileName: string): AuthoringResult
}

/** The addon is loaded once and memoised (it is a native `.node` binary). */
let cachedAddon: AuthoringAddon | null | undefined

/**
 * Resolve and load the `@treaty/authoring-node` NAPI addon — the ONE Treaty
 * compiler. Resolution prefers the package id (a normal marketplace install
 * has it as a dependency); when running from the bundled extension it falls
 * back to the addon shipped beside the server. Cached after the first attempt.
 */
function loadAuthoringAddon(context: vscode.ExtensionContext): AuthoringAddon | null {
	if (cachedAddon !== undefined) {
		return cachedAddon
	}
	const req = createRequire(path.join(context.extensionPath, 'dist', 'extension.js'))
	for (const id of ['@treaty/authoring-node', './authoring-node']) {
		try {
			const mod = req(id) as AuthoringAddon
			if (typeof mod.compile === 'function') {
				cachedAddon = mod
				return mod
			}
		} catch {
			// try the next candidate
		}
	}
	cachedAddon = null
	return null
}

/** Print compiler errors to the Treaty output channel. */
function reportErrors(document: vscode.TextDocument, errors: readonly string[]): void {
	if (!output) {
		return
	}
	if (errors.length === 0) {
		output.appendLine(`[treaty] ${path.basename(document.uri.fsPath || document.uri.toString())}: compiled cleanly.`)
		return
	}
	output.appendLine(`[treaty] ${path.basename(document.uri.fsPath || document.uri.toString())}: ${errors.length} error(s):`)
	for (const err of errors) {
		output.appendLine(`  - ${err}`)
	}
	output.show(true)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** Whether a document is one of the Treaty authoring formats. */
function isAuthoringDocument(document: vscode.TextDocument): boolean {
	return TREATY_LANGUAGES.has(document.languageId)
}

/**
 * Resolve the runnable `@treaty/lsp` server entry. Prefer the server bundled
 * into this extension's `dist/` (so a packaged VSIX is self-contained); fall
 * back to the package's declared entry / `treaty-lsp` bin for source checkouts.
 * The server loads the native authoring addon itself at runtime, so the addon
 * is never bundled into the extension.
 */
function resolveServerModule(context: vscode.ExtensionContext): string {
	// A developer override always wins.
	const override = vscode.workspace.getConfiguration('treaty').get<string>('server.path')
	if (override) {
		return override
	}
	const bundled = path.join(context.extensionPath, 'dist', 'server.mjs')
	try {
		require('node:fs').accessSync(bundled)
		return bundled
	} catch {
		// not bundled — fall through to package resolution
	}
	try {
		return require.resolve('@treaty/lsp/dist/server.js')
	} catch {
		return require.resolve('treaty-lsp')
	}
}

/** Whether the Treaty document formatter is enabled (`treaty.format.enable`). */
function formattingEnabled(): boolean {
	return vscode.workspace.getConfiguration('treaty').get<boolean>('format.enable', true)
}

/** A short, human-readable description of an unknown thrown value. */
function describeError(err: unknown): string {
	return err instanceof Error ? err.message : String(err)
}
