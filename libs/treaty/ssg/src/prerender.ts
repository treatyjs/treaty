/**
 * @module
 *
 * The **prerender pipeline** for `@treaty/ssg`: from a routes config to static
 * HTML files on disk plus a route → output-file manifest.
 *
 * For each prerenderable route the pipeline:
 *   1. discovers the concrete URL(s) ({@link ./routes}),
 *   2. compiles the route's component source to Ivy via `@treaty/compiler`
 *      (the Rust authoring compiler behind the NAPI addon),
 *   3. executes the component's top-of-file render-time macro / RSC data through
 *      the {@link RenderRuntime} (Nova-backed in production, stubbed otherwise),
 *   4. hands the emitted Ivy + resolved render data to the Rust SSG core (the
 *      `@treaty/ssg-node` addon), which statically interprets the template to an
 *      HTML fragment, detects the route's hydration islands, resolves the
 *      head/SEO, and wraps the result in a full hydration-ready document,
 *   5. writes the returned document to the output directory.
 *
 * Steps 2–3 are the boundaries Rust cannot own (the compiler and the Nova
 * runtime); every deterministic SSG byte in step 4 — the Ivy → HTML
 * interpretation, head/SEO emit, document wrapping, island detection — is
 * produced by the Rust core (`treaty_ssg`). See [[rust-core-ts-shim-layering]].
 *
 * Treaty is a compiler, not a host: the pipeline only EMITS the static files and
 * a manifest. Serving them (and bootstrapping client hydration via Angular's
 * `@angular/platform-browser` hydration, referenced structurally as an optional
 * peer) is the dev server's / platform's job.
 */

import { TreatyCompiler, type TransformResult } from '@treaty/compiler'
import { loadNative } from './native.js'
import {
	StubRenderRuntime,
	type JsonValue,
	type RenderData,
	type RenderMacro,
	type RenderRuntime,
} from './runtime.js'
import {
	discoverRoutes,
	type DiscoveredRoute,
	type RouteLike,
	type RouteParamsMap,
} from './routes.js'

/**
 * The component a route renders. A route either points at inline component
 * source (the common SSG input: the authoring file text Treaty compiles) or, for
 * a lazy route, supplies it via {@link RoutePrerenderInput.source}. The compiler
 * owns the lowering; the pipeline only needs the source + a file id for routing.
 */
export interface RoutePrerenderInput {
	/**
	 * Full authoring source of the route's component (`.tsx`/`.ts`/`.treaty`
	 * text). Compiled to Ivy via `@treaty/compiler`.
	 */
	readonly source: string
	/**
	 * File id for the source — its extension selects the compiler front-end
	 * (`.treaty`/`.tsx`/`.tjsx`/`.ts`). Defaults to `<routePath>.tsx`.
	 */
	readonly fileId?: string
	/**
	 * The route's render-time macro / RSC data unit, if any. Its result is the
	 * render data the template interpolates. Omit for a purely static component.
	 */
	readonly macro?: RenderMacro
}

/**
 * Resolve the {@link RoutePrerenderInput} for a discovered route. Lets callers
 * map a route node to its component source however they like (a co-located
 * `source` field, a file read, a manifest lookup). Returning `null` skips the
 * route (e.g. a layout shell with no own component).
 */
export type ResolveRouteInput = (route: DiscoveredRoute) => RoutePrerenderInput | null

