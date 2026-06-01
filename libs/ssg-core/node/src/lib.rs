//! `treaty_ssg_node` — the NAPI binding over the pure Rust SSG core.
//!
//! Treaty is a compiler, not a host. The deterministic Static-Site-Generation
//! logic — route discovery + parameterized fan-out, the Ivy -> static-HTML
//! interpreter, head/SEO emit, `sitemap.xml` / `robots.txt`, and the per-route
//! hydration manifest — lives in the `treaty_ssg` crate (`libs/ssg-core`). This
//! binding is *pure glue*: it takes the site config and the per-route render
//! inputs as JSON, drives the `treaty_ssg` pipeline, and returns the whole
//! generated site (pages' HTML + the sitemap/robots/manifest artifacts) back as
//! JSON. No SSG decision is made here — every byte of output is produced by the
//! Rust core, so `@treaty/ssg` is a thin shim that only does the disk writes.
//! See [[rust-core-ts-shim-layering]].
//!
//! ## Entry points
//!
//! * [`discover_routes`] — fan a routes config out into the concrete
//!   prerenderable URLs (static + parameterized). The deterministic discovery
//!   walk runs in [`treaty_ssg::discovery`]; the TS shim re-attaches each route
//!   node (which cannot cross the JSON boundary) to the returned descriptors.
//! * [`generate_site`] / [`prerender_site`] — the original Phase-1 entry: drive
//!   [`treaty_ssg::prerender_site`] over a config + a per-URL render-input map,
//!   using a pass-through seam (the caller resolves render data upstream).
//! * [`generate_site_full`] — the richer entry the `@treaty/ssg` shim uses: the
//!   caller has already discovered routes and resolved each one's render data
//!   (compiling its component to Ivy + running any macro through Nova upstream),
//!   so it passes a fully-resolved, ordered `pages` list with optional per-route
//!   `title` / `head` / `sitemap_entry` overrides. This binding then drives the
//!   pure `treaty_ssg` modules (`ivy_html` -> `seo` -> `manifest`) to emit every
//!   document and the sitemap/robots/hydration-manifest artifacts. It exists so
//!   the shim can honour the TS package's full public surface (per-route title,
//!   head, sitemap enrichment) while keeping all emit logic in Rust.
//!
//! ## The seam
//!
//! The one runtime boundary the core abstracts is [`treaty_ssg::RenderSeam`] —
//! the Nova `run_macro` execution that turns a route's render-time macro into
//! its render data. That execution is NOT part of this deterministic binding:
//! the caller resolves each route's render data ahead of time (running its macro
//! through the Nova runtime in the TS/runtime layer) and passes the finished
//! render data, so the binding never needs a runtime. The Phase-1 entries
//! install a pass-through seam to honour [`treaty_ssg::prerender_site`]'s
//! signature; [`generate_site_full`] takes the data verbatim and never touches
//! the seam at all.

#[macro_use]
extern crate napi_derive;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use treaty_ssg::discovery::{
    discover_routes as core_discover_routes, DiscoverOptions, RouteParamsMap,
};
use treaty_ssg::{
    ivy_html, manifest, prerender_site as core_prerender_site, seo, ArtifactKind, ChangeFreq,
    DiscoveredRoute, GeneratedSite, HeadMeta, PrerenderedPage, RenderData, RenderSeam,
    RouteHydration, RouteRenderInput, RouteSpec, SiteArtifact, SitemapEntry, SsgConfig,
};

/// The JSON payload describing what to prerender for the Phase-1 entries: the
/// routes config to discover, the static parameter sets that fan parameterized
/// routes out, and the already-resolved render input per concrete route URL.
///
/// Mirrors what the TS shim assembles before calling into the core: it has
/// compiled each route's component to Ivy and (for routes with a render-time
/// macro) run that macro through the Nova runtime to obtain the render data, so
/// every [`RouteRenderInput`] here carries its final `ivy_code` + `macro_input`
/// (render data) with `macro_source` omitted. The core then needs no runtime.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RenderInputs {
    /// The app's routes config (the structural subset the discovery walk needs).
    #[serde(default)]
    routes: Vec<RouteSpec>,
    /// Static parameter sets keyed by declared route path (e.g. `"blog/:slug"`),
    /// used to materialize parameterized routes. Mirrors the TS `RouteParamsMap`.
    #[serde(default)]
    params: RouteParamsMap,
    /// The per-route render input, keyed by the *concrete discovered URL*
    /// (`"/"`, `"/about"`, `"/blog/hello-world"`). A discovered route with no
    /// entry here is skipped (the core's provider-returns-`None` path), so a
    /// layout/index route with no own component is naturally omitted.
    #[serde(default)]
    inputs: BTreeMap<String, RouteRenderInput>,
}

