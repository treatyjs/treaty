//! The Angular **partial-declaration emitter** (Rust) — the inverse of [`crate::linker`].
//!
//! A published Angular library is, by default, *partial*-compiled: each class emits
//! `ɵɵngDeclare*({...})` calls (the partial format) rather than the full AOT `ɵɵdefine*` calls. The
//! application build's Angular **Linker** later rewrites every `ɵɵngDeclare*` back into the
//! corresponding `ɵɵdefine*`. [`crate::linker`] is that linker; THIS module is its inverse — it
//! turns an AOT `ɵɵdefine*` module into the partial `ɵɵngDeclare*` form a library publishes with
//! `compilationMode: "partial"`.
//!
//! # Scope (and why it is mode-gated)
//!
//! The DI + pipe family — `ɵfac` (factory), `ɵprov` (`ɵɵdefineInjectable`), `ɵpipe`
//! (`ɵɵdefinePipe`), `ɵmod` (`ɵɵdefineNgModule`), `ɵinj` (`ɵɵdefineInjector`) — have pure-DATA
//! definition objects, so inverting them to `ɵɵngDeclare*` is a faithful syntactic transform that
//! **round-trips exactly** back through [`crate::linker::link_partial`] to the original AOT
//! `ɵɵdefine*`. [`emit_partial`] performs that inversion as a surgical span rewrite — every byte
//! outside a rewritten definition (the class body, the `X.ɵfac = …` assignment scaffold, imports)
//! is preserved verbatim — exactly mirroring how the linker rewrites only the call spans.
//!
//! Component / directive (`ɵcmp` / `ɵdir`) partial declarations are intentionally **out of scope**:
//! `ɵɵngDeclareComponent` carries the component `template` as an HTML STRING (and the directive host
//! bindings / queries in declarative form), so producing it from an AOT `ɵɵdefineComponent` would
//! require decompiling the lowered instruction stream back to HTML — a template DECOMPILER, a major
//! feature in its own right. Those definitions are left as their AOT `ɵɵdefine*` form and reported,
//! so a partial build of a component library still produces a valid, loadable module (the AOT
//! component defs are themselves directly usable; they simply are not in partial format).
//!
//! Because this is a SEPARATE entry the caller invokes only for `compilationMode: "partial"`, the
//! default AOT compile path is byte-for-byte untouched.

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, Expression, ObjectExpression, ObjectPropertyKind, Program, PropertyKey, Statement,
};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType};

/// The Angular version a partial declaration is stamped with. A published library stamps its own
/// compiler version; this is the in-repo placeholder value the linker (and ngcc) treat as "newest
/// behaviour" — `read_is_standalone` / `is_placeholder_version` in [`crate::linker`] accept any
/// `0.0.0-…` prerelease, so the round-trip is version-stable.
const PARTIAL_VERSION: &str = "0.0.0-PLACEHOLDER";
/// The `minVersion` a partial declaration records (the earliest linker that understands the shape).
const MIN_VERSION: &str = "12.0.0";

/// The result of a partial emit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialEmit {
    /// The module source with every supported AOT `ɵɵdefine*` definition rewritten to its
    /// `ɵɵngDeclare*` partial form. Byte-identical to the input outside the rewritten spans.
    pub code: String,
    /// Diagnostics: each component/directive definition left as AOT (partial declaration of those
    /// kinds is not yet emitted — see the module note), one message per skipped definition.
    pub notes: Vec<String>,
}

