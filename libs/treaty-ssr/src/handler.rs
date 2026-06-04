//! [`SsrHandler`] — the request-time render path: one HTTP request in, one
//! hydration-ready HTML document out, reusing the `treaty_ssg` interpreter and
//! the `treaty_runtime` execution engine end to end.
//!
//! The pipeline mirrors the SSG `prerender_site` loop but is driven by a live
//! request instead of a discovered route:
//!   1. [`SsrRouteTable::resolve`] matches the request path to a server route,
//!      binding `:param` segments out of the URL.
//!   2. The request (params/query/headers/cookies) + the matched route's
//!      server-fn results are assembled into the macro `input`.
//!   3. The route's render-time macro runs through the injected [`RenderSeam`]
//!      (Nova in production) to produce the route's [`RenderData`].
//!   4. `treaty_ssg::ivy_html::render_ivy_to_html` interprets the `dist/server`
//!      Ivy against that data to an HTML fragment, and `detect_islands` records
//!      the hydration islands.
//!   5. `treaty_ssg::seo` resolves the head and wraps the fragment in the same
//!      hydration-marked document the SSG path emits, with the request's render
//!      state embedded — so the client hydrates a request render identically.
//!
//! Every behavioural boundary is injected (the [`RenderSeam`] and the
//! [`ServerFnExecutor`]), so the handler is unit-tested with fakes and wired to
//! Nova in production via [`SsrHandler::nova`].

use treaty_ssg::{ivy_html, seo, RenderData, RenderSeam};

use crate::hydration::RequestHydration;
use crate::request::{SsrRequest, SsrResponse};
use crate::route::SsrRouteTable;
use crate::seam::NovaRenderSeam;
use crate::server_fn::{load_server_data, NovaServerFnExecutor, ServerFnExecutor};

/// The full result of rendering one request: the HTTP-shaped [`SsrResponse`] plus
/// the [`RequestHydration`] descriptor the client boots against. A host serves
/// `response.body` and may inline `hydration` for the client island boot.
#[derive(Debug, Clone, PartialEq)]
pub struct SsrRender {
    /// The HTTP response (status, content type, document body, render data).
    pub response: SsrResponse,
    /// The per-request hydration descriptor (islands + the state they were filled
    /// from). `None` for a 404 (no route rendered).
    pub hydration: Option<RequestHydration>,
}

/// The request-time SSR handler: holds the server route table and the injected
/// execution seams, and renders one request at a time.
///
/// Construct with [`SsrHandler::nova`] for the production Nova-backed path, or
/// [`SsrHandler::with_seams`] to inject fakes in a test. The handler is `Send`
/// + `Sync`-friendly data (the route table) plus zero-sized seam markers, so a
/// host can share one behind an `Arc` across request tasks.
pub struct SsrHandler<S: RenderSeam, E: ServerFnExecutor> {
    routes: SsrRouteTable,
    seam: S,
    executor: E,
    lang: String,
}

impl SsrHandler<NovaRenderSeam, NovaServerFnExecutor> {
    /// The production handler: Nova-backed macro execution and server-fn
    /// execution, both running the exact `treaty_runtime` paths the production
    /// host uses for prerender and `/__server/<name>` calls respectively.
    pub fn nova(routes: SsrRouteTable) -> Self {
        Self {
            routes,
            seam: NovaRenderSeam,
            executor: NovaServerFnExecutor,
            lang: "en".to_string(),
        }
    }
}

impl<S: RenderSeam, E: ServerFnExecutor> SsrHandler<S, E> {
    /// Construct a handler with explicit seams (for tests or an alternative
    /// runtime). `lang` is the `<html lang>` value passed through to the document.
    pub fn with_seams(routes: SsrRouteTable, seam: S, executor: E, lang: impl Into<String>) -> Self {
        Self { routes, seam, executor, lang: lang.into() }
    }

