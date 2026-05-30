//! JSX attribute / directive lowering: mapping JSX props (`className`, `onClick`, `style`, …)
//! onto Angular template syntax (`class`, `(click)`, `[style.color]`, …) and recognizing the three
//! Treaty directive-application syntaxes.
//!
//! [`super::template`] performs the structural element/attribute lowering; this module owns the
//! *name-level* policy decisions so there is a single table for them:
//!   * [`map_attribute_name`] — rename a JSX prop to its Angular attribute name (`className` →
//!     `class`, `htmlFor` → `for`).
//!   * [`event_name`] — recognize a React-style `onX` handler prop and return the Angular event
//!     name (`onClick` → `click`), or `None` when the prop is not an event handler.
//!
//! # Directives
//!
//! A directive in Treaty is just a class the author imports; like components, directives resolve by
//! SELECTORLESS auto-import — referencing one in the template adds it to the component's
//! `dependencies` with no manual `imports` array (see [`register_directive_reference`] /
//! [`take_directive_references`], which the JSX front-end drains after lowering and feeds to
//! [`crate::sfc::compile_from_parts_with_directives`]).
//!
//! Three application syntaxes are recognized during attribute lowering ([`classify_attribute`]):
//!
//! 1. **Namespace form (PREFERRED / documented):** `use:autofocus` (no value) applies the
//!    `Autofocus` directive; `use:tooltip={expr}` applies `Tooltip` and binds its primary input
//!    (`[tooltip]="expr"`). The directive class is the PascalCase of the suffix; the primary input
//!    name is the suffix verbatim.
//!
//! 2. **Capitalized-attribute form:** `<input Autofocus />` applies `Autofocus`; `<button
//!    Tooltip="hi">` / `Tooltip={expr}` applies `Tooltip` with the value bound to its primary input
//!    (`[tooltip]`, the lower-camel of the class). A capitalized first letter distinguishes a
//!    directive from a lowercase DOM attribute.
//!
//! 3. **Angular-attribute form:** a bare lowercase attribute whose PascalCase matches a known
//!    (imported) directive class applies that directive. A leading `*` marks a STRUCTURAL directive:
//!    `*highlight={expr}` lowers to the Angular structural form (an `<ng-template>` carrying the
//!    `[highlight]="expr"` binding around the host), exactly as `*ngIf` desugars. Because `*` is not
//!    a legal JSX attribute character, the JSX-authorable equivalent of `*highlight` is the
//!    `structural:highlight` namespace; both spellings lower identically.

use std::cell::RefCell;

thread_local! {
    /// The candidate set + collected directive class references for the in-flight lowering pass.
    ///
    /// `candidates` is the set of imported class names (set once, before lowering) used to resolve
    /// the bare-lowercase Angular-attribute form. `referenced` accumulates the directive classes the
    /// lowering actually applied, in first-seen order, so the front-end can auto-import them.
    static STATE: RefCell<DirectiveState> = RefCell::new(DirectiveState::default());
}

#[derive(Default)]
struct DirectiveState {
    candidates: Vec<String>,
    referenced: Vec<String>,
}

/// Seed the lowering pass with the imported class-name candidate set and clear any prior
/// references. Call once before lowering a component's JSX.
pub fn begin_pass(candidate_names: &[String]) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.candidates = candidate_names.to_vec();
        s.referenced.clear();
    });
}

/// Record that the directive class `name` was applied in the template (deduplicated, first-seen
/// order). The JSX front-end drains these via [`take_directive_references`] after lowering to feed
/// the selectorless auto-import.
pub fn register_directive_reference(name: &str) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if !s.referenced.iter().any(|n| n == name) {
            s.referenced.push(name.to_string());
        }
    });
}

/// Drain the directive class references collected during the lowering pass (first-seen order).
pub fn take_directive_references() -> Vec<String> {
    STATE.with(|s| std::mem::take(&mut s.borrow_mut().referenced))
}

/// Whether `name` (a bare lowercase attribute) PascalCase-folds to a known imported directive class.
fn matches_known_directive(name: &str) -> Option<String> {
    let pascal = pascal_case(name);
    STATE.with(|s| {
        let s = s.borrow();
        if s.candidates.iter().any(|c| c == &pascal) {
            Some(pascal)
        } else {
            None
        }
    })
}

