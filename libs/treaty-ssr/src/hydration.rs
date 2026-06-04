//! Request-time hydration wiring: turn a request-rendered route into the
//! hydration boundary the client runtime boots against, and into a per-request
//! hydration-manifest entry that extends the SSG manifest with the request data.
//!
//! The SSG core already marks hydration islands (`treaty_ssg::ivy_html::detect_islands`),
//! embeds serialized render state, and wraps the root in the
//! `data-treaty-ssg`-marked mount (`treaty_ssg::seo::wrap_document`). Request-time
//! SSR REUSES all of that — a request render must hydrate exactly as a prerender
//! — and adds the one thing a request render needs that a build render does not:
//! the manifest entry carries the REQUEST'S render data, because the islands the
//! client boots were filled from this request, not from a build-time snapshot.

use serde::{Deserialize, Serialize};

use treaty_ssg::{HydrationIsland, RenderData};

/// The hydration descriptor for ONE request-rendered route — the request-time
/// analogue of `treaty_ssg::RouteHydration`, extended with the request's render
/// data so the client boots the islands against the exact state the server used.
///
/// A static (build) hydration entry references state embedded once in the
/// prerendered file. A request entry instead carries the per-request `data`
/// inline (the server filled the islands from THIS request), so a host that
/// serves request renders can hand the client the matching state without a
/// separate manifest fetch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestHydration {
    /// The request URL this render was produced for.
    pub url: String,
    /// The component id whose islands were rendered (matches the build manifest's
    /// island ids so the client uses one boot path for build + request renders).
    pub component_id: String,
    /// The hydration islands present in this render (root component + one per
    /// interpolation the server filled).
    pub islands: Vec<HydrationIsland>,
    /// Whether this render embedded serialized state (non-empty render data).
    pub has_state: bool,
    /// The per-request render data the islands were filled from — the state the
    /// client hydrates against. Empty for a purely static route.
    pub data: RenderData,
}

impl RequestHydration {
    /// Build the descriptor for a request render. `has_state` mirrors the SSG
    /// rule: a route embedded state iff its render data is non-empty.
    pub fn new(
        url: impl Into<String>,
        component_id: impl Into<String>,
        islands: Vec<HydrationIsland>,
        data: RenderData,
    ) -> Self {
        let has_state = !data.is_empty();
        Self { url: url.into(), component_id: component_id.into(), islands, has_state, data }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;
    use treaty_ssg::IslandKind;

    fn island(kind: IslandKind, id: &str) -> HydrationIsland {
        HydrationIsland { kind, id: id.to_string() }
    }

    #[test]
    fn request_hydration_tracks_state_from_data() {
        let mut data: RenderData = BTreeMap::new();
        data.insert("title".to_string(), json!("Hi"));
        let h = RequestHydration::new(
            "/blog/x",
            "blog.tsx",
            vec![island(IslandKind::Component, "blog.tsx")],
            data,
        );
        assert!(h.has_state);
        assert_eq!(h.url, "/blog/x");
        assert_eq!(h.component_id, "blog.tsx");
        assert_eq!(h.data.get("title"), Some(&json!("Hi")));
    }

    #[test]
    fn empty_data_means_no_state() {
        let h = RequestHydration::new("/", "app.tsx", Vec::new(), BTreeMap::new());
        assert!(!h.has_state);
    }

    #[test]
    fn round_trips_through_serde() {
        let h = RequestHydration::new("/", "app.tsx", Vec::new(), BTreeMap::new());
        let text = serde_json::to_string(&h).expect("serialize");
        let back: RequestHydration = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(h, back);
    }
}
