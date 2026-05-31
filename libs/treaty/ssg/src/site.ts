/**
 * @module
 *
 * The **whole-site generator** for `@treaty/ssg`: {@link prerenderSite} is the
 * one-call entry that turns a Treaty app's routes config into a complete static
 * site on disk — every prerenderable route's HTML, a hydration manifest, a
 * `sitemap.xml`, a `robots.txt`, copied static assets — and returns the emitted
 * artifact set.
 *
 * It composes the lower-level pieces of this package:
 *   1. {@link discoverRoutesAsync} enumerates concrete routes, resolving each
 *      parameterized route's params through a `getStaticPaths`-style provider
 *      (in addition to any static `params` map),
 *   2. {@link prerenderRouteResult} compiles each route's component, runs its
 *      render-time macro through the {@link RenderRuntime} (Nova-backed in
 *      production, stubbed otherwise), statically renders the Ivy template, and
 *      assembles a hydration-ready document + the route's hydration islands,
 *   3. {@link copyAssets} mirrors a static asset directory into the output,
 *   4. {@link buildSitemap} / {@link buildRobots} emit the crawler artifacts.
 *
 * Treaty is a compiler, not a host: `prerenderSite` only EMITS files (through an
 * injectable {@link FileSystemPort}); serving them and bootstrapping client
 * hydration is the dev server's / platform's job. Everything is backend-agnostic
 * — no Angular platform-server, no bundler, no HTTP.
 */

import { TreatyCompiler } from '@treaty/compiler'
import {
	copyAssets,
	createNodeFileSystem,
	type CopiedAsset,
	type FileSystemPort,
} from './assets.js'
import {
	prerenderRouteResult,
	type HeadMeta,
	type HydrationIsland,
	type ResolveRouteInput,
	type RouteHydration,
} from './prerender.js'
import {
	discoverRoutesAsync,
	type DiscoveredRoute,
	type RouteLike,
	type RouteParamsMap,
	type StaticPathsProvider,
} from './routes.js'
import { StubRenderRuntime, type RenderData, type RenderRuntime } from './runtime.js'
import {
	absoluteUrl,
	buildRobots,
	buildSitemap,
	type SitemapEntry,
} from './sitemap.js'

/** Default output directory for a generated site. */
export const DEFAULT_SITE_OUT_DIR = 'dist/ssg'

/** Default basename of the emitted hydration manifest. */
export const HYDRATION_MANIFEST_FILE = 'treaty-hydration.json'

