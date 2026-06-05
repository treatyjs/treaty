/**
 * @module
 *
 * **The native SSG core bridge.** Loads the `treaty_ssg_node` NAPI addon — the
 * thin binding over the pure Rust SSG core (`treaty_ssg`, at `libs/ssg-core`) —
 * and exposes its JSON-in/JSON-out entry points to the rest of `@treaty/ssg`.
 *
 * Per [[rust-core-ts-shim-layering]], the deterministic SSG logic (route
 * discovery + parameterized fan-out, the Ivy → static-HTML interpreter, head /
 * SEO emit, `sitemap.xml` / `robots.txt`, the per-route hydration manifest)
 * lives in Rust. This module is the single place the TS shim reaches into it; the
 * rest of the package only does the host glue Rust cannot: running route
 * resolution / `getStaticPaths` callbacks, executing render-time macros through
 * the Nova {@link RenderRuntime}, and writing files through a {@link
 * FileSystemPort}.
 *
 * The addon is `@treaty/ssg-node` (a detached, private NAPI crate with its own
 * `target/`). It is resolved by package name first (when the workspace links it
 * into `node_modules`) and otherwise by its known in-repo location, mirroring how
 * the Nova addon is loaded defensively — so a build that has not linked the
 * binding fails with a precise error rather than a confusing module-not-found.
 */

import { createRequire } from 'node:module'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'

/** The functions the `treaty_ssg_node` addon exports (JSON in, JSON out). */
export interface SsgNativeBinding {
	/**
	 * Enumerate the concrete prerenderable routes from a routes config. Input is
	 * `{ routes, params, includeComponentless }` (the structural route subset +
	 * static param sets + the componentless flag); output is the
	 * `DiscoveredRoute[]` (`url` / `routePath` / `parameterized` / `params`) JSON.
	 */
	discoverRoutes(discoverInputsJson: string): string
	/**
	 * Statically interpret emitted Ivy JS for one component into an HTML fragment,
	 * binding interpolations against `dataJson` (a JSON object, or `{}`). Returns
	 * the empty string for a module with no template function.
	 */
	renderIvyToHtml(ivyCode: string, dataJson: string): string
	/**
	 * Render a `sitemap.xml` document. Input is `{ origin, entries }` (the site
	 * origin + the `SitemapEntry[]`); output is the XML string.
	 */
	buildSitemap(sitemapInputsJson: string): string
	/**
	 * Render a `robots.txt` body. Input is `{ sitemapUrl?, disallow? }`; output is
	 * the robots string.
	 */
	buildRobots(robotsInputsJson: string): string
	/** Join a site origin with a root-relative URL path into one absolute URL. */
	absoluteUrl(origin: string, urlPath: string): string
	/**
	 * Generate a whole site from a config + a fully-resolved page list. Input is
	 * `{ config, pages }` where every page carries its already-resolved `data`,
	 * emitted `ivyCode`, and optional `title` / `head` / `sitemapEntry` overrides;
	 * output is the `GeneratedSite` JSON (every page's document + the sitemap /
	 * robots / hydration-manifest artifacts + the in-memory manifest).
	 */
	generateSiteFull(siteInputsJson: string): string
	/**
	 * The Phase-1 entry: drive the core's `prerender_site` over a config + a
	 * per-URL render-input map. Retained for callers that prefer the
	 * routes-config-in form; {@link generateSiteFull} is what the shim uses.
	 */
	generateSite(configJson: string, renderInputsJson: string): string
	/** Alias of {@link generateSite} under the core's pipeline name. */
	prerenderSite(configJson: string, renderInputsJson: string): string
}

/** Raised when the native SSG core addon cannot be loaded. */
export class SsgNativeError extends Error {
	constructor(message: string, options?: { cause?: unknown }) {
		super(message, options)
		this.name = 'SsgNativeError'
	}
}

const require = createRequire(import.meta.url)

/**
 * Candidate module specifiers for the `treaty_ssg_node` addon loader, most
 * portable first:
 *   1. the published/linked package name (resolved through `node_modules`),
 *   2. the in-repo detached crate's loader, relative to this file's location.
 *      Built `dist/` sits at `libs/treaty/ssg/dist` (and the source at
 *      `libs/treaty/ssg/src`), so three levels up reaches `libs/` and the addon
 *      is at `libs/ssg-core/node/index.js`.
 */
function candidates(): readonly string[] {
	const here = dirname(fileURLToPath(import.meta.url))
	return ['@treaty/ssg-node', join(here, '..', '..', '..', 'ssg-core', 'node', 'index.js')]
}

let cached: SsgNativeBinding | null = null

/**
 * Load (and memoize) the native SSG core binding. Throws {@link SsgNativeError}
 * with the underlying cause when no candidate resolves, so a misconfigured build
 * gets a precise message instead of a bare `MODULE_NOT_FOUND`.
 */
export function loadNative(): SsgNativeBinding {
	if (cached !== null) return cached
	let lastError: unknown
	for (const specifier of candidates()) {
		try {
			const mod = require(specifier) as Partial<SsgNativeBinding>
			if (
				typeof mod.discoverRoutes === 'function' &&
				typeof mod.generateSiteFull === 'function' &&
				typeof mod.renderIvyToHtml === 'function' &&
				typeof mod.buildSitemap === 'function' &&
				typeof mod.buildRobots === 'function' &&
				typeof mod.absoluteUrl === 'function'
			) {
				cached = mod as SsgNativeBinding
				return cached
			}
		} catch (cause) {
			lastError = cause
		}
	}
	throw new SsgNativeError(
		'Failed to load the treaty_ssg_node native SSG core addon (@treaty/ssg-node). ' +
			'Build it with `napi build --platform --release` in libs/ssg-core/node.',
		{ cause: lastError }
	)
}