/** Configuration for {@link prerenderAll}. */
export interface PrerenderConfig {
	/** The app's routes config (Angular `Routes` / `@treaty/module-federation` shape). */
	readonly routes: readonly RouteLike[]
	/**
	 * Map each discovered route to the component source + macro to prerender.
	 * Required: it is the only Treaty-app-specific seam in the pipeline.
	 */
	readonly resolve: ResolveRouteInput
	/**
	 * Param sets for parameterized routes, keyed by declared path (see
	 * {@link RouteParamsMap}). Parameterized routes without an entry are skipped.
	 */
	readonly params?: RouteParamsMap
	/** Directory to write static HTML files into. Defaults to `'dist/ssg'`. */
	readonly outDir?: string
	/**
	 * The render runtime that executes render-time macros. Defaults to a
	 * {@link StubRenderRuntime}; pass a `createNovaRenderRuntime(addon)` instance
	 * to execute arbitrary macros through Nova.
	 */
	readonly runtime?: RenderRuntime
	/**
	 * Document `<title>`, or a function of the route. Defaults to the route URL.
	 */
	readonly title?: string | ((route: DiscoveredRoute) => string)
	/**
	 * Head / SEO metadata per route (description, canonical, Open Graph, links).
	 * A static {@link HeadMeta} applies to every route, or a function tailors it
	 * per route (receiving the route and its resolved render data so a macro can
	 * drive SEO). Conventional render-data keys (`description`, `canonical`) are
	 * picked up automatically when not overridden here.
	 */
	readonly head?: HeadMeta | ((route: DiscoveredRoute, data: RenderData) => HeadMeta)
	/** Language for the `<html lang>` attribute. Defaults to `'en'`. */
	readonly lang?: string
	/** Shared {@link TreatyCompiler}; one is created per call when omitted. */
	readonly compiler?: TreatyCompiler
	/**
	 * Sink that writes a file. Defaults to writing via `node:fs/promises`
	 * (creating parent dirs). Injectable so the pipeline is testable without
	 * touching disk and so a platform can redirect output.
	 */
	readonly writeFile?: (path: string, contents: string) => Promise<void>
}

/** One prerendered route in the manifest. */
export interface PrerenderedRoute {
	/** The concrete URL that was prerendered (`'/'`, `'/blog/hello'`). */
	readonly url: string
	/** Output file path (relative to `outDir`'s parent), e.g. `dist/ssg/blog/hello/index.html`. */
	readonly output: string
	/** The declared route path before param substitution. */
	readonly routePath: string
	/** Whether this entry came from a parameterized route. */
	readonly parameterized: boolean
	/** Byte length of the emitted document. */
	readonly bytes: number
	/** The hydration islands detected in this route's prerendered markup. */
	readonly islands: readonly HydrationIsland[]
}

/** The result of {@link prerenderAll}: the route → output manifest. */
export interface PrerenderManifest {
	/** Output directory the documents were written to. */
	readonly outDir: string
	/** One entry per prerendered route, in discovery order. */
	readonly routes: readonly PrerenderedRoute[]
}

/** The marker a hydrating client runtime keys off to take over a prerender. */
export const HYDRATION_MARKER_ATTR = 'data-treaty-ssg'

/** The element id under which serialized render state is embedded for hydration. */
export const HYDRATION_STATE_ID = '__TREATY_SSG_STATE__'

/**
 * Head / SEO metadata for a prerendered document. Every field is optional; the
 * pipeline derives sensible defaults from the route's render data (a `title`,
 * `description`, or `canonical` key in the render data is picked up
 * automatically) and a caller can override per route via
 * {@link PrerenderConfig.head}.
 */
export interface HeadMeta {
	/** Document `<title>`. */
	readonly title?: string
	/** `<meta name="description">`. */
	readonly description?: string
	/** `<link rel="canonical">` href. */
	readonly canonical?: string
	/** `<html lang>` override (else {@link PrerenderConfig.lang}). */
	readonly lang?: string
	/**
	 * Open Graph / Twitter / arbitrary `<meta>` tags as `name -> content`. A key
	 * starting with `og:` or `article:` is emitted as a `property=` meta (the
	 * Open Graph convention); anything else as a `name=` meta.
	 */
	readonly meta?: Readonly<Record<string, string>>
	/** Extra `<link>` tags as `rel -> href` (e.g. `{ icon: '/favicon.ico' }`). */
	readonly links?: Readonly<Record<string, string>>
}

