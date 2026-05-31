//! Pillar 3 (mechanical codegen): parse an Angular TypeScript source with oxc and EMIT Rust for
//! the **mechanical subset only** — the parts of `packages/compiler` that are pure data and can be
//! transpiled deterministically, byte-for-byte, with no judgement calls:
//!
//!   * (a) the `ɵɵ*` instruction name table from `render3/r3_identifiers.ts` — the `Identifiers`
//!     class of `static x: o.ExternalReference = {name: 'ɵɵx', moduleName: CORE}` fields — emitted
//!     as a Rust `&[(&str, &str)]` const table (field name -> wire name).
//!   * (b) numeric enums / flags (`AttributeMarker`, `SelectorFlags`, `ChangeDetectionStrategy`,
//!     `ViewEncapsulation`, `RenderFlags`, …) — emitted as Rust `#[repr(i64)] enum`s whose
//!     discriminants are IDENTICAL to the TS, evaluating `1 << 0`, `0b1000`, `A | B`, member
//!     back-references, etc. as TS would.
//!   * (c) simple string/number lookup tables — `export const T = {a: 1, b: 2}` /
//!     `export const T = ['a', 'b']` — emitted as Rust `&[(&str, i64)]` / `&[&str]` consts.
//!
//! Everything else is **REFUSED**: the emitter never guesses. A construct outside the subset (a
//! function body, an object with a non-literal value, a string enum, a member reference it cannot
//! resolve, …) becomes an [`Unsupported`] entry in the [`CodegenReport`] and is omitted from the
//! emitted Rust. Determinism over coverage (see `migration/RENDER3-SYNC-PLAN.md`).
//!
//! [`diff_against`] compares the emitted Rust against a committed Rust source so the harness can
//! prove the port is still 1:1 (pillar-3 verification) or surface the exact textual drift.

use std::fmt::Write as _;

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    BindingPattern, ClassElement, Declaration, Expression, ObjectPropertyKind, PropertyKey,
    Statement, TSEnumMemberName,
};
use oxc_parser::Parser;
use oxc_span::SourceType;
use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};

/// What kind of mechanical artifact the emitter produced for a top-level export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmittedKind {
    /// A numeric enum / flag set -> Rust `#[repr(i64)] enum`.
    Enum,
    /// The `Identifiers` `ɵɵ*` table -> Rust `&[(&str, &str)]` const.
    IdentifierTable,
    /// `const T = {a: 1, ...}` -> Rust `&[(&str, i64)]` const.
    NumberMap,
    /// `const T = ['a', ...]` -> Rust `&[&str]` const.
    StringList,
}

/// One successfully emitted mechanical artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Emitted {
    /// The TS export name (and the Rust item name).
    pub name: String,
    /// Which mechanical shape it matched.
    pub kind: EmittedKind,
    /// The emitted Rust source for this item (also concatenated into [`CodegenReport::rust`]).
    pub rust: String,
}

/// A construct the emitter refused to transpile — recorded, never guessed at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unsupported {
    /// The export name (or a synthetic label when anonymous).
    pub name: String,
    /// Why it is outside the mechanical subset.
    pub reason: String,
}

/// The deterministic output of [`emit_rust`]: the emitted Rust plus a structured ledger of what
/// was emitted and what was refused. A human (or a gated step) acts on `unsupported`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CodegenReport {
    /// The concatenated emitted Rust for every supported export, in source order.
    pub rust: String,
    /// Per-export emission records (supported subset).
    pub emitted: Vec<Emitted>,
    /// Per-export refusals (outside the subset). Non-empty ⇒ the file is not fully mechanical.
    pub unsupported: Vec<Unsupported>,
}

impl CodegenReport {
    /// True when every top-level export was mechanically emitted (no refusals).
    pub fn is_fully_mechanical(&self) -> bool {
        self.unsupported.is_empty()
    }
}

