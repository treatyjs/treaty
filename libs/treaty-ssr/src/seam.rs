//! The request-time render seam: a Nova-backed [`RenderSeam`] that executes a
//! route's render-time macro PER REQUEST to produce its [`RenderData`].
//!
//! The SSG core runs a route's macro once at build time through an injected
//! `RenderSeam`. Request-time SSR runs the SAME macro again per request, with the
//! live request injected as the macro `input`, so a route renders against this
//! request's params/query/headers/cookies + server-fn results. Reusing the SSG
//! `RenderSeam` trait means the request path feeds the identical `ivy_html` +
//! `seo` rendering the build path uses — only the data source differs.
//!
//! [`NovaRenderSeam`] backs that with [`treaty_runtime::run_macro`], the exact
//! Nova path the SSG production prerender uses; a macro that throws degrades to
//! the injected input as render data (the route still renders, just without the
//! macro's enrichment) rather than 500-ing the request.

use serde_json::Value;

use crate::RenderData;
use treaty_ssg::RenderSeam;

/// A Nova-backed [`RenderSeam`] for request-time render-data resolution.
///
/// Executes a route's render-time macro through [`treaty_runtime::run_macro`]
/// with the per-request input injected, and adapts the resulting JSON object back
/// into the [`RenderData`] (`BTreeMap<String, Value>`) the `treaty_ssg`
/// interpreter binds against. A macro whose result is not a JSON object (or that
/// throws) falls back to the injected input flattened to render data, so a route
/// always has SOMETHING to render and a single broken macro never fails the page.
#[derive(Debug, Default, Clone, Copy)]
pub struct NovaRenderSeam;

impl RenderSeam for NovaRenderSeam {
    fn render(&self, macro_src: &str, input: &RenderData) -> RenderData {
        // `RenderData` is `BTreeMap<String, Value>`; the runtime macro reads a
        // single JSON object `input`, so project the map into one object.
        let input_json = render_data_to_json(input);

        match treaty_runtime::run_macro(macro_src, &input_json) {
            Ok(output) => match output.into_value() {
                // The macro returned a JSON object: that IS the render data.
                Value::Object(map) => map.into_iter().collect(),
                // A non-object result (the macro returned a scalar / null) cannot
                // be a render-data map; fall back to the request input so the
                // route still renders against the known request fields.
                _ => input.clone(),
            },
            // The macro threw / failed to transpile: degrade to the input as data
            // rather than aborting the request.
            Err(_) => input.clone(),
        }
    }
}

/// Project a [`RenderData`] map into the single JSON object the runtime macro
/// receives as `input`.
fn render_data_to_json(data: &RenderData) -> Value {
    Value::Object(data.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn data(pairs: &[(&str, Value)]) -> RenderData {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    #[test]
    fn macro_reads_request_input_and_returns_render_data() {
        // A real macro run through Nova: it reads `input.params.slug` and returns
        // a `{ title }` object that becomes the render data.
        let mut params = serde_json::Map::new();
        params.insert("slug".to_string(), json!("hello-world"));
        let input = data(&[("params", Value::Object(params))]);

        let macro_src = "export default { title: 'Post: ' + input.params.slug }";
        let out = NovaRenderSeam.render(macro_src, &input);
        assert_eq!(out.get("title"), Some(&json!("Post: hello-world")));
    }

    #[test]
    fn a_throwing_macro_degrades_to_the_input() {
        let input = data(&[("path", json!("/x"))]);
        // A macro that throws: the seam must not panic; it returns the input.
        let out = NovaRenderSeam.render("throw new Error('boom')", &input);
        assert_eq!(out, input);
    }

    #[test]
    fn a_non_object_macro_result_falls_back_to_input() {
        let input = data(&[("k", json!("v"))]);
        let out = NovaRenderSeam.render("export default 42", &input);
        assert_eq!(out, input);
    }

    #[test]
    fn empty_input_is_handled() {
        let out = NovaRenderSeam.render("export default { x: 1 }", &BTreeMap::new());
        assert_eq!(out.get("x"), Some(&json!(1)));
    }
}