/**
 * One hydratable unit (component / island) detected in a prerendered route, for
 * the hydration manifest the client runtime consumes to take over the static
 * markup. A "component" island is the route's root component; an interpolation
 * island marks dynamic text the static render filled but that the client may
 * re-evaluate.
 */
export interface HydrationIsland {
	/** The island kind: the route root component, or a dynamic interpolation. */
	readonly kind: 'component' | 'interpolation'
	/** The component/template identifier the island corresponds to. */
	readonly id: string
}

/** The hydration descriptor emitted per route into the hydration manifest. */
export interface RouteHydration {
	/** The concrete URL this descriptor is for. */
	readonly url: string
	/** Output HTML file for the route. */
	readonly output: string
	/** The hydration islands detected in the route's prerendered markup. */
	readonly islands: readonly HydrationIsland[]
	/** Whether the route embedded serialized render state for reuse on the client. */
	readonly hasState: boolean
}

/** Error raised when a route cannot be prerendered. */
export class PrerenderError extends Error {
	constructor(
		readonly url: string,
		message: string
	) {
		super(`Failed to prerender ${url}: ${message}`)
		this.name = 'PrerenderError'
	}
}

/** Default output directory for emitted static documents. */
export const DEFAULT_OUT_DIR = 'dist/ssg'

/** The full result of prerendering one route: the document plus its metadata. */
export interface PrerenderRouteResult {
	/** The complete hydration-ready HTML document. */
	readonly document: string
	/** The render data the template was bound against (also embedded for hydration). */
	readonly data: RenderData
	/** The hydration islands detected in the rendered markup. */
	readonly islands: readonly HydrationIsland[]
}

/** A fully-resolved page the Rust core's `generateSiteFull` emits. */
interface NativePageInput {
	readonly url: string
	readonly routePath: string
	readonly parameterized: boolean
	readonly ivyCode: string
	readonly componentId: string
	readonly data: RenderData
	readonly title?: string
	readonly head?: HeadMeta
}

/** A page in the `GeneratedSite` JSON the Rust core returns. */
interface NativePage {
	readonly url: string
	readonly output: string
	readonly routePath: string
	readonly parameterized: boolean
	readonly document: string
	readonly bytes: number
	readonly data: RenderData
	readonly islands: HydrationIsland[]
}

/** The `GeneratedSite` JSON shape the Rust core returns. */
interface NativeGeneratedSite {
	readonly outDir: string
	readonly pages: NativePage[]
	readonly artifacts: { kind: string; output: string; contents: string; bytes: number }[]
	readonly hydration: { version: number; routes: RouteHydration[] }
}

/** Drop `undefined`-valued fields so the JSON omits absent overrides cleanly. */
function compactHead(head: HeadMeta | undefined): HeadMeta | undefined {
	if (head === undefined) return undefined
	const out: Record<string, unknown> = {}
	if (head.title !== undefined) out['title'] = head.title
	if (head.description !== undefined) out['description'] = head.description
	if (head.canonical !== undefined) out['canonical'] = head.canonical
	if (head.lang !== undefined) out['lang'] = head.lang
	if (head.meta !== undefined) out['meta'] = head.meta
	if (head.links !== undefined) out['links'] = head.links
	return out as HeadMeta
}

/** The resolved render of one route: its emitted Ivy, render data, and file id. */
export interface ResolvedRouteRender {
	/** Emitted Ivy JS for the route's compiled component. */
	readonly ivyCode: string
	/** The route's resolved render data (macro output, route params, or `{}`). */
	readonly data: RenderData
	/** The file id used for the source (and the hydration islands' component id). */
	readonly fileId: string
}

/**
 * Compile a route's component and run its macro through the runtime, returning
 * the emitted Ivy + the resolved render data + the file id. Returns `null` when
 * the compiler does not own / produce a result for the source (a pass-through
 * module). The compile boundary (the authoring compiler) and the macro boundary
 * (the Nova runtime) are the two host seams Rust cannot run; everything after is
 * the Rust core's job. Shared by {@link prerenderRouteResult} and the whole-site
 * generator so the resolution semantics (param merge, error wrapping) are single-
 * sourced.
 */
