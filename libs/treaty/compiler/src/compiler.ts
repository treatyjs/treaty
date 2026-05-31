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
 *
 * Source files (`.tsx`/`.tjsx` and `@Component` `.ts`) route through the unified
 * authoring front-end, so BARE JSX (`export default` returning JSX with no
 * `@Component`) lowers to Ivy alongside decorated classes. `.treaty` files keep
 * their dedicated single-file-component entry point.
 *
 * Cold builds can lower a whole batch in one round trip via
 * {@link TreatyCompiler.transformMany}, which fans the work out across the Rust
 * addon's parallel `compileMany` and applies the same cache + dead-code metadata
 * per file (so any already-cached file skips the batch).
 */

import {
	compileMany,
	compileSource,
	compileTreaty,
	compileUnifiedSource,
	type AuthoringFile,
	type CompiledAuthoring,
	type CompiledAuthoringEntry,
} from './addon.js'
import { contentHash, IncrementalCache, type CacheStats } from './cache.js'
import { splitServerModule } from './server-chunks.js'
import {
	annotatePureFactories,
	dropUnusedServerFns,
	PURE_MODULE,
	type SideEffectsDescriptor,
} from './treeshake.js'
import type {
	TransformInput,
	TransformResult,
	TreatyCompilerOptions,
	TreatyFileKind,
} from './types.js'

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

/**
 * The unified JSX front-end (used for `.tsx`/`.tjsx`) requires the module to
 * declare a JSX component (a default-export or named function/arrow returning
 * JSX). A `.tsx` that instead carries a classic `@Component` class with a string
 * `template` (and no JSX return) produces this specific diagnostic. We detect it
 * so such files can fall back to the `@Component`-source entry point rather than
 * failing the build — preserving both bare-JSX and `@Component`-in-`.tsx` support.
 */
