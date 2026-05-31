//! Route discovery: enumerate the concrete, prerenderable routes from a routes
//! config, fanning parameterized routes out into one [`DiscoveredRoute`] per
//! supplied (or computed) param set. Ports the deterministic core of the TS
//! `routes.ts`.
//!
//! Two kinds of route are prerenderable: **static** routes (`path: "about"`,
//! `path: ""`) yield exactly one output, and **parameterized** routes
//! (`path: "blog/:slug"`) yield one output per param set the caller supplies —
//! Treaty cannot know the universe of `:slug` values at build time, so a
//! parameterized route with no supplied params is skipped. The walk recurses
//! into children, accumulating the URL prefix, so a renderable child of a layout
//! route is still discovered at its full path. Wildcard (`**`) and pure-redirect
//! routes are never prerenderable and are dropped.
//!
//! Param sets come from two sources, mirroring the TS `discoverRoutes` /
//! `discoverRoutesAsync` pair:
//!   - a static [`RouteParamsMap`] keyed by declared path, and
//!   - a `getStaticPaths`-style [`StaticPathsProvider`] computed at build time
//!     (typically backed by a render-time macro run through the injected
//!     [`RenderSeam`]). Static sets come first, then provided ones, so a route
//!     can mix hard-coded and computed params.

use crate::types::{DiscoveredRoute, RenderData, RenderSeam, RouteParams, RouteSpec};

use std::collections::BTreeMap;

use serde_json::Value;

/// Caller-supplied parameter sets for parameterized routes, keyed by the
/// route's declared path (e.g. `"blog/:slug"`). Mirrors the TS `RouteParamsMap`.
pub type RouteParamsMap = BTreeMap<String, Vec<RouteParams>>;

/// The request a [`StaticPathsProvider`] receives for one parameterized route.
/// Mirrors the TS `StaticPathsRequest` (minus the route node, which the pure
/// core does not carry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticPathsRequest<'a> {
    /// The full declared route path including any parent prefix (`"blog/:slug"`).
    pub route_path: &'a str,
    /// The `:param` names declared in the path (`["slug"]`), in path order.
    pub params: &'a [String],
}

