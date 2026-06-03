//! The **partial component / directive declaration emitter** (Rust) — the source-side counterpart of
//! [`crate::partial_emit`] for the `ɵcmp` / `ɵdir` family.
//!
//! [`crate::partial_emit`] inverts an AOT module's DI/pipe-family `ɵɵdefine*` calls to their
//! `ɵɵngDeclare*` form by surgical span rewrite. It *cannot* do that for a component, because an AOT
//! `ɵɵdefineComponent` carries the template as a lowered INSTRUCTION STREAM, and reconstructing the
//! `ɵɵngDeclareComponent` partial form (which carries the template as an HTML STRING) would require a
//! template DECOMPILER.
//!
//! This module avoids the decompiler entirely: at the point the source front-end has the ORIGINAL
//! template string + the fully-built [`R3DirectiveMetadata`] (inputs/outputs/host/queries/…), it
//! emits `ɵɵngDeclareComponent` / `ɵɵngDeclareDirective` DIRECTLY from that metadata — a faithful
//! parallel emit path, NOT a decompilation. The default (Full) emit path is untouched; this runs only
//! when `compilationMode: "partial"` is requested via [`crate::source_compile::CompileOptions`].
//!
//! The emitted declaration round-trips back to the AOT `ɵɵdefineComponent` / `ɵɵdefineDirective`
//! through [`crate::linker::link_partial`] — the existing Angular linker port — exactly as a real
//! ng-packagr partial library does (verified by the round-trip tests in `source_compile`).

use crate::output_ast::{self as o, Expr, ExternalReference, LiteralValue};
use crate::view::compiler::{
    ChangeDetectionStrategy, OrderedMap, QueryPredicate, R3DirectiveMetadata, R3HostDirectiveMetadata,
    R3HostMetadata, R3InputMetadata, R3QueryMetadata, R3TemplateDependencyKind, ViewEncapsulation,
};

/// The Angular version a partial declaration is stamped with — the in-repo placeholder the linker
/// treats as "newest behaviour" (any `0.0.0-…` prerelease → `is_placeholder_version`), so the
/// round-trip is version-stable. Mirrors [`crate::partial_emit`].
const PARTIAL_VERSION: &str = "0.0.0-PLACEHOLDER";
/// The `minVersion` a `ɵɵngDeclareComponent` / `ɵɵngDeclareDirective` records — `"14.0.0"`, the
/// earliest linker that understands the directive/component declaration shape (matching Angular's own
/// partial component/directive emitters; the DI/pipe family uses `"12.0.0"`).
const MIN_VERSION: &str = "14.0.0";

/// A description of ONE dependency a component declares (`{kind, type, selector?}`), captured at the
/// point the source front-end resolves the component's template dependencies. The `selector` is
/// carried for fidelity with a real ng-packagr `dependencies` entry; the linker only needs
/// `kind` + `type` to round-trip (it re-derives matching from the template + selectors).
#[derive(Debug, Clone)]
pub struct PartialDependency {
    pub kind: R3TemplateDependencyKind,
    /// The dependency class reference (e.g. `RouterOutlet`).
    pub ty: Expr,
    /// The dependency's CSS selector, when known (a `@Directive`/`@Component` selector).
    pub selector: Option<String>,
}

/// The component-specific inputs to [`emit_ng_declare_component`] (everything beyond the shared
/// directive base): the ORIGINAL inline template HTML string and the declarative view metadata.
pub struct PartialComponentInputs<'a> {
    /// The template HTML, carried VERBATIM as a string literal. For an INLINE `template:` this is the
    /// source string and [`Self::is_inline`] is `true`; for an external `templateUrl` this is the
    /// HOST-RESOLVED file content and [`Self::is_inline`] is `false` (ng-packagr inlines the resolved
    /// template into the partial declaration and OMITS `isInline`).
    pub template_html: &'a str,
    /// Whether `template_html` came from an INLINE `template:` (`isInline: true`) rather than an
    /// external `templateUrl` (the resolved content, no `isInline` field — matching ng-packagr).
    pub is_inline: bool,
    pub change_detection: ChangeDetectionStrategy,
    pub encapsulation: ViewEncapsulation,
    /// Inline component style strings — RAW (unscoped); scoping happens at link time.
    pub styles: &'a [String],
    pub animations: Option<&'a Expr>,
    pub view_providers: Option<&'a Expr>,
    pub preserve_whitespaces: Option<bool>,
    /// The resolved template dependencies (the binder's matched directives/pipes).
    pub dependencies: &'a [PartialDependency],
}

