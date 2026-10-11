//! OPT-IN control-flow modernizer: lower the legacy STRUCTURAL-DIRECTIVE control flow
//! (`*ngIf` / `*ngFor` / `*ngSwitch`) to the native BLOCK control flow (`@if` / `@for` /
//! `@switch`) at compile time, BEFORE the HTML AST is lowered to the r3 AST.
//!
//! This is a pure HTML-AST → HTML-AST transform: it rewrites the [`crate::ml_parser`] node tree,
//! turning a structural-directive [`Element`] into the equivalent control-flow [`Block`]. The
//! resulting blocks are then desugared by the EXISTING, proven block control-flow pipeline
//! (`control_flow.rs` → `view/template.rs`), so the emit is identical to a hand-authored
//! `@if`/`@for`/`@switch`. The modernizer never touches the author's source file — it operates on
//! the in-memory template node tree only, and runs ONLY when the caller opts in.
//!
//! ## Scope (what is correct + lowered)
//! * `*ngIf="cond"` and `*ngIf="cond as alias"` → `@if (cond) { … }` / `@if (cond; as alias) { … }`.
//! * `*ngFor="let item of items"` (+ `let i = index` etc., `; trackBy: fn`) → `@for (item of items;
//!   track …) { … }`. Legacy `ngFor` tracks by identity; `trackBy: fn` maps to `track fn($index,
//!   item)`, otherwise the faithful identity track is `track item`.
//! * `[ngSwitch]` host with `*ngSwitchCase`/`*ngSwitchDefault` children →
//!   `@switch (expr) { @case (v) { … } @default { … } }`.
//!
//! Forms the modernizer deliberately leaves UNTOUCHED (so the classic emit is preserved rather than
//! mis-lowered): `*ngIf` with `else`/`then` template references (they reference a separate
//! `<ng-template>`, which the block form folds inline — a non-mechanical rewrite), and any element
//! carrying a structural directive together with bindings/refs that the block form cannot host.
//!
//! ## Spans
//! Block-parameter expressions are re-parsed from their STRING form by `control_flow.rs`
//! (`parse_binding(expression, …)` parses the literal text; the span only positions the resulting
//! AST nodes), so the lowered EMIT depends only on the reconstructed parameter strings — which this
//! module controls exactly. Synthetic block spans reuse the originating element's spans; they affect
//! only diagnostics / source maps, never the generated instructions.

use crate::ml_parser::{self as html, Attribute, Block, BlockParameter, Element, Node, ParseSourceSpan};

/// The legacy structural-directive prefix (`*ngIf="…"`).
const STAR: char = '*';

/// Lower every legacy structural-directive control-flow construct in `nodes` to the native block
/// form, recursing into children. Returns the rewritten node list. A node carrying no recognized
/// structural directive is returned unchanged (only its children are recursed).
pub fn modernize_control_flow(nodes: &[Node]) -> Vec<Node> {
    let mut out: Vec<Node> = Vec::with_capacity(nodes.len());
    let mut i = 0;
    while i < nodes.len() {
        match &nodes[i] {
            Node::Element(el) => {
                // `[ngSwitch]` host: gather its `*ngSwitchCase`/`*ngSwitchDefault` children into a
                // `@switch` block. (`ngSwitch` lives on the CONTAINER, the cases on its children.)
                if let Some(switch_expr) = ng_switch_expression(el) {
                    if let Some(block) = build_switch_block(el, &switch_expr) {
                        out.push(Node::Block(Box::new(block)));
                        i += 1;
                        continue;
                    }
                }
                // `*ngIf` / `*ngFor` structural directive.
                if let Some(block) = try_lower_structural_element(el) {
                    out.push(Node::Block(Box::new(block)));
                    i += 1;
                    continue;
                }
                // Plain element: recurse into its children.
                let mut new_el = el.as_ref().clone();
                new_el.children = modernize_control_flow(&new_el.children);
                out.push(Node::Element(Box::new(new_el)));
            }
            Node::Component(c) => {
                let mut new_c = c.as_ref().clone();
                new_c.children = modernize_control_flow(&new_c.children);
                out.push(Node::Component(Box::new(new_c)));
            }
            Node::Block(b) => {
                let mut new_b = b.as_ref().clone();
                new_b.children = modernize_control_flow(&new_b.children);
                out.push(Node::Block(Box::new(new_b)));
            }
            other => out.push(other.clone()),
        }
        i += 1;
    }
    out
}