/// Parse `source` as TypeScript and emit Rust for the mechanical subset, recording refusals.
///
/// The parse is best-effort: a recoverable parse error still emits whatever top-level exports
/// parsed cleanly. A hard parse failure (no usable program) yields a single [`Unsupported`].
pub fn emit_rust(source: &str) -> CodegenReport {
    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true);
    let ret = Parser::new(&allocator, source, source_type).parse();

    let mut report = CodegenReport::default();

    if ret.panicked {
        report.unsupported.push(Unsupported {
            name: "<file>".into(),
            reason: "oxc could not parse the TypeScript source".into(),
        });
        return report;
    }

    for stmt in &ret.program.body {
        // Only top-level *exported* declarations are part of the port surface.
        let Statement::ExportNamedDeclaration(export) = stmt else {
            continue;
        };
        let Some(decl) = &export.declaration else {
            continue;
        };
        emit_declaration(decl, &mut report);
    }

    // Concatenate emitted items in source order with a blank line between them.
    let mut rust = String::new();
    for (i, item) in report.emitted.iter().enumerate() {
        if i > 0 {
            rust.push('\n');
        }
        rust.push_str(&item.rust);
    }
    report.rust = rust;
    report
}

/// Compare emitted Rust against a committed Rust source. Returns `Ok(())` when textually identical
/// (the port is still 1:1 here), else `Err(unified_diff)` — a deterministic, human-readable diff.
///
/// Comparison is exact except that a single trailing newline difference is normalized away (the
/// emitter does not append a file-final newline; committed files usually do).
pub fn diff_against(emitted_rust: &str, committed_rust: &str) -> Result<(), String> {
    let a = emitted_rust.trim_end_matches('\n');
    let b = committed_rust.trim_end_matches('\n');
    if a == b {
        return Ok(());
    }

    let diff = TextDiff::from_lines(a, b);
    let mut out = String::new();
    out.push_str("--- emitted\n+++ committed\n");
    for change in diff.iter_all_changes() {
        let sign = match change.tag() {
            ChangeTag::Delete => "-",
            ChangeTag::Insert => "+",
            ChangeTag::Equal => " ",
        };
        let _ = write!(out, "{sign}{}", change.value());
        if !change.value().ends_with('\n') {
            out.push('\n');
        }
    }
    Err(out)
}

// ---------------------------------------------------------------------------------------------
// Declaration dispatch.
// ---------------------------------------------------------------------------------------------

fn emit_declaration(decl: &Declaration, report: &mut CodegenReport) {
    match decl {
        Declaration::TSEnumDeclaration(e) => emit_enum(e, report),
        Declaration::ClassDeclaration(c) => {
            let name = c.id.as_ref().map(|id| id.name.as_str()).unwrap_or("<class>");
            // The only mechanical class shape is Angular's `Identifiers` ɵɵ* table.
            match try_emit_identifier_table(name, &c.body.body) {
                Ok(item) => report.emitted.push(item),
                Err(reason) => report.unsupported.push(Unsupported {
                    name: name.to_string(),
                    reason,
                }),
            }
        }
        Declaration::VariableDeclaration(v) => {
            for d in &v.declarations {
                let name = match &d.id {
                    BindingPattern::BindingIdentifier(id) => id.name.as_str(),
                    _ => {
                        report.unsupported.push(Unsupported {
                            name: "<destructuring>".into(),
                            reason: "destructuring const bindings are not a mechanical table".into(),
                        });
                        continue;
                    }
                };
                match &d.init {
                    Some(Expression::ObjectExpression(obj)) => {
                        match try_emit_number_map(name, obj) {
                            Ok(item) => report.emitted.push(item),
                            Err(reason) => report.unsupported.push(Unsupported {
                                name: name.to_string(),
                                reason,
                            }),
                        }
                    }
                    Some(Expression::ArrayExpression(arr)) => {
                        match try_emit_string_list(name, arr) {
                            Ok(item) => report.emitted.push(item),
                            Err(reason) => report.unsupported.push(Unsupported {
                                name: name.to_string(),
                                reason,
                            }),
                        }
                    }
                    _ => report.unsupported.push(Unsupported {
                        name: name.to_string(),
                        reason: "const initializer is not a literal object/array lookup table".into(),
                    }),
                }
            }
        }
        Declaration::FunctionDeclaration(f) => {
            let name = f.id.as_ref().map(|id| id.name.as_str()).unwrap_or("<fn>");
            report.unsupported.push(Unsupported {
                name: name.to_string(),
                reason: "function bodies are not mechanically transpilable (control flow / ownership)"
                    .into(),
            });
        }
        Declaration::TSInterfaceDeclaration(i) => report.unsupported.push(Unsupported {
            name: i.id.name.to_string(),
            reason: "interfaces carry no runtime data to emit".into(),
        }),
        Declaration::TSTypeAliasDeclaration(t) => report.unsupported.push(Unsupported {
            name: t.id.name.to_string(),
            reason: "type aliases carry no runtime data to emit".into(),
        }),
        _ => report.unsupported.push(Unsupported {
            name: "<declaration>".into(),
            reason: "unrecognized declaration kind, outside the mechanical subset".into(),
        }),
    }
}

