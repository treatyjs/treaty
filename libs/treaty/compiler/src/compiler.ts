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
	buildImportedSelectors,
	buildSelectorRegistry,
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
	ImportedSelectorMap,
	ProjectSelectors,
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
 * decorated class — a `@Component`, `@Directive`, `@Pipe`, `@Injectable`, or
 * `@NgModule`. We cheaply pre-screen for ANY of those decorators so that the
 * unified authoring front-end lowers each to its Ivy definition
 * (`ɵɵdefineComponent` / `ɵɵdefineDirective` / `ɵɵdefinePipe` / `ɵfac` +
 * `ɵɵdefineInjectable` / `ɵɵdefineNgModule`) AOT. A `.ts` carrying none of these
 * is an ordinary TypeScript module: the compiler returns `null` for it, letting
 * the bundler's normal TS pipeline handle it.
 *
 * Screening only `@Component` (the previous behaviour) left a `@Directive`/`@Pipe`/
 * `@Injectable`/`@NgModule` `.ts` to fall through to esbuild's raw decorator
 * transform (`__decorateClass([Directive({…})], …)`), shipping a decorated class
 * with NO Ivy definition — so Angular fell to its JIT compiler at runtime and
 * crashed ("needs to be compiled using the JIT compiler"). Recognising every kind
 * keeps the whole module graph AOT. The regex is a deliberately cheap pre-screen
 * (a false positive merely routes a module to the Rust front-end, which itself
 * AST-detects the real decorators and passes a non-Angular `.ts` through unchanged).
 */
