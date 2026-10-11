//! `render3/util.ts` — shared render3 helper types and functions.
//!
//! PORT TARGET: `tools/angular-ref/packages/compiler/src/render3/util.ts`
//!
//! These are the small, dependency-free building blocks shared across the render3 emitters
//! (`factory.rs`, `pipe_module_injector.rs`, `view/compiler.rs`, …): the [`R3Reference`] /
//! [`R3CompiledExpression`] data shapes returned by every `compile*` emitter, plus the
//! [`type_with_parameters`] and [`ts_ignore_comment`] helpers.
//!
//! Field-name note: the TS `R3Reference.type` / `R3CompiledExpression.type` field is named `ty`
//! here because `type` is a reserved word in Rust. The value is an `output_ast` expression
//! ([`R3Reference`]) or [`Type`] ([`R3CompiledExpression`]) respectively.

use crate::output_ast::{self as o, Expr, LeadingComment, Stmt, Type};

/// `compiler_facade_interface.ts` `FactoryTarget` enum. Discriminants pinned to the Angular main (78d2a67d) source
/// (`Directive = 0 … Service = 5`). Shared across the render3 emitters (`factory.rs`,
/// `view/compiler.rs`, `pipe_module_injector.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactoryTarget {
    Directive = 0,
    Component = 1,
    Injectable = 2,
    Pipe = 3,
    NgModule = 4,
    Service = 5,
}

/// `core.ts` `InjectFlags` const enum (bitflags). `bitflags` is unavailable, so a plain `u8`
/// newtype with `BitOr`, mirroring how the source ORs flags together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InjectFlags(pub u8);

impl InjectFlags {
    pub const DEFAULT: InjectFlags = InjectFlags(0b0_0000);
    pub const HOST: InjectFlags = InjectFlags(1 << 0);
    pub const SELF: InjectFlags = InjectFlags(1 << 1);
    pub const SKIP_SELF: InjectFlags = InjectFlags(1 << 2);
    pub const OPTIONAL: InjectFlags = InjectFlags(1 << 3);
    /// `@internal` flag used for pipe-target dependencies.
    pub const FOR_PIPE: InjectFlags = InjectFlags(1 << 4);

    #[inline]
    pub fn bits(self) -> u8 {
        self.0
    }
}

impl std::ops::BitOr for InjectFlags {
    type Output = InjectFlags;
    fn bitor(self, rhs: InjectFlags) -> InjectFlags {
        InjectFlags(self.0 | rhs.0)
    }
}

/// `util.ts` `R3Reference { value, type }` — a pair of expressions: the runtime value and the
/// `.d.ts` type expression for a referenced symbol. `type` → `ty` (Rust keyword).
#[derive(Debug, Clone, PartialEq)]
pub struct R3Reference {
    pub value: Expr,
    pub ty: Expr,
}

impl R3Reference {
    /// Convenience constructor (`{value, type}`).
    pub fn new(value: Expr, ty: Expr) -> R3Reference {
        R3Reference { value, ty }
    }
}

/// `util.ts` `R3CompiledExpression { expression, type, statements }` — the result of compiling a
/// render3 code unit (component, directive, pipe, etc.): the def RHS expression, its `.d.ts` type,
/// and any extra top-level statements. `type` → `ty` (Rust keyword).
#[derive(Debug, Clone, PartialEq)]
pub struct R3CompiledExpression {
    pub expression: Expr,
    pub ty: Type,
    pub statements: Vec<Stmt>,
}

/// `util.ts` `typeWithParameters(type, numParams)` — `ExpressionType(type)` with `numParams`
/// `DYNAMIC_TYPE` type-arguments (none when `numParams === 0`).
pub fn type_with_parameters(ty: Expr, num_params: u32) -> Type {
    if num_params == 0 {
        o::expression_type(ty, None, None)
    } else {
        let params = (0..num_params).map(|_| o::dynamic_type()).collect();
        o::expression_type(ty, None, Some(params))
    }
}

/// `util.ts` `tsIgnoreComment()` — a leading, multiline `@ts-ignore` comment with a trailing
/// newline. It must sit on a *statement* (the newline would break a `return` if placed on an
/// expression).
pub fn ts_ignore_comment() -> LeadingComment {
    o::leading_comment("@ts-ignore", true, true)
}

/// `util.ts` `IDENTIFIER_PATTERN = /^[$A-Z_][0-9A-Z_$]*$/i`: whether `name` is a valid JavaScript
/// identifier for the purposes of `let` parameters and variable names. ASCII only, case-insensitive
/// (the regex has no `u` flag). The empty string is not an identifier.
pub fn is_identifier_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first == '$' || first == '_' || first.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '$' || c == '_' || c.is_ascii_alphanumeric())
}

