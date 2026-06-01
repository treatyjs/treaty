//! Adversarial, end-to-end integration test for `treaty_ssg`.
//!
//! Unlike the per-module unit tests (which reach private helpers), this test
//! compiles as an external crate and drives ONLY the public API surface
//! [`treaty_ssg::prerender_site`] / [`treaty_ssg::discovery`] against a realistic
//! fixture site — exactly as the future TS shim will. It is the Phase-3 contract:
//!
//!   1. route discovery + a `getStaticPaths`-style macro-backed provider fan a
//!      parameterized `blog/:slug` route out over a computed slug universe, on top
//!      of the static index and `about` routes;
//!   2. the prerender renders a real Ivy `*_Template` instruction stream to the
//!      EXACT expected HTML (full document asserted byte-for-byte, not a substring
//!      smoke check);
//!   3. `sitemap.xml`, `robots.txt`, and the hydration manifest are emitted with
//!      the exact expected bytes;
//!   4. the WHOLE pipeline is deterministic — two independent runs (each with a
//!      freshly constructed seam/provider) produce byte-identical artifacts and
//!      documents, the reproducibility guarantee SSG depends on.
//!
//! The Nova macro boundary is the injected [`RenderSeam`]; the fixture supplies a
//! deterministic fake so the test carries no runtime.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use treaty_ssg::discovery::{
    discover_routes_with, DiscoverOptions, MacroStaticPaths, RouteParamsMap, StaticPathsRequest,
};
use treaty_ssg::{
    prerender_site, ArtifactKind, DiscoveredRoute, RenderData, RenderSeam, RouteRenderInput,
    RouteSpec, SitemapEntry, SsgConfig,
};

/// A deterministic fake render seam standing in for the Nova `run_macro`
/// boundary. It is a closed-over function table keyed by macro source so the
/// fixture can model two distinct macros — the `getStaticPaths` macro (returns a
/// `paths` array) and each route's render-data macro (returns the page's data) —
/// without any JS runtime. Deterministic for a given `(macro_src, input)`.
struct FixtureSeam;

impl RenderSeam for FixtureSeam {
    fn render(&self, macro_src: &str, input: &RenderData) -> RenderData {
        match macro_src {
            // The blog index's getStaticPaths macro: the slug universe a real
            // macro would compute from a content dir / CMS. Emitted in a stable
            // order so discovery output is reproducible.
            GET_STATIC_PATHS_SRC => {
                let mut data: RenderData = BTreeMap::new();
                data.insert(
                    "paths".to_string(),
                    json!([{ "slug": "hello-world" }, { "slug": "ssg in rust" }]),
                );
                data
            }
            // Each route's render-data macro echoes its injected input as the
            // render data (the title/description the page binds against). The
            // pipeline injects the macro_input; echoing it keeps the fixture's
            // expected HTML a pure function of that input.
            _ => input.clone(),
        }
    }
}

/// The blog index's `getStaticPaths` macro source — the key the [`FixtureSeam`]
/// dispatches on. (Body text is illustrative; the seam keys off identity.)
const GET_STATIC_PATHS_SRC: &str =
    "export default () => ({ paths: listPosts().map((p) => ({ slug: p.slug })) })";

/// Each page's render-data macro source. The seam echoes the injected input.
const RENDER_DATA_SRC: &str = "export default ({ title, description }) => ({ title, description })";

/// A real Ivy `*_Template`: an `<article>` wrapping an `<h1>` (interpolating
/// `ctx.title`) and a `<p>` (interpolating `ctx.description`). This is the exact
/// instruction-stream shape Ivy lowers such a template to — create block under
/// `rf & 1`, update block under `rf & 2`, slot-indexed text nodes, `ɵɵadvance`
/// cursor moves between interpolations.
const PAGE_IVY: &str = r#"
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
"#;

/// Build the fixture route set: a static index (`""`), a static `about`, and a
/// parameterized `blog/:slug` whose slug universe the `getStaticPaths` macro
/// computes. Mirrors a real app's `Routes` array reduced to the structural
/// subset the discovery walk branches on.
fn fixture_routes() -> Vec<RouteSpec> {
    let component = |path: &str| RouteSpec {
        path: path.to_string(),
        has_component: true,
        ..RouteSpec::default()
    };
    vec![component(""), component("about"), component("blog/:slug")]
}