/// Emit the partial-declaration form of an AOT-compiled Ivy module.
///
/// `aot_code` is the output of the AOT source front-end (`X.ɵfac = function …`, `X.ɵprov =
/// i0.ɵɵdefineInjectable({…})`, `X.ɵpipe = i0.ɵɵdefinePipe({…})`, …). Every supported DI/pipe-family
/// definition is rewritten to its `ɵɵngDeclare*` partial form; component/directive defs are left as
/// AOT and reported in `notes`. The transform is a surgical span rewrite, so the surrounding module
/// is preserved verbatim. On a parse failure the input is returned unchanged with a note.
pub fn emit_partial(aot_code: &str) -> PartialEmit {
    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true).with_module(true);
    let parsed = Parser::new(&allocator, aot_code, source_type).parse();
    if !parsed.errors.is_empty() {
        return PartialEmit {
            code: aot_code.to_string(),
            notes: vec![format!(
                "partial emit: input did not parse ({} error(s)); left as AOT",
                parsed.errors.len()
            )],
        };
    }

    let mut rewrites: Vec<Rewrite> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    collect_rewrites(&parsed.program, aot_code, &mut rewrites, &mut notes);

    // Apply rewrites back-to-front so earlier byte offsets stay valid.
    rewrites.sort_by_key(|r| std::cmp::Reverse(r.start));
    let mut code = aot_code.to_string();
    for r in &rewrites {
        code.replace_range(r.start as usize..r.end as usize, &r.text);
    }

    PartialEmit { code, notes }
}

/// One span replacement: the byte range to overwrite and the replacement text.
struct Rewrite {
    start: u32,
    end: u32,
    text: String,
}

/// Walk every top-level statement collecting the `ɵfac`/`ɵprov`/`ɵpipe`/`ɵmod`/`ɵinj` definition
/// assignments to rewrite, and noting each `ɵcmp`/`ɵdir` left as AOT.
fn collect_rewrites(
    program: &Program,
    source: &str,
    rewrites: &mut Vec<Rewrite>,
    notes: &mut Vec<String>,
) {
    for stmt in &program.body {
        let Statement::ExpressionStatement(es) = stmt else {
            continue;
        };
        let Expression::AssignmentExpression(assign) = &es.expression else {
            continue;
        };
        // LHS must be `<Ident>.<member>`.
        let Some((type_name, member)) = assignment_member(assign) else {
            continue;
        };

        match member.as_str() {
            // `X.ɵfac = function X_Factory(t){ return new (t||X)(); }` → ngDeclareFactory.
            "\u{0275}fac" => {
                // Determine this class's factory TARGET from a sibling definition member in the
                // module (ɵcmp→Component, ɵdir→Directive, ɵpipe→Pipe, ɵprov→Injectable, ɵmod→
                // NgModule). Default to Injectable when none is found (a bare `@Injectable`).
                let target = factory_target_for(program, &type_name);
                if let Some(text) = ng_declare_factory(&type_name, target) {
                    rewrites.push(Rewrite {
                        start: assign.right.span().start,
                        end: assign.right.span().end,
                        text,
                    });
                }
            }
            // `X.ɵprov = i0.ɵɵdefineInjectable({...})` → ngDeclareInjectable.
            "\u{0275}prov" => {
                if let Some(obj) = define_call_object(&assign.right, "\u{0275}\u{0275}defineInjectable") {
                    let text = ng_declare_injectable(&type_name, obj, source);
                    rewrites.push(Rewrite {
                        start: assign.right.span().start,
                        end: assign.right.span().end,
                        text,
                    });
                }
            }
            // `X.ɵpipe = i0.ɵɵdefinePipe({...})` → ngDeclarePipe.
            "\u{0275}pipe" => {
                if let Some(obj) = define_call_object(&assign.right, "\u{0275}\u{0275}definePipe") {
                    let text = ng_declare_pipe(&type_name, obj, source);
                    rewrites.push(Rewrite {
                        start: assign.right.span().start,
                        end: assign.right.span().end,
                        text,
                    });
                }
            }
            // `X.ɵinj = i0.ɵɵdefineInjector({...})` → ngDeclareInjector.
            "\u{0275}inj" => {
                if let Some(obj) = define_call_object(&assign.right, "\u{0275}\u{0275}defineInjector") {
                    let text = ng_declare_injector(&type_name, obj, source);
                    rewrites.push(Rewrite {
                        start: assign.right.span().start,
                        end: assign.right.span().end,
                        text,
                    });
                }
            }
            // `X.ɵmod = i0.ɵɵdefineNgModule({...})` → ngDeclareNgModule.
            "\u{0275}mod" => {
                if let Some(obj) = define_call_object(&assign.right, "\u{0275}\u{0275}defineNgModule") {
                    let text = ng_declare_ng_module(&type_name, obj, source, program);
                    rewrites.push(Rewrite {
                        start: assign.right.span().start,
                        end: assign.right.span().end,
                        text,
                    });
                }
            }
            // Component / directive: left as AOT (template decompilation out of scope).
            "\u{0275}cmp" => notes.push(format!(
                "{type_name}: ɵcmp (component) left as AOT — ɵɵngDeclareComponent requires template decompilation"
            )),
            "\u{0275}dir" => notes.push(format!(
                "{type_name}: ɵdir (directive) left as AOT — ɵɵngDeclareDirective partial emit not implemented"
            )),
            _ => {}
        }
    }
}