// ---------------------------------------------------------------------------------------------
// (b) numeric enums / flags.
// ---------------------------------------------------------------------------------------------

fn emit_enum(e: &oxc_ast::ast::TSEnumDeclaration, report: &mut CodegenReport) {
    let name = e.id.name.as_str();

    // Resolve every member to an i64, supporting implicit auto-increment, numeric literals, the
    // bitwise operators TS enums actually use, and back-references to earlier members.
    let mut members: Vec<(String, i64)> = Vec::new();
    let mut next_auto: i64 = 0;

    for m in &e.body.members {
        let member_name = match &m.id {
            TSEnumMemberName::Identifier(id) => id.name.to_string(),
            TSEnumMemberName::String(s) => s.value.to_string(),
            _ => {
                report.unsupported.push(Unsupported {
                    name: name.to_string(),
                    reason: "enum member with a computed name is not mechanical".into(),
                });
                return;
            }
        };

        let value = match &m.initializer {
            None => {
                let v = next_auto;
                next_auto += 1;
                v
            }
            Some(expr) => match eval_int(expr, &members) {
                Ok(v) => {
                    next_auto = v + 1;
                    v
                }
                Err(reason) => {
                    report.unsupported.push(Unsupported {
                        name: name.to_string(),
                        reason: format!("enum member `{member_name}`: {reason}"),
                    });
                    return;
                }
            },
        };
        members.push((member_name, value));
    }

    let rust = render_enum(name, &members);
    report.emitted.push(Emitted {
        name: name.to_string(),
        kind: EmittedKind::Enum,
        rust,
    });
}

/// Render a numeric enum to Rust with identical discriminants. `#[repr(i64)]` so any value the TS
/// uses (incl. large bit-flag combinations) round-trips. Duplicate discriminants (Angular's
/// `Default = 1; Eager = 1`) are legal in TS but NOT in a Rust `enum`, so colliding later variants
/// are emitted as associated `const`s referencing the canonical variant — preserving every name
/// and value without a Rust compile error.
fn render_enum(name: &str, members: &[(String, i64)]) -> String {
    let mut seen: Vec<(i64, String)> = Vec::new();
    let mut variants: Vec<(String, i64)> = Vec::new();
    let mut aliases: Vec<(String, String)> = Vec::new();

    for (m_name, value) in members {
        if let Some((_, canonical)) = seen.iter().find(|(v, _)| v == value) {
            aliases.push((m_name.clone(), canonical.clone()));
        } else {
            seen.push((*value, m_name.clone()));
            variants.push((m_name.clone(), *value));
        }
    }

    let mut out = String::new();
    let _ = writeln!(out, "#[repr(i64)]");
    let _ = writeln!(
        out,
        "#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]"
    );
    let _ = writeln!(out, "pub enum {name} {{");
    for (m_name, value) in &variants {
        let _ = writeln!(out, "    {m_name} = {value},");
    }
    out.push_str("}\n");

    if !aliases.is_empty() {
        let _ = writeln!(out, "\nimpl {name} {{");
        for (alias, canonical) in &aliases {
            // TS allows duplicate enum values; mirror the alias as a const of the same type.
            let _ = writeln!(
                out,
                "    pub const {alias}: {name} = {name}::{canonical};"
            );
        }
        out.push_str("}\n");
    }

    out
}