/// A `getStaticPaths`-style provider: given a parameterized route, return the
/// param sets to materialize at build time. This is the dynamic counterpart to
/// a static [`RouteParamsMap`] entry — a route can compute its `:slug` universe
/// from a content directory, a CMS, or a render-time macro instead of
/// hard-coding it.
///
/// Returning an empty `Vec` for a route materializes none of it. Implementations
/// MUST be deterministic for a given request so discovery output is reproducible.
pub trait StaticPathsProvider {
    /// Resolve the param sets for one parameterized route.
    fn static_paths(&self, request: &StaticPathsRequest<'_>) -> Vec<RouteParams>;
}

/// Any closure of the right shape is a [`StaticPathsProvider`], so a caller can
/// pass an inline lambda without a named type.
impl<F> StaticPathsProvider for F
where
    F: Fn(&StaticPathsRequest<'_>) -> Vec<RouteParams>,
{
    fn static_paths(&self, request: &StaticPathsRequest<'_>) -> Vec<RouteParams> {
        (self)(request)
    }
}

/// A [`StaticPathsProvider`] that computes a route's param sets by running a
/// render-time macro through the injected [`RenderSeam`] — the Rust counterpart
/// of a TS `getStaticPaths` backed by a macro.
///
/// For each parameterized route, the macro source is resolved (keyed by declared
/// path through `resolve_macro`) and run through the seam with the route path
/// injected as the macro `input`; the returned [`RenderData`] is read for a
/// `paths` array, each element of which is interpreted as one param set
/// (a JSON object of `name -> value`, non-string scalars coerced to their
/// natural string form, non-scalars dropped). A route with no resolved macro
/// contributes no sets. This keeps the heavy execution behind the seam so the
/// core stays pure and unit-testable with a fake seam.
pub struct MacroStaticPaths<'a, R, F>
where
    R: RenderSeam + ?Sized,
    F: Fn(&StaticPathsRequest<'_>) -> Option<String>,
{
    seam: &'a R,
    resolve_macro: F,
}

impl<'a, R, F> MacroStaticPaths<'a, R, F>
where
    R: RenderSeam + ?Sized,
    F: Fn(&StaticPathsRequest<'_>) -> Option<String>,
{
    /// Build a macro-backed static-paths provider over `seam`, resolving each
    /// route's macro source through `resolve_macro` (return `None` to skip).
    pub fn new(seam: &'a R, resolve_macro: F) -> Self {
        Self { seam, resolve_macro }
    }
}

impl<R, F> StaticPathsProvider for MacroStaticPaths<'_, R, F>
where
    R: RenderSeam + ?Sized,
    F: Fn(&StaticPathsRequest<'_>) -> Option<String>,
{
    fn static_paths(&self, request: &StaticPathsRequest<'_>) -> Vec<RouteParams> {
        let Some(macro_src) = (self.resolve_macro)(request) else {
            return Vec::new();
        };
        let mut input: RenderData = BTreeMap::new();
        input.insert("routePath".to_string(), Value::String(request.route_path.to_string()));
        input.insert(
            "params".to_string(),
            Value::Array(request.params.iter().cloned().map(Value::String).collect()),
        );
        let data = self.seam.render(&macro_src, &input);
        param_sets_from_render_data(&data)
    }
}

/// Read the `paths` array out of a macro's [`RenderData`] result into param
/// sets. Each array element must be a JSON object; its scalar entries become
/// `name -> value` bindings (strings verbatim, numbers/bools coerced to their
/// natural string form, `null` and non-scalar values dropped). A missing or
/// non-array `paths` key yields no sets.
fn param_sets_from_render_data(data: &RenderData) -> Vec<RouteParams> {
    let Some(Value::Array(items)) = data.get("paths") else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| match item {
            Value::Object(map) => Some(
                map.iter()
                    .filter_map(|(name, value)| {
                        scalar_to_string(value).map(|text| (name.clone(), text))
                    })
                    .collect::<RouteParams>(),
            ),
            _ => None,
        })
        .collect()
}

/// Coerce a JSON scalar to its param-string form, or `None` for `null`/arrays/
/// objects (which cannot be a URL segment value).
fn scalar_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

/// Options controlling route discovery. Mirrors the deterministic subset of the
/// TS `DiscoverRoutesOptions`.
#[derive(Debug, Clone, Default)]
pub struct DiscoverOptions {
    /// Include routes that have no component to render (pure layout/redirect
    /// shells with only `children`). Defaults to `false`: a route is only
    /// emitted when it carries an eager component or a lazy loader. Layout
    /// routes are still walked for their children regardless of this flag.
    pub include_componentless: bool,
}

/// Join a parent URL prefix with a child segment, trimming surrounding slashes.
fn join_path(parent: &str, segment: &str) -> String {
    let child = segment.trim_matches('/');
    if parent.is_empty() {
        child.to_string()
    } else if child.is_empty() {
        parent.to_string()
    } else {
        format!("{parent}/{child}")
    }
}

/// A wildcard catch-all (`**`) is never a concrete prerenderable URL.
fn is_wildcard(path: &str) -> bool {
    path.trim_matches('/') == "**"
}

/// The `:param` names declared in a route path (`"blog/:slug"` -> `["slug"]`).
fn param_names(route_path: &str) -> Vec<String> {
    route_path
        .split('/')
        .filter_map(|seg| seg.strip_prefix(':').map(str::to_string))
        .collect()
}

/// Substitute `:param` segments in `route_path` using `params`; returns `None`
/// if any declared param is missing or empty (mirrors the TS error path, but as
/// a skip so discovery stays infallible and pure).
fn substitute(route_path: &str, params: &RouteParams) -> Option<String> {
    let mut segments = Vec::new();
    for seg in route_path.split('/') {
        match seg.strip_prefix(':') {
            None => segments.push(seg.to_string()),
            Some(name) => {
                let value = params.get(name).filter(|value| !value.is_empty())?;
                segments.push(encode_segment(value));
            }
        }
    }
    Some(segments.join("/"))
}

/// Percent-encode a param value for use as a URL path segment, mirroring the TS
/// `encodeURIComponent`: unreserved characters and the `encodeURIComponent`
/// exception set (`!'()*-._~`) pass through, everything else is `%XX` of its
/// UTF-8 bytes.
fn encode_segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if is_unreserved(byte) {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(to_hex_digit(byte >> 4));
            out.push(to_hex_digit(byte & 0x0f));
        }
    }
    out
}

/// Whether `byte` is left unescaped by `encodeURIComponent`.
fn is_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')')
}

/// Map a nibble (`0..=15`) to its uppercase hex digit, matching the casing
/// `encodeURIComponent` emits.
fn to_hex_digit(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        _ => (b'A' + (nibble - 10)) as char,
    }
}

