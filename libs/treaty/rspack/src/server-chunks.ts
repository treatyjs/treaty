/**
 * @module
 *
 * Rspack-side wiring for Treaty server-function CHUNKING.
 *
 * `@treaty/compiler` hands a transformed authoring file its server functions as
 * INDIVIDUAL {@link ServerFnChunk} units (not one `serverModule` blob). This
 * module turns those units into real Rspack output so each server fn is split
 * into its OWN separately-loadable chunk while its BODY never enters the client
 * (Ivy) bundle:
 *
 *   1. {@link emitServerFnChunks} — invoked from the loader for every owned module
 *      that produced server chunks. For each fn it:
 *        - emits the fn's server-side `code` as a build asset named
 *          `<chunkId>.server.js` (so the body lands in the output as its own
 *          loadable unit — a chunk — and is reachable by the server runtime), and
 *        - records the fn in a per-compilation registry so the plugin can emit the
 *          aggregate fn-id -> chunk manifest once the build settles.
 *      It returns the CLIENT module text: the compiler's already body-free Ivy JS
 *      with each fn's `clientBinding` appended. The client binding is a lazy
 *      `import('./<chunkId>.server.js')` boundary, so Rspack code-splits each fn
 *      into a separate async chunk AND the client module only ever carries the
 *      binding shim — never the fn body.
 *
 *   2. {@link TreatyServerFnRegistry} — a per-compilation accumulator of the
 *      emitted chunks, used to build the manifest asset.
 *
 *   3. {@link serverFnManifestAsset} — serializes the accumulated manifest
 *      (fn-id -> { exportName, chunkRef, asset }) to the JSON asset the plugin
 *      writes (`treaty-server-fns.json`).
 *
 * Framework note: this only uses the structural slice of the loader context it
 * needs (`emitFile`), so the package still typechecks without `@rspack/core`.
 */

import { buildServerFnManifest, type ServerFnChunk, type TransformResult } from '@treaty/compiler'

/** The fixed name of the emitted server-fn manifest asset. */
export const SERVER_FN_MANIFEST_ASSET = 'treaty-server-fns.json'

/**
 * The asset file name a server-fn chunk is emitted under. Stable and derived
 * solely from the fn's stable chunk id, so the same fn always lands in the same
 * output file across builds (and the client binding can import it by name).
 */
export function serverFnAssetName(chunk: ServerFnChunk): string {
	return `${chunk.id}.server.js`
}

/**
 * One manifest record per emitted server fn: the stable chunk id, the author's
 * export name, the chunk reference (its stable id), and the emitted asset path.
 * Keyed in the manifest by the fn's stable {@link ServerFnChunk.id}.
 */
export interface ServerFnManifestRecord {
	readonly exportName: string
	readonly chunkRef: string
	readonly asset: string
}

/** The emitted manifest shape: stable fn id -> its record. */
export type RspackServerFnManifest = Record<string, ServerFnManifestRecord>

/**
 * The structural slice of the loader context required to emit a server chunk as
 * a build asset. `@rspack/core`'s real `LoaderContext.emitFile` is assignable to
 * this (we only use the two-argument `(name, content)` form). Declared here so
 * the package typechecks without the peer installed.
 */
export interface ServerChunkEmitter {
	/** Emit a build asset; the body becomes its own loadable output file. */
	emitFile(name: string, content: string): void
}

/**
 * A per-compilation registry of every server-fn chunk emitted across all owned
 * modules. The plugin creates one per build and the loader feeds it; at build
 * settle the plugin serializes it into the manifest asset.
 *
 * De-dupes by stable chunk id: the same fn (same file + name) emitted from more
 * than one loader pass (e.g. HMR re-runs) records once, keeping the manifest and
 * the emitted-asset set stable.
 */
export class TreatyServerFnRegistry {
	private readonly chunks = new Map<string, ServerFnChunk>()

	/** Record every chunk from a transform result. Idempotent per chunk id. */
	add(result: TransformResult): void {
		if (!result.serverChunks) return
		for (const chunk of result.serverChunks) {
			if (!this.chunks.has(chunk.id)) this.chunks.set(chunk.id, chunk)
		}
	}

	/** Whether any server-fn chunks have been recorded. */
	get isEmpty(): boolean {
		return this.chunks.size === 0
	}

	/** Every recorded chunk, in insertion order. */
	all(): ServerFnChunk[] {
		return [...this.chunks.values()]
	}

