//! `treaty_ssr` — request-time server-side rendering for Treaty.
//!
//! The static SSG core (`treaty_ssg`) bakes each discovered route to a
//! hydration-ready document ONCE at build time. This crate is its request-time
//! sibling: it renders one route PER HTTP request, injecting that request's data
//! (route params / query / headers / cookies + server-fn results), interpreting
//! the route's emitted `dist/server` Ivy template to HTML, and wrapping it with
//! the SAME hydration boundary the SSG core emits — so the client runtime
//! hydrates a request render exactly as it hydrates a prerender.
//!
//! It REUSES the done foundations rather than rebuilding them:
//!   * [`treaty_ssg`] supplies the Ivy->HTML interpreter (`ivy_html`), the
//!     head/SEO + document-wrap (`seo`), and the hydration manifest shapes
//!     ([`RenderData`], [`treaty_ssg::HydrationIsland`], the `RenderSeam` trait).
//!   * [`treaty_runtime`] supplies the Nova-based `run_macro` / `run_server_fn`
//!     execution used to resolve a route's server data per request.
//!
//! The one entry point is [`SsrHandler::render_request`]: a route table + the
//! injected execution seams in, an [`SsrRender`] (HTTP response + hydration
//! descriptor) out. Every behavioural boundary is injected — the render seam
//! ([`seam::NovaRenderSeam`]) and the server-fn executor
//! ([`server_fn::NovaServerFnExecutor`]) — so the pipeline is unit-tested with
//! deterministic fakes and wired to Nova in production via [`SsrHandler::nova`].

pub(crate) mod escape;
pub mod handler;
pub mod hydration;
pub mod request;
pub mod route;
pub mod seam;
pub mod server_fn;

// Re-export the `treaty_ssg` render-data type + hydration island so callers of
// this crate need not also name `treaty_ssg` for the common shapes. The render
// pipeline is the same one the SSG core uses; only the data source (a live
// request vs a build-time discovery) differs.
pub use treaty_ssg::{HydrationIsland, IslandKind, RenderData, RenderSeam};

pub use handler::{SsrHandler, SsrRender};
pub use hydration::RequestHydration;
pub use request::{SsrRequest, SsrResponse};
pub use route::{MatchedRoute, SsrRoute, SsrRouteTable};
pub use seam::NovaRenderSeam;
pub use server_fn::{
    load_server_data, NovaServerFnExecutor, ServerFnExecutor, SsrServerFn,
};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// End-to-end smoke through the PUBLIC surface using the production Nova
    /// seams: a parameterized route whose render-time macro reads the request's
    /// bound `:slug` and a server-fn result, renders the title into the document,
    /// and emits a hydration descriptor carrying the request state. This proves
    /// the full request path (route match -> request injection -> Nova server fn
    /// -> Nova macro -> Ivy interpret -> hydration wrap) works against the REAL
    /// runtime, not just fakes.
    #[test]
    fn nova_handler_renders_and_carries_hydration_state() {
        let route = SsrRoute {
            pattern: "/blog/:slug".to_string(),
            ivy_code: r#"
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
            "#
            .to_string(),
            component_id: "blog.tsx".to_string(),
            // The macro reads the bound slug AND the server-fn result, proving
            // both request injection and server-side data loading reach it.
            macro_source: Some(
                "export default { title: input.server.author + ': ' + input.params.slug }"
                    .to_string(),
            ),
            server_fns: vec![SsrServerFn {
                name: "author".to_string(),
                source: "return 'Ada';".to_string(),
                args: json!([]),
            }],
        };
        let handler = SsrHandler::nova(SsrRouteTable::new(vec![route]));

        let render = handler.render_request(&SsrRequest::new("/blog/hello-world"));
        let resp = &render.response;
        assert_eq!(resp.status, 200);
        // Real HTML: the Nova-run macro composed the title from the server-fn
        // result + the URL-bound slug, and the Ivy interpreter rendered it.
        assert!(
            resp.body.contains("<h1>Ada: hello-world</h1>"),
            "expected rendered title in body, got: {}",
            resp.body
        );
        // The hydration boundary is present and carries the request's state.
        assert!(resp.body.contains("data-treaty-ssg=\"1\""));
        let h = render.hydration.expect("hydration");
        assert!(h.has_state);
        assert_eq!(h.data.get("title"), Some(&json!("Ada: hello-world")));
        assert_eq!(h.component_id, "blog.tsx");
    }

    #[test]
    fn nova_handler_404s_unmatched() {
        let handler = SsrHandler::nova(SsrRouteTable::new(vec![SsrRoute::new("/", "")]));
        let render = handler.render_request(&SsrRequest::new("/nope"));
        assert_eq!(render.response.status, 404);
        assert!(render.hydration.is_none());
    }
}