/// A pass-through render seam: the caller has already resolved every route's
/// render data (running its macro through the Nova runtime upstream), so each
/// `RouteRenderInput` here arrives with `macro_source: None` and the core never
/// calls `render`. This impl exists only to satisfy the `prerender_site`
/// signature; were a macro source present it echoes the input verbatim, keeping
/// the binding deterministic.
struct PassThroughSeam;

impl RenderSeam for PassThroughSeam {
    fn render(&self, _macro_src: &str, input: &RenderData) -> RenderData {
        input.clone()
    }
}

/// Drive the pure SSG core over the JSON inputs and return the generated site.
///
/// Shared by both Phase-1 `#[napi]` entry points. Parsing errors surface as
/// `napi::Error` (a thrown JS error) so the shim sees a precise message rather
/// than a panic.
fn run(config_json: &str, render_inputs_json: &str) -> napi::Result<String> {
    let config: SsgConfig = serde_json::from_str(config_json)
        .map_err(|err| napi::Error::from_reason(format!("invalid SSG config JSON: {err}")))?;
    let RenderInputs {
        routes,
        params,
        inputs,
    } = serde_json::from_str(render_inputs_json)
        .map_err(|err| napi::Error::from_reason(format!("invalid render inputs JSON: {err}")))?;

    // The provider maps each discovered route to its pre-resolved render input by
    // concrete URL; a route with no entry is skipped (returns `None`). Cloning is
    // required because the core takes the input by value per route.
    let provider =
        |route: &DiscoveredRoute| -> Option<RouteRenderInput> { inputs.get(&route.url).cloned() };

    let site: GeneratedSite =
        core_prerender_site(&config, &routes, &params, &PassThroughSeam, &provider);

    serde_json::to_string(&site).map_err(|err| {
        napi::Error::from_reason(format!("failed to serialize generated site: {err}"))
    })
}

/// Generate a complete static site, returning the [`treaty_ssg::GeneratedSite`]
/// as JSON: every prerendered page's full HTML document, plus the emitted
/// `sitemap.xml`, `robots.txt`, and hydration-manifest artifacts and the
/// in-memory hydration manifest.
///
/// `config_json` is a `SsgConfig`; `render_inputs_json` is the routes config,
/// the static param sets, and the per-URL pre-resolved render inputs (see
/// [`RenderInputs`]). All SSG logic runs in the Rust core; this binding only
/// (de)serializes. Deterministic: identical inputs yield byte-identical JSON.
#[napi]
pub fn generate_site(config_json: String, render_inputs_json: String) -> napi::Result<String> {
    run(&config_json, &render_inputs_json)
}

/// Alias of [`generate_site`] under the core's pipeline name, so JS callers can
/// invoke the binding by the same name as the Rust entry point
/// (`prerenderSite`). Identical behaviour and output.
#[napi]
pub fn prerender_site(config_json: String, render_inputs_json: String) -> napi::Result<String> {
    run(&config_json, &render_inputs_json)
}

// ---------------------------------------------------------------------------
// Route discovery
// ---------------------------------------------------------------------------

/// The JSON payload for [`discover_routes`]: the routes config to walk and the
/// static parameter sets that fan parameterized routes out.
///
/// `getStaticPaths`-computed params (a TS async function seam) are resolved in
/// the shim and folded into `params` before the call, so this binding only ever
/// sees concrete static param sets — the deterministic walk has everything it
/// needs without a callback crossing the JSON boundary.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct DiscoverInputs {
    /// The app's routes config (the structural subset the discovery walk needs).
    #[serde(default)]
    routes: Vec<RouteSpec>,
    /// Static parameter sets keyed by declared route path. Mirrors `RouteParamsMap`.
    #[serde(default)]
    params: RouteParamsMap,
    /// Include componentless layout/redirect shells (the TS
    /// `includeComponentless`). Layout routes are walked for children regardless.
    #[serde(default, rename = "includeComponentless")]
    include_componentless: bool,
}