function isMissingJsxComponentError(errors: readonly string[]): boolean {
	return errors.some((e) => /no component .*returning JSX.* found/i.test(e))
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

		return this.finishLower(id, compiled, code, hash)
	}

	/**
	 * Batch transform for COLD builds: lower many files in a single round trip
	 * through the Rust addon's parallel `compileMany`. Designed for the initial
	 * build pass where a bundler hands the compiler its whole owned-file set at
	 * once.
	 *
	 * Behaviour mirrors {@link transform} exactly, per file:
	 *   - files this compiler does not own (extension/`@Component` screen) yield a
	 *     `null` result in the returned array, in input order;
	 *   - `.treaty` files are lowered through their dedicated entry point (they are
	 *     not part of the unified source batch) so routing stays identical to the
	 *     per-file path;
	 *   - already-cached files (byte-identical content for the same id) are served
	 *     from the cache and SKIP the batch entirely;
	 *   - the same dead-code / pure annotations and dependents indexing are applied;
	 *   - results are cached so a subsequent {@link transform} is a hit.
	 *
	 * @throws {TreatyCompileError} for the first file the Rust compiler reports
	 *   diagnostics on (matching {@link transform}'s fail-fast contract).
	 */
	transformMany(files: readonly TransformInput[]): (TransformResult | null)[] {
		const out: (TransformResult | null)[] = new Array(files.length).fill(null)
		// Files to lower through the unified parallel batch, with their slot index.
		const batch: { index: number; id: string; code: string; hash: string }[] = []
		const sources: AuthoringFile[] = []

		for (let i = 0; i < files.length; i++) {
			const { id, code } = files[i]!
			const kind = classify(id)
			if (kind === null) continue
			if (kind === 'component' && !isAngularComponentSource(code)) continue

			const hash = contentHash(code)
			// Cache hit: serve and skip the batch, exactly like transform().
			if (this.cacheEnabled) {
				const cached = this.cache.get(id, hash)
				if (cached !== undefined) {
					out[i] = cached
					continue
				}
			}

			if (kind === 'treaty') {
				// `.treaty` is not part of the unified source batch; lower in place
				// so routing matches the single-file path.
				const compiled = this.lower(id, kind, code)
				if (compiled.errors.length > 0) {
					throw new TreatyCompileError(id, compiled.errors)
				}
				out[i] = this.finishLower(id, compiled, code, hash)
				continue
			}

			batch.push({ index: i, id, code, hash })
			sources.push({ id, code })
		}

		if (sources.length > 0) {
			const compiled: CompiledAuthoringEntry[] = compileMany(sources)
			for (let b = 0; b < batch.length; b++) {
				const slot = batch[b]!
				let entry: CompiledAuthoring = compiled[b]!
				// Mirror the per-file `.tsx` fallback: a batched JSX module that is
				// really an `@Component` class retries via the `@Component`-source path.
				if (
					entry.errors.length > 0 &&
					isMissingJsxComponentError(entry.errors) &&
					isAngularComponentSource(slot.code)
				) {
					entry = compileSource(slot.code)
				}
				if (entry.errors.length > 0) {
					throw new TreatyCompileError(slot.id, entry.errors)
				}
				out[slot.index] = this.finishLower(slot.id, entry, slot.code, slot.hash)
			}
		}

		return out
	}

	/**
	 * Post-process a freshly lowered result and commit it: apply dead-code
	 * annotations, record import dependents, and store in the incremental cache.
	 * Shared by the per-file and batch paths so they stay byte-identical.
	 */
	private finishLower(
		id: string,
		compiled: CompiledAuthoring,
		code: string,
		hash: string
	): TransformResult {
		const result = this.postProcess(id, compiled)
		this.recordDependents(id, code)
		if (this.cacheEnabled) this.cache.set(id, hash, result)
		return result
	}

	/** Route to the correct addon entry point for the file kind. */
	private lower(id: string, kind: TreatyFileKind, code: string): CompiledAuthoring {
		switch (kind) {
			case 'treaty':
				return compileTreaty(code, id)
			case 'jsx':
				// Unified front-end: bare JSX (`export default`/named fn returning
				// JSX) lowers to Ivy. A `.tsx`/`.tjsx` that is actually a classic
				// `@Component` class (string template, no JSX) is not a JSX module —
				// fall back to the `@Component`-source path so it still compiles.
				return this.lowerJsx(id, code)
			case 'component':
				// `.ts` `@Component`: the unified front-end routes by extension to the
				// `@Component` path, so this handles it directly.
				return compileUnifiedSource(code, id)
		}
	}

	/**
	 * Lower a `.tsx`/`.tjsx` module. Tries the unified JSX front-end first (so
	 * bare JSX lowers to Ivy); if it reports only the "no JSX component" diagnostic
	 * and the source carries an `@Component`, retries via the `@Component`-source
	 * entry point. Any other diagnostic is returned as-is for the caller to throw.
	 */
	private lowerJsx(id: string, code: string): CompiledAuthoring {
		const jsx = compileUnifiedSource(code, id)
		if (
			jsx.errors.length > 0 &&
			isMissingJsxComponentError(jsx.errors) &&
			isAngularComponentSource(code)
		) {
			return compileSource(code)
		}
		return jsx
	}

	/**
	 * Apply tree-shaking annotations to emitted Ivy JS, and — when the file
	 * declared server functions — decompose the single `serverModule` blob into
	 * per-fn {@link ServerFnChunk}s so a bundler can code-split each fn into its
	 * own loadable chunk. `serverModule` is retained as the back-compat blob.
	 */
	private postProcess(id: string, compiled: CompiledAuthoring): TransformResult {
		let out = compiled.code
		if (this.dropServerFns) out = dropUnusedServerFns(out)
		if (this.annotatePure) out = annotatePureFactories(out)
		if (compiled.serverModule === undefined) {
			return { code: out, sideEffects: false }
		}
		const serverChunks = splitServerModule(id, compiled.serverModule)
		return serverChunks.length > 0
			? { code: out, serverModule: compiled.serverModule, serverChunks, sideEffects: false }
			: { code: out, serverModule: compiled.serverModule, sideEffects: false }
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