// ---------------------------------------------------------------------------------------------
// (a) the `Identifiers` ɵɵ* name table.
// ---------------------------------------------------------------------------------------------

/// Try to read a class as Angular's `Identifiers` table: every member must be a `static` property
/// whose initializer is an object literal `{name: <string|null>, moduleName: <ident|string>}`. The
/// emitted Rust is a `&[(&str, &str)]` of `(field_name, wire_name)` — wire name copied verbatim
/// (preserving the `ɵ` U+0275 prefix), empty string for a `null` name (the `core` namespace entry).
fn try_emit_identifier_table(
    name: &str,
    elements: &[ClassElement],
) -> Result<Emitted, String> {
    let mut rows: Vec<(String, String)> = Vec::new();

    for el in elements {
        let ClassElement::PropertyDefinition(prop) = el else {
            return Err("class has a non-property member (method / static block); not a pure \
                        identifier table"
                .into());
        };
        if !prop.r#static {
            return Err("class has a non-static member; the Identifiers table is all static".into());
        }
        let field = match &prop.key {
            PropertyKey::StaticIdentifier(id) => id.name.to_string(),
            PropertyKey::StringLiteral(s) => s.value.to_string(),
            _ => return Err("identifier-table field with a computed/private name".into()),
        };
        let Some(init) = &prop.value else {
            return Err(format!("field `{field}` has no initializer"));
        };
        let Expression::ObjectExpression(obj) = init else {
            return Err(format!(
                "field `{field}` initializer is not an `{{name, moduleName}}` object"
            ));
        };

        let wire = extract_external_reference_name(obj)
            .map_err(|e| format!("field `{field}`: {e}"))?;
        rows.push((field, wire));
    }

    if rows.is_empty() {
        return Err("class has no static identifier fields".into());
    }

    let mut out = String::new();
    let _ = writeln!(
        out,
        "/// `(field name, ɵɵ wire name)` rows of Angular's `{name}` instruction table."
    );
    let _ = writeln!(
        out,
        "pub const {}: &[(&str, &str)] = &[",
        to_screaming_snake(name)
    );
    for (field, wire) in &rows {
        let _ = writeln!(
            out,
            "    ({:?}, {:?}),",
            field, wire
        );
    }
    out.push_str("];\n");

    Ok(Emitted {
        name: name.to_string(),
        kind: EmittedKind::IdentifierTable,
        rust: out,
    })
}

