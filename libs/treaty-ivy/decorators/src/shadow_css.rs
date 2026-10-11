//! Port of Angular's `packages/compiler/src/shadow_css.ts` — the emulated-encapsulation
//! CSS rewriter.
//!
//! `ShadowCss::shim_css_text(css, content_attr, host_attr)` scopes a component's raw CSS to the
//! component by appending the `content_attr` (e.g. `_ngcontent-%COMP%`) to each simple selector and
//! rewriting `:host` / `:host-context` host selectors to the `host_attr` (e.g. `_nghost-%COMP%`).
//! The `%COMP%` token is left literal — the runtime substitutes the component id at instantiation.
//!
//! This port reproduces Angular's algorithm closely enough to match its output for the CSS that
//! component authors write: plain rules, descendant/child/sibling combinators, `:host`,
//! `:host(...)`, `:host-context(...)`, `::ng-deep` / `>>>` / `/deep/` deep-combinator removal,
//! scoped at-rules (`@media`, `@supports`, …) whose inner rules are scoped, and `@keyframes` /
//! `@font-face` / `@page` which are left unscoped. Keyframe-name scoping (rewriting
//! `animation: name` → `animation: scope_name`) is also reproduced.
//!
//! The implementation uses the same placeholder-based block/comment/string escaping strategy as the
//! original so that braces, commas and colons inside strings or nested blocks do not confuse the
//! rule splitter.

use regex::{Captures, Regex};
use std::sync::LazyLock;

const COMMENT_PLACEHOLDER: &str = "%COMMENT%";
const BLOCK_PLACEHOLDER: &str = "%BLOCK%";
const COMMA_IN_PLACEHOLDER: &str = "%COMMA_IN_PLACEHOLDER%";
const SEMI_IN_PLACEHOLDER: &str = "%SEMI_IN_PLACEHOLDER%";
const COLON_IN_PLACEHOLDER: &str = "%COLON_IN_PLACEHOLDER%";

const POLYFILL_HOST: &str = "-shadowcsshost";
const POLYFILL_HOST_CONTEXT: &str = "-shadowcsscontext";
const POLYFILL_HOST_NO_COMBINATOR: &str = "-shadowcsshost-no-combinator";

const SCOPED_AT_RULE_IDENTIFIERS: &[&str] = &[
    "@media",
    "@supports",
    "@document",
    "@layer",
    "@container",
    "@scope",
    "@starting-style",
];

/// Animation shorthand keywords that must not be treated as keyframe names while scoping.
const ANIMATION_KEYWORDS: &[&str] = &[
    "inherit",
    "initial",
    "revert",
    "unset",
    "alternate",
    "alternate-reverse",
    "normal",
    "reverse",
    "backwards",
    "both",
    "forwards",
    "none",
    "paused",
    "running",
    "ease",
    "ease-in",
    "ease-in-out",
    "ease-out",
    "linear",
    "step-start",
    "step-end",
    "end",
    "jump-both",
    "jump-end",
    "jump-none",
    "jump-start",
    "start",
];

// ---------------------------------------------------------------------------
// Precompiled regexes (mirroring the `const … = /…/` declarations in the TS).
// ---------------------------------------------------------------------------

static NEW_LINES_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\r?\n").unwrap());
static COMMENT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)/\*.*?\*/").unwrap());
static COMMENT_WITH_HASH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/\*\s*#\s*source(?:Mapping)?URL=").unwrap());
static COMMENT_WITH_HASH_PLACEHOLDER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(COMMENT_PLACEHOLDER).unwrap());

static SHADOW_DEEP_SELECTORS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:>>>)|(?:/deep/)|(?:::ng-deep)").unwrap());
static POLYFILL_HOST_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)-shadowcsshost").unwrap());

// `_polyfillHostNoCombinatorRe = /-shadowcsshost-no-combinator([^\s,]*)/`
static POLYFILL_HOST_NO_COMBINATOR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-shadowcsshost-no-combinator([^\s,]*)").unwrap());

// `_ruleRe`: `(\s*(?:%COMMENT%\s*)*)([^;{}]+?)(\s*)((?:{%BLOCK%}?\s*;?)|(?:\s*;))`
static RULE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)(\s*(?:%COMMENT%\s*)*)([^;{}]+?)(\s*)((?:\{%BLOCK%\}?\s*;?)|(?:\s*;))").unwrap()
});

// nth-child / nth-of-type expression matcher (simplified one-level paren form, sufficient for
// authored CSS): `(:nth-[-\w]+)\(([^)]*)\)`
static NTH_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(:nth-[-\w]+)\(([^)]*)\)").unwrap());

// Attribute selector matcher for SafeSelector: `(\[[^\]]*\])`
static ATTR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\[[^\]]*\])").unwrap());

// Keyframes header prefix matcher: `^@(?:-webkit-)?keyframes\s+`. The name (with optional matching
// quotes) is parsed by hand in `scope_local_keyframe_declarations` because the original relies on a
// `\2` backreference (matching close quote) which the `regex` crate does not support.
static KEYFRAMES_PREFIX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(@(?:-webkit-)?keyframes\s+)").unwrap());

// ---------------------------------------------------------------------------
// Public entry.
// ---------------------------------------------------------------------------

/// Scope `css_text` to a component. `selector` is the content attribute (added to every element in
/// the host's view, e.g. `_ngcontent-%COMP%`); `host_selector` is the attribute placed on the host
/// itself (e.g. `_nghost-%COMP%`).
pub fn shim_css_text(css_text: &str, selector: &str, host_selector: &str) -> String {
    ShadowCss::default().shim_css_text(css_text, selector, host_selector)
}

#[derive(Default)]
struct ShadowCss {
    safe_selector: Option<SafeSelector>,
    should_scope_indicator: bool,
}

