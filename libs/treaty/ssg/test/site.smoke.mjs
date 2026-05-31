/**
 * Node smoke test for the @treaty/ssg WHOLE-SITE generator (prerenderSite).
 *
 * Exercises the public surface against the built dist (what a build tool would
 * consume), using an in-memory FileSystemPort so nothing touches disk. It asserts:
 *   1. discoverRoutesAsync resolves a parameterized route's params through a
 *      getStaticPaths-style provider (combined with a static params map),
 *   2. buildSitemap / buildRobots emit well-formed crawler artifacts with the
 *      origin made absolute,
 *   3. copyAssets mirrors a static asset directory into the output (in-memory FS),
 *   4. prerenderSite (THE whole-site entry) prerenders every route to static HTML,
 *      copies assets, and emits sitemap.xml + robots.txt + the hydration manifest,
 *      returning the full artifact set; the hydration manifest lists islands per
 *      route and SEO meta (description/canonical) lands in the document <head>,
 *   5. (when the prebuilt @treaty/authoring-node addon is present) prerenderSite
 *      drives a route through the REAL Nova runtime: a macro that COMPUTES its
 *      render data (beyond the stub) executes in Nova and the computed value
 *      appears in the emitted static HTML.
 *
 * Cases 1–4 use the built-in StubRenderRuntime (no native addon). Case 5 plugs the
 * Nova-backed RenderRuntime in via createNovaRenderRuntime; skipped (not failed)
 * when the prebuilt addon is absent.
 *
 * Run: node libs/treaty/ssg/test/site.smoke.mjs
 */

import assert from 'node:assert/strict'
import {
	prerenderSite,
	discoverRoutesAsync,
	buildSitemap,
	buildRobots,
	copyAssets,
	createNovaRenderRuntime,
	StubRenderRuntime,
	HYDRATION_MANIFEST_FILE,
} from '../dist/index.js'

let failures = 0
const results = []

function check(label, fn) {
	return Promise.resolve()
		.then(fn)
		.then(() => results.push(`PASS ${label}`))
		.catch((err) => {
			failures++
			results.push(`FAIL ${label}: ${err.stack ?? err.message}`)
		})
}

async function loadNovaAddon() {
	try {
		const addon = await import('@treaty/authoring-node')
		const runMacro = addon.runMacro ?? addon.default?.runMacro
		return typeof runMacro === 'function' ? { runMacro } : null
	} catch {
		return null
	}
}

// A component whose template interpolates a single binding.
const interpolationComponent =
	"import { Component } from '@angular/core';\n" +
	"@Component({ selector: 'app-hello', template: '<h1>{{ title }}</h1>' })\n" +
	'export class HelloComponent {}\n'

const decoder = new TextDecoder()

/**
 * An in-memory FileSystemPort. Writes land in a Map; an optional seed map
 * pre-populates files so copyAssets has a source directory to mirror.
 */
function memFs(seed = new Map()) {
	const files = new Map(seed)
	return {
		files,
		async readDir(dir) {
			const prefix = dir.replace(/[/\\]+$/, '') + '/'
			const names = new Map()
			for (const path of files.keys()) {
				if (!path.startsWith(prefix)) continue
				const rest = path.slice(prefix.length)
				const slash = rest.indexOf('/')
				if (slash === -1) names.set(rest, false)
				else names.set(rest.slice(0, slash), true)
			}
			return [...names].map(([name, isDirectory]) => ({ name, isDirectory }))
		},
		async readFile(path) {
			const v = files.get(path)
			if (v === undefined) throw new Error(`ENOENT ${path}`)
			return typeof v === 'string' ? new TextEncoder().encode(v) : v
		},
		async writeFile(path, contents) {
			files.set(path.replace(/\\/g, '/'), contents)
		},
		async exists(path) {
			const norm = path.replace(/[/\\]+$/, '')
			if (files.has(norm)) return true
			const prefix = norm + '/'
			for (const p of files.keys()) if (p.startsWith(prefix)) return true
			return false
		},
	}
}

