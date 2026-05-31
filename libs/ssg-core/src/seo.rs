//! Head / SEO emit, plus the two static discovery artifacts: resolve the
//! effective [`HeadMeta`] for a route (caller overrides winning over
//! conventional render-data keys), render the `<head>` SEO/meta tags (title,
//! description, canonical, Open Graph / meta, link), wrap a rendered fragment
//! in a full, hydration-ready HTML document, and build `sitemap.xml` +
//! `robots.txt`.
//!
//! Ports the head subset of the TS `prerender.ts` (`resolveHead`, `renderHead`,
//! `wrapDocument`) and the whole of the TS `sitemap.ts` (`buildSitemap`,
//! `buildRobots`, `absoluteUrl`). Everything here is a pure string builder with
//! no I/O, so the caller owns when/where to write the output.

use crate::types::{HeadMeta, RenderData, SitemapEntry};
use serde_json::Value;

/// The marker a hydrating client runtime keys off to take over a prerender.
pub const HYDRATION_MARKER_ATTR: &str = "data-treaty-ssg";

/// The element id under which serialized render state is embedded for hydration.
pub const HYDRATION_STATE_ID: &str = "__TREATY_SSG_STATE__";

/// Escape text for embedding inside an HTML element body. Mirrors the TS
/// `escapeHtml`: only `&`, `<`, `>` (in that order).
fn escape_html(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Escape an HTML attribute value for safe double-quoted output. Mirrors the TS
/// `escapeAttr`: `&` then `"`.
fn escape_attr(value: &str) -> String {
    value.replace('&', "&amp;").replace('"', "&quot;")
}

/// Escape text for safe embedding inside XML element bodies / attributes.
/// Mirrors the TS `escapeXml`: `&`, `<`, `>`, `"`, `'` (in that order).
fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Whether a meta key uses the Open Graph `property=` convention. Mirrors the TS
/// `isPropertyMeta` (`og:` / `article:` / `fb:` prefixes).
fn is_property_meta(name: &str) -> bool {
    name.starts_with("og:") || name.starts_with("article:") || name.starts_with("fb:")
}

/// Derive the effective [`HeadMeta`] for a route: caller-supplied `head` fields
/// win, falling back to conventional keys in the render `data` (`description`,
/// `canonical`) and the resolved document `title`/`lang`. This is what lets a
/// render-time macro drive SEO simply by returning those keys. Ports the TS
/// `resolveHead`.
pub fn resolve_head(head: Option<&HeadMeta>, data: &RenderData, title: &str, lang: &str) -> HeadMeta {
    // A conventional render-data key resolves to its value only when it is a
    // JSON string (mirrors the TS `typeof value === 'string'` guard).
    let from_data = |key: &str| -> Option<String> {
        match data.get(key) {
            Some(Value::String(s)) => Some(s.clone()),
            _ => None,
        }
    };
    HeadMeta {
        title: Some(head.and_then(|h| h.title.clone()).unwrap_or_else(|| title.to_string())),
        lang: Some(head.and_then(|h| h.lang.clone()).unwrap_or_else(|| lang.to_string())),
        description: head.and_then(|h| h.description.clone()).or_else(|| from_data("description")),
        canonical: head.and_then(|h| h.canonical.clone()).or_else(|| from_data("canonical")),
        meta: head.map(|h| h.meta.clone()).unwrap_or_default(),
        links: head.map(|h| h.links.clone()).unwrap_or_default(),
    }
}

/// Render the `<head>` SEO/meta tags for a document from [`HeadMeta`] (charset,
/// viewport, title, description, canonical, Open Graph / meta, link tags). Each
/// emitted tag is on its own line, terminated by a newline. Ports the TS
/// `renderHead`.
pub fn render_head(head: &HeadMeta, title: &str) -> String {
    let mut lines: Vec<String> = vec![
        "<meta charset=\"utf-8\">".to_string(),
        "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">".to_string(),
        format!("<title>{}</title>", escape_html(title)),
    ];
    if let Some(description) = &head.description {
        lines.push(format!("<meta name=\"description\" content=\"{}\">", escape_attr(description)));
    }
    if let Some(canonical) = &head.canonical {
        lines.push(format!("<link rel=\"canonical\" href=\"{}\">", escape_attr(canonical)));
    }
    for (name, content) in &head.meta {
        let attr = if is_property_meta(name) { "property" } else { "name" };
        lines.push(format!(
            "<meta {attr}=\"{}\" content=\"{}\">",
            escape_attr(name),
            escape_attr(content)
        ));
    }
    for (rel, href) in &head.links {
        lines.push(format!("<link rel=\"{}\" href=\"{}\">", escape_attr(rel), escape_attr(href)));
    }
    let mut out = String::new();
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// Serialize the render `data` for embedding in a non-executable JSON script,
/// HTML-escaping it and neutralizing any `</script` so it cannot break out of
/// the embedding `<script>`. Mirrors the TS `wrapDocument` state encoding.
fn encode_state(data: &RenderData) -> String {
    let json = serde_json::to_string(data).unwrap_or_else(|_| "{}".to_string());
    let escaped = escape_html(&json);
    // Case-insensitively neutralize the `</script` close sequence, preserving
    // the original case of the matched text (mirrors the TS `/<\/script/gi`).
    neutralize_script_close(&escaped)
}

/// Replace every case-insensitive occurrence of `</script` with `<\/script`,
/// preserving the matched text's original casing. Mirrors the TS
/// `.replace(/<\/script/gi, '<\\/script')`.
fn neutralize_script_close(value: &str) -> String {
    const NEEDLE: &str = "</script";
    let lower = value.to_ascii_lowercase();
    let mut out = String::with_capacity(value.len());
    let mut start = 0;
    while let Some(rel) = lower[start..].find(NEEDLE) {
        let at = start + rel;
        out.push_str(&value[start..at]);
        // Preserve original casing: insert a backslash after the `<`, keeping
        // the rest of the matched slice verbatim (`</scrIpt` -> `<\/scrIpt`).
        out.push('<');
        out.push('\\');
        out.push_str(&value[at + 1..at + NEEDLE.len()]);
        start = at + NEEDLE.len();
    }
    out.push_str(&value[start..]);
    out
}

/// Wrap a rendered HTML fragment in a full, hydration-ready HTML document: the
/// `<head>` from `head`, the root mount carrying [`HYDRATION_MARKER_ATTR`], and
/// the serialized render `data` embedded under [`HYDRATION_STATE_ID`]. Ports the
/// TS `wrapDocument`.
pub fn wrap_document(fragment: &str, data: &RenderData, head: &HeadMeta) -> String {
    let state = encode_state(data);
    let lang = head.lang.clone().unwrap_or_else(|| "en".to_string());
    let title = head.title.clone().unwrap_or_default();
    format!(
        "<!doctype html>\n\
         <html lang=\"{lang}\">\n\
         <head>\n\
         {head_tags}\
         </head>\n\
         <body>\n\
         <app-root {marker}=\"1\">{fragment}</app-root>\n\
         <script type=\"application/json\" id=\"{state_id}\">{state}</script>\n\
         </body>\n\
         </html>\n",
        lang = escape_attr(&lang),
        head_tags = render_head(head, &title),
        marker = HYDRATION_MARKER_ATTR,
        fragment = fragment,
        state_id = HYDRATION_STATE_ID,
        state = state,
    )
}

/// Join a site origin (`https://example.com`, possibly trailing-slashed) with a
/// root-relative URL path (`/about`) into one absolute, deduplicated-slash
/// location. A path already absolute (`http(s)://…`) is returned verbatim.
/// Ports the TS `absoluteUrl`.
pub fn absolute_url(origin: &str, url_path: &str) -> String {
    let lower = url_path.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return url_path.to_string();
    }
    let base = origin.trim_end_matches('/');
    if url_path.starts_with('/') {
        format!("{base}{url_path}")
    } else {
        format!("{base}/{url_path}")
    }
}

/// Clamp a sitemap priority into the valid `[0,1]` range; non-finite values fall
/// back to `0.5`. Mirrors the TS `clampPriority`.
fn clamp_priority(priority: f64) -> f64 {
    if !priority.is_finite() {
        return 0.5;
    }
    priority.clamp(0.0, 1.0)
}

/// Render a sitemap XML document for `entries`, resolving each entry's `url`
/// against `origin` into an absolute `<loc>`. Output is deterministic (entries
/// in the order given) and minimal — only the optional fields actually supplied
/// are emitted. Ports the TS `buildSitemap` (here named `generate_sitemap`).
pub fn generate_sitemap(origin: &str, entries: &[SitemapEntry]) -> String {
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
            lines.push(format!("    <priority>{:.1}</priority>", clamp_priority(priority)));
        }
        lines.push("  </url>".to_string());
    }
    lines.push("</urlset>".to_string());
    format!("{}\n", lines.join("\n"))
}

