//! `treaty_ssg_node` — the NAPI binding over the pure Rust SSG core.
//!
//! Treaty is a compiler, not a host. The deterministic Static-Site-Generation
//! logic — route discovery + parameterized fan-out, the Ivy -> static-HTML
//! interpreter, head/SEO emit, `sitemap.xml` / `robots.txt`, and the per-route
//! hydration manifest — lives in the `treaty_ssg` crate (`libs/ssg-core`). This
//! binding is *pure glue*: it takes the site config and the per-route render
//! inputs as JSON, drives [`treaty_ssg::prerender_site`], and returns the whole
//! [`treaty_ssg::GeneratedSite`] (pages' HTML + the sitemap/robots/manifest
//! artifacts) back as JSON. No SSG decision is made here — every byte of output
//! is produced by the Rust core, so `@treaty/ssg` can become a thin shim that
//! only does the disk writes. See [[rust-core-ts-shim-layering]].
//!
//! ## The seam
//!
//! The one runtime boundary the core abstracts is [`treaty_ssg::RenderSeam`] —
//! the Nova `run_macro` execution that turns a route's render-time macro into
//! its render data. That execution is NOT part of this deterministic binding:
//! the caller resolves each route's render data ahead of time (running its macro
//! through the Nova runtime in the TS/runtime layer) and passes the finished
//! [`treaty_ssg::RouteRenderInput`] per route with `macro_source` omitted, so the
//! core takes its pass-through branch (`macro_input` IS the render data). The
//! binding installs a pass-through seam to honour that contract while keeping the
//! `prerender_site` signature intact.

#[macro_use]
extern crate napi_derive;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use treaty_ssg::discovery::RouteParamsMap;
use treaty_ssg::{
    prerender_site as core_prerender_site, DiscoveredRoute, GeneratedSite, RenderData, RenderSeam,
    RouteRenderInput, RouteSpec, SsgConfig,
};

/// The JSON payload describing what to prerender: the routes config to discover,
/// the static parameter sets that fan parameterized routes out, and the
/// already-resolved render input per concrete route URL.
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
/// Shared by both `#[napi]` entry points. Parsing errors surface as `napi::Error`
/// (a thrown JS error) so the shim sees a precise message rather than a panic.
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
        use treaty_ssg::ArtifactKind;
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
}