function text(fs, path) {
	const v = fs.files.get(path)
	return v === undefined ? undefined : typeof v === 'string' ? v : decoder.decode(v)
}

await check('discoverRoutesAsync resolves params via a getStaticPaths provider', async () => {
	const routes = await discoverRoutesAsync([{ path: 'blog/:slug', component: {} }], {
		params: { 'blog/:slug': [{ slug: 'static-one' }] },
		getStaticPaths: async ({ routePath, params }) => {
			assert.equal(routePath, 'blog/:slug', 'provider sees the full declared path')
			assert.deepEqual([...params], ['slug'], 'provider sees the param names')
			return [{ slug: 'computed-two' }, { slug: 'computed-three' }]
		},
	})
	const urls = routes.map((r) => r.url).sort()
	assert.deepEqual(
		urls,
		['/blog/computed-three', '/blog/computed-two', '/blog/static-one'],
		'static params + provider params combined'
	)
})

await check('buildSitemap / buildRobots emit well-formed crawler artifacts', () => {
	const xml = buildSitemap('https://example.com/', [
		{ url: '/', priority: 1 },
		{ url: '/about', changefreq: 'monthly' },
	])
	assert.ok(xml.includes('<?xml version="1.0" encoding="UTF-8"?>'), 'xml prolog')
	assert.ok(xml.includes('<loc>https://example.com/</loc>'), 'origin made absolute, root url')
	assert.ok(xml.includes('<loc>https://example.com/about</loc>'), 'about url')
	assert.ok(xml.includes('<priority>1.0</priority>'), 'priority clamped/formatted')
	assert.ok(xml.includes('<changefreq>monthly</changefreq>'), 'changefreq emitted')

	const robots = buildRobots({ sitemapUrl: 'https://example.com/sitemap.xml', disallow: ['/admin'] })
	assert.ok(robots.includes('User-agent: *'), 'robots agent group')
	assert.ok(robots.includes('Disallow: /admin'), 'disallow prefix')
	assert.ok(robots.includes('Sitemap: https://example.com/sitemap.xml'), 'sitemap line')
})

await check('copyAssets mirrors a static asset directory into the output', async () => {
	const fs = memFs(
		new Map([
			['public/favicon.ico', 'ICO'],
			['public/img/logo.png', 'PNG-BYTES'],
		])
	)
	const copied = await copyAssets('public', 'dist/ssg', fs)
	const paths = copied.map((c) => c.path).sort()
	assert.deepEqual(paths, ['favicon.ico', 'img/logo.png'], 'recursive copy preserves layout')
	assert.equal(text(fs, 'dist/ssg/favicon.ico'), 'ICO', 'favicon written to out dir')
	assert.equal(text(fs, 'dist/ssg/img/logo.png'), 'PNG-BYTES', 'nested asset written to out dir')

	// A missing source dir is a no-op.
	const empty = await copyAssets('does-not-exist', 'dist/ssg', memFs())
	assert.deepEqual(empty, [], 'missing asset dir copies nothing')
})