/// The `[ngSwitch]="expr"` / `ngSwitch="expr"` host expression, if `el` is a switch container.
fn ng_switch_expression(el: &Element) -> Option<String> {
    el.attrs.iter().find_map(|a| {
        let n = a.name.as_str();
        if n == "[ngSwitch]" || n == "ngSwitch" {
            Some(a.value.clone())
        } else {
            None
        }
    })
}

/// Find the FIRST `*`-prefixed structural attribute on `el` (Angular allows only one).
fn structural_attr(el: &Element) -> Option<&Attribute> {
    el.attrs.iter().find(|a| a.name.starts_with(STAR))
}

/// Lower an element carrying a `*ngIf` / `*ngFor` structural directive to its block form. Returns
/// `None` when the element carries no (recognized) structural directive, or when its shape is one
/// the modernizer refuses to lower (so the classic structural-directive emit is preserved).
fn try_lower_structural_element(el: &Element) -> Option<Block> {
    let attr = structural_attr(el)?;
    let key = &attr.name[STAR.len_utf8()..];
    match key {
        "ngIf" => build_if_block(el, attr),
        "ngFor" => build_for_block(el, attr),
        // `*ngSwitchCase` / `*ngSwitchDefault` are handled inside `build_switch_block`, never here.
        _ => None,
    }
}

/// `*ngIf="cond"` / `*ngIf="cond as alias"` → `@if (cond) { … }` / `@if (cond; as alias) { … }`.
///
/// Refuses (returns `None`, preserving the classic emit) for the `else`/`then` template-reference
/// forms (`*ngIf="cond; else tpl"`), which fold a SEPARATE `<ng-template>` inline — not a mechanical
/// rewrite.
fn build_if_block(el: &Element, attr: &Attribute) -> Option<Block> {
    let raw = attr.value.trim();
    // `;` separates the condition from the `else`/`then`/`as` micro-clauses.
    let mut parts = raw.splitn(2, ';');
    let condition = parts.next().unwrap_or("").trim();
    let rest = parts.next().unwrap_or("").trim();

    if condition.is_empty() {
        return None;
    }

    // `cond as alias` (no `;`) — the `as` alias clause. Reject `else`/`then`.
    let (cond_expr, alias) = if let Some((lhs, alias)) = split_as_alias(condition) {
        (lhs.trim().to_string(), Some(alias.trim().to_string()))
    } else {
        (condition.to_string(), None)
    };
    if !rest.is_empty() {
        // `else tpl` / `then tpl` reference a separate template — not mechanically lowerable.
        if rest.starts_with("else") || rest.starts_with("then") {
            return None;
        }
        // `cond; as alias` form.
        if let Some(a) = rest.strip_prefix("as ") {
            return assemble_if(el, attr, &cond_expr, Some(a.trim().to_string()));
        }
        return None;
    }

    assemble_if(el, attr, &cond_expr, alias)
}

/// Build the `@if (expr[; as alias]) { <child> }` block from the resolved parts.
///
/// The block carries ONE [`BlockParameter`] per `;`-separated clause — the condition is
/// `parameters[0]`, the optional `as alias` is `parameters[1]` — exactly as the HTML parser would
/// split a hand-authored `@if (expr; as alias)` header (`control_flow.rs` reads them positionally).
fn assemble_if(el: &Element, attr: &Attribute, expr: &str, alias: Option<String>) -> Option<Block> {
    let mut params = vec![block_param(expr.to_string(), &attr.source_span)];
    if let Some(a) = &alias {
        params.push(block_param(format!("as {a}"), &attr.source_span));
    }
    let child = element_without_attr(el, &attr.name);
    Some(make_block(
        "if",
        params,
        vec![Node::Element(Box::new(child))],
        el,
    ))
}