/// `<Ident>.<member>` on the LHS of an assignment → `(ident, member)`.
fn assignment_member(assign: &oxc_ast::ast::AssignmentExpression) -> Option<(String, String)> {
    use oxc_ast::ast::AssignmentTarget;
    let AssignmentTarget::StaticMemberExpression(member) = &assign.left else {
        return None;
    };
    let Expression::Identifier(obj) = &member.object else {
        return None;
    };
    Some((obj.name.to_string(), member.property.name.to_string()))
}

/// The object-literal argument of `<ns?>.<callee>({...})`, or `None` if `expr` is not that call.
fn define_call_object<'a>(expr: &'a Expression<'a>, callee: &str) -> Option<&'a ObjectExpression<'a>> {
    let Expression::CallExpression(call) = expr else {
        return None;
    };
    let is_target = match &call.callee {
        Expression::Identifier(id) => id.name == callee,
        Expression::StaticMemberExpression(m) => m.property.name == callee,
        _ => false,
    };
    if !is_target {
        return None;
    }
    match call.arguments.first() {
        Some(Argument::ObjectExpression(obj)) => Some(obj),
        _ => None,
    }
}

/// The factory target for `type_name`, inferred from which definition member the class also carries.
fn factory_target_for(program: &Program, type_name: &str) -> &'static str {
    let mut found: Option<&'static str> = None;
    for stmt in &program.body {
        let Statement::ExpressionStatement(es) = stmt else { continue };
        let Expression::AssignmentExpression(assign) = &es.expression else { continue };
        let Some((name, member)) = assignment_member(assign) else { continue };
        if name != type_name {
            continue;
        }
        let target = match member.as_str() {
            "\u{0275}cmp" => Some("Component"),
            "\u{0275}dir" => Some("Directive"),
            "\u{0275}pipe" => Some("Pipe"),
            "\u{0275}mod" => Some("NgModule"),
            "\u{0275}prov" => Some("Injectable"),
            _ => None,
        };
        if let Some(t) = target {
            found = Some(t);
            // Component/Directive/Pipe/NgModule are more specific than Injectable; prefer them.
            if t != "Injectable" {
                break;
            }
        }
    }
    found.unwrap_or("Injectable")
}

/// Read a property's value verbatim from the source (so opaque expressions — `providedIn: SomeMod`,
/// `useFactory: () => …` — round-trip exactly). Returns the trimmed source slice.
fn prop_source<'a>(obj: &ObjectExpression, name: &str, source: &'a str) -> Option<&'a str> {
    for p in &obj.properties {
        if let ObjectPropertyKind::ObjectProperty(op) = p {
            if key_name(&op.key) == Some(name) {
                let span = op.value.span();
                return Some(source[span.start as usize..span.end as usize].trim());
            }
        }
    }
    None
}

/// The static-identifier / string name of a property key.
fn key_name<'a>(key: &'a PropertyKey<'a>) -> Option<&'a str> {
    match key {
        PropertyKey::StaticIdentifier(id) => Some(id.name.as_str()),
        PropertyKey::StringLiteral(s) => Some(s.value.as_str()),
        _ => None,
    }
}

/// The shared `version` / `ngImport` suffix every `ɵɵngDeclare*` object carries, with a per-kind
/// `minVersion` (the earliest linker that understands that declaration shape — `"12.0.0"` for the
/// factory/injectable/injector/ngmodule family, `"14.0.0"` for pipe/directive/component, matching
/// Angular's own partial emitters).
fn declare_prelude_min(min_version: &str) -> String {
    format!("minVersion: \"{min_version}\", version: \"{PARTIAL_VERSION}\", ngImport: i0")
}