/// Build the `i0.<name>` callee for a `ɵɵngDeclare*` call. Emitted through the namespace-import
/// machinery exactly like the AOT `ɵɵdefine*` callee, so it prints as `i0.<name>` and the assembled
/// module's leading `import * as i0 from "@angular/core"` covers it.
fn declare_callee(name: &str) -> Expr {
    o::import_expr(ExternalReference::new(Some("@angular/core".into()), name), None)
}

/// An `i0.<member>` reference (e.g. `i0.ChangeDetectionStrategy`, `i0.ɵɵFactoryTarget`) — the
/// namespace member partial declarations reference for runtime enum members.
fn i0_member(member: &str) -> Expr {
    o::variable("i0", None).prop(member)
}

fn str_lit(s: &str) -> Expr {
    o::literal(LiteralValue::String(s.to_string()), None)
}

fn bool_lit(b: bool) -> Expr {
    o::literal(LiteralValue::Bool(b), None)
}

/// An unquoted-key object property entry (`key: value`) — the well-known declaration field names are
/// always valid identifiers.
fn field(key: &str, value: Expr) -> (String, bool, Expr) {
    (key.to_string(), false, value)
}

/// A DATA-keyed object property entry (`inputs`/`outputs`/`host` maps), where the key is a
/// user-controlled property name. Quote it only when it is NOT a valid JS identifier — exactly as
/// Angular's map-literal emitter decides — so `counter`/`name` print unquoted (matching ng-packagr)
/// while `data-foo` / `@my.event` print quoted.
fn data_field(key: &str, value: Expr) -> (String, bool, Expr) {
    (key.to_string(), !is_valid_js_identifier(key), value)
}

/// Whether `name` is a valid (unquoted-able) JS identifier — ASCII-letter/`_`/`$` start, then
/// ASCII-alphanumeric/`_`/`$`. Conservative (ASCII-only): a non-ASCII or punctuation name is quoted.
fn is_valid_js_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

/// The shared leading `minVersion` / `version` / `type` prelude every directive/component
/// declaration opens with. Field order matches Angular's `createDirectiveDefinitionMap` /
/// `createComponentDefinitionMap` (`minVersion`, `version`, `type`, …); `ngImport: i0` is appended
/// AFTER the directive base (right before `template` on a component, last on a directive), exactly as
/// Angular's emitters place it — see [`ng_import_field`].
fn prelude_fields(type_expr: Expr) -> Vec<(String, bool, Expr)> {
    vec![
        field("minVersion", str_lit(MIN_VERSION)),
        field("version", str_lit(PARTIAL_VERSION)),
        field("type", type_expr),
    ]
}

/// The `ngImport: i0` field — placed AFTER the directive base by both emitters (Angular emits it
/// right before `template` on a component and as the trailing field on a directive).
fn ng_import_field() -> (String, bool, Expr) {
    field("ngImport", o::variable("i0", None))
}