/** Configuration for {@link prerenderSite}. */
export interface SiteConfig {
	/** The app's routes config (Angular `Routes` / `@treaty/module-federation` shape). */
	readonly routes: readonly RouteLike[]
	/**
	 * Map each discovered route to the component source + macro to prerender.
	 * Returning `null` skips the route. This is the only app-specific seam.
	 */
	readonly resolve: ResolveRouteInput
	/**
	 * Static param sets for parameterized routes, keyed by declared path. Combined
	 * with {@link SiteConfig.getStaticPaths} (static entries first).
	 */
	readonly params?: RouteParamsMap
	/**
	 * A `getStaticPaths`-style provider computing each parameterized route's param
	 * sets at build time (from content, a CMS, a macro). See
	 * {@link StaticPathsProvider}.
	 */
	readonly getStaticPaths?: StaticPathsProvider
	/** Directory to write the site into. Defaults to {@link DEFAULT_SITE_OUT_DIR}. */
	readonly outDir?: string
	/**
	 * The render runtime executing render-time macros. Defaults to a
	 * {@link StubRenderRuntime}; pass `createNovaRenderRuntime(addon)` for real
	 * Nova execution.
	 */
	readonly runtime?: RenderRuntime
	/** Document `<title>`, or a function of the route. Defaults to the route URL. */
	readonly title?: string | ((route: DiscoveredRoute) => string)
	/**
	 * Head / SEO metadata: a static {@link HeadMeta} for all routes or a function
	 * of `(route, data)`. Conventional render-data keys (`description`,
	 * `canonical`) are picked up automatically when not overridden.
	 */
	readonly head?: HeadMeta | ((route: DiscoveredRoute, data: RenderData) => HeadMeta)
	/** `<html lang>` value. Defaults to `'en'`. */
	readonly lang?: string
	/** Shared {@link TreatyCompiler}; one is created per call when omitted. */
	readonly compiler?: TreatyCompiler
	/**
	 * Site origin (`https://example.com`) used to make `sitemap.xml` `<loc>`s and
	 * the `robots.txt` `Sitemap:` line absolute. When omitted, the sitemap uses
	 * root-relative locations and robots advertises no sitemap.
	 */
	readonly origin?: string
	/**
	 * Static asset directory to copy into `outDir` (preserving layout). Skipped
	 * (no-op) when absent or when the directory does not exist.
	 */
	readonly assetsDir?: string
	/** `robots.txt` disallow prefixes. Defaults to none (everything allowed). */
	readonly disallow?: readonly string[]
	/**
	 * Emit `sitemap.xml`. Defaults to `true`. Each prerendered route becomes one
	 * entry (optionally enriched per route via {@link SiteConfig.sitemapEntry}).
	 */
	readonly sitemap?: boolean
	/** Emit `robots.txt`. Defaults to `true`. */
	readonly robots?: boolean
	/** Emit the hydration manifest JSON. Defaults to `true`. */
	readonly hydrationManifest?: boolean
	/**
	 * Per-route sitemap enrichment: return `lastmod`/`changefreq`/`priority` for a
	 * route, or `null` to exclude it from the sitemap. The `loc` is always the
	 * route url; only the extra fields come from here.
	 */
	readonly sitemapEntry?: (
		route: DiscoveredRoute
	) => Omit<SitemapEntry, 'url'> | null | undefined
	/**
	 * Filesystem port for all writes (HTML, manifest, sitemap, robots, assets).
	 * Defaults to a `node:fs/promises`-backed port. Injectable so the whole
	 * generator is testable without disk.
	 */
	readonly fs?: FileSystemPort
}

/** One prerendered page in the {@link SiteManifest}. */
export interface SitePage {
	/** The concrete URL prerendered (`'/'`, `'/blog/hello'`). */
	readonly url: string
	/** Output HTML file path under `outDir`. */
	readonly output: string
	/** The declared route path before param substitution. */
	readonly routePath: string
	/** Whether this page came from a parameterized route. */
	readonly parameterized: boolean
	/** Byte length of the emitted document. */
	readonly bytes: number
	/** The hydration islands detected in the page. */
	readonly islands: readonly HydrationIsland[]
}

/** A single non-HTML artifact the generator emitted (sitemap, robots, manifest, asset). */
export interface SiteArtifact {
	/** The artifact kind. */
	readonly kind: 'sitemap' | 'robots' | 'hydration-manifest' | 'asset'
	/** The output path written. */
	readonly output: string
	/** Byte length of the artifact. */
	readonly bytes: number
}

/** The full artifact set {@link prerenderSite} returns. */
export interface SiteManifest {
	/** The output directory the site was written into. */
	readonly outDir: string
	/** One entry per prerendered page, in discovery order. */
	readonly pages: readonly SitePage[]
	/** The non-HTML artifacts emitted (sitemap, robots, hydration manifest, assets). */
	readonly artifacts: readonly SiteArtifact[]
	/** The serialized hydration manifest (route → islands), for in-memory callers. */
	readonly hydration: readonly RouteHydration[]
}

/** The on-disk shape of the hydration manifest JSON. */
interface HydrationManifestFile {
	readonly version: 1
	readonly routes: readonly RouteHydration[]
}

/** Join `outDir` with a forward-slash relative path. */
function join(outDir: string, rel: string): string {
	const base = outDir.replace(/[/\\]+$/, '')
	return `${base}/${rel.replace(/^[/\\]+/, '')}`
}

/** Map a concrete URL to its `index.html` output path under `outDir`. */
function htmlOutput(outDir: string, url: string): string {
	const clean = url.replace(/^\/+|\/+$/g, '')
	const dir = clean === '' ? outDir : join(outDir, clean)
	return `${dir.replace(/[/\\]+$/, '')}/index.html`
}

/** Resolve the title for a route from the config. */
function titleFor(config: SiteConfig, route: DiscoveredRoute): string {
	const t = config.title
	if (typeof t === 'function') return t(route)
	if (typeof t === 'string') return t
	return route.url
}