/// The default `minVersion` prelude (`"12.0.0"` — the DI/ngmodule family).
fn declare_prelude() -> String {
    declare_prelude_min(MIN_VERSION)
}

/// `ɵɵngDeclareFactory({...})` for `type_name` with the given factory `target` and an EMPTY `deps`
/// array (a no-arg constructor factory — the dominant published-library shape). An empty `deps: []`
/// is what makes the linker emit the simple `function X_Factory(t){ return new (t||X)(); }` back
/// (an ABSENT `deps` would instead inherit the base-class factory).
fn ng_declare_factory(type_name: &str, target: &str) -> Option<String> {
    Some(format!(
        "i0.\u{0275}\u{0275}ngDeclareFactory({{ {}, type: {type_name}, deps: [], target: i0.\u{0275}\u{0275}FactoryTarget.{target} }})",
        declare_prelude()
    ))
}

/// `ɵɵngDeclareInjectable({...})` from an `ɵɵdefineInjectable({ token, factory, providedIn })`.
/// The `token`/`factory` fields are the linker's responsibility to regenerate, so only `type` +
/// `providedIn` (the declarative inputs) are carried.
fn ng_declare_injectable(type_name: &str, obj: &ObjectExpression, source: &str) -> String {
    let mut fields = format!("{}, type: {type_name}", declare_prelude());
    if let Some(provided_in) = prop_source(obj, "providedIn", source) {
        fields.push_str(&format!(", providedIn: {provided_in}"));
    }
    format!("i0.\u{0275}\u{0275}ngDeclareInjectable({{ {fields} }})")
}

/// `ɵɵngDeclarePipe({...})` from an `ɵɵdefinePipe({ name, type, pure })`. Mirrors Angular's own
/// partial pipe emitter: `minVersion: "14.0.0"`, an explicit `isStandalone: true` for the
/// standalone-default case, the pipe `name`, and an explicit `pure: false` only when impure.
fn ng_declare_pipe(type_name: &str, obj: &ObjectExpression, source: &str) -> String {
    // The pipe declaration's minVersion is `"14.0.0"` (the earliest linker that understands the
    // pipe declaration shape), matching ng-packagr's published output.
    let mut fields = format!("{}, type: {type_name}", declare_prelude_min("14.0.0"));
    // The pipe DEF carries no `standalone` field (it is the v19+ default `true`); a published
    // declaration states it explicitly, exactly as ng-packagr emits `isStandalone: true`.
    fields.push_str(", isStandalone: true");
    if let Some(name) = prop_source(obj, "name", source) {
        fields.push_str(&format!(", name: {name}"));
    }
    // `pure` defaults to true in both the def and the declaration; carry an explicit `false` only.
    if prop_source(obj, "pure", source) == Some("false") {
        fields.push_str(", pure: false");
    }
    format!("i0.\u{0275}\u{0275}ngDeclarePipe({{ {fields} }})")
}

/// `ɵɵngDeclareInjector({...})` from an `ɵɵdefineInjector({ providers?, imports? })`. Both are opaque
/// arrays carried verbatim.
fn ng_declare_injector(type_name: &str, obj: &ObjectExpression, source: &str) -> String {
    let mut fields = format!("{}, type: {type_name}", declare_prelude());
    if let Some(providers) = prop_source(obj, "providers", source) {
        fields.push_str(&format!(", providers: {providers}"));
    }
    if let Some(imports) = prop_source(obj, "imports", source) {
        fields.push_str(&format!(", imports: {imports}"));
    }
    format!("i0.\u{0275}\u{0275}ngDeclareInjector({{ {fields} }})")
}

