/**
 * @module
 *
 * FUNCTION CHUNKING for the Rsbuild plugin. The shared `@treaty/compiler` core
 * hands every transformed authoring file an optional `serverChunks` array — one
 * {@link ServerFnChunk} per extracted server function — instead of a single
 * `serverModule` blob. This module turns those chunks into emitted build output:
 *
 *   - one `<chunkId>.server.js` chunk file per server fn, carrying ONLY that
 *     fn's body (so it is separately loadable and never enters the client/Ivy
 *     bundle), and
 *   - a single `treaty-server-fns.json` manifest mapping each stable fn id to
 *     its export name + chunk ref, so a runtime can resolve a fn id to the chunk
 *     that backs it.
 *
 * The client side of each fn — the {@link ServerFnChunk.clientBinding} shim that
 * replaces the fn body in the component — is the only trace that reaches the
 * client bundle. `@treaty/compiler` already produces the client `code` with the
 * bodies removed; this module's job is purely the SERVER half: collecting the
 * per-fn chunks across the build and emitting them plus the manifest.
 *
 * It is bundler-agnostic on purpose: it accumulates chunks via a plain
 * {@link ServerChunkCollector} and renders an {@link EmittableAsset} list, so the
 * Rsbuild `processAssets` adapter (and the smoke tests) can drive it without a
 * running bundler.
 */

import {
	buildServerFnManifest,
	type ServerFnChunk,
	type ServerFnManifest,
	type TransformResult,
} from '@treaty/compiler'

/** Output file name of the emitted server-fn manifest. */
export const SERVER_FN_MANIFEST_NAME = 'treaty-server-fns.json'

/** Build the emitted file name for a server-fn chunk from its stable id. */
export function serverChunkFileName(chunkId: string): string {
	return `${chunkId}.server.js`
}

/** One ready-to-emit asset: a build-output file name plus its source text. */
export interface EmittableAsset {
	/** Output file name (relative to the build's asset root). */
	readonly name: string
	/** Full source text of the asset. */
	readonly source: string
}

/**
 * Accumulates the server-fn chunks discovered across every transformed module in
 * a single build. De-duplicates by stable chunk id, so the same fn referenced
 * from multiple modules contributes exactly one chunk. The client `code` of each
 * module is emitted by the bundler as usual — only the server halves land here.
 */
export class ServerChunkCollector {
	/** stable chunk id -> the chunk, in first-seen order via the Map. */
	private readonly chunks = new Map<string, ServerFnChunk>()

	/**
	 * Record every server-fn chunk carried by a transform result. A result with
	 * no `serverChunks` (a pure component, or a file with no server fns) is a
	 * no-op. Re-adding a chunk id keeps the first occurrence (ids are stable and
	 * body-independent, so duplicates are byte-identical anyway).
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
	 * Render the full set of assets this build should emit for its server fns:
	 * one `<chunkId>.server.js` per collected chunk (the fn body) followed by the
	 * `treaty-server-fns.json` manifest. Empty when no server fns were collected,
	 * so the bundler emits nothing extra for a pure client build.
	 */
	assets(): EmittableAsset[] {
		const out: EmittableAsset[] = []
		for (const chunk of this.chunks.values()) {
			out.push({ name: serverChunkFileName(chunk.id), source: chunk.code })
		}
		if (out.length > 0) {
			out.push({
				name: SERVER_FN_MANIFEST_NAME,
				source: `${JSON.stringify(this.manifest(), null, 2)}\n`,
			})
		}
		return out
	}
}