/// From an object literal `{name: 'ɵɵx', moduleName: CORE}`, extract the `name` value as the wire
/// string. `name: null` (the `core` namespace entry) yields `""`. The `moduleName` is required to
/// be present (so we don't silently accept a differently-shaped object) but its value is not
/// emitted — every render3 identifier resolves against `@angular/core`.
fn extract_external_reference_name(
    obj: &oxc_ast::ast::ObjectExpression,
) -> Result<String, String> {
    let mut name_value: Option<String> = None;
    let mut saw_module_name = false;

    for prop in &obj.properties {
        let ObjectPropertyKind::ObjectProperty(p) = prop else {
            return Err("object has a spread element".into());
        };
        let key = match &p.key {
            PropertyKey::StaticIdentifier(id) => id.name.as_str(),
            PropertyKey::StringLiteral(s) => s.value.as_str(),
            _ => return Err("object has a computed key".into()),
        };
        match key {
            "name" => match &p.value {
                Expression::StringLiteral(s) => name_value = Some(s.value.to_string()),
                Expression::NullLiteral(_) => name_value = Some(String::new()),
                _ => return Err("`name` is not a string literal or null".into()),
            },
            "moduleName" => {
                // Accept either the `CORE` identifier or a string literal; value not emitted.
                match &p.value {
                    Expression::Identifier(_) | Expression::StringLiteral(_) => {
                        saw_module_name = true;
                    }
                    _ => return Err("`moduleName` is not an identifier or string".into()),
                }
            }
            other => return Err(format!("unexpected key `{other}` (expected name/moduleName)")),
        }
    }

    match (name_value, saw_module_name) {
        (Some(v), true) => Ok(v),
        (None, _) => Err("missing `name` property".into()),
        (_, false) => Err("missing `moduleName` property".into()),
    }
}

// ---------------------------------------------------------------------------------------------
// (c) simple string/number lookup tables.
// ---------------------------------------------------------------------------------------------

/// `export const T = {a: 1, b: 2}` -> Rust `&[(&str, i64)]`. Every value must be an integer
/// constant expression (numeric literal / bitwise combo / unary minus). Anything else refuses.
fn try_emit_number_map(
    name: &str,
    obj: &oxc_ast::ast::ObjectExpression,
) -> Result<Emitted, String> {
    if obj.properties.is_empty() {
        return Err("empty object is not a useful lookup table".into());
    }
    let mut rows: Vec<(String, i64)> = Vec::new();
    for prop in &obj.properties {
        let ObjectPropertyKind::ObjectProperty(p) = prop else {
            return Err("object has a spread element".into());
        };
        let key = match &p.key {
            PropertyKey::StaticIdentifier(id) => id.name.to_string(),
            PropertyKey::StringLiteral(s) => s.value.to_string(),
            _ => return Err("object has a computed/non-string key".into()),
        };
        let value = eval_int(&p.value, &[])
            .map_err(|e| format!("value of `{key}`: {e}"))?;
        rows.push((key, value));
    }

    let mut out = String::new();
    let _ = writeln!(out, "pub const {}: &[(&str, i64)] = &[", to_screaming_snake(name));
    for (k, v) in &rows {
        let _ = writeln!(out, "    ({:?}, {}),", k, v);
    }
    out.push_str("];\n");

    Ok(Emitted {
        name: name.to_string(),
        kind: EmittedKind::NumberMap,
        rust: out,
    })
}

/// `export const T = ['a', 'b']` -> Rust `&[&str]`. Every element must be a string literal.
fn try_emit_string_list(
    name: &str,
    arr: &oxc_ast::ast::ArrayExpression,
) -> Result<Emitted, String> {
    if arr.elements.is_empty() {
        return Err("empty array is not a useful lookup table".into());
    }
    let mut items: Vec<String> = Vec::new();
    for el in &arr.elements {
        match el.as_expression() {
            Some(Expression::StringLiteral(s)) => items.push(s.value.to_string()),
            _ => return Err("array element is not a string literal (or is a hole/spread)".into()),
        }
    }

    let mut out = String::new();
    let _ = writeln!(out, "pub const {}: &[&str] = &[", to_screaming_snake(name));
    for it in &items {
        let _ = writeln!(out, "    {:?},", it);
    }
    out.push_str("];\n");

    Ok(Emitted {
        name: name.to_string(),
        kind: EmittedKind::StringList,
        rust: out,
    })
}

// ---------------------------------------------------------------------------------------------
// Integer constant-expression evaluator (the only "logic" the subset allows — pure, total over
// the operators TS data tables use; refuses everything else).
// ---------------------------------------------------------------------------------------------

