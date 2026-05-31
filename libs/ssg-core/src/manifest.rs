//! Crawler-artifact + hydration-manifest emit: `sitemap.xml` and `robots.txt`
//! string-builders, plus the per-route hydration manifest assembly and its
//! deterministic JSON serialization. Ports the deterministic core of the TS
//! `sitemap.ts` and the hydration-manifest portion of `site.ts`.
//!
//! Everything here is a pure string-builder (no I/O): the generator decides
//! when and where to write the results. Output is reproducible across builds —
//! entries are emitted in the order given, slashes are deduplicated, and the
//! hydration JSON is pretty-printed identically to the TS
//! `JSON.stringify(file, null, 2)` so the emitted bytes match the shim.

use crate::types::{HydrationManifest, RouteHydration, SitemapEntry};

/// Default basename of the emitted hydration manifest. Mirrors the TS
/// `HYDRATION_MANIFEST_FILE`.
pub const HYDRATION_MANIFEST_FILE: &str = "treaty-hydration.json";

/// Options for [`build_robots`]. Mirrors the TS `RobotsOptions`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RobotsOptions {
    /// The absolute `Sitemap:` URL to advertise. `None` emits no sitemap line.
    pub sitemap_url: Option<String>,
    /// Path prefixes to disallow for all agents. Empty allows everything.
    pub disallow: Vec<String>,
}