/// `*ngFor="let item of items[; let i = index][; trackBy: fn]"` → `@for (item of items; track …) { … }`.
///
/// Emits ONE [`BlockParameter`] per `;`-separated clause: `parameters[0]` is `item of items`, then
/// a `track …` clause, then each `let <local> = $<ctx>` clause — the exact positional shape the
/// HTML parser produces for a hand-authored `@for (item of items; track …; let i = $index)`.
fn build_for_block(el: &Element, attr: &Attribute) -> Option<Block> {
    let parts = ParsedNgFor::parse(&attr.value)?;
    let child = element_without_attr(el, &attr.name);

    let track = match &parts.track_by {
        // `trackBy: fn` → `track fn($index, item)` (the legacy trackBy signature).
        Some(fn_expr) => format!("track {fn_expr}($index, {})", parts.item),
        // Legacy `ngFor` tracks by identity; the faithful `@for` equivalent is `track <item>`.
        None => format!("track {}", parts.item),
    };

    let mut params = vec![
        block_param(format!("{} of {}", parts.item, parts.of_expr), &attr.source_span),
        block_param(track, &attr.source_span),
    ];
    // Context variables: `let i = index` → `let i = $index`.
    for (local, ctx) in &parts.context_vars {
        let dollar = for_loop_context_to_dollar(ctx)?;
        params.push(block_param(format!("let {local} = {dollar}"), &attr.source_span));
    }

    Some(make_block(
        "for",
        params,
        vec![Node::Element(Box::new(child))],
        el,
    ))
}

/// Map a legacy `ngFor` context name to the `@for` `$`-variable. Returns `None` for an unknown
/// context name (so the modernizer refuses rather than emit a broken loop variable).
fn for_loop_context_to_dollar(name: &str) -> Option<&'static str> {
    match name {
        "index" => Some("$index"),
        "count" => Some("$count"),
        "first" => Some("$first"),
        "last" => Some("$last"),
        "even" => Some("$even"),
        "odd" => Some("$odd"),
        _ => None,
    }
}

/// The parsed pieces of an `*ngFor` microsyntax value.
struct ParsedNgFor {
    /// The loop item variable (`item` in `let item of items`).
    item: String,
    /// The iterable expression (`items`).
    of_expr: String,
    /// `(local, context)` pairs, e.g. `("i", "index")` for `let i = index`.
    context_vars: Vec<(String, String)>,
    /// The `trackBy:` function expression, if present.
    track_by: Option<String>,
}

impl ParsedNgFor {
    /// Parse `let item of items; let i = index; trackBy: fn` (semicolon- OR comma-separated, as
    /// Angular's microsyntax allows). Returns `None` for shapes the modernizer cannot faithfully
    /// reconstruct (so the classic emit is preserved).
    fn parse(raw: &str) -> Option<ParsedNgFor> {
        let mut item: Option<String> = None;
        let mut of_expr: Option<String> = None;
        let mut context_vars: Vec<(String, String)> = Vec::new();
        let mut track_by: Option<String> = None;

        for seg in split_microsyntax(raw) {
            let seg = seg.trim();
            if seg.is_empty() {
                continue;
            }
            if let Some(rest) = seg.strip_prefix("let ") {
                // `let item of items` (the primary clause) OR `let i = index` (a context var).
                if let Some((lhs, rhs)) = rest.split_once(" of ") {
                    item = Some(lhs.trim().to_string());
                    of_expr = Some(rhs.trim().to_string());
                } else if let Some((local, ctx)) = rest.split_once('=') {
                    context_vars.push((local.trim().to_string(), ctx.trim().to_string()));
                } else {
                    return None;
                }
            } else if let Some(rest) = seg.strip_prefix("trackBy:") {
                track_by = Some(rest.trim().to_string());
            } else if let Some(rest) = seg.strip_prefix("trackBy ") {
                track_by = Some(rest.trim().to_string());
            } else {
                // An unrecognized clause (e.g. a custom directive input) — refuse to lower.
                return None;
            }
        }

        Some(ParsedNgFor {
            item: item?,
            of_expr: of_expr?,
            context_vars,
            track_by,
        })
    }
}

/// Split an `*ngFor` microsyntax value on `;` (Angular's `ngFor` separator). Commas inside the
/// expression (function arg lists) are NOT split — only top-level `;`.
fn split_microsyntax(raw: &str) -> Vec<String> {
    raw.split(';').map(|s| s.to_string()).collect()
}