/// The whole-site config the fixture prerenders under: a fixed origin and a
/// `/draft` disallow so the emitted robots/sitemap bytes are fully pinned.
fn fixture_config() -> SsgConfig {
    SsgConfig {
        out_dir: "dist/site".to_string(),
        origin: "https://treaty.dev".to_string(),
        lang: "en".to_string(),
        disallow: vec!["/draft".to_string()],
        sitemap: true,
        robots: true,
        hydration_manifest: true,
    }
}

/// Per-route render input provider: every route renders the same `<article>`
/// component; the title/description are keyed off the discovered URL so each page
/// has distinct, asserted content. The blog index route (`/blog`) itself has no
/// own page (its children are the posts) and is mapped to `None` (skipped),
/// proving the provider's skip seam.
fn fixture_provider(route: &DiscoveredRoute) -> Option<RouteRenderInput> {
    let (title, description) = match route.url.as_str() {
        "/" => ("Treaty SSG", "The pure Rust SSG core."),
        "/about" => ("About", "Who builds Treaty."),
        "/blog/hello-world" => ("Hello, World", "The first post."),
        // The space-bearing slug is percent-encoded in the URL but the human
        // title is spelled out; proves the URL<->data decoupling.
        "/blog/ssg%20in%20rust" => ("SSG in Rust", "Prerendering, the Rust way."),
        _ => return None,
    };
    let mut macro_input: RenderData = BTreeMap::new();
    macro_input.insert("title".to_string(), Value::String(title.to_string()));
    macro_input.insert("description".to_string(), Value::String(description.to_string()));
    Some(RouteRenderInput {
        ivy_code: PAGE_IVY.to_string(),
        component_id: "page.tsx".to_string(),
        macro_source: Some(RENDER_DATA_SRC.to_string()),
        macro_input,
    })
}

/// Drive the full discovery + prerender pipeline once, returning the generated
/// site. Each call constructs a fresh seam/provider so two calls are genuinely
/// independent runs (the determinism check below relies on that).
fn run_pipeline() -> treaty_ssg::GeneratedSite {
    let config = fixture_config();
    let routes = fixture_routes();
    let seam = FixtureSeam;

    // getStaticPaths: resolve the blog route's slug universe through the macro
    // boundary, exactly as `discoverRoutesAsync` would. Only `blog/:slug` has a
    // macro; everything else returns `None` (no computed params).
    let get_static_paths = MacroStaticPaths::new(&seam, |request: &StaticPathsRequest<'_>| {
        if request.route_path == "blog/:slug" {
            assert_eq!(request.params, &["slug".to_string()]);
            Some(GET_STATIC_PATHS_SRC.to_string())
        } else {
            None
        }
    });

    // Discover the concrete routes (static + macro-fanned params). Assert the
    // discovery contract here so a discovery regression fails loudly.
    let discovered = discover_routes_with(
        &routes,
        &RouteParamsMap::new(),
        &DiscoverOptions::default(),
        Some(&get_static_paths),
    );
    let urls: Vec<&str> = discovered.iter().map(|r| r.url.as_str()).collect();
    assert_eq!(
        urls,
        vec!["/", "/about", "/blog/hello-world", "/blog/ssg%20in%20rust"],
        "discovery must fan the parameterized route out over the macro's slug universe \
         (space-bearing slug percent-encoded) on top of the two static routes"
    );

    // The same static params map the discovery used must drive the prerender, so
    // the pipeline materializes exactly the discovered routes.
    let mut params: RouteParamsMap = BTreeMap::new();
    let mut sets = Vec::new();
    for route in &discovered {
        if route.parameterized {
            sets.push(route.params.clone());
        }
    }
    params.insert("blog/:slug".to_string(), sets);

    prerender_site(&config, &routes, &params, &seam, &fixture_provider)
}

