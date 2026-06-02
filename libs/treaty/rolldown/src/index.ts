/**
 * @module
 *
 * `@treaty/rolldown` — the Rolldown plugin for Treaty authoring formats. It wires
 * the framework-agnostic {@link TreatyCompiler} core from `@treaty/compiler` into
 * Rolldown's plugin lifecycle so that `.treaty`, `.tsx`, `.tjsx`, and Angular
 * `@Component` `.ts` files are lowered to Ivy JS during a Rolldown build, and it
 * links published *partial*-compiled `@angular/*` libraries in `node_modules` to
 * AOT (`ɵɵngDeclare*` → `ɵɵdefine*`) so the bundle needs NO JIT and NO
 * `@angular/compiler` — the exact same guarantee `@treaty/vite` provides.
 *
 * Treaty is a compiler, not a host: this plugin does not reimplement any lowering
 * or linking. It delegates every owned authoring file to the core's `transform`
 * (which routes through the Rust authoring compiler via `@treaty/compiler`), and
 * delegates partial-library linking to the SHARED, Rust-backed
 * `linkPartialCode` / `isPartialModule` from `@treaty/ts-vite` — the very helpers
 * the `@treaty/vite` `LinkPartialPlugin` calls. The plugin's only job is Rolldown
 * integration: extension ownership, the incremental cache, server-fn chunking via
 * `emitFile`, and watch-mode invalidation.
 *
 * Rolldown's plugin interface is Rollup-compatible, so every hook used here
 * (`buildStart`, `resolveId`, `load`, `transform`, `watchChange`,
 * `generateBundle`) has the same signature it has in Rollup/Vite. The Vite plugin
 * is itself a Rollup-native design with no Vite-internal value APIs, so the two
 * share the same mechanisms. The pieces that ARE Vite-only — the dev-server
 * content-type middleware, the `/__server/*` dev backend, the `optimizeDeps`
 * esbuild prebundle linker, the `index.html` JIT-script guard, and esbuild
 * type-stripping of `.treaty`/`.tjsx` — have no Rolldown analogue: Rolldown is a
 * bundler with no dependency prebundling, no dev server, and a native TS/TSX
 * parser, so a single Rollup-style `transform` covers both authoring lowering and
 * partial linking with nothing extra needed.
 */

import { readFile } from 'node:fs/promises'
import {
	createTreatyCompiler,
	classify,
	type ServerFnChunk,
	type TransformInput,
	type TreatyCompiler,
	type TreatyCompilerOptions,
} from '@treaty/compiler'
import { isPartialModule, linkPartialCode } from '@treaty/ts-vite'
import type { EmittedAsset, Plugin } from 'rolldown'
import {
	CLIENT_VIRTUAL_PREFIX,
	MANIFEST_FILE_NAME,
	SERVER_VIRTUAL_PREFIX,
	buildBuildManifest,
	clientStubModule,
	injectClientBindings,
	matchServerChunkSpecifier,
	serverChunkFileName,
	type TrackedServerFn,
} from './server-chunks.js'