/// `util.ts` `LET_PATTERN = /^let\s+([\S\s]*)/`: when `expression` starts with the keyword `let`
/// followed by at least one whitespace character, returns the captured remainder (the leading
/// whitespace run is consumed, as the greedy `\s+` does). Returns `None` otherwise.
pub fn match_let_pattern(expression: &str) -> Option<&str> {
    let rest = expression.strip_prefix("let")?;
    let trimmed = rest.trim_start_matches(char::is_whitespace);
    if trimmed.len() == rest.len() {
        // No whitespace followed `let` (e.g. `letter`, or a bare `let`): not a `let` parameter.
        return None;
    }
    Some(trimmed)
}

/// `util.ts` `namespaceCssVariable(varName)`: namespaces a CSS variable name. `--global--x` opts out
/// of namespacing (`--x`); any other `--x` becomes `--%NS%x`, which the component's style scoping
/// later replaces with the component's namespace. The single-hyphen `--global-` error is commented
/// out upstream (`g3-only`), so it is not raised here either.
pub fn namespace_css_variable(var_name: &str) -> String {
    match var_name.strip_prefix("--global--") {
        Some(rest) => format!("--{rest}"),
        None => format!("--%NS%{}", &var_name["--".len()..]),
    }
}

/// Port of `hyphenate` (`template/pipeline/src/phases/parse_extracted_styles.ts`): a `-` goes between
/// a lower-case letter and the upper-case letter after it, then the whole name is lower-cased, so
/// `backgroundColor` becomes `background-color`.
pub fn hyphenate(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    let mut out = String::with_capacity(chars.len() + 2);
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 && c.is_ascii_uppercase() && chars[i - 1].is_ascii_lowercase() {
            out.push('-');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// Port of `normalizeStylePropName` (`template/pipeline/src/phases/naming.ts`): a CSS custom property
/// (`--x`) keeps its name, any other style property name is hyphenated.
pub fn normalize_style_prop_name(name: &str) -> String {
    if name.starts_with("--") {
        name.to_string()
    } else {
        hyphenate(name)
    }
}

/// Port of `stripImportant` (`template/pipeline/src/phases/naming.ts`): drops everything from a
/// `!important` suffix on a style or class name.
pub fn strip_important(name: &str) -> &str {
    match name.find("!important") {
        Some(index) => &name[..index],
        None => name,
    }
}

#[cfg(test)]
mod style_prop_name_tests {
    use super::{hyphenate, normalize_style_prop_name, strip_important};

    #[test]
    fn hyphenates_camel_case_style_names_and_keeps_custom_properties() {
        assert_eq!(hyphenate("borderImage"), "border-image");
        assert_eq!(hyphenate("WIDTH"), "width");
        assert_eq!(normalize_style_prop_name("boxShadow"), "box-shadow");
        assert_eq!(normalize_style_prop_name("--myVar"), "--myVar");
    }

    #[test]
    fn strips_important_suffix() {
        assert_eq!(strip_important("width!important"), "width");
        assert_eq!(strip_important("color"), "color");
    }
}

#[cfg(test)]
mod namespace_css_variable_tests {
    use super::namespace_css_variable;

    #[test]
    fn namespaces_a_plain_variable() {
        assert_eq!(namespace_css_variable("--foo"), "--%NS%foo");
        assert_eq!(namespace_css_variable("--my-color"), "--%NS%my-color");
    }

    #[test]
    fn global_double_hyphen_opts_out_of_namespacing() {
        assert_eq!(namespace_css_variable("--global--mycolor"), "--mycolor");
    }

    #[test]
    fn single_hyphen_global_is_namespaced_as_a_plain_variable() {
        // The `--global-` single-hyphen form throws upstream only in the g3-internal build, so the
        // public behaviour is to namespace it like any other variable.
        assert_eq!(namespace_css_variable("--global-x"), "--%NS%global-x");
    }
}

#[cfg(test)]
mod let_and_identifier_tests {
    use super::*;

    #[test]
    fn identifier_pattern_accepts_js_identifiers() {
        assert!(is_identifier_name("foo"));
        assert!(is_identifier_name("$bar"));
        assert!(is_identifier_name("_baz9"));
        assert!(is_identifier_name("Qux_$1"));
    }

    #[test]
    fn identifier_pattern_rejects_non_identifiers() {
        assert!(!is_identifier_name(""));
        assert!(!is_identifier_name("9lives"));
        assert!(!is_identifier_name("a-b"));
        assert!(!is_identifier_name("a b"));
        assert!(!is_identifier_name("café"));
    }

    #[test]
    fn let_pattern_captures_remainder_after_whitespace() {
        assert_eq!(match_let_pattern("let  foo = $index"), Some("foo = $index"));
        assert_eq!(match_let_pattern("let\tbar"), Some("bar"));
        assert_eq!(match_let_pattern("let "), Some(""));
    }

    #[test]
    fn let_pattern_requires_keyword_then_whitespace() {
        assert_eq!(match_let_pattern("letter"), None);
        assert_eq!(match_let_pattern("let"), None);
        assert_eq!(match_let_pattern(" let x"), None);
        assert_eq!(match_let_pattern("x = let y"), None);
    }
}
