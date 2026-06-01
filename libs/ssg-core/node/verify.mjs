// JS-verify for the `treaty_ssg_node` NAPI binding (Phase 1).
//
// Loads the built `.node` over the loader, drives `generateSite` over a realistic
// fixture route set, and adversarially asserts the binding returns what the pure
// Rust SSG core (`treaty_ssg`) produces:
//   - one prerendered HTML page per concrete route (static + parameterized),
//   - `sitemap.xml`, `robots.txt`, and the hydration-manifest artifacts,
//   - a hydration manifest with one descriptor per route,
// and that the WHOLE pipeline is DETERMINISTIC: two calls over identical inputs
// return byte-identical JSON, and the `prerenderSite` alias matches `generateSite`.
//
// All SSG decisions are made in Rust; this script only feeds JSON in and checks
// the JSON out — exactly as the `@treaty/ssg` shim will. Run: node verify.mjs

import { createRequire } from 'node:module'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import assert from 'node:assert/strict'

const require = createRequire(import.meta.url)
const here = dirname(fileURLToPath(import.meta.url))
const { generateSite, prerenderSite } = require(join(here, 'index.js'))

assert.equal(typeof generateSite, 'function', 'generateSite must be exported')
assert.equal(typeof prerenderSite, 'function', 'prerenderSite must be exported')

// A real Ivy `*_Template`: an `<article>` wrapping an `<h1>` (interpolating
// `ctx.title`) and a `<p>` (interpolating `ctx.description`) — the exact
// instruction-stream shape Ivy lowers such a template to.
const PAGE_IVY = `
  function Page_Template(rf, ctx) {
    if (rf & 1) {
      i0.ɵɵelementStart(0, "article", ["class", "post"]);
      i0.ɵɵelementStart(1, "h1");
      i0.ɵɵtext(2);
      i0.ɵɵelementEnd();
      i0.ɵɵelementStart(3, "p");
      i0.ɵɵtext(4);
      i0.ɵɵelementEnd();
      i0.ɵɵelementEnd();
    }
    if (rf & 2) {
      i0.ɵɵadvance(2);
      i0.ɵɵtextInterpolate(ctx.title);
      i0.ɵɵadvance(2);
      i0.ɵɵtextInterpolate(ctx.description);
    }
  }
`

// The core's structs carry snake_case serde field names (no camelCase rename),
// so the JSON boundary uses `out_dir` / `has_component` / `ivy_code` /
// `component_id` / `macro_input` verbatim. The render data is already resolved
// upstream (the Nova macro boundary lives outside this deterministic binding),
// so each input omits `macro_source`.
const CONFIG = {
  out_dir: 'dist/site',
  origin: 'https://treaty.dev',
  lang: 'en',
  disallow: ['/draft'],
  sitemap: true,
  robots: true,
  hydration_manifest: true,
}

function input(componentId, title, description) {
  return {
    ivy_code: PAGE_IVY,
    component_id: componentId,
    macro_input: { title, description },
  }
}

const RENDER_INPUTS = {
  // A static index, a static `about`, and a parameterized `blog/:slug` (its slug
  // universe supplied via the static params map). The `blog` layout (no own
  // component) is naturally skipped: no `inputs` entry => the core skips it.
  routes: [
    { path: '', has_component: true },
    { path: 'about', has_component: true },
    { path: 'blog/:slug', has_component: true },
  ],
  params: {
    'blog/:slug': [{ slug: 'hello-world' }, { slug: 'ssg in rust' }],
  },
  inputs: {
    '/': input('page.tsx', 'Treaty SSG', 'The pure Rust SSG core.'),
    '/about': input('page.tsx', 'About', 'Who builds Treaty.'),
    '/blog/hello-world': input('page.tsx', 'Hello, World', 'The first post.'),
    // The space-bearing slug is percent-encoded in the discovered URL.
    '/blog/ssg%20in%20rust': input('page.tsx', 'SSG in Rust', 'Prerendering, the Rust way.'),
  },
}

const configJson = JSON.stringify(CONFIG)
const inputsJson = JSON.stringify(RENDER_INPUTS)

// ---- call the binding ------------------------------------------------------
const json = generateSite(configJson, inputsJson)
assert.equal(typeof json, 'string', 'binding returns a JSON string')
const site = JSON.parse(json)

// ---- pages: four discovered (the `blog` layout is skipped) -----------------
assert.equal(site.out_dir, 'dist/site', 'out_dir threads through from config')
assert.equal(site.pages.length, 4, 'index + about + two posts')