/// Normalize a discovered URL to a single leading slash (index -> `"/"`).
fn to_url(raw_path: &str) -> String {
    let clean = raw_path.trim_matches('/');
    if clean.is_empty() {
        "/".to_string()
    } else {
        format!("/{clean}")
    }
}

/// Whether `route` is a concrete prerenderable node (not a wildcard / pure
/// redirect), honoring the componentless-inclusion flag.
fn is_renderable(route: &RouteSpec, include_componentless: bool) -> bool {
    let has_component = route.renderable_component();
    let wildcard = is_wildcard(&route.path);
    let pure_redirect = route.redirect_to.is_some() && !has_component;
    (has_component || include_componentless) && !wildcard && !pure_redirect
}

/// Emit the single [`DiscoveredRoute`] for a static (non-parameterized) node.
fn static_route(here: &str) -> DiscoveredRoute {
    DiscoveredRoute {
        url: to_url(here),
        route_path: here.to_string(),
        parameterized: false,
        params: RouteParams::new(),
    }
}

/// Emit one [`DiscoveredRoute`] per param set for a parameterized node, dropping
/// any set that does not bind every declared `:param`.
fn parameterized_routes(here: &str, sets: &[RouteParams]) -> Vec<DiscoveredRoute> {
    sets.iter()
        .filter_map(|params| {
            substitute(here, params).map(|substituted| DiscoveredRoute {
                url: to_url(&substituted),
                route_path: here.to_string(),
                parameterized: true,
                params: params.clone(),
            })
        })
        .collect()
}

/// The static param sets declared for a route in `params`, looked up by the
/// declared `path` first and then by the full accumulated path, mirroring the TS
/// `staticParamSets` fallback.
fn static_param_sets<'a>(
    params: &'a RouteParamsMap,
    route: &RouteSpec,
    here: &str,
) -> &'a [RouteParams] {
    params
        .get(&route.path)
        .or_else(|| params.get(here))
        .map_or(&[][..], Vec::as_slice)
}

/// Walk `routes`, accumulating the URL prefix, and collect every concrete
/// prerenderable route, resolving parameterized routes through the static
/// `params` map plus an optional `provider`.
fn walk(
    routes: &[RouteSpec],
    prefix: &str,
    params: &RouteParamsMap,
    options: &DiscoverOptions,
    provider: Option<&dyn StaticPathsProvider>,
    out: &mut Vec<DiscoveredRoute>,
) {
    for route in routes {
        let here = join_path(prefix, &route.path);

        if is_renderable(route, options.include_componentless) {
            let names = param_names(&here);
            if names.is_empty() {
                out.push(static_route(&here));
            } else {
                let mut sets = static_param_sets(params, route, &here).to_vec();
                if let Some(provider) = provider {
                    let request = StaticPathsRequest { route_path: &here, params: &names };
                    sets.extend(provider.static_paths(&request));
                }
                out.extend(parameterized_routes(&here, &sets));
            }
        }

        if !route.children.is_empty() {
            walk(&route.children, &here, params, options, provider, out);
        }
    }
}

/// Enumerate every concrete, prerenderable route from a routes config.
///
/// Static routes (no `:param`) each yield one [`DiscoveredRoute`];
/// parameterized routes yield one per param set supplied in `params` (and none
/// if no set is given — Treaty cannot invent the param values). Wildcard
/// (`**`) and pure-redirect routes are excluded. Children are walked
/// recursively, so the result is the full flat list of URLs the prerender
/// pipeline will materialize.
///
/// This is the static-only entry point (defaults: components-only); use
/// [`discover_routes_with`] to inject a [`StaticPathsProvider`] or change the
/// componentless-inclusion behavior.
pub fn discover_routes(routes: &[RouteSpec], params: &RouteParamsMap) -> Vec<DiscoveredRoute> {
    discover_routes_with(routes, params, &DiscoverOptions::default(), None)
}

