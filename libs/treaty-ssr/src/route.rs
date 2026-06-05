//! The server-side route table the [`crate::SsrHandler`] resolves a request
//! against: each entry is one route pattern paired with the server component it
//! renders (the `dist/server` Ivy + an optional render-time macro + the server
//! functions whose results the macro consumes).
//!
//! This is the request-time counterpart of the SSG `IvyProvider`: where the
//! build path resolves a DISCOVERED route to a `RouteRenderInput`, the request
//! path matches a live request PATH against these patterns, binding `:param`
//! segments out of the URL so the matched route's render reads them.
//!
//! Pattern matching is the same `:segment` / `*` shape the file-routing core and
//! `@angular/router` use, kept deliberately small (segment-wise compare with a
//! single trailing wildcard) so a route table built from either source matches
//! identically.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::server_fn::SsrServerFn;

/// One server-side route: a URL pattern and the server component that renders it.
///
/// The component is described by its emitted `dist/server` Ivy (`ivy_code`), the
/// optional render-time macro that computes its data, and the server functions
/// the macro may call. None of this is loaded from disk by the pure core — a
/// host (the generated axum host, the vite SSR build, a test) constructs the
/// table from its `dist/server` output and the build's hydration manifest.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SsrRoute {
    /// The route pattern with `:param` segments and an optional trailing `*`
    /// wildcard (`"/"`, `"/about"`, `"/blog/:slug"`, `"/docs/*"`).
    pub pattern: String,
    /// The emitted server (`dist/server`) Ivy JS for the route's component. The
    /// request render interprets this to HTML exactly as the SSG path does.
    #[serde(default)]
    pub ivy_code: String,
    /// The component / file identifier, used to name hydration islands so the
    /// request render's manifest entry matches the build's.
    #[serde(default)]
    pub component_id: String,
    /// The route's render-time macro source, if any. Executed per request
    /// through the render seam with the request injected as `input`; its JSON
    /// result is the render data. Omit for a purely static component.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub macro_source: Option<String>,
    /// The server functions this route's macro may invoke to load data. Their
    /// bodies run server-side (Nova) and their results are merged into the macro
    /// input under `input.server.<name>` before the macro runs — the request-time
    /// "load server data, then render" step.
    #[serde(default)]
    pub server_fns: Vec<SsrServerFn>,
}

impl SsrRoute {
    /// A static (non-parameterized) route serving `ivy_code` at `pattern`.
    pub fn new(pattern: impl Into<String>, ivy_code: impl Into<String>) -> Self {
        Self { pattern: pattern.into(), ivy_code: ivy_code.into(), ..Self::default() }
    }
}

/// A route matched against a concrete request path: the route plus the `:param`
/// bindings extracted from the URL.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchedRoute<'a> {
    /// The route whose pattern matched.
    pub route: &'a SsrRoute,
    /// The path params bound from the URL (`:slug` -> the matched segment).
    pub params: BTreeMap<String, String>,
}

/// The ordered server route table. Routes are matched in order, first match
/// wins, so a host can place more specific patterns before catch-alls (mirroring
/// `@angular/router` and the file-routing core's ordering).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SsrRouteTable {
    /// The routes, in match-precedence order.
    pub routes: Vec<SsrRoute>,
}

impl SsrRouteTable {
    /// Build a table from a route list (match order preserved).
    pub fn new(routes: Vec<SsrRoute>) -> Self {
        Self { routes }
    }

    /// Resolve a request `path` to the first matching route and its bound params,
    /// or `None` when nothing matches. A leading/trailing slash is normalized so
    /// `"/about"`, `"/about/"`, and `"about"` resolve the same.
    pub fn resolve(&self, path: &str) -> Option<MatchedRoute<'_>> {
        let req_segments = split_path(path);
        for route in &self.routes {
            if let Some(params) = match_pattern(&route.pattern, &req_segments) {
                return Some(MatchedRoute { route, params });
            }
        }
        None
    }
}

/// Split a URL path into its non-empty segments, dropping a leading/trailing
/// slash. `"/"` and `""` both yield an empty segment list (the index route).
fn split_path(path: &str) -> Vec<&str> {
    // Strip a query/hash the host may not have removed, then split on `/`.
    let path = path.split(['?', '#']).next().unwrap_or(path);
    path.split('/').filter(|s| !s.is_empty()).collect()
}

/// Match one route pattern against the request's path segments, returning the
/// bound `:param` map on success. A `:name` segment binds any one segment; a
/// trailing `*` segment matches the remaining path (including zero segments); a
/// literal segment must compare equal. Lengths must otherwise match.
fn match_pattern(pattern: &str, req: &[&str]) -> Option<BTreeMap<String, String>> {
    let pat = split_path(pattern);
    let mut params = BTreeMap::new();

    let mut i = 0;
    while i < pat.len() {
        let seg = pat[i];
        // A trailing `*` (or `**`) wildcard soaks up the rest of the path.
        if seg == "*" || seg == "**" {
            // Only valid as the final pattern segment.
            return if i == pat.len() - 1 { Some(params) } else { None };
        }
        // Ran out of request segments before the pattern was consumed.
        let Some(req_seg) = req.get(i) else { return None };
        if let Some(name) = seg.strip_prefix(':') {
            params.insert(name.to_string(), (*req_seg).to_string());
        } else if seg != *req_seg {
            return None;
        }
        i += 1;
    }

    // The pattern is fully consumed; it matches only if the request has no extra
    // trailing segments (a non-wildcard pattern is an exact-length match).
    if req.len() == pat.len() { Some(params) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> SsrRouteTable {
        SsrRouteTable::new(vec![
            SsrRoute::new("/", "index"),
            SsrRoute::new("/about", "about"),
            SsrRoute::new("/blog/:slug", "post"),
            SsrRoute::new("/docs/*", "docs"),
        ])
    }

    #[test]
    fn index_route_matches_root_variants() {
        let t = table();
        for p in ["/", "", "/?x=1"] {
            let m = t.resolve(p).unwrap_or_else(|| panic!("no match for {p:?}"));
            assert_eq!(m.route.ivy_code, "index");
            assert!(m.params.is_empty());
        }
    }

    #[test]
    fn static_route_matches_with_trailing_slash() {
        let t = table();
        assert_eq!(t.resolve("/about").unwrap().route.ivy_code, "about");
        assert_eq!(t.resolve("/about/").unwrap().route.ivy_code, "about");
    }

    #[test]
    fn parameterized_route_binds_param() {
        let t = table();
        let m = t.resolve("/blog/hello-world").expect("post match");
        assert_eq!(m.route.ivy_code, "post");
        assert_eq!(m.params.get("slug").map(String::as_str), Some("hello-world"));
    }

    #[test]
    fn wildcard_route_soaks_remaining_path() {
        let t = table();
        let m = t.resolve("/docs/a/b/c").expect("docs match");
        assert_eq!(m.route.ivy_code, "docs");
        // A wildcard with zero remaining segments still matches.
        assert!(t.resolve("/docs").is_some());
    }

    #[test]
    fn unmatched_path_resolves_none() {
        let t = table();
        assert!(t.resolve("/nope/here").is_none());
        // A longer request than a static pattern does not match it.
        assert!(t.resolve("/about/extra").is_none());
    }

    #[test]
    fn first_match_wins_in_order() {
        let t = SsrRouteTable::new(vec![
            SsrRoute::new("/x/:id", "param"),
            SsrRoute::new("/x/special", "special"),
        ]);
        // The parameterized route is listed first, so it wins for `/x/special`.
        assert_eq!(t.resolve("/x/special").unwrap().route.ivy_code, "param");
    }
}
