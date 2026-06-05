//! Minimal HTML text escaping for the bits of a response this crate builds
//! itself (the 404 body, the hydration-boundary wrappers). The main document is
//! escaped by `treaty_ssg`'s `seo`/`ivy_html`; this is only for the small amount
//! of markup the request path adds around it.

/// Escape `&`, `<`, `>` in element-body text. Matches `treaty_ssg`'s
/// `escape_html` (same order, so request-built markup escapes identically to the
/// SSG-built document it wraps).
pub fn escape_html(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_the_three_html_chars_in_order() {
        assert_eq!(escape_html("a & b < c > d"), "a &amp; b &lt; c &gt; d");
        // `&` first so it does not double-escape the `&` of `&lt;`.
        assert_eq!(escape_html("<&>"), "&lt;&amp;&gt;");
    }
}
