//! `treaty_ssg` — the pure, deterministic Rust core of `@treaty/ssg`.
//!
//! Treaty is a compiler, not a host. This crate ports the deterministic core of
//! the TS `@treaty/ssg` package — route discovery, an Ivy -> static-HTML
//! interpreter, head/SEO emit, the per-route hydration manifest, and
//! `sitemap.xml` + `robots.txt` — to Rust, so the TS package can become a thin
//! shim over it. See [[rust-core-ts-shim-layering]].
//!
//! It is kept dependency-light and pure: the Nova prerender EXECUTION (running
//! a route's render-time macro to obtain its render data) is the injected
//! [`RenderSeam`] trait, faked in the unit tests. All outputs are
//! serde-serializable.
//!
//! The pipeline composes four modules:
//!   1. [`discovery`] enumerates the concrete prerenderable routes,
//!   2. [`ivy_html`] statically interprets each route's emitted Ivy template
//!      against its render data into an HTML fragment,
//!   3. [`seo`] resolves head/SEO metadata and wraps the fragment in a full,
//!      hydration-ready document,
//!   4. [`manifest`] emits the hydration manifest, `sitemap.xml`, `robots.txt`.
//!
//! [`prerender_site`] is the one-call entry point wiring those together; the
//! [`RenderSeam`] and a route-input provider (`ivy_provider`) are injected.

pub mod discovery;
pub mod ivy_html;
pub mod manifest;
pub mod seo;
pub mod types;

pub use types::{
    ArtifactKind, ChangeFreq, DiscoveredRoute, GeneratedSite, HeadMeta, HydrationIsland,
    HydrationManifest, IslandKind, PrerenderedPage, RenderData, RenderSeam, RouteHydration,
    RouteParams, RouteRenderInput, RouteSpec, SiteArtifact, SitemapEntry, SsgConfig,
};

pub use discovery::RouteParamsMap;

/// Map a discovered route to the component source + macro to prerender, or
/// `None` to skip it (e.g. a layout shell with no own component). This is the
/// only app-specific seam in the pipeline (the Rust counterpart of the TS
/// `ResolveRouteInput`); the `ivy_provider` argument to [`prerender_site`]
/// implements it.
pub trait IvyProvider {
    /// Resolve the render input for one discovered route, or `None` to skip.
    fn resolve(&self, route: &DiscoveredRoute) -> Option<RouteRenderInput>;
}

impl<F> IvyProvider for F
where
    F: Fn(&DiscoveredRoute) -> Option<RouteRenderInput>,
{
    fn resolve(&self, route: &DiscoveredRoute) -> Option<RouteRenderInput> {
        (self)(route)
    }
}

/// Map a concrete URL to its `index.html` output path under `out_dir`. Mirrors
/// the TS `htmlOutput`: strip surrounding slashes, then `""` -> `<out>/index.html`
/// and `"/blog/hello"` -> `<out>/blog/hello/index.html`.
fn html_output(out_dir: &str, url: &str) -> String {
    let base = out_dir.trim_end_matches(['/', '\\']);
    let clean = url.trim_matches('/');
    if clean.is_empty() {
        format!("{base}/index.html")
    } else {
        format!("{base}/{clean}/index.html")
    }
}

/// Join `out_dir` with a forward-slash relative path. Mirrors the TS `join`.
fn join(out_dir: &str, rel: &str) -> String {
    let base = out_dir.trim_end_matches(['/', '\\']);
    let rel = rel.trim_start_matches(['/', '\\']);
    format!("{base}/{rel}")
}