/// Append the shared DIRECTIVE base fields (selector, inputs, outputs, host, queries, viewQueries,
/// exportAs, providers, hostDirectives, isStandalone, isSignal, usesInheritance, usesOnChanges) onto
/// `fields`, in the order Angular's `createDirectiveDefinitionMap` emits them. Both
/// `ɵɵngDeclareComponent` and `ɵɵngDeclareDirective` carry this base.
fn push_directive_base(fields: &mut Vec<(String, bool, Expr)>, base: &R3DirectiveMetadata) {
    // `isStandalone` — published declarations state it explicitly (defaults to true v19+, but
    // ng-packagr emits it; the linker reads it back via `read_is_standalone`).
    fields.push(field("isStandalone", bool_lit(base.is_standalone)));

    if let Some(selector) = &base.selector {
        fields.push(field("selector", str_lit(selector)));
    }

    if !base.inputs.is_empty() {
        fields.push(field("inputs", inputs_map(&base.inputs)));
    }
    if !base.outputs.is_empty() {
        fields.push(field("outputs", outputs_map(&base.outputs)));
    }

    if !host_is_empty(&base.host) {
        fields.push(field("host", host_object(&base.host)));
    }

    if let Some(providers) = &base.providers {
        fields.push(field("providers", providers.clone()));
    }

    if !base.queries.is_empty() {
        fields.push(field("queries", queries_array(&base.queries)));
    }
    if !base.view_queries.is_empty() {
        fields.push(field("viewQueries", queries_array(&base.view_queries)));
    }

    if let Some(export_as) = &base.export_as {
        if !export_as.is_empty() {
            let names = export_as.iter().map(|n| str_lit(n)).collect();
            fields.push(field("exportAs", o::literal_arr(names, None)));
        }
    }

    if base.uses_inheritance {
        fields.push(field("usesInheritance", bool_lit(true)));
    }
    if base.lifecycle.uses_on_changes {
        fields.push(field("usesOnChanges", bool_lit(true)));
    }

    if let Some(host_directives) = &base.host_directives {
        if !host_directives.is_empty() {
            fields.push(field("hostDirectives", host_directives_array(host_directives)));
        }
    }

    if base.is_signal {
        fields.push(field("isSignal", bool_lit(true)));
    }
}

/// `inputs: { prop: { classPropertyName, publicName, isSignal, isRequired, transformFunction? } }`.
/// Emitted in the rich object form (the form Angular v16+ uses and the linker's `to_input_mapping`
/// reads back).
fn inputs_map(inputs: &OrderedMap<String, R3InputMetadata>) -> Expr {
    let entries = inputs
        .iter()
        .map(|(key, m)| {
            let mut e = vec![
                field("classPropertyName", str_lit(&m.class_property_name)),
                field("publicName", str_lit(&m.binding_property_name)),
                field("isSignal", bool_lit(m.is_signal)),
                field("isRequired", bool_lit(m.required)),
            ];
            if let Some(tf) = &m.transform_function {
                e.push(field("transformFunction", tf.clone()));
            } else {
                e.push(field("transformFunction", o::literal(LiteralValue::Null, None)));
            }
            data_field(key, o::literal_map(e, None))
        })
        .collect();
    o::literal_map(entries, None)
}

/// `outputs: { classProperty: "publicName" }`.
fn outputs_map(outputs: &OrderedMap<String, String>) -> Expr {
    let entries = outputs
        .iter()
        .map(|(key, pub_name)| data_field(key, str_lit(pub_name)))
        .collect();
    o::literal_map(entries, None)
}

fn host_is_empty(host: &R3HostMetadata) -> bool {
    host.attributes.is_empty()
        && host.listeners.is_empty()
        && host.properties.is_empty()
        && host.special_attributes.style_attr.is_none()
        && host.special_attributes.class_attr.is_none()
}

/// `host: { attributes?, listeners?, properties?, classAttribute?, styleAttribute? }` — the split
/// form a declaration carries (the linker's `read_host` maps each sub-field directly).
fn host_object(host: &R3HostMetadata) -> Expr {
    let mut fields: Vec<(String, bool, Expr)> = Vec::new();
    if !host.attributes.is_empty() {
        let entries = host
            .attributes
            .iter()
            .map(|(k, v)| data_field(k, v.clone()))
            .collect();
        fields.push(field("attributes", o::literal_map(entries, None)));
    }
    if !host.listeners.is_empty() {
        fields.push(field("listeners", string_map(&host.listeners)));
    }
    if !host.properties.is_empty() {
        fields.push(field("properties", string_map(&host.properties)));
    }
    if let Some(class_attr) = &host.special_attributes.class_attr {
        fields.push(field("classAttribute", str_lit(class_attr)));
    }
    if let Some(style_attr) = &host.special_attributes.style_attr {
        fields.push(field("styleAttribute", str_lit(style_attr)));
    }
    o::literal_map(fields, None)
}

/// A `{ key: "value" }` string→string map (host `listeners` / `properties`). Keys that are not valid
/// identifiers (`click`, `keydown.enter`, `window:resize`) are quoted; plain ones are not.
fn string_map(map: &OrderedMap<String, String>) -> Expr {
    let entries = map
        .iter()
        .map(|(k, v)| data_field(k, str_lit(v)))
        .collect();
    o::literal_map(entries, None)
}

