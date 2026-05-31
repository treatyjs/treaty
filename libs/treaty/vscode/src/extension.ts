/**
 * @module
 *
 * VS Code extension client for Treaty. It launches the {@link @treaty/lsp}
 * volarjs language server in a child process over IPC and connects a
 * {@link LanguageClient} to it. The server serves every registered Treaty
 * authoring format (`.treaty`, `.tjsx`) plus the plain Angular files it now
 * understands (`.ts`, `.html`), so the document selector covers all four.
 *
 * The grammar and language-configuration contributions live in
 * `package.json`; this file owns only the runtime wiring: resolving the server
 * entry, building the client, and registering the Volar Labs hooks plus
 * auto-insertion for Treaty documents.
 */

import type * as vscode from 'vscode'
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

let client: LanguageClient | undefined

/**
 * Activate the extension: resolve the bundled `@treaty/lsp` server, start it
 * over IPC, and connect a language client. Returns the Volar Labs export so
 * the Volar Labs view can introspect this client.
 */
export async function activate(context: vscode.ExtensionContext) {
	const serverModule = resolveServerModule()

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
	}

	client = new LanguageClient('treaty', 'Treaty Language Server', serverOptions, clientOptions)
	await client.start()

	// Auto-insertion (e.g. closing of embedded constructs) for Treaty documents.
	context.subscriptions.push(
		activateAutoInsertion([{ language: 'treaty' }, { language: 'treaty-jsx' }], client),
	)

	// Expose the client to the Volar Labs extension for virtual-file inspection.
	const labsInfo = createLabsInfo(protocol)
	labsInfo.addLanguageClient(client)
	return labsInfo.extensionExports
}

/** Stop the language client on deactivation. */
export async function deactivate(): Promise<void> {
	await client?.stop()
	client = undefined
}

/**
 * Resolve the runnable `@treaty/lsp` server entry. Prefer the package's
 * declared `dist/server.js`; fall back to the `treaty-lsp` bin if the package
 * layout changes. The server loads the NAPI authoring addon itself at runtime,
 * so it is never bundled into the extension.
 */
function resolveServerModule(): string {
	try {
		return require.resolve('@treaty/lsp/dist/server.js')
	} catch {
		return require.resolve('treaty-lsp')
	}
}