/// Generate a complete static site and return the emitted artifact set.
///
/// Discovers every prerenderable route from `routes` (static + parameterized
/// via `params`), resolves each route's component source through `ivy_provider`,
/// executes its render-time macro through the injected `seam`, statically
/// interprets the Ivy template, wraps the result in a hydration-ready document,
/// and assembles the hydration manifest plus `sitemap.xml` / `robots.txt`
/// (gated by `config`). All output is returned in-memory in [`GeneratedSite`];
/// the TS shim owns the actual disk writes.
///
/// Ports the TS `prerenderSite` (`site.ts`) over the pure module APIs: the Nova
/// macro execution is the injected `seam`, the per-route component source is the
/// injected `ivy_provider`. Routes the provider maps to `None` are skipped.
pub fn prerender_site(
    config: &SsgConfig,
    routes: &[RouteSpec],
    params: &RouteParamsMap,
    seam: &dyn RenderSeam,
    ivy_provider: &dyn IvyProvider,
) -> GeneratedSite {
    let out_dir = &config.out_dir;
    let discovered = discovery::discover_routes(routes, params);

    let mut pages: Vec<PrerenderedPage> = Vec::new();
    let mut hydration_routes: Vec<RouteHydration> = Vec::new();
    let mut sitemap_entries: Vec<SitemapEntry> = Vec::new();

    for route in &discovered {
        let input = match ivy_provider.resolve(route) {
            Some(input) => input,
            None => continue,
        };

        // Resolve the render data: a route with a render-time macro runs it
        // through the seam; otherwise the supplied macro input is the data.
        let data: RenderData = match &input.macro_source {
            Some(macro_src) => seam.render(macro_src, &input.macro_input),
            None => input.macro_input.clone(),
        };

        // Statically interpret the Ivy template against the render data.
        let fragment = ivy_html::render_ivy_to_html(&input.ivy_code, &data);
        let islands = ivy_html::detect_islands(&input.component_id, &input.ivy_code);

        // Resolve the head/SEO metadata and wrap the fragment in a full,
        // hydration-ready document. Title defaults to the route URL.
        let title = route.url.clone();
        let head = seo::resolve_head(None, &data, &title, &config.lang);
        let document = seo::wrap_document(&fragment, &data, &head);

        let output = html_output(out_dir, &route.url);
        let bytes = document.len();

        pages.push(PrerenderedPage {
            url: route.url.clone(),
            output: output.clone(),
            route_path: route.route_path.clone(),
            parameterized: route.parameterized,
            document,
            bytes,
            data: data.clone(),
            islands: islands.clone(),
        });
        hydration_routes.push(manifest::route_hydration(
            route.url.clone(),
            output,
            islands,
            data.is_empty(),
        ));

        if config.sitemap {
            sitemap_entries.push(SitemapEntry::new(route.url.clone()));
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
        // Advertise the sitemap only when one is emitted and the origin is set
        // (an empty origin cannot produce an absolute `Sitemap:` URL).
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

    GeneratedSite { out_dir: out_dir.clone(), pages, artifacts, hydration }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use serde_json::json;

    /// A deterministic fake render seam standing in for the Nova `run_macro`
    /// boundary: it returns the injected `input` verbatim as render data, so
    /// tests exercise the pipeline without any heavy runtime.
    struct EchoSeam;

    impl RenderSeam for EchoSeam {
        fn render(&self, _macro_src: &str, input: &RenderData) -> RenderData {
            input.clone()
        }
    }

    #[test]
    fn seam_is_injectable_and_deterministic() {
        let seam = EchoSeam;
        let mut input: RenderData = BTreeMap::new();
        input.insert("title".to_string(), json!("Hello"));
        let out = seam.render("export default {}", &input);
        assert_eq!(out, input);
    }

    #[test]
    fn prerender_site_wires_config_out_dir() {
        let config = SsgConfig { out_dir: "out/site".to_string(), ..SsgConfig::default() };
        let params: RouteParamsMap = BTreeMap::new();
        let provider = |_route: &DiscoveredRoute| -> Option<RouteRenderInput> { None };
        let site = prerender_site(&config, &[], &params, &EchoSeam, &provider);
        assert_eq!(site.out_dir, "out/site");
        assert!(site.pages.is_empty());
        assert_eq!(site.hydration.version, 1);
        assert!(site.hydration.routes.is_empty());
        // With no pages the gated artifacts are still emitted (empty sitemap,
        // allow-all robots, empty hydration manifest), keyed under out_dir.
        assert!(site
            .artifacts
            .iter()
            .any(|a| a.kind == ArtifactKind::Sitemap && a.output == "out/site/sitemap.xml"));
    }

    /// An eager-component route at `path` whose discovered URL the e2e provider
    /// keys off to return its Ivy template.
    fn component(path: &str) -> RouteSpec {
        RouteSpec { path: path.to_string(), has_component: true, ..RouteSpec::default() }
    }

    #[test]
    fn prerender_site_end_to_end_wires_all_modules() {
        // Two routes: a static index and a parameterized `blog/:slug` (its slug
        // universe supplied via the static params map).
        let routes = vec![component(""), component("blog/:slug")];
        let mut params: RouteParamsMap = BTreeMap::new();
        let mut slug: RouteParams = BTreeMap::new();
        slug.insert("slug".to_string(), "hello-world".to_string());
        params.insert("blog/:slug".to_string(), vec![slug]);

        // A real Ivy `*_Template`: an `<h1>` whose text interpolates `ctx.title`.
        let ivy = r#"
            function App_Template(rf, ctx) {
                if (rf & 1) {
                    i0.ɵɵelementStart(0, "h1");
                    i0.ɵɵtext(1);
                    i0.ɵɵelementEnd();
                }
                if (rf & 2) {
                    i0.ɵɵadvance(1);
                    i0.ɵɵtextInterpolate(ctx.title);
                }
            }
        "#;

        // Provider: every route renders the same component; the title comes from
        // the macro input, run through the (echo) seam as render data.
        let provider = |route: &DiscoveredRoute| -> Option<RouteRenderInput> {
            let mut macro_input: RenderData = BTreeMap::new();
            let title = if route.parameterized { "Hello World Post" } else { "Home Page" };
            macro_input.insert("title".to_string(), json!(title));
            Some(RouteRenderInput {
                ivy_code: ivy.to_string(),
                component_id: "app.tsx".to_string(),
                macro_source: Some("export default ({ title }) => ({ title })".to_string()),
                macro_input,
            })
        };

        let config = SsgConfig {
            out_dir: "dist/site".to_string(),
            origin: "https://example.com".to_string(),
            ..SsgConfig::default()
        };
        let site = prerender_site(&config, &routes, &params, &EchoSeam, &provider);

        // Two pages discovered: the static index and the parameterized post.
        assert_eq!(site.pages.len(), 2);
        let index = site.pages.iter().find(|p| p.url == "/").expect("index page");
        let post = site
            .pages
            .iter()
            .find(|p| p.url == "/blog/hello-world")
            .expect("parameterized post page");

        // Output-path convention: "/" -> index.html, "/blog/hello-world" -> nested.
        assert_eq!(index.output, "dist/site/index.html");
        assert_eq!(post.output, "dist/site/blog/hello-world/index.html");
        assert!(post.parameterized && !index.parameterized);

        // The interpolated title (from the seam-resolved render data) is in the
        // rendered fragment, and the head/title wraps it into a full document.
        assert!(index.document.contains("<h1>Home Page</h1>"));
        assert!(post.document.contains("<h1>Hello World Post</h1>"));
        assert!(index.document.starts_with("<!doctype html>\n<html lang=\"en\">"));
        assert!(index.document.contains("<title>/</title>"));
        assert_eq!(index.bytes, index.document.len());

        // Islands: the component root + one interpolation island per page.
        assert_eq!(index.islands[0].kind, IslandKind::Component);
        assert!(index.islands.iter().any(|i| i.kind == IslandKind::Interpolation));

        // Hydration manifest carries both routes (with embedded state since the
        // render data is non-empty).
        assert_eq!(site.hydration.version, 1);
        assert_eq!(site.hydration.routes.len(), 2);
        assert!(site.hydration.routes.iter().any(|r| r.url == "/blog/hello-world" && r.has_state));

        // Sitemap, robots, and hydration-manifest artifacts are all present.
        let sitemap = site
            .artifacts
            .iter()
            .find(|a| a.kind == ArtifactKind::Sitemap)
            .expect("sitemap artifact");
        assert_eq!(sitemap.output, "dist/site/sitemap.xml");
        assert!(sitemap.contents.contains("<loc>https://example.com/blog/hello-world</loc>"));
        assert_eq!(sitemap.bytes, sitemap.contents.len());

        let robots = site
            .artifacts
            .iter()
            .find(|a| a.kind == ArtifactKind::Robots)
            .expect("robots artifact");
        assert_eq!(robots.output, "dist/site/robots.txt");
        assert!(robots.contents.contains("Sitemap: https://example.com/sitemap.xml"));

        let manifest_artifact = site
            .artifacts
            .iter()
            .find(|a| a.kind == ArtifactKind::HydrationManifest)
            .expect("hydration-manifest artifact");
        assert_eq!(manifest_artifact.output, "dist/site/treaty-hydration.json");
        assert!(manifest_artifact.contents.contains("\"version\": 1"));
        assert!(manifest_artifact.contents.contains("/blog/hello-world"));
    }

    #[test]
    fn config_default_matches_ts_defaults() {
        let config = SsgConfig::default();
        assert_eq!(config.out_dir, "dist/ssg");
        assert_eq!(config.lang, "en");
        assert!(config.sitemap && config.robots && config.hydration_manifest);
    }

    #[test]
    fn generated_site_round_trips_through_serde() {
        let site = GeneratedSite { out_dir: "dist/ssg".to_string(), ..GeneratedSite::default() };
        let text = serde_json::to_string(&site).expect("serialize");
        let back: GeneratedSite = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(site, back);
    }
}