const byUrl = new Map(site.pages.map((p) => [p.url, p]))
const page = (url) => {
  const p = byUrl.get(url)
  assert.ok(p, `missing prerendered page ${url}`)
  return p
}

// The index page: full hydration-ready document with both interpolations filled.
const index = page('/')
assert.equal(index.output, 'dist/site/index.html', 'index output path')
assert.ok(index.document.startsWith('<!doctype html>\n<html lang="en">'), 'full HTML document')
assert.ok(
  index.document.includes(
    '<article class="post"><h1>Treaty SSG</h1><p>The pure Rust SSG core.</p></article>',
  ),
  'index render fills both interpolation slots from the resolved render data',
)
assert.equal(index.bytes, Buffer.byteLength(index.document, 'utf8'), 'byte count matches document')

// The parameterized post: nested output path + percent-encoded URL.
const post = page('/blog/ssg%20in%20rust')
assert.equal(post.output, 'dist/site/blog/ssg%20in%20rust/index.html', 'nested output path')
assert.equal(post.parameterized, true, 'flagged parameterized')
assert.ok(
  post.document.includes(
    '<article class="post"><h1>SSG in Rust</h1><p>Prerendering, the Rust way.</p></article>',
  ),
  'post render fills both slots',
)

// Hydration islands: a component island + one interpolation island per ɵɵtextInterpolate.
assert.equal(index.islands.length, 3, '1 component + 2 interpolation islands')
assert.equal(index.islands[0].kind, 'component', 'first island is the root component')
assert.equal(
  index.islands.filter((i) => i.kind === 'interpolation').length,
  2,
  'two interpolation islands',
)

// ---- artifacts: sitemap.xml + robots.txt + hydration manifest --------------
const artifact = (kind) => {
  const hits = site.artifacts.filter((a) => a.kind === kind)
  assert.equal(hits.length, 1, `exactly one ${kind} artifact`)
  return hits[0]
}

const sitemap = artifact('sitemap')
assert.equal(sitemap.output, 'dist/site/sitemap.xml', 'sitemap output path')
assert.ok(sitemap.contents.startsWith('<?xml version="1.0" encoding="UTF-8"?>'), 'sitemap is XML')
assert.ok(
  sitemap.contents.includes('<loc>https://treaty.dev/blog/ssg%20in%20rust</loc>'),
  'sitemap carries the absolute, encoded post URL',
)
assert.ok(sitemap.contents.includes('<loc>https://treaty.dev/</loc>'), 'sitemap carries the index')
assert.equal(sitemap.bytes, Buffer.byteLength(sitemap.contents, 'utf8'), 'sitemap byte count')

const robots = artifact('robots')
assert.equal(robots.output, 'dist/site/robots.txt', 'robots output path')
assert.ok(robots.contents.includes('User-agent: *'), 'robots has a user-agent line')
assert.ok(robots.contents.includes('Disallow: /draft'), 'robots honours the disallow list')
assert.ok(
  robots.contents.includes('Sitemap: https://treaty.dev/sitemap.xml'),
  'robots advertises the absolute sitemap URL',
)

const manifestArtifact = artifact('hydration-manifest')
assert.equal(manifestArtifact.output, 'dist/site/treaty-hydration.json', 'manifest output path')
const parsedManifest = JSON.parse(manifestArtifact.contents)
assert.deepEqual(parsedManifest, site.hydration, 'manifest artifact JSON equals the in-memory manifest')

// ---- hydration manifest: one descriptor per route, all carry state ---------
assert.equal(site.hydration.version, 1, 'manifest schema version 1')
assert.equal(site.hydration.routes.length, 4, 'one hydration descriptor per page')
assert.ok(
  site.hydration.routes.every((r) => r.has_state),
  'every page has non-empty render data, so every route embeds hydration state',
)

// ---- DETERMINISM: a second call over identical inputs is byte-identical ----
const again = generateSite(configJson, inputsJson)
assert.equal(again, json, 'generateSite is deterministic: identical inputs => byte-identical JSON')

// ---- the `prerenderSite` alias matches `generateSite` ----------------------
const viaAlias = prerenderSite(configJson, inputsJson)
assert.equal(viaAlias, json, 'prerenderSite alias returns byte-identical output')

// ---- a malformed config is a thrown JS error, not a crash ------------------
assert.throws(
  () => generateSite('not json', inputsJson),
  /invalid SSG config JSON/,
  'malformed config throws a precise error',
)

console.log(
  `OK: treaty_ssg_node returned ${site.pages.length} HTML pages + ${site.artifacts.length} artifacts ` +
    `(sitemap.xml, robots.txt, hydration manifest); output is deterministic and the prerenderSite alias matches.`,
)
