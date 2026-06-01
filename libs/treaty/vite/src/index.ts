/**
 * @module
 *
 * `@treaty/vite` — the Vite plugin for Treaty authoring formats. It wires the
 * framework-agnostic {@link TreatyCompiler} core from `@treaty/compiler` into
 * Vite's plugin lifecycle so that `.treaty`, `.tsx`, `.tjsx`, and Angular
 * `@Component` `.ts` files are lowered to Ivy JS during dev and build.
 *
 * Treaty is a compiler, not a host: this plugin does not reimplement any
 * lowering. It delegates every owned file to the core's `transform`, which in
 * turn routes through the Rust authoring compiler. The plugin's job is purely
 * Vite integration: extension ownership, esbuild/resolve configuration, the
 * incremental cache, and hot-update / deletion handling.
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
import {
	toViteFederation,
	type MfOptions,
	type ViteFederationOptions,
} from '@treaty/module-federation'
import {
	createLinkPartialPlugins,
	generateRoutesModule,
	isTreatyRoutesId,
	RESOLVED_TREATY_ROUTES_ID,
	type RoutesVirtualModuleOptions,
} from '@treaty/ts-vite'
import type { Plugin } from 'vite'
import type { EmittedFile } from 'rollup'
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
import {
	createServerFnMiddleware,
	type DevBackendServer,
	type DevServerFn,
} from './dev-backend.js'

/** Public options for {@link treaty}. */
export interface PluginOptions extends TreatyCompilerOptions {
	/**
	 * Emit a JSON source map alongside the transformed code when the core
	 * produces one. Defaults to `true`. When `false`, a null map is returned so
	 * Vite skips source-map work for Treaty modules.
	 */
	readonly sourceMap?: boolean
	/**
	 * Force `esbuild` to treat the listed extensions with the given loader so
	 * Vite's dependency optimizer and esbuild passes do not choke on the JSX
	 * authoring extensions this plugin owns. Defaults to mapping `.tjsx` to the
	 * `tsx` loader (`.tsx` is already known to esbuild).
	 */
	readonly esbuildLoaders?: Readonly<Record<string, 'ts' | 'tsx' | 'js' | 'jsx'>>
	/**
	 * Whether to force esbuild's JSX mode to `'preserve'` for the whole app —
	 * both Vite's main transform pass (`esbuild.jsx`) and the dependency
	 * optimizer's pre-bundle scanner (`optimizeDeps.esbuildOptions.jsx`).
	 * Defaults to `true`, and production usage should leave it on.
	 *
	 * Treaty JSX is **Ivy, not React**: a `.tsx`/`.tjsx` authoring file is lowered
	 * to `ɵɵdefineComponent` by this plugin's `enforce: 'pre'` transform, which
	 * consumes the RAW JSX. It never calls a React-style `jsx()`/`jsxDEV()` runtime
	 * factory. But a Treaty app's `tsconfig.json` sets `jsx: "react-jsx"` +
	 * `jsxImportSource: "@treaty/jsx"` so the editor/`tsgo` can type-check the JSX —
	 * and esbuild's *automatic* JSX dev transform reads that tsconfig and INJECTS an
	 * `import { jsxDEV } from "@treaty/jsx/jsx-dev-runtime"` into the file. During
	 * dev that injected import escapes to Vite's dependency scanner as if the
	 * authoring file imported it, and the scan fails to resolve it (the JSX runtime
	 * is a types/back-compat shim, not a dep the app actually imports).
	 *
	 * Forcing `jsx: 'preserve'` makes esbuild leave the JSX untouched (no factory
	 * import is ever injected), so the Treaty transform — which already owns and
	 * lowers these files before esbuild's transform runs — is the only thing that
	 * ever consumes the JSX. Vite zeros the tsconfig `jsx`/`jsxImportSource` when an
	 * explicit `esbuild.jsx` is present, so this cleanly overrides the project
	 * tsconfig for the bundler without changing how `tsgo`/the editor type-check.
	 *
	 * Set `false` only if an embedder deliberately wants esbuild to apply a
	 * React-style automatic-runtime transform to `.tsx` (non-Treaty JSX); a real
	 * Treaty app never needs this.
	 */
	readonly preserveJsx?: boolean
	/**
	 * Cold-build prewarm: a list of absolute paths to owned authoring files to
	 * batch-compile up front via the core's `transformMany` (one parallel round
	 * trip through the Rust addon). Only runs for a one-shot `build` (not dev),
	 * during `buildStart`. The results populate the incremental cache, so the
	 * per-module `transform` calls Vite makes during the build are served as cache
	 * hits instead of re-entering the compiler one file at a time.
	 *
	 * Vite/Rollup is a pull-based pipeline with no hook that hands the plugin the
	 * full owned-file set, so this batch path is opt-in: pass the entry/owned
	 * authoring files you want compiled eagerly. When omitted, the plugin uses
	 * per-file `transform` only (the default, and the path used for incremental
	 * dev rebuilds regardless of this option).
	 */
	readonly prewarm?: readonly string[]
	/**
	 * Automatic Module Federation. Every Treaty app is a Module Federation host
	 * by default — Treaty generates the federation config from these options so
	 * the developer writes no `federation()`/`ModuleFederationPlugin` by hand.
	 *
	 *   - `true` / omitted via {@link treatyWithFederation}: enable with defaults
	 *     (the app is a host that shares the Angular runtime as eager singletons).
	 *   - an {@link MfOptions} object: configure the app name, the remotes it
	 *     consumes, the modules it exposes, and extra shared deps.
	 *   - `false`: disable federation entirely.
	 *
	 * The base {@link treaty} factory does not apply federation (so existing
	 * single-plugin usage is unchanged); use {@link treatyWithFederation} to get
	 * the Treaty plugin and the auto-generated federation plugin together.
	 */
	readonly moduleFederation?: MfOptions | boolean
	/**
	 * File-system routing as a VIRTUAL MODULE, generated DURING the build (no
	 * checked-in / prebuilt `routes.ts`). When set, the plugin serves
	 * `import routes from 'virtual:treaty-routes'` by driving the Rust file-routing
	 * core (`@treaty/authoring-node`.`generateRoutes`) over the configured
	 * `routesRoot` on every load, so the route graph always reflects the on-disk
	 * `routes/` tree. The route entry files the module references are registered as
	 * Vite watch dependencies so editing/adding/removing a route regenerates the
	 * virtual module in dev.
	 *
	 *   - a {@link RoutesVirtualModuleOptions} object: enable, taking `routesRoot`
	 *     (the project root containing `routes/`/`api/`) plus the optional
	 *     `routesDir` / `apiDir` / `dynamicSegmentStyle` / `federation` / `importBase`
	 *     knobs forwarded to the file-routing core.
	 *   - omitted: the virtual module is not served (apps that do not use file
	 *     routing are unaffected).
	 *
	 * The routing logic itself lives ONCE in Rust; this plugin is the thin Vite
	 * shim (resolveId/load + watch-file registration), mirroring the linker.
	 */
	readonly fileRoutes?: RoutesVirtualModuleOptions
	/**
	 * Function chunking. When `true` (the default), each server function the
	 * compiler extracts from an authoring file is emitted as its OWN
	 * separately-loadable Rollup chunk (`<fn-id>.server.js`), the component code
	 * keeps only the per-fn client binding, and a `treaty-server-fns.json`
	 * manifest (fn-id -> chunk file + export name) is emitted as a build asset.
	 *
	 * This guarantees a server-fn BODY never lands in the client module graph:
	 * the body lives only in its emitted chunk, while the client follows the
	 * binding to a tiny RPC stub that calls the fn's `/__server/<name>` route.
	 *
	 * Set `false` to leave server fns as the compiler's single `serverModule`
	 * blob (no per-fn code-splitting and no manifest).
	 */
	readonly functionChunking?: boolean
	/**
	 * Factory used to construct the underlying {@link TreatyCompiler}. Defaults to
	 * `createTreatyCompiler` from `@treaty/compiler`. Provided as a seam so an
	 * embedder (or a test) can supply an alternative compiler implementation
	 * without changing the plugin's Vite wiring; production usage never sets this.
	 */
	readonly compilerFactory?: (options: TreatyCompilerOptions) => TreatyCompiler
}