/// Enumerate every concrete, prerenderable route from a routes config, returning
/// the [`treaty_ssg::DiscoveredRoute`] list (`url`, `route_path`, `parameterized`,
/// `params`) as JSON.
///
/// The deterministic discovery walk — static + parameterized fan-out, wildcard /
/// pure-redirect dropping, recursive children — runs entirely in
/// [`treaty_ssg::discovery`]. The TS shim re-attaches each route node (the
/// non-serializable `route` field of its `DiscoveredRoute`) to the result, since
/// a route node carries function-valued loaders that cannot cross JSON.
#[napi]
pub fn discover_routes(discover_inputs_json: String) -> napi::Result<String> {
    let DiscoverInputs {
        routes,
        params,
        include_componentless,
    } = serde_json::from_str(&discover_inputs_json)
        .map_err(|err| napi::Error::from_reason(format!("invalid discover inputs JSON: {err}")))?;

    let options = DiscoverOptions {
        include_componentless,
    };
    let discovered = treaty_ssg::discovery::discover_routes_with(&routes, &params, &options, None);
    // The static-only path (no provider) is identical to `discover_routes`; using
    // `_with` lets the shim opt into `include_componentless` without a second
    // entry point. Keep a reference to the simple entry so it is not unused.
    let _ = core_discover_routes;

    serde_json::to_string(&discovered).map_err(|err| {
        napi::Error::from_reason(format!("failed to serialize discovered routes: {err}"))
    })
}

// ---------------------------------------------------------------------------
// Crawler-artifact string builders (the standalone sitemap/robots entries)
// ---------------------------------------------------------------------------

/// The JSON payload for [`build_sitemap`]: the site origin and the entry list.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SitemapInputs {
    /// Site origin used to make each entry's `<loc>` absolute. Empty => relative.
    #[serde(default)]
    origin: String,
    /// The URL entries, in the order they should appear.
    #[serde(default)]
    entries: Vec<SitemapEntry>,
}

/// Render a `sitemap.xml` document for the entries, resolving each `url` against
/// `origin` into an absolute `<loc>`. Pure pass-through to
/// [`treaty_ssg::manifest::build_sitemap`]; deterministic and minimal.
#[napi]
pub fn build_sitemap(sitemap_inputs_json: String) -> napi::Result<String> {
    let SitemapInputs { origin, entries } = serde_json::from_str(&sitemap_inputs_json)
        .map_err(|err| napi::Error::from_reason(format!("invalid sitemap inputs JSON: {err}")))?;
    Ok(manifest::build_sitemap(&origin, &entries))
}

/// The JSON payload for [`build_robots`]: the optional sitemap URL + disallow set.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RobotsInputs {
    /// Absolute `Sitemap:` URL to advertise. Omit to emit no sitemap line.
    #[serde(default, rename = "sitemapUrl", skip_serializing_if = "Option::is_none")]
    sitemap_url: Option<String>,
    /// Path prefixes to disallow for all agents. Empty allows everything.
    #[serde(default)]
    disallow: Vec<String>,
}

/// Render a `robots.txt` body (a single `User-agent: *` group with any disallow
/// prefixes, optionally advertising a sitemap). Pure pass-through to
/// [`treaty_ssg::manifest::build_robots`].
#[napi]
pub fn build_robots(robots_inputs_json: String) -> napi::Result<String> {
    let RobotsInputs {
        sitemap_url,
        disallow,
    } = serde_json::from_str(&robots_inputs_json)
        .map_err(|err| napi::Error::from_reason(format!("invalid robots inputs JSON: {err}")))?;
    Ok(manifest::build_robots(&manifest::RobotsOptions {
        sitemap_url,
        disallow,
    }))
}

/// Join a site `origin` with a root-relative `url_path` into one absolute,
/// deduplicated-slash location (an already-absolute `http(s)://…` path passes
/// through). Pure pass-through to [`treaty_ssg::manifest::absolute_url`].
#[napi]
pub fn absolute_url(origin: String, url_path: String) -> String {
    manifest::absolute_url(&origin, &url_path)
}

// ---------------------------------------------------------------------------
// Static Ivy -> HTML rendering (the standalone renderer entry)
// ---------------------------------------------------------------------------

/// Statically interpret emitted Ivy JS for one component into an HTML fragment,
/// binding interpolations against `data_json` (a JSON object of render data, or
/// `{}`). Returns the empty string when `ivy_code` carries no recognizable
/// template function (a pass-through module), matching the TS `renderIvyToHtml`.
///
/// The interpretation runs entirely in [`treaty_ssg::ivy_html`]; this binding
/// only parses the render data and stringifies the fragment. A non-object
/// `data_json` (or invalid JSON) surfaces as a thrown error so the shim sees a
/// precise message.
#[napi]
pub fn render_ivy_to_html(ivy_code: String, data_json: String) -> napi::Result<String> {
    let data: RenderData = serde_json::from_str(&data_json).map_err(|err| {
        napi::Error::from_reason(format!("invalid render data JSON (must be a JSON object): {err}"))
    })?;
    Ok(ivy_html::render_ivy_to_html(&ivy_code, &data))
}

