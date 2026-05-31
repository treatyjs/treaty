/**
 * @module
 *
 * FUNCTION CHUNKING for the Treaty rslib (library) build. Like the app-facing
 * Rsbuild plugin, this consumes the per-fn `serverChunks` the `@treaty/compiler`
 * core attaches to each transformed authoring file and turns them into emitted
 * library output. The difference is the LIBRARY contract: each server fn is
 * exposed as its OWN separately-exported chunk entry, so a downstream consumer
 * can import a single server fn and have only that fn's chunk pulled in.
 *
 * For a library that declared server fns this emits:
 *   - one `<chunkId>.server.js` chunk per fn — the fn BODY alone, separately
 *     loadable and never part of the library's client/Ivy output;
 *   - a `server/index.js` barrel that re-exports each fn under its author name
 *     from its own chunk (the per-fn client binding), so the library's server
 *     surface is a set of independently code-splittable named exports; and
 *   - a `treaty-server-fns.json` manifest mapping each stable fn id to its
 *     export name + chunk ref.
 *
 * The client binding (the import-and-re-export shim) is the only trace of a
 * server fn that reaches the library's client bundle — the body lives solely in
 * its chunk. Bundler-agnostic: chunks accumulate in a plain collector that
 * renders an asset list, so the rslib `processAssets` adapter and the smoke
 * tests drive it without a running bundler.
 */

import {
	buildServerFnManifest,
	type ServerFnChunk,
	type ServerFnManifest,
	type TransformResult,
} from '@treaty/compiler'

/** Output file name of the emitted server-fn manifest. */
export const SERVER_FN_MANIFEST_NAME = 'treaty-server-fns.json'

/** Output file name of the library's server-fn barrel (re-exports each fn). */
export const SERVER_FN_BARREL_NAME = 'server/index.js'

/** Build the emitted file name for a server-fn chunk from its stable id. */
export function serverChunkFileName(chunkId: string): string {
	return `${chunkId}.server.js`
}

/** One ready-to-emit asset: a build-output file name plus its source text. */
export interface EmittableAsset {
	/** Output file name (relative to the library's dist root). */
	readonly name: string
	/** Full source text of the asset. */
	readonly source: string
}

/**
 * Accumulates the per-fn server chunks discovered across every transformed
 * module in a library build, de-duplicated by stable chunk id (so a fn imported
 * from several modules is emitted once). Renders the library's server output:
 * one chunk per fn, a re-export barrel, and the manifest.
 */
export class ServerChunkCollector {
	/** stable chunk id -> chunk, in first-seen order via the Map. */
	private readonly chunks = new Map<string, ServerFnChunk>()

	/**
	 * Record every server-fn chunk a transform result carries. A result with no
	 * `serverChunks` is a no-op; a repeated chunk id keeps the first occurrence
	 * (ids are stable and body-independent, so duplicates are byte-identical).
	 */
	add(result: TransformResult | null | undefined): void {
		if (!result?.serverChunks) return
		for (const chunk of result.serverChunks) {
			if (!this.chunks.has(chunk.id)) this.chunks.set(chunk.id, chunk)
		}
	}

	/** Whether any server-fn chunk has been collected. */
	get isEmpty(): boolean {
		return this.chunks.size === 0
	}

	/** The collected chunks, in first-seen order. */
	collected(): readonly ServerFnChunk[] {
		return [...this.chunks.values()]
	}

	/** The server-fn manifest for the collected chunks: id -> export name + ref. */
	manifest(): ServerFnManifest {
		return buildServerFnManifest([{ code: '', sideEffects: false, serverChunks: this.collected() }])
	}

	/**
	 * The library's server-fn barrel: one re-export per fn, each pulling its
	 * binding from the fn's own `<id>.server.js` chunk. Because every fn is
	 * re-exported from its own chunk, a consumer that imports a single fn pulls in
	 * only that chunk — i.e. each server fn is a separately-exported chunk entry.
	 */
	barrel(): string {
		const lines = this.collected().map((chunk) => {
			const from = JSON.stringify(`../${serverChunkFileName(chunk.id)}`)
			return `export { ${chunk.exportName} } from ${from};`
		})
		return `${lines.join('\n')}\n`
	}

	/**
	 * Render the assets the library should emit for its server fns: one
	 * `<chunkId>.server.js` per fn (the body), the `server/index.js` re-export
	 * barrel, then the `treaty-server-fns.json` manifest. Empty when no server fns
	 * were collected, so a pure client library emits nothing extra.
	 */
	assets(): EmittableAsset[] {
		if (this.chunks.size === 0) return []
		const out: EmittableAsset[] = []
		for (const chunk of this.chunks.values()) {
			out.push({ name: serverChunkFileName(chunk.id), source: chunk.code })
		}
		out.push({ name: SERVER_FN_BARREL_NAME, source: this.barrel() })
		out.push({
			name: SERVER_FN_MANIFEST_NAME,
			source: `${JSON.stringify(this.manifest(), null, 2)}\n`,
		})
		return out
	}
}
