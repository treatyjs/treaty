/**
 * @module
 *
 * **`sitemap.xml` + `robots.txt` emit** for `@treaty/ssg`. Once the prerender
 * pipeline knows the concrete URLs it materialized, it can emit the two static
 * discovery artifacts every static site wants: a sitemap enumerating those URLs
 * for crawlers and a robots policy that points at it.
 *
 * The builders themselves live in the Rust SSG core (`treaty_ssg::manifest`, via
 * the `@treaty/ssg-node` addon) — per [[rust-core-ts-shim-layering]] these are
 * deterministic string builders that belong in Rust. This module is the thin
 * marshalling wrapper exposing them under the package's public API; the
 * whole-site generator emits these artifacts directly through the core's
 * `generateSiteFull`, so these standalone helpers are for callers that want a
 * sitemap/robots string in isolation.
 *
 * Treaty is a compiler, not a host: these are static files the generator emits;
 * serving them is the platform's job.
 */

import { loadNative } from './native.js'

/** A single URL entry for the sitemap. */
export interface SitemapEntry {
	/** The site-root-relative URL path (`'/'`, `'/blog/hello'`). */
	readonly url: string
	/** ISO-8601 last-modified date, emitted as `<lastmod>` when present. */
	readonly lastmod?: string
	/** Change frequency hint, emitted as `<changefreq>` when present. */
	readonly changefreq?: 'always' | 'hourly' | 'daily' | 'weekly' | 'monthly' | 'yearly' | 'never'
	/** Crawl priority in `[0,1]`, emitted as `<priority>` when present. */
	readonly priority?: number
}

/**
 * Join a site origin (`https://example.com`, possibly with a trailing slash)
 * with a root-relative URL path (`'/about'`) into one absolute, deduplicated-slash
 * location. A path that is already absolute (`http(s)://…`) is returned verbatim.
 * Pure pass-through to the Rust core's `absolute_url`.
 */
export function absoluteUrl(origin: string, urlPath: string): string {
	return loadNative().absoluteUrl(origin, urlPath)
}

/**
 * Render a sitemap XML document for `entries`, resolving each entry's `url`
 * against `origin` into an absolute `<loc>`. Output is deterministic (entries in
 * the order given) and minimal — only the optional fields actually supplied are
 * emitted — so it is reproducible across builds. The Rust core builds the XML.
 */
export function buildSitemap(origin: string, entries: readonly SitemapEntry[]): string {
	return loadNative().buildSitemap(JSON.stringify({ origin, entries }))
}

/** Options for {@link buildRobots}. */
export interface RobotsOptions {
	/**
	 * The absolute `Sitemap:` URL to advertise. Omit to emit no sitemap line
	 * (e.g. when no `origin` is configured).
	 */
	readonly sitemapUrl?: string
	/**
	 * Path prefixes to disallow for all agents (`['/admin', '/draft']`). Each
	 * becomes a `Disallow:` line. Defaults to none (everything allowed).
	 */
	readonly disallow?: readonly string[]
}

/**
 * Render a `robots.txt` body: a single `User-agent: *` group that allows
 * crawling (with any supplied `disallow` prefixes), optionally followed by a
 * `Sitemap:` line. Deterministic; the Rust core builds the body.
 */
export function buildRobots(options: RobotsOptions = {}): string {
	return loadNative().buildRobots(
		JSON.stringify({ sitemapUrl: options.sitemapUrl, disallow: options.disallow ?? [] })
	)
}