export function resolveRouteRender(
	route: DiscoveredRoute,
	input: RoutePrerenderInput,
	runtime: RenderRuntime,
	compiler: TreatyCompiler
): ResolvedRouteRender | null {
	const fileId = input.fileId ?? `${route.routePath || 'index'}.tsx`

	let compiled: TransformResult | null
	try {
		compiled = compiler.transform(fileId, input.source)
	} catch (cause) {
		throw new PrerenderError(route.url, cause instanceof Error ? cause.message : String(cause))
	}
	if (compiled === null) return null

	// Execute render-time data: macro params are merged over the route's own
	// params so a macro can read `input.slug` for a `:slug` route by default.
	let data: RenderData = {}
	if (input.macro) {
		const macro: RenderMacro = {
			source: input.macro.source,
			input: { ...(route.params as Record<string, JsonValue>), ...(input.macro.input ?? {}) },
		}
		try {
			data = runtime.runMacro(macro)
		} catch (cause) {
			throw new PrerenderError(route.url, cause instanceof Error ? cause.message : String(cause))
		}
	} else if (Object.keys(route.params).length > 0) {
		// No macro: still expose route params to the template as render data.
		data = { ...(route.params as Record<string, JsonValue>) }
	}

	return { ivyCode: compiled.code, data, fileId }
}

/**
 * Drive the Rust core's `generateSiteFull` over a list of pre-resolved pages and
 * return the parsed {@link NativeGeneratedSite}. Every deterministic SSG byte —
 * the Ivy → HTML interpretation, island detection, head/SEO emit, document
 * wrapping, sitemap/robots/hydration-manifest emit — is produced by the Rust
 * core; this only marshals JSON in and out.
 */
function generateSiteNative(
	pages: readonly NativePageInput[],
	config: { outDir: string; lang: string; origin?: string; disallow?: readonly string[]; sitemap?: boolean; robots?: boolean; hydrationManifest?: boolean }
): NativeGeneratedSite {
	const native = loadNative()
	const json = native.generateSiteFull(
		JSON.stringify({
			config: {
				out_dir: config.outDir,
				lang: config.lang,
				origin: config.origin ?? '',
				disallow: config.disallow ?? [],
				sitemap: config.sitemap ?? true,
				robots: config.robots ?? true,
				hydration_manifest: config.hydrationManifest ?? true,
			},
			pages,
		})
	)
	return JSON.parse(json) as NativeGeneratedSite
}

/**
 * Prerender a single discovered route to a complete HTML document string. Pure
 * (no disk I/O) so it is independently testable: compiles the component, runs
 * its macro through the runtime, then has the Rust core statically render the
 * Ivy template and wrap it in a hydration-ready document. Returns `null` when the
 * route has no component to render. The optional `head` supplies SEO metadata for
 * the document (else it is derived from the render data + title).
 */
export function prerenderRoute(
	route: DiscoveredRoute,
	input: RoutePrerenderInput,
	runtime: RenderRuntime,
	compiler: TreatyCompiler,
	title: string,
	lang: string,
	head?: HeadMeta
): string | null {
	const result = prerenderRouteResult(route, input, runtime, compiler, title, lang, head)
	return result === null ? null : result.document
}

/**
 * Prerender a single discovered route to its full {@link PrerenderRouteResult}
 * (document + render data + hydration islands). Like {@link prerenderRoute} but
 * surfaces the render data and detected islands the site generator folds into
 * its hydration manifest. Returns `null` when the route has no component to
 * render.
 */