// ---------------------------------------------------------------------------
// Full-site generation (the entry the @treaty/ssg shim drives)
// ---------------------------------------------------------------------------

/// Per-route sitemap enrichment passed by the shim (the resolved result of the
/// TS `sitemapEntry(route)` callback). Mirrors the optional fields of a
/// [`treaty_ssg::SitemapEntry`] plus an `exclude` flag for the callback's `null`
/// (drop-from-sitemap) return.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SitemapEntryOverride {
    /// Exclude this route from `sitemap.xml` entirely (the TS callback returned
    /// `null`). When `true`, the other fields are ignored.
    #[serde(default)]
    exclude: bool,
    /// ISO-8601 `<lastmod>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lastmod: Option<String>,
    /// `<changefreq>` hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    changefreq: Option<ChangeFreq>,
    /// `<priority>` in `[0,1]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    priority: Option<f64>,
}

/// One fully-resolved page the shim hands to [`generate_site_full`]: the
/// discovered route identity plus its already-resolved render data and the
/// emitted Ivy, with optional per-route head/title/sitemap overrides.
///
/// Everything that needs a runtime or a function seam (component compilation,
/// macro execution through Nova, and the `title` / `head` / `sitemapEntry`
/// callbacks of the TS `SiteConfig`) is resolved by the shim *before* this
/// crosses the boundary, so the binding can emit the document with no callbacks.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PageInput {
    /// The concrete discovered URL (`"/"`, `"/blog/hello"`).
    url: String,
    /// The declared route path before substitution (`"blog/:slug"`).
    #[serde(default, rename = "routePath")]
    route_path: String,
    /// Whether this page came from a parameterized route.
    #[serde(default)]
    parameterized: bool,
    /// Emitted Ivy JS for the route's compiled component (interpreted to HTML).
    #[serde(default, rename = "ivyCode")]
    ivy_code: String,
    /// Component / file identifier, used to name the hydration islands.
    #[serde(default, rename = "componentId")]
    component_id: String,
    /// The route's already-resolved render data (macro output, or `{}`).
    #[serde(default)]
    data: RenderData,
    /// Document `<title>` override (the resolved TS `title`). Defaults to the URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    /// Resolved head/SEO override (the resolved TS `head`). Conventional data
    /// keys (`description` / `canonical`) still fill in when this leaves them
    /// unset, exactly as [`treaty_ssg::seo::resolve_head`] does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    head: Option<HeadMeta>,
    /// Resolved sitemap enrichment (the resolved TS `sitemapEntry`).
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "sitemapEntry")]
    sitemap_entry: Option<SitemapEntryOverride>,
}

/// The JSON payload for [`generate_site_full`]: the site config plus the ordered,
/// fully-resolved page list. There is no routes/params/seam here — discovery and
/// macro execution already happened in the shim, so this is pure emit input.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SiteInputs {
    /// The deterministic site config (out dir, lang, origin, artifact toggles).
    #[serde(default)]
    config: SsgConfig,
    /// The pages to emit, in the order they should appear in the manifest.
    #[serde(default)]
    pages: Vec<PageInput>,
}

/// Generate a complete static site from a config + a fully-resolved page list,
/// returning the [`treaty_ssg::GeneratedSite`] as JSON.
///
/// This is the entry the `@treaty/ssg` shim drives. The shim has already (a)
/// discovered the routes (via [`discover_routes`]), (b) compiled each route's
/// component to Ivy and run its render-time macro through the Nova runtime to
/// resolve the render data, and (c) resolved the `title` / `head` /
/// `sitemapEntry` callbacks of the TS `SiteConfig` to plain values. This binding
/// then drives the pure `treaty_ssg` modules to do every byte of emit:
/// [`ivy_html`] interprets each template to an HTML fragment + detects its
/// hydration islands, [`seo`] resolves the head and wraps the fragment into a
/// full hydration-ready document, and [`manifest`] emits `sitemap.xml`,
/// `robots.txt`, and the hydration manifest (each gated by the config). No SSG
/// decision is made here — the binding only sequences the core's pure functions.
/// Deterministic: identical inputs yield byte-identical JSON.
#[napi]
pub fn generate_site_full(site_inputs_json: String) -> napi::Result<String> {
    let SiteInputs { config, pages } = serde_json::from_str(&site_inputs_json)
        .map_err(|err| napi::Error::from_reason(format!("invalid site inputs JSON: {err}")))?;

    let site = build_site(&config, &pages);

    serde_json::to_string(&site).map_err(|err| {
        napi::Error::from_reason(format!("failed to serialize generated site: {err}"))
    })
}