const ANGULAR_DECORATOR_RE = /@(?:Component|Directive|Pipe|Injectable|NgModule)\s*\(/
function isAngularDecoratorSource(code: string): boolean {
	return ANGULAR_DECORATOR_RE.test(code)
}

/**
 * A plain `.ts` (or `.tsx`/`.tjsx`) module that carries NO Angular decorator is
 * still Treaty's to compile when it declares a SERVER FUNCTION, because the Rust
 * front-end must lift those bodies out of the client bundle (and the backend dev
 * runtime needs the extracted server module). Without this screen a file-level
 * `'use server'` module — e.g. `todos.server.ts` exporting `listTodos` — would
 * short-circuit to `null` here and be served by the bundler's plain-TS pipeline,
 * shipping the full body (and any secret in it) to the browser: the exact
 * client-leak the Rust extraction was hardened to prevent (it never runs if the
 * TS gate returns early).
 *
 * The markers mirror the Rust extractor (`extract_server_block`):
 *   * a file-level / function-level `'use server'` or `'use websocket'` string
 *     directive,
 *   * a top-level `server { … }` / `server:lang { … }` block, or
 *   * a `$$`-suffixed declaration name (the inline server-fn marker).
 *
 * This is a deliberately cheap pre-screen; a false positive merely routes the
 * module to the Rust front-end, which AST-detects the real markers and passes a
 * non-server module through unchanged (returning no `serverModule`).
 */
const SERVER_MARKER_RE =
	/(?:^|[\n;{])\s*['"]use (?:server|websocket)['"]|(?:^|[\n;{}])\s*server(?::[A-Za-z_$][\w$]*)?\s*\{|\b[A-Za-z_$][\w$]*\$\$\s*(?:=|\()/m
function hasServerMarker(code: string): boolean {
	return SERVER_MARKER_RE.test(code)
}

/**
 * Whether a `'component'`-kind `.ts`/`.tsx`/`.tjsx` module should be routed to
 * the Rust front-end: it carries an Angular decorator OR a server-fn marker.
 * A module with neither is an ordinary TypeScript module the bundler handles.
 */
function isTreatyComponentSource(code: string): boolean {
	return isAngularDecoratorSource(code) || hasServerMarker(code)
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
	/**
	 * The project-wide `className -> selector` map for CROSS-MODULE selector
	 * resolution, scanned once via {@link prewarmSelectorRegistry}. `undefined`
	 * until a plugin prewarms it; while unset, every transform takes the
	 * byte-identical fold path (so the registry is strictly opt-in).
	 */
	private projectSelectors: ProjectSelectors | undefined

	constructor(options: TreatyCompilerOptions = {}) {
		this.cacheEnabled = options.cache ?? true
		this.annotatePure = options.annotatePure ?? true
		this.dropServerFns = options.dropUnusedServerFns ?? true
		this.cache = new IncrementalCache()
	}

	/**
	 * COLD-BUILD PREWARM for cross-module selector resolution. Scan every
	 * first-party `.ts` under `rootDir` (via the Rust selector scanner) into the
	 * project-wide `className -> selector` map and hold it for the build. A bundler
	 * plugin calls this ONCE in `buildStart`; afterwards {@link transform} /
	 * {@link transformMany} derive each file's per-file {@link ImportedSelectorMap}
	 * from it so an IMPORTED component used by its conventional `@Component` selector
	 * (e.g. `<app-stat-card>` for `class StatCard`) resolves as a real dependency.
	 *
	 * Returns the number of selectors discovered (0 ⇒ nothing first-party owns a
	 * selector, so every transform stays on the fold path). Re-callable: the latest
	 * scan replaces the previous map.
	 */
	prewarmSelectorRegistry(rootDir: string): number {
		const project = buildSelectorRegistry(rootDir)
		this.projectSelectors = project
		let count = 0
		for (const _ in project) count++
		return count
	}

	/** Whether a project-wide selector registry has been prewarmed (and is non-empty). */
	hasSelectorRegistry(): boolean {
		if (this.projectSelectors === undefined) return false
		for (const _ in this.projectSelectors) return true
		return false
	}

	/**
	 * The per-file {@link ImportedSelectorMap} for `code`, derived from the
	 * prewarmed project map — or `undefined` when no registry was prewarmed, or no
	 * import in `code` resolves to a known selector (the fold path). Only computed
	 * for the base-Angular `'component'` kind: `.treaty`/JSX front-ends key on the
	 * filename convention, not cross-module imports, so the registry would never
	 * apply there (and the Rust entry ignores it for them).
	 */
	private importedSelectorsFor(
		kind: TreatyFileKind,
		code: string
	): ImportedSelectorMap | undefined {
		if (kind !== 'component' || this.projectSelectors === undefined) return undefined
		return buildImportedSelectors(code, this.projectSelectors)
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
			return code === undefined ? true : isTreatyComponentSource(code)
		}
		return true
	}

	/**
	 * Transform `code` for file `id` to Ivy JS, or return `null` for files this
	 * compiler does not own. Identical content for the same id is served from the
	 * incremental cache.
	 *
	 * `importedSelectors` is the OPTIONAL per-file cross-module selector registry
	 * ({@link ImportedSelectorMap}). When omitted, it is derived from the prewarmed
	 * project map (see {@link prewarmSelectorRegistry}); pass one explicitly to
	 * override (or `undefined` with no prewarm to force the fold path). It only
	 * affects the base-Angular `'component'` kind; for every other kind, and absent a
	 * registry, the output is byte-for-byte identical to the no-registry path.
	 *
	 * @throws {TreatyCompileError} when the Rust compiler reports diagnostics.
	 */
	transform(
		id: string,
		code: string,
		importedSelectors?: ImportedSelectorMap
	): TransformResult | null {
		const kind = classify(id)
		if (kind === null) return null
		if (kind === 'component' && !isTreatyComponentSource(code)) return null

		const hash = contentHash(code)
		if (this.cacheEnabled) {
			const cached = this.cache.get(id, hash)
			if (cached !== undefined) return cached
		}

		const registry = importedSelectors ?? this.importedSelectorsFor(kind, code)
		const compiled = this.lower(id, kind, code, registry)
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
	transformMany(
		files: readonly TransformInput[],
		registries: readonly (ImportedSelectorMap | undefined)[] = []
	): (TransformResult | null)[] {
		const out: (TransformResult | null)[] = new Array(files.length).fill(null)
		// Files to lower through the unified parallel batch, with their slot index.
		const batch: { index: number; id: string; code: string; hash: string }[] = []
		const sources: AuthoringFile[] = []
		// Per-batched-file cross-module registry, positionally aligned with `sources`.
		const batchRegistries: (ImportedSelectorMap | undefined)[] = []

		for (let i = 0; i < files.length; i++) {
			const { id, code } = files[i]!
			const kind = classify(id)
			if (kind === null) continue
			if (kind === 'component' && !isTreatyComponentSource(code)) continue

			const hash = contentHash(code)
			// Cache hit: serve and skip the batch, exactly like transform().
			if (this.cacheEnabled) {
				const cached = this.cache.get(id, hash)
				if (cached !== undefined) {
					out[i] = cached
					continue
				}
			}

			// The per-file registry: an explicit one (positionally matched to `files`)
			// takes precedence; otherwise derive from the prewarmed project map. Only
			// the base-Angular `'component'` kind consumes it.
			const registry = registries[i] ?? this.importedSelectorsFor(kind, code)

			if (kind === 'treaty') {
				// `.treaty` is not part of the unified source batch; lower in place
				// so routing matches the single-file path. (No cross-module registry
				// applies to `.treaty`, which keys on the filename convention.)
				const compiled = this.lower(id, kind, code, registry)
				if (compiled.errors.length > 0) {
					throw new TreatyCompileError(id, compiled.errors)
				}
				out[i] = this.finishLower(id, compiled, code, hash)
				continue
			}

			batch.push({ index: i, id, code, hash })
			sources.push({ id, code })
			batchRegistries.push(registry)
		}

		if (sources.length > 0) {
			const compiled: CompiledAuthoringEntry[] = compileMany(sources, batchRegistries)
			for (let b = 0; b < batch.length; b++) {
				const slot = batch[b]!
				let entry: CompiledAuthoring = compiled[b]!
				// Mirror the per-file `.tsx` fallback: a batched JSX module that is
				// really a classic decorated class (no JSX return) retries via the
				// `@Component`-source path, which lowers every Angular decorator kind.
				if (
					entry.errors.length > 0 &&
					isMissingJsxComponentError(entry.errors) &&
					isAngularDecoratorSource(slot.code)
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

	/**
	 * Route to the correct addon entry point for the file kind, threading the
	 * OPTIONAL per-file cross-module selector registry. The registry only affects
	 * the base-Angular `'component'` path (the cross-module-import case it exists
	 * for); for `.treaty`/JSX it is `undefined` and the output is unchanged.
	 */
	private lower(
		id: string,
		kind: TreatyFileKind,
		code: string,
		importedSelectors?: ImportedSelectorMap
	): CompiledAuthoring {
		switch (kind) {
			case 'treaty':
				return compileTreaty(code, id)
			case 'jsx':
				// Unified front-end: bare JSX (`export default`/named fn returning
				// JSX) lowers to Ivy. A `.tsx`/`.tjsx` that is actually a classic
				// `@Component` class (string template, no JSX) is not a JSX module —
				// fall back to the `@Component`-source path so it still compiles.
				return this.lowerJsx(id, code, importedSelectors)
			case 'component':
				// `.ts` `@Component`: the unified front-end routes by extension to the
				// `@Component` path, so this handles it directly — with the per-file
				// cross-module selector registry when one resolved.
				return compileUnifiedSource(code, id, importedSelectors)
		}
	}

	/**
	 * Lower a `.tsx`/`.tjsx` module. Tries the unified JSX front-end first (so
	 * bare JSX lowers to Ivy); if it reports only the "no JSX component" diagnostic
	 * and the source carries a classic Angular decorator, retries via the
	 * `@Component`-source entry point (which lowers every Angular decorator kind).
	 * Any other diagnostic is returned as-is for the caller to throw.
	 */
	private lowerJsx(
		id: string,
		code: string,
		importedSelectors?: ImportedSelectorMap
	): CompiledAuthoring {
		const jsx = compileUnifiedSource(code, id, importedSelectors)
		if (
			jsx.errors.length > 0 &&
			isMissingJsxComponentError(jsx.errors) &&
			isAngularDecoratorSource(code)
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
	 *
	 * The additive Source Map v3 JSON the Rust addon produced (`compiled.map`) is
	 * threaded onto the result unchanged. CLIENT PRIVACY: the addon has already
	 * redacted every lifted server-fn body from the map's `sourcesContent`, so the
	 * map shipped to the client never carries server-fn source text — see
	 * {@link assertNoServerBodyInMap}, the test-time guard for this invariant.
	 */
	private postProcess(id: string, compiled: CompiledAuthoring): TransformResult {
		let out = compiled.code
		if (this.dropServerFns) out = dropUnusedServerFns(out)
		if (this.annotatePure) out = annotatePureFactories(out)
		const map = compiled.map
		if (compiled.serverModule === undefined) {
			return map === undefined
				? { code: out, sideEffects: false }
				: { code: out, map, sideEffects: false }
		}
		const serverChunks = splitServerModule(id, compiled.serverModule)
		const base: TransformResult =
			serverChunks.length > 0
				? { code: out, serverModule: compiled.serverModule, serverChunks, sideEffects: false }
				: { code: out, serverModule: compiled.serverModule, sideEffects: false }
		return map === undefined ? base : { ...base, map }
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