/// `queries` / `viewQueries` array — each entry `{ propertyName, first?, predicate, descendants?,
/// read?, static?, emitDistinctChangesOnly?, isSignal? }`. Only non-default fields are emitted
/// (matching Angular's `compileQuery`-derived declaration map).
fn queries_array(queries: &[R3QueryMetadata]) -> Expr {
    let entries = queries
        .iter()
        .map(|q| {
            let mut fields = vec![field("propertyName", str_lit(&q.property_name))];
            if q.first {
                fields.push(field("first", bool_lit(true)));
            }
            fields.push(field("predicate", query_predicate(&q.predicate)));
            if q.descendants {
                fields.push(field("descendants", bool_lit(true)));
            }
            if let Some(read) = &q.read {
                fields.push(field("read", read.clone()));
            }
            if q.static_ {
                fields.push(field("static", bool_lit(true)));
            }
            // `emitDistinctChangesOnly` defaults to `true`; emit an explicit `false` only.
            if !q.emit_distinct_changes_only {
                fields.push(field("emitDistinctChangesOnly", bool_lit(false)));
            }
            if q.is_signal {
                fields.push(field("isSignal", bool_lit(true)));
            }
            o::literal_map(fields, None)
        })
        .collect();
    o::literal_arr(entries, None)
}

/// A query `predicate` — either a `["sel", …]` string-selector array or an opaque class-reference
/// expression (carried verbatim).
fn query_predicate(predicate: &QueryPredicate) -> Expr {
    match predicate {
        QueryPredicate::Selectors(sels) => {
            o::literal_arr(sels.iter().map(|s| str_lit(s)).collect(), None)
        }
        QueryPredicate::Expr(expr) => expr.clone(),
    }
}

/// `hostDirectives` array — each entry `{ directive, inputs?, outputs? }`. The inputs/outputs are
/// the FLAT `[publicName, alias, …]` arrays a declaration carries (the linker's
/// `read_host_directive_mapping` reads them back).
fn host_directives_array(host_directives: &[R3HostDirectiveMetadata]) -> Expr {
    let entries = host_directives
        .iter()
        .map(|hd| {
            let mut fields = vec![field("directive", hd.directive.value.clone())];
            if let Some(inputs) = &hd.inputs {
                fields.push(field("inputs", host_directive_mapping(inputs)));
            }
            if let Some(outputs) = &hd.outputs {
                fields.push(field("outputs", host_directive_mapping(outputs)));
            }
            o::literal_map(fields, None)
        })
        .collect();
    o::literal_arr(entries, None)
}

/// `["publicName", "alias", …]` — the flat host-directive input/output binding array.
fn host_directive_mapping(map: &OrderedMap<String, String>) -> Expr {
    let mut elems: Vec<Expr> = Vec::new();
    for (public, alias) in map.iter() {
        elems.push(str_lit(public));
        elems.push(str_lit(alias));
    }
    o::literal_arr(elems, None)
}

/// `dependencies` array — each entry `{ kind, type, selector? }`.
fn dependencies_array(dependencies: &[PartialDependency]) -> Expr {
    let entries = dependencies
        .iter()
        .map(|d| {
            let kind = match d.kind {
                R3TemplateDependencyKind::Directive => "directive",
                R3TemplateDependencyKind::Pipe => "pipe",
                R3TemplateDependencyKind::NgModule => "ngmodule",
            };
            let mut fields = vec![field("kind", str_lit(kind)), field("type", d.ty.clone())];
            if let Some(selector) = &d.selector {
                fields.push(field("selector", str_lit(selector)));
            }
            o::literal_map(fields, None)
        })
        .collect();
    o::literal_arr(entries, None)
}

/// `changeDetection: i0.ChangeDetectionStrategy.X` — emitted EXPLICITLY for the component's strategy.
///
/// Both `OnPush` and `Default` are emitted explicitly (rather than omitting `Default`) so the partial
/// declaration round-trips through the linker to the SAME AOT `ɵɵdefineComponent` the direct Full
/// emit produces, regardless of the linker's version-dependent default. The linker assumes OnPush for
/// a placeholder/v22 declaration when `changeDetection` is ABSENT; emitting the strategy verbatim
/// pins the AOT result (an explicit `Default` → omitted on the AOT def; an explicit `OnPush` →
/// `changeDetection: 0`), exactly matching the source AOT path's "omit Default, emit OnPush".
fn change_detection_field(strategy: ChangeDetectionStrategy) -> Option<(String, bool, Expr)> {
    let name = match strategy {
        ChangeDetectionStrategy::OnPush => "OnPush",
        ChangeDetectionStrategy::Default => "Default",
    };
    Some(field(
        "changeDetection",
        i0_member("ChangeDetectionStrategy").prop(name),
    ))
}