/** Public options for {@link treaty}. */
export interface PluginOptions extends TreatyCompilerOptions {
	/**
	 * Emit a JSON source map alongside the transformed code when the core
	 * produces one. Defaults to `true`. When `false`, a null map is returned so
	 * Rolldown skips source-map work for Treaty modules.
	 */
	readonly sourceMap?: boolean
	/**
	 * Cold-build prewarm: a list of absolute paths to owned authoring files to
	 * batch-compile up front via the core's `transformMany` (one parallel round
	 * trip through the Rust addon). Runs once during `buildStart`. The results
	 * populate the incremental cache, so the per-module `transform` calls Rolldown
	 * makes during the build are served as cache hits instead of re-entering the
	 * compiler one file at a time.
	 *
	 * Rolldown (like Rollup) is a pull-based pipeline with no hook that hands the
	 * plugin the full owned-file set, so this batch path is opt-in: pass the
	 * entry/owned authoring files you want compiled eagerly. When omitted, the
	 * plugin uses per-file `transform` only.
	 */
	readonly prewarm?: readonly string[]
	/**
	 * Function chunking. When `true` (the default), each server function the
	 * compiler extracts from an authoring file is emitted as its OWN
	 * separately-loadable output asset (`<fn-id>.server.js`), the component code
	 * keeps only the per-fn client binding, and a `treaty-server-fns.json`
	 * manifest (fn-id -> chunk file + export name) is emitted as a build asset.
	 *
	 * This guarantees a server-fn BODY never lands in the client module graph:
	 * the body lives only in its emitted asset, while the client follows the
	 * binding to a tiny RPC stub that calls the fn's `/__server/<name>` route.
	 *
	 * Set `false` to leave server fns as the compiler's single `serverModule`
	 * blob (no per-fn code-splitting and no manifest).
	 */
	readonly functionChunking?: boolean
	/**
	 * Whether to also link published partial-compiled `@angular/*` libraries in
	 * `node_modules` (`ɵɵngDeclare*` → AOT `ɵɵdefine*`) via the shared Rust-backed
	 * linker. Defaults to `true`. This is the same de-partialling `@treaty/vite`
	 * performs in dev and prod; without it a Rolldown bundle that consumes partial
	 * Angular libraries throws "needs JIT / `@angular/compiler` not available" at
	 * runtime. Set `false` only if an embedder links partials by other means.
	 */
	readonly linkPartials?: boolean
	/**
	 * Factory used to construct the underlying {@link TreatyCompiler}. Defaults to
	 * `createTreatyCompiler` from `@treaty/compiler`. Provided as a seam so an
	 * embedder (or a test) can supply an alternative compiler implementation
	 * without changing the plugin's Rolldown wiring; production usage never sets this.
	 */
	readonly compilerFactory?: (options: TreatyCompilerOptions) => TreatyCompiler
}

/** The authoring plugin name surfaced in Rolldown logs and the plugin pipeline. */
const PLUGIN_NAME = 'rolldown-plugin-treaty'

/** The partial-linker plugin name surfaced in Rolldown logs. */
const LINK_PLUGIN_NAME = 'rolldown-plugin-treaty-link-partial'

