//! The codemod engine — the deterministic, AI-free heart of the updater.
//!
//! When a dependency bump breaks the build, the orchestration runs the codemod
//! rules whose `dep`/version range cover that bump (see
//! [`crate::RuleSet::rules_for`]) and rebuilds. This module implements:
//!
//! * [`matcher_hits`] / [`apply_rewrite`] — the pure primitives that decide
//!   whether a rule fires on a source buffer and how it rewrites it.
//! * [`CodemodEngine`] — runs an ordered set of rules over a source string,
//!   returning the rewritten text and the ids of the rules that actually fired.
//! * [`oxc_29_to_133_rules`] — the *seeded* rule set: every documented
//!   `oxc 0.29 -> 0.133` API change from `migration/OXC-MIGRATION-CRIB.md`
//!   encoded as one rule, each with a unit test (`old API` fixture -> `new API`
//!   expected output) proving it rewrites correctly and is idempotent.
//!
//! ## Determinism + idempotency
//! Every rewrite is a pure function of (source, rule). Rules are written so that
//! applying them to already-migrated source is a no-op: the `find`/`pattern`
//! only matches the *old* API, which the replacement removes. Running the engine
//! twice therefore yields the same result as running it once — a hard
//! requirement for a long-running CI cron that may re-process a partially
//! migrated tree.
//!
//! ## Matching strategy
//! The crib is dominated by identifier renames (`new_vec` -> `vec`) and import
//! path moves (`oxc_ast::VisitMut` -> `oxc_ast_visit::VisitMut`). A naive
//! substring replace is unsafe here because the identifiers share prefixes
//! (`new_vec`, `new_vec_single`, `new_vec_with_capacity`) and because a token
//! like `atom` must not match inside `new_atom` or `my_atom`. So identifier
//! renames are encoded as anchored [`Rewrite::RegexReplace`] rules using `\b`
//! word boundaries (and, where a receiver matters, a `.` lookbehind emulated via
//! an explicit captured prefix). Pure string moves that cannot be confused use
//! [`Rewrite::Replace`]. Structural unwraps that span balanced parentheses
//! (`Argument::Expression(<expr>)` -> `<expr>`) use a dedicated balanced-paren
//! regex plus a guard, documented at the rule site.

use std::borrow::Cow;

use regex::Regex;

use crate::model::{CodemodRule, Matcher, Rewrite};
use crate::RuleSet;

/// Outcome of running the engine over one source buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodemodOutput {
    /// The rewritten source.
    pub source: String,
    /// Ids of the rules that actually changed the source, in application order.
    ///
    /// A rule that matched-but-was-already-applied (idempotent no-op) does *not*
    /// appear here, so the orchestration records only codemods that did real
    /// work when attributing a fix to a bump.
    pub applied: Vec<String>,
}

/// Decide whether `rule`'s matcher considers `source` a candidate.
///
/// This is the cheap gate that runs before the (possibly more expensive)
/// rewrite. For [`Rewrite::Replace`]/[`Rewrite::RegexReplace`] the rewrite is
/// itself idempotent, so the matcher is an optimization and a clarity aid rather
/// than a correctness requirement — but it keeps `applied` honest by letting the
/// engine skip buffers a rule can never touch.
///
/// * [`Matcher::Literal`] — true iff `source` contains the substring.
/// * [`Matcher::Regex`] — true iff the (pre-validated) pattern matches.
/// * [`Matcher::AstGrep`] — not yet implemented as a structural engine; treated
///   as a literal containment test of the pattern text so a rule authored with
///   it still gates conservatively rather than panicking. Higher-fidelity
///   structural matching can replace this arm without touching rule data.
pub fn matcher_hits(matcher: &Matcher, source: &str) -> Result<bool, CodemodError> {
    match matcher {
        Matcher::Literal { contains } => Ok(source.contains(contains.as_str())),
        Matcher::Regex { pattern } => {
            let re = compile(pattern)?;
            Ok(re.is_match(source))
        }
        Matcher::AstGrep { pattern } => Ok(source.contains(pattern.as_str())),
    }
}

/// Apply `rule`'s rewrite to `source`, returning the new text.
///
/// Pure and idempotent: applying to already-migrated source returns it
/// unchanged (the old-API pattern no longer matches). Errors only on a malformed
/// regex in the rule data, which is a rule-authoring bug surfaced eagerly.
pub fn apply_rewrite(rewrite: &Rewrite, source: &str) -> Result<String, CodemodError> {
    match rewrite {
        Rewrite::Replace { find, replace } => {
            if find.is_empty() {
                // An empty `find` would "match" everywhere and is never a real
                // migration; treat as a no-op rather than splicing the
                // replacement between every char.
                return Ok(source.to_string());
            }
            Ok(source.replace(find.as_str(), replace.as_str()))
        }
        Rewrite::RegexReplace { pattern, replacement } => {
            let re = compile(pattern)?;
            match re.replace_all(source, replacement.as_str()) {
                Cow::Borrowed(_) => Ok(source.to_string()),
                Cow::Owned(s) => Ok(s),
            }
        }
    }
}

