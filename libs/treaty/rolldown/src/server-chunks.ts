/**
 * @module
 *
 * Function-chunking wiring for `@treaty/rolldown`.
 *
 * `@treaty/compiler` hands a transformed authoring file its server functions as
 * individual {@link ServerFnChunk} units (one per `createServerFn`). This module
 * turns each of those into a *separately-loadable* Rolldown output asset so the
 * server-fn BODY is code-split out of the component module and never re-bundled
 * into the client graph, while the component keeps only the per-fn client
 * binding.
 *
 * The mechanism is purely Rollup/Rolldown-native (no bundler-internal APIs), so
 * it is exactly the same machinery `@treaty/vite` uses — Rolldown's plugin
 * interface is Rollup-compatible, so `this.emitFile`, `resolveId`, and `load`
 * behave identically:
 *
 *   - The server body for each chunk is exposed as a virtual module
 *     (`SERVER_VIRTUAL_PREFIX + chunkId`) whose `load()` returns the chunk
 *     `code`. At build time it is emitted as an output ASSET via
 *     `this.emitFile({ type: 'asset' })` with a stable `fileName`
 *     (`<chunkId>.server.js`). It is emitted as an asset, not a chunk, because a
 *     server-fn body is a BACKEND module (the default axum backend emits
 *     Rust/axum) and Rolldown must NOT parse it as client JavaScript.
 *
 *   - The component's `clientBinding` imports `./<chunkId>.server.js`. On the
 *     CLIENT side that specifier is intercepted and resolved to a tiny RPC stub
 *     virtual module (`CLIENT_VIRTUAL_PREFIX + chunkId`) that calls the fn's
 *     `/__server/<name>` route — so following the binding never drags the server
 *     body into the client module graph. The body lives only in its emitted asset.
 *
 *   - A `treaty-server-fns.json` manifest asset (fn-id -> emitted chunk file +
 *     export name) is emitted in `generateBundle`, built from
 *     {@link buildBuildManifest} plus the resolved output file names.
 */

import type { ServerFnChunk } from '@treaty/compiler'

/** Virtual-module id prefix for a server-fn BODY chunk (server-side artifact). */
export const SERVER_VIRTUAL_PREFIX = '\0treaty-server-fn:'

/**
 * Virtual-module id prefix for a server-fn CLIENT binding stub. The component's
 * `clientBinding` import of `./<chunkId>.server.js` is rewritten to this so the
 * client graph never reaches the server body.
 */
export const CLIENT_VIRTUAL_PREFIX = '\0treaty-server-fn-client:'

/** The emitted manifest asset's file name (fn-id -> chunk file + export name). */
export const MANIFEST_FILE_NAME = 'treaty-server-fns.json'

/** Server route prefix every backend registers a fn under (`/__server/<name>`). */
const SERVER_ROUTE_PREFIX = '/__server/'

/**
 * One server-fn entry tracked across the build: the compiler's chunk plus the
 * stable output file name the body chunk is emitted under (so the client stub's
 * import and the manifest can both reference it).
 */
export interface TrackedServerFn {
	readonly chunk: ServerFnChunk
	/** Stable output file name for the body chunk, e.g. `srvfn_ab12cd.server.js`. */
	readonly fileName: string
}

/** The stable output file name a server-fn body chunk is emitted under. */
export function serverChunkFileName(chunk: ServerFnChunk): string {
	return `${chunk.id}.server.js`
}

/**
 * The import specifier the compiler's `clientBinding` uses to reach a fn's body
 * (`./<chunkId>.server.js`). On the client side this specifier is intercepted
 * and routed to the RPC stub instead of the body chunk.
 */
export function serverChunkImportSpecifier(chunk: ServerFnChunk): string {
	return `./${serverChunkFileName(chunk)}`
}

/**
 * Whether `source` is a server-fn body import emitted by a `clientBinding`
 * (`./<id>.server.js` or `<id>.server.js`). Returns the bare chunk id when it is,
 * else `null`. Used by the plugin's `resolveId` to redirect the client import.
 */
