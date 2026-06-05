//! Shared, serde-serializable data types for `treaty_ssg`, plus the injected
//! [`RenderSeam`] trait that abstracts the Nova prerender execution.
//!
//! These mirror the structural shapes the TS `@treaty/ssg` package exposes
//! (route discovery, head/SEO, prerendered pages, the hydration manifest,
//! sitemap entries, the whole-site config + manifest) so the TS package can
//! later become a thin shim over this crate. Everything here is plain data:
//! the only behavioural boundary is [`RenderSeam`], which the pipeline depends
//! on as a shape rather than embedding Nova.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The render data a route's template interpolates against — the JSON value a
/// render-time macro returns (or `{}` for a purely static route).
///
/// Mirrors the TS `RenderData` (`{ [key: string]: JsonValue }`): the render
/// data is always a JSON object. A `serde_json::Value` is used so arbitrary
/// nested JSON crosses the seam, exactly as the Nova `run_macro` boundary does.
/// A [`BTreeMap`] keeps key order deterministic for reproducible output.
pub type RenderData = BTreeMap<String, Value>;

/// A concrete parameter binding for one materialization of a route
/// (`:slug` -> `"hello-world"`). Ordered for deterministic URL substitution.
///
/// Mirrors the TS `RouteParams` (`Readonly<Record<string, string>>`).
pub type RouteParams = BTreeMap<String, String>;

/// The minimal structural shape of an Angular route this crate needs — a
/// deliberate subset of `@angular/router`'s `Route` (identical in spirit to the
/// TS `RouteLike`) so a caller can pass a real route config without an adapter.
///
/// Extra Angular fields are ignored. Lazy/eager component presence is reduced
/// to the booleans the discovery walk actually branches on, so this crate stays
/// free of any function/loader values that could not cross the JSON boundary.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RouteSpec {
    /// The URL segment for this route (`""` is the empty/index path).
    #[serde(default)]
    pub path: String,
    /// Whether the route carries an eager `component`.
    #[serde(default)]
    pub has_component: bool,
    /// Whether the route carries a lazy `loadComponent` loader.
    #[serde(default)]
    pub has_load_component: bool,
    /// A redirect target; a route that only redirects is not prerenderable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect_to: Option<String>,
    /// Nested child routes, walked recursively with the URL prefix accumulated.
    #[serde(default)]
    pub children: Vec<RouteSpec>,
}

impl RouteSpec {
    /// A route is renderable iff it has an eager component or a lazy loader.
    pub fn renderable_component(&self) -> bool {
        self.has_component || self.has_load_component
    }
}

/// A single concrete, prerenderable route resolved from the config — the route
/// identity in the prerender manifest. Mirrors the TS `DiscoveredRoute`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiscoveredRoute {
    /// The concrete URL with params substituted and a single leading slash
    /// (`"/"`, `"/about"`, `"/blog/hello-world"`).
    pub url: String,
    /// The declared route path before substitution (`"blog/:slug"`).
    pub route_path: String,
    /// Whether the declared path carried any `:param` segments.
    pub parameterized: bool,
    /// The param bindings applied to produce [`DiscoveredRoute::url`].
    pub params: RouteParams,
}

/// Head / SEO metadata for a prerendered document. Every field is optional; the
/// pipeline derives sensible defaults from the route's render data (a
/// `description` or `canonical` key is picked up automatically) and a caller
/// can override per route. Mirrors the TS `HeadMeta`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HeadMeta {
    /// Document `<title>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// `<meta name="description">`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `<link rel="canonical">` href.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical: Option<String>,
    /// `<html lang>` override (else [`SsgConfig::lang`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    /// Open Graph / Twitter / arbitrary `<meta>` tags as `name -> content`. A
    /// key starting with `og:`, `article:`, or `fb:` is emitted as a
    /// `property=` meta (the Open Graph convention); anything else as `name=`.
    #[serde(default)]
    pub meta: BTreeMap<String, String>,
    /// Extra `<link>` tags as `rel -> href` (e.g. `{ "icon": "/favicon.ico" }`).
    #[serde(default)]
    pub links: BTreeMap<String, String>,
}

/// The island kind for one hydratable unit in a prerendered route.
///
/// Mirrors the TS `HydrationIsland['kind']` union (`'component' |
/// 'interpolation'`): a `Component` island is the route root component, an
/// `Interpolation` island marks dynamic text the static render filled but the
/// client may re-evaluate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IslandKind {
    /// The route's root component.
    Component,
    /// A dynamic text interpolation the static render filled.
    Interpolation,
}