/// Escape text for safe embedding inside XML element bodies / attributes. Ports
/// the TS `escapeXml`. The replacement order matters: `&` is escaped first so
/// the ampersands introduced by the later replacements are not double-escaped.
fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Whether `url_path` is already an absolute `http(s)://…` URL (case-insensitive
/// scheme), in which case [`absolute_url`] returns it verbatim.
fn is_absolute(url_path: &str) -> bool {
    let lower = url_path.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Join a site origin (`https://example.com`, possibly with a trailing slash)
/// with a root-relative URL path (`/about`) into one absolute,
/// deduplicated-slash location. A path already absolute (`http(s)://…`) is
/// returned verbatim. Ports the TS `absoluteUrl`.
pub fn absolute_url(origin: &str, url_path: &str) -> String {
    if is_absolute(url_path) {
        return url_path.to_string();
    }
    let base = origin.trim_end_matches('/');
    if url_path.starts_with('/') {
        format!("{base}{url_path}")
    } else {
        format!("{base}/{url_path}")
    }
}

/// Clamp a sitemap priority into the valid `[0,1]` range, defaulting a
/// non-finite value to `0.5`. Ports the TS `clampPriority`.
fn clamp_priority(priority: f64) -> f64 {
    if !priority.is_finite() {
        return 0.5;
    }
    priority.clamp(0.0, 1.0)
}

/// Format a clamped priority as the single-decimal text the TS `toFixed(1)`
/// emits (`0.7`, `1.0`).
fn format_priority(priority: f64) -> String {
    format!("{:.1}", clamp_priority(priority))
}

/// Render a sitemap XML document for `entries`, resolving each entry's `url`
/// against `origin` into an absolute `<loc>`. Output is deterministic (entries
/// in the order given) and minimal — only the optional fields actually supplied
/// are emitted. Ports the TS `buildSitemap`.
pub fn build_sitemap(origin: &str, entries: &[SitemapEntry]) -> String {
    let mut lines: Vec<String> = vec![
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>".to_string(),
        "<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">".to_string(),
    ];
    for entry in entries {
        lines.push("  <url>".to_string());
        lines.push(format!("    <loc>{}</loc>", escape_xml(&absolute_url(origin, &entry.url))));
        if let Some(lastmod) = &entry.lastmod {
            lines.push(format!("    <lastmod>{}</lastmod>", escape_xml(lastmod)));
        }
        if let Some(changefreq) = entry.changefreq {
            lines.push(format!("    <changefreq>{}</changefreq>", changefreq.as_str()));
        }
        if let Some(priority) = entry.priority {
            lines.push(format!("    <priority>{}</priority>", format_priority(priority)));
        }
        lines.push("  </url>".to_string());
    }
    lines.push("</urlset>".to_string());
    format!("{}\n", lines.join("\n"))
}

/// Render a `robots.txt` body: a single `User-agent: *` group that allows
/// crawling (with any `disallow` prefixes, each normalized to a leading slash),
/// optionally followed by a blank line and a `Sitemap:` line. Ports the TS
/// `buildRobots`.
pub fn build_robots(options: &RobotsOptions) -> String {
    let mut lines: Vec<String> = vec!["User-agent: *".to_string()];
    if options.disallow.is_empty() {
        lines.push("Allow: /".to_string());
    } else {
        for prefix in &options.disallow {
            let normalized =
                if prefix.starts_with('/') { prefix.clone() } else { format!("/{prefix}") };
            lines.push(format!("Disallow: {normalized}"));
        }
    }
    if let Some(sitemap_url) = &options.sitemap_url {
        lines.push(String::new());
        lines.push(format!("Sitemap: {sitemap_url}"));
    }
    format!("{}\n", lines.join("\n"))
}

/// Assemble the v1 [`HydrationManifest`] from the per-route descriptors, in
/// discovery order. Ports the hydration-manifest assembly in the TS `site.ts`
/// (`{ version: 1, routes }`).
pub fn build_hydration_manifest(routes: Vec<RouteHydration>) -> HydrationManifest {
    HydrationManifest::new(routes)
}

/// Build the per-route [`RouteHydration`] descriptor the generator pushes for a
/// prerendered route. `has_state` mirrors the TS `Object.keys(result.data)
/// .length > 0`: a route embedded serialized render state iff its render data is
/// non-empty.
pub fn route_hydration(
    url: impl Into<String>,
    output: impl Into<String>,
    islands: Vec<crate::types::HydrationIsland>,
    data_is_empty: bool,
) -> RouteHydration {
    RouteHydration { url: url.into(), output: output.into(), islands, has_state: !data_is_empty }
}

/// Serialize a [`HydrationManifest`] to the exact on-disk JSON bytes the TS shim
/// emits: pretty-printed with two-space indentation (matching
/// `JSON.stringify(file, null, 2)`) and a single trailing newline. Returned as a
/// `String` so the caller owns the write; the pure core never touches disk.
pub fn hydration_manifest_json(manifest: &HydrationManifest) -> String {
    let body = serde_json::to_string_pretty(manifest)
        .expect("HydrationManifest is always serializable");
    format!("{body}\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{HydrationIsland, IslandKind};

    fn island(kind: IslandKind, id: &str) -> HydrationIsland {
        HydrationIsland { kind, id: id.to_string() }
    }

    #[test]
    fn route_hydration_entry_has_expected_shape() {
        let entry = route_hydration(
            "/blog/hello",
            "dist/ssg/blog/hello/index.html",
            vec![island(IslandKind::Component, "blog.tsx")],
            /* data_is_empty */ false,
        );
        assert_eq!(entry.url, "/blog/hello");
        assert_eq!(entry.output, "dist/ssg/blog/hello/index.html");
        assert_eq!(entry.islands.len(), 1);
        assert_eq!(entry.islands[0].kind, IslandKind::Component);
        assert_eq!(entry.islands[0].id, "blog.tsx");
        // Non-empty render data ⇒ state was embedded for client reuse.
        assert!(entry.has_state);
    }

    #[test]
    fn route_hydration_has_state_tracks_empty_render_data() {
        let with_data = route_hydration("/", "dist/ssg/index.html", Vec::new(), false);
        let without_data = route_hydration("/", "dist/ssg/index.html", Vec::new(), true);
        assert!(with_data.has_state);
        assert!(!without_data.has_state);
    }

    #[test]
    fn build_hydration_manifest_is_v1_and_preserves_route_order() {
        let routes = vec![
            route_hydration("/", "dist/ssg/index.html", Vec::new(), true),
            route_hydration(
                "/about",
                "dist/ssg/about/index.html",
                vec![island(IslandKind::Component, "about.tsx")],
                false,
            ),
        ];
        let manifest = build_hydration_manifest(routes);
        assert_eq!(manifest.version, 1);
        assert_eq!(manifest.routes.len(), 2);
        assert_eq!(manifest.routes[0].url, "/");
        assert_eq!(manifest.routes[1].url, "/about");
    }

    #[test]
    fn hydration_manifest_json_is_pretty_with_trailing_newline() {
        let manifest = build_hydration_manifest(vec![route_hydration(
            "/blog/hello",
            "dist/ssg/blog/hello/index.html",
            vec![
                island(IslandKind::Component, "blog.tsx"),
                island(IslandKind::Interpolation, "blog.tsx#0"),
            ],
            false,
        )]);
        let json = hydration_manifest_json(&manifest);

        // Pretty-printed (two-space indent), v1, with the route entry fields.
        assert!(json.contains("\"version\": 1"));
        assert!(json.contains("\"url\": \"/blog/hello\""));
        assert!(json.contains("\"output\": \"dist/ssg/blog/hello/index.html\""));
        assert!(json.contains("\"hasState\": true") || json.contains("\"has_state\": true"));
        assert!(json.contains("\"kind\": \"component\""));
        assert!(json.contains("\"kind\": \"interpolation\""));
        assert!(json.contains("\n  \"version\""), "expected two-space indentation");
        // Exactly one trailing newline, matching `JSON.stringify(...) + '\\n'`.
        assert!(json.ends_with("}\n"));
        assert!(!json.ends_with("\n\n"));

        // Round-trips back to an identical manifest.
        let back: HydrationManifest = serde_json::from_str(json.trim_end()).expect("deserialize");
        assert_eq!(back, manifest);
    }

    #[test]
    fn absolute_url_dedupes_slashes_and_passes_absolute_through() {
        assert_eq!(absolute_url("https://example.com/", "/about"), "https://example.com/about");
        assert_eq!(absolute_url("https://example.com", "about"), "https://example.com/about");
        assert_eq!(absolute_url("https://example.com", "/"), "https://example.com/");
        // Already-absolute paths are returned verbatim (case-insensitive scheme).
        assert_eq!(
            absolute_url("https://example.com", "https://cdn.example.com/x"),
            "https://cdn.example.com/x"
        );
        assert_eq!(
            absolute_url("https://example.com", "HTTPS://cdn.example.com/x"),
            "HTTPS://cdn.example.com/x"
        );
    }

    #[test]
    fn build_sitemap_emits_minimal_deterministic_xml() {
        let entries = vec![
            SitemapEntry::new("/"),
            SitemapEntry {
                url: "/blog/a&b".to_string(),
                lastmod: Some("2026-05-31".to_string()),
                changefreq: Some(crate::types::ChangeFreq::Weekly),
                priority: Some(0.73),
            },
        ];
        let xml = build_sitemap("https://example.com", &entries);
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"));
        assert!(xml.contains("<loc>https://example.com/</loc>"));
        // XML-escaped ampersand and clamped, single-decimal priority.
        assert!(xml.contains("<loc>https://example.com/blog/a&amp;b</loc>"));
        assert!(xml.contains("<lastmod>2026-05-31</lastmod>"));
        assert!(xml.contains("<changefreq>weekly</changefreq>"));
        assert!(xml.contains("<priority>0.7</priority>"));
        assert!(xml.ends_with("</urlset>\n"));
        // The bare entry emits only <loc> (no optional fields).
        let bare = build_sitemap("", &[SitemapEntry::new("/x")]);
        assert!(!bare.contains("<lastmod>"));
        assert!(!bare.contains("<changefreq>"));
        assert!(!bare.contains("<priority>"));
    }

    #[test]
    fn build_robots_allows_by_default_and_advertises_sitemap() {
        let allow_all = build_robots(&RobotsOptions::default());
        assert_eq!(allow_all, "User-agent: *\nAllow: /\n");

        let with_disallow = build_robots(&RobotsOptions {
            sitemap_url: Some("https://example.com/sitemap.xml".to_string()),
            disallow: vec!["/admin".to_string(), "draft".to_string()],
        });
        assert!(with_disallow.contains("Disallow: /admin\n"));
        // A prefix without a leading slash is normalized.
        assert!(with_disallow.contains("Disallow: /draft\n"));
        assert!(!with_disallow.contains("Allow: /"));
        assert!(with_disallow.ends_with("\nSitemap: https://example.com/sitemap.xml\n"));
    }
}