/** Strip a bundler-appended query/hash suffix (`?foo`, `#bar`) from an id. */
function cleanId(id: string): string {
	return id.replace(/[?#].*$/, '')
}

/**
 * The signature of Ivy JS this plugin's compiler emits: the `import * as i0 from
 * "@angular/core"` namespace import the emitter always prepends, paired with one
 * of the Ivy definition members it writes (`i0.ɵɵdefine*`, a `.ɵfac =` factory,
 * or a `.ɵcmp`/`.ɵdir`/`.ɵmod`/`.ɵpipe`/`.ɵinj` static). Authoring source never
 * writes the `i0` namespace alias against `@angular/core`, so this pair only ever
 * appears in code this plugin already lowered.
 */
const IVY_NAMESPACE_IMPORT = /import\s*\*\s*as\s+i0\s+from\s*["']@angular\/core["']/
const IVY_DEFINITION = /(?:i0\.ɵɵdefine[A-Za-z]+\b|\.ɵfac\s*=|\.ɵ(?:cmp|dir|mod|pipe|inj|loc)\b)/

/**
 * Whether `code` is already lowered Ivy output this plugin (or an earlier pass)
 * produced, rather than raw authoring source. The Treaty `transform` is otherwise
 * not idempotent: feeding emitted Ivy back through the compiler raises
 * "no component … returning JSX … found" because the lowered JS has no authoring
 * component. Detecting the emitter's signature lets a second pass skip
 * recompilation and pass the already-lowered JS straight through, so each module
 * is compiled exactly once.
 */
function isLoweredIvy(code: string): boolean {
	return IVY_NAMESPACE_IMPORT.test(code) && IVY_DEFINITION.test(code)
}

/**
 * Whether the resolved id is one this plugin should attempt to transform. We
 * rely on the core's {@link classify} so ownership stays in one place; a plain
 * `.ts` is only fully claimed by the core's `transform` (which screens for an
 * `@Component` decorator and returns `null` otherwise).
 */
function isCandidate(id: string): boolean {
	return classify(cleanId(id)) !== null
}

/**
 * The minimal Rolldown `PluginContext` slice the server-fn chunking path uses:
 * `emitFile` (Rollup-compatible) to write the body asset and manifest, and
 * `resolve` (Rollup-compatible, with `skipSelf`) to defer authoring-id resolution.
 * Declared structurally so a unit test can drive the hooks without a real
 * Rolldown build context.
 */
interface TreatyPluginContext {
	emitFile(file: EmittedAsset): string
	resolve(
		source: string,
		importer: string | undefined,
		options: { skipSelf?: boolean } & Record<string, unknown>
	): Promise<{ id: string } | null>
}

/**
 * Create the Treaty Rolldown plugins. Returns an array: the Treaty authoring
 * plugin (which delegates all lowering to the shared {@link TreatyCompiler} core)
 * plus — unless disabled via {@link PluginOptions.linkPartials} — the Angular
 * partial-declaration linker plugin built around the SHARED Rust-backed
 * `linkPartialCode` from `@treaty/ts-vite`.
 *
 * The linker plugin is why a Treaty app that consumes published *partial*-compiled
 * Angular libraries (`@angular/{common,forms,router,platform-browser,core}`, whose
 * decorated classes ship as `ɵɵngDeclare*` calls) bundles with NO JIT and NO
 * `@angular/compiler`: it de-partials those libraries to AOT `ɵɵdefine*` at
 * transform time. In Vite this needed three plugins (an `optimizeDeps` prebundle
 * linker, a module-graph `transform` linker, and an `index.html` script guard)
 * because Vite prebundles deps with esbuild and serves an HTML page; Rolldown has
 * neither, so the single Rollup-style `transform` hook below links every partial
 * `node_modules` module the bundle pulls in.
 *
 * The linker logic itself is shared (one source of truth in `@treaty/ts-vite`,
 * backed by Rust); this package only wires it into a Rolldown hook. Rolldown
 * flattens nested plugin arrays, so `plugins: [treaty(...)]` works unchanged.
 */
export default function treaty(options: PluginOptions = {}): Plugin[] {
	const emitSourceMap = options.sourceMap ?? true
	const prewarmFiles = options.prewarm ?? []
	const functionChunking = options.functionChunking ?? true
	const linkPartials = options.linkPartials ?? true

	const compiler: TreatyCompiler = (options.compilerFactory ?? createTreatyCompiler)(options)

	// Server-fn registries, populated during `transform` and read by the virtual
	// `load`/`resolveId` hooks and the manifest emit:
	//   serverBodies  — virtual server-body module id -> chunk code (server side)
	//   clientStubs   — virtual client-stub module id -> RPC stub code (client side)
	//   tracked       — chunk id -> { chunk, fileName }, drives the manifest asset
	const serverBodies = new Map<string, string>()
	const clientStubs = new Map<string, string>()
	const tracked = new Map<string, TrackedServerFn>()

	/**
	 * Register one extracted server fn as its own code-split asset: stash the body
	 * under its server-virtual id and emit it as a Rolldown output asset with a
	 * stable file name; stash the client RPC stub under its client-virtual id; and
	 * record it for the manifest.
	 *
	 * The body is emitted as an ASSET (verbatim `source`), NOT a chunk: a server-fn
	 * body is a BACKEND module (the default axum backend emits Rust/axum), so
	 * emitting it as a `chunk` would make Rolldown PARSE it as client JavaScript and
	 * fail on the first non-JS token. The client never imports this body — the
	 * component's `clientBinding` import of `./<id>.server.js` is redirected to the
	 * RPC stub by `resolveId`, so the body stays out of the client JS graph; as an
	 * asset it is written verbatim to its stable file name for a server runtime to
	 * consume via the manifest, and Rolldown never parses it.
	 */
	function registerServerChunk(ctx: TreatyPluginContext, chunk: ServerFnChunk): void {
		const fileName = serverChunkFileName(chunk)
		serverBodies.set(`${SERVER_VIRTUAL_PREFIX}${chunk.id}`, chunk.code)
		clientStubs.set(`${CLIENT_VIRTUAL_PREFIX}${chunk.id}`, clientStubModule(chunk.exportName))
		tracked.set(chunk.id, { chunk, fileName })
		ctx.emitFile({ type: 'asset', fileName, source: chunk.code })
	}

	const treatyPlugin: Plugin = {
		name: PLUGIN_NAME,

		/**
		 * Cold-build batch prewarm. Read the configured {@link PluginOptions.prewarm}
		 * files and lower them in a single `transformMany` round trip so the
		 * per-module `transform` calls Rolldown makes during the build are cache hits.
		 * No-op when nothing is listed.
		 */
		async buildStart() {
			if (prewarmFiles.length === 0) return
			const inputs: TransformInput[] = []
			for (const file of prewarmFiles) {
				const id = cleanId(file)
				if (!isCandidate(id)) continue
				try {
					inputs.push({ id, code: await readFile(file, 'utf8') })
				} catch {
					// A missing/unreadable prewarm entry is skipped; the per-file transform
					// (or Rolldown's own resolver) will surface any real error.
				}
			}
			if (inputs.length > 0) compiler.transformMany(inputs)
		},

		/**
		 * Resolve bare/relative `.treaty` (and other owned) imports so that an
		 * importing module's `import x from './foo.treaty'` keeps a stable id that
		 * this plugin's `transform` then owns, and redirect a component's server-fn
		 * client binding to the per-fn RPC stub so the server BODY never enters the
		 * client graph. Owned-extension ids that are not virtual defer the actual
		 * path resolution to Rolldown via `this.resolve`.
		 */
		async resolveId(source, importer, resolveOptions) {
			// Server-fn virtual ids resolve to themselves so `load` can serve them.
			if (source.startsWith(SERVER_VIRTUAL_PREFIX) || source.startsWith(CLIENT_VIRTUAL_PREFIX)) {
				return source
			}
			// A component's client binding imports `./<fn-id>.server.js`. Redirect that
			// to the per-fn client RPC stub so following the binding never pulls the
			// server BODY into the client module graph. The body is its own emitted
			// asset; only the stub reaches the client.
			if (functionChunking) {
				const chunkId = matchServerChunkSpecifier(source)
				if (chunkId !== null) {
					const clientId = `${CLIENT_VIRTUAL_PREFIX}${chunkId}`
					if (clientStubs.has(clientId)) return clientId
				}
			}
			if (!isCandidate(source)) return null
			// Avoid infinite recursion: skip ids we have already resolved.
			const resolved = await this.resolve(source, importer, {
				...resolveOptions,
				skipSelf: true,
			})
			return resolved ? resolved.id : null
		},

		/**
		 * Serve the server-fn virtual modules: the server BODY chunk
		 * (`SERVER_VIRTUAL_PREFIX`) and the client RPC stub
		 * (`CLIENT_VIRTUAL_PREFIX`). All other ids fall through to Rolldown.
		 */
		load(id) {
			const body = serverBodies.get(id)
			if (body !== undefined) return body
			const stub = clientStubs.get(id)
			if (stub !== undefined) return stub
			return null
		},

		/**
		 * The heart of the plugin: hand owned files to the core compiler and return
		 * Rolldown's `{ code, map }` shape. Files the core does not own (it returns
		 * `null`) fall through to Rolldown's normal pipeline untouched.
		 */
		transform(code, id) {
			if (!isCandidate(id)) return null
			// Idempotency guard: a module whose extension this plugin owns may re-enter
			// the transform already carrying the FIRST pass's lowered Ivy output (the id
			// keeps its authoring extension, so `isCandidate` re-claims it). Recompiling
			// lowered Ivy throws "no component … found", so detect the emitter's
			// signature and pass the already-lowered JS through untouched.
			if (isLoweredIvy(code)) return null

			const result = compiler.transform(cleanId(id), code)
			if (result === null) return null

			// Function chunking: emit each extracted server fn as its own loadable
			// asset and replace the component code with the per-fn client bindings, so
			// the server-fn body never enters this client module.
			let out = result.code
			if (functionChunking && result.serverChunks && result.serverChunks.length > 0) {
				for (const chunk of result.serverChunks) {
					registerServerChunk(this as unknown as TreatyPluginContext, chunk)
				}
				out = injectClientBindings(out, result.serverChunks)
			}

			const map = emitSourceMap && result.map !== undefined ? result.map : null
			return { code: out, map }
		},

		/**
		 * Drop the stale incremental-cache entry for a changed authoring file (watch
		 * mode), and on deletion additionally evict every dependent the core reports
		 * so the next build re-resolves the now-changed reference. Rolldown calls
		 * `watchChange` with the same `{ event }` shape Rollup uses.
		 */
		watchChange(id, change) {
			const file = cleanId(id)
			if (!isCandidate(file)) return
			if (change.event === 'delete') {
				compiler.onDelete(file)
				return
			}
			compiler.invalidate(file)
		},

		/**
		 * Emit the server-fn manifest (`treaty-server-fns.json`) once the bundle is
		 * generated: a map of every extracted fn's stable id to its emitted body
		 * chunk file and export name, so a server runtime can resolve a fn id to the
		 * chunk that backs it. No-op when chunking is off or no server fns were seen.
		 */
		generateBundle() {
			if (!functionChunking || tracked.size === 0) return
			const manifest = buildBuildManifest(tracked.values())
			this.emitFile({
				type: 'asset',
				fileName: MANIFEST_FILE_NAME,
				source: `${JSON.stringify(manifest, null, 2)}\n`,
			})
		},
	}

	const plugins: Plugin[] = [treatyPlugin]
	if (linkPartials) plugins.push(createLinkPartialPlugin())
	return plugins
}

/**
 * Build the Rolldown plugin that links published Angular partial-declaration
 * libraries (`ɵɵngDeclare*` → AOT `ɵɵdefine*`) at transform time, so a bundle that
 * consumes them needs NO JIT and NO `@angular/compiler`.
 *
 * This is the Rolldown analogue of the `@treaty/vite` `LinkPartialPlugin`: a
 * single Rollup-style `transform` hook that, for any partial `node_modules`
 * module, hands the source to the SHARED Rust-backed {@link linkPartialCode}. The
 * cheap {@link isPartialModule} guard (under `node_modules` AND contains a
 * `ɵɵngDeclare*` call) keeps the linker off first-party files and off vendored
 * files that are not partial. Linking is a span rewrite that preserves byte
 * offsets outside the rewritten declarations, so a `null` (identity) source map is
 * returned for these vendored libraries.
 *
 * Vite needed a second esbuild prebundle plugin and an `index.html` guard because
 * it prebundles deps and serves a page; Rolldown does neither, so this one
 * `transform` is the whole linker.
 */
export function createLinkPartialPlugin(): Plugin {
	return {
		name: LINK_PLUGIN_NAME,
		transform(code, id) {
			// Fast bail before touching the linker: only published modules carrying a
			// ɵɵngDeclare* call.
			if (!isPartialModule(id, code)) return null
			const linked = linkPartialCode(code, id)
			// `null` here means the linker addon is unavailable: serve the source unchanged.
			if (linked === null) return null
			// Identity map: the linker preserves offsets outside the rewritten spans, so a
			// passthrough/null map is acceptable for these vendored libraries.
			return { code: linked, map: null }
		},
	}
}

export {
	SERVER_VIRTUAL_PREFIX,
	CLIENT_VIRTUAL_PREFIX,
	MANIFEST_FILE_NAME,
	serverChunkFileName,
	clientStubModule,
	injectClientBindings,
	type TrackedServerFn,
} from './server-chunks.js'
export { createTreatyCompiler } from '@treaty/compiler'
export type { TreatyCompilerOptions } from '@treaty/compiler'
export { isPartialModule, linkPartialCode } from '@treaty/ts-vite'