/// Enumerate prerenderable routes, additionally resolving each parameterized
/// route's param sets through `provider` (a `getStaticPaths`-style seam, e.g.
/// [`MacroStaticPaths`]) on top of the static `params` map.
///
/// Static `params` sets come first, then provider-computed ones, so a route can
/// mix hard-coded and computed params — mirroring the TS `discoverRoutesAsync`.
/// Static routes and non-parameterized output are identical to
/// [`discover_routes`].
pub fn discover_routes_with(
    routes: &[RouteSpec],
    params: &RouteParamsMap,
    options: &DiscoverOptions,
    provider: Option<&dyn StaticPathsProvider>,
) -> Vec<DiscoveredRoute> {
    let mut out = Vec::new();
    if !routes.is_empty() {
        walk(routes, "", params, options, provider, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Build a `RouteSpec` carrying an eager component at `path`.
    fn component(path: &str) -> RouteSpec {
        RouteSpec { path: path.to_string(), has_component: true, ..RouteSpec::default() }
    }

    /// Build a single-binding `RouteParams` set (`name -> value`).
    fn params_set(pairs: &[(&str, &str)]) -> RouteParams {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    /// A deterministic fake render seam: it echoes the macro source's intent by
    /// reading a fixed `paths` payload it is constructed with, ignoring input,
    /// so the macro-backed provider can be exercised without a runtime.
    struct PathsSeam {
        data: RenderData,
    }

    impl RenderSeam for PathsSeam {
        fn render(&self, _macro_src: &str, _input: &RenderData) -> RenderData {
            self.data.clone()
        }
    }

    #[test]
    fn static_routes_yield_one_each_with_leading_slash() {
        let routes = vec![component(""), component("about"), component("blog")];
        let routes_discovered = discover_routes(&routes, &RouteParamsMap::new());
        let urls: Vec<&str> = routes_discovered.iter().map(|r| r.url.as_str()).collect();
        assert_eq!(urls, vec!["/", "/about", "/blog"]);
        assert!(routes_discovered.iter().all(|r| !r.parameterized));
    }

    #[test]
    fn children_are_walked_with_accumulated_prefix() {
        let routes = vec![RouteSpec {
            path: "blog".to_string(),
            children: vec![component(""), component("archive")],
            ..RouteSpec::default()
        }];
        let discovered = discover_routes(&routes, &RouteParamsMap::new());
        let urls: Vec<&str> = discovered.iter().map(|r| r.url.as_str()).collect();
        // The layout `blog` node is componentless so only its children emit.
        assert_eq!(urls, vec!["/blog", "/blog/archive"]);
    }

    #[test]
    fn componentless_layout_is_skipped_but_recursed_by_default() {
        let routes = vec![RouteSpec {
            path: "shell".to_string(),
            children: vec![component("inner")],
            ..RouteSpec::default()
        }];
        let discovered = discover_routes(&routes, &RouteParamsMap::new());
        let urls: Vec<&str> = discovered.iter().map(|r| r.url.as_str()).collect();
        assert_eq!(urls, vec!["/shell/inner"]);
    }

    #[test]
    fn componentless_layout_is_emitted_when_opted_in() {
        let routes = vec![RouteSpec {
            path: "shell".to_string(),
            children: vec![component("inner")],
            ..RouteSpec::default()
        }];
        let options = DiscoverOptions { include_componentless: true };
        let discovered = discover_routes_with(&routes, &RouteParamsMap::new(), &options, None);
        let urls: Vec<&str> = discovered.iter().map(|r| r.url.as_str()).collect();
        assert_eq!(urls, vec!["/shell", "/shell/inner"]);
    }

    #[test]
    fn wildcard_and_pure_redirect_routes_are_dropped() {
        let routes = vec![
            component("home"),
            RouteSpec { path: "**".to_string(), has_component: true, ..RouteSpec::default() },
            RouteSpec {
                path: "old".to_string(),
                redirect_to: Some("/home".to_string()),
                ..RouteSpec::default()
            },
        ];
        let discovered = discover_routes(&routes, &RouteParamsMap::new());
        let urls: Vec<&str> = discovered.iter().map(|r| r.url.as_str()).collect();
        assert_eq!(urls, vec!["/home"]);
    }

    #[test]
    fn redirect_with_a_component_is_still_renderable() {
        let routes = vec![RouteSpec {
            path: "landing".to_string(),
            has_component: true,
            redirect_to: Some("/elsewhere".to_string()),
            ..RouteSpec::default()
        }];
        let discovered = discover_routes(&routes, &RouteParamsMap::new());
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].url, "/landing");
    }

    #[test]
    fn parameterized_route_without_params_is_skipped() {
        let routes = vec![component("blog/:slug")];
        let discovered = discover_routes(&routes, &RouteParamsMap::new());
        assert!(discovered.is_empty());
    }

    #[test]
    fn parameterized_route_fans_out_over_static_param_sets() {
        let routes = vec![component("blog/:slug")];
        let mut params = RouteParamsMap::new();
        params.insert(
            "blog/:slug".to_string(),
            vec![params_set(&[("slug", "hello-world")]), params_set(&[("slug", "second")])],
        );
        let discovered = discover_routes(&routes, &params);
        let urls: Vec<&str> = discovered.iter().map(|r| r.url.as_str()).collect();
        assert_eq!(urls, vec!["/blog/hello-world", "/blog/second"]);
        assert!(discovered.iter().all(|r| r.parameterized));
        assert_eq!(discovered[0].params.get("slug").map(String::as_str), Some("hello-world"));
    }

    #[test]
    fn param_values_are_percent_encoded_like_encode_uri_component() {
        let routes = vec![component("docs/:topic")];
        let mut params = RouteParamsMap::new();
        params.insert(
            "docs/:topic".to_string(),
            vec![params_set(&[("topic", "a b/c?d")])],
        );
        let discovered = discover_routes(&routes, &params);
        assert_eq!(discovered[0].url, "/docs/a%20b%2Fc%3Fd");
    }

    #[test]
    fn param_set_missing_a_binding_is_dropped() {
        let routes = vec![component("blog/:slug")];
        let mut params = RouteParamsMap::new();
        params.insert(
            "blog/:slug".to_string(),
            vec![params_set(&[("other", "x")]), params_set(&[("slug", "ok")])],
        );
        let discovered = discover_routes(&routes, &params);
        let urls: Vec<&str> = discovered.iter().map(|r| r.url.as_str()).collect();
        assert_eq!(urls, vec!["/blog/ok"]);
    }

    #[test]
    fn provider_fans_out_via_a_closure() {
        let routes = vec![component("blog/:slug")];
        let provider = |request: &StaticPathsRequest<'_>| {
            assert_eq!(request.route_path, "blog/:slug");
            assert_eq!(request.params, &["slug".to_string()]);
            vec![params_set(&[("slug", "from-provider")])]
        };
        let discovered =
            discover_routes_with(&routes, &RouteParamsMap::new(), &DiscoverOptions::default(), Some(&provider));
        let urls: Vec<&str> = discovered.iter().map(|r| r.url.as_str()).collect();
        assert_eq!(urls, vec!["/blog/from-provider"]);
    }

    #[test]
    fn static_params_precede_provider_params() {
        let routes = vec![component("blog/:slug")];
        let mut params = RouteParamsMap::new();
        params.insert("blog/:slug".to_string(), vec![params_set(&[("slug", "static")])]);
        let provider =
            |_request: &StaticPathsRequest<'_>| vec![params_set(&[("slug", "computed")])];
        let discovered =
            discover_routes_with(&routes, &params, &DiscoverOptions::default(), Some(&provider));
        let urls: Vec<&str> = discovered.iter().map(|r| r.url.as_str()).collect();
        assert_eq!(urls, vec!["/blog/static", "/blog/computed"]);
    }

    #[test]
    fn macro_static_paths_fans_out_through_the_render_seam() {
        let routes = vec![component("blog/:slug")];
        let mut data: RenderData = BTreeMap::new();
        data.insert(
            "paths".to_string(),
            json!([{ "slug": "first" }, { "slug": "second" }, { "slug": 3 }]),
        );
        let seam = PathsSeam { data };
        let provider = MacroStaticPaths::new(&seam, |request: &StaticPathsRequest<'_>| {
            assert_eq!(request.route_path, "blog/:slug");
            Some("export default () => ({ paths: [] })".to_string())
        });
        let discovered =
            discover_routes_with(&routes, &RouteParamsMap::new(), &DiscoverOptions::default(), Some(&provider));
        let urls: Vec<&str> = discovered.iter().map(|r| r.url.as_str()).collect();
        // String slugs pass through; the numeric `3` is coerced to "3".
        assert_eq!(urls, vec!["/blog/first", "/blog/second", "/blog/3"]);
    }

    #[test]
    fn macro_static_paths_skips_routes_with_no_resolved_macro() {
        let routes = vec![component("blog/:slug")];
        let seam = PathsSeam { data: BTreeMap::new() };
        let provider = MacroStaticPaths::new(&seam, |_request: &StaticPathsRequest<'_>| None);
        let discovered =
            discover_routes_with(&routes, &RouteParamsMap::new(), &DiscoverOptions::default(), Some(&provider));
        assert!(discovered.is_empty());
    }

    #[test]
    fn discovery_output_round_trips_through_serde() {
        let routes = vec![component("about")];
        let discovered = discover_routes(&routes, &RouteParamsMap::new());
        let text = serde_json::to_string(&discovered).expect("serialize");
        let back: Vec<DiscoveredRoute> = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(discovered, back);
    }
}
