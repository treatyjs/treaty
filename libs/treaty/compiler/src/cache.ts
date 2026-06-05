/**
 * @module
 *
 * Incremental transform cache for {@link TreatyCompiler}. Entries are keyed by
 * file id and validated by a content hash, so re-transforming byte-identical
 * source is a cache hit and never re-enters the Rust compiler.
 *
 * The hash is a fast, dependency-free FNV-1a over the source text. It is used
 * only to detect content change for a given id, not as a cryptographic digest.
 */

import type { TransformResult } from './types.js'

/** A cached transform: the content hash it was produced from plus the result. */
interface CacheEntry {
	readonly hash: string
	readonly result: TransformResult
}

/** Counters exposed for tests and bundler diagnostics. */
export interface CacheStats {
	/** Number of transforms served from the cache. */
	readonly hits: number
	/** Number of transforms that had to run the compiler. */
	readonly misses: number
	/** Number of cache entries dropped via {@link IncrementalCache.invalidate}. */
	readonly invalidations: number
	/** Current number of live entries. */
	readonly size: number
}

/**
 * Compute a stable 32-bit FNV-1a hash of `input`, returned as an 8-char hex
 * string. Deterministic and allocation-light; collisions are astronomically
 * unlikely for source files and only ever cause a (harmless) recompile.
 */
export function contentHash(input: string): string {
	let h = 0x811c9dc5
	for (let i = 0; i < input.length; i++) {
		h ^= input.charCodeAt(i)
		// 32-bit FNV prime multiply via shifts to stay in int range.
		h = (h + ((h << 1) + (h << 4) + (h << 7) + (h << 8) + (h << 24))) >>> 0
	}
	return h.toString(16).padStart(8, '0')
}

/** Content-addressed incremental cache with hit/miss accounting. */
export class IncrementalCache {
	private readonly entries = new Map<string, CacheEntry>()
	private hits = 0
	private misses = 0
	private invalidations = 0

	/**
	 * Return the cached result for `id` if it was produced from source whose
	 * hash matches `hash`; otherwise record a miss and return `undefined`.
	 */
	get(id: string, hash: string): TransformResult | null | undefined {
		const entry = this.entries.get(id)
		if (entry && entry.hash === hash) {
			this.hits++
			return entry.result
		}
		this.misses++
		return undefined
	}

	/** Store `result` for `id` under content `hash`, replacing any prior entry. */
	set(id: string, hash: string, result: TransformResult): void {
		this.entries.set(id, { hash, result })
	}

	/** Drop the entry for `id`. Returns true if an entry was removed. */
	invalidate(id: string): boolean {
		const had = this.entries.delete(id)
		if (had) this.invalidations++
		return had
	}

	/** Whether a (possibly stale) entry exists for `id`. */
	has(id: string): boolean {
		return this.entries.has(id)
	}

	/** Remove every entry without resetting the counters. */
	clear(): void {
		this.entries.clear()
	}

	/** Snapshot of the current counters. */
	stats(): CacheStats {
		return {
			hits: this.hits,
			misses: this.misses,
			invalidations: this.invalidations,
			size: this.entries.size,
		}
	}
}