await check('prerenderSite emits HTML + assets + sitemap + robots + hydration manifest', async () => {
	const fs = memFs(new Map([['public/favicon.ico', 'ICO']]))
	const manifest = await prerenderSite({
		routes: [
			{ path: '', component: {} },
			{ path: 'blog/:slug', component: {} },
		],
		getStaticPaths: ({ routePath }) =>
			routePath === 'blog/:slug' ? [{ slug: 'hello' }] : undefined,
		resolve: () => ({
			source: interpolationComponent,
			macro: { source: 'export default { title: "Hi", description: "A page", canonical: "https://x.test/" }' },
		}),
		outDir: 'dist/ssg',
		origin: 'https://x.test',
		assetsDir: 'public',
		fs,
	})

	// Pages: root + one parameterized.
	const urls = manifest.pages.map((p) => p.url).sort()
	assert.deepEqual(urls, ['/', '/blog/hello'], 'both routes prerendered')

	const root = manifest.pages.find((p) => p.url === '/')
	assert.equal(root.output, 'dist/ssg/index.html', 'root -> index.html')
	const blog = manifest.pages.find((p) => p.url === '/blog/hello')
	assert.equal(blog.output, 'dist/ssg/blog/hello/index.html', 'parameterized -> nested index.html')

	const doc = text(fs, root.output)
	assert.ok(doc.includes('<h1>Hi</h1>'), 'macro-rendered interpolation in HTML')
	assert.ok(doc.includes('<meta name="description" content="A page">'), 'SEO description in head')
	assert.ok(doc.includes('<link rel="canonical" href="https://x.test/">'), 'canonical link in head')

	// Hydration manifest lists islands per route.
	assert.equal(root.islands[0].kind, 'component', 'root component island recorded')
	assert.ok(
		root.islands.some((i) => i.kind === 'interpolation'),
		'interpolation island detected from the template'
	)
	const hydrationArtifact = manifest.artifacts.find((a) => a.kind === 'hydration-manifest')
	assert.ok(hydrationArtifact, 'hydration manifest artifact emitted')
	const hydrationJson = JSON.parse(text(fs, hydrationArtifact.output))
	assert.equal(hydrationJson.version, 1, 'hydration manifest versioned')
	assert.equal(hydrationJson.routes.length, 2, 'one hydration entry per page')
	assert.ok(hydrationArtifact.output.endsWith(HYDRATION_MANIFEST_FILE), 'manifest at expected path')

	// sitemap.xml with both urls, made absolute.
	const sitemap = manifest.artifacts.find((a) => a.kind === 'sitemap')
	const sitemapXml = text(fs, sitemap.output)
	assert.ok(sitemapXml.includes('<loc>https://x.test/</loc>'), 'sitemap root loc absolute')
	assert.ok(sitemapXml.includes('<loc>https://x.test/blog/hello</loc>'), 'sitemap blog loc absolute')

	// robots.txt advertising the sitemap.
	const robots = manifest.artifacts.find((a) => a.kind === 'robots')
	assert.ok(text(fs, robots.output).includes('Sitemap: https://x.test/sitemap.xml'), 'robots advertises sitemap')

	// Asset mirrored.
	assert.equal(text(fs, 'dist/ssg/favicon.ico'), 'ICO', 'asset copied into the site output')
	assert.ok(
		manifest.artifacts.some((a) => a.kind === 'asset' && a.output === 'dist/ssg/favicon.ico'),
		'copied asset in artifact set'
	)
})

// Case 5: the REAL Nova runtime end-to-end through prerenderSite.
const novaAddon = await loadNovaAddon()
if (novaAddon) {
	await check('prerenderSite executes a computed macro through the REAL Nova runtime', async () => {
		const runtime = createNovaRenderRuntime(novaAddon)
		const computedMacro = {
			source: 'const items = [1, 2, 3, 4]; ({ title: items.reduce((a, b) => a + b, input.base) })',
			input: { base: 10 },
		}
		// Sanity: the stub canNOT interpret this macro.
		assert.throws(
			() => new StubRenderRuntime().runMacro(computedMacro),
			/StubRenderRuntime supports only a literal-object macro/,
			'computed macro is beyond the stub — a true Nova path'
		)

		const fs = memFs()
		const manifest = await prerenderSite({
			routes: [{ path: 'computed', component: {} }],
			resolve: () => ({ source: interpolationComponent, macro: computedMacro }),
			runtime,
			outDir: 'dist/ssg',
			fs,
		})
		const page = manifest.pages.find((p) => p.url === '/computed')
		assert.ok(page, 'computed route prerendered through Nova')
		assert.ok(
			text(fs, page.output).includes('<h1>20</h1>'),
			'static HTML contains the value COMPUTED by Nova (1+2+3+4+10)'
		)
	})
} else {
	results.push('SKIP Nova run_macro case (prebuilt @treaty/authoring-node addon not present)')
}

for (const line of results) console.log(line)
if (failures > 0) {
	console.error(`\nSITE SMOKE TEST FAILED: ${failures} case(s) failed`)
	process.exit(1)
}
console.log('\nSITE SMOKE TEST PASSED')