/** Resolve the caller-supplied head for a route. */
function headFor(config: SiteConfig, route: DiscoveredRoute, data: RenderData): HeadMeta | undefined {
	const h = config.head
	if (typeof h === 'function') return h(route, data)
	return h
}

const ENCODER = new TextEncoder()

/**
 * Generate a complete static site from `config` and return the emitted artifact
 * set. This is the package's whole-site entry point: it discovers every
 * prerenderable route (static + parameterized via `getStaticPaths`), prerenders
 * each through the {@link RenderRuntime}, writes the HTML, copies static assets,
 * and emits `sitemap.xml`, `robots.txt`, and the hydration manifest.
 *
 * All writes go through an injectable {@link FileSystemPort} (disk by default),
 * so the generator is fully testable without touching the filesystem and a
 * platform can redirect output. Backend-agnostic by construction.
 */
export async function prerenderSite(config: SiteConfig): Promise<SiteManifest> {
	const outDir = config.outDir ?? DEFAULT_SITE_OUT_DIR
	const runtime = config.runtime ?? new StubRenderRuntime()
	const lang = config.lang ?? 'en'
	const compiler = config.compiler ?? new TreatyCompiler()
	const fs = config.fs ?? (await createNodeFileSystem())

	const write = async (path: string, contents: string): Promise<number> => {
		const bytes = ENCODER.encode(contents)
		await fs.writeFile(path, bytes)
		return bytes.byteLength
	}

	const discovered = await discoverRoutesAsync(config.routes, {
		params: config.params,
		getStaticPaths: config.getStaticPaths,
	})

	const pages: SitePage[] = []
	const hydration: RouteHydration[] = []
	const sitemapEntries: SitemapEntry[] = []

	for (const route of discovered) {
		const input = config.resolve(route)
		if (input === null) continue

		const title = titleFor(config, route)
		const result = prerenderRouteResult(
			route,
			input,
			runtime,
			compiler,
			title,
			lang,
			headFor(config, route, {})
		)
		if (result === null) continue

		const output = htmlOutput(outDir, route.url)
		const bytes = await write(output, result.document)

		pages.push({
			url: route.url,
			output,
			routePath: route.routePath,
			parameterized: route.parameterized,
			bytes,
			islands: result.islands,
		})
		hydration.push({
			url: route.url,
			output,
			islands: result.islands,
			hasState: Object.keys(result.data).length > 0,
		})

		if (config.sitemap !== false) {
			const extra = config.sitemapEntry?.(route)
			if (extra !== null) sitemapEntries.push({ url: route.url, ...(extra ?? {}) })
		}
	}

	const artifacts: SiteArtifact[] = []

	// Static assets: mirror the asset directory into the output.
	if (config.assetsDir !== undefined) {
		const copied = await copyAssets(config.assetsDir, outDir, fs)
		for (const asset of copied) artifacts.push(assetArtifact(asset))
	}

	// sitemap.xml
	if (config.sitemap !== false) {
		const origin = config.origin ?? ''
		const xml = buildSitemap(origin, sitemapEntries)
		const output = join(outDir, 'sitemap.xml')
		artifacts.push({ kind: 'sitemap', output, bytes: await write(output, xml) })
	}

	// robots.txt
	if (config.robots !== false) {
		const sitemapUrl =
			config.sitemap !== false && config.origin !== undefined
				? absoluteUrl(config.origin, '/sitemap.xml')
				: undefined
		const txt = buildRobots({ sitemapUrl, disallow: config.disallow })
		const output = join(outDir, 'robots.txt')
		artifacts.push({ kind: 'robots', output, bytes: await write(output, txt) })
	}

	// Hydration manifest.
	if (config.hydrationManifest !== false) {
		const file: HydrationManifestFile = { version: 1, routes: hydration }
		const output = join(outDir, HYDRATION_MANIFEST_FILE)
		const json = `${JSON.stringify(file, null, 2)}\n`
		artifacts.push({ kind: 'hydration-manifest', output, bytes: await write(output, json) })
	}

	return { outDir, pages, artifacts, hydration }
}

/** Build the {@link SiteArtifact} record for one copied asset. */
function assetArtifact(asset: CopiedAsset): SiteArtifact {
	return { kind: 'asset', output: asset.output, bytes: asset.bytes }
}