/// Map a concrete URL to its `index.html` output path under `out_dir`, matching
/// the convention `treaty_ssg::prerender_site` uses (`""` -> `<out>/index.html`,
/// `"/blog/hello"` -> `<out>/blog/hello/index.html`).
fn html_output(out_dir: &str, url: &str) -> String {
    let base = out_dir.trim_end_matches(['/', '\\']);
    let clean = url.trim_matches('/');
    if clean.is_empty() {
        format!("{base}/index.html")
    } else {
        format!("{base}/{clean}/index.html")
    }
}

/// Join `out_dir` with a forward-slash relative path (for the sitemap/robots/
/// manifest artifact paths), matching `treaty_ssg::prerender_site`'s `join`.
fn join(out_dir: &str, rel: &str) -> String {
    let base = out_dir.trim_end_matches(['/', '\\']);
    let rel = rel.trim_start_matches(['/', '\\']);
    format!("{base}/{rel}")
}

/// Assemble the whole site from the resolved pages by sequencing the pure
/// `treaty_ssg` modules. This is the Rust counterpart of the TS `prerenderSite`
/// loop, but every emit step (`render_ivy_to_html`, `detect_islands`,
/// `resolve_head`, `wrap_document`, `build_sitemap`, `build_robots`,
/// `hydration_manifest_json`) is a `treaty_ssg` function — the binding owns only
/// the sequencing, not the output.
fn build_site(config: &SsgConfig, pages: &[PageInput]) -> GeneratedSite {
    let out_dir = &config.out_dir;

    let mut out_pages: Vec<PrerenderedPage> = Vec::with_capacity(pages.len());
    let mut hydration_routes: Vec<RouteHydration> = Vec::with_capacity(pages.len());
    let mut sitemap_entries: Vec<SitemapEntry> = Vec::new();

    for page in pages {
        // Statically interpret the Ivy template against the resolved render data,
        // and detect the page's hydration islands — both in `treaty_ssg::ivy_html`.
        let fragment = ivy_html::render_ivy_to_html(&page.ivy_code, &page.data);
        let islands = ivy_html::detect_islands(&page.component_id, &page.ivy_code);

        // Resolve head/SEO (caller override > conventional data keys > defaults)
        // and wrap into a full hydration-ready document — both in `treaty_ssg::seo`.
        // The title defaults to the route URL, matching `prerender_site`.
        let title = page.title.clone().unwrap_or_else(|| page.url.clone());
        let head = seo::resolve_head(page.head.as_ref(), &page.data, &title, &config.lang);
        let document = seo::wrap_document(&fragment, &page.data, &head);

        let output = html_output(out_dir, &page.url);
        let bytes = document.len();

        out_pages.push(PrerenderedPage {
            url: page.url.clone(),
            output: output.clone(),
            route_path: page.route_path.clone(),
            parameterized: page.parameterized,
            document,
            bytes,
            data: page.data.clone(),
            islands: islands.clone(),
        });
        hydration_routes.push(manifest::route_hydration(
            page.url.clone(),
            output,
            islands,
            page.data.is_empty(),
        ));

        // Sitemap entry: emit one per page unless the shim's resolved
        // `sitemapEntry` excluded it; carry through any lastmod/changefreq/priority.
        if config.sitemap {
            match &page.sitemap_entry {
                Some(over) if over.exclude => {}
                Some(over) => sitemap_entries.push(SitemapEntry {
                    url: page.url.clone(),
                    lastmod: over.lastmod.clone(),
                    changefreq: over.changefreq,
                    priority: over.priority,
                }),
                None => sitemap_entries.push(SitemapEntry::new(page.url.clone())),
            }
        }
    }

    let hydration = manifest::build_hydration_manifest(hydration_routes);

    let mut artifacts: Vec<SiteArtifact> = Vec::new();

    if config.sitemap {
        let xml = manifest::build_sitemap(&config.origin, &sitemap_entries);
        let bytes = xml.len();
        artifacts.push(SiteArtifact {
            kind: ArtifactKind::Sitemap,
            output: join(out_dir, "sitemap.xml"),
            contents: xml,
            bytes,
        });
    }

    if config.robots {
        let sitemap_url = if config.sitemap && !config.origin.is_empty() {
            Some(manifest::absolute_url(&config.origin, "/sitemap.xml"))
        } else {
            None
        };
        let txt = manifest::build_robots(&manifest::RobotsOptions {
            sitemap_url,
            disallow: config.disallow.clone(),
        });
        let bytes = txt.len();
        artifacts.push(SiteArtifact {
            kind: ArtifactKind::Robots,
            output: join(out_dir, "robots.txt"),
            contents: txt,
            bytes,
        });
    }

    if config.hydration_manifest {
        let json = manifest::hydration_manifest_json(&hydration);
        let bytes = json.len();
        artifacts.push(SiteArtifact {
            kind: ArtifactKind::HydrationManifest,
            output: join(out_dir, manifest::HYDRATION_MANIFEST_FILE),
            contents: json,
            bytes,
        });
    }

    GeneratedSite {
        out_dir: out_dir.clone(),
        pages: out_pages,
        artifacts,
        hydration,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The binding is exercised end-to-end through its public `run` over a small
    /// fixture: a static index + a parameterized post, each with pre-resolved
    /// render data. It must return a parseable `GeneratedSite` with both pages'
    /// HTML and all three artifacts — and be deterministic.
    #[test]
    fn run_drives_the_core_and_returns_a_full_site() {
        // The core's structs carry snake_case serde field names (no camelCase
        // rename), so the JSON boundary uses `out_dir` / `has_component` /
        // `ivy_code` / `macro_input` verbatim — asserting the real field names
        // (a camelCase typo would silently `#[serde(default)]` and fail below).
        let config = json!({
            "out_dir": "dist/site",
            "origin": "https://example.com"
        });
        // A real Ivy `*_Template`: an `<h1>` interpolating `ctx.title`.
        let ivy = "function App_Template(rf, ctx) { \
            if (rf & 1) { i0.ɵɵelementStart(0, \"h1\"); i0.ɵɵtext(1); i0.ɵɵelementEnd(); } \
            if (rf & 2) { i0.ɵɵadvance(1); i0.ɵɵtextInterpolate(ctx.title); } }";
        let inputs = json!({
            "routes": [
                { "path": "", "has_component": true },
                { "path": "blog/:slug", "has_component": true }
            ],
            "params": { "blog/:slug": [ { "slug": "hello-world" } ] },
            "inputs": {
                "/": {
                    "ivy_code": ivy,
                    "component_id": "app.tsx",
                    "macro_input": { "title": "Home Page" }
                },
                "/blog/hello-world": {
                    "ivy_code": ivy,
                    "component_id": "post.tsx",
                    "macro_input": { "title": "Hello World Post" }
                }
            }
        });

        let out = run(&config.to_string(), &inputs.to_string()).expect("binding runs");
        let site: GeneratedSite = serde_json::from_str(&out).expect("output is a GeneratedSite");

        assert_eq!(site.out_dir, "dist/site");
        assert_eq!(site.pages.len(), 2);
        let index = site
            .pages
            .iter()
            .find(|p| p.url == "/")
            .expect("index page");
        assert!(index.document.contains("<h1>Home Page</h1>"));
        let post = site
            .pages
            .iter()
            .find(|p| p.url == "/blog/hello-world")
            .expect("post page");
        assert!(post.document.contains("<h1>Hello World Post</h1>"));

        // All three artifact kinds present.
        assert!(site
            .artifacts
            .iter()
            .any(|a| a.kind == ArtifactKind::Sitemap));
        assert!(site
            .artifacts
            .iter()
            .any(|a| a.kind == ArtifactKind::Robots));
        assert!(site
            .artifacts
            .iter()
            .any(|a| a.kind == ArtifactKind::HydrationManifest));

        // Determinism: a second run over the same JSON is byte-identical.
        let again = run(&config.to_string(), &inputs.to_string()).expect("second run");
        assert_eq!(out, again);
    }

    /// A discovered route with no entry in `inputs` is skipped, proving the
    /// provider's `None` (skip) seam crosses the JSON boundary.
    #[test]
    fn route_without_a_render_input_is_skipped() {
        let config = json!({ "out_dir": "dist/x" });
        let inputs = json!({
            "routes": [ { "path": "ghost", "has_component": true } ],
            "params": {},
            "inputs": {}
        });
        let out = run(&config.to_string(), &inputs.to_string()).expect("runs");
        let site: GeneratedSite = serde_json::from_str(&out).expect("parses");
        assert!(site.pages.is_empty());
    }

    /// Malformed config JSON surfaces as a thrown error, not a panic.
    #[test]
    fn invalid_config_is_an_error() {
        let err = run("not json", "{}").expect_err("must reject bad config");
        assert!(err.reason.contains("invalid SSG config JSON"));
    }

    /// `discover_routes` fans a parameterized route out over its static param
    /// sets and drops a parameterized route with no params, returning the
    /// `DiscoveredRoute` list verbatim from the core walk.
    #[test]
    fn discover_routes_fans_out_and_serializes() {
        let inputs = json!({
            "routes": [
                { "path": "", "has_component": true },
                { "path": "blog/:slug", "has_component": true },
                { "path": "ghost/:id", "has_component": true }
            ],
            "params": { "blog/:slug": [ { "slug": "a" }, { "slug": "b" } ] }
        });
        let out = discover_routes(inputs.to_string()).expect("discovers");
        let routes: Vec<DiscoveredRoute> = serde_json::from_str(&out).expect("parses");
        let urls: Vec<&str> = routes.iter().map(|r| r.url.as_str()).collect();
        // Index + two fanned-out blog posts; `ghost/:id` (no params) is dropped.
        assert_eq!(urls, vec!["/", "/blog/a", "/blog/b"]);
        assert!(routes.iter().find(|r| r.url == "/blog/a").unwrap().parameterized);
    }

    /// `discover_routes` honours `includeComponentless`: a layout shell with only
    /// children is emitted when opted in, and skipped (but recursed) by default.
    #[test]
    fn discover_routes_honours_include_componentless() {
        let routes = json!([
            { "path": "shell", "children": [ { "path": "inner", "has_component": true } ] }
        ]);
        let default_out =
            discover_routes(json!({ "routes": routes }).to_string()).expect("default");
        let default_routes: Vec<DiscoveredRoute> =
            serde_json::from_str(&default_out).expect("parses");
        assert_eq!(
            default_routes.iter().map(|r| r.url.as_str()).collect::<Vec<_>>(),
            vec!["/shell/inner"]
        );

        let opted_out =
            discover_routes(json!({ "routes": routes, "includeComponentless": true }).to_string())
                .expect("opted in");
        let opted_routes: Vec<DiscoveredRoute> = serde_json::from_str(&opted_out).expect("parses");
        assert_eq!(
            opted_routes.iter().map(|r| r.url.as_str()).collect::<Vec<_>>(),
            vec!["/shell", "/shell/inner"]
        );
    }

    /// The crawler-artifact entries pass JSON straight through to the core's
    /// pure builders: the sitemap carries an absolute, enriched entry; robots
    /// honours the disallow list + advertises the sitemap; `absoluteUrl` joins.
    #[test]
    fn crawler_artifact_entries_pass_through_to_the_core() {
        let sitemap = build_sitemap(
            json!({
                "origin": "https://x.test",
                "entries": [
                    { "url": "/" , "priority": 1.0 },
                    { "url": "/about", "changefreq": "monthly" }
                ]
            })
            .to_string(),
        )
        .expect("sitemap");
        assert!(sitemap.contains("<loc>https://x.test/</loc>"));
        assert!(sitemap.contains("<loc>https://x.test/about</loc>"));
        assert!(sitemap.contains("<priority>1.0</priority>"));
        assert!(sitemap.contains("<changefreq>monthly</changefreq>"));

        let robots = build_robots(
            json!({ "sitemapUrl": "https://x.test/sitemap.xml", "disallow": ["/admin"] })
                .to_string(),
        )
        .expect("robots");
        assert!(robots.contains("User-agent: *"));
        assert!(robots.contains("Disallow: /admin"));
        assert!(robots.contains("Sitemap: https://x.test/sitemap.xml"));

        assert_eq!(
            absolute_url("https://x.test/".to_string(), "/about".to_string()),
            "https://x.test/about"
        );
    }

    /// `render_ivy_to_html` interprets an emitted Ivy interpolation template
    /// against render data into the bound HTML, and rejects non-object data JSON.
    #[test]
    fn render_ivy_to_html_binds_interpolation_and_rejects_bad_data() {
        let ivy = "function H_Template(rf, ctx) { \
            if (rf & 1) { i0.ɵɵelementStart(0, \"h1\"); i0.ɵɵtext(1); i0.ɵɵelementEnd(); } \
            if (rf & 2) { i0.ɵɵadvance(1); i0.ɵɵtextInterpolate(ctx.title); } }";
        let html =
            render_ivy_to_html(ivy.to_string(), json!({ "title": "Hello from SSG" }).to_string())
                .expect("renders");
        assert_eq!(html, "<h1>Hello from SSG</h1>");

        // A pass-through module (no template fn) renders nothing.
        let empty = render_ivy_to_html("export const x = 1;".to_string(), "{}".to_string())
            .expect("renders empty");
        assert_eq!(empty, "");

        // Non-object data JSON is a thrown error, not a panic.
        let err = render_ivy_to_html(ivy.to_string(), "42".to_string())
            .expect_err("rejects non-object data");
        assert!(err.reason.contains("invalid render data JSON"));
    }

    /// `generate_site_full` drives the pure modules over a resolved page list:
    /// the document carries the data-bound interpolation, the per-route `head`
    /// override lands in the head, the `sitemapEntry` override enriches the
    /// sitemap, and a route excluded from the sitemap still gets a page + manifest
    /// entry. Determinism holds.
    #[test]
    fn generate_site_full_emits_documents_and_artifacts() {
        let ivy = "function P_Template(rf, ctx) { \
            if (rf & 1) { i0.ɵɵelementStart(0, \"h1\"); i0.ɵɵtext(1); i0.ɵɵelementEnd(); } \
            if (rf & 2) { i0.ɵɵadvance(1); i0.ɵɵtextInterpolate(ctx.title); } }";
        let inputs = json!({
            "config": { "out_dir": "dist/site", "origin": "https://t.dev", "disallow": ["/draft"] },
            "pages": [
                {
                    "url": "/",
                    "routePath": "",
                    "parameterized": false,
                    "ivyCode": ivy,
                    "componentId": "home.tsx",
                    "data": { "title": "Home" },
                    "head": { "description": "the home page" },
                    "sitemapEntry": { "priority": 1.0, "changefreq": "daily" }
                },
                {
                    "url": "/secret",
                    "routePath": "secret",
                    "parameterized": false,
                    "ivyCode": ivy,
                    "componentId": "secret.tsx",
                    "data": { "title": "Secret" },
                    "sitemapEntry": { "exclude": true }
                }
            ]
        });

        let out = generate_site_full(inputs.to_string()).expect("generates");
        let site: GeneratedSite = serde_json::from_str(&out).expect("parses");

        assert_eq!(site.out_dir, "dist/site");
        assert_eq!(site.pages.len(), 2, "both routes prerendered (sitemap exclusion != page exclusion)");
        let home = site.pages.iter().find(|p| p.url == "/").expect("home page");
        assert!(home.document.contains("<h1>Home</h1>"), "data-bound interpolation");
        assert!(
            home.document.contains("<meta name=\"description\" content=\"the home page\">"),
            "per-route head override in the document head"
        );
        assert_eq!(home.output, "dist/site/index.html");

        let secret = site.pages.iter().find(|p| p.url == "/secret").expect("secret page");
        assert!(secret.document.contains("<h1>Secret</h1>"));
        assert_eq!(secret.output, "dist/site/secret/index.html");

        // Sitemap: home is enriched, the excluded route is absent, robots honours
        // the disallow list, and the hydration manifest carries both routes.
        let sitemap = site
            .artifacts
            .iter()
            .find(|a| a.kind == ArtifactKind::Sitemap)
            .expect("sitemap");
        assert!(sitemap.contents.contains("<loc>https://t.dev/</loc>"));
        assert!(sitemap.contents.contains("<priority>1.0</priority>"));
        assert!(sitemap.contents.contains("<changefreq>daily</changefreq>"));
        assert!(!sitemap.contents.contains("/secret"), "excluded from sitemap");

        let robots = site
            .artifacts
            .iter()
            .find(|a| a.kind == ArtifactKind::Robots)
            .expect("robots");
        assert!(robots.contents.contains("Disallow: /draft"));
        assert!(robots.contents.contains("Sitemap: https://t.dev/sitemap.xml"));

        assert_eq!(site.hydration.version, 1);
        assert_eq!(site.hydration.routes.len(), 2);

        let again = generate_site_full(inputs.to_string()).expect("second run");
        assert_eq!(out, again, "generate_site_full is deterministic");
    }
}
