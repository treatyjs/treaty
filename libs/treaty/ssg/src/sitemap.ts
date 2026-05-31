/**
 * @module
 *
 * **`sitemap.xml` + `robots.txt` emit** for `@treaty/ssg`. Once the prerender
 * pipeline knows the concrete URLs it materialized, it can emit the two static
 * discovery artifacts every static site wants: a sitemap enumerating those URLs
 * for crawlers and a robots policy that points at it. Both are pure
 * string-builders (no I/O) so the site generator owns when/where to write them.
 *
 * Treaty is a compiler, not a host: these are static files the generator emits;
 * serving them is the platform's job.
 */

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

/** Escape text for safe embedding inside XML element bodies / attributes. */
function escapeXml(value: string): string {
	return value
		.replace(/&/g, '&amp;')
		.replace(/</g, '&lt;')
		.replace(/>/g, '&gt;')
		.replace(/"/g, '&quot;')
		.replace(/'/g, '&apos;')
}

/**
 * Join a site origin (`https://example.com`, possibly with a trailing slash)
 * with a root-relative URL path (`'/about'`) into one absolute, deduplicated-slash
 * location. A path that is already absolute (`http(s)://…`) is returned verbatim.
 */
export function absoluteUrl(origin: string, urlPath: string): string {
	if (/^https?:\/\//i.test(urlPath)) return urlPath
	const base = origin.replace(/\/+$/, '')
	const path = urlPath.startsWith('/') ? urlPath : `/${urlPath}`
	return `${base}${path}`
}

/**
 * Render a sitemap XML document for `entries`, resolving each entry's `url`
 * against `origin` into an absolute `<loc>`. Output is deterministic (entries in
 * the order given) and minimal — only the optional fields actually supplied are
 * emitted — so it is reproducible across builds.
 */
export function buildSitemap(origin: string, entries: readonly SitemapEntry[]): string {
	const lines = ['<?xml version="1.0" encoding="UTF-8"?>', '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">']
	for (const entry of entries) {
		lines.push('  <url>')
		lines.push(`    <loc>${escapeXml(absoluteUrl(origin, entry.url))}</loc>`)
		if (entry.lastmod !== undefined) lines.push(`    <lastmod>${escapeXml(entry.lastmod)}</lastmod>`)
		if (entry.changefreq !== undefined) lines.push(`    <changefreq>${entry.changefreq}</changefreq>`)
		if (entry.priority !== undefined) {
			lines.push(`    <priority>${clampPriority(entry.priority).toFixed(1)}</priority>`)
		}
		lines.push('  </url>')
	}
	lines.push('</urlset>')
	return `${lines.join('\n')}\n`
}

/** Clamp a sitemap priority into the valid `[0,1]` range. */
function clampPriority(priority: number): number {
	if (!Number.isFinite(priority)) return 0.5
	return Math.min(1, Math.max(0, priority))
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
 * `Sitemap:` line. Deterministic and dependency-free.
 */
export function buildRobots(options: RobotsOptions = {}): string {
	const lines = ['User-agent: *']
	const disallow = options.disallow ?? []
	if (disallow.length === 0) {
		lines.push('Allow: /')
	} else {
		for (const prefix of disallow) lines.push(`Disallow: ${prefix.startsWith('/') ? prefix : `/${prefix}`}`)
	}
	if (options.sitemapUrl !== undefined) {
		lines.push('')
		lines.push(`Sitemap: ${options.sitemapUrl}`)
	}
	return `${lines.join('\n')}\n`
}