#[test]
fn fixture_prerenders_to_expected_html_artifacts_and_is_deterministic() {
    let site = run_pipeline();

    // ---- pages: four discovered (blog index skipped by the provider) -----------
    assert_eq!(site.out_dir, "dist/site");
    assert_eq!(site.pages.len(), 4, "index + about + two posts; /blog index skipped");

    let page = |url: &str| {
        site.pages
            .iter()
            .find(|p| p.url == url)
            .unwrap_or_else(|| panic!("missing prerendered page {url}"))
    };

    // ---- SSR render: the index page document, asserted byte-for-byte -----------
    // This pins the entire interpreter contract: the create-block element tree
    // (with the static `class="post"` attr), both `ctx.*` interpolations resolved
    // against the seam-produced render data, the head/SEO emit (title from the
    // route URL fallback is overridden here by... no — title defaults to the route
    // URL; description comes from the render data), and the hydration wrapper
    // (marker attr + embedded JSON state).
    let index = page("/");
    assert_eq!(index.output, "dist/site/index.html");
    let expected_index = concat!(
        "<!doctype html>\n",
        "<html lang=\"en\">\n",
        "<head>\n",
        "<meta charset=\"utf-8\">\n",
        "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n",
        "<title>/</title>\n",
        "<meta name=\"description\" content=\"The pure Rust SSG core.\">\n",
        "</head>\n",
        "<body>\n",
        "<app-root data-treaty-ssg=\"1\">",
        "<article class=\"post\"><h1>Treaty SSG</h1><p>The pure Rust SSG core.</p></article>",
        "</app-root>\n",
        // Render state embeds inside a `<script type="application/json">` body,
        // where only `<`/`>`/`&` are escaped (HTML-escape) — double quotes are
        // safe in that context and stay raw. `RenderData` is a BTreeMap, so the
        // JSON keys are in sorted order (`description` before `title`).
        "<script type=\"application/json\" id=\"__TREATY_SSG_STATE__\">",
        "{\"description\":\"The pure Rust SSG core.\",",
        "\"title\":\"Treaty SSG\"}",
        "</script>\n",
        "</body>\n",
        "</html>\n",
    );
    assert_eq!(index.document, expected_index, "index document must match byte-for-byte");
    assert_eq!(index.bytes, index.document.len());

    // ---- SSR render: the parameterized post, nested output path + content ------
    let post = page("/blog/ssg%20in%20rust");
    assert_eq!(post.output, "dist/site/blog/ssg%20in%20rust/index.html");
    assert!(post.parameterized);
    assert!(
        post.document.contains(
            "<article class=\"post\"><h1>SSG in Rust</h1><p>Prerendering, the Rust way.</p></article>"
        ),
        "the post's render data must fill both interpolation slots: {}",
        post.document
    );
    // Its `<title>` defaults to the concrete (encoded) URL.
    assert!(post.document.contains("<title>/blog/ssg%20in%20rust</title>"));

    // Islands: a component island + one interpolation island per `ɵɵtextInterpolate`.
    assert_eq!(index.islands.len(), 3, "1 component + 2 interpolations");
    assert_eq!(index.islands[0].kind, treaty_ssg::IslandKind::Component);
    assert!(index
        .islands
        .iter()
        .filter(|i| i.kind == treaty_ssg::IslandKind::Interpolation)
        .count()
        == 2);

    // ---- sitemap.xml: exact bytes ----------------------------------------------
    let sitemap = artifact(&site, ArtifactKind::Sitemap);
    assert_eq!(sitemap.output, "dist/site/sitemap.xml");
    let expected_sitemap = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        "<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n",
        "  <url>\n    <loc>https://treaty.dev/</loc>\n  </url>\n",
        "  <url>\n    <loc>https://treaty.dev/about</loc>\n  </url>\n",
        "  <url>\n    <loc>https://treaty.dev/blog/hello-world</loc>\n  </url>\n",
        "  <url>\n    <loc>https://treaty.dev/blog/ssg%20in%20rust</loc>\n  </url>\n",
        "</urlset>\n",
    );
    assert_eq!(sitemap.contents, expected_sitemap, "sitemap.xml must match byte-for-byte");
    assert_eq!(sitemap.bytes, sitemap.contents.len());

    // ---- robots.txt: exact bytes -----------------------------------------------
    let robots = artifact(&site, ArtifactKind::Robots);
    assert_eq!(robots.output, "dist/site/robots.txt");
    let expected_robots = concat!(
        "User-agent: *\n",
        "Disallow: /draft\n",
        "\n",
        "Sitemap: https://treaty.dev/sitemap.xml\n",
    );
    assert_eq!(robots.contents, expected_robots, "robots.txt must match byte-for-byte");

    // ---- hydration manifest: shape + the two posts carry embedded state --------
    assert_eq!(site.hydration.version, 1);
    assert_eq!(site.hydration.routes.len(), 4);
    assert!(
        site.hydration.routes.iter().all(|r| r.has_state),
        "every page's render data is non-empty, so all routes embed hydration state"
    );
    let manifest = artifact(&site, ArtifactKind::HydrationManifest);
    assert_eq!(manifest.output, "dist/site/treaty-hydration.json");
    // The manifest is valid, pretty-printed JSON that round-trips back to the
    // in-memory manifest (the on-disk bytes are exactly what the shim writes).
    let parsed: treaty_ssg::HydrationManifest =
        serde_json::from_str(manifest.contents.trim_end()).expect("hydration manifest is valid JSON");
    assert_eq!(parsed, site.hydration);
    assert!(manifest.contents.ends_with("}\n"));
    assert!(manifest.contents.contains("\"url\": \"/blog/ssg%20in%20rust\""));

    // ---- DETERMINISM: a second independent run is byte-identical ---------------
    let again = run_pipeline();
    assert_eq!(
        site, again,
        "the whole pipeline (discovery + render + artifacts + manifest) must be byte-for-byte \
         reproducible across independent runs"
    );
    // Belt-and-braces: the serialized site is byte-identical too (catches any
    // non-determinism a structural `==` might mask, e.g. map iteration order).
    let first_json = serde_json::to_string(&site).expect("serialize first run");
    let second_json = serde_json::to_string(&again).expect("serialize second run");
    assert_eq!(first_json, second_json, "serialized site must be byte-stable across runs");
}

