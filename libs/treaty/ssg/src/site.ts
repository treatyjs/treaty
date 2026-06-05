/**
 * @module
 *
 * The **whole-site generator** for `@treaty/ssg`: {@link prerenderSite} is the
 * one-call entry that turns a Treaty app's routes config into a complete static
 * site on disk — every prerenderable route's HTML, a hydration manifest, a
 * `sitemap.xml`, a `robots.txt`, copied static assets — and returns the emitted
 * artifact set.
 *
 * It is a thin orchestrator over the Rust SSG core (`treaty_ssg`, via the
 * `@treaty/ssg-node` addon) plus the host seams Rust cannot run:
 *   1. {@link discoverRoutesAsync} enumerates concrete routes (the deterministic
 *      walk is the Rust core; the `getStaticPaths` callback runs in TS),
 *   2. for each route the shim compiles the component (`@treaty/compiler`) and
 *      runs its render-time macro through the {@link RenderRuntime} (Nova-backed
 *      in production, stubbed otherwise), and resolves the `title` / `head` /
 *      `sitemapEntry` callbacks to plain values,
 *   3. the Rust core's `generateSiteFull` does every deterministic byte of emit —
 *      Ivy → HTML, island detection, head/SEO, document wrap, `sitemap.xml`,
 *      `robots.txt`, the hydration manifest,
 *   4. the shim writes the returned documents + artifacts through an injectable
 *      {@link FileSystemPort} and copies static assets ({@link copyAssets}).
 *
 * Treaty is a compiler, not a host: `prerenderSite` only EMITS files (file I/O is
 * the host glue that stays in TS); serving them and bootstrapping client
 * hydration is the dev server's / platform's job. Backend-agnostic by
 * construction. See [[rust-core-ts-shim-layering]].
 */

import { TreatyCompiler } from '@treaty/compiler'
import {
	copyAssets,
	createNodeFileSystem,
	type CopiedAsset,
	type FileSystemPort,
} from './assets.js'
import { loadNative } from './native.js'
import {
	resolveRouteRender,
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
import { type SitemapEntry } from './sitemap.js'

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

/** A fully-resolved page fed to the Rust core's `generateSiteFull`. */
interface NativePageInput {
	readonly url: string
	readonly routePath: string
	readonly parameterized: boolean
	readonly ivyCode: string
	readonly componentId: string
	readonly data: RenderData
	readonly title?: string
	readonly head?: HeadMeta
	readonly sitemapEntry?: {
		exclude?: boolean
		lastmod?: string
		changefreq?: SitemapEntry['changefreq']
		priority?: number
	}
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

/** An artifact in the `GeneratedSite` JSON the Rust core returns. */
interface NativeArtifact {
	readonly kind: 'sitemap' | 'robots' | 'hydration-manifest'
	readonly output: string
	readonly contents: string
	readonly bytes: number
}

/** The `GeneratedSite` JSON shape the Rust core returns. */
interface NativeGeneratedSite {
	readonly outDir: string
	readonly pages: NativePage[]
	readonly artifacts: NativeArtifact[]
	readonly hydration: { version: number; routes: RouteHydration[] }
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

/**
 * Resolve a route's sitemap-entry override from the config callback into the
 * shape the Rust core consumes: the callback's `null` (exclude from sitemap)
 * becomes `{ exclude: true }`; an object passes its lastmod/changefreq/priority
 * through. When no callback is set, `undefined` lets the core emit a bare entry.
 */
function sitemapEntryFor(
	config: SiteConfig,
	route: DiscoveredRoute
): NativePageInput['sitemapEntry'] {
	if (config.sitemapEntry === undefined) return undefined
	const extra = config.sitemapEntry(route)
	if (extra === null || extra === undefined) return { exclude: true }
	return { lastmod: extra.lastmod, changefreq: extra.changefreq, priority: extra.priority }
}

const ENCODER = new TextEncoder()

/**
 * Generate a complete static site from `config` and return the emitted artifact
 * set. This is the package's whole-site entry point: it discovers every
 * prerenderable route (static + parameterized via `getStaticPaths`), compiles +
 * macro-resolves each through the {@link RenderRuntime}, has the Rust SSG core
 * emit every document + `sitemap.xml` + `robots.txt` + the hydration manifest,
 * writes them through an injectable {@link FileSystemPort}, and copies static
 * assets.
 *
 * All writes go through the {@link FileSystemPort} (disk by default), so the
 * generator is fully testable without touching the filesystem and a platform can
 * redirect output. The deterministic SSG logic is the Rust core; this function is
 * the host glue (compile, macro execution, callbacks, file I/O).
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

	// Resolve every route's render data (compile + macro) and per-route overrides
	// in TS, then hand the whole page list to the Rust core for emit.
	const pages: NativePageInput[] = []
	for (const route of discovered) {
		const input = config.resolve(route)
		if (input === null) continue

		// Compile + macro-resolve through the shared resolver (the compiler + Nova
		// host seams); everything after is the Rust core's deterministic emit.
		const rendered = resolveRouteRender(route, input, runtime, compiler)
		if (rendered === null) continue

		pages.push({
			url: route.url,
			routePath: route.routePath,
			parameterized: route.parameterized,
			ivyCode: rendered.ivyCode,
			componentId: rendered.fileId,
			data: rendered.data,
			title: titleFor(config, route),
			head: compactHead(headFor(config, route, rendered.data)),
			sitemapEntry: sitemapEntryFor(config, route),
		})
	}

	const native = loadNative()
	const siteJson = native.generateSiteFull(
		JSON.stringify({
			config: {
				out_dir: outDir,
				lang,
				origin: config.origin ?? '',
				disallow: config.disallow ?? [],
				sitemap: config.sitemap !== false,
				robots: config.robots !== false,
				hydration_manifest: config.hydrationManifest !== false,
			},
			pages,
		})
	)
	const site = JSON.parse(siteJson) as NativeGeneratedSite

	const outPages: SitePage[] = []
	const artifacts: SiteArtifact[] = []

	// Write the prerendered documents (file I/O is the host glue).
	for (const page of site.pages) {
		const bytes = await write(page.output, page.document)
		outPages.push({
			url: page.url,
			output: page.output,
			routePath: page.routePath,
			parameterized: page.parameterized,
			bytes,
			islands: page.islands,
		})
	}

	// Static assets: mirror the asset directory into the output (TS I/O glue).
	if (config.assetsDir !== undefined) {
		const copied = await copyAssets(config.assetsDir, outDir, fs)
		for (const asset of copied) artifacts.push(assetArtifact(asset))
	}

	// Write the Rust-emitted artifacts (sitemap.xml, robots.txt, hydration manifest).
	for (const artifact of site.artifacts) {
		const bytes = await write(artifact.output, artifact.contents)
		artifacts.push({ kind: artifact.kind, output: artifact.output, bytes })
	}

	return { outDir, pages: outPages, artifacts, hydration: site.hydration.routes }
}

/** Build the {@link SiteArtifact} record for one copied asset. */
function assetArtifact(asset: CopiedAsset): SiteArtifact {
	return { kind: 'asset', output: asset.output, bytes: asset.bytes }
}
