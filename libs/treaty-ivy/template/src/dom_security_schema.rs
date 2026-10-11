//! Port of Angular `schema/dom_security_schema.ts` (`SECURITY_SCHEMA` / `checkSecurityContext`).
//!
//! Maps a property or attribute name, on a tag in a namespace, to the sanitization context the
//! binding needs. The schema is namespace-aware: the first level is the lowercase property or
//! attribute name, the second the namespace (`""` for HTML, `svg`, `math`), the third the lowercase
//! tag name or `*` for every element.
//!
//! `ATTRIBUTE_NO_BINDING` (the `validateAttribute` context for SVG animation attributes and
//! `iframe`/`unknown` sandbox-style attributes) is [`SecurityContext::AttributeNoBinding`].

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

/// `DomElementSchemaRegistry.securityContext` / `getMappedPropName` helpers: [`ATTR_TO_PROP`] and
/// [`ALL_KNOWN_ELEMENT_NAMES`] are generated from Angular's `dom_element_schema_registry.ts`.

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
    (
        SecurityContext::AttributeNoBinding,
        SVG_NAMESPACE,
        &[
            ("animate", &["attributeName", "values", "to", "from"]),
            ("set", &["to", "attributeName"]),
            ("animateMotion", &["attributeName"]),
            ("animateTransform", &["attributeName"]),
        ],
    ),
    (
        SecurityContext::AttributeNoBinding,
        NO_NAMESPACE,
        &[
            (
                "unknown",
                &[
                    "attributeName",
                    "values",
                    "to",
                    "from",
                    "sandbox",
                    "allow",
                    "allowFullscreen",
                    "referrerPolicy",
                    "csp",
                    "fetchPriority",
                    "credentialless",
                ],
            ),
            (
                "iframe",
                &[
                    "sandbox",
                    "allow",
                    "allowFullscreen",
                    "referrerPolicy",
                    "csp",
                    "fetchPriority",
                    "credentialless",
                ],
            ),
        ],
    ),
];

/// Schema shape: `attribute (lowercase) -> namespace -> tag (lowercase or "*") -> context`.
type Schema = HashMap<String, HashMap<&'static str, HashMap<String, SecurityContext>>>;

/// Lazily-built schema (`SECURITY_SCHEMA()` is also memoised in Angular).
fn schema() -> &'static Schema {
    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        // `registerContext` lowercases every attribute and tag name, and a later row overwrites an
        // earlier one for the same (attribute, namespace, tag) triple.
        let mut out: Schema = HashMap::new();
        for (ctx, ns, specs) in REGISTRATIONS {
            for (tag, attrs) in specs.iter() {
                for attr in attrs.iter() {
                    out.entry(attr.to_lowercase())
                        .or_default()
                        .entry(ns)
                        .or_default()
                        .insert(tag.to_lowercase(), ctx.clone());
                }
            }
        }
        out
    })
}


/// `getMappedPropName` (`DomElementSchemaRegistry`): the DOM property an attribute name stands for
/// (`class` → `className`, `aria-label` → `ariaLabel`), or the name itself when it has no mapping.
pub fn mapped_prop_name(name: &str) -> &str {
    ATTR_TO_PROP
        .iter()
        .find(|(attr, _)| *attr == name)
        .map(|(_, prop)| *prop)
        .unwrap_or(name)
}