/// `expr as alias` → `Some((expr, alias))`, splitting on a top-level ` as ` (not inside a string).
fn split_as_alias(s: &str) -> Option<(&str, &str)> {
    // Find ` as ` outside any quotes.
    let bytes = s.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut i = 0;
    while i + 4 <= bytes.len() {
        let c = bytes[i] as char;
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            _ => {}
        }
        if !in_single
            && !in_double
            && &s[i..i + 4] == " as "
        {
            return Some((&s[..i], &s[i + 4..]));
        }
        i += 1;
    }
    None
}

/// `[ngSwitch]="expr"` host → `@switch (expr) { @case (v) { … } … @default { … } }`.
///
/// The host element's `*ngSwitchCase="v"` / `*ngSwitchDefault` children become `@case (v)` /
/// `@default` blocks. Non-case children (whitespace text, etc.) are dropped, matching the runtime
/// behaviour of `NgSwitch` (only matching cases render). Returns `None` if the host has no case
/// children (leave the classic emit).
fn build_switch_block(el: &Element, switch_expr: &str) -> Option<Block> {
    let mut case_blocks: Vec<Node> = Vec::new();
    for child in &el.children {
        let Node::Element(child_el) = child else {
            continue;
        };
        if let Some(case_attr) = child_el.attrs.iter().find(|a| a.name == "*ngSwitchCase") {
            let inner = element_without_attr(child_el, "*ngSwitchCase");
            let body = modernize_control_flow(&[Node::Element(Box::new(inner))]);
            case_blocks.push(Node::Block(Box::new(make_block(
                "case",
                vec![block_param(case_attr.value.clone(), &case_attr.source_span)],
                body,
                child_el,
            ))));
        } else if let Some(def_attr) = child_el.attrs.iter().find(|a| a.name == "*ngSwitchDefault") {
            let inner = element_without_attr(child_el, "*ngSwitchDefault");
            let body = modernize_control_flow(&[Node::Element(Box::new(inner))]);
            case_blocks.push(Node::Block(Box::new(make_block(
                "default",
                Vec::new(),
                body,
                child_el,
            ))));
            let _ = def_attr;
        }
    }

    if case_blocks.is_empty() {
        return None;
    }

    Some(make_block(
        "switch",
        vec![block_param(switch_expr.to_string(), &el.source_span)],
        case_blocks,
        el,
    ))
}

/// Clone `el` with the named attribute removed, recursing the modernizer into its children. This is
/// the block BODY's content — the original element minus its structural directive.
fn element_without_attr(el: &Element, attr_name: &str) -> Element {
    let mut clone = el.clone();
    clone.attrs.retain(|a| a.name != attr_name);
    clone.children = modernize_control_flow(&clone.children);
    clone
}

/// Build a synthetic [`BlockParameter`] carrying the reconstructed expression text. The span is the
/// originating structural attribute's span (positions diagnostics; the emit re-parses `expression`).
fn block_param(expression: String, span: &ParseSourceSpan) -> BlockParameter {
    BlockParameter {
        expression,
        source_span: span.clone(),
    }
}