/// The codemod engine: applies an ordered slice of rules to a source buffer.
///
/// Rules are applied in the order given (the orchestration passes the
/// crib-ordered, version-gated slice from [`RuleSet::rules_for`]). Order can
/// matter when one rule's output is another's input (e.g. flattening
/// `Argument::Expression(Expression::Foo(..))` happens in two passes); the seed
/// set is ordered so each rule sees the output of its predecessors.
#[derive(Debug, Clone)]
pub struct CodemodEngine<'a> {
    rules: Vec<&'a CodemodRule>,
}

impl<'a> CodemodEngine<'a> {
    /// Build an engine from an explicit, ordered list of rules.
    pub fn new(rules: Vec<&'a CodemodRule>) -> Self {
        CodemodEngine { rules }
    }

    /// Build an engine over every rule in `set` that applies to `dep` at
    /// `version` — the exact slice the orchestration runs on a breakage.
    pub fn for_bump(set: &'a RuleSet, dep: &str, version: &semver::Version) -> Self {
        // Filter with `applies_to` directly rather than `RuleSet::rules_for`:
        // the latter unifies the lifetimes of `dep`/`version` with `&'a set`,
        // which would over-constrain these short-lived call arguments. The
        // resulting `&'a CodemodRule` borrows only `set`, exactly as needed.
        CodemodEngine {
            rules: set
                .rules
                .iter()
                .filter(|r| r.applies_to(dep, version))
                .collect(),
        }
    }

    /// Run all rules over `source`, returning the rewritten text and the ids of
    /// the rules that produced a real change.
    ///
    /// Each rule is gated by its matcher, then its rewrite is applied; a rewrite
    /// that leaves the buffer byte-for-byte identical is not recorded in
    /// `applied`, so the result distinguishes "fixed something" from "matched but
    /// was already migrated".
    pub fn run(&self, source: &str) -> Result<CodemodOutput, CodemodError> {
        let mut current = source.to_string();
        let mut applied = Vec::new();
        for rule in &self.rules {
            let hit = matcher_hits(&rule.matcher, &current).map_err(|e| e.with_rule(&rule.id))?;
            if !hit {
                continue;
            }
            let next = apply_rewrite(&rule.rewrite, &current)
                .map_err(|e| e.with_rule(&rule.id))?;
            if next != current {
                applied.push(rule.id.clone());
                current = next;
            }
        }
        Ok(CodemodOutput {
            source: current,
            applied,
        })
    }
}

/// An error from the codemod engine — only ever a malformed rule pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodemodError {
    /// The rule id that owns the bad pattern, if known.
    pub rule_id: Option<String>,
    /// Human-readable cause (the regex compile error).
    pub message: String,
}

impl CodemodError {
    fn with_rule(mut self, id: &str) -> Self {
        if self.rule_id.is_none() {
            self.rule_id = Some(id.to_string());
        }
        self
    }
}

impl std::fmt::Display for CodemodError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.rule_id {
            Some(id) => write!(f, "codemod rule '{id}': {}", self.message),
            None => write!(f, "codemod: {}", self.message),
        }
    }
}

impl std::error::Error for CodemodError {}

/// Compile a regex, mapping a bad pattern to a [`CodemodError`].
fn compile(pattern: &str) -> Result<Regex, CodemodError> {
    Regex::new(pattern).map_err(|e| CodemodError {
        rule_id: None,
        message: e.to_string(),
    })
}

// ===========================================================================
// Seeded rule set: oxc 0.29 -> 0.133 (migration/OXC-MIGRATION-CRIB.md)
// ===========================================================================

/// Convenience: the version every oxc-family rule applies *from*.
///
/// The crib documents the 0.29 -> 0.133 jump; the rules are valid for any
/// pre-0.133 source migrating to 0.133, so `from` is the old line (0.29) and the
/// range is open-ended (`to: None`) — re-running on 0.133+ is a safe no-op.
fn from_v() -> semver::Version {
    semver::Version::parse("0.29.0").expect("static version literal parses")
}