	/** Build the manifest object for the recorded chunks. */
	manifest(): RspackServerFnManifest {
		const base = buildServerFnManifest([{ code: '', sideEffects: false, serverChunks: this.all() }])
		const out: RspackServerFnManifest = {}
		for (const chunk of this.all()) {
			const entry = base[chunk.id]
			if (entry === undefined) continue
			out[chunk.id] = {
				exportName: entry.exportName,
				chunkRef: entry.chunkRef,
				asset: serverFnAssetName(chunk),
			}
		}
		return out
	}
}

/**
 * Per-compilation registry store. The loader and the plugin run in the same
 * process but exchange no direct handle, so they share the registry keyed by the
 * active compilation object: the loader feeds it via {@link registryFor} as it
 * walks modules, and the plugin reads the same instance at build settle to emit
 * the manifest. A `WeakMap` lets a finished compilation's registry be collected.
 *
 * When no compilation key is available (a minimal/test loader context), a single
 * process-wide registry under {@link PROCESS_REGISTRY_KEY} is used instead.
 */
const registries = new WeakMap<object, TreatyServerFnRegistry>()

/** Fallback key for contexts without a compilation (tests, minimal hosts). */
const PROCESS_REGISTRY_KEY: object = {}

/**
 * The {@link TreatyServerFnRegistry} for a compilation, created on first use.
 * Pass the same `compilation` object from the loader and the plugin to share one
 * registry; pass `undefined` to get the process-wide fallback registry.
 */
export function registryFor(compilation: object | undefined): TreatyServerFnRegistry {
	const key = compilation ?? PROCESS_REGISTRY_KEY
	let registry = registries.get(key)
	if (registry === undefined) {
		registry = new TreatyServerFnRegistry()
		registries.set(key, registry)
	}
	return registry
}

/**
 * Serialize a registry's manifest to the JSON text written as the build asset.
 * Stable key order (sorted by fn id) so byte output is deterministic across
 * builds — a clean diff and a cache-friendly asset.
 */
export function serverFnManifestAsset(registry: TreatyServerFnRegistry): string {
	const manifest = registry.manifest()
	const ordered: RspackServerFnManifest = {}
	for (const id of Object.keys(manifest).sort()) {
		ordered[id] = manifest[id]!
	}
	return `${JSON.stringify(ordered, null, 2)}\n`
}

/**
 * The client-side lazy binding for ONE server fn. Replaces the compiler's
 * `clientBinding` static import with a dynamic `import()` of the fn's emitted
 * server asset, so Rspack code-splits the fn into its own async chunk and the
 * client module carries only this boundary — never the fn body.
 *
 * The named `webpackChunkName` magic comment pins the async chunk to the fn's
 * stable id, so the split chunk is named for the fn (matching the manifest's
 * `chunkRef`). The export is an async proxy that loads the chunk on first call
 * and forwards to the fn's named export.
 */
function clientLazyBinding(chunk: ServerFnChunk): string {
	const spec = JSON.stringify(`./${serverFnAssetName(chunk)}`)
	const name = JSON.stringify(chunk.id)
	const ref = JSON.stringify(chunk.exportName)
	// A lazy, code-split binding: the fn body lives in the async chunk; only this
	// proxy reaches the client module.
	return [
		`export const ${chunk.exportName} = (...args) =>`,
		`\timport(/* webpackChunkName: ${name} */ ${spec}).then((m) => m[${ref}](...args));`,
	].join('\n')
}

/**
 * Emit a transformed module's server-fn chunks as Rspack output and return the
 * CLIENT module text.
 *
 * For each {@link ServerFnChunk} on `result`:
 *   - emit its server-side `code` as the asset `<chunkId>.server.js` (the body
 *     becomes its own loadable output unit — never part of the client chunk), and
 *   - record it in `registry` for the aggregate manifest asset.
 *
 * The returned text is the compiler's already body-free `result.code` (client Ivy
 * JS) with each fn's lazy client binding appended. When `result` has no server
 * chunks the original `result.code` is returned unchanged.
 */
export function emitServerFnChunks(
	emitter: ServerChunkEmitter,
	registry: TreatyServerFnRegistry,
	result: TransformResult
): string {
	if (!result.serverChunks || result.serverChunks.length === 0) return result.code

	registry.add(result)
	const bindings: string[] = []
	for (const chunk of result.serverChunks) {
		emitter.emitFile(serverFnAssetName(chunk), chunk.code)
		bindings.push(clientLazyBinding(chunk))
	}

	// Client module = body-free Ivy JS + per-fn lazy bindings. The bindings are the
	// ONLY trace of the server fns in the client graph.
	const head = result.code.endsWith('\n') ? result.code : `${result.code}\n`
	return `${head}\n${bindings.join('\n\n')}\n`
}