/// `ɵɵngDeclareNgModule({...})` from an `ɵɵdefineNgModule({ type, ... })`.
///
/// The AOT NgModule def emits its declarations/imports/exports as a tree-shakeable
/// `ɵɵsetNgModuleScope(X, { declarations, imports, exports })` SIDE-EFFECT statement (not on the def
/// object), so the declarative arrays are read from that sibling call when present and folded back
/// onto the declaration object (matching the published partial shape).
fn ng_declare_ng_module(
    type_name: &str,
    obj: &ObjectExpression,
    source: &str,
    program: &Program,
) -> String {
    let mut fields = format!("{}, type: {type_name}", declare_prelude());
    // `bootstrap`/`id` may sit on the def object directly.
    for key in ["bootstrap", "id"] {
        if let Some(v) = prop_source(obj, key, source) {
            fields.push_str(&format!(", {key}: {v}"));
        }
    }
    // declarations/imports/exports come from a sibling `ɵɵsetNgModuleScope(X, {...})` call.
    if let Some(scope) = find_set_scope_object(program, type_name, source) {
        for key in ["declarations", "imports", "exports"] {
            if let Some(v) = scope.get(key) {
                fields.push_str(&format!(", {key}: {v}"));
            }
        }
    }
    format!("i0.\u{0275}\u{0275}ngDeclareNgModule({{ {fields} }})")
}

/// Locate a `i0.ɵɵsetNgModuleScope(<type_name>, { ... })` call and return its scope object's
/// `declarations`/`imports`/`exports` field source slices keyed by name.
fn find_set_scope_object(
    program: &Program,
    type_name: &str,
    source: &str,
) -> Option<std::collections::HashMap<&'static str, String>> {
    for stmt in &program.body {
        // The scope call may be wrapped in a `(typeof ngJitMode … && i0.ɵɵsetNgModuleScope(...))`
        // guard expression-statement; scan expression statements for the call.
        let Statement::ExpressionStatement(es) = stmt else { continue };
        if let Some(map) = scope_from_expression(&es.expression, type_name, source) {
            return Some(map);
        }
    }
    None
}