/// One hydratable unit (component / island) detected in a prerendered route,
/// for the hydration manifest the client runtime consumes. Mirrors the TS
/// `HydrationIsland`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HydrationIsland {
    /// The island kind: the route root component, or a dynamic interpolation.
    pub kind: IslandKind,
    /// The component/template identifier the island corresponds to.
    pub id: String,
}

/// The result of prerendering one route to a complete, hydration-ready HTML
/// document plus the metadata the site generator folds into its manifest.
///
/// Combines the TS `PrerenderedRoute` manifest entry with the
/// `PrerenderRouteResult` document payload: the full document, the render data
/// the template was bound against (also embedded for hydration), and the
/// detected hydration islands. Byte length is derived by the caller.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrerenderedPage {
    /// The concrete URL that was prerendered (`"/"`, `"/blog/hello"`).
    pub url: String,
    /// Output `index.html` file path under the site's `out_dir`.
    pub output: String,
    /// The declared route path before param substitution.
    pub route_path: String,
    /// Whether this entry came from a parameterized route.
    pub parameterized: bool,
    /// The complete hydration-ready HTML document.
    pub document: String,
    /// Byte length of the emitted document (UTF-8).
    pub bytes: usize,
    /// The render data the template was bound against (embedded for hydration).
    pub data: RenderData,
    /// The hydration islands detected in this route's prerendered markup.
    pub islands: Vec<HydrationIsland>,
}

/// The hydration descriptor emitted per route into the hydration manifest.
/// Mirrors the TS `RouteHydration`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteHydration {
    /// The concrete URL this descriptor is for.
    pub url: String,
    /// Output HTML file for the route.
    pub output: String,
    /// The hydration islands detected in the route's prerendered markup.
    pub islands: Vec<HydrationIsland>,
    /// Whether the route embedded serialized render state for reuse on client.
    pub has_state: bool,
}

/// The serializable hydration manifest (route -> islands), the on-disk shape the
/// client runtime consumes. Mirrors the TS `HydrationManifestFile`
/// (`{ version: 1, routes: RouteHydration[] }`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HydrationManifest {
    /// Manifest schema version. Always `1` for this format.
    pub version: u32,
    /// One descriptor per prerendered route, in discovery order.
    pub routes: Vec<RouteHydration>,
}

impl HydrationManifest {
    /// Build a v1 manifest from a route descriptor list.
    pub fn new(routes: Vec<RouteHydration>) -> Self {
        Self { version: 1, routes }
    }
}

impl Default for HydrationManifest {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

/// A sitemap `<changefreq>` hint. Mirrors the TS `SitemapEntry['changefreq']`
/// union; serialized lowercase to match the emitted XML text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeFreq {
    Always,
    Hourly,
    Daily,
    Weekly,
    Monthly,
    Yearly,
    Never,
}

impl ChangeFreq {
    /// The `<changefreq>` body text for this hint.
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeFreq::Always => "always",
            ChangeFreq::Hourly => "hourly",
            ChangeFreq::Daily => "daily",
            ChangeFreq::Weekly => "weekly",
            ChangeFreq::Monthly => "monthly",
            ChangeFreq::Yearly => "yearly",
            ChangeFreq::Never => "never",
        }
    }
}

/// A single URL entry for the sitemap. Mirrors the TS `SitemapEntry`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SitemapEntry {
    /// The site-root-relative URL path (`"/"`, `"/blog/hello"`).
    pub url: String,
    /// ISO-8601 last-modified date, emitted as `<lastmod>` when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lastmod: Option<String>,
    /// Change frequency hint, emitted as `<changefreq>` when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changefreq: Option<ChangeFreq>,
    /// Crawl priority in `[0,1]`, emitted as `<priority>` when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<f64>,
}

impl SitemapEntry {
    /// A bare entry for `url` with no optional fields set.
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into(), lastmod: None, changefreq: None, priority: None }
    }
}

/// The component a route renders, supplied by the caller for a discovered
/// route. Mirrors the TS `RoutePrerenderInput`: the emitted Ivy JS for the
/// route's compiled component plus an optional render-time macro source.
///
/// This crate does not own compilation — the caller (the TS shim / pipeline)
/// supplies the already-emitted Ivy `ivy_code`. The `macro_source`, when
/// present, is executed through the [`RenderSeam`] to produce render data.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RouteRenderInput {
    /// Emitted Ivy JS for the route's compiled component. Interpreted to static
    /// HTML; an empty string (or one with no template function) renders nothing.
    pub ivy_code: String,
    /// Component / file identifier, used to name the hydration islands.
    #[serde(default)]
    pub component_id: String,
    /// The route's render-time macro source, if any. Executed through the
    /// [`RenderSeam`]; its JSON result is the render data. Omit for a purely
    /// static component.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub macro_source: Option<String>,
    /// Extra JSON input merged over the route params and injected as the macro
    /// `input`. Mirrors the TS `RenderMacro.input`.
    #[serde(default)]
    pub macro_input: RenderData,
}