/// Options for [`generate_robots`]. Mirrors the TS `RobotsOptions`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RobotsOptions {
    /// The absolute `Sitemap:` URL to advertise. `None` emits no sitemap line
    /// (e.g. when no `origin` is configured).
    pub sitemap_url: Option<String>,
    /// Path prefixes to disallow for all agents (`["/admin", "/draft"]`). Each
    /// becomes a `Disallow:` line. Empty allows everything.
    pub disallow: Vec<String>,
}

/// Render a `robots.txt` body: a single `User-agent: *` group that allows
/// crawling (with any supplied `disallow` prefixes), optionally followed by a
/// `Sitemap:` line. Deterministic and dependency-free. Ports the TS
/// `buildRobots` (here named `generate_robots`).
pub fn generate_robots(options: &RobotsOptions) -> String {
    let mut lines: Vec<String> = vec!["User-agent: *".to_string()];
    if options.disallow.is_empty() {
        lines.push("Allow: /".to_string());
    } else {
        for prefix in &options.disallow {
            let normalized = if prefix.starts_with('/') {
                prefix.clone()
            } else {
                format!("/{prefix}")
            };
            lines.push(format!("Disallow: {normalized}"));
        }
    }
    if let Some(sitemap_url) = &options.sitemap_url {
        lines.push(String::new());
        lines.push(format!("Sitemap: {sitemap_url}"));
    }
    format!("{}\n", lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ChangeFreq;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn data(pairs: &[(&str, Value)]) -> RenderData {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    #[test]
    fn render_head_emits_baseline_charset_viewport_and_title() {
        let head = HeadMeta::default();
        let out = render_head(&head, "Home & <Away>");
        assert!(out.contains("<meta charset=\"utf-8\">\n"));
        assert!(out.contains("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n"));
        // Title body is HTML-escaped (& < >), not attribute-escaped.
        assert!(out.contains("<title>Home &amp; &lt;Away&gt;</title>\n"));
    }

    #[test]
    fn render_head_emits_description_and_canonical() {
        let head = HeadMeta {
            description: Some("A \"great\" page".to_string()),
            canonical: Some("https://example.com/x".to_string()),
            ..HeadMeta::default()
        };
        let out = render_head(&head, "T");
        assert!(out.contains("<meta name=\"description\" content=\"A &quot;great&quot; page\">\n"));
        assert!(out.contains("<link rel=\"canonical\" href=\"https://example.com/x\">\n"));
    }

    #[test]
    fn render_head_uses_property_for_open_graph_and_name_otherwise() {
        let mut meta = BTreeMap::new();
        meta.insert("og:title".to_string(), "OG".to_string());
        meta.insert("article:author".to_string(), "Me".to_string());
        meta.insert("fb:app_id".to_string(), "123".to_string());
        meta.insert("twitter:card".to_string(), "summary".to_string());
        let head = HeadMeta { meta, ..HeadMeta::default() };
        let out = render_head(&head, "T");
        assert!(out.contains("<meta property=\"og:title\" content=\"OG\">\n"));
        assert!(out.contains("<meta property=\"article:author\" content=\"Me\">\n"));
        assert!(out.contains("<meta property=\"fb:app_id\" content=\"123\">\n"));
        assert!(out.contains("<meta name=\"twitter:card\" content=\"summary\">\n"));
    }

    #[test]
    fn render_head_emits_link_tags() {
        let mut links = BTreeMap::new();
        links.insert("icon".to_string(), "/favicon.ico".to_string());
        let head = HeadMeta { links, ..HeadMeta::default() };
        let out = render_head(&head, "T");
        assert!(out.contains("<link rel=\"icon\" href=\"/favicon.ico\">\n"));
    }

    #[test]
    fn resolve_head_prefers_caller_then_data_then_defaults() {
        // Caller override wins for title; description falls back to render data.
        let caller = HeadMeta { title: Some("Override".to_string()), ..HeadMeta::default() };
        let d = data(&[
            ("description", json!("from data")),
            ("canonical", json!("https://e.com/c")),
        ]);
        let resolved = resolve_head(Some(&caller), &d, "Fallback Title", "en");
        assert_eq!(resolved.title.as_deref(), Some("Override"));
        assert_eq!(resolved.lang.as_deref(), Some("en"));
        assert_eq!(resolved.description.as_deref(), Some("from data"));
        assert_eq!(resolved.canonical.as_deref(), Some("https://e.com/c"));
    }

    #[test]
    fn resolve_head_falls_back_to_title_and_lang_when_no_caller() {
        let resolved = resolve_head(None, &RenderData::new(), "The Title", "fr");
        assert_eq!(resolved.title.as_deref(), Some("The Title"));
        assert_eq!(resolved.lang.as_deref(), Some("fr"));
        assert!(resolved.description.is_none());
        assert!(resolved.canonical.is_none());
    }

    #[test]
    fn resolve_head_ignores_non_string_data_keys() {
        let d = data(&[("description", json!(42)), ("canonical", json!(true))]);
        let resolved = resolve_head(None, &d, "T", "en");
        assert!(resolved.description.is_none());
        assert!(resolved.canonical.is_none());
    }

    #[test]
    fn wrap_document_embeds_marker_fragment_and_state() {
        let d = data(&[("k", json!("v"))]);
        let head = resolve_head(None, &d, "Title", "en");
        let doc = wrap_document("<p>hi</p>", &d, &head);
        assert!(doc.starts_with("<!doctype html>\n<html lang=\"en\">\n"));
        assert!(doc.contains(&format!("<app-root {HYDRATION_MARKER_ATTR}=\"1\"><p>hi</p></app-root>\n")));
        assert!(doc.contains(&format!(
            "<script type=\"application/json\" id=\"{HYDRATION_STATE_ID}\">"
        )));
        assert!(doc.contains("<title>Title</title>"));
        assert!(doc.ends_with("</body>\n</html>\n"));
    }

    #[test]
    fn wrap_document_neutralizes_script_close_in_state() {
        let d = data(&[("x", json!("</script><script>alert(1)</SCRIPT>"))]);
        let head = HeadMeta::default();
        let doc = wrap_document("", &d, &head);
        // The payload's `<` is HTML-escaped to `&lt;` (mirroring the TS
        // `escapeHtml` applied before the `</script` replace), so the embedded
        // JSON can never break out of the state script element. The only raw
        // `</script` left is the live closing tag of the state element itself.
        let lower = doc.to_ascii_lowercase();
        assert_eq!(lower.matches("</script").count(), 1);
        assert!(doc.contains("&lt;/script&gt;&lt;script&gt;"));
    }

    #[test]
    fn absolute_url_joins_origin_and_path_deduping_slashes() {
        assert_eq!(absolute_url("https://e.com/", "/about"), "https://e.com/about");
        assert_eq!(absolute_url("https://e.com", "about"), "https://e.com/about");
        assert_eq!(absolute_url("https://e.com//", "/"), "https://e.com/");
    }

    #[test]
    fn absolute_url_passes_through_already_absolute_paths() {
        assert_eq!(absolute_url("https://e.com", "https://cdn.x/y"), "https://cdn.x/y");
        assert_eq!(absolute_url("https://e.com", "HTTP://cdn.x/y"), "HTTP://cdn.x/y");
    }

    #[test]
    fn generate_sitemap_emits_urls_in_order_with_absolute_loc() {
        let entries = vec![SitemapEntry::new("/"), SitemapEntry::new("/blog/hello")];
        let xml = generate_sitemap("https://e.com", &entries);
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"));
        assert!(xml.contains("<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">"));
        let home = xml.find("<loc>https://e.com/</loc>").expect("home loc");
        let blog = xml.find("<loc>https://e.com/blog/hello</loc>").expect("blog loc");
        assert!(home < blog, "entries must be emitted in the given order");
        assert!(xml.ends_with("</urlset>\n"));
    }

    #[test]
    fn generate_sitemap_emits_only_supplied_optional_fields() {
        let entry = SitemapEntry {
            url: "/x".to_string(),
            lastmod: Some("2026-05-31".to_string()),
            changefreq: Some(ChangeFreq::Weekly),
            priority: Some(0.8),
        };
        let xml = generate_sitemap("https://e.com", &[entry]);
        assert!(xml.contains("<lastmod>2026-05-31</lastmod>"));
        assert!(xml.contains("<changefreq>weekly</changefreq>"));
        assert!(xml.contains("<priority>0.8</priority>"));

        // A bare entry emits none of the optional tags.
        let bare = generate_sitemap("https://e.com", &[SitemapEntry::new("/y")]);
        assert!(!bare.contains("<lastmod>"));
        assert!(!bare.contains("<changefreq>"));
        assert!(!bare.contains("<priority>"));
    }

    #[test]
    fn generate_sitemap_clamps_priority_and_handles_non_finite() {
        let high = SitemapEntry { priority: Some(5.0), ..SitemapEntry::new("/a") };
        let low = SitemapEntry { priority: Some(-1.0), ..SitemapEntry::new("/b") };
        let nan = SitemapEntry { priority: Some(f64::NAN), ..SitemapEntry::new("/c") };
        let xml = generate_sitemap("https://e.com", &[high, low, nan]);
        assert!(xml.contains("<priority>1.0</priority>"));
        assert!(xml.contains("<priority>0.0</priority>"));
        assert!(xml.contains("<priority>0.5</priority>"));
    }

    #[test]
    fn generate_sitemap_escapes_loc_and_lastmod() {
        let entry = SitemapEntry {
            url: "/search?q=a&b<c".to_string(),
            lastmod: Some("a&b".to_string()),
            ..SitemapEntry::new("/ignored")
        };
        let xml = generate_sitemap("https://e.com", &[entry]);
        assert!(xml.contains("<loc>https://e.com/search?q=a&amp;b&lt;c</loc>"));
        assert!(xml.contains("<lastmod>a&amp;b</lastmod>"));
    }

    #[test]
    fn generate_robots_allows_all_by_default() {
        let txt = generate_robots(&RobotsOptions::default());
        assert_eq!(txt, "User-agent: *\nAllow: /\n");
    }

    #[test]
    fn generate_robots_emits_disallow_and_normalizes_leading_slash() {
        let options = RobotsOptions {
            disallow: vec!["/admin".to_string(), "draft".to_string()],
            ..RobotsOptions::default()
        };
        let txt = generate_robots(&options);
        assert!(txt.contains("Disallow: /admin\n"));
        assert!(txt.contains("Disallow: /draft\n"));
        // With disallows present, the blanket Allow line is omitted.
        assert!(!txt.contains("Allow: /\n"));
    }

    #[test]
    fn generate_robots_advertises_sitemap_after_blank_line() {
        let options = RobotsOptions {
            sitemap_url: Some("https://e.com/sitemap.xml".to_string()),
            ..RobotsOptions::default()
        };
        let txt = generate_robots(&options);
        assert_eq!(txt, "User-agent: *\nAllow: /\n\nSitemap: https://e.com/sitemap.xml\n");
    }
}