/// `DomElementSchemaRegistry.securityContext(tagName, propName, isAttribute)`: the sanitization context
/// of `propName` on the element `tag_name`, which may carry a namespace prefix (`:svg:a`). For an
/// attribute the mapped property name is checked (Angular: "use the mapped property name").
pub fn security_context_for_element(tag_name: &str, prop_name: &str, is_attribute: bool) -> SecurityContext {
    let prop = if is_attribute { mapped_prop_name(prop_name) } else { prop_name };
    let (ns, name) = crate::ml_parser::split_ns_name(tag_name);
    check_security_context(&name, prop, ns.as_deref())
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
    let Some(attr_schema) = schema.get(&prop_name.to_lowercase()) else {
        return SecurityContext::None;
    };
    let tag_lower = tag_name.to_lowercase();
    let mut context: Option<SecurityContext> = None;

    if let Some(ns) = namespace.filter(|n| !n.is_empty()) {
        if let Some(ns_schema) = attr_schema.get(ns) {
            context = ns_schema
                .get(&tag_lower)
                .or_else(|| ns_schema.get(MATCH_ALL_ELEMENTS))
                .cloned();
        }
    }

    if context.is_none() {
        if let Some(default_schema) = attr_schema.get(NO_NAMESPACE) {
            context = default_schema
                .get(&tag_lower)
                .or_else(|| default_schema.get(MATCH_ALL_ELEMENTS))
                .cloned();
        }
    }

    // An SVG animation element declared outside of an `<svg>` has no explicit namespace at compile
    // time, but can still animate once it is projected into an SVG subtree.
    if context.is_none() && namespace.map_or(true, |n| n.is_empty()) {
        if let Some(svg_schema) = attr_schema.get(SVG_NAMESPACE) {
            context = svg_schema.get(&tag_lower).cloned();
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
    fn attribute_no_binding_rows_cover_iframe_sandbox_style_attributes_and_svg_animation() {
        assert_eq!(check_security_context("iframe", "allow", None), SecurityContext::AttributeNoBinding);
        assert_eq!(check_security_context("iframe", "fetchPriority", None), SecurityContext::AttributeNoBinding);
        assert_eq!(check_security_context("div", "allow", None), SecurityContext::None);
        assert_eq!(
            check_security_context("animate", "attributeName", Some(SVG_NAMESPACE)),
            SecurityContext::AttributeNoBinding
        );
    }

    #[test]
    fn mapped_prop_name_follows_attr_to_prop_and_security_context_for_element_maps_attributes() {
        assert_eq!(mapped_prop_name("class"), "className");
        assert_eq!(mapped_prop_name("aria-label"), "ariaLabel");
        assert_eq!(mapped_prop_name("title"), "title");
        // An attribute is checked under its mapped property name (`for` → `htmlFor`).
        assert_eq!(security_context_for_element("a", "href", true), SecurityContext::Url);
        assert_eq!(security_context_for_element(":svg:a", "href", false), SecurityContext::Url);
    }

    #[test]
    fn all_known_element_names_lists_the_dom_schema_keys() {
        assert!(ALL_KNOWN_ELEMENT_NAMES.contains(&"div"));
        assert!(ALL_KNOWN_ELEMENT_NAMES.contains(&"iframe"));
        assert!(ALL_KNOWN_ELEMENT_NAMES.contains(&"unknown"));
        assert_eq!(ALL_KNOWN_ELEMENT_NAMES.len(), 232);
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

/// Angular `_ATTR_TO_PROP` (`schema/dom_element_schema_registry.ts`): the attribute spelling of a
/// property whose DOM name differs (`getMappedPropName`), including the ARIA property names.
pub const ATTR_TO_PROP: &[(&str, &str)] = &[
    ("class", "className"),
    ("for", "htmlFor"),
    ("formaction", "formAction"),
    ("innerHtml", "innerHTML"),
    ("readonly", "readOnly"),
    ("tabindex", "tabIndex"),
    ("aria-activedescendant", "ariaActiveDescendantElement"),
    ("aria-atomic", "ariaAtomic"),
    ("aria-autocomplete", "ariaAutoComplete"),
    ("aria-busy", "ariaBusy"),
    ("aria-checked", "ariaChecked"),
    ("aria-colcount", "ariaColCount"),
    ("aria-colindex", "ariaColIndex"),
    ("aria-colindextext", "ariaColIndexText"),
    ("aria-colspan", "ariaColSpan"),
    ("aria-controls", "ariaControlsElements"),
    ("aria-current", "ariaCurrent"),
    ("aria-describedby", "ariaDescribedByElements"),
    ("aria-description", "ariaDescription"),
    ("aria-details", "ariaDetailsElements"),
    ("aria-disabled", "ariaDisabled"),
    ("aria-errormessage", "ariaErrorMessageElements"),
    ("aria-expanded", "ariaExpanded"),
    ("aria-flowto", "ariaFlowToElements"),
    ("aria-haspopup", "ariaHasPopup"),
    ("aria-hidden", "ariaHidden"),
    ("aria-invalid", "ariaInvalid"),
    ("aria-keyshortcuts", "ariaKeyShortcuts"),
    ("aria-label", "ariaLabel"),
    ("aria-labelledby", "ariaLabelledByElements"),
    ("aria-level", "ariaLevel"),
    ("aria-live", "ariaLive"),
    ("aria-modal", "ariaModal"),
    ("aria-multiline", "ariaMultiLine"),
    ("aria-multiselectable", "ariaMultiSelectable"),
    ("aria-orientation", "ariaOrientation"),
    ("aria-owns", "ariaOwnsElements"),
    ("aria-placeholder", "ariaPlaceholder"),
    ("aria-posinset", "ariaPosInSet"),
    ("aria-pressed", "ariaPressed"),
    ("aria-readonly", "ariaReadOnly"),
    ("aria-required", "ariaRequired"),
    ("aria-roledescription", "ariaRoleDescription"),
    ("aria-rowcount", "ariaRowCount"),
    ("aria-rowindex", "ariaRowIndex"),
    ("aria-rowindextext", "ariaRowIndexText"),
    ("aria-rowspan", "ariaRowSpan"),
    ("aria-selected", "ariaSelected"),
    ("aria-setsize", "ariaSetSize"),
    ("aria-sort", "ariaSort"),
    ("aria-valuemax", "ariaValueMax"),
    ("aria-valuemin", "ariaValueMin"),
    ("aria-valuenow", "ariaValueNow"),
    ("aria-valuetext", "ariaValueText"),
];

/// `DomElementSchemaRegistry.allKnownElementNames()`: the keys of Angular's DOM element schema, in
/// schema order (`Array.from(this._schema.keys())`).
pub const ALL_KNOWN_ELEMENT_NAMES: &[&str] = &[
    "[element]",
    "[htmlelement]",
    "abbr",
    "address",
    "article",
    "aside",
    "b",
    "bdi",
    "bdo",
    "cite",
    "content",
    "code",
    "dd",
    "dfn",
    "dt",
    "em",
    "figcaption",
    "figure",
    "footer",
    "header",
    "hgroup",
    "i",
    "kbd",
    "main",
    "mark",
    "nav",
    "noscript",
    "rb",
    "rp",
    "rt",
    "rtc",
    "ruby",
    "s",
    "samp",
    "search",
    "section",
    "small",
    "strong",
    "sub",
    "sup",
    "u",
    "var",
    "wbr",
    "media",
    ":svg:",
    ":svg:graphics",
    ":svg:animation",
    ":svg:geometry",
    ":svg:componenttransferfunction",
    ":svg:gradient",
    ":svg:textcontent",
    ":svg:textpositioning",
    "a",
    "area",
    "audio",
    "br",
    "base",
    "body",
    "button",
    "canvas",
    "dl",
    "data",
    "datalist",
    "details",
    "dialog",
    "dir",
    "div",
    "embed",
    "fieldset",
    "font",
    "form",
    "frame",
    "frameset",
    "geolocation",
    "hr",
    "head",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "html",
    "iframe",
    "img",
    "input",
    "li",
    "label",
    "legend",
    "link",
    "map",
    "marquee",
    "menu",
    "meta",
    "meter",
    "ins",
    "del",
    "ol",
    "object",
    "optgroup",
    "option",
    "output",
    "p",
    "param",
    "picture",
    "pre",
    "progress",
    "q",
    "blockquote",
    "script",
    "select",
    "selectedcontent",
    "slot",
    "source",
    "span",
    "style",
    "caption",
    "th",
    "td",
    "col",
    "colgroup",
    "table",
    "tr",
    "tfoot",
    "thead",
    "tbody",
    "template",
    "textarea",
    "time",
    "title",
    "track",
    "ul",
    "unknown",
    "video",
    ":svg:a",
    ":svg:animate",
    ":svg:animatemotion",
    ":svg:animatetransform",
    ":svg:circle",
    ":svg:clippath",
    ":svg:defs",
    ":svg:desc",
    ":svg:discard",
    ":svg:ellipse",
    ":svg:feblend",
    ":svg:fecolormatrix",
    ":svg:fecomponenttransfer",
    ":svg:fecomposite",
    ":svg:feconvolvematrix",
    ":svg:fediffuselighting",
    ":svg:fedisplacementmap",
    ":svg:fedistantlight",
    ":svg:fedropshadow",
    ":svg:feflood",
    ":svg:fefunca",
    ":svg:fefuncb",
    ":svg:fefuncg",
    ":svg:fefuncr",
    ":svg:fegaussianblur",
    ":svg:feimage",
    ":svg:femerge",
    ":svg:femergenode",
    ":svg:femorphology",
    ":svg:feoffset",
    ":svg:fepointlight",
    ":svg:fespecularlighting",
    ":svg:fespotlight",
    ":svg:fetile",
    ":svg:feturbulence",
    ":svg:filter",
    ":svg:foreignobject",
    ":svg:g",
    ":svg:image",
    ":svg:line",
    ":svg:lineargradient",
    ":svg:mpath",
    ":svg:marker",
    ":svg:mask",
    ":svg:metadata",
    ":svg:path",
    ":svg:pattern",
    ":svg:polygon",
    ":svg:polyline",
    ":svg:radialgradient",
    ":svg:rect",
    ":svg:svg",
    ":svg:script",
    ":svg:set",
    ":svg:stop",
    ":svg:style",
    ":svg:switch",
    ":svg:symbol",
    ":svg:tspan",
    ":svg:text",
    ":svg:textpath",
    ":svg:title",
    ":svg:use",
    ":svg:view",
    "keygen",
    "menuitem",
    "summary",
    ":svg:cursor",
    ":math:",
    ":math:math",
    ":math:maction",
    ":math:menclose",
    ":math:merror",
    ":math:mfenced",
    ":math:mfrac",
    ":math:mi",
    ":math:mmultiscripts",
    ":math:mn",
    ":math:mo",
    ":math:mover",
    ":math:mpadded",
    ":math:mphantom",
    ":math:mroot",
    ":math:mrow",
    ":math:ms",
    ":math:mspace",
    ":math:msqrt",
    ":math:mstyle",
    ":math:msub",
    ":math:msubsup",
    ":math:msup",
    ":math:mtable",
    ":math:mtd",
    ":math:mtext",
    ":math:mtr",
    ":math:munder",
    ":math:munderover",
    ":math:semantics",
];