/**
 * The `@module-federation/vite` `federation()` factory, declared structurally so
 * `@treaty/vite` typechecks (and the base plugin runs) without the peer package
 * installed. The real default export is assignable to this.
 */
type ViteFederationFactory = (options: ViteFederationOptions) => Plugin

/** The plugin name surfaced in Vite logs and the plugin pipeline. */
const PLUGIN_NAME = 'treaty:vite'

/** Default esbuild loader assignments for Treaty's JSX authoring extensions. */
const DEFAULT_ESBUILD_LOADERS: Readonly<Record<string, 'ts' | 'tsx' | 'js' | 'jsx'>> = {
	'.tjsx': 'tsx',
}

/** Strip a bundler-appended query/hash suffix (`?foo`, `#bar`) from an id. */
function cleanId(id: string): string {
	return id.replace(/[?#].*$/, '')
}

/**
 * The minimal esbuild surface this plugin uses to strip TypeScript syntax from
 * lowered authoring output. Declared structurally so the plugin typechecks
 * without a direct `esbuild` dependency — esbuild always rides along with Vite,
 * and we load it lazily by specifier ({@link loadEsbuildTransform}).
 */
interface EsbuildLike {
	transform(
		input: string,
		options: {
			loader?: 'ts' | 'tsx' | 'js' | 'jsx'
			format?: 'esm'
			target?: string
			charset?: 'ascii' | 'utf8'
			sourcefile?: string
			sourcemap?: boolean | 'external'
			tsconfigRaw?: string
		}
	): Promise<{ code: string; map: string }>
}

/**
 * Lazily-loaded esbuild `transform` (memoized). The Treaty compiler emits the
 * authoring body verbatim as TypeScript (`.treaty`/JSX bodies are "TS-by-default";
 * the lowered Ivy keeps `signal<T[]>(…)` generics and `: T` annotations), so the
 * output must be type-stripped before it is valid ECMAScript. Vite's built-in
 * esbuild pass strips `.ts`/`.tsx` ids itself, but it never sees Treaty's own
 * `.treaty`/`.tjsx` extensions — so for those this plugin strips types here, using
 * the same esbuild Vite ships. Returns `null` if esbuild cannot be loaded (then
 * the output is returned unstripped, preserving the prior behaviour).
 */
let esbuildPromise: Promise<EsbuildLike | null> | undefined
function loadEsbuildTransform(): Promise<EsbuildLike | null> {
	if (esbuildPromise === undefined) {
		esbuildPromise = import('esbuild')
			.then((m) => (m as unknown as { default?: EsbuildLike } & EsbuildLike).default ?? (m as unknown as EsbuildLike))
			.catch(() => null)
	}
	return esbuildPromise
}

/**
 * The owned extensions whose lowered output Vite's own esbuild transform will NOT
 * type-strip, because Vite does not recognise them as TypeScript ids. `.ts`/`.tsx`
 * are stripped by Vite natively; `.treaty` and `.tjsx` are Treaty-only extensions,
 * so this plugin strips their lowered TS itself. `.tjsx` lowers JSX-flavoured
 * source so it needs the `tsx` loader; `.treaty` lowers to plain TS (`ts`).
 */
const STRIP_LOADER_BY_EXT: Readonly<Record<string, 'ts' | 'tsx'>> = {
	'.treaty': 'ts',
	'.tjsx': 'tsx',
}

/** Pick the esbuild type-strip loader for `id`, or `null` if Vite already strips it. */
function stripLoaderFor(id: string): 'ts' | 'tsx' | null {
	const clean = cleanId(id).toLowerCase()
	for (const ext of Object.keys(STRIP_LOADER_BY_EXT)) {
		if (clean.endsWith(ext)) return STRIP_LOADER_BY_EXT[ext]
	}
	return null
}

/**
 * The Treaty-only authoring extensions Vite's dev server does NOT recognise as
 * JavaScript when it decides the HTTP `Content-Type` of a served module.
 *
 * Vite's transform middleware only serves a module with a JS content-type
 * (`text/javascript`) when the request reaches its transform branch, which is
 * gated on the request URL being a JS request (`isJSRequest` — matched against a
 * fixed known-JS-extension regex covering `.[jt]sx?`/`.m[jt]s`/`.vue`/`.svelte`/…),
 * a CSS request, an explicit `?import` request, or a script fetch
 * (`sec-fetch-dest: script`). `.ts`/`.tsx` match the known-JS regex, so a bare
 * `GET /x.tsx` is transformed and labelled JS. But `.treaty`/`.tjsx` are NOT in
 * that regex, so a `GET /x.treaty` that arrives WITHOUT a `?import` query (a
 * dynamic `import()` whose specifier import-analysis did not rewrite, a hard
 * navigation, a proxy that drops `sec-fetch-dest`, or a re-request of the bare
 * URL) skips transform entirely and falls through to Vite's static/fs middleware,
 * which serves the RAW authoring source with an EMPTY content-type. The browser
 * then refuses the module ("disallowed MIME type ()" / `NS_ERROR_CORRUPTED_CONTENT`)
 * and the lazy route never loads.
 *
 * These are exactly the extensions this plugin lowers but Vite cannot identify as
 * JS by extension (mirroring {@link STRIP_LOADER_BY_EXT}); `.ts`/`.tsx` are not
 * listed because Vite already treats them as JS requests.
 */
const DEV_SERVE_JS_EXTS: readonly string[] = Object.keys(STRIP_LOADER_BY_EXT)

/**
 * Matches Vite's `import` query marker (`?import` / `&import`, with or without a
 * value) — the same shape Vite's `isImportRequest` tests for. Used by the
 * dev-serve content-type fix to avoid re-injecting the marker on a request that
 * already carries it.
 */
const IMPORT_QUERY_RE = /[?&]import(?:=|&|$)/

/**
 * The minimal connect/dev-server middleware shape the dev-serve content-type fix
 * uses: a `req` carrying a mutable `url`, a `res`, and a `next` continuation.
 * Declared structurally so the plugin typechecks without importing Vite/connect
 * middleware types (which it otherwise does not depend on at the value level).
 */
type DevMiddleware = (
	req: { url?: string },
	res: unknown,
	next: () => void
) => void

/**
 * Whether `url` (a dev-server request URL, possibly carrying a query/hash) targets
 * a Treaty-only authoring module Vite would otherwise mislabel — i.e. its path ends
 * in {@link DEV_SERVE_JS_EXTS}. Used by the dev-serve content-type fix to decide
 * whether to force the request through Vite's JS transform path.
 */
function isOwnedAuthoringUrl(url: string): boolean {
	const clean = cleanId(url).toLowerCase()
	return DEV_SERVE_JS_EXTS.some((ext) => clean.endsWith(ext))
}

/**
 * Append the `import` query Vite's transform middleware uses to claim a request,
 * preserving any existing query string and stripping a trailing `#hash` so the
 * marker lands on the path. Mirrors Vite's own `injectQuery('…', 'import')`: a
 * bare `/x.treaty` becomes `/x.treaty?import`; `/x.treaty?foo` becomes
 * `/x.treaty?import&foo`. The middleware later strips it via `removeImportQuery`
 * before transforming, so the injected marker never reaches the compiler.
 */
function injectImportQuery(url: string): string {
	const hashIndex = url.indexOf('#')
	const hash = hashIndex === -1 ? '' : url.slice(hashIndex)
	const path = hashIndex === -1 ? url : url.slice(0, hashIndex)
	const queryIndex = path.indexOf('?')
	if (queryIndex === -1) return `${path}?import${hash}`
	const base = path.slice(0, queryIndex)
	const query = path.slice(queryIndex + 1)
	return `${base}?import&${query}${hash}`
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
 * component. A `.tjsx`/`.tsx`/`.treaty` module can re-enter the `enforce: 'pre'`
 * transform with the FIRST pass's Ivy text (the id keeps its authoring extension,
 * so ownership re-claims it) — e.g. a re-resolved lazy `import()` chunk or the
 * esbuild `.tjsx` loader handing the transformed module back. Detecting the
 * emitter's signature lets the second pass skip recompilation and pass the
 * already-lowered JS straight through, so each module is compiled exactly once.
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
 * The Angular decorators that mean a module must be LOWERED to Ivy (even in SSR).
 * Mirrors the compiler-core screen; used only to tell a pure server module apart
 * from a component that happens to colocate a `server { … }` block.
 */
const ANGULAR_DECORATOR_RE = /@(?:Component|Directive|Pipe|Injectable|NgModule)\s*\(/

/**
 * The server-fn markers the Rust front-end extracts: a `'use server'`/
 * `'use websocket'` directive, a `server[:lang] { … }` block, or a `$$`-suffixed
 * declaration. Mirrors `hasServerMarker` in `@treaty/compiler`.
 */
const SERVER_MARKER_RE =
	/(?:^|[\n;{])\s*['"]use (?:server|websocket)['"]|(?:^|[\n;{}])\s*server(?::[A-Za-z_$][\w$]*)?\s*\{|\b[A-Za-z_$][\w$]*\$\$\s*(?:=|\()/m

/**
 * Whether `code` is a PURE server module — it declares server functions but no
 * Angular component/directive/etc. to lower. Such a module has nothing to render
 * client- or SSR-side beyond its server fns, so the dev backend's SSR pass passes
 * it through unchanged (letting Vite transpile the raw TS and run the real
 * bodies). A component that colocates a `server { … }` block is NOT pure: it must
 * still be lowered to Ivy, so it is excluded here.
 */
function isPureServerModule(code: string): boolean {
	return SERVER_MARKER_RE.test(code) && !ANGULAR_DECORATOR_RE.test(code)
}

/** The plugin name surfaced in Vite logs for the file-routes virtual module. */
const ROUTES_PLUGIN_NAME = 'treaty:vite:file-routes'

/**
 * Build the Vite plugin that serves the file-routing virtual module
 * (`virtual:treaty-routes`) generated DURING the build by the Rust file-routing
 * core — no prebuilt `routes.ts`. The routing logic lives ONCE in Rust
 * (`@treaty/authoring-node`.`generateRoutes`, the shim over `treaty_file_routing`);
 * this plugin is the thin Vite registration (mirroring the partial-declaration
 * linker): `resolveId` claims the id, `load` serves the freshly generated module
 * and registers each referenced route file as a watch dependency, and
 * `handleHotUpdate` invalidates the virtual module when a route file changes, is
 * added, or is removed so dev regenerates it.
 */
function createFileRoutesPlugin(routes: RoutesVirtualModuleOptions): Plugin {
	// The build root, captured from configResolved, used as the base for a relative
	// routesRoot so the route graph is stable regardless of the launch cwd.
	let root: string | undefined

	return {
		name: ROUTES_PLUGIN_NAME,
		// Resolve/serve the virtual id before Vite's core resolution treats it as a
		// missing file.
		enforce: 'pre',

		configResolved(resolved) {
			root = resolved.root
		},

		resolveId(source) {
			if (isTreatyRoutesId(source)) return RESOLVED_TREATY_ROUTES_ID
			return null
		},

		load(id) {
			if (!isTreatyRoutesId(id)) return null
			const generated = generateRoutesModule({ cwd: root, ...routes })
			// Register every referenced route entry file so a change to one
			// invalidates this virtual module (the route module's `import(...)`
			// targets are also added to the graph as Vite pulls them in, but watching
			// the source files makes edits to a route re-run generation in dev).
			for (const file of generated.watchFiles) this.addWatchFile(file)
			return { code: generated.code, map: null }
		},

		/**
		 * When a route source file changes (or is added/removed — Vite routes
		 * add/unlink through this hook too), invalidate the virtual routes module so
		 * the next request regenerates the route graph. A change to a file already in
		 * the graph reloads naturally; this additionally covers ADD/REMOVE, where the
		 * set of routes (not just one route's body) changed.
		 */
		handleHotUpdate(ctx) {
			const graph = ctx.server.moduleGraph
			const mod = graph.getModuleById(RESOLVED_TREATY_ROUTES_ID)
			if (mod === undefined) return
			graph.invalidateModule(mod)
			ctx.server.ws.send({ type: 'full-reload' })
			return [...ctx.modules, mod]
		},
	}
}

/**
 * Create the Treaty Vite plugins. Returns an array: the Treaty authoring plugin
 * (which delegates all lowering to the shared {@link TreatyCompiler} core) plus
 * the shared Angular partial-declaration linker plugins from `@treaty/ts-vite`
 * ({@link createLinkPartialPlugins}).
 *
 * The linker plugins are why a Treaty app that consumes published *partial*-compiled
 * Angular libraries (`@angular/{common,forms,router,platform-browser,core}`, whose
 * decorated classes ship as `ɵɵngDeclare*` calls) boots with NO JIT and NO
 * `@angular/compiler`: they de-partial those libraries to AOT `ɵɵdefine*` at
 * prebundle/transform time (dev-serve and build alike) via the Rust linker
 * (`@treaty/authoring-node`.`linkPartial`) and exclude `@angular/compiler` from the
 * dependency prebundle. Without them `@treaty/vite` served partial Angular libs
 * un-linked, which threw "needs JIT / `@angular/compiler` not available" at runtime.
 *
 * The linker logic itself is shared (one source of truth in `@treaty/ts-vite`,
 * backed by Rust); `@treaty/vite` only spreads the plugins here. Vite flattens
 * nested plugin arrays, so `plugins: [treaty(...)]` works unchanged.
 */
export default function treaty(options: PluginOptions = {}): Plugin[] {
	const emitSourceMap = options.sourceMap ?? true
	const esbuildLoaders = options.esbuildLoaders ?? DEFAULT_ESBUILD_LOADERS
	const preserveJsx = options.preserveJsx ?? true
	const prewarmFiles = options.prewarm ?? []

	const functionChunking = options.functionChunking ?? true

	const compiler: TreatyCompiler = (options.compilerFactory ?? createTreatyCompiler)(options)
	// Set by configResolved; gates the cold-build-only prewarm in buildStart.
	let isColdBuild = false

	// Server-fn registries, populated during `transform` and read by the virtual
	// `load`/`resolveId` hooks and the manifest emit:
	//   serverBodies  — virtual server-body module id -> chunk code (server side)
	//   clientStubs   — virtual client-stub module id -> RPC stub code (client side)
	//   tracked       — chunk id -> { chunk, fileName }, drives the manifest asset
	const serverBodies = new Map<string, string>()
	const clientStubs = new Map<string, string>()
	const tracked = new Map<string, TrackedServerFn>()

	// Dev backend registry: export name -> the original module id whose SSR-loaded
	// export is the REAL server-fn body. Populated during `transform` as server fns
	// are discovered; read by the dev `/__server/<name>` middleware to run the body
	// server-side (the client only ever sees the RPC stub). Keyed by export name —
	// the route segment the client stub fetches.
	const devServerFns = new Map<string, DevServerFn>()

	/**
	 * Register one extracted server fn as its own code-split chunk: stash the
	 * body under its server-virtual id and emit it as a Rollup chunk with a
	 * stable file name; stash the client RPC stub under its client-virtual id;
	 * and record it for the manifest. `emitFile` is only available on the build
	 * `PluginContext`, so dev (where it is absent) just registers the virtuals.
	 */
	function registerServerChunk(
		ctx: { emitFile?: (file: EmittedFile) => string },
		chunk: ServerFnChunk,
		sourceId: string
	): void {
		const fileName = serverChunkFileName(chunk)
		serverBodies.set(`${SERVER_VIRTUAL_PREFIX}${chunk.id}`, chunk.code)
		clientStubs.set(`${CLIENT_VIRTUAL_PREFIX}${chunk.id}`, clientStubModule(chunk.exportName))
		tracked.set(chunk.id, { chunk, fileName })
		// Dev backend: route `/__server/<exportName>` to the ORIGINAL module so its
		// SSR-loaded export runs the real body server-side (never shipped to the
		// client). `sourceId` is the authoring file the fn was extracted from.
		devServerFns.set(chunk.exportName, { exportName: chunk.exportName, moduleId: sourceId })
		// Emit the server-body asset ONLY for a one-shot `build`. In dev (serve) Vite's
		// plugin context exposes an `emitFile` that merely WARNS ("not supported in serve
		// mode") — and the asset is not needed there, since the dev backend runs the real
		// server body by SSR-loading the original module. So we skip emit in dev and rely
		// on the registered virtuals + the dev backend; the build path still emits the asset.
		if (isColdBuild && typeof ctx.emitFile === 'function') {
			// Emit the server body as a Rollup ASSET (verbatim `source`), NOT a
			// `chunk`. A server-fn body is a BACKEND module -- the default axum
			// backend emits a Rust/axum service. Emitting it as `type: 'chunk'`
			// made Rollup PARSE it as client JavaScript, which threw `Expected
			// ';'` on the first non-JS token (the Rust `use`) and failed the whole
			// client build. The client never imports this body -- the component's
			// `clientBinding` import of `./<id>.server.js` is redirected to the RPC
			// stub by `resolveId`, so the body stays out of the client JS graph. As
			// an asset it is written verbatim to its stable file name (for a server
			// runtime to consume via the manifest) and Rollup never parses it.
			ctx.emitFile({
				type: 'asset',
				fileName,
				source: chunk.code,
			})
		}
	}

	const treatyPlugin: Plugin = {
		name: PLUGIN_NAME,
		// Run before Vite's core TS/esbuild handling so authoring files reach the
		// Treaty compiler as their original source rather than esbuild output.
		enforce: 'pre',

		/**
		 * Teach esbuild about Treaty's JSX authoring extensions, and force esbuild's
		 * JSX mode to `'preserve'` so it never applies a React-style automatic-runtime
		 * transform to Treaty JSX.
		 *
		 * The loader map is needed because the dependency optimizer / esbuild
		 * transform pass would otherwise not know how to read `.tjsx` files
		 * (`.treaty` files are never handed to esbuild because this plugin transforms
		 * them first).
		 *
		 * The `jsx: 'preserve'` settings are the dev-serve JSX fix. Treaty JSX is
		 * **Ivy, not React**: this plugin's `enforce: 'pre'` transform lowers a
		 * `.tsx`/`.tjsx` authoring file to `ɵɵdefineComponent`, consuming the raw JSX
		 * — it never invokes a React-style `jsx()`/`jsxDEV()` runtime factory. But a
		 * Treaty app's `tsconfig.json` sets `jsx: "react-jsx"` +
		 * `jsxImportSource: "@treaty/jsx"` (so the editor/`tsgo` type-check JSX), and
		 * esbuild's *automatic* JSX transform reads that tsconfig and INJECTS
		 * `import { jsxDEV } from "@treaty/jsx/jsx-dev-runtime"` into the file. During
		 * dev that injected import escapes to Vite's dependency scanner as a phantom
		 * dependency of the authoring file, which then fails to resolve and aborts the
		 * dep scan. Setting `jsx: 'preserve'` on BOTH the dependency scanner
		 * (`optimizeDeps.esbuildOptions.jsx`) and Vite's main transform pass
		 * (`esbuild.jsx`) makes esbuild leave the JSX untouched, so no foreign runtime
		 * import is ever injected and the Treaty transform stays the sole consumer of
		 * the JSX. Vite zeros the tsconfig `jsx`/`jsxImportSource` whenever an explicit
		 * `esbuild.jsx` is present, so this overrides the project tsconfig for the
		 * bundler without affecting how `tsgo`/the editor type-check.
		 */
		config() {
			const jsx = preserveJsx ? ('preserve' as const) : undefined
			return {
				// Vite's MAIN esbuild transform pass: forcing `jsx: 'preserve'` makes
				// Vite drop the project tsconfig's `jsx`/`jsxImportSource`, so any `.tsx`
				// that reaches this pass keeps its JSX rather than gaining an injected
				// `@treaty/jsx` runtime import. (Treaty-owned files are already lowered
				// by the `enforce: 'pre'` transform before they get here.)
				...(preserveJsx ? { esbuild: { jsx } } : {}),
				optimizeDeps: {
					esbuildOptions: {
						loader: { ...esbuildLoaders },
						// The DEP SCANNER path: with `jsx` set here Vite passes `jsx:
						// 'preserve'` to the scan's esbuild context and zeroes the tsconfig's
						// `jsx`/`jsxImportSource`, so scanning a `.tsx` authoring file no longer
						// injects an unresolvable `@treaty/jsx/jsx-dev-runtime` import. This is
						// the exact site of the dev-serve "could not be resolved" failure.
						...(preserveJsx ? { jsx } : {}),
					},
				},
			}
		},

		/**
		 * Capture whether we are building so the cache can be left enabled in dev
		 * (where re-transforms are common) and the core's defaults otherwise.
		 */
		configResolved(resolved) {
			// One-shot production builds gain nothing from a stale in-memory cache;
			// clear it so a fresh build never serves a stale dev entry. Also record
			// that this is a cold build so `buildStart` may batch-prewarm.
			isColdBuild = resolved.command === 'build'
			if (isColdBuild) compiler.clearCache()
		},

		/**
		 * DEV-SERVE CONTENT-TYPE FIX. Register a pre-middleware so the dev server
		 * serves `.treaty`/`.tjsx` authoring modules with a JavaScript content-type.
		 *
		 * Vite's transform middleware labels a served module `text/javascript` only
		 * when the request reaches its transform branch, which is gated on the URL
		 * being a JS request (a fixed known-extension regex covering `.[jt]sx?` etc.),
		 * a CSS request, an explicit `?import` request, or a script fetch
		 * (`sec-fetch-dest: script`). `.ts`/`.tsx` match that regex, so they always
		 * transform and serve as JS. `.treaty`/`.tjsx` do NOT — so a request for one
		 * that arrives WITHOUT a `?import` query (a dynamic `import()` import-analysis
		 * left un-rewritten, a hard navigation, or a proxy that strips
		 * `sec-fetch-dest`) skips transform and is served RAW by the static/fs
		 * middleware with an EMPTY content-type. The browser then blocks the module
		 * ("disallowed MIME type ()" / `NS_ERROR_CORRUPTED_CONTENT`) and the lazy
		 * route never loads — the exact reported failure for `import "./greeter.treaty"`.
		 *
		 * The fix runs before Vite's own middlewares (`pre`) and, for any request
		 * targeting an owned authoring extension that is missing the `import` marker,
		 * injects it into `req.url`. Vite's transform middleware then claims the
		 * request (`isImportRequest` is true), runs this plugin's `enforce: 'pre'`
		 * transform to lower the module to Ivy JS, strips the marker via
		 * `removeImportQuery`, and serves the lowered code with the correct
		 * `text/javascript` content-type. Requests that already carry `?import`, or
		 * that are not owned authoring extensions, are passed through untouched — so
		 * the existing transform/idempotency/linker/source-map behaviour is preserved
		 * and only the response LABEL for these two extensions changes.
		 */
		configureServer(server: { middlewares: { use(fn: DevMiddleware): void } } & DevBackendServer) {
			server.middlewares.use((req, _res, next) => {
				const url = req.url
				if (typeof url === 'string' && isOwnedAuthoringUrl(url) && !IMPORT_QUERY_RE.test(url)) {
					req.url = injectImportQuery(url)
				}
				next()
			})

			// DEV BACKEND. Serve `/__server/<name>` by loading the original server
			// module SSR-side and invoking the real body, so the client RPC stub's
			// `fetch('/__server/<name>')` gets a genuine response in dev instead of a
			// 404. The fn registry is populated lazily as authoring files are
			// transformed, so the middleware reads it at request time (a fn the app
			// has not yet compiled simply 404s until its module is first transformed).
			server.middlewares.use(createServerFnMiddleware(server, devServerFns))
		},

		/**
		 * Cold-build batch prewarm. On a one-shot `build`, read the configured
		 * {@link PluginOptions.prewarm} files and lower them in a single
		 * `transformMany` round trip so the per-module `transform` calls Vite makes
		 * during the build are cache hits. No-op in dev or when nothing is listed —
		 * incremental rebuilds always use per-file `transform`.
		 */
		async buildStart() {
			if (!isColdBuild || prewarmFiles.length === 0) return
			const inputs: TransformInput[] = []
			for (const file of prewarmFiles) {
				const id = cleanId(file)
				if (!isCandidate(id)) continue
				try {
					inputs.push({ id, code: await readFile(file, 'utf8') })
				} catch {
					// A missing/unreadable prewarm entry is skipped; the per-file
					// transform (or Vite's own resolver) will surface any real error.
				}
			}
			if (inputs.length > 0) compiler.transformMany(inputs)
		},

		/**
		 * Resolve bare/relative `.treaty` (and other owned) imports so that an
		 * importing module's `import x from './foo.treaty'` keeps a stable id that
		 * this plugin's `transform` then owns. We only intervene for ids that carry
		 * an owned extension and are not already absolute/virtual, deferring the
		 * actual path resolution to Vite via `this.resolve`.
		 */
		async resolveId(source, importer, resolveOptions) {
			// Server-fn virtual ids resolve to themselves so `load` can serve them.
			if (source.startsWith(SERVER_VIRTUAL_PREFIX) || source.startsWith(CLIENT_VIRTUAL_PREFIX)) {
				return source
			}
			// A component's client binding imports `./<fn-id>.server.js`. Redirect that
			// to the per-fn client RPC stub so following the binding never pulls the
			// server BODY into the client module graph. The body is its own emitted
			// chunk; only the stub reaches the client.
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
		 * (`CLIENT_VIRTUAL_PREFIX`). All other ids fall through to Vite.
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
		 * Vite's `{ code, map }` shape. Files the core does not own (it returns
		 * `null`) fall through to Vite's normal pipeline untouched.
		 */
		async transform(code, id, transformOptions) {
			if (!isCandidate(id)) return null
			// Idempotency guard: a module whose extension this plugin owns may re-enter
			// the pre-transform already carrying the FIRST pass's lowered Ivy output
			// (the id keeps its authoring extension, so `isCandidate` re-claims it).
			// Recompiling lowered Ivy throws "no component … found", so detect the
			// emitter's signature and pass the already-lowered JS through untouched —
			// guaranteeing each authoring module is compiled exactly once.
			if (isLoweredIvy(code)) return null

			// DEV BACKEND SSR PASS. The dev `/__server/<name>` middleware loads the
			// ORIGINAL server module via `ssrLoadModule` to RUN the real body. In that
			// SSR pass we must NOT lift the server fns out (that would replace the body
			// with the RPC stub and recurse). A pure server module — server markers but
			// no Angular component to lower — is therefore passed through untouched in
			// SSR so Vite's own TS pipeline transpiles and executes the genuine bodies.
			// (A `.treaty`/JSX component still lowers normally, even in SSR.)
			if (transformOptions?.ssr === true && isPureServerModule(code)) {
				return null
			}

			const result = compiler.transform(cleanId(id), code)
			if (result === null) return null

			// Function chunking: emit each extracted server fn as its own loadable
			// chunk and replace the component code with the per-fn client bindings,
			// so the server-fn body never enters this client module.
			let out = result.code
			if (functionChunking && result.serverChunks && result.serverChunks.length > 0) {
				for (const chunk of result.serverChunks) {
					registerServerChunk(this, chunk, cleanId(id))
				}
				out = injectClientBindings(out, result.serverChunks)
			}

			const map = emitSourceMap && result.map !== undefined ? result.map : null

			// Type-strip the lowered output for Treaty-only extensions (`.treaty`/`.tjsx`)
			// that Vite's built-in esbuild pass never sees. The Treaty compiler emits the
			// authoring body verbatim as TypeScript ("TS-by-default"), so without this the
			// lowered module still carries `signal<T[]>(…)` generics / `: T` annotations and
			// fails Rollup's JS parse. `.ts`/`.tsx` are left for Vite's own esbuild pass.
			//
			// The strip is a pure type-erasure pass: it removes type tokens but keeps the
			// runtime statements on their original lines, so the compiler's authoring-source
			// map is retained as-is (rather than replaced by esbuild's lowered-relative map,
			// which would break the authoring → emitted chain). We therefore request no map
			// from esbuild and keep `result.map`.
			const stripLoader = stripLoaderFor(id)
			if (stripLoader !== null) {
				const esbuild = await loadEsbuildTransform()
				if (esbuild !== null) {
					const stripped = await esbuild.transform(out, {
						loader: stripLoader,
						format: 'esm',
						target: 'es2022',
						// Keep non-ASCII identifiers (the Ivy emitter's U+0275-prefixed members —
						// the factory/component/etc. statics) as literal UTF-8 rather than `\uXXXX`
						// escapes. esbuild defaults to an ASCII charset, which would emit those
						// members as escaped `ɵ`-sequences and defeat the `isLoweredIvy`
						// idempotency guard (whose regex matches the literal U+0275 char), so a
						// re-entrant pass would wrongly try to recompile already-lowered Ivy.
						charset: 'utf8',
						sourcefile: cleanId(id),
						sourcemap: false,
						// Skip any tsconfig the project may carry (we only strip types; we do not
						// apply project compiler options).
						tsconfigRaw: '{}',
					})
					out = stripped.code
				}
			}

			return { code: out, map }
		},

		/**
		 * Re-transform changed authoring files and propagate deletions through the
		 * core's `onDelete`. On a normal change we invalidate the incremental cache
		 * entry so the next `transform` recompiles; on a delete we evict the file
		 * and additionally invalidate every module that imported it so Vite picks
		 * up the now-broken (or changed) reference.
		 */
		async handleHotUpdate(ctx) {
			const file = cleanId(ctx.file)
			if (!isCandidate(file)) return

			let exists = true
			try {
				await ctx.read()
			} catch {
				// `read()` throwing signals the file is gone (deleted/renamed).
				exists = false
			}

			if (!exists) {
				const dependents = compiler.onDelete(file)
				const affected = [...ctx.modules]
				const graph = ctx.server.moduleGraph
				for (const depId of dependents) {
					for (const mod of graph.getModulesByFile(depId) ?? []) {
						affected.push(mod)
					}
				}
				return affected
			}

			// Changed-in-place: drop the stale cache entry so the reload recompiles.
			compiler.invalidate(file)
			return ctx.modules
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

	// The shared Rust-backed Angular partial-declaration linker plugins run alongside the
	// authoring plugin: they own published `node_modules` partial Angular libraries (de-partialling
	// `ɵɵngDeclare*` → AOT `ɵɵdefine*`) and exclude `@angular/compiler`, while `treatyPlugin` owns
	// first-party authoring files. The two ownerships are disjoint, so ordering between them is safe.
	const plugins: Plugin[] = [treatyPlugin, ...createLinkPartialPlugins()]
	// File routing as a virtual module, generated during the build (no prebuilt
	// routes.ts). Only added when the app opted in via `fileRoutes`.
	if (options.fileRoutes !== undefined) plugins.push(createFileRoutesPlugin(options.fileRoutes))
	return plugins
}

/**
 * Resolve the user's `moduleFederation` option to the concrete
 * {@link MfOptions} when federation is enabled, or `null` when it is disabled.
 * `true` (and the default within {@link treatyWithFederation}) ⇒ defaults `{}`.
 * `false`, or an {@link MfOptions} object carrying `enabled: false`, ⇒ disabled,
 * so federation can be switched off in config without removing its wiring.
 */
function resolveMfOptions(value: MfOptions | boolean | undefined): MfOptions | null {
	if (value === false) return null
	if (value === true || value === undefined) return {}
	if (value.enabled === false) return null
	return value
}

/**
 * Build the auto-generated `@module-federation/vite` plugin for the given
 * Treaty options. Returns a promise so the optional peer is loaded lazily — the
 * Treaty plugin itself never depends on `@module-federation/vite` being present
 * unless federation is actually used. Vite accepts a `Promise<Plugin>` entry in
 * its `plugins` array, so the returned value can be placed there directly.
 */
async function createFederationPlugin(mf: MfOptions): Promise<Plugin> {
	const options = toViteFederation(mf)
	// Loaded by specifier so bundlers do not eagerly require the optional peer.
	const mod: { default?: unknown; federation?: unknown } = await import(
		'@module-federation/vite'
	)
	const factory = (mod.federation ?? mod.default) as ViteFederationFactory | undefined
	if (typeof factory !== 'function') {
		throw new Error(
			'@treaty/vite: Module Federation is enabled but "@module-federation/vite" ' +
				'did not export a federation() factory. Install @module-federation/vite to use auto-MF.'
		)
	}
	return factory(options)
}

/**
 * Treaty's Vite integration **with automatic Module Federation**: returns the
 * Treaty authoring plugin plus the auto-generated `@module-federation/vite`
 * plugin, so every Treaty app is a federation host with zero config. Pass
 * `moduleFederation` to declare remotes/exposes/shared; omit it to get the
 * defaults (host that shares the Angular runtime as eager singletons). Set
 * `moduleFederation: false` to opt out (equivalent to plain {@link treaty}).
 *
 * The federation plugin entry is a `Promise<Plugin>` (Vite supports this), so
 * the optional `@module-federation/vite` peer is only loaded when MF is on.
 */
export function treatyWithFederation(
	options: PluginOptions = {}
): Array<Plugin | Promise<Plugin>> {
	const mf = resolveMfOptions(options.moduleFederation ?? true)
	const plugins: Array<Plugin | Promise<Plugin>> = [...treaty(options)]
	if (mf !== null) plugins.push(createFederationPlugin(mf))
	return plugins
}

export {
	createServerFnMiddleware,
	parseServerFnRoute,
	decodeArgs,
	SERVER_ROUTE_PREFIX,
	type DevServerFn,
	type DevBackendServer,
} from './dev-backend.js'
export { toViteFederation, generateMfConfig } from '@treaty/module-federation'
export type { MfOptions, NormalizedMfConfig } from '@treaty/module-federation'
export { createTreatyCompiler } from '@treaty/compiler'
export type { TreatyCompilerOptions } from '@treaty/compiler'