/// Recursively search an expression for a `ɵɵsetNgModuleScope(<type_name>, {…})` call.
fn scope_from_expression(
    expr: &Expression,
    type_name: &str,
    source: &str,
) -> Option<std::collections::HashMap<&'static str, String>> {
    match expr {
        Expression::CallExpression(call) => {
            let is_set_scope = match &call.callee {
                Expression::Identifier(id) => id.name == "\u{0275}\u{0275}setNgModuleScope",
                Expression::StaticMemberExpression(m) => {
                    m.property.name == "\u{0275}\u{0275}setNgModuleScope"
                }
                _ => false,
            };
            if is_set_scope {
                // First arg is the module type; second is the scope object.
                let first = call.arguments.first().and_then(|a| a.as_expression());
                let matches_type =
                    matches!(first, Some(Expression::Identifier(id)) if id.name == type_name);
                if matches_type {
                    if let Some(Argument::ObjectExpression(obj)) = call.arguments.get(1) {
                        let mut map = std::collections::HashMap::new();
                        for key in ["declarations", "imports", "exports"] {
                            if let Some(v) = prop_source(obj, key, source) {
                                map.insert(key, v.to_string());
                            }
                        }
                        return Some(map);
                    }
                }
            }
            // Recurse into arguments (guarded forms wrap the call).
            for a in &call.arguments {
                if let Some(inner) = a.as_expression() {
                    if let Some(m) = scope_from_expression(inner, type_name, source) {
                        return Some(m);
                    }
                }
            }
            None
        }
        Expression::LogicalExpression(l) => scope_from_expression(&l.left, type_name, source)
            .or_else(|| scope_from_expression(&l.right, type_name, source)),
        Expression::ParenthesizedExpression(p) => scope_from_expression(&p.expression, type_name, source),
        Expression::SequenceExpression(seq) => seq
            .expressions
            .iter()
            .find_map(|e| scope_from_expression(e, type_name, source)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link_partial;
    use crate::source_compile::compile_component_source;

    /// Canonicalize a module to compare a partial→linked round-trip against the original AOT,
    /// ignoring cosmetic differences that are semantically inert: ALL whitespace is removed, the
    /// linker's `(function …);` factory-wrapper parens are stripped, and redundant `;` runs are
    /// collapsed. What remains is the exact token stream of the definitions.
    fn norm(code: &str) -> String {
        // Drop all ASCII whitespace.
        let mut s: String = code.chars().filter(|c| !c.is_whitespace()).collect();
        // The linker wraps a regenerated constructor factory as `ɵfac=(function …(){ … });` — strip
        // the wrapping `(` after `ɵfac=` and the matching `)` before its terminating `;` so it
        // matches the AOT `ɵfac=function …(){ … };`. Both forms reduce to the same body.
        s = s.replace("ɵfac=(function", "ɵfac=function");
        // The constructor factory body ends `...)();}` (its closing brace). The linker then closes
        // its `(function …)` wrapper with `)` before the terminating `;`, yielding `();})`. Drop that
        // single wrapper-closing `)` so it matches the AOT's bare `();}`.
        s = s.replace("();})", "();}");
        // Collapse redundant `;` runs the linker leaves between rewritten statements.
        while s.contains(";;") {
            s = s.replace(";;", ";");
        }
        s
    }

    #[test]
    fn pipe_partial_emits_ng_declare() {
        let aot = compile_component_source(
            "import { Pipe } from '@angular/core';\n@Pipe({ name: 'shout', standalone: true })\nexport class ShoutPipe { transform(v){return v;} }",
        );
        let partial = emit_partial(&aot.code);
        assert!(
            partial.code.contains("\u{0275}\u{0275}ngDeclarePipe"),
            "no ngDeclarePipe; got:\n{}",
            partial.code
        );
        assert!(partial.code.contains("\u{0275}\u{0275}ngDeclareFactory"), "no ngDeclareFactory");
        assert!(partial.code.contains("name: \"shout\""), "pipe name lost");
        // The partial form must NOT carry the AOT define call.
        assert!(!partial.code.contains("\u{0275}\u{0275}definePipe"), "AOT definePipe survived");
    }

    #[test]
    fn pipe_round_trips_through_linker() {
        let aot = compile_component_source(
            "import { Pipe } from '@angular/core';\n@Pipe({ name: 'shout', standalone: true })\nexport class ShoutPipe { transform(v){return v;} }",
        );
        let partial = emit_partial(&aot.code);
        let relinked = link_partial(&partial.code, "x.mjs");
        assert!(relinked.errors.is_empty(), "relink errors: {:?}", relinked.errors);
        // The partial→linked output must match the original AOT (modulo cosmetic whitespace).
        assert_eq!(
            norm(&relinked.code),
            norm(&aot.code),
            "pipe round-trip diverged\n--- AOT ---\n{}\n--- RELINKED ---\n{}",
            aot.code,
            relinked.code
        );
    }

    #[test]
    fn injectable_round_trips_through_linker() {
        let aot = compile_component_source(
            "import { Injectable } from '@angular/core';\n@Injectable({ providedIn: 'root' })\nexport class Svc { x = 1; }",
        );
        let partial = emit_partial(&aot.code);
        assert!(partial.code.contains("\u{0275}\u{0275}ngDeclareInjectable"), "no ngDeclareInjectable");
        let relinked = link_partial(&partial.code, "x.mjs");
        assert!(relinked.errors.is_empty(), "relink errors: {:?}", relinked.errors);
        assert_eq!(
            norm(&relinked.code),
            norm(&aot.code),
            "injectable round-trip diverged\n--- AOT ---\n{}\n--- RELINKED ---\n{}",
            aot.code,
            relinked.code
        );
    }

    #[test]
    fn component_left_as_aot_with_note() {
        let aot = compile_component_source(
            "import { Component } from '@angular/core';\n@Component({ selector: 'app-x', template: '<div>{{x}}</div>' })\nexport class X { x = 1; }",
        );
        let partial = emit_partial(&aot.code);
        // The component def is left as AOT (no partial component emit) and reported.
        assert!(partial.code.contains("\u{0275}\u{0275}defineComponent"), "component def should remain");
        assert!(
            partial.notes.iter().any(|n| n.contains("component")),
            "expected a note about the component left as AOT; got: {:?}",
            partial.notes
        );
    }
}
