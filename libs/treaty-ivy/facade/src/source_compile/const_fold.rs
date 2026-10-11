//! Same-file constant folding for decorator metadata.
//!
//! ngtsc evaluates an identifier that names a constant in the same file (`template: myTemplate` with
//! `const myTemplate = '...'`), so a decorator value does not have to be a literal (`non_literal_template*.ts`
//! goldens). Treaty's readers accept only literals, so before compilation the string-valued metadata
//! keys of each class's `@Component` / `@Directive` / `@Pipe` object are folded in place:
//!
//!   * a string literal, or a template literal with no substitution;
//!   * an identifier bound to a foldable string in the same file (`var`/`let`/`const`, any order, following
//!     other constants);
//!   * `a + b` concatenation of two foldable strings (`'<div>' + greeting + '</div>'`);
//!   * a template literal whose substitutions are `${identifier}` or `${'literal'}` (`` `<b>${greeting}</b>` ``).
//!
//! Only the keys whose value Treaty reads as a string are folded: `template`, `selector`, `exportAs`,
//! `styles` (each entry) and a pipe's `name`. Anything else is left exactly as it was, so a value that does
//! not fold is still refused by the reader that expects it.
//!
//! Cross-file resolution is NOT done here: an imported constant stays unresolved in Treaty and is resolved
//! only by the bridge (`treaty_ivy_ng_bridge`, when the `ng-analyze` feature is on).

use std::collections::HashMap;

use crate::parse::{LitValue, NArg, NArrayElement, NExpr, NObjectProp, NTopStmt, ObjLit, ParseOutput};

/// The decorator-object keys folded, per decorator name.
fn folded_keys(decorator: &str) -> &'static [&'static str] {
    match decorator {
        "Component" | "Directive" => &["template", "selector", "exportAs", "styles"],
        "Pipe" => &["name"],
        _ => &[],
    }
}

/// Returns `summary` with the foldable string-valued decorator metadata replaced by string literals.
/// Returns the input unchanged (a clone) when `source` declares no foldable constant at all.
pub(super) fn fold_same_file_consts(summary: &ParseOutput, source: &str) -> ParseOutput {
    let consts = same_file_string_consts(summary, source);
    let mut out = summary.clone();
    if consts.is_empty() {
        return out;
    }
    for class in &mut out.classes {
        for dec in &mut class.decorators {
            let keys = folded_keys(&dec.name);
            if keys.is_empty() {
                continue;
            }
            if let Some(obj) = dec.object.as_mut() {
                fold_object(obj, keys, &consts, source);
            }
            for arg in &mut dec.arguments {
                if let NArg::Expr(NExpr::Object(props), _) = arg {
                    fold_props(props, keys, &consts, source);
                }
            }
        }
    }
    out
}

/// The same-file string constants of `summary`, by name. A declaration counts when it has an initializer
/// that folds to a string; the set is closed over references between constants.
fn same_file_string_consts(summary: &ParseOutput, source: &str) -> HashMap<String, String> {
    let decls: Vec<(&str, &NExpr)> = summary
        .top_level
        .iter()
        .filter_map(|stmt| match stmt {
            NTopStmt::VarDecl { decls, .. } => Some(decls),
            _ => None,
        })
        .flatten()
        .filter_map(|d| Some((d.name.as_deref()?, d.init.as_ref()?)))
        .collect();
    let mut consts: HashMap<String, String> = HashMap::new();
    // Each pass can resolve constants that reference ones found in an earlier pass; the number of passes is
    // bounded by the declaration count, so a cycle (`const a = b; const b = a;`) terminates unresolved.
    for _ in 0..=decls.len() {
        let mut progressed = false;
        for (name, init) in &decls {
            if consts.contains_key(*name) {
                continue;
            }
            if let Some(value) = fold_string(init, &consts, source) {
                consts.insert((*name).to_string(), value);
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }
    consts
}

/// Folds `expr` to a string under `consts`, or `None` when it is not a foldable string.
fn fold_string(expr: &NExpr, consts: &HashMap<String, String>, source: &str) -> Option<String> {
    match expr {
        NExpr::String(s) => Some(s.clone()),
        NExpr::Identifier(name) => consts.get(name).cloned(),
        NExpr::Parenthesized(inner) => fold_string(inner, consts, source),
        NExpr::Binary { op, left, right } if op == "+" => {
            let mut out = fold_string(left, consts, source)?;
            out.push_str(&fold_string(right, consts, source)?);
            Some(out)
        }
        // A template literal with substitutions is not modelled by the neutral surface (it lowers to
        // `Other` carrying its span), so its source text is read back and folded here.
        NExpr::Other(span) => {
            let text = source.get(span.start as usize..span.end as usize)?;
            fold_template_literal(text, consts)
        }
        _ => None,
    }
}

/// Folds the source text of a template literal (`` `…${x}…` ``) whose substitutions are identifiers bound to
/// foldable constants or string literals. Escapes are cooked for `\n`, `\t`, `\r`, `` \` ``, `\$`, `\\`; any
/// other escape (for example `\u`) makes the literal unfoldable.
fn fold_template_literal(text: &str, consts: &HashMap<String, String>) -> Option<String> {
    let mut rest = text.strip_prefix('`')?.strip_suffix('`')?;
    let mut out = String::new();
    while let Some(c) = rest.chars().next() {
        if c == '\\' {
            let esc = rest[1..].chars().next()?;
            out.push(match esc {
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                '`' => '`',
                '$' => '$',
                '\\' => '\\',
                _ => return None,
            });
            rest = &rest[1 + esc.len_utf8()..];
        } else if let Some(body) = rest.strip_prefix("${") {
            let close = body.find('}')?;
            out.push_str(&fold_substitution(body[..close].trim(), consts)?);
            rest = &body[close + 1..];
        } else {
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    Some(out)
}

/// A `${…}` substitution: a bare identifier bound to a folded constant, or a quoted string literal.
fn fold_substitution(body: &str, consts: &HashMap<String, String>) -> Option<String> {
    if let Some(value) = consts.get(body) {
        return Some(value.clone());
    }
    let quoted = (body.starts_with('\'') && body.ends_with('\'') && body.len() >= 2)
        || (body.starts_with('"') && body.ends_with('"') && body.len() >= 2);
    if quoted {
        return Some(body[1..body.len() - 1].to_string());
    }
    None
}

fn fold_object(obj: &mut ObjLit, keys: &[&str], consts: &HashMap<String, String>, source: &str) {
    fold_props(&mut obj.nprops, keys, consts, source);
    for (key, value) in obj.props.iter_mut() {
        if !keys.contains(&key.as_str()) {
            continue;
        }
        if let LitValue::Identifier(name) = value {
            if let Some(folded) = consts.get(name.as_str()) {
                *value = LitValue::String(folded.clone());
            }
        }
    }
}

fn fold_props(props: &mut [NObjectProp], keys: &[&str], consts: &HashMap<String, String>, source: &str) {
    for prop in props.iter_mut() {
        let NObjectProp::KeyValue { key, value, .. } = prop else {
            continue;
        };
        if !keys.contains(&key.as_str()) {
            continue;
        }
        // `styles: [a, b]` folds entry by entry; every other key takes a single string value.
        if let NExpr::Array(elements) = value {
            for el in elements.iter_mut() {
                if let NArrayElement::Expr(inner) = el {
                    if let Some(s) = fold_string(inner, consts, source) {
                        *inner = NExpr::String(s);
                    }
                }
            }
        } else if let Some(s) = fold_string(value, consts, source) {
            *value = NExpr::String(s);
        }
    }
}