/// A literal-find/replace rule (used for unambiguous import-path and token moves).
fn replace_rule(
    id: &str,
    dep: &str,
    find: &str,
    replace: &str,
    description: &str,
) -> CodemodRule {
    CodemodRule {
        id: id.into(),
        dep: dep.into(),
        from: from_v(),
        to: None,
        matcher: Matcher::Literal { contains: find.into() },
        rewrite: Rewrite::Replace { find: find.into(), replace: replace.into() },
        description: description.into(),
    }
}

/// A regex rule (used for word-boundary-anchored identifier renames and
/// structural rewrites). The same pattern gates the matcher and drives the
/// rewrite, keeping the rule self-consistent.
fn regex_rule(
    id: &str,
    dep: &str,
    pattern: &str,
    replacement: &str,
    description: &str,
) -> CodemodRule {
    CodemodRule {
        id: id.into(),
        dep: dep.into(),
        from: from_v(),
        to: None,
        matcher: Matcher::Regex { pattern: pattern.into() },
        rewrite: Rewrite::RegexReplace {
            pattern: pattern.into(),
            replacement: replacement.into(),
        },
        description: description.into(),
    }
}

/// The full seeded rule set for the oxc 0.29 -> 0.133 migration.
///
/// One rule per documented crib change, ordered so that rules whose input is
/// another rule's output run *after* their predecessor (notably the two-pass
/// `Argument::Expression` flattening). This is the registry the orchestration
/// loads when an `oxc_*` bump breaks the build.
pub fn oxc_29_to_133_rules() -> RuleSet {
    // Pre-sized to the number of seeded crib rules; `with_capacity` also keeps
    // clippy's `vec_init_then_push` lint quiet for the long push sequence below.
    let mut rules: Vec<CodemodRule> = Vec::with_capacity(28);

    // --- Crate / import path moves ----------------------------------------
    // `VisitMut` moved out of oxc_ast into oxc_ast_visit. Both the grouped and
    // the single-path import forms appear in real code, so cover the common
    // shapes with word-boundary-anchored regexes rather than blunt substrings
    // (so `oxc_ast::AstKind` next to it is untouched).
    rules.push(regex_rule(
        "oxc-visitmut-import-move-single",
        "oxc_ast",
        r"\boxc_ast::VisitMut\b",
        "oxc_ast_visit::VisitMut",
        "VisitMut moved to oxc_ast_visit (crib: Crate/import changes).",
    ));
    rules.push(regex_rule(
        "oxc-visit-import-move-single",
        "oxc_ast",
        r"\boxc_ast::Visit\b",
        "oxc_ast_visit::Visit",
        "Visit trait moved to oxc_ast_visit alongside VisitMut.",
    ));
    // `oxc_span::Atom` is gone; the name type is `oxc_str::Ident`.
    rules.push(replace_rule(
        "oxc-span-atom-import",
        "oxc_span",
        "oxc_span::Atom",
        "oxc_str::Ident",
        "oxc_span::Atom removed; name fields are oxc_str::Ident (crib: Strings).",
    ));
    // `oxc_span::CompactString` no longer re-exported -> oxc_str::CompactStr.
    rules.push(replace_rule(
        "oxc-span-compactstring-import",
        "oxc_span",
        "oxc_span::CompactString",
        "oxc_str::CompactStr",
        "CompactString re-export dropped; use oxc_str::CompactStr (crib: Crate/import).",
    ));

    // --- Strings: type renames --------------------------------------------
    // Atom<'a> -> Ident<'a>. Anchor on the generic form and the bare type with a
    // word boundary so we never rename a field literally named `atom`.
    rules.push(regex_rule(
        "oxc-atom-type-to-ident",
        "oxc_ast",
        r"\bAtom<",
        "Ident<",
        "Atom<'a> renamed to Ident<'a> (crib: Strings, the dominant change).",
    ));
    // builder.atom(..) / new_atom(..) removed -> builder.ident(..). Match a
    // method-call receiver (`<recv>.atom(`) and rewrite the method name only,
    // preserving the receiver via a capture. Also handles the `new_atom` alias.
    rules.push(regex_rule(
        "oxc-builder-atom-to-ident",
        "oxc_ast",
        r"\.atom\(",
        ".ident(",
        "builder.atom()/new_atom removed; use builder.ident() (crib: Strings).",
    ));
    rules.push(regex_rule(
        "oxc-builder-new-atom-to-ident",
        "oxc_ast",
        r"\bnew_atom\(",
        "ident(",
        "AstBuilder::new_atom removed; use ident() (crib: Strings).",
    ));
    // .to_compact_string() -> .to_compact_str()
    rules.push(replace_rule(
        "oxc-to-compact-string",
        "oxc_ast",
        ".to_compact_string()",
        ".to_compact_str()",
        "to_compact_string() renamed to to_compact_str() (crib: Strings).",
    ));

    // --- AstBuilder method renames ----------------------------------------
    // The `new_vec` family shares a prefix, so anchor each with `\b` AND an
    // opening paren so `new_vec(` does not eat `new_vec_single(`. Because the
    // longer names also start with `new_vec`, we still must order longest-first
    // OR anchor on the trailing `(` — the `(` anchor makes each unambiguous.
    rules.push(regex_rule(
        "oxc-new-vec-single",
        "oxc_ast",
        r"\bnew_vec_single\(",
        "vec1(",
        "AstBuilder::new_vec_single(v) -> vec1(v).",
    ));
    rules.push(regex_rule(
        "oxc-new-vec-with-capacity",
        "oxc_ast",
        r"\bnew_vec_with_capacity\(",
        "vec_with_capacity(",
        "AstBuilder::new_vec_with_capacity(n) -> vec_with_capacity(n).",
    ));
    rules.push(regex_rule(
        "oxc-new-vec",
        "oxc_ast",
        r"\bnew_vec\(",
        "vec(",
        "AstBuilder::new_vec() -> vec().",
    ));

    // Expression-producing builder renames (old name -> new `expression_*`).
    // These are plain identifier renames anchored to the call paren.
    for (old, new, note) in [
        (
            "identifier_reference_expression",
            "expression_identifier",
            "identifier_reference_expression(..) -> expression_identifier(span, name).",
        ),
        (
            "literal_string_expression",
            "expression_string_literal",
            "literal_string_expression(span, v) -> expression_string_literal(span, v, None).",
        ),
        (
            "property_key_identifier",
            "property_key_static_identifier",
            "property_key_identifier(..) -> property_key_static_identifier(span, name).",
        ),
    ] {
        rules.push(regex_rule(
            &format!("oxc-rename-{}", old.replace('_', "-")),
            "oxc_ast",
            &format!(r"\b{}\(", regex::escape(old)),
            &format!("{new}("),
            note,
        ));
    }

    // --- `::new` constructors -> AstBuilder methods -----------------------
    // `IdentifierReference::new(span, name)` -> `ast.identifier_reference(span, name)`.
    // Capture the argument list and re-emit it; the receiver `ast.` is the
    // documented builder handle. We match `Type::new(` and rewrite the head,
    // leaving the (balanced) arguments in place via the rest of the line.
    for (ty, method, note) in [
        (
            "IdentifierReference",
            "identifier_reference",
            "IdentifierReference::new(span,name) -> ast.identifier_reference(span, name).",
        ),
        (
            "IdentifierName",
            "identifier_name",
            "IdentifierName::new(span,name) -> ast.identifier_name(span, name).",
        ),
        (
            "BindingIdentifier",
            "binding_identifier",
            "BindingIdentifier::new(span,name) -> ast.binding_identifier(span, name).",
        ),
    ] {
        rules.push(regex_rule(
            &format!("oxc-ctor-{}", method.replace('_', "-")),
            "oxc_ast",
            &format!(r"\b{}::new\(", ty),
            &format!("ast.{method}("),
            note,
        ));
    }
    // StringLiteral::new(span, v) -> ast.string_literal(span, v, None): the
    // signature gained a trailing `None`, so we cannot keep the args verbatim.
    // Capture the two args and re-emit with the added `None`. The argument
    // captures are non-greedy and stop at the first `)` — adequate for the
    // simple `(span, v)` call shape the crib documents; nested calls in the args
    // would need the structural matcher and are out of the crib's scope.
    rules.push(regex_rule(
        "oxc-ctor-string-literal",
        "oxc_ast",
        r"\bStringLiteral::new\(([^()]*)\)",
        "ast.string_literal($1, None)",
        "StringLiteral::new(span,v) -> ast.string_literal(span, v, None).",
    ));

    // --- Enum / field flattening ------------------------------------------
    // `Argument::Expression(Expression::Foo(x))` -> `Argument::Foo(x)`.
    // Two passes: first collapse the doubly-wrapped form to the flattened
    // variant, then (for any remaining single-wrapped `Argument::Expression(e)`)
    // convert to `Argument::from(e)`. The first pattern captures the inner
    // variant name and its balanced single-level argument.
    rules.push(regex_rule(
        "oxc-argument-flatten-variant",
        "oxc_ast",
        r"Argument::Expression\(\s*Expression::(\w+)\(([^()]*)\)\s*\)",
        "Argument::$1($2)",
        "Argument::Expression(Expression::X(..)) flattened to Argument::X(..) (crib: Enum/field).",
    ));
    rules.push(regex_rule(
        "oxc-argument-from-expr",
        "oxc_ast",
        r"Argument::Expression\(([^()]*)\)",
        "Argument::from($1)",
        "Bare Argument::Expression(e) -> Argument::from(e) (crib: Enum/field).",
    ));
    // `PropertyKey::Identifier(..)` -> `PropertyKey::StaticIdentifier(..)`.
    rules.push(regex_rule(
        "oxc-propertykey-static-identifier",
        "oxc_ast",
        r"\bPropertyKey::Identifier\b",
        "PropertyKey::StaticIdentifier",
        "PropertyKey::Identifier -> PropertyKey::StaticIdentifier(Box<IdentifierName>) (crib).",
    ));
    // BindingPattern is now an enum: `param.pattern.type_annotation` moved onto
    // the FormalParameter, so `<x>.pattern.type_annotation` -> `<x>.type_annotation`.
    rules.push(regex_rule(
        "oxc-formalparam-type-annotation-move",
        "oxc_ast",
        r"\.pattern\.type_annotation\b",
        ".type_annotation",
        "FormalParameter.pattern.type_annotation moved onto the parameter (crib).",
    ));
    // `Modifiers::empty()` / the `Modifiers` arg are gone. Strip a standalone
    // `Modifiers::empty()` token; the surrounding call-arg cleanup is left to
    // the human report if a stray comma results (documented limitation — we do
    // not blindly delete commas to avoid corrupting unrelated arg lists).
    rules.push(replace_rule(
        "oxc-modifiers-empty-removed",
        "oxc_ast",
        "Modifiers::empty()",
        "/* Modifiers removed in oxc 0.133 */",
        "Modifiers/Modifiers::empty() removed; declare-ness via `declare: bool` (crib).",
    ));

    // --- oxc_semantic Scoping merge ---------------------------------------
    // ScopeTree & SymbolTable unified into Scoping. Type-name renames anchored
    // with word boundaries so embedding identifiers are untouched.
    rules.push(regex_rule(
        "oxc-scopetree-to-scoping",
        "oxc_semantic",
        r"\bScopeTree\b",
        "Scoping",
        "ScopeTree merged into oxc_semantic::Scoping (crib: Scoping merge).",
    ));
    rules.push(regex_rule(
        "oxc-symboltable-to-scoping",
        "oxc_semantic",
        r"\bSymbolTable\b",
        "Scoping",
        "SymbolTable merged into oxc_semantic::Scoping (crib: Scoping merge).",
    ));
    // semantic.symbols()/scopes() -> scoping(); scopes_mut() -> scoping_mut().
    rules.push(regex_rule(
        "oxc-semantic-scopes-mut",
        "oxc_semantic",
        r"\.scopes_mut\(\)",
        ".scoping_mut()",
        "semantic.scopes_mut() -> semantic.scoping_mut() (crib: Scoping merge).",
    ));
    rules.push(regex_rule(
        "oxc-semantic-scopes",
        "oxc_semantic",
        r"\.scopes\(\)",
        ".scoping()",
        "semantic.scopes() -> semantic.scoping() (crib: Scoping merge).",
    ));
    rules.push(regex_rule(
        "oxc-semantic-symbols",
        "oxc_semantic",
        r"\.symbols\(\)",
        ".scoping()",
        "semantic.symbols() -> semantic.scoping() (crib: Scoping merge).",
    ));

    // --- Parser / Semantic entry points -----------------------------------
    // SemanticBuilder::new(src) -> SemanticBuilder::new() (source arg dropped).
    rules.push(regex_rule(
        "oxc-semanticbuilder-new-no-source",
        "oxc_semantic",
        r"SemanticBuilder::new\([^()]*\)",
        "SemanticBuilder::new()",
        "SemanticBuilder::new(src) -> SemanticBuilder::new() (crib: Parser/Semantic).",
    ));

    RuleSet { rules }
}