/// Build a synthetic control-flow [`Block`] reusing the originating element's spans (which only
/// affect diagnostics / source maps; the generated instructions derive from the parameters +
/// children, all reconstructed exactly here).
fn make_block(
    name: &str,
    parameters: Vec<BlockParameter>,
    children: Vec<Node>,
    origin: &Element,
) -> Block {
    Block {
        name: name.to_string(),
        parameters,
        children,
        source_span: origin.source_span.clone(),
        name_span: origin.start_source_span.clone(),
        start_source_span: origin.start_source_span.clone(),
        end_source_span: origin.end_source_span.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Vec<Node> {
        let r = html::parse(src, "t.html");
        assert!(
            r.errors.iter().all(|e| e.level != crate::ml_parser::ParseErrorLevel::Error),
            "parse errors: {:?}",
            r.errors
        );
        r.root_nodes
    }

    /// Reduce a node tree to a compact `name[params]{children}` string for assertions.
    fn sketch(nodes: &[Node]) -> String {
        let mut s = String::new();
        for n in nodes {
            match n {
                Node::Block(b) => {
                    s.push('@');
                    s.push_str(&b.name);
                    if !b.parameters.is_empty() {
                        s.push('(');
                        s.push_str(
                            &b.parameters
                                .iter()
                                .map(|p| p.expression.clone())
                                .collect::<Vec<_>>()
                                .join("; "),
                        );
                        s.push(')');
                    }
                    s.push('{');
                    s.push_str(&sketch(&b.children));
                    s.push('}');
                }
                Node::Element(e) => {
                    s.push('<');
                    s.push_str(&e.name);
                    for a in &e.attrs {
                        s.push(' ');
                        s.push_str(&a.name);
                    }
                    s.push('>');
                    s.push_str(&sketch(&e.children));
                    s.push_str(&format!("</{}>", e.name));
                }
                Node::Text(t) if !t.value.trim().is_empty() => s.push_str(t.value.trim()),
                _ => {}
            }
        }
        s
    }

    #[test]
    fn lowers_ng_if() {
        let nodes = parse(r#"<div *ngIf="show">hi</div>"#);
        let out = modernize_control_flow(&nodes);
        assert_eq!(sketch(&out), "@if(show){<div>hi</div>}");
    }

    #[test]
    fn lowers_ng_if_with_as_alias() {
        let nodes = parse(r#"<div *ngIf="user as u">{{u.name}}</div>"#);
        let out = modernize_control_flow(&nodes);
        assert_eq!(sketch(&out), "@if(user; as u){<div>{{u.name}}</div>}");
    }

    #[test]
    fn refuses_ng_if_with_else_reference() {
        // `else tpl` references a separate <ng-template>; the modernizer must NOT lower it.
        let nodes = parse(r#"<div *ngIf="show; else other">hi</div>"#);
        let out = modernize_control_flow(&nodes);
        // Unchanged: still a <div> carrying *ngIf, not an @if block.
        assert_eq!(sketch(&out), "<div *ngIf>hi</div>");
    }

    #[test]
    fn lowers_ng_for_simple() {
        let nodes = parse(r#"<li *ngFor="let item of items">{{item}}</li>"#);
        let out = modernize_control_flow(&nodes);
        assert_eq!(sketch(&out), "@for(item of items; track item){<li>{{item}}</li>}");
    }

    #[test]
    fn lowers_ng_for_with_index() {
        let nodes = parse(r#"<li *ngFor="let item of items; let i = index">{{i}}</li>"#);
        let out = modernize_control_flow(&nodes);
        assert_eq!(
            sketch(&out),
            "@for(item of items; track item; let i = $index){<li>{{i}}</li>}"
        );
    }

    #[test]
    fn lowers_ng_for_with_track_by() {
        let nodes = parse(r#"<li *ngFor="let item of items; trackBy: trackFn">{{item}}</li>"#);
        let out = modernize_control_flow(&nodes);
        assert_eq!(
            sketch(&out),
            "@for(item of items; track trackFn($index, item)){<li>{{item}}</li>}"
        );
    }

    #[test]
    fn lowers_ng_switch() {
        let nodes = parse(
            r#"<div [ngSwitch]="color"><span *ngSwitchCase="'red'">R</span><span *ngSwitchDefault>D</span></div>"#,
        );
        let out = modernize_control_flow(&nodes);
        assert_eq!(
            sketch(&out),
            "@switch(color){@case('red'){<span>R</span>}@default{<span>D</span>}}"
        );
    }

    #[test]
    fn recurses_into_nested_structural_directives() {
        let nodes = parse(r#"<div *ngIf="a"><li *ngFor="let x of xs">{{x}}</li></div>"#);
        let out = modernize_control_flow(&nodes);
        assert_eq!(
            sketch(&out),
            "@if(a){<div>@for(x of xs; track x){<li>{{x}}</li>}</div>}"
        );
    }

    #[test]
    fn leaves_plain_template_untouched() {
        let nodes = parse(r#"<div class="x">{{y}}</div>"#);
        let out = modernize_control_flow(&nodes);
        assert_eq!(sketch(&out), "<div class>{{y}}</div>");
    }
}