/// Evaluate an integer constant expression as TypeScript would. `prior` are the already-evaluated
/// members of the *same enum* (for `A | B` style back-references). Refuses non-integers,
/// non-resolvable references, and operators outside the bitwise/arithmetic subset.
fn eval_int(expr: &Expression, prior: &[(String, i64)]) -> Result<i64, String> {
    use oxc_syntax::operator::{BinaryOperator, UnaryOperator};

    match expr {
        Expression::NumericLiteral(n) => {
            let v = n.value;
            if v.fract() != 0.0 || !v.is_finite() {
                return Err(format!("`{v}` is not an integer"));
            }
            Ok(v as i64)
        }
        Expression::ParenthesizedExpression(p) => eval_int(&p.expression, prior),
        Expression::UnaryExpression(u) => {
            let arg = eval_int(&u.argument, prior)?;
            match u.operator {
                UnaryOperator::UnaryNegation => Ok(-arg),
                UnaryOperator::UnaryPlus => Ok(arg),
                UnaryOperator::BitwiseNot => Ok(!arg),
                _ => Err(format!("unsupported unary operator `{}`", u.operator.as_str())),
            }
        }
        Expression::BinaryExpression(b) => {
            let l = eval_int(&b.left, prior)?;
            let r = eval_int(&b.right, prior)?;
            match b.operator {
                BinaryOperator::BitwiseOR => Ok(l | r),
                BinaryOperator::BitwiseAnd => Ok(l & r),
                BinaryOperator::BitwiseXOR => Ok(l ^ r),
                BinaryOperator::ShiftLeft => Ok(l << r),
                BinaryOperator::ShiftRight => Ok(l >> r),
                BinaryOperator::Addition => Ok(l + r),
                BinaryOperator::Subtraction => Ok(l - r),
                BinaryOperator::Multiplication => Ok(l * r),
                _ => Err(format!(
                    "unsupported binary operator `{}`",
                    b.operator.as_str()
                )),
            }
        }
        // Bare back-reference to an earlier member of the same enum: `Foo` in `Bar = Foo | 1`.
        Expression::Identifier(id) => resolve_member(id.name.as_str(), prior),
        // Qualified back-reference: `SelectorFlags.NOT` — only resolvable against the same enum.
        Expression::StaticMemberExpression(m) => {
            resolve_member(m.property.name.as_str(), prior)
        }
        _ => Err("not an integer constant expression".into()),
    }
}

/// Resolve `name` against earlier members of the same enum. Refuses unknown / forward references.
fn resolve_member(name: &str, prior: &[(String, i64)]) -> Result<i64, String> {
    prior
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| *v)
        .ok_or_else(|| {
            format!(
                "reference `{name}` is not an earlier member of this enum (forward or external \
                 references are not mechanical)"
            )
        })
}

// ---------------------------------------------------------------------------------------------
// Naming.
// ---------------------------------------------------------------------------------------------