impl ShadowCss {
    fn shim_css_text(&mut self, css_text: &str, selector: &str, host_selector: &str) -> String {
        // Collect comments and replace them with a placeholder.
        let mut comments: Vec<String> = Vec::new();
        let replaced = COMMENT_RE.replace_all(css_text, |caps: &Captures| {
            let m = &caps[0];
            if COMMENT_WITH_HASH_RE.is_match(m) {
                comments.push(m.to_string());
            } else {
                let joined: String =
                    NEW_LINES_RE.find_iter(m).map(|mm| mm.as_str()).collect();
                comments.push(joined);
            }
            COMMENT_PLACEHOLDER.to_string()
        });

        let scoped = self.scope_css_text(&replaced, selector, host_selector);

        // Add back comments at the original positions.
        let mut idx = 0usize;
        COMMENT_WITH_HASH_PLACEHOLDER_RE
            .replace_all(&scoped, |_: &Captures| {
                let c = comments.get(idx).cloned().unwrap_or_default();
                idx += 1;
                c
            })
            .into_owned()
    }

    fn scope_css_text(&mut self, css_text: &str, scope_selector: &str, host_selector: &str) -> String {
        let mut css = self.insert_polyfill_host_in_css_text(css_text);
        css = self.convert_colon_host(&css);
        css = self.convert_colon_host_context(&css);
        if !scope_selector.is_empty() {
            css = self.scope_keyframes_related_css(&css, scope_selector);
            css = self.scope_selectors(&css, scope_selector, host_selector);
        }
        css.trim().to_string()
    }

    fn insert_polyfill_host_in_css_text(&self, selector: &str) -> String {
        // Angular: replace `:host-context(` (followed by non-empty args) with the context
        // placeholder, then standalone `:host` with the host placeholder. The `regex` crate has no
        // lookaround, so we tokenize manually: scan for `:host`, decide whether it is the
        // `-context` form, and only rewrite `:host-context` when it is immediately followed by
        // `(` + non-whitespace args (mirroring `_colonHostContextRe`'s `(?=\(\s*[^)\s])`).
        let lower = selector.to_ascii_lowercase();
        let bytes = lower.as_bytes();
        let mut out = String::with_capacity(selector.len());
        let mut i = 0usize;
        while i < bytes.len() {
            if lower[i..].starts_with(":host") {
                let is_context = lower[i..].starts_with(":host-context");
                if is_context {
                    // Look at the char after `:host-context` for `(` + `\s*` + non-`)`-non-space.
                    let after = i + ":host-context".len();
                    let mut j = after;
                    let mut matches_lookahead = false;
                    if bytes.get(j) == Some(&b'(') {
                        j += 1;
                        while bytes.get(j).is_some_and(|c| c.is_ascii_whitespace()) {
                            j += 1;
                        }
                        if let Some(&c) = bytes.get(j) {
                            if c != b')' && !c.is_ascii_whitespace() {
                                matches_lookahead = true;
                            }
                        }
                    }
                    if matches_lookahead {
                        out.push_str(POLYFILL_HOST_CONTEXT);
                        i = after;
                        continue;
                    }
                    // Not a context-with-args: fall through and treat the `:host` prefix only.
                    out.push_str(POLYFILL_HOST);
                    i += ":host".len();
                    continue;
                }
                // Standalone `:host` (not `-context`).
                out.push_str(POLYFILL_HOST);
                i += ":host".len();
                continue;
            }
            // Copy one full char (preserve original casing from `selector`, not the lowercased copy).
            let ch_len = utf8_len(bytes[i]);
            out.push_str(&selector[i..i + ch_len]);
            i += ch_len;
        }
        out
    }

    fn convert_colon_host(&self, css_text: &str) -> String {
        // `_cssColonHostRe = -shadowcsshost(\(...\))?([^,{]*)` — the entire parenthesised group is
        // optional, so it is wrapped in `(?:…)?`.
        let re = Regex::new(&format!(
            r"(?is){}(?:{})?([^,{{]*)",
            regex::escape(POLYFILL_HOST),
            r"\(([^()]*(?:\([^()]*\)[^()]*)*)\)"
        ))
        .unwrap();
        re.replace_all(css_text, |caps: &Captures| {
            let host_selectors = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let other_selectors = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            if !host_selectors.is_empty() {
                let parts: Vec<&str> = split_on_top_level_commas(host_selectors, true);
                if parts.len() > 1 {
                    return format!(":host({host_selectors}){other_selectors}");
                }
                let trimmed = parts.first().map(|p| p.trim()).unwrap_or("");
                if !trimmed.is_empty() {
                    let trimmed_no_host =
                        POLYFILL_HOST_RE.replace_all(trimmed, "").into_owned();
                    return format!(
                        "{POLYFILL_HOST_NO_COMBINATOR}{trimmed_no_host}{other_selectors}"
                    );
                }
            }
            format!("{POLYFILL_HOST_NO_COMBINATOR}{other_selectors}")
        })
        .into_owned()
    }

    fn convert_colon_host_context(&self, css_text: &str) -> String {
        let mut results: Vec<String> = Vec::new();
        for part in split_on_top_level_commas(css_text, false) {
            results.push(self.convert_colon_host_context_in_selector_part(part));
        }
        results.join(",")
    }