export function matchServerChunkSpecifier(source: string): string | null {
	const m = /^\.?\/?([A-Za-z0-9_$]+)\.server\.js$/.exec(source)
	return m && m[1] ? m[1] : null
}

/**
 * Build the client RPC stub module for a server fn: a binding that POSTs its
 * arguments to the fn's `/__server/<name>` route and returns the JSON result.
 * This is the ONLY thing the client graph ever sees of the fn — the body stays
 * in the emitted server chunk. Exports the fn under its author name and default.
 */
export function clientStubModule(exportName: string): string {
	const route = JSON.stringify(`${SERVER_ROUTE_PREFIX}${exportName}`)
	const name = JSON.stringify(exportName)
	return [
		'// Treaty server-fn client binding (generated). The server body is in a',
		'// separate chunk and never enters this client module.',
		`export async function ${exportName}(...args) {`,
		`\tconst res = await fetch(${route}, {`,
		`\t\tmethod: 'POST',`,
		`\t\theaders: { 'content-type': 'application/json' },`,
		'\t\tbody: JSON.stringify(args.length === 1 ? args[0] : args),',
		'\t})',
		'\tif (!res.ok) {',
		`\t\tthrow new Error('treaty server fn ' + ${name} + ' failed: ' + res.status)`,
		'\t}',
		'\treturn res.json()',
		'}',
		`export default ${exportName}`,
	].join('\n')
}

/**
 * Prepend each chunk's `clientBinding` to the component code so the client
 * references the per-fn binding instead of the (now extracted) server body. The
 * bindings import `./<id>.server.js`, which `resolveId` redirects to the client
 * stub, keeping the body out of the client graph.
 */
export function injectClientBindings(code: string, chunks: readonly ServerFnChunk[]): string {
	if (chunks.length === 0) return code
	// A PURE server MODULE (a file-level `'use server'` file) is lowered by the
	// compiler to client code that ALREADY exports a binding for each server fn
	// (e.g. `export const listTodos = (() => edenHttpResource(...))`). Injecting the
	// chunk's `import { listTodos } …; export { listTodos };` binding on top would
	// re-export the same name and produce a duplicate-export error. So only inject a
	// chunk's binding when the client code does NOT already export that name; the
	// chunk is still emitted as the server-body artifact regardless. For a component
	// whose INLINE server fns were lifted, the client code has no such export, so the
	// binding is injected as before.
	const needed = chunks.filter((c) => !clientAlreadyExports(code, c.exportName))
	if (needed.length === 0) return code
	const bindings = needed.map((c) => c.clientBinding).join('\n')
	return `${bindings}\n${code}`
}

/**
 * Whether `code` already declares a top-level export named `name` — an
 * `export const|let|var|function|class <name>` or an `export { … name … }` list.
 * Used to avoid double-exporting a server fn whose binding the compiler already
 * emitted (the pure-server-module case).
 */
function clientAlreadyExports(code: string, name: string): boolean {
	const esc = name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
	const decl = new RegExp(`(?:^|[\\n;])\\s*export\\s+(?:const|let|var|function|class)\\s+${esc}\\b`)
	if (decl.test(code)) return true
	const list = new RegExp(`export\\s*\\{[^}]*\\b${esc}\\b[^}]*\\}`)
	return list.test(code)
}

/**
 * The manifest payload: fn-id -> its export name and the emitted chunk file the
 * server body lives in. Serialized to {@link MANIFEST_FILE_NAME}.
 */
export type ServerFnBuildManifest = Record<
	string,
	{ readonly exportName: string; readonly chunkFile: string }
>

/** Build the serializable manifest from the tracked server fns. */
export function buildBuildManifest(tracked: Iterable<TrackedServerFn>): ServerFnBuildManifest {
	const manifest: ServerFnBuildManifest = {}
	for (const { chunk, fileName } of tracked) {
		manifest[chunk.id] = { exportName: chunk.exportName, chunkFile: fileName }
	}
	return manifest
}