/// Whole-site generation config. Mirrors the deterministic subset of the TS
/// `SiteConfig` (the function-valued seams — `resolve`, `getStaticPaths`,
/// `fs` — are supplied as Rust callbacks / pre-resolved inputs at the call site
/// rather than living on this data struct).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SsgConfig {
    /// Directory the site is written into. Defaults to `"dist/ssg"`.
    #[serde(default = "default_out_dir")]
    pub out_dir: String,
    /// `<html lang>` value. Defaults to `"en"`.
    #[serde(default = "default_lang")]
    pub lang: String,
    /// Site origin (`https://example.com`) used to make `sitemap.xml` `<loc>`s
    /// and the `robots.txt` `Sitemap:` line absolute. When empty, the sitemap
    /// uses root-relative locations and robots advertises no sitemap.
    #[serde(default)]
    pub origin: String,
    /// `robots.txt` disallow prefixes. Defaults to none (everything allowed).
    #[serde(default)]
    pub disallow: Vec<String>,
    /// Emit `sitemap.xml`. Defaults to `true`.
    #[serde(default = "default_true")]
    pub sitemap: bool,
    /// Emit `robots.txt`. Defaults to `true`.
    #[serde(default = "default_true")]
    pub robots: bool,
    /// Emit the hydration manifest JSON. Defaults to `true`.
    #[serde(default = "default_true")]
    pub hydration_manifest: bool,
}

impl Default for SsgConfig {
    fn default() -> Self {
        Self {
            out_dir: default_out_dir(),
            lang: default_lang(),
            origin: String::new(),
            disallow: Vec::new(),
            sitemap: true,
            robots: true,
            hydration_manifest: true,
        }
    }
}

/// Default output directory for a generated site (`dist/ssg`).
pub fn default_out_dir() -> String {
    "dist/ssg".to_string()
}

/// Default `<html lang>` value (`en`).
pub fn default_lang() -> String {
    "en".to_string()
}

fn default_true() -> bool {
    true
}

/// The artifact kind for one non-HTML output the generator emits.
/// Mirrors the TS `SiteArtifact['kind']` union.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactKind {
    /// `sitemap.xml`.
    Sitemap,
    /// `robots.txt`.
    Robots,
    /// The hydration manifest JSON.
    HydrationManifest,
}

/// A single non-HTML artifact the generator emitted. Mirrors the TS
/// `SiteArtifact` (the `asset` copy kind lives in the TS shim's file layer, not
/// in this pure core).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SiteArtifact {
    /// The artifact kind.
    pub kind: ArtifactKind,
    /// The output path written.
    pub output: String,
    /// The artifact body (the pure core emits content; the shim writes it).
    pub contents: String,
    /// Byte length of the artifact (UTF-8).
    pub bytes: usize,
}

/// The full, serde-serializable result of [`crate::prerender_site`]: every
/// prerendered page, the emitted non-HTML artifacts, and the hydration
/// manifest. Mirrors the TS `SiteManifest` (returned in-memory; the shim is
/// responsible for the actual disk writes).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GeneratedSite {
    /// The output directory the site is written into.
    pub out_dir: String,
    /// One entry per prerendered page, in discovery order.
    pub pages: Vec<PrerenderedPage>,
    /// The non-HTML artifacts emitted (sitemap, robots, hydration manifest).
    pub artifacts: Vec<SiteArtifact>,
    /// The hydration manifest (route -> islands), for in-memory callers.
    pub hydration: HydrationManifest,
}

/// The execute-render-time-data seam — the Nova `run_macro` boundary.
///
/// Treaty is a compiler, not a host, so this crate does not embed Nova: it
/// depends only on this shape. A Nova-backed implementation transpiles the TS
/// `macro_src` to JS, runs it in a fresh isolate with `input` injected as the
/// macro input, and returns the JSON object the template binds against. The
/// unit tests supply a deterministic fake.
///
/// Implementations MUST be deterministic for a given `(macro_src, input)` so
/// prerender output is reproducible. Mirrors the TS `RenderRuntime.runMacro`.
pub trait RenderSeam {
    /// Execute one render-time macro and return its JSON result as the render
    /// data for a route's component.
    fn render(&self, macro_src: &str, input: &RenderData) -> RenderData;
}