/// `encapsulation: i0.ViewEncapsulation.X` — emitted only for a non-default (Emulated is the omitted
/// default the linker assumes).
fn encapsulation_field(encapsulation: ViewEncapsulation) -> Option<(String, bool, Expr)> {
    let name = match encapsulation {
        ViewEncapsulation::Emulated => return None,
        ViewEncapsulation::None => "None",
        ViewEncapsulation::ShadowDom => "ShadowDom",
    };
    Some(field(
        "encapsulation",
        i0_member("ViewEncapsulation").prop(name),
    ))
}

/// Emit `i0.ɵɵngDeclareComponent({ … })` from the directive base + component-specific inputs.
///
/// Field order mirrors Angular's `createComponentDefinitionMap`: the directive base
/// (`minVersion`/`version`/`ngImport`/`type`/`isStandalone`/selector/inputs/outputs/host/providers/
/// queries/viewQueries/exportAs/…) then the component fields (`template`/`isInline`/styles/
/// dependencies/viewProviders/animations/changeDetection/encapsulation/preserveWhitespaces).
pub fn emit_ng_declare_component(
    base: &R3DirectiveMetadata,
    inputs: &PartialComponentInputs,
) -> Expr {
    let mut fields = prelude_fields(base.ty.value.clone());
    push_directive_base(&mut fields, base);

    // `ngImport: i0` sits between the directive base and the component-specific `template` field,
    // exactly as Angular's `createComponentDefinitionMap` places it.
    fields.push(ng_import_field());

    // template + isInline. `isInline: true` is emitted ONLY for an inline `template:` string; an
    // external `templateUrl` inlines its RESOLVED content as the `template` string and OMITS
    // `isInline` (exactly as ng-packagr's partial emitter does — see `GOLDEN_PARTIAL` external
    // resource case). The linker reads an absent `isInline` as "external", which is inert here
    // because the resolved string is already carried verbatim.
    fields.push(field("template", str_lit(inputs.template_html)));
    if inputs.is_inline {
        fields.push(field("isInline", bool_lit(true)));
    }

    if !inputs.styles.is_empty() {
        let styles = inputs.styles.iter().map(|s| str_lit(s)).collect();
        fields.push(field("styles", o::literal_arr(styles, None)));
    }

    if !inputs.dependencies.is_empty() {
        fields.push(field("dependencies", dependencies_array(inputs.dependencies)));
    }

    if let Some(view_providers) = inputs.view_providers {
        fields.push(field("viewProviders", view_providers.clone()));
    }
    if let Some(animations) = inputs.animations {
        fields.push(field("animations", animations.clone()));
    }

    if let Some(cd) = change_detection_field(inputs.change_detection) {
        fields.push(cd);
    }
    if let Some(enc) = encapsulation_field(inputs.encapsulation) {
        fields.push(enc);
    }
    if let Some(pw) = inputs.preserve_whitespaces {
        fields.push(field("preserveWhitespaces", bool_lit(pw)));
    }

    declare_callee("\u{0275}\u{0275}ngDeclareComponent").call_fn(vec![o::literal_map(fields, None)], false)
}

/// Emit `i0.ɵɵngDeclareDirective({ … })` from the directive base. A directive carries the shared
/// directive base only (no template / view metadata).
pub fn emit_ng_declare_directive(base: &R3DirectiveMetadata) -> Expr {
    let mut fields = prelude_fields(base.ty.value.clone());
    push_directive_base(&mut fields, base);
    // `ngImport: i0` is the trailing field on a directive declaration (Angular's
    // `createDirectiveDefinitionMap` order).
    fields.push(ng_import_field());
    declare_callee("\u{0275}\u{0275}ngDeclareDirective").call_fn(vec![o::literal_map(fields, None)], false)
}