    fn convert_colon_host_context_in_selector_part(&self, css_text: &str) -> String {
        // `_cssColonHostContextReGlobal`: optional `:where(`/`:is(` prefix, then
        // `-shadowcsscontext(...)([^{]*)`.
        let re = Regex::new(&format!(
            r"(?is)(:(?:where|is)\()?({}(?:\(([^()]*(?:\([^()]*\)[^()]*)*)\))?([^{{]*))",
            regex::escape(POLYFILL_HOST_CONTEXT)
        ))
        .unwrap();
        re.replace_all(css_text, |caps: &Captures| {
            let pseudo_prefix = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let mut selector_text = caps.get(2).map(|m| m.as_str()).unwrap_or("").to_string();

            let mut context_selector_groups: Vec<Vec<String>> = vec![vec![]];

            let mut start_index = selector_text.find(POLYFILL_HOST_CONTEXT);
            while let Some(si) = start_index {
                let after_prefix =
                    selector_text[si + POLYFILL_HOST_CONTEXT.len()..].to_string();
                if after_prefix.is_empty() || !after_prefix.starts_with('(') {
                    selector_text = after_prefix;
                    start_index = selector_text.find(POLYFILL_HOST_CONTEXT);
                    continue;
                }

                let mut new_context_selectors: Vec<String> = Vec::new();
                let inner = &after_prefix[1..];
                let mut end_index = 0usize;
                for sel in split_on_top_level_commas(inner, true) {
                    end_index += sel.len() + 1;
                    let trimmed = sel.trim();
                    if !trimmed.is_empty() {
                        new_context_selectors.push(trimmed.to_string());
                    }
                }

                let groups_len = context_selector_groups.len();
                repeat_groups(&mut context_selector_groups, new_context_selectors.len());
                for (i, ncs) in new_context_selectors.iter().enumerate() {
                    for j in 0..groups_len {
                        context_selector_groups[j + i * groups_len].push(ncs.clone());
                    }
                }

                // `afterPrefix.substring(endIndex + 1)`
                selector_text = after_prefix
                    .get(end_index + 1..)
                    .unwrap_or("")
                    .to_string();
                start_index = selector_text.find(POLYFILL_HOST_CONTEXT);
            }

            context_selector_groups
                .iter()
                .map(|ctx| {
                    combine_host_context_selectors(ctx, &selector_text, pseudo_prefix)
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .into_owned()
    }

    fn scope_keyframes_related_css(&self, css_text: &str, scope_selector: &str) -> String {
        let mut unscoped: Vec<String> = Vec::new();
        let scoped_decls = process_rules(css_text, &mut |rule| {
            scope_local_keyframe_declarations(rule, scope_selector, &mut unscoped)
        });
        let unscoped_set = unscoped;
        process_rules(&scoped_decls, &mut |rule| {
            scope_animation_rule(rule, scope_selector, &unscoped_set)
        })
    }

    fn scope_selectors(&mut self, css_text: &str, scope_selector: &str, host_selector: &str) -> String {
        process_rules(css_text, &mut |rule| {
            let mut selector = rule.selector.clone();
            let mut content = rule.content.clone();
            if !rule.selector.starts_with('@') {
                selector = self.scope_selector(&selector, scope_selector, host_selector, true);
            } else if SCOPED_AT_RULE_IDENTIFIERS
                .iter()
                .any(|at| rule.selector.starts_with(at))
            {
                content = self.scope_selectors(&rule.content, scope_selector, host_selector);
            } else if rule.selector.starts_with("@font-face") || rule.selector.starts_with("@page") {
                content = strip_scoping_selectors(&rule.content);
            }
            CssRule { selector, content }
        })
    }

    fn scope_selector(
        &mut self,
        selector: &str,
        scope_selector: &str,
        host_selector: &str,
        is_parent_selector: bool,
    ) -> String {
        // Split on top-level commas (not inside parens).
        let parts = split_selector_on_commas(selector);
        parts
            .iter()
            .map(|part| {
                let deep_parts: Vec<&str> = SHADOW_DEEP_SELECTORS.split(part).collect();
                let shallow = deep_parts.first().copied().unwrap_or("");
                let others = &deep_parts[deep_parts.len().min(1)..];
                let scoped_shallow = if self.selector_needs_scoping(shallow, scope_selector) {
                    self.apply_selector_scope(shallow, scope_selector, host_selector, is_parent_selector)
                } else {
                    shallow.to_string()
                };
                let mut joined = vec![scoped_shallow];
                joined.extend(others.iter().map(|s| s.to_string()));
                joined.join(" ")
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn selector_needs_scoping(&self, selector: &str, scope_selector: &str) -> bool {
        let escaped = scope_selector.replace('[', r"\[").replace(']', r"\]");
        // `^(scope)([>\s~+[.,{:][\s\S]*)?$` (multiline).
        let re = Regex::new(&format!(
            r"(?m)^({escaped})([>\s~+\[.,{{:][\s\S]*)?$"
        ))
        .unwrap();
        !re.is_match(selector)
    }

    fn apply_simple_selector_scope(
        &self,
        selector: &str,
        scope_selector: &str,
        host_selector: &str,
    ) -> String {
        if POLYFILL_HOST_RE.is_match(selector) {
            let replace_by = format!("[{host_selector}]");
            let mut result = selector.to_string();
            while POLYFILL_HOST_NO_COMBINATOR_RE.is_match(&result) {
                result = POLYFILL_HOST_NO_COMBINATOR_RE
                    .replace(&result, |caps: &Captures| {
                        let sel = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                        // `selector.replace(/([^:\)]*)(:*)(.*)/, before + replaceBy + colon + after)`
                        let re = Regex::new(r"(?s)([^:\)]*)(:*)(.*)").unwrap();
                        re.replace(sel, |c: &Captures| {
                            format!(
                                "{}{}{}{}",
                                &c[1], replace_by, &c[2], &c[3]
                            )
                        })
                        .into_owned()
                    })
                    .into_owned();
            }
            return POLYFILL_HOST_RE.replace_all(&result, replace_by.as_str()).into_owned();
        }
        format!("{scope_selector} {selector}")
    }

    fn apply_selector_scope(
        &mut self,
        selector: &str,
        scope_selector: &str,
        host_selector: &str,
        is_parent_selector: bool,
    ) -> String {
        // `scopeSelector.replace(/\[is=([^\]]*)\]/g, parts[0])`
        let is_re = Regex::new(r"\[is=([^\]]*)\]").unwrap();
        let scope_selector = is_re.replace_all(scope_selector, "$1").into_owned();
        let attr_name = format!("[{scope_selector}]");

        let mut selector = selector.to_string();
        if is_parent_selector {
            let safe = SafeSelector::new(&selector);
            selector = safe.content().to_string();
            self.safe_selector = Some(safe);
        }

        let has_host = selector.contains(POLYFILL_HOST_NO_COMBINATOR);
        if is_parent_selector || self.should_scope_indicator {
            self.should_scope_indicator = !has_host;
        }

        // Split into combinator-separated parts.
        let mut scoped_selector = String::new();
        for piece in split_on_combinators(&selector) {
            match piece {
                Piece::Part(part) => {
                    let scoped_part =
                        self.scope_selector_part(&part, &scope_selector, host_selector, &attr_name);
                    scoped_selector.push_str(&scoped_part);
                }
                Piece::Separator(sep) => {
                    scoped_selector.push(' ');
                    scoped_selector.push_str(&sep);
                    scoped_selector.push(' ');
                }
            }
        }

        match &self.safe_selector {
            Some(s) => s.restore(&scoped_selector),
            None => scoped_selector,
        }
    }

    fn scope_selector_part(
        &mut self,
        p: &str,
        scope_selector: &str,
        host_selector: &str,
        attr_name: &str,
    ) -> String {
        // `_pseudoFunctionAwareScopeSelectorPart`: collect outer :where()/:is() wrappers.
        let pseudo_parts = collect_pseudo_selector_parts(p);
        if !pseudo_parts.is_empty() && pseudo_parts.join("") == p {
            return pseudo_parts
                .iter()
                .map(|sp| {
                    let prefix_re = Regex::new(r"(?i)^:(?:where|is)\(").unwrap();
                    let css_pseudo = prefix_re.find(sp).map(|m| m.as_str()).unwrap_or("");
                    // slice(cssPseudo.length, -1)
                    let inner = &sp[css_pseudo.len()..sp.len().saturating_sub(1)];
                    if inner.contains(POLYFILL_HOST_NO_COMBINATOR) {
                        self.should_scope_indicator = true;
                    }
                    let scoped_inner =
                        self.scope_selector(inner, scope_selector, host_selector, false);
                    format!("{css_pseudo}{scoped_inner})")
                })
                .collect::<Vec<_>>()
                .join("");
        }

        self.should_scope_indicator =
            self.should_scope_indicator || p.contains(POLYFILL_HOST_NO_COMBINATOR);
        if self.should_scope_indicator {
            self.scope_simple_part(p, scope_selector, host_selector, attr_name)
        } else {
            p.to_string()
        }
    }

    fn scope_simple_part(
        &self,
        p: &str,
        scope_selector: &str,
        host_selector: &str,
        attr_name: &str,
    ) -> String {
        let trimmed = p.trim();
        if trimmed.is_empty() {
            return p.to_string();
        }

        if p.contains(POLYFILL_HOST_NO_COMBINATOR) {
            let mut scoped_p = self.apply_simple_selector_scope(p, scope_selector, host_selector);
            // `if (!p.match(_polyfillHostNoCombinatorOutsidePseudoFunction)) { add attrName }`
            // where the regex is `-shadowcsshost-no-combinator(?![^(]*\))` — i.e. the marker is
            // "outside a pseudo function" when, scanning forward from it, a `(` is reached (or the
            // end) before any `)`. Only when NO such outside occurrence exists is the content attr
            // appended (so `:host(.foo)` → `[hosta].foo` gets no content attr, but a marker buried
            // inside `:where(... )` would).
            if !host_marker_outside_pseudo_function(p) {
                let re = Regex::new(r"(?s)([^:]*)(:*)([\s\S]*)").unwrap();
                if let Some(c) = re.captures(&scoped_p) {
                    scoped_p = format!("{}{}{}{}", &c[1], attr_name, &c[2], &c[3]);
                }
            }
            scoped_p
        } else {
            // remove :host (now -shadowcsshost) since it should be unnecessary
            let t = POLYFILL_HOST_RE.replace_all(p, "").into_owned();
            if t.is_empty() {
                p.to_string()
            } else {
                let re = Regex::new(r"(?s)([^:]*)(:*)([\s\S]*)").unwrap();
                if let Some(c) = re.captures(&t) {
                    format!("{}{}{}{}", &c[1], attr_name, &c[2], &c[3])
                } else {
                    t
                }
            }
        }
    }
}

/// Emulates `/-shadowcsshost-no-combinator(?![^(]*\))/.test(p)`: does the host marker appear
/// somewhere that is NOT inside a pseudo function? For each occurrence of the marker, scan forward;
/// if a `(` (or end of string) is reached before any `)`, the negative lookahead succeeds and the
/// marker is "outside" a pseudo function.
fn host_marker_outside_pseudo_function(p: &str) -> bool {
    let mut search = 0usize;
    while let Some(rel) = p[search..].find(POLYFILL_HOST_NO_COMBINATOR) {
        let after = search + rel + POLYFILL_HOST_NO_COMBINATOR.len();
        let mut outside = true;
        for c in p[after..].bytes() {
            if c == b'(' {
                break;
            }
            if c == b')' {
                outside = false;
                break;
            }
        }
        if outside {
            return true;
        }
        search = after;
    }
    false
}

fn strip_scoping_selectors(css_text: &str) -> String {
    process_rules(css_text, &mut |rule| {
        let selector = SHADOW_DEEP_SELECTORS.replace_all(&rule.selector, " ");
        let selector = POLYFILL_HOST_NO_COMBINATOR_RE
            .replace_all(&selector, " ")
            .into_owned();
        CssRule {
            selector,
            content: rule.content.clone(),
        }
    })
}

// ---------------------------------------------------------------------------
// Keyframes / animation scoping.
// ---------------------------------------------------------------------------

fn scope_local_keyframe_declarations(
    rule: &CssRule,
    scope_selector: &str,
    unscoped: &mut Vec<String>,
) -> CssRule {
    let Some(m) = KEYFRAMES_PREFIX_RE.find(&rule.selector) else {
        return rule.clone();
    };
    let start = &rule.selector[..m.end()];
    let rest = &rule.selector[m.end()..];
    // Split trailing whitespace (the `(\s*)$` group).
    let trimmed = rest.trim_end();
    let end_spaces = &rest[trimmed.len()..];
    // Optional matching surrounding quotes.
    let (quote, name) = if (trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2)
        || (trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2)
    {
        let q = &trimmed[..1];
        (q, &trimmed[1..trimmed.len() - 1])
    } else {
        ("", trimmed)
    };
    unscoped.push(unescape_quotes(name, !quote.is_empty()));
    CssRule {
        selector: format!("{start}{quote}{scope_selector}_{name}{quote}{end_spaces}"),
        content: rule.content.clone(),
    }
}

fn scope_animation_keyframe(keyframe: &str, scope_selector: &str, unscoped: &[String]) -> String {
    // Port of `/^(\s*)(['"]?)(.+?)\2(\s*)$/`: leading spaces, optional matching quote, name,
    // trailing spaces. The `\2` close-quote backreference is reproduced by hand.
    let leading_len = keyframe.len() - keyframe.trim_start().len();
    let spaces1 = &keyframe[..leading_len];
    let after_lead = &keyframe[leading_len..];
    let trailing_trimmed = after_lead.trim_end();
    let spaces2 = &after_lead[trailing_trimmed.len()..];
    if trailing_trimmed.is_empty() {
        return keyframe.to_string();
    }
    let (quote, name) = if (trailing_trimmed.starts_with('\'') && trailing_trimmed.ends_with('\'') && trailing_trimmed.len() >= 2)
        || (trailing_trimmed.starts_with('"') && trailing_trimmed.ends_with('"') && trailing_trimmed.len() >= 2)
    {
        (&trailing_trimmed[..1], &trailing_trimmed[1..trailing_trimmed.len() - 1])
    } else {
        ("", trailing_trimmed)
    };
    let prefix = if unscoped.contains(&unescape_quotes(name, !quote.is_empty())) {
        format!("{scope_selector}_")
    } else {
        String::new()
    };
    format!("{spaces1}{quote}{prefix}{name}{quote}{spaces2}")
}

fn scope_animation_rule(rule: &CssRule, scope_selector: &str, unscoped: &[String]) -> CssRule {
    // `animation: …`
    let anim_re = Regex::new(r"((?:^|\s+|;)(?:-webkit-)?animation\s*:\s*)([^;]+)").unwrap();
    // Capture a single keyframe token: either a quoted name (group 3) or a non-quoted CSS ident
    // (group 4), preceded by start/space/comma (group 1) and followed by a separator. The original
    // uses a `(?!['"])`-based inner repetition for the quoted body; the `regex` crate has no
    // lookahead, so the quoted body is matched as "any non-matching-quote run" via `[^'"]`/`[^"']`
    // alternatives keyed off the opening quote (group 2).
    let kf_re = Regex::new(
        r#"(^|\s+|,)(?:(?:'((?:\\.|[^'])*)')|(?:"((?:\\.|[^"])*)")|(-?[A-Za-z][\w\-]*))([,\s]|$)"#,
    )
    .unwrap();

    let content = anim_re.replace_all(&rule.content, |caps: &Captures| {
        let start = &caps[1];
        let decls = &caps[2];
        let scoped = kf_re.replace_all(decls, |c: &Captures| {
            let leading = c.get(1).map(|m| m.as_str()).unwrap_or("");
            let single_quoted = c.get(2).map(|m| m.as_str());
            let double_quoted = c.get(3).map(|m| m.as_str());
            let non_quoted = c.get(4).map(|m| m.as_str());
            let trailing = c.get(5).map(|m| m.as_str()).unwrap_or("");
            let full = &c[0];
            if let Some(qn) = single_quoted {
                let scoped =
                    scope_animation_keyframe(&format!("'{qn}'"), scope_selector, unscoped);
                format!("{leading}{scoped}{trailing}")
            } else if let Some(qn) = double_quoted {
                let scoped =
                    scope_animation_keyframe(&format!("\"{qn}\""), scope_selector, unscoped);
                format!("{leading}{scoped}{trailing}")
            } else if let Some(nq) = non_quoted {
                if ANIMATION_KEYWORDS.contains(&nq) {
                    full.to_string()
                } else {
                    let scoped = scope_animation_keyframe(nq, scope_selector, unscoped);
                    format!("{leading}{scoped}{trailing}")
                }
            } else {
                full.to_string()
            }
        });
        format!("{start}{scoped}")
    });

    // `animation-name: …`
    let anim_name_re =
        Regex::new(r"((?:^|\s+|;)(?:-webkit-)?animation-name\s*:\s*)([^;]+)").unwrap();
    let content = anim_name_re
        .replace_all(&content, |caps: &Captures| {
            let start = &caps[1];
            let kfs = &caps[2];
            let scoped = kfs
                .split(',')
                .map(|kf| scope_animation_keyframe(kf, scope_selector, unscoped))
                .collect::<Vec<_>>()
                .join(",");
            format!("{start}{scoped}")
        })
        .into_owned();

    CssRule {
        selector: rule.selector.clone(),
        content,
    }
}

fn unescape_quotes(s: &str, is_quoted: bool) -> String {
    if !is_quoted {
        return s.to_string();
    }
    // Remove a backslash that escapes a following quote. The `regex` crate has no lookahead, so we
    // scan manually: a `\` immediately followed by `'` or `"` is dropped (the quote is kept).
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            match bytes.get(i + 1) {
                Some(&b'\'') | Some(&b'"') => {
                    // Drop the backslash; the quote is emitted on the next iteration.
                    i += 1;
                    continue;
                }
                Some(_) => {
                    // Escaped non-quote: keep both bytes verbatim.
                    out.push('\\');
                    let ch_len = utf8_len(bytes[i + 1]);
                    out.push_str(&s[i + 1..i + 1 + ch_len]);
                    i += 1 + ch_len;
                    continue;
                }
                None => {
                    out.push('\\');
                    i += 1;
                    continue;
                }
            }
        }
        let ch_len = utf8_len(bytes[i]);
        out.push_str(&s[i..i + ch_len]);
        i += ch_len;
    }
    out
}

/// Length in bytes of the UTF-8 sequence whose leading byte is `b`.
fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b < 0xE0 {
        2
    } else if b < 0xF0 {
        3
    } else {
        4
    }
}

// ---------------------------------------------------------------------------
// :host-context combination.
// ---------------------------------------------------------------------------

fn combine_host_context_selectors(
    context_selectors: &[String],
    other_selectors: &str,
    pseudo_prefix: &str,
) -> String {
    let host_marker = POLYFILL_HOST_NO_COMBINATOR;
    let other_has_host = POLYFILL_HOST_RE.is_match(other_selectors);

    if context_selectors.is_empty() {
        return format!("{host_marker}{other_selectors}");
    }

    let mut ctx = context_selectors.to_vec();
    let mut combined: Vec<String> = vec![ctx.pop().unwrap_or_default()];
    while let Some(context_selector) = ctx.pop() {
        let length = combined.len();
        combined.resize(length * 3, String::new());
        for i in 0..length {
            let previous = combined[i].clone();
            combined[length * 2 + i] = format!("{previous} {context_selector}");
            combined[length + i] = format!("{context_selector} {previous}");
            combined[i] = format!("{context_selector}{previous}");
        }
    }

    combined
        .iter()
        .map(|s| {
            if other_has_host {
                format!("{pseudo_prefix}{s}{other_selectors}")
            } else {
                format!(
                    "{pseudo_prefix}{s}{host_marker}{other_selectors}, {pseudo_prefix}{s} {host_marker}{other_selectors}"
                )
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn repeat_groups(groups: &mut Vec<Vec<String>>, multiples: usize) {
    let length = groups.len();
    groups.resize(length * multiples.max(1), vec![]);
    for i in 1..multiples {
        for j in 0..length {
            groups[j + i * length] = groups[j].clone();
        }
    }
}

// ---------------------------------------------------------------------------
// Selector splitting helpers.
// ---------------------------------------------------------------------------

/// Split on top-level commas (commas not inside parens). When `return_on_closing_paren` is set,
/// stop at an extra closing paren (used to read the contents of a `(...)`).
fn split_on_top_level_commas(text: &str, return_on_closing_paren: bool) -> Vec<&str> {
    let mut out = Vec::new();
    let mut parens: i32 = 0;
    let mut prev = 0usize;
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'(' {
            parens += 1;
        } else if c == b')' {
            parens -= 1;
            if parens < 0 && return_on_closing_paren {
                out.push(&text[prev..i]);
                return out;
            }
        } else if c == b',' && parens == 0 {
            out.push(&text[prev..i]);
            prev = i + 1;
        }
        i += 1;
    }
    out.push(&text[prev..]);
    out
}

/// Split a selector on top-level commas, respecting up to three nested paren levels (matching the
/// TS `selectorSplitRe`). Returns trimmed-by-the-regex parts (the regex allows an optional space on
/// either side of the comma).
fn split_selector_on_commas(selector: &str) -> Vec<String> {
    // Equivalent to splitting on top-level commas (parens guard nesting). Then strip a single
    // surrounding space as the original ` ?,...? ?` regex does.
    split_on_top_level_commas(selector, false)
        .into_iter()
        .map(|p| p.trim_matches(' ').to_string())
        .collect()
}

enum Piece {
    Part(String),
    Separator(String),
}

/// Split a selector into alternating Part / Separator pieces on top-level combinators
/// (space, `>`, `+`, `~`), not inside parentheses.
fn split_on_combinators(selector: &str) -> Vec<Piece> {
    let mut pieces = Vec::new();
    let bytes = selector.as_bytes();
    let mut parens: i32 = 0;
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'(' {
            parens += 1;
            i += 1;
            continue;
        }
        if c == b')' {
            parens -= 1;
            i += 1;
            continue;
        }
        if parens == 0 {
            let is_combinator = c == b'>' || c == b'+' || (c == b'~' && bytes.get(i + 1) != Some(&b'='));
            let is_space = c == b' ' || c == b'\t' || c == b'\n' || c == b'\r';
            if is_combinator || is_space {
                // Gather the run: a combinator may be surrounded by whitespace; the original regex
                // treats `( |>|+|~)` as the separator plus trailing `\s*`.
                let part = &selector[start..i];
                // Determine the separator char (whitespace-only separators map to " ").
                let mut j = i;
                // skip leading spaces
                while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                    j += 1;
                }
                let sep = if j < bytes.len()
                    && (bytes[j] == b'>' || bytes[j] == b'+' || (bytes[j] == b'~' && bytes.get(j + 1) != Some(&b'=')))
                {
                    let s = (bytes[j] as char).to_string();
                    j += 1;
                    s
                } else {
                    " ".to_string()
                };
                // skip trailing whitespace (the `\s*` after the separator)
                while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t' || bytes[j] == b'\n' || bytes[j] == b'\r') {
                    j += 1;
                }
                if !part.is_empty() || sep != " " {
                    pieces.push(Piece::Part(part.to_string()));
                    pieces.push(Piece::Separator(sep));
                    start = j;
                    i = j;
                    continue;
                }
            }
        }
        i += 1;
    }
    pieces.push(Piece::Part(selector[start..].to_string()));
    pieces
}

/// Collect outer `:where(...)` / `:is(...)` parts of a selector (balancing parens). Returns the
/// matched wrapper substrings, or empty if none.
fn collect_pseudo_selector_parts(selector_part: &str) -> Vec<String> {
    let prefix_re = Regex::new(r"(?i):(?:where|is)\(").unwrap();
    let mut parts = Vec::new();
    let bytes = selector_part.as_bytes();
    let mut search_from = 0usize;
    while let Some(m) = prefix_re.find_at(selector_part, search_from) {
        let mut open = 1i32;
        let mut index = m.end();
        while index < bytes.len() {
            let ch = bytes[index];
            index += 1;
            if ch == b'(' {
                open += 1;
            } else if ch == b')' {
                open -= 1;
                if open == 0 {
                    break;
                }
            }
        }
        parts.push(selector_part[m.start()..index].to_string());
        search_from = index;
    }
    parts
}

// ---------------------------------------------------------------------------
// SafeSelector — placeholder attribute/escape/nth values.
// ---------------------------------------------------------------------------

struct SafeSelector {
    placeholders: Vec<String>,
    content: String,
}

impl SafeSelector {
    fn new(selector: &str) -> Self {
        let mut placeholders: Vec<String> = Vec::new();
        let mut index = 0usize;

        // Replace attribute selectors `[...]` with placeholders.
        let mut s = ATTR_RE
            .replace_all(selector, |caps: &Captures| {
                let keep = caps[1].to_string();
                let replace = format!("__ph-{index}__");
                placeholders.push(keep);
                index += 1;
                replace
            })
            .into_owned();

        // Replace escape sequences `\.` with placeholders.
        s = Regex::new(r"(?s)(\\.)")
            .unwrap()
            .replace_all(&s, |caps: &Captures| {
                let keep = caps[1].to_string();
                let replace = format!("__esc-ph-{index}__");
                placeholders.push(keep);
                index += 1;
                replace
            })
            .into_owned();

        // Replace `:nth-…(expr)` expressions with placeholders.
        let content = NTH_RE
            .replace_all(&s, |caps: &Captures| {
                let pseudo = &caps[1];
                let exp = &caps[2];
                let replace = format!("__ph-{index}__");
                placeholders.push(format!("({exp})"));
                index += 1;
                format!("{pseudo}{replace}")
            })
            .into_owned();

        SafeSelector {
            placeholders,
            content,
        }
    }

    fn restore(&self, content: &str) -> String {
        Regex::new(r"__(?:ph|esc-ph)-(\d+)__")
            .unwrap()
            .replace_all(content, |caps: &Captures| {
                let i: usize = caps[1].parse().unwrap_or(0);
                self.placeholders.get(i).cloned().unwrap_or_default()
            })
            .into_owned()
    }

    fn content(&self) -> &str {
        &self.content
    }
}

// ---------------------------------------------------------------------------
// processRules — block/comment/string-safe rule iteration.
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct CssRule {
    pub selector: String,
    pub content: String,
}

/// Iterate over CSS rules, calling `cb` with each `(selector, block-content)` pair and substituting
/// the returned rule. Blocks (`{ … }`), comments and string contents are placeholdered first so the
/// `RULE_RE` splitter is not confused by inner braces/commas/semicolons/colons.
fn process_rules(input: &str, cb: &mut dyn FnMut(&CssRule) -> CssRule) -> String {
    let escaped = escape_in_strings(input);
    let blocks = escape_blocks(&escaped);
    let mut next_block = 0usize;

    let result = RULE_RE.replace_all(&blocks.escaped_string, |caps: &Captures| {
        let prefix = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let selector = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        let between = caps.get(3).map(|m| m.as_str()).unwrap_or("");
        let mut suffix = caps.get(4).map(|m| m.as_str()).unwrap_or("").to_string();

        let mut content = String::new();
        let mut content_prefix = "";
        let block_open = format!("{{{BLOCK_PLACEHOLDER}");
        if suffix.starts_with(&block_open) {
            content = blocks.blocks.get(next_block).cloned().unwrap_or_default();
            next_block += 1;
            suffix = suffix[BLOCK_PLACEHOLDER.len() + 1..].to_string();
            content_prefix = "{";
        }

        let rule = cb(&CssRule {
            selector: selector.to_string(),
            content,
        });
        format!(
            "{prefix}{}{between}{content_prefix}{}{suffix}",
            rule.selector, rule.content
        )
    });

    unescape_in_strings(&result)
}

struct StringWithEscapedBlocks {
    escaped_string: String,
    blocks: Vec<String>,
}

fn escape_blocks(input: &str) -> StringWithEscapedBlocks {
    let mut result_parts: Vec<String> = Vec::new();
    let mut escaped_blocks: Vec<String> = Vec::new();
    let mut open_count = 0i32;
    let mut non_block_start = 0usize;
    let mut block_start: i64 = -1;
    let chars: Vec<char> = input.chars().collect();
    // Work in byte indices to slice safely; build a char->byte map.
    let byte_indices: Vec<usize> = input.char_indices().map(|(b, _)| b).collect();
    let byte_at = |ci: usize| -> usize {
        if ci >= byte_indices.len() {
            input.len()
        } else {
            byte_indices[ci]
        }
    };

    let mut i = 0usize;
    while i < chars.len() {
        let ch = chars[i];
        if ch == '\\' {
            i += 1;
        } else if ch == '}' && open_count > 0 {
            open_count -= 1;
            if open_count == 0 {
                let bs = block_start as usize;
                escaped_blocks.push(input[byte_at(bs)..byte_at(i)].to_string());
                result_parts.push(BLOCK_PLACEHOLDER.to_string());
                non_block_start = i;
                block_start = -1;
            }
        } else if ch == '{' && open_count > 0 {
            open_count += 1;
        } else if open_count == 0 && ch == '{' {
            open_count = 1;
            block_start = (i + 1) as i64;
            result_parts.push(input[byte_at(non_block_start)..byte_at(i + 1)].to_string());
        }
        i += 1;
    }

    if block_start != -1 {
        escaped_blocks.push(input[byte_at(block_start as usize)..].to_string());
        result_parts.push(BLOCK_PLACEHOLDER.to_string());
    } else {
        result_parts.push(input[byte_at(non_block_start)..].to_string());
    }

    StringWithEscapedBlocks {
        escaped_string: result_parts.join(""),
        blocks: escaped_blocks,
    }
}

fn escape_in_strings(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut quote: Option<char> = None;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            result.push(c);
            if let Some(next) = chars.next() {
                result.push(next);
            }
            continue;
        }
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                    result.push(c);
                } else {
                    match c {
                        ';' => result.push_str(SEMI_IN_PLACEHOLDER),
                        ',' => result.push_str(COMMA_IN_PLACEHOLDER),
                        ':' => result.push_str(COLON_IN_PLACEHOLDER),
                        _ => result.push(c),
                    }
                }
            }
            None => {
                if c == '\'' || c == '"' {
                    quote = Some(c);
                }
                result.push(c);
            }
        }
    }
    result
}