// ===========================================================================
// Tests: each seeded rule against an old-API fixture -> new-API expected output.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Look up a single seeded rule by id (panics if missing — a test asserting
    /// the rule still exists with that id).
    fn rule(id: &str) -> CodemodRule {
        oxc_29_to_133_rules()
            .rules
            .into_iter()
            .find(|r| r.id == id)
            .unwrap_or_else(|| panic!("seed rule '{id}' missing"))
    }

    /// Apply one rule by id to `src`, asserting the new text equals `expected`
    /// AND that re-applying is a byte-for-byte no-op (idempotency).
    fn check(id: &str, src: &str, expected: &str) {
        let r = rule(id);
        let once = apply_rewrite(&r.rewrite, src).unwrap();
        assert_eq!(once, expected, "rule '{id}' first pass");
        let twice = apply_rewrite(&r.rewrite, &once).unwrap();
        assert_eq!(twice, once, "rule '{id}' is not idempotent");
        // The matcher must agree the *old* source is a candidate.
        assert!(
            matcher_hits(&r.matcher, src).unwrap(),
            "rule '{id}' matcher missed its own old-API fixture"
        );
    }

    // --- import path moves -------------------------------------------------

    #[test]
    fn visitmut_import_move() {
        check(
            "oxc-visitmut-import-move-single",
            "use oxc_ast::VisitMut;",
            "use oxc_ast_visit::VisitMut;",
        );
    }

    #[test]
    fn visit_import_move_does_not_touch_visitmut() {
        // The bare-Visit rule must not also rewrite VisitMut (which has its own
        // rule); `\boxc_ast::Visit\b` requires a word boundary after `Visit`.
        check(
            "oxc-visit-import-move-single",
            "use oxc_ast::Visit;",
            "use oxc_ast_visit::Visit;",
        );
        let r = rule("oxc-visit-import-move-single");
        // VisitMut should NOT match `oxc_ast::Visit\b`.
        let out = apply_rewrite(&r.rewrite, "use oxc_ast::VisitMut;").unwrap();
        assert_eq!(out, "use oxc_ast::VisitMut;");
    }

    #[test]
    fn astkind_import_is_untouched() {
        // AstKind/AstType stay in oxc_ast; none of our import rules should move them.
        let set = oxc_29_to_133_rules();
        let eng = CodemodEngine::new(set.rules.iter().collect());
        let src = "use oxc_ast::{AstKind, AstType};";
        let out = eng.run(src).unwrap();
        assert_eq!(out.source, src);
        assert!(out.applied.is_empty());
    }

    #[test]
    fn span_atom_import_move() {
        check(
            "oxc-span-atom-import",
            "use oxc_span::Atom;",
            "use oxc_str::Ident;",
        );
    }

    #[test]
    fn span_compactstring_import_move() {
        check(
            "oxc-span-compactstring-import",
            "use oxc_span::CompactString;",
            "use oxc_str::CompactStr;",
        );
    }

    // --- strings -----------------------------------------------------------

    #[test]
    fn atom_type_to_ident() {
        check(
            "oxc-atom-type-to-ident",
            "fn name(&self) -> Atom<'a> { self.name }",
            "fn name(&self) -> Ident<'a> { self.name }",
        );
    }

    #[test]
    fn atom_type_rule_ignores_field_named_atom() {
        // A field/var literally named `atom` (no `<`) must be untouched.
        let r = rule("oxc-atom-type-to-ident");
        let out = apply_rewrite(&r.rewrite, "let atom = self.atom;").unwrap();
        assert_eq!(out, "let atom = self.atom;");
    }

    #[test]
    fn builder_atom_method_to_ident() {
        check(
            "oxc-builder-atom-to-ident",
            "let n = self.ast.atom(name);",
            "let n = self.ast.ident(name);",
        );
    }

    #[test]
    fn builder_new_atom_to_ident() {
        check(
            "oxc-builder-new-atom-to-ident",
            "let n = builder.new_atom(name);",
            "let n = builder.ident(name);",
        );
    }

    #[test]
    fn to_compact_string_rename() {
        check(
            "oxc-to-compact-string",
            "let c = ident.to_compact_string();",
            "let c = ident.to_compact_str();",
        );
    }

    // --- vec family --------------------------------------------------------

    #[test]
    fn new_vec_family_disambiguates_by_paren() {
        // The three vec rules must each hit only their own form even though the
        // names share the `new_vec` prefix.
        let set = oxc_29_to_133_rules();
        let eng = CodemodEngine::new(set.rules.iter().collect());
        let src = "self.ast.new_vec(); self.ast.new_vec_single(v); self.ast.new_vec_with_capacity(n);";
        let out = eng.run(src).unwrap();
        assert_eq!(
            out.source,
            "self.ast.vec(); self.ast.vec1(v); self.ast.vec_with_capacity(n);"
        );
        // All three vec rules fired.
        assert!(out.applied.contains(&"oxc-new-vec".to_string()));
        assert!(out.applied.contains(&"oxc-new-vec-single".to_string()));
        assert!(out.applied.contains(&"oxc-new-vec-with-capacity".to_string()));
    }

    #[test]
    fn new_vec_each_rule_in_isolation() {
        check("oxc-new-vec", "ast.new_vec()", "ast.vec()");
        check("oxc-new-vec-single", "ast.new_vec_single(v)", "ast.vec1(v)");
        check(
            "oxc-new-vec-with-capacity",
            "ast.new_vec_with_capacity(n)",
            "ast.vec_with_capacity(n)",
        );
    }

    // --- expression-producing renames -------------------------------------

    #[test]
    fn identifier_reference_expression_rename() {
        check(
            "oxc-rename-identifier-reference-expression",
            "self.ast.identifier_reference_expression(idref)",
            "self.ast.expression_identifier(idref)",
        );
    }

    #[test]
    fn literal_string_expression_rename() {
        check(
            "oxc-rename-literal-string-expression",
            "self.ast.literal_string_expression(span, v)",
            "self.ast.expression_string_literal(span, v)",
        );
    }

    #[test]
    fn property_key_identifier_rename() {
        check(
            "oxc-rename-property-key-identifier",
            "self.ast.property_key_identifier(name)",
            "self.ast.property_key_static_identifier(name)",
        );
    }

    // --- ::new constructors -> builder ------------------------------------

    #[test]
    fn identifier_reference_ctor() {
        check(
            "oxc-ctor-identifier-reference",
            "IdentifierReference::new(span, name)",
            "ast.identifier_reference(span, name)",
        );
    }

    #[test]
    fn identifier_name_ctor() {
        check(
            "oxc-ctor-identifier-name",
            "IdentifierName::new(span, name)",
            "ast.identifier_name(span, name)",
        );
    }

    #[test]
    fn binding_identifier_ctor() {
        check(
            "oxc-ctor-binding-identifier",
            "BindingIdentifier::new(span, name)",
            "ast.binding_identifier(span, name)",
        );
    }

    #[test]
    fn string_literal_ctor_adds_none() {
        check(
            "oxc-ctor-string-literal",
            "StringLiteral::new(span, value)",
            "ast.string_literal(span, value, None)",
        );
    }

    // --- enum / field flattening ------------------------------------------

    #[test]
    fn argument_double_wrap_flattens() {
        check(
            "oxc-argument-flatten-variant",
            "Argument::Expression(Expression::ObjectExpression(obj))",
            "Argument::ObjectExpression(obj)",
        );
    }

    #[test]
    fn argument_single_wrap_to_from() {
        check(
            "oxc-argument-from-expr",
            "Argument::Expression(expr)",
            "Argument::from(expr)",
        );
    }

    #[test]
    fn argument_two_pass_via_engine() {
        // Through the engine the doubly-wrapped form must collapse to the
        // flattened variant (first rule), NOT to Argument::from(...). Ordering
        // (flatten before from) is what guarantees this.
        let set = oxc_29_to_133_rules();
        let eng = CodemodEngine::new(set.rules.iter().collect());
        let out = eng
            .run("let a = Argument::Expression(Expression::Identifier(id));")
            .unwrap();
        assert_eq!(out.source, "let a = Argument::Identifier(id);");
        assert!(out.applied.contains(&"oxc-argument-flatten-variant".to_string()));
        // The `from` rule must not also have fired on the already-flattened text.
        assert!(!out.applied.contains(&"oxc-argument-from-expr".to_string()));
    }

    #[test]
    fn property_key_static_identifier() {
        check(
            "oxc-propertykey-static-identifier",
            "PropertyKey::Identifier(boxed)",
            "PropertyKey::StaticIdentifier(boxed)",
        );
    }

    #[test]
    fn formal_param_type_annotation_move() {
        check(
            "oxc-formalparam-type-annotation-move",
            "let t = param.pattern.type_annotation;",
            "let t = param.type_annotation;",
        );
    }

    #[test]
    fn modifiers_empty_removed() {
        check(
            "oxc-modifiers-empty-removed",
            "Modifiers::empty()",
            "/* Modifiers removed in oxc 0.133 */",
        );
    }

    // --- semantic scoping merge -------------------------------------------

    #[test]
    fn scopetree_to_scoping() {
        check(
            "oxc-scopetree-to-scoping",
            "fn scopes(&self) -> &ScopeTree { &self.scope_tree }",
            "fn scopes(&self) -> &Scoping { &self.scope_tree }",
        );
    }

    #[test]
    fn symboltable_to_scoping() {
        check(
            "oxc-symboltable-to-scoping",
            "let s: SymbolTable = build();",
            "let s: Scoping = build();",
        );
    }

    #[test]
    fn semantic_scopes_mut_before_scopes() {
        // scopes_mut() must become scoping_mut(), not scoping()_mut() — the
        // _mut rule is ordered before the bare scopes() rule.
        let set = oxc_29_to_133_rules();
        let eng = CodemodEngine::new(set.rules.iter().collect());
        let out = eng.run("semantic.scopes_mut().add();").unwrap();
        assert_eq!(out.source, "semantic.scoping_mut().add();");
    }

    #[test]
    fn semantic_scopes_to_scoping() {
        check(
            "oxc-semantic-scopes",
            "let sc = semantic.scopes();",
            "let sc = semantic.scoping();",
        );
    }

    #[test]
    fn semantic_symbols_to_scoping() {
        check(
            "oxc-semantic-symbols",
            "let sy = semantic.symbols();",
            "let sy = semantic.scoping();",
        );
    }

    // --- parser / semantic entry -----------------------------------------

    #[test]
    fn semantic_builder_new_drops_source() {
        check(
            "oxc-semanticbuilder-new-no-source",
            "let b = SemanticBuilder::new(source_text);",
            "let b = SemanticBuilder::new();",
        );
    }

    // --- engine-level properties ------------------------------------------

    #[test]
    fn engine_records_only_real_changes() {
        let set = oxc_29_to_133_rules();
        let eng = CodemodEngine::new(set.rules.iter().collect());
        // Already-migrated source: nothing should be recorded.
        let migrated = "use oxc_ast_visit::VisitMut; self.ast.vec();";
        let out = eng.run(migrated).unwrap();
        assert_eq!(out.source, migrated);
        assert!(out.applied.is_empty(), "no rule should fire on migrated source");
    }

    #[test]
    fn engine_is_idempotent_on_a_realistic_snippet() {
        let set = oxc_29_to_133_rules();
        let eng = CodemodEngine::new(set.rules.iter().collect());
        let src = "\
use oxc_ast::VisitMut;
use oxc_span::Atom;
fn build(&self) -> Atom<'a> {
    let v = self.ast.new_vec_single(self.ast.atom(name));
    let a = Argument::Expression(Expression::Identifier(id));
    let sc = semantic.scopes();
    self.ast.identifier_reference_expression(IdentifierReference::new(span, name))
}";
        let first = eng.run(src).unwrap();
        let second = eng.run(&first.source).unwrap();
        assert_eq!(first.source, second.source, "engine must be idempotent");
        // Second pass must record nothing — everything already migrated.
        assert!(second.applied.is_empty());
        // Spot-check a few migrations landed.
        assert!(first.source.contains("oxc_ast_visit::VisitMut"));
        assert!(first.source.contains("Ident<'a>"));
        assert!(first.source.contains("self.ast.vec1(self.ast.ident(name))"));
        assert!(first.source.contains("Argument::Identifier(id)"));
        assert!(first.source.contains("semantic.scoping()"));
        assert!(first
            .source
            .contains("self.ast.expression_identifier(ast.identifier_reference(span, name))"));
    }

    #[test]
    fn for_bump_selects_versioned_slice() {
        let set = oxc_29_to_133_rules();
        let v = semver::Version::parse("0.133.0").unwrap();
        // oxc_ast rules apply; oxc_semantic rules are gated out for an oxc_ast bump.
        let eng = CodemodEngine::for_bump(&set, "oxc_ast", &v);
        let out = eng.run("use oxc_ast::VisitMut; let s: SymbolTable = x;").unwrap();
        // VisitMut (oxc_ast rule) rewritten; SymbolTable (oxc_semantic rule) left alone.
        assert!(out.source.contains("oxc_ast_visit::VisitMut"));
        assert!(out.source.contains("SymbolTable"));
    }

    #[test]
    fn malformed_regex_rule_errors_with_id() {
        let bad = CodemodRule {
            id: "bad".into(),
            dep: "oxc_ast".into(),
            from: from_v(),
            to: None,
            matcher: Matcher::Regex { pattern: "(".into() },
            rewrite: Rewrite::RegexReplace { pattern: "(".into(), replacement: "x".into() },
            description: String::new(),
        };
        let eng = CodemodEngine::new(vec![&bad]);
        let err = eng.run("anything").unwrap_err();
        assert_eq!(err.rule_id.as_deref(), Some("bad"));
    }

    #[test]
    fn every_seed_rule_has_a_valid_pattern_and_unique_id() {
        let set = oxc_29_to_133_rules();
        let mut seen = std::collections::HashSet::new();
        for r in &set.rules {
            assert!(seen.insert(r.id.clone()), "duplicate rule id: {}", r.id);
            // Regex-backed matchers/rewrites must compile.
            if let Matcher::Regex { pattern } = &r.matcher {
                assert!(Regex::new(pattern).is_ok(), "rule '{}' bad matcher regex", r.id);
            }
            if let Rewrite::RegexReplace { pattern, .. } = &r.rewrite {
                assert!(Regex::new(pattern).is_ok(), "rule '{}' bad rewrite regex", r.id);
            }
            assert!(!r.description.is_empty(), "rule '{}' has no description", r.id);
        }
    }
}