/// Fetch the single artifact of `kind`, asserting exactly one exists.
fn artifact(site: &treaty_ssg::GeneratedSite, kind: ArtifactKind) -> &treaty_ssg::SiteArtifact {
    let mut matches = site.artifacts.iter().filter(|a| a.kind == kind);
    let found = matches.next().unwrap_or_else(|| panic!("missing artifact {kind:?}"));
    assert!(matches.next().is_none(), "expected exactly one {kind:?} artifact");
    found
}

#[test]
fn empty_render_data_route_does_not_embed_hydration_state() {
    // A purely static route (no macro, no macro_input) renders fine but embeds no
    // hydration state — proving the `has_state` flag tracks real render data, not
    // merely the presence of a page. This is the static-only branch the e2e
    // fixture (all routes carry data) does not cover.
    let config = SsgConfig {
        out_dir: "dist/static-only".to_string(),
        origin: "https://treaty.dev".to_string(),
        ..SsgConfig::default()
    };
    let routes = vec![RouteSpec {
        path: "legal".to_string(),
        has_component: true,
        ..RouteSpec::default()
    }];
    let static_ivy = r#"
        function Legal_Template(rf, ctx) {
            if (rf & 1) {
                ɵɵelementStart(0, "h1");
                ɵɵtext(1, "Terms");
                ɵɵelementEnd();
            }
        }
    "#;
    let provider = |_route: &DiscoveredRoute| {
        Some(RouteRenderInput {
            ivy_code: static_ivy.to_string(),
            component_id: "legal.tsx".to_string(),
            macro_source: None,
            macro_input: RenderData::new(),
        })
    };
    let site = prerender_site(&config, &routes, &RouteParamsMap::new(), &FixtureSeam, &provider);

    assert_eq!(site.pages.len(), 1);
    let legal = &site.pages[0];
    assert!(legal.document.contains("<h1>Terms</h1>"));
    // No render data ⇒ no embedded state; the manifest records that.
    assert!(legal.data.is_empty());
    assert_eq!(site.hydration.routes.len(), 1);
    assert!(!site.hydration.routes[0].has_state, "empty render data embeds no hydration state");
    // The embedded state script is still present but holds the empty object.
    assert!(legal.document.contains(
        "<script type=\"application/json\" id=\"__TREATY_SSG_STATE__\">{}</script>"
    ));
}

#[test]
fn sitemap_entry_helper_is_reachable_through_the_public_api() {
    // A small public-surface sanity check: `SitemapEntry::new` is part of the
    // re-exported API the shim builds on.
    let entry = SitemapEntry::new("/about");
    assert_eq!(entry.url, "/about");
    assert!(entry.lastmod.is_none() && entry.changefreq.is_none() && entry.priority.is_none());
}