/// PascalCase a directive attribute suffix/name: `tooltip` → `Tooltip`, `auto-focus` → `AutoFocus`,
/// `my_dir` → `MyDir`. Non-alphanumeric characters delimit words and are dropped.
pub fn pascal_case(name: &str) -> String {
    let mut out = String::new();
    let mut new_word = true;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            if new_word {
                out.extend(ch.to_uppercase());
                new_word = false;
            } else {
                out.push(ch);
            }
        } else {
            new_word = true;
        }
    }
    out
}

/// The lower-camel primary-input name for a directive class. The convention (matching Angular's
/// common `[appHighlight]`-style directives, and Treaty's `use:tooltip` suffix) is the class name
/// with its first letter lower-cased: `Tooltip` → `tooltip`, `NgModel` → `ngModel`.
pub fn primary_input_name_of(class_name: &str) -> String {
    primary_input_name(class_name)
}

/// See [`primary_input_name_of`].
fn primary_input_name(class_name: &str) -> String {
    let mut chars = class_name.chars();
    match chars.next() {
        Some(first) => first.to_ascii_lowercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

/// A recognized directive application parsed from a JSX attribute name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectiveApplication {
    /// The directive class (PascalCase), e.g. `Autofocus`, `Tooltip`, `Highlight`.
    pub class_name: String,
    /// The directive's primary input name (e.g. `tooltip`) when the syntax carries a value to bind;
    /// `None` for a value-less application (e.g. `use:autofocus`, `<input Autofocus />`).
    pub input_name: Option<String>,
    /// `true` for a structural application (`*highlight`), which lowers to an `<ng-template>` host.
    pub structural: bool,
}

/// Classify a JSX attribute `raw_name` as one of the three directive syntaxes, or `None` when it is
/// not a directive (an ordinary DOM attribute / property).
///
/// `has_value` indicates whether the attribute carries a value (`Tooltip="hi"`, `use:tooltip={x}`),
/// which decides whether the primary input is bound. The bare Angular-attribute form is only a
/// directive when its PascalCase matches a known imported class (see [`matches_known_directive`]).
pub fn classify_attribute(raw_name: &str, has_value: bool) -> Option<DirectiveApplication> {
    // 1. Namespace form: `use:autofocus`, `use:tooltip`.
    if let Some(suffix) = raw_name.strip_prefix("use:") {
        if suffix.is_empty() {
            return None;
        }
        // `use:class` is NOT a directive — it is an accepted alias for the `class` binding (alongside
        // `class` and `className`), so let it fall through to the class-attribute handler.
        if suffix == "class" {
            return None;
        }
        let class_name = pascal_case(suffix);
        return Some(DirectiveApplication {
            input_name: if has_value {
                // Preserve the author's suffix verbatim as the input name (`use:tooltip` → input
                // `tooltip`); this is the documented primary-input convention.
                Some(suffix.to_string())
            } else {
                None
            },
            class_name,
            structural: false,
        });
    }

    // 3. Structural Angular-attribute form. The Angular spelling is `*highlight={expr}` (the `*`
    //    marks a structural directive). Because `*` is not a legal JSX attribute character, the
    //    JSX-authorable equivalent is the `structural:` namespace (`structural:highlight={expr}`);
    //    both spellings are accepted and lower identically. The name must fold to a known imported
    //    directive class (a structural directive is always one the author has imported).
    let structural_suffix = raw_name
        .strip_prefix('*')
        .or_else(|| raw_name.strip_prefix("structural:"));
    if let Some(suffix) = structural_suffix {
        if suffix.is_empty() {
            return None;
        }
        let class_name = matches_known_directive(suffix)?;
        return Some(DirectiveApplication {
            class_name,
            input_name: if has_value { Some(suffix.to_string()) } else { None },
            structural: true,
        });
    }

    let first = raw_name.chars().next()?;

    // 2. Capitalized-attribute form: `<input Autofocus />`, `<button Tooltip="hi">`. A leading
    //    uppercase ASCII letter marks a directive (distinguishing it from lowercase DOM attributes).
    //    The class is the name verbatim; the primary input is its lower-camel form.
    if first.is_ascii_uppercase() {
        let class_name = raw_name.to_string();
        let input_name = if has_value {
            Some(primary_input_name(&class_name))
        } else {
            None
        };
        return Some(DirectiveApplication {
            class_name,
            input_name,
            structural: false,
        });
    }

    // 3b. Bare lowercase Angular-attribute form: applies a directive iff its PascalCase matches a
    //     known imported directive class; otherwise it is an ordinary DOM attribute.
    if first.is_ascii_lowercase() {
        if let Some(class_name) = matches_known_directive(raw_name) {
            let input_name = if has_value {
                Some(raw_name.to_string())
            } else {
                None
            };
            return Some(DirectiveApplication {
                class_name,
                input_name,
                structural: false,
            });
        }
    }

    None
}

/// Translate a JSX attribute name to its Angular template equivalent.
///
/// React spells two DOM attributes differently from HTML because their HTML names collide with JS
/// reserved-word-ish identifiers; Angular templates use the real HTML attribute names, so undo the
/// React spelling:
///   * `className` → `class`
///   * `htmlFor`   → `for`
///
/// Every other name passes through unchanged (event handlers are handled separately via
/// [`event_name`], so they never reach this function as element attributes).
pub fn map_attribute_name(name: &str) -> String {
    match name {
        // `class` is the natural/preferred form; `className` (React-compat) and `use:class`
        // (directive-namespace form) are accepted aliases that all resolve to the class binding.
        "className" | "use:class" => "class".to_string(),
        "htmlFor" => "for".to_string(),
        other => other.to_string(),
    }
}

/// If `name` is a React-style event handler prop (`onClick`, `onInput`, `onDblClick`, …) return the
/// Angular DOM event name (`click`, `input`, `dblclick`), otherwise `None`.
///
/// The rule mirrors React→DOM event naming: strip the `on` prefix and lowercase the remainder.
/// React event names are the DOM event name with the first letter upper-cased (`onClick` →
/// `click`, `onMouseEnter` → `mouseenter`, `onInput` → `input`), so a straight ASCII-lowercase of
/// the whole remainder yields the DOM event name. A bare `on` (no event) is not a handler.
pub fn event_name(name: &str) -> Option<String> {
    let rest = name.strip_prefix("on")?;
    // The character after `on` must be upper-case for this to be an event prop (`onClick`), so that
    // a legitimate attribute like `online` is not mistaken for an event handler.
    let first = rest.chars().next()?;
    if !first.is_ascii_uppercase() {
        return None;
    }
    Some(rest.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run `body` with a fresh directive pass seeded with `candidates`, isolating thread-local state.
    fn with_candidates<T>(candidates: &[&str], body: impl FnOnce() -> T) -> T {
        let owned: Vec<String> = candidates.iter().map(|s| s.to_string()).collect();
        begin_pass(&owned);
        let out = body();
        // Drain so a later test in the same thread starts clean.
        let _ = take_directive_references();
        out
    }

    #[test]
    fn renames_react_spellings() {
        assert_eq!(map_attribute_name("className"), "class");
        assert_eq!(map_attribute_name("htmlFor"), "for");
        assert_eq!(map_attribute_name("id"), "id");
        assert_eq!(map_attribute_name("data-x"), "data-x");
    }

    #[test]
    fn class_aliases_resolve_to_class_and_are_not_directives() {
        // `class` is preferred; `className` and `use:class` are accepted aliases.
        assert_eq!(map_attribute_name("class"), "class");
        assert_eq!(map_attribute_name("className"), "class");
        assert_eq!(map_attribute_name("use:class"), "class");
        // `use:class` must NOT be classified as a directive (it falls through to the class handler).
        assert!(classify_attribute("use:class", true).is_none());
        assert!(classify_attribute("use:class", false).is_none());
        // a genuine `use:` directive still classifies.
        assert!(classify_attribute("use:autofocus", false).is_some());
    }

    #[test]
    fn recognizes_event_handlers() {
        assert_eq!(event_name("onClick").as_deref(), Some("click"));
        assert_eq!(event_name("onInput").as_deref(), Some("input"));
        assert_eq!(event_name("onDblClick").as_deref(), Some("dblclick"));
        assert_eq!(event_name("onMouseEnter").as_deref(), Some("mouseenter"));
    }

    #[test]
    fn non_events_are_not_handlers() {
        // No `on` prefix.
        assert_eq!(event_name("class"), None);
        // `on` followed by a lower-case letter is a real attribute (e.g. `online`), not an event.
        assert_eq!(event_name("online"), None);
        // Bare `on`.
        assert_eq!(event_name("on"), None);
    }

    #[test]
    fn pascal_case_folds_words() {
        assert_eq!(pascal_case("tooltip"), "Tooltip");
        assert_eq!(pascal_case("auto-focus"), "AutoFocus");
        assert_eq!(pascal_case("my_dir"), "MyDir");
        assert_eq!(pascal_case("Autofocus"), "Autofocus");
    }

    #[test]
    fn primary_input_is_lower_camel_of_class() {
        assert_eq!(primary_input_name("Tooltip"), "tooltip");
        assert_eq!(primary_input_name("NgModel"), "ngModel");
        assert_eq!(primary_input_name("A"), "a");
    }

    #[test]
    fn classifies_namespace_form() {
        with_candidates(&[], || {
            // `use:autofocus` — value-less, applies `Autofocus`, no input bound.
            let app = classify_attribute("use:autofocus", false).expect("namespace directive");
            assert_eq!(app.class_name, "Autofocus");
            assert_eq!(app.input_name, None);
            assert!(!app.structural);

            // `use:tooltip={expr}` — applies `Tooltip`, binds the `tooltip` input.
            let app = classify_attribute("use:tooltip", true).expect("namespace directive");
            assert_eq!(app.class_name, "Tooltip");
            assert_eq!(app.input_name.as_deref(), Some("tooltip"));
            assert!(!app.structural);
        });
    }

    #[test]
    fn classifies_capitalized_form() {
        with_candidates(&[], || {
            // `<input Autofocus />` — value-less directive application.
            let app = classify_attribute("Autofocus", false).expect("capitalized directive");
            assert_eq!(app.class_name, "Autofocus");
            assert_eq!(app.input_name, None);
            assert!(!app.structural);

            // `<button Tooltip="hi">` — binds the lower-camel primary input `tooltip`.
            let app = classify_attribute("Tooltip", true).expect("capitalized directive");
            assert_eq!(app.class_name, "Tooltip");
            assert_eq!(app.input_name.as_deref(), Some("tooltip"));
        });
    }

    #[test]
    fn classifies_bare_lowercase_only_when_known() {
        // `tooltip` is a directive only when `Tooltip` is an imported candidate.
        with_candidates(&["Tooltip"], || {
            let app = classify_attribute("tooltip", true).expect("known directive");
            assert_eq!(app.class_name, "Tooltip");
            assert_eq!(app.input_name.as_deref(), Some("tooltip"));
            assert!(!app.structural);
        });
        // Without the import, a bare lowercase attribute stays a DOM attribute.
        with_candidates(&[], || {
            assert_eq!(classify_attribute("tooltip", true), None);
            assert_eq!(classify_attribute("href", false), None);
        });
    }

    #[test]
    fn classifies_structural_star_form() {
        with_candidates(&["Highlight"], || {
            let app = classify_attribute("*highlight", true).expect("structural directive");
            assert_eq!(app.class_name, "Highlight");
            assert_eq!(app.input_name.as_deref(), Some("highlight"));
            assert!(app.structural);
        });
        // The JSX-authorable spelling `structural:highlight` lowers identically to `*highlight`.
        with_candidates(&["Highlight"], || {
            let app = classify_attribute("structural:highlight", true).expect("structural directive");
            assert_eq!(app.class_name, "Highlight");
            assert_eq!(app.input_name.as_deref(), Some("highlight"));
            assert!(app.structural);
        });
        // A `*` / `structural:` whose name is not a known directive is not classified as a directive.
        with_candidates(&[], || {
            assert_eq!(classify_attribute("*unknown", true), None);
            assert_eq!(classify_attribute("structural:unknown", true), None);
        });
    }

    #[test]
    fn collects_references_in_first_seen_order() {
        with_candidates(&["Highlight"], || {
            register_directive_reference("Autofocus");
            register_directive_reference("Tooltip");
            register_directive_reference("Autofocus"); // duplicate ignored
            let refs = take_directive_references();
            assert_eq!(refs, vec!["Autofocus".to_string(), "Tooltip".to_string()]);
        });
    }
}
