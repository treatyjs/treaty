/**
 * @module
 *
 * The file-by-file compiler core: {@link TreatyCompiler}. This is the shared
 * layer every Treaty bundler plugin builds on. It owns extension routing, an
 * incremental content-hash cache, deleted-file eviction, and the tree-shaking
 * metadata helpers — and nothing bundler-specific.
 *
 * Treaty is a compiler, not a host: actual lowering to Ivy JS happens in the
 * Rust authoring compiler reached through {@link ./addon}. This module only
 * decides which addon entry point a given file id routes to, caches the result,
 * and applies dead-code annotations.
 */

import { compileSource, compileTreaty, type CompiledComponent } from './addon.js'
import { contentHash, IncrementalCache, type CacheStats } from './cache.js'
import {
	annotatePureFactories,
	dropUnusedServerFns,
	PURE_MODULE,
	type SideEffectsDescriptor,
} from './treeshake.js'
import type { TransformResult, TreatyCompilerOptions, TreatyFileKind } from './types.js'

/** Error raised when the Rust compiler reports one or more diagnostics. */
export class TreatyCompileError extends Error {
	constructor(
		readonly id: string,
		readonly errors: readonly string[]
	) {
		super(`Treaty failed to compile ${id}:\n${errors.join('\n')}`)
		this.name = 'TreatyCompileError'
	}
}

/** Map a file id (path or url) to the authoring kind Treaty owns, or `null`. */
export function classify(id: string): TreatyFileKind | null {
	// Strip query/hash suffixes a bundler may append (e.g. `?foo`, `#bar`).
	const clean = id.replace(/[?#].*$/, '').toLowerCase()
	if (clean.endsWith('.treaty')) return 'treaty'
	if (clean.endsWith('.tsx') || clean.endsWith('.tjsx')) return 'jsx'
	if (clean.endsWith('.ts') && !clean.endsWith('.d.ts')) return 'component'
	return null
}

/**
 * A plain `.ts` file is only Treaty's to compile when it declares an Angular
 * component. We cheaply pre-screen for an `@Component` decorator so that
 * ordinary TypeScript modules are left untouched (the compiler returns `null`
 * for them, letting the bundler's normal TS pipeline handle them).
 */
function isAngularComponentSource(code: string): boolean {
	return /@Component\s*\(/.test(code)
}

/** The file-by-file Treaty compiler shared by every bundler plugin. */
export class TreatyCompiler {
	private readonly cacheEnabled: boolean
	private readonly annotatePure: boolean
	private readonly dropServerFns: boolean
	private readonly cache: IncrementalCache
	/** Reverse index: imported id -> set of importer ids that referenced it. */
	private readonly dependents = new Map<string, Set<string>>()

	constructor(options: TreatyCompilerOptions = {}) {
		this.cacheEnabled = options.cache ?? true
		this.annotatePure = options.annotatePure ?? true
		this.dropServerFns = options.dropUnusedServerFns ?? true
		this.cache = new IncrementalCache()
	}

	/** The `sideEffects` descriptor bundlers should use for emitted modules. */
	get sideEffects(): SideEffectsDescriptor {
		return PURE_MODULE
	}

	/** Whether this compiler owns `id` (by extension / component screen). */
	owns(id: string, code?: string): boolean {
		const kind = classify(id)
		if (kind === null) return false
		if (kind === 'component') {
			// Without source we can't be sure; assume yes and let transform decide.
			return code === undefined ? true : isAngularComponentSource(code)
		}
		return true
	}

	/**
	 * Transform `code` for file `id` to Ivy JS, or return `null` for files this
	 * compiler does not own. Identical content for the same id is served from the
	 * incremental cache.
	 *
	 * @throws {TreatyCompileError} when the Rust compiler reports diagnostics.
	 */
	transform(id: string, code: string): TransformResult | null {
		const kind = classify(id)
		if (kind === null) return null
		if (kind === 'component' && !isAngularComponentSource(code)) return null

		const hash = contentHash(code)
		if (this.cacheEnabled) {
			const cached = this.cache.get(id, hash)
			if (cached !== undefined) return cached
		}

		const compiled = this.lower(id, kind, code)
		if (compiled.errors.length > 0) {
			throw new TreatyCompileError(id, compiled.errors)
		}

		const result = this.postProcess(compiled.code)
		this.recordDependents(id, code)
		if (this.cacheEnabled) this.cache.set(id, hash, result)
		return result
	}

	/** Route to the correct addon entry point for the file kind. */
	private lower(id: string, kind: TreatyFileKind, code: string): CompiledComponent {
		switch (kind) {
			case 'treaty':
				return compileTreaty(code, id)
			case 'jsx':
			case 'component':
				return compileSource(code)
		}
	}

	/** Apply tree-shaking annotations to emitted Ivy JS. */
	private postProcess(code: string): TransformResult {
		let out = code
		if (this.dropServerFns) out = dropUnusedServerFns(out)
		if (this.annotatePure) out = annotatePureFactories(out)
		return { code: out, sideEffects: false }
	}

	/** Index the importers a module references, for {@link onDelete}. */
	private recordDependents(id: string, code: string): void {
		const importRe = /\bfrom\s+['"]([^'"]+)['"]/g
		let m: RegExpExecArray | null
		while ((m = importRe.exec(code)) !== null) {
			const target = m[1]
			if (!target) continue
			let set = this.dependents.get(target)
			if (!set) {
				set = new Set<string>()
				this.dependents.set(target, set)
			}
			set.add(id)
		}
	}

	/** Evict `id` from the cache. Returns true if an entry was removed. */
	invalidate(id: string): boolean {
		return this.cache.invalidate(id)
	}

	/**
	 * Deleted-file hook: evict `id` from the cache and from the dependents index,
	 * and return the ids of any modules that imported it so the caller can
	 * re-evaluate them. Kept intentionally simple — it reports dependents but
	 * does not transitively invalidate them.
	 */
	onDelete(id: string): readonly string[] {
		this.cache.invalidate(id)
		const dependents = this.dependents.get(id)
		const affected = dependents ? [...dependents] : []
		this.dependents.delete(id)
		// Drop `id` from every other module's importer set.
		for (const set of this.dependents.values()) set.delete(id)
		return affected
	}

	/** Current cache statistics (hit/miss/invalidation counts, live size). */
	stats(): CacheStats {
		return this.cache.stats()
	}

	/** Clear all cached entries (counters are preserved). */
	clearCache(): void {
		this.cache.clear()
	}
}

/** Factory mirroring `new TreatyCompiler(options)`. */
export function createTreatyCompiler(options?: TreatyCompilerOptions): TreatyCompiler {
	return new TreatyCompiler(options)
}
