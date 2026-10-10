//! Port of Angular `schema/dom_security_schema.ts` (`SECURITY_SCHEMA` / `checkSecurityContext`).
//!
//! Maps a property or attribute name, on a tag in a namespace, to the sanitization context the
//! binding needs. The schema is namespace-aware: the first level is the lowercase property or
//! attribute name, the second the namespace (`""` for HTML, `svg`, `math`), the third the lowercase
//! tag name or `*` for every element.
//!
//! PORT NOTE: `ATTRIBUTE_NO_BINDING` (the `validateAttribute` context for SVG animation attributes
//! and `iframe`/`unknown` sandbox-style attributes) is a `core.SecurityContext` value that the
//! `treaty_ivy_core` enum does not carry yet, so those entries resolve to [`SecurityContext::None`]
//! here. Adding the variant to `core` (and mapping it to `ɵɵvalidateAttribute`) is required for
//! full parity; see the stage-2 report.

use crate::expression::ast::SecurityContext;
use std::collections::HashMap;
use std::sync::OnceLock;

/// `SVG_NAMESPACE` (`template/pipeline/src/namespaces.ts`).
pub const SVG_NAMESPACE: &str = "svg";
/// `MATH_ML_NAMESPACE` (`template/pipeline/src/namespaces.ts`).
pub const MATH_ML_NAMESPACE: &str = "math";
/// The default (HTML) namespace key of the schema.
const NO_NAMESPACE: &str = "";
/// The wildcard tag key that matches every element.
const MATCH_ALL_ELEMENTS: &str = "*";

/// One `registerContext` table row: `(tag, attribute names)`.
type Spec = (&'static str, &'static [&'static str]);

/// `registerContext(ctx, namespace, specs)` data, transcribed in the same order as Angular
/// `SECURITY_SCHEMA()`. Later registrations overwrite earlier ones for the same
/// (attribute, namespace, tag) triple, exactly as `registerContext` does.
const REGISTRATIONS: &[(SecurityContext, &str, &[Spec])] = &[
    (
        SecurityContext::Html,
        NO_NAMESPACE,
        &[("iframe", &["srcdoc"]), ("*", &["innerHTML", "outerHTML"])],
    ),
    (SecurityContext::Style, NO_NAMESPACE, &[("*", &["style"])]),
    (
        SecurityContext::Url,
        NO_NAMESPACE,
        &[
            ("*", &["formAction"]),
            ("area", &["href"]),
            ("a", &["href", "xlink:href"]),
            ("form", &["action"]),
        ],
    ),
    (
        SecurityContext::Url,
        MATH_ML_NAMESPACE,
        &[("*", &["href", "xlink:href"])],
    ),
    (
        SecurityContext::ResourceUrl,
        NO_NAMESPACE,
        &[
            ("base", &["href"]),
            ("embed", &["src"]),
            ("frame", &["src"]),
            ("iframe", &["src"]),
            ("link", &["href"]),
            ("object", &["codebase", "data"]),
        ],
    ),
    (
        SecurityContext::Url,
        SVG_NAMESPACE,
        &[("a", &["href", "xlink:href"])],
    ),
];

/// Schema shape: `attribute (lowercase) -> namespace -> tag (lowercase or "*") -> context`.
type Schema = HashMap<&'static str, HashMap<&'static str, HashMap<&'static str, SecurityContext>>>;

/// Lazily-built schema (`SECURITY_SCHEMA()` is also memoised in Angular).
fn schema() -> &'static Schema {
    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        let mut out: Schema = HashMap::new();
        for (ctx, ns, specs) in REGISTRATIONS {
            for (tag, attrs) in specs.iter() {
                for attr in attrs.iter() {
                    out.entry(attr_key(attr))
                        .or_default()
                        .entry(ns)
                        .or_default()
                        .insert(tag, ctx.clone());
                }
            }
        }
        out
    })
}

