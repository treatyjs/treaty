//! The per-request inputs an [`crate::SsrHandler`] resolves a route render
//! against, plus the serde-serializable response it produces.
//!
//! A request-time render differs from a build-time prerender in exactly one
//! way: the render data is not known ahead of time — it depends on THIS
//! request's route params, query string, headers, and cookies (and any server
//! functions those drive). [`SsrRequest`] captures that surface as plain data so
//! the handler is a pure function of `(route, request)`; nothing here touches a
//! socket. The host (the generated axum `main.rs`, a test, or a dev server)
//! builds an [`SsrRequest`] from the live HTTP request and hands it in.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One HTTP request, reduced to the data a route render reads.
///
/// Every map is ordered for deterministic injection (a given request always
/// produces byte-identical render input), and the whole struct is
/// serde-serializable so a host can construct it from a parsed HTTP request or a
/// test can build one literally. The fields mirror what a render-time macro /
/// server function legitimately depends on: the matched route's `params`, the
/// URL `query`, request `headers`, and parsed `cookies`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SsrRequest {
    /// The request path as received (`"/blog/hello-world"`), used to resolve the
    /// route and as the default document title.
    #[serde(default)]
    pub path: String,
    /// The matched route's path params (`:slug` -> `"hello-world"`). For a
    /// file-routing / Router match the host fills these from the matched pattern.
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    /// The parsed URL query parameters (`?page=2` -> `page` -> `"2"`).
    #[serde(default)]
    pub query: BTreeMap<String, String>,
    /// The request headers, lowercased names -> value (`"user-agent"` -> ...).
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// The parsed `Cookie:` header, name -> value.
    #[serde(default)]
    pub cookies: BTreeMap<String, String>,
}

impl SsrRequest {
    /// A bare request for `path` with no params/query/headers/cookies. The
    /// common case for a static (non-parameterized) route.
    pub fn new(path: impl Into<String>) -> Self {
        Self { path: path.into(), ..Self::default() }
    }

    /// Project this request into the single JSON object a render-time macro /
    /// server function receives as its `input`. The shape is stable so an author
    /// can read `input.params.slug`, `input.query.page`, `input.headers`, and
    /// `input.cookies` from a render-time macro and get this request's values.
    ///
    /// This is the request-time analogue of the SSG `RouteRenderInput.macro_input`:
    /// where the build path injects a hard-coded macro input, the request path
    /// injects the live request. Returned as a [`Value::Object`] so it can be
    /// merged with any caller-supplied macro input.
    pub fn to_macro_input(&self) -> Value {
        let mut obj = serde_json::Map::new();
        obj.insert("path".to_string(), Value::String(self.path.clone()));
        obj.insert("params".to_string(), string_map_to_json(&self.params));
        obj.insert("query".to_string(), string_map_to_json(&self.query));
        obj.insert("headers".to_string(), string_map_to_json(&self.headers));
        obj.insert("cookies".to_string(), string_map_to_json(&self.cookies));
        Value::Object(obj)
    }
}

/// Convert an ordered `String -> String` map into a JSON object value.
fn string_map_to_json(map: &BTreeMap<String, String>) -> Value {
    Value::Object(
        map.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect(),
    )
}

/// The result of rendering one route for one request: the complete,
/// hydration-ready HTML document plus the metadata a host folds into its HTTP
/// response (status, content type) and the diagnostics a caller asserts on.
///
/// Mirrors the SSG `PrerenderedPage` document payload, but produced per request:
/// the same `<head>` + hydration-marker + embedded-state document shape, so the
/// client runtime hydrates a request render exactly as a prerender.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SsrResponse {
    /// The HTTP status code the host should send (200 for a rendered route, 404
    /// when no route resolved).
    pub status: u16,
    /// The `Content-Type` for the body (`"text/html; charset=utf-8"`).
    pub content_type: String,
    /// The complete hydration-ready HTML document (or a minimal 404 body).
    pub body: String,
    /// The render data the template was bound against this request — the same
    /// state embedded in the document for hydration, surfaced for callers/tests.
    pub data: crate::RenderData,
}

impl SsrResponse {
    /// The standard HTML content type for an SSR document body.
    pub const HTML_CONTENT_TYPE: &'static str = "text/html; charset=utf-8";

    /// A 200 HTML response carrying a rendered `body` and its render `data`.
    pub fn html(body: String, data: crate::RenderData) -> Self {
        Self {
            status: 200,
            content_type: Self::HTML_CONTENT_TYPE.to_string(),
            body,
            data,
        }
    }

    /// A 404 response for a request that matched no SSR route. The body is a
    /// minimal HTML document so a browser still renders something sensible.
    pub fn not_found(path: &str) -> Self {
        Self {
            status: 404,
            content_type: Self::HTML_CONTENT_TYPE.to_string(),
            body: format!(
                "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
                 <title>404 Not Found</title>\n</head>\n<body>\n\
                 <h1>404 Not Found</h1>\n<p>No route matched {path}.</p>\n\
                 </body>\n</html>\n",
                path = crate::escape::escape_html(path),
            ),
            data: crate::RenderData::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macro_input_exposes_request_surface() {
        let mut req = SsrRequest::new("/blog/hello");
        req.params.insert("slug".to_string(), "hello".to_string());
        req.query.insert("page".to_string(), "2".to_string());
        req.headers.insert("user-agent".to_string(), "probe".to_string());
        req.cookies.insert("sid".to_string(), "abc".to_string());

        let input = req.to_macro_input();
        assert_eq!(input["path"], Value::String("/blog/hello".to_string()));
        assert_eq!(input["params"]["slug"], Value::String("hello".to_string()));
        assert_eq!(input["query"]["page"], Value::String("2".to_string()));
        assert_eq!(input["headers"]["user-agent"], Value::String("probe".to_string()));
        assert_eq!(input["cookies"]["sid"], Value::String("abc".to_string()));
    }

    #[test]
    fn not_found_is_a_minimal_html_document() {
        let resp = SsrResponse::not_found("/missing");
        assert_eq!(resp.status, 404);
        assert!(resp.body.starts_with("<!doctype html>"));
        assert!(resp.body.contains("404 Not Found"));
        assert!(resp.body.contains("/missing"));
        assert!(resp.data.is_empty());
    }

    #[test]
    fn not_found_escapes_the_path() {
        let resp = SsrResponse::not_found("/<script>");
        assert!(!resp.body.contains("/<script>"));
        assert!(resp.body.contains("/&lt;script&gt;"));
    }

    #[test]
    fn request_round_trips_through_serde() {
        let mut req = SsrRequest::new("/x");
        req.query.insert("a".to_string(), "1".to_string());
        let text = serde_json::to_string(&req).expect("serialize");
        let back: SsrRequest = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(req, back);
    }
}