    /// The server route table this handler resolves against.
    pub fn routes(&self) -> &SsrRouteTable {
        &self.routes
    }

    /// Render one request to a hydration-ready document (or a 404 when no route
    /// matches). This is the single per-request entry point the generated axum
    /// host's `GET /*` route calls.
    pub fn render_request(&self, request: &SsrRequest) -> SsrRender {
        // (1) Resolve the route and bind its path params from the URL.
        let Some(matched) = self.routes.resolve(&request.path) else {
            return SsrRender {
                response: SsrResponse::not_found(&request.path),
                hydration: None,
            };
        };
        let route = matched.route;

        // (2) Assemble the macro input: the request surface (params/query/
        // headers/cookies), with the URL-bound route params folded into the
        // request params, plus the server-fn results under `server`.
        let mut request_with_params = request.clone();
        for (k, v) in &matched.params {
            request_with_params.params.entry(k.clone()).or_insert_with(|| v.clone());
        }
        let mut input_obj = match request_with_params.to_macro_input() {
            serde_json::Value::Object(map) => map,
            // `to_macro_input` always returns an object; this arm is unreachable.
            _ => serde_json::Map::new(),
        };
        // Run the route's server functions (request-time data load) and expose
        // their results to the macro as `input.server.<name>`.
        if !route.server_fns.is_empty() {
            let server_data = load_server_data(&route.server_fns, &self.executor);
            input_obj.insert("server".to_string(), server_data);
        }
        // The macro input crosses the seam as a `RenderData` map.
        let macro_input: RenderData = input_obj.into_iter().collect();

        // (3) Resolve the render data: a route with a macro runs it through the
        // seam (with the request injected); a macro-less route renders against
        // the request input directly.
        let data: RenderData = match &route.macro_source {
            Some(src) => self.seam.render(src, &macro_input),
            None => macro_input,
        };

        // (4) Interpret the dist/server Ivy template to an HTML fragment, and
        // record the hydration islands — the SAME interpreter the SSG path uses.
        let fragment = ivy_html::render_ivy_to_html(&route.ivy_code, &data);
        let islands = ivy_html::detect_islands(&route.component_id, &route.ivy_code);

        // (5) Resolve the head and wrap the fragment in the hydration-marked
        // document, embedding THIS request's render state — identical document
        // shape to the SSG prerender, so the client hydrates it the same way.
        let title = request.path.clone();
        let head = seo::resolve_head(None, &data, &title, &self.lang);
        let document = seo::wrap_document(&fragment, &data, &head);

        let hydration = RequestHydration::new(
            request.path.clone(),
            route.component_id.clone(),
            islands,
            data.clone(),
        );

        SsrRender {
            response: SsrResponse::html(document, data),
            hydration: Some(hydration),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use treaty_ssg::IslandKind;

    use crate::route::SsrRoute;
    use crate::server_fn::SsrServerFn;

    /// A render seam that runs a closure over `(macro_src, input)` so a test can
    /// stand in for Nova without the engine.
    struct FnSeam<F: Fn(&str, &RenderData) -> RenderData>(F);
    impl<F: Fn(&str, &RenderData) -> RenderData> RenderSeam for FnSeam<F> {
        fn render(&self, src: &str, input: &RenderData) -> RenderData {
            (self.0)(src, input)
        }
    }

    /// An executor returning a fixed value per fn name.
    struct StubExecutor;
    impl ServerFnExecutor for StubExecutor {
        fn execute(&self, f: &SsrServerFn) -> Option<Value> {
            Some(json!({ "name": f.name }))
        }
    }

    /// A real Ivy `*_Template` whose `<h1>` interpolates `ctx.title`.
    const IVY: &str = r#"
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

    fn route(pattern: &str) -> SsrRoute {
        SsrRoute {
            pattern: pattern.to_string(),
            ivy_code: IVY.to_string(),
            component_id: "app.tsx".to_string(),
            macro_source: Some("export default {}".to_string()),
            server_fns: Vec::new(),
        }
    }

    #[test]
    fn renders_a_route_to_a_hydration_ready_document() {
        // The seam reads the request's bound slug and produces the title.
        let seam = FnSeam(|_src: &str, input: &RenderData| {
            let slug = input
                .get("params")
                .and_then(|p| p.get("slug"))
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string();
            [("title".to_string(), json!(format!("Post {slug}")))].into_iter().collect()
        });
        let table = SsrRouteTable::new(vec![route("/blog/:slug")]);
        let handler = SsrHandler::with_seams(table, seam, StubExecutor, "en");

        let render = handler.render_request(&SsrRequest::new("/blog/hello"));
        let resp = &render.response;
        assert_eq!(resp.status, 200);
        assert_eq!(resp.content_type, SsrResponse::HTML_CONTENT_TYPE);

        // (a) Real HTML: the interpolated title rendered into the <h1>.
        assert!(resp.body.contains("<h1>Post hello</h1>"), "body: {}", resp.body);
        // (b) The hydration boundary: the marked root mount + embedded state.
        assert!(resp.body.contains("data-treaty-ssg=\"1\""));
        assert!(resp.body.contains("__TREATY_SSG_STATE__"));
        assert!(resp.body.contains("Post hello")); // state echoes the title
        assert!(resp.body.starts_with("<!doctype html>"));

        // (c) Hydration descriptor: the component island + an interpolation
        // island, with the request's state embedded.
        let h = render.hydration.expect("hydration descriptor");
        assert_eq!(h.component_id, "app.tsx");
        assert_eq!(h.islands[0].kind, IslandKind::Component);
        assert!(h.islands.iter().any(|i| i.kind == IslandKind::Interpolation));
        assert!(h.has_state);
        assert_eq!(h.data.get("title"), Some(&json!("Post hello")));
    }

    #[test]
    fn unmatched_request_renders_a_404() {
        let table = SsrRouteTable::new(vec![route("/about")]);
        let handler = SsrHandler::with_seams(table, FnSeam(|_, i| i.clone()), StubExecutor, "en");
        let render = handler.render_request(&SsrRequest::new("/missing"));
        assert_eq!(render.response.status, 404);
        assert!(render.hydration.is_none());
    }

    #[test]
    fn server_fn_results_are_injected_into_the_macro_input() {
        // The seam asserts the server-fn result reached `input.server.<name>` and
        // surfaces it as the title.
        let seam = FnSeam(|_src: &str, input: &RenderData| {
            let name = input
                .get("server")
                .and_then(|s| s.get("loadUser"))
                .and_then(|u| u.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("none")
                .to_string();
            [("title".to_string(), json!(name))].into_iter().collect()
        });
        let mut r = route("/u");
        r.server_fns = vec![SsrServerFn { name: "loadUser".to_string(), ..Default::default() }];
        let table = SsrRouteTable::new(vec![r]);
        let handler = SsrHandler::with_seams(table, seam, StubExecutor, "en");

        let render = handler.render_request(&SsrRequest::new("/u"));
        assert!(render.response.body.contains("<h1>loadUser</h1>"), "body: {}", render.response.body);
    }

    #[test]
    fn macro_less_route_renders_against_request_input() {
        // No macro: the request input IS the render data. A query param surfaces
        // through `ctx` only if the template reads it, so just assert the route
        // renders a 200 document with the hydration boundary.
        let mut r = route("/static");
        r.macro_source = None;
        let table = SsrRouteTable::new(vec![r]);
        let handler = SsrHandler::with_seams(table, FnSeam(|_, i| i.clone()), StubExecutor, "en");
        let render = handler.render_request(&SsrRequest::new("/static"));
        assert_eq!(render.response.status, 200);
        assert!(render.response.body.contains("data-treaty-ssg=\"1\""));
    }
}
