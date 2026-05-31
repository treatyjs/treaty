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
 *   4. statically interprets the emitted Ivy template against that data to a
 *      static HTML fragment ({@link ./render}),
 *   5. wraps it in a full HTML document with a **hydration marker** + serialized
 *      render state, and writes it to the output directory.
 *
 * Treaty is a compiler, not a host: the pipeline only EMITS the static files and
 * a manifest. Serving them (and bootstrapping client hydration via Angular's
 * `@angular/platform-browser` hydration, referenced structurally as an optional
 * peer) is the dev server's / platform's job.
 */

import { TreatyCompiler, type TransformResult } from '@treaty/compiler'
import { renderIvyToHtml } from './render.js'
import {
	StubRenderRuntime,
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

/** Map a concrete URL to its `index.html` output path under `outDir`. */
function outputPathFor(outDir: string, url: string): string {
	const clean = url.replace(/^\/+|\/+$/g, '')
	const dir = clean === '' ? outDir : `${outDir}/${clean}`
	return `${dir}/index.html`
}

/** Escape text for embedding inside an HTML element body. */
function escapeHtml(value: string): string {
	return value.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
}

/** Escape an HTML attribute value for safe double-quoted output. */
function escapeAttr(value: string): string {
	return value.replace(/&/g, '&amp;').replace(/"/g, '&quot;')
}

/** Whether a meta key uses the Open Graph `property=` convention. */
function isPropertyMeta(name: string): boolean {
	return name.startsWith('og:') || name.startsWith('article:') || name.startsWith('fb:')
}

/** Render the `<head>` SEO/meta tags for a document from {@link HeadMeta}. */
function renderHead(head: HeadMeta, title: string): string {
	const lines = [
		'<meta charset="utf-8">',
		'<meta name="viewport" content="width=device-width, initial-scale=1">',
		`<title>${escapeHtml(title)}</title>`,
	]
	if (head.description !== undefined) {
		lines.push(`<meta name="description" content="${escapeAttr(head.description)}">`)
	}
	if (head.canonical !== undefined) {
		lines.push(`<link rel="canonical" href="${escapeAttr(head.canonical)}">`)
	}
	for (const [name, content] of Object.entries(head.meta ?? {})) {
		const attr = isPropertyMeta(name) ? 'property' : 'name'
		lines.push(`<meta ${attr}="${escapeAttr(name)}" content="${escapeAttr(content)}">`)
	}
	for (const [rel, href] of Object.entries(head.links ?? {})) {
		lines.push(`<link rel="${escapeAttr(rel)}" href="${escapeAttr(href)}">`)
	}
	return lines.map((l) => `${l}\n`).join('')
}

/**
 * Derive the effective {@link HeadMeta} for a route: caller-supplied `head`
 * fields win, falling back to conventional keys in the render `data`
 * (`description`, `canonical`) and the resolved document `title`. This is what
 * lets a render-time macro drive SEO simply by returning those keys.
 */
function resolveHead(head: HeadMeta | undefined, data: RenderData, title: string, lang: string): HeadMeta {
	const fromData = (key: string): string | undefined => {
		const value = data[key]
		return typeof value === 'string' ? value : undefined
	}
	return {
		title: head?.title ?? title,
		lang: head?.lang ?? lang,
		description: head?.description ?? fromData('description'),
		canonical: head?.canonical ?? fromData('canonical'),
		meta: head?.meta,
		links: head?.links,
	}
}

/**
 * Wrap a rendered HTML fragment in a full, hydration-ready HTML document. The
 * root mount carries {@link HYDRATION_MARKER_ATTR} (so the client runtime knows
 * the markup is a prerender to hydrate rather than replace) and the serialized
 * render state is embedded as JSON in a non-executable script under
 * {@link HYDRATION_STATE_ID} for the client to reuse without a refetch. The
 * `<head>` is built from the resolved {@link HeadMeta} (title + SEO meta).
 */
function wrapDocument(fragment: string, data: RenderData, head: HeadMeta): string {
	const state = escapeHtml(JSON.stringify(data)).replace(/<\/script/gi, '<\\/script')
	const lang = head.lang ?? 'en'
	const title = head.title ?? ''
	return (
		'<!doctype html>\n' +
		`<html lang="${escapeAttr(lang)}">\n` +
		'<head>\n' +
		renderHead(head, title) +
		'</head>\n' +
		'<body>\n' +
		`<app-root ${HYDRATION_MARKER_ATTR}="1">${fragment}</app-root>\n` +
		`<script type="application/json" id="${HYDRATION_STATE_ID}">${state}</script>\n` +
		'</body>\n' +
		'</html>\n'
	)
}

/** Default file writer: write `contents` to `path`, creating parent dirs. */
async function defaultWriteFile(path: string, contents: string): Promise<void> {
	const { mkdir, writeFile } = await import('node:fs/promises')
	const slash = Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\'))
	if (slash > 0) await mkdir(path.slice(0, slash), { recursive: true })
	await writeFile(path, contents, 'utf8')
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

/** The full result of prerendering one route: the document plus its metadata. */
export interface PrerenderRouteResult {
	/** The complete hydration-ready HTML document. */
	readonly document: string
	/** The render data the template was bound against (also embedded for hydration). */
	readonly data: RenderData
	/** The hydration islands detected in the rendered markup. */
	readonly islands: readonly HydrationIsland[]
}

/** Count the static interpolation islands the renderer filled in `code`. */
function detectIslands(componentId: string, code: string): HydrationIsland[] {
	const islands: HydrationIsland[] = [{ kind: 'component', id: componentId }]
	const interpRe = /ɵɵtextInterpolate\d*\s*\(/g
	let count = 0
	while (interpRe.exec(code) !== null) count++
	for (let i = 0; i < count; i++) {
		islands.push({ kind: 'interpolation', id: `${componentId}#${i}` })
	}
	return islands
}

/**
 * Compile a route's component, run its macro through the runtime, and statically
 * render the Ivy template to render data + an HTML fragment + detected hydration
 * islands. Returns `null` when the route has no compiler-owned component or
 * template. Shared by {@link prerenderRoute} and the site pipeline so document
 * assembly is the only thing that differs between them.
 */
function renderRouteFragment(
	route: DiscoveredRoute,
	input: RoutePrerenderInput,
	runtime: RenderRuntime,
	compiler: TreatyCompiler
): { fragment: string; data: RenderData; islands: HydrationIsland[] } | null {
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
			input: { ...route.params, ...(input.macro.input ?? {}) },
		}
		try {
			data = runtime.runMacro(macro)
		} catch (cause) {
			throw new PrerenderError(route.url, cause instanceof Error ? cause.message : String(cause))
		}
	} else if (Object.keys(route.params).length > 0) {
		// No macro: still expose route params to the template as render data.
		data = { ...route.params }
	}

	const fragment = renderIvyToHtml(compiled.code, data)
	const islands = detectIslands(fileId, compiled.code)
	return { fragment, data, islands }
}

/**
 * Prerender a single discovered route to a complete HTML document string. Pure
 * (no disk I/O) so it is independently testable: compiles the component, runs
 * its macro through the runtime, statically renders the Ivy template, and wraps
 * the result in a hydration-ready document. Returns `null` when the route has no
 * component to render. The optional `head` supplies SEO metadata for the
 * document (else it is derived from the render data + title).
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
	const rendered = renderRouteFragment(route, input, runtime, compiler)
	if (rendered === null) return null
	const resolved = resolveHead(head, rendered.data, title, lang)
	return wrapDocument(rendered.fragment, rendered.data, resolved)
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
	const rendered = renderRouteFragment(route, input, runtime, compiler)
	if (rendered === null) return null
	const resolved = resolveHead(head, rendered.data, title, lang)
	const document = wrapDocument(rendered.fragment, rendered.data, resolved)
	return { document, data: rendered.data, islands: rendered.islands }
}

/**
 * Prerender every prerenderable route in `config` to a static HTML document and
 * return the route → output-file manifest. This is the package's one-call entry
 * point.
 *
 * Static routes yield one document; parameterized routes yield one per supplied
 * param set ({@link PrerenderConfig.params}). Routes the resolver maps to `null`,
 * and components the compiler does not own / that have no template, are skipped
 * (they contribute no manifest entry). All documents are written through
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
	const routes: PrerenderedRoute[] = []

	for (const route of discovered) {
		const input = config.resolve(route)
		if (input === null) continue

		const rendered = renderRouteFragment(route, input, runtime, compiler)
		if (rendered === null) continue

		const title = resolveTitle(config, route)
		const head = resolveHead(resolveHeadConfig(config, route, rendered.data), rendered.data, title, lang)
		const document = wrapDocument(rendered.fragment, rendered.data, head)

		const output = outputPathFor(outDir, route.url)
		await write(output, document)
		routes.push({
			url: route.url,
			output,
			routePath: route.routePath,
			parameterized: route.parameterized,
			bytes: Buffer.byteLength(document, 'utf8'),
			islands: rendered.islands,
		})
	}

	return { outDir, routes }
}
