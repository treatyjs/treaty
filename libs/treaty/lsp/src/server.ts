#!/usr/bin/env node
/**
 * @module
 *
 * Runnable entry point for the `treaty-lsp` binary: a volarjs language server
 * that serves every registered Treaty authoring format. It wires the authoring
 * registry into a TypeScript-backed volarjs project so embedded code is
 * type-checked by the real TypeScript language service, and surfaces the Rust
 * authoring compiler's diagnostics on top, forwarding everything over the
 * standard LSP connection.
 *
 * Importing this module (transitively, via `./language.js` → `./plugins.js`)
 * seeds the default `.treaty` / `.tsx` / `.tjsx` formats; additional formats
 * registered before `initialize` runs are picked up automatically.
 *
 * The module never starts a server on import: {@link createServer} builds and
 * wires a server against a caller-provided (or freshly created) connection, and
 * {@link start} is invoked only when this file is executed directly as the
 * `treaty-lsp` binary.
 */

import { createRequire } from 'node:module'
import { fileURLToPath } from 'node:url'
import type { Connection } from 'vscode-languageserver/node'
import type { URI } from 'vscode-uri'
import {
	createConnection,
	createServer as createVolarServer,
	createTypeScriptProject,
	loadTsdkByPath,
} from '@volar/language-server/node'
import { create as createTypeScriptServices } from 'volar-service-typescript'
import { createTreatyLanguagePlugin } from './language.js'
import { applyTreatyJsxAutoTypes, resolveTreatyJsxTypesEntry } from './jsx-types.js'

const require = createRequire(import.meta.url)

/**
 * The wired Treaty language server: the underlying volarjs server object plus
 * the connection it was built against. Tests drive the server through this
 * handle; the CLI just calls {@link start}.
 */
export interface TreatyLanguageServer {
	/** The LSP connection the server is bound to. */
	readonly connection: Connection
	/** The underlying volarjs server (initialize/initialized/shutdown, etc.). */
	readonly server: ReturnType<typeof createVolarServer>
	/** Begin listening on the connection. Idempotent for a given connection. */
	listen(): void
}

/**
 * Build and wire a Treaty language server against `connection` (a fresh stdio
 * connection is created when none is given).
 *
 * On `initialize`, a {@link createTypeScriptProject TypeScript project} is set
 * up with the Treaty {@link createTreatyLanguagePlugin language plugin} so every
 * authoring format's embedded code is projected into TypeScript, and the
 * `volar-service-typescript` language-service plugins provide diagnostics,
 * completion, hover and navigation over that embedded code. The TypeScript SDK
 * is taken from the client's `initializationOptions.typescript.tsdk` when
 * provided, falling back to the `typescript` package bundled with this server.
 *
 * Does not start listening; call {@link TreatyLanguageServer.listen} (or use
 * {@link start}) when ready.
 */
export function createServer(connection: Connection = createConnection()): TreatyLanguageServer {
	const server = createVolarServer(connection)

	connection.onInitialize((params) => {
		const tsdk = (params.initializationOptions as InitializationOptions | undefined)
			?.typescript?.tsdk
		const { typescript, diagnosticMessages } = loadTsdkByPath(
			tsdk ?? resolveDefaultTsdk(),
			params.locale,
		)

		// Locate the shipped @treaty/jsx ambient declarations once. The resolved
		// .d.ts (when present) is injected as an extra root file, and every
		// project compilerOptions are augmented to auto-include @treaty/jsx and
		// route the automatic JSX runtime through it, so every Treaty file
		// resolves the global JSX namespace with no per-project tsconfig opt-in.
		// See applyTreatyJsxAutoTypes.
		const jsxTypesEntry = resolveTreatyJsxTypesEntry()

		return server.initialize(
			params,
			createTypeScriptProject(typescript, diagnosticMessages, ({ projectHost }) => {
				applyTreatyJsxAutoTypes(projectHost, jsxTypesEntry)
				return {
					languagePlugins: [
						createTreatyLanguagePlugin<URI>({
							scriptIdToFileName: (uri) => uri.fsPath || uri.path,
						}),
					],
				}
			}),
			createTypeScriptServices(typescript),
		)
	})

	connection.onInitialized(() => {
		server.initialized()
	})

	connection.onShutdown(() => {
		server.shutdown()
	})

	return {
		connection,
		server,
		listen() {
			connection.listen()
		},
	}
}

/** Initialization options understood by the Treaty server. */
interface InitializationOptions {
	readonly typescript?: {
		/** Absolute path to the client's TypeScript `lib` directory. */
		readonly tsdk?: string
	}
}

/**
 * Create the default stdio server and start listening. This is the body of the
 * `treaty-lsp` binary.
 */
export function start(): TreatyLanguageServer {
	const handle = createServer()
	handle.listen()
	return handle
}

/**
 * Resolve the TypeScript `lib` directory bundled with this server, used when the
 * client does not provide one via `initializationOptions.typescript.tsdk`.
 */
function resolveDefaultTsdk(): string {
	const entry = require.resolve('typescript')
	const sep = Math.max(entry.lastIndexOf('/'), entry.lastIndexOf('\\'))
	return sep === -1 ? entry : entry.slice(0, sep)
}

// Start the server only when executed directly as the `treaty-lsp` binary, not
// when imported (e.g. by tests or by `./index.js`).
if (isMainModule()) {
	start()
}

/** True when this module is the process entry point. */
function isMainModule(): boolean {
	const entry = process.argv[1]
	if (!entry) {
		return false
	}
	try {
		return fileURLToPath(import.meta.url) === entry
	} catch {
		return false
	}
}