export function prerenderRouteResult(
	route: DiscoveredRoute,
	input: RoutePrerenderInput,
	runtime: RenderRuntime,
	compiler: TreatyCompiler,
	title: string,
	lang: string,
	head?: HeadMeta
): PrerenderRouteResult | null {
	const rendered = resolveRouteRender(route, input, runtime, compiler)
	if (rendered === null) return null

	// Single-page emit through the Rust core: the artifacts are not needed here
	// (this is the per-route entry), only the page's document + data + islands.
	const site = generateSiteNative(
		[
			{
				url: route.url,
				routePath: route.routePath,
				parameterized: route.parameterized,
				ivyCode: rendered.ivyCode,
				componentId: rendered.fileId,
				data: rendered.data,
				title,
				head: compactHead(head),
			},
		],
		{ outDir: DEFAULT_OUT_DIR, lang, sitemap: false, robots: false, hydrationManifest: false }
	)
	const page = site.pages[0]
	if (page === undefined) return null
	return { document: page.document, data: page.data, islands: page.islands }
}

/** Resolve the document title for a route from the config. */
function resolveTitle(config: PrerenderConfig, route: DiscoveredRoute): string {
	const t = config.title
	if (typeof t === 'function') return t(route)
	if (typeof t === 'string') return t
	return route.url
}

/** Resolve the caller-supplied {@link PrerenderConfig.head} for a route. */
function resolveHeadConfig(
	config: PrerenderConfig,
	route: DiscoveredRoute,
	data: RenderData
): HeadMeta | undefined {
	const h = config.head
	if (typeof h === 'function') return h(route, data)
	return h
}

/** Default file writer: write `contents` to `path`, creating parent dirs. */
async function defaultWriteFile(path: string, contents: string): Promise<void> {
	const { mkdir, writeFile } = await import('node:fs/promises')
	const slash = Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\'))
	if (slash > 0) await mkdir(path.slice(0, slash), { recursive: true })
	await writeFile(path, contents, 'utf8')
}

/**
 * Prerender every prerenderable route in `config` to a static HTML document and
 * return the route → output-file manifest. This is the package's one-call entry
 * point.
 *
 * Static routes yield one document; parameterized routes yield one per supplied
 * param set ({@link PrerenderConfig.params}). Routes the resolver maps to `null`,
 * and components the compiler does not own / that have no template, are skipped
 * (they contribute no manifest entry). The deterministic render + document
 * assembly happens in the Rust core; all documents are written through
 * {@link PrerenderConfig.writeFile} (disk by default).
 *
 * @throws {PrerenderError} if a route's component fails to compile or its macro
 *   fails to execute.
 */
export async function prerenderAll(config: PrerenderConfig): Promise<PrerenderManifest> {
	const outDir = config.outDir ?? DEFAULT_OUT_DIR
	const runtime = config.runtime ?? new StubRenderRuntime()
	const lang = config.lang ?? 'en'
	const compiler = config.compiler ?? new TreatyCompiler()
	const write = config.writeFile ?? defaultWriteFile

	const discovered = discoverRoutes(config.routes, { params: config.params })

	// Resolve every route's render data (compile + macro) in TS, then emit all
	// pages through the Rust core in one call.
	const pages: NativePageInput[] = []
	for (const route of discovered) {
		const input = config.resolve(route)
		if (input === null) continue
		const rendered = resolveRouteRender(route, input, runtime, compiler)
		if (rendered === null) continue
		pages.push({
			url: route.url,
			routePath: route.routePath,
			parameterized: route.parameterized,
			ivyCode: rendered.ivyCode,
			componentId: rendered.fileId,
			data: rendered.data,
			title: resolveTitle(config, route),
			head: compactHead(resolveHeadConfig(config, route, rendered.data)),
		})
	}

	const site = generateSiteNative(pages, {
		outDir,
		lang,
		sitemap: false,
		robots: false,
		hydrationManifest: false,
	})

	const routes: PrerenderedRoute[] = []
	for (const page of site.pages) {
		await write(page.output, page.document)
		routes.push({
			url: page.url,
			output: page.output,
			routePath: page.routePath,
			parameterized: page.parameterized,
			bytes: page.bytes,
			islands: page.islands,
		})
	}

	return { outDir, routes }
}