/// `PascalCaseName` / `camelCase` -> `SCREAMING_SNAKE_CASE` for the emitted const item name.
fn to_screaming_snake(name: &str) -> String {
    let mut out = String::new();
    let mut prev_lower_or_digit = false;
    for ch in name.chars() {
        if ch.is_ascii_uppercase() {
            if prev_lower_or_digit {
                out.push('_');
            }
            out.push(ch);
            prev_lower_or_digit = false;
        } else if ch == '_' {
            out.push('_');
            prev_lower_or_digit = false;
        } else {
            out.push(ch.to_ascii_uppercase());
            prev_lower_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_matching_rust_enum_for_synthetic_attribute_marker() {
        let report = emit_rust("export enum AttributeMarker { Classes = 1, Styles = 2 }");
        assert!(report.is_fully_mechanical(), "no refusals: {:?}", report.unsupported);
        assert_eq!(report.emitted.len(), 1);
        assert_eq!(report.emitted[0].kind, EmittedKind::Enum);
        let expected = "\
#[repr(i64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttributeMarker {
    Classes = 1,
    Styles = 2,
}
";
        assert_eq!(report.rust, expected);
    }

    #[test]
    fn evaluates_bitwise_and_back_references() {
        // SelectorFlags-style const enum with binary/shift literals and a member back-ref.
        let src = "export const enum SelectorFlags {
            NOT = 0b0001,
            ATTRIBUTE = 0b0010,
            ELEMENT = 0b0100,
            CLASS = 1 << 3,
            NOT_ELEMENT = NOT | ELEMENT,
        }";
        let report = emit_rust(src);
        assert!(report.is_fully_mechanical(), "{:?}", report.unsupported);
        let r = &report.rust;
        assert!(r.contains("NOT = 1,"), "{r}");
        assert!(r.contains("ATTRIBUTE = 2,"), "{r}");
        assert!(r.contains("ELEMENT = 4,"), "{r}");
        assert!(r.contains("CLASS = 8,"), "{r}");
        assert!(r.contains("NOT_ELEMENT = 5,"), "{r}");
    }

    #[test]
    fn duplicate_enum_values_become_aliases() {
        // ChangeDetectionStrategy: `Default = 1; Eager = 1` — illegal as a Rust enum variant.
        let src = "export enum ChangeDetectionStrategy { OnPush = 0, Default = 1, Eager = 1 }";
        let report = emit_rust(src);
        assert!(report.is_fully_mechanical(), "{:?}", report.unsupported);
        let r = &report.rust;
        assert!(r.contains("OnPush = 0,"), "{r}");
        assert!(r.contains("Default = 1,"), "{r}");
        // `Eager` must be an alias const, not a duplicate variant.
        assert!(!r.contains("Eager = 1,"), "Eager should not be a variant: {r}");
        assert!(
            r.contains("pub const Eager: ChangeDetectionStrategy = ChangeDetectionStrategy::Default;"),
            "{r}"
        );
    }

    #[test]
    fn auto_increment_without_initializers() {
        let report = emit_rust("export enum E { A, B, C = 10, D }");
        assert!(report.is_fully_mechanical(), "{:?}", report.unsupported);
        let r = &report.rust;
        assert!(r.contains("A = 0,"), "{r}");
        assert!(r.contains("B = 1,"), "{r}");
        assert!(r.contains("C = 10,"), "{r}");
        assert!(r.contains("D = 11,"), "{r}");
    }

    #[test]
    fn emits_identifier_table_preserving_wire_names() {
        let src = "export class Identifiers {
            static core = {name: null, moduleName: CORE};
            static element = {name: 'ɵɵelement', moduleName: CORE};
            static elementStart = {name: 'ɵɵelementStart', moduleName: CORE};
        }";
        let report = emit_rust(src);
        assert!(report.is_fully_mechanical(), "{:?}", report.unsupported);
        assert_eq!(report.emitted[0].kind, EmittedKind::IdentifierTable);
        let r = &report.rust;
        assert!(r.contains("pub const IDENTIFIERS: &[(&str, &str)] = &["), "{r}");
        // null name -> empty string; ɵ prefix preserved verbatim.
        assert!(r.contains("(\"core\", \"\"),"), "{r}");
        assert!(r.contains("(\"element\", \"\u{275}\u{275}element\"),"), "{r}");
        assert!(r.contains("(\"elementStart\", \"\u{275}\u{275}elementStart\"),"), "{r}");
    }

    #[test]
    fn emits_number_map_and_string_list() {
        let report = emit_rust("export const PAIRS = {a: 1, b: 2};");
        assert!(report.is_fully_mechanical(), "{:?}", report.unsupported);
        assert_eq!(report.emitted[0].kind, EmittedKind::NumberMap);
        assert!(report.rust.contains("pub const PAIRS: &[(&str, i64)] = &["), "{}", report.rust);
        assert!(report.rust.contains("(\"a\", 1),"), "{}", report.rust);

        let report = emit_rust("export const NAMES = ['foo', 'bar'];");
        assert!(report.is_fully_mechanical(), "{:?}", report.unsupported);
        assert_eq!(report.emitted[0].kind, EmittedKind::StringList);
        assert!(report.rust.contains("pub const NAMES: &[&str] = &["), "{}", report.rust);
        assert!(report.rust.contains("\"foo\","), "{}", report.rust);
    }

    #[test]
    fn refuses_unsupported_constructs_instead_of_guessing() {
        // A function body, a string enum, and a non-literal const are all outside the subset.
        let src = "export function compile(x: number) { return x + 1; }
                   export enum Mode { Read = 'r', Write = 'w' }
                   export const CFG = makeConfig();";
        let report = emit_rust(src);
        assert!(!report.is_fully_mechanical());
        assert!(report.emitted.is_empty(), "nothing should be emitted: {:?}", report.emitted);
        let names: Vec<&str> = report.unsupported.iter().map(|u| u.name.as_str()).collect();
        assert!(names.contains(&"compile"), "{names:?}");
        assert!(names.contains(&"Mode"), "{names:?}");
        assert!(names.contains(&"CFG"), "{names:?}");
        // The string-enum refusal must name the offending member, not silently coerce it.
        let mode = report.unsupported.iter().find(|u| u.name == "Mode").unwrap();
        assert!(mode.reason.contains("Read"), "{}", mode.reason);
    }

    #[test]
    fn matches_real_angular_enum_discriminants() {
        // Verbatim from `tools/angular-ref/packages/compiler/src/core.ts` (pinned 22.x): a
        // `const enum` with a discriminant gap (1 is historical/removed). The emitted variants
        // must carry the exact values the committed `view/compiler.rs` port pins.
        let src = "export const enum ViewEncapsulation {
            Emulated = 0,
            None = 2,
            ShadowDom = 3,
            ExperimentalIsolatedShadowDom = 4,
        }";
        let report = emit_rust(src);
        assert!(report.is_fully_mechanical(), "{:?}", report.unsupported);
        let r = &report.rust;
        assert!(r.contains("Emulated = 0,"), "{r}");
        assert!(r.contains("None = 2,"), "{r}");
        assert!(r.contains("ShadowDom = 3,"), "{r}");
        assert!(r.contains("ExperimentalIsolatedShadowDom = 4,"), "{r}");

        // AttributeMarker — the full real 0..=6 set drives template attribute codegen.
        let am = emit_rust(
            "export const enum AttributeMarker {
                NamespaceURI = 0, Classes = 1, Styles = 2, Bindings = 3,
                Template = 4, ProjectAs = 5, I18n = 6,
            }",
        );
        assert!(am.is_fully_mechanical(), "{:?}", am.unsupported);
        for pair in [
            "NamespaceURI = 0,",
            "Classes = 1,",
            "Styles = 2,",
            "Bindings = 3,",
            "Template = 4,",
            "ProjectAs = 5,",
            "I18n = 6,",
        ] {
            assert!(am.rust.contains(pair), "missing `{pair}` in:\n{}", am.rust);
        }
    }

    #[test]
    fn diff_against_matches_and_reports() {
        let report = emit_rust("export enum E { A = 1 }");
        // Committed copy identical except a trailing newline -> still 1:1.
        let committed = format!("{}\n", report.rust);
        assert!(diff_against(&report.rust, &committed).is_ok());

        // A drifted committed copy -> a unified diff naming the changed line.
        let drifted = report.rust.replace("A = 1", "A = 2");
        let err = diff_against(&report.rust, &drifted).unwrap_err();
        assert!(err.contains("-    A = 1,"), "{err}");
        assert!(err.contains("+    A = 2,"), "{err}");
    }
}