/// Attribute keys are lowercase in the schema (`registerContext` lowercases every name).
fn attr_key(attr: &'static str) -> &'static str {
    // The table is written in lowercase-compatible spelling already except `innerHTML`,
    // `outerHTML`, `formAction`, `xlink:href`; those are lowercased at lookup time, so the
    // schema key is the lowercase form of the table entry.
    match attr {
        "innerHTML" => "innerhtml",
        "outerHTML" => "outerhtml",
        "formAction" => "formaction",
        other => other,
    }
}

/// `checkSecurityContext(tagName, propName, namespace)`: the sanitization context for a property or
/// attribute on the element `tag_name` (local name) in `namespace` (`None`/`""` for HTML).
///
/// Lookup order (faithful to Angular): the element's own namespace, then the HTML namespace, then
/// (for a namespace-less animation element outside an `<svg>`) the SVG namespace, falling back to
/// the `*` wildcard tag at each namespace level. Unknown attributes are [`SecurityContext::None`].
pub fn check_security_context(
    tag_name: &str,
    prop_name: &str,
    namespace: Option<&str>,
) -> SecurityContext {
    let schema = schema();
    let Some(attr_schema) = schema.get(prop_name.to_lowercase().as_str()) else {
        return SecurityContext::None;
    };
    let tag_lower = tag_name.to_lowercase();
    let mut context: Option<SecurityContext> = None;

    if let Some(ns) = namespace.filter(|n| !n.is_empty()) {
        if let Some(ns_schema) = attr_schema.get(ns) {
            context = ns_schema
                .get(tag_lower.as_str())
                .or_else(|| ns_schema.get(MATCH_ALL_ELEMENTS))
                .cloned();
        }
    }

    if context.is_none() {
        if let Some(default_schema) = attr_schema.get(NO_NAMESPACE) {
            context = default_schema
                .get(tag_lower.as_str())
                .or_else(|| default_schema.get(MATCH_ALL_ELEMENTS))
                .cloned();
        }
    }

    // An SVG animation element declared outside of an `<svg>` has no explicit namespace at compile
    // time, but can still animate once it is projected into an SVG subtree.
    if context.is_none() && namespace.map_or(true, |n| n.is_empty()) {
        if let Some(svg_schema) = attr_schema.get(SVG_NAMESPACE) {
            context = svg_schema.get(tag_lower.as_str()).cloned();
        }
    }

    context.unwrap_or(SecurityContext::None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn href_on_anchor_is_url_and_on_link_is_resource_url() {
        assert_eq!(check_security_context("a", "href", None), SecurityContext::Url);
        assert_eq!(check_security_context("link", "href", None), SecurityContext::ResourceUrl);
        assert_eq!(check_security_context("base", "href", None), SecurityContext::ResourceUrl);
    }

    /// NEW schema change: `img.src` is no longer a URL context (the old `['img', ['src']]` row was
    /// removed), so `[src]` on an image is not sanitized.
    #[test]
    fn img_src_is_not_sanitized_in_new_schema() {
        assert_eq!(check_security_context("img", "src", None), SecurityContext::None);
        assert_eq!(check_security_context("iframe", "src", None), SecurityContext::ResourceUrl);
        assert_eq!(check_security_context("object", "data", None), SecurityContext::ResourceUrl);
    }

    #[test]
    fn inner_html_is_html_on_any_element_and_srcdoc_only_on_iframe() {
        assert_eq!(check_security_context("div", "innerHTML", None), SecurityContext::Html);
        assert_eq!(check_security_context("div", "innerhtml", None), SecurityContext::Html);
        assert_eq!(check_security_context("iframe", "srcdoc", None), SecurityContext::Html);
        assert_eq!(check_security_context("div", "srcdoc", None), SecurityContext::None);
    }

    #[test]
    fn form_action_is_url_everywhere_and_form_action_attr_on_form_only() {
        assert_eq!(check_security_context("button", "formAction", None), SecurityContext::Url);
        assert_eq!(check_security_context("form", "action", None), SecurityContext::Url);
        assert_eq!(check_security_context("div", "action", None), SecurityContext::None);
    }

    #[test]
    fn style_is_style_on_every_element() {
        assert_eq!(check_security_context("span", "style", None), SecurityContext::Style);
    }

    /// `:svg:a` resolves through the SVG namespace; the HTML `a` table does not apply to the
    /// namespaced element's own namespace lookup but `xlink:href` is still URL there.
    #[test]
    fn namespaced_elements_use_their_namespace_table() {
        assert_eq!(
            check_security_context("a", "href", Some(SVG_NAMESPACE)),
            SecurityContext::Url
        );
        assert_eq!(
            check_security_context("a", "xlink:href", Some(SVG_NAMESPACE)),
            SecurityContext::Url
        );
        // MathML `*` href is URL on any MathML element.
        assert_eq!(
            check_security_context("mi", "href", Some(MATH_ML_NAMESPACE)),
            SecurityContext::Url
        );
    }

    #[test]
    fn unknown_property_is_none() {
        assert_eq!(check_security_context("div", "title", None), SecurityContext::None);
    }
}