fn unescape_in_strings(input: &str) -> String {
    input
        .replace(COMMA_IN_PLACEHOLDER, ",")
        .replace(SEMI_IN_PLACEHOLDER, ";")
        .replace(COLON_IN_PLACEHOLDER, ":")
}

// ---------------------------------------------------------------------------
// Tests vs known Angular ShadowCss outputs.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const CONTENT: &str = "_ngcontent-%COMP%";
    const HOST: &str = "_nghost-%COMP%";

    /// Mirror Angular's `toEqualCss` normalization: collapse whitespace runs, drop the space after
    /// a `:`, and the space before a `}`.
    fn norm(css: &str) -> String {
        let collapsed = Regex::new(r"\s+").unwrap().replace_all(css.trim(), " ");
        let no_colon_space = collapsed.replace(": ", ":");
        no_colon_space.replace(" }", "}")
    }

    fn s(css: &str) -> String {
        norm(&shim_css_text(css, CONTENT, HOST))
    }

    fn eq(css: &str, expected: &str) {
        assert_eq!(s(css), norm(expected), "input: {css}");
    }

    #[test]
    fn simple_class_rule_gets_content_attr() {
        eq(".gauge {color: red;}", ".gauge[_ngcontent-%COMP%] {color: red;}");
    }

    #[test]
    fn element_selector_gets_content_attr() {
        eq("div {color: red;}", "div[_ngcontent-%COMP%] {color: red;}");
    }

    #[test]
    fn descendant_selector_scopes_each_simple_selector() {
        eq(
            ".a .b {color: red;}",
            ".a[_ngcontent-%COMP%] .b[_ngcontent-%COMP%] {color: red;}",
        );
    }

    #[test]
    fn child_combinator_preserved() {
        eq(
            ".a > .b {color: red;}",
            ".a[_ngcontent-%COMP%] > .b[_ngcontent-%COMP%] {color: red;}",
        );
    }

    #[test]
    fn host_becomes_host_attr() {
        eq(":host {color: red;}", "[_nghost-%COMP%] {color: red;}");
    }

    #[test]
    fn host_with_args() {
        // Angular: `:host(.foo)` → `.foo[host]` (the host arg precedes the host attribute).
        eq(":host(.foo) {color: red;}", ".foo[_nghost-%COMP%] {color: red;}");
    }

    #[test]
    fn host_variants_match_angular() {
        // Cross-checked against packages/compiler/test/shadow_css/host_and_host_context_spec.ts
        // (host attr written as `H` for the `%COMP%` token here).
        eq(":host(ul) {}", "ul[_nghost-%COMP%] {}");
        eq(":host[attr] {}", "[attr][_nghost-%COMP%] {}");
        eq(":host(.a.b) {}", ".a.b[_nghost-%COMP%] {}");
        eq(":host:before {}", "[_nghost-%COMP%]:before {}");
        eq(":host(.class):before {}", ".class[_nghost-%COMP%]:before {}");
        // A top-level comma list inside :host(...) is left intact (and gets a content attr).
        eq(":host(.a, .b) {}", "[_ngcontent-%COMP%]:host(.a, .b) {}");
    }

    #[test]
    fn host_context() {
        eq(
            ":host-context(.foo) .bar {color: red;}",
            ".foo[_nghost-%COMP%] .bar[_ngcontent-%COMP%], .foo [_nghost-%COMP%] .bar[_ngcontent-%COMP%] {color: red;}",
        );
    }

    #[test]
    fn ng_deep_removed_and_descendants_unscoped() {
        eq(":host ::ng-deep .x {color: red;}", "[_nghost-%COMP%] .x {color: red;}");
    }

    #[test]
    fn media_query_inner_rules_scoped_header_left() {
        eq(
            "@media screen {.a {color: red;}}",
            "@media screen {.a[_ngcontent-%COMP%] {color: red;}}",
        );
    }

    #[test]
    fn keyframes_left_unscoped_in_header() {
        // @keyframes header is not given a content attr; the keyframe name is scoped.
        let out = s("@keyframes foo {from {opacity:0;} to {opacity:1;}}");
        assert!(out.starts_with("@keyframes _ngcontent-%COMP%_foo"), "got: {out}");
    }

    #[test]
    fn animation_name_scoped_when_defined_locally() {
        // `norm` strips the space after `:`, so the scoped declaration reads `animation:…`.
        let out = s(".box {animation: foo 1s;} @keyframes foo {to {opacity:1;}}");
        assert!(out.contains("animation:_ngcontent-%COMP%_foo 1s"), "got: {out}");
        assert!(out.contains("@keyframes _ngcontent-%COMP%_foo"), "got: {out}");
    }

    #[test]
    fn animation_keyword_not_scoped() {
        let out = s(".box {animation: 1s none;}");
        assert!(out.contains("1s none"), "got: {out}");
    }

    #[test]
    fn comment_preserved_when_source_url() {
        let out = s(".a {color:red;}\n/*# sourceMappingURL=x.css.map */");
        assert!(out.contains("sourceMappingURL=x.css.map"), "got: {out}");
    }

    #[test]
    fn multiple_comma_selectors_each_scoped() {
        eq(
            ".a, .b {color: red;}",
            ".a[_ngcontent-%COMP%], .b[_ngcontent-%COMP%] {color: red;}",
        );
    }

    #[test]
    fn attribute_selector_scoped() {
        eq("a[href] {color: red;}", "a[href][_ngcontent-%COMP%] {color: red;}");
    }

    #[test]
    fn font_face_not_scoped() {
        let out = s("@font-face {font-family: x;}");
        assert!(out.starts_with("@font-face"), "got: {out}");
        assert!(!out.contains("_ngcontent"), "got: {out}");
    }
}
