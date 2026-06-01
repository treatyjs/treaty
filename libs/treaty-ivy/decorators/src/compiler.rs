//! `render3/view/compiler.ts` — the direct-to-Ivy entrypoint that assembles
//! `ɵɵdefineComponent({...})` / `ɵɵdefineDirective({...})`.
//!
//! PORT TARGET: see `migration/render3-specs/11-view_compiler.md`
//! Sources:
//!   - `tools/angular-ref/packages/compiler/src/render3/view/compiler.ts`
//!   - `tools/angular-ref/packages/compiler/src/render3/view/api.ts`
//!
//! This module is the top-level orchestrator of component/directive compilation. It takes
//! fully-resolved metadata ([`R3ComponentMetadata`] / [`R3DirectiveMetadata`]) and emits the
//! `output_ast` IR for the `ɵɵdefineComponent` / `ɵɵdefineDirective` call expression plus the
//! `.d.ts` declaration [`Type`]. It owns all the *non-template* definition fields; the actual
//! `ɵɵelement`/`ɵɵtext`/… instruction stream is produced by the template pipeline
//! (`ingest`/`transform`/`emit`).
//!
//! Since the IR in this crate is owned (Box/Vec/String, no arena lifetime), the metadata
//! structs here are owned too — TS unions become Rust enums, and `{[k]: v}` maps whose
//! emitted order is observable become `IndexMap`.
//!
//! The template emission and host-binding emission are abstracted behind the [`TemplateBuilder`]
//! trait (template) and [`HostBindingsBuilder`] trait (host bindings), keeping this orchestrator
//! decoupled from the (large) view compiler. The production wiring lives elsewhere:
//! `compile::RealTemplateBuilder` (in the facade crate `treaty_ivy`) drives the classic
//! [`crate::view::template::TemplateDefinitionBuilder`], and [`DefaultHostBindingsBuilder`]
//! generates the `hostBindings` function + `hostAttrs`/`hostVars` directly. The trait-default
//! [`StubTemplateBuilder`] (empty-bodied template, zero decls/vars/consts) exists only so the
//! orchestration around the template fn can be unit-tested in isolation.

#![allow(clippy::needless_lifetimes)]

use std::collections::HashMap;

use crate::identifiers::R3;
use crate::output_ast::{
    self as o, ArrowBody, Expr, ExprKind, FnParam, LiteralValue, ParseSourceSpan,
    Stmt, StmtKind, StmtModifier, Type,
};
use crate::template::r3_ast as t;
use crate::util::{ts_ignore_comment, type_with_parameters, R3CompiledExpression, R3Reference};
use crate::view::template::chain_statements;

/// A tiny insertion-ordered map (stand-in for `indexmap::IndexMap`, which is not a workspace
/// dependency). Iteration / serialization order is the insertion order — this is golden-observable
/// for `inputs`/`outputs`/`host.*`, so order preservation is required (spec §3.12 / §7).
#[derive(Debug, Clone, PartialEq)]
pub struct OrderedMap<K, V> {
    entries: Vec<(K, V)>,
}

impl<K, V> Default for OrderedMap<K, V> {
    fn default() -> Self {
        OrderedMap { entries: Vec::new() }
    }
}

impl<K: PartialEq, V> OrderedMap<K, V> {
    pub fn new() -> OrderedMap<K, V> {
        OrderedMap { entries: Vec::new() }
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    /// Insert (upsert by key) — later insert overwrites the value but keeps position.
    pub fn insert(&mut self, key: K, value: V) {
        if let Some(slot) = self.entries.iter_mut().find(|(k, _)| *k == key) {
            slot.1 = value;
        } else {
            self.entries.push((key, value));
        }
    }
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: PartialEq + ?Sized,
    {
        self.entries
            .iter()
            .find(|(k, _)| k.borrow() == key)
            .map(|(_, v)| v)
    }
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: std::borrow::Borrow<Q>,
        Q: PartialEq + ?Sized,
    {
        self.entries.iter().any(|(k, _)| k.borrow() == key)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.entries.iter().map(|(k, v)| (k, v))
    }
    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.entries.iter().map(|(k, _)| k)
    }
}

impl<K, V> IntoIterator for OrderedMap<K, V> {
    type Item = (K, V);
    type IntoIter = std::vec::IntoIter<(K, V)>;
    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

const COMPONENT_VARIABLE: &str = "%COMP%";
// `_nghost-%COMP%` / `_ngcontent-%COMP%`.
fn host_attr() -> String {
    format!("_nghost-{COMPONENT_VARIABLE}")
}
fn content_attr() -> String {
    format!("_ngcontent-{COMPONENT_VARIABLE}")
}

// ---------------------------------------------------------------------------
// `core` enums (`../../core`), reproduced locally with their runtime discriminants.
// ---------------------------------------------------------------------------

/// `core.ViewEncapsulation` — discriminants pinned to the runtime enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewEncapsulation {
    Emulated = 0,
    None = 2,
    ShadowDom = 3,
}

impl ViewEncapsulation {
    pub fn as_number(self) -> f64 {
        self as i32 as f64
    }
}

/// `core.ChangeDetectionStrategy` — `OnPush = 0`, `Default = 1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeDetectionStrategy {
    OnPush = 0,
    Default = 1,
}

/// `core.InputFlags` — bitflags. `bitflags` is unavailable, so a `u16` newtype.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InputFlags(pub u16);

impl InputFlags {
    pub const NONE: InputFlags = InputFlags(0);
    pub const SIGNAL_BASED: InputFlags = InputFlags(1 << 0);
    pub const HAS_DECORATOR_INPUT_TRANSFORM: InputFlags = InputFlags(1 << 1);

    #[inline]
    pub fn bits(self) -> u16 {
        self.0
    }
}

impl std::ops::BitOr for InputFlags {
    type Output = InputFlags;
    fn bitor(self, rhs: InputFlags) -> InputFlags {
        InputFlags(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for InputFlags {
    fn bitor_assign(&mut self, rhs: InputFlags) {
        self.0 |= rhs.0;
    }
}

/// `core.SelectorFlags` — bit flags emitted as numeric markers inside an R3 selector entry.
mod selector_flags {
    /// Beginning of a new negative (`:not(...)`) selector.
    pub const NOT: u32 = 0b0001;
    /// Attribute-matching mode.
    pub const ATTRIBUTE: u32 = 0b0010;
    /// Tag-name matching mode.
    pub const ELEMENT: u32 = 0b0100;
    /// Class-name matching mode.
    pub const CLASS: u32 = 0b1000;
}

/// A parsed CSS selector (`CssSelector` from `directive_matching.ts`).
#[derive(Debug, Default)]
struct CssSelector {
    element: Option<String>,
    /// Flat `[name, value, name, value, …]`. Values are lowercased.
    attrs: Vec<String>,
    /// Lowercased class names.
    class_names: Vec<String>,
    not_selectors: Vec<CssSelector>,
}

impl CssSelector {
    fn add_attribute(&mut self, name: &str, value: &str) {
        self.attrs.push(name.to_string());
        self.attrs
            .push(if value.is_empty() { String::new() } else { value.to_lowercase() });
    }
    fn add_class_name(&mut self, name: &str) {
        self.class_names.push(name.to_lowercase());
    }
}

/// `CssSelector.parse(selector)` — splits a (possibly comma-separated) selector string into
/// individual [`CssSelector`]s, honoring `tag`, `.class`, `#id`, `[attr]`, `[attr=value]` and
/// `:not(...)` groups.
fn parse_css_selectors(selector: &str) -> Vec<CssSelector> {
    let mut results: Vec<CssSelector> = Vec::new();
    let chars: Vec<char> = selector.chars().collect();
    let mut i = 0usize;

    let mut current_top = CssSelector::default();
    let mut in_not = false;

    // `current` is either the top-level selector or the active `:not()` sub-selector. We track it
    // by index into `current_top.not_selectors` to satisfy the borrow checker.
    macro_rules! current {
        () => {
            if in_not {
                current_top.not_selectors.last_mut().unwrap()
            } else {
                &mut current_top
            }
        };
    }

    let is_ident = |c: char| c.is_alphanumeric() || c == '-' || c == '_';

    while i < chars.len() {
        let c = chars[i];
        match c {
            c if c.is_whitespace() => {
                i += 1;
            }
            ',' => {
                // Flush the current top-level selector and start a fresh one.
                let finished = std::mem::take(&mut current_top);
                results.push(finished);
                in_not = false;
                i += 1;
            }
            ':' if chars[i..].starts_with(&[':', 'n', 'o', 't', '(']) => {
                in_not = true;
                current_top.not_selectors.push(CssSelector::default());
                i += 5;
            }
            ')' => {
                in_not = false;
                i += 1;
            }
            '.' | '#' => {
                let prefix = c;
                i += 1;
                let start = i;
                while i < chars.len() && is_ident(chars[i]) {
                    i += 1;
                }
                let name: String = chars[start..i].iter().collect();
                if prefix == '#' {
                    current!().add_attribute("id", &name);
                } else {
                    current!().add_class_name(&name);
                }
            }
            '[' => {
                i += 1;
                let start = i;
                while i < chars.len() && chars[i] != ']' {
                    i += 1;
                }
                let inner: String = chars[start..i].iter().collect();
                if i < chars.len() {
                    i += 1; // consume ']'
                }
                if let Some((name, value)) = inner.split_once('=') {
                    let value = value.trim().trim_matches(['"', '\'']);
                    current!().add_attribute(name.trim(), value);
                } else {
                    current!().add_attribute(inner.trim(), "");
                }
            }
            _ if is_ident(c) => {
                let start = i;
                while i < chars.len() && is_ident(chars[i]) {
                    i += 1;
                }
                let tag: String = chars[start..i].iter().collect();
                current!().element = Some(tag);
            }
            _ => {
                i += 1;
            }
        }
    }

    results.push(current_top);
    results
}

/// `parserSelectorToSimpleSelector` — `[element, ...attrs, (CLASS, ...classNames)?]`.
fn simple_selector_to_r3(selector: &CssSelector) -> Vec<SelectorPart> {
    let mut parts: Vec<SelectorPart> = Vec::new();
    let element = match &selector.element {
        Some(e) if e != "*" => e.clone(),
        _ => String::new(),
    };
    parts.push(SelectorPart::Str(element));
    for attr in &selector.attrs {
        parts.push(SelectorPart::Str(attr.clone()));
    }
    if !selector.class_names.is_empty() {
        parts.push(SelectorPart::Num(selector_flags::CLASS as f64));
        for cls in &selector.class_names {
            parts.push(SelectorPart::Str(cls.clone()));
        }
    }
    parts
}

/// `parserSelectorToNegativeSelector` — a `:not(...)` group prefixed with `NOT | mode`.
fn negative_selector_to_r3(selector: &CssSelector) -> Vec<SelectorPart> {
    let mut parts: Vec<SelectorPart> = Vec::new();
    let class_tail = |parts: &mut Vec<SelectorPart>| {
        if !selector.class_names.is_empty() {
            parts.push(SelectorPart::Num(selector_flags::CLASS as f64));
            for cls in &selector.class_names {
                parts.push(SelectorPart::Str(cls.clone()));
            }
        }
    };

    if let Some(element) = &selector.element {
        parts.push(SelectorPart::Num((selector_flags::NOT | selector_flags::ELEMENT) as f64));
        parts.push(SelectorPart::Str(element.clone()));
        for attr in &selector.attrs {
            parts.push(SelectorPart::Str(attr.clone()));
        }
        class_tail(&mut parts);
    } else if !selector.attrs.is_empty() {
        parts.push(SelectorPart::Num((selector_flags::NOT | selector_flags::ATTRIBUTE) as f64));
        for attr in &selector.attrs {
            parts.push(SelectorPart::Str(attr.clone()));
        }
        class_tail(&mut parts);
    } else if !selector.class_names.is_empty() {
        parts.push(SelectorPart::Num((selector_flags::NOT | selector_flags::CLASS) as f64));
        for cls in &selector.class_names {
            parts.push(SelectorPart::Str(cls.clone()));
        }
    }
    parts
}

/// `core.parseSelectorToR3Selector(selector)` → `R3CssSelectorList` (`(string | number)[][]`).
///
/// Port of `core.ts`'s `parseSelectorToR3Selector` over the [`CssSelector`] parser
/// (`directive_matching.ts`). Each selector becomes a flat array: the positive simple selector
/// (`[element, ...attrs, (CLASS, ...classes)?]`) followed by every `:not(...)` negative group
/// (each prefixed with the `NOT | mode` combined flag). `*` and an absent tag both collapse to
/// the empty-string element token.
pub fn parse_selector_to_r3_selector(selector: Option<&str>) -> Vec<Vec<SelectorPart>> {
    let selector = match selector {
        Some(s) if !s.trim().is_empty() => s.trim(),
        _ => return Vec::new(),
    };

    parse_css_selectors(selector)
        .iter()
        .map(|sel| {
            let mut parts = simple_selector_to_r3(sel);
            for not in &sel.not_selectors {
                parts.extend(negative_selector_to_r3(not));
            }
            parts
        })
        .collect()
}

/// A part of an R3 selector entry — either a string token or a numeric `SelectorFlags` marker.
#[derive(Debug, Clone, PartialEq)]
pub enum SelectorPart {
    Str(String),
    Num(f64),
}

// ---------------------------------------------------------------------------
// `render3/view/util.ts` helpers needed here.
// ---------------------------------------------------------------------------

/// `asLiteral(value)` — recursively turn a nested string/number array into a `literalArr`/`literal`
/// with `INFERRED_TYPE`. We model the R3-selector value type ([`SelectorPart`] nesting).
pub fn as_literal_selectors(selectors: &[Vec<SelectorPart>]) -> Expr {
    let outer = selectors
        .iter()
        .map(|group| {
            let inner = group.iter().map(as_literal_part).collect();
            o::literal_arr(inner, None)
        })
        .collect();
    o::literal_arr(outer, None)
}

fn as_literal_part(part: &SelectorPart) -> Expr {
    match part {
        SelectorPart::Str(s) => o::literal(LiteralValue::String(s.clone()), Some(o::inferred_type())),
        SelectorPart::Num(n) => o::literal(LiteralValue::Number(*n), Some(o::inferred_type())),
    }
}

/// `DefinitionMap` — order-preserving object-literal builder. `set` is falsy-skipping (`None`
/// dropped) and upserts by key. Insertion order is the emitted key order (golden-sensitive).
#[derive(Debug, Clone, Default)]
pub struct DefinitionMap {
    /// `(key, quoted, value)` triples in insertion order.
    pub values: Vec<(String, bool, Expr)>,
}

impl DefinitionMap {
    pub fn new() -> DefinitionMap {
        DefinitionMap { values: Vec::new() }
    }

    /// `set(key, value)` — no-op when `value` is `None` (mirrors JS `if (value)` truthiness),
    /// otherwise upserts by key (later set overwrites). `quoted` is always `false`.
    pub fn set(&mut self, key: &str, value: Option<Expr>) {
        if let Some(value) = value {
            if let Some(existing) = self.values.iter_mut().find(|(k, _, _)| k == key) {
                existing.2 = value;
            } else {
                self.values.push((key.to_string(), false, value));
            }
        }
    }

    /// `toLiteralMap()` → `o.literalMap(values)`.
    pub fn to_literal_map(&self) -> Expr {
        o::literal_map(self.values.clone(), None)
    }
}

/// `conditionallyCreateDirectiveBindingLiteral(map, forInputs)` for **outputs** (plain
/// `{field: publicName}` map). Returns `None` when empty. Outputs never track flags or aliases.
fn conditionally_create_outputs_literal(map: &OrderedMap<String, String>) -> Option<Expr> {
    if map.is_empty() {
        return None;
    }
    let entries = map
        .iter()
        .map(|(field, public_name)| {
            (
                field.clone(),
                is_unsafe_object_key(field),
                o::literal(LiteralValue::String(public_name.clone()), Some(o::inferred_type())),
            )
        })
        .collect::<Vec<_>>();
    Some(o::literal_map(entries, None))
}

/// `conditionallyCreateDirectiveBindingLiteral(inputs, /*forInputs*/ true)` — the inputs variant,
/// which tracks declared name (for `ngOnChanges`), transform functions and flags.
fn conditionally_create_inputs_literal(map: &OrderedMap<String, R3InputMetadata>) -> Option<Expr> {
    if map.is_empty() {
        return None;
    }
    let entries = map
        .iter()
        .map(|(minified_name, value)| {
            let declared_name = &value.class_property_name;
            let public_name = &value.binding_property_name;
            let different_declaring_name = public_name != declared_name;
            let has_transform = value.transform_function.is_some();

            let mut flags = InputFlags::NONE;
            if value.is_signal {
                flags |= InputFlags::SIGNAL_BASED;
            }
            if has_transform {
                flags |= InputFlags::HAS_DECORATOR_INPUT_TRANSFORM;
            }

            let expression_value = if different_declaring_name
                || has_transform
                || flags != InputFlags::NONE
            {
                let mut result = vec![
                    o::literal(LiteralValue::Number(flags.bits() as f64), None),
                    o::literal(LiteralValue::String(public_name.clone()), Some(o::inferred_type())),
                ];
                if different_declaring_name || has_transform {
                    result.push(o::literal(
                        LiteralValue::String(declared_name.clone()),
                        Some(o::inferred_type()),
                    ));
                    if has_transform {
                        result.push(value.transform_function.clone().unwrap());
                    }
                }
                o::literal_arr(result, None)
            } else {
                o::literal(LiteralValue::String(public_name.clone()), Some(o::inferred_type()))
            };

            (
                minified_name.clone(),
                is_unsafe_object_key(minified_name),
                expression_value,
            )
        })
        .collect::<Vec<_>>();
    Some(o::literal_map(entries, None))
}

/// `isUnsafeObjectKey(key)` — whether a property name needs quoting in the emitted object literal.
/// (A valid JS identifier never needs quoting; anything else does.)
fn is_unsafe_object_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        None => true,
        Some(c) if !(c.is_ascii_alphabetic() || c == '_' || c == '$') => true,
        _ => !chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$'),
    }
}

/// `stringMapAsLiteralExpression(map)` — `{key: 'value', ...}` with quoted keys (used in `.d.ts`
/// types). For a `[v]`-arrayed value the first element is used; here values are plain strings.
fn string_map_as_literal_expression(map: Option<&OrderedMap<String, String>>) -> Expr {
    let entries = match map {
        Some(map) => map
            .iter()
            .map(|(k, v)| (k.clone(), true, o::literal(LiteralValue::String(v.clone()), None)))
            .collect::<Vec<_>>(),
        None => Vec::new(),
    };
    o::literal_map(entries, None)
}

// ---------------------------------------------------------------------------
// `render3/identifiers` import helpers. (`tsIgnoreComment` / `typeWithParameters` now live in
// the shared `crate::util` module.)
// ---------------------------------------------------------------------------

fn import_r3(id: R3) -> Expr {
    o::import_expr(id.reference(), None)
}

fn import_r3_with_params(id: R3, type_params: Vec<Type>) -> Expr {
    o::import_expr(id.reference(), Some(type_params))
}

// ---------------------------------------------------------------------------
// api.ts — metadata input types (owned).
// ---------------------------------------------------------------------------

/// `DeferBlockDepsEmitMode` (`api.ts` const enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DeferBlockDepsEmitMode {
    PerBlock = 0,
    PerComponent = 1,
}

/// `DeclarationListEmitMode` (`api.ts` const enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DeclarationListEmitMode {
    Direct,
    Closure,
    ClosureResolved,
    RuntimeResolved,
}

/// `R3DirectiveMetadata.deps: R3DependencyMetadata[] | 'invalid' | null` (TS union → enum).
#[derive(Debug, Clone, PartialEq)]
pub enum Deps {
    /// `null` — inherit / no constructor.
    None,
    /// `'invalid'` sentinel.
    Invalid,
    /// Resolved dependency list (uses the factory's [`crate::factory::R3DependencyMetadata`]).
    List(Vec<crate::factory::R3DependencyMetadata>),
}

/// `{usesOnChanges: boolean}`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Lifecycle {
    pub uses_on_changes: bool,
}

/// `{passThroughInput: string | null}`.
#[derive(Debug, Clone, PartialEq)]
pub struct ControlCreate {
    pub pass_through_input: Option<String>,
}

/// `R3InputMetadata` (`api.ts`).
#[derive(Debug, Clone, PartialEq)]
pub struct R3InputMetadata {
    pub class_property_name: String,
    pub binding_property_name: String,
    pub required: bool,
    pub is_signal: bool,
    pub transform_function: Option<Expr>,
}

/// `R3HostMetadata.specialAttributes` (`{styleAttr?, classAttr?}`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpecialAttrs {
    pub style_attr: Option<String>,
    pub class_attr: Option<String>,
}

/// `R3HostMetadata` (`api.ts`). Iteration order of the maps is emitted → `IndexMap`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct R3HostMetadata {
    pub attributes: OrderedMap<String, Expr>,
    pub listeners: OrderedMap<String, String>,
    pub properties: OrderedMap<String, String>,
    pub special_attributes: SpecialAttrs,
}

/// `R3HostDirectiveMetadata` (`api.ts`).
#[derive(Debug, Clone, PartialEq)]
pub struct R3HostDirectiveMetadata {
    pub directive: R3Reference,
    pub is_forward_reference: bool,
    pub inputs: Option<OrderedMap<String, String>>,
    pub outputs: Option<OrderedMap<String, String>>,
}

/// `MaybeForwardRefExpression | string[]` query predicate. Forward-ref wrapping is resolved
/// upstream of this metadata, so the expression variant carries a bare [`Expr`]; it is mapped to
/// [`crate::view::queries::MaybeForwardRefExpression`] (with `ForwardRefHandling::None`) by
/// [`map_query_metadata`] before query-function generation.
#[derive(Debug, Clone, PartialEq)]
pub enum QueryPredicate {
    Expr(Expr),
    Selectors(Vec<String>),
}

/// `R3QueryMetadata` (`api.ts`).
#[derive(Debug, Clone, PartialEq)]
pub struct R3QueryMetadata {
    pub property_name: String,
    pub first: bool,
    pub predicate: QueryPredicate,
    pub descendants: bool,
    pub emit_distinct_changes_only: bool,
    pub read: Option<Expr>,
    pub static_: bool,
    pub is_signal: bool,
}

/// `R3DirectiveMetadata` (`api.ts`). Owned; `type` → `ty` (Rust keyword).
#[derive(Debug, Clone, PartialEq)]
pub struct R3DirectiveMetadata {
    pub name: String,
    pub ty: R3Reference,
    pub type_argument_count: u32,
    pub type_source_span: ParseSourceSpan,
    pub deps: Deps,
    pub selector: Option<String>,
    pub queries: Vec<R3QueryMetadata>,
    pub view_queries: Vec<R3QueryMetadata>,
    pub host: R3HostMetadata,
    pub lifecycle: Lifecycle,
    /// Insertion order matters (feeds inputs literal + `.d.ts` inputs type).
    pub inputs: OrderedMap<String, R3InputMetadata>,
    pub outputs: OrderedMap<String, String>,
    pub uses_inheritance: bool,
    pub control_create: Option<ControlCreate>,
    pub export_as: Option<Vec<String>>,
    pub providers: Option<Expr>,
    pub is_standalone: bool,
    pub is_signal: bool,
    pub host_directives: Option<Vec<R3HostDirectiveMetadata>>,
    pub legacy_optional_chaining: bool,
}

/// `R3TemplateDependencyKind` (`api.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum R3TemplateDependencyKind {
    Directive = 0,
    Pipe = 1,
    NgModule = 2,
}

/// `R3TemplateDependency` — the trait every declaration must provide (`kind` + `type`).
pub trait R3TemplateDependency {
    fn kind(&self) -> R3TemplateDependencyKind;
    /// The `type` expression used in the `dependencies` array.
    fn ty(&self) -> Expr;
}

/// A simple owned template dependency carrying just `{kind, type}` (the common case used by the
/// `dependencies` array). Richer variants (`R3DirectiveDependencyMetadata`, etc.) can be added as
/// the binder port lands.
#[derive(Debug, Clone, PartialEq)]
pub struct R3TemplateDependencyMetadata {
    pub kind: R3TemplateDependencyKind,
    pub ty: Expr,
}

impl R3TemplateDependency for R3TemplateDependencyMetadata {
    fn kind(&self) -> R3TemplateDependencyKind {
        self.kind
    }
    fn ty(&self) -> Expr {
        self.ty.clone()
    }
}

/// `R3ForeignComponentMetadata` (`api.ts`).
#[derive(Debug, Clone, PartialEq)]
pub struct R3ForeignComponentMetadata {
    pub name: String,
    pub component: Expr,
}

/// `R3ComponentMetadata.template` (`{nodes, ngContentSelectors, preserveWhitespaces}`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ComponentTemplate {
    pub nodes: Vec<t::Node>,
    pub ng_content_selectors: Vec<String>,
    pub preserve_whitespaces: Option<bool>,
}

/// `R3ComponentDeferMetadata` (`api.ts` discriminated union). `PerBlock` keys by a stable id
/// (never AST-node pointer identity).
#[derive(Debug, Clone, PartialEq)]
pub enum R3ComponentDeferMetadata {
    PerBlock { blocks: HashMap<usize, Option<Expr>> },
    PerComponent { dependencies_fn: Option<Expr> },
}

impl R3ComponentDeferMetadata {
    pub fn mode(&self) -> DeferBlockDepsEmitMode {
        match self {
            R3ComponentDeferMetadata::PerBlock { .. } => DeferBlockDepsEmitMode::PerBlock,
            R3ComponentDeferMetadata::PerComponent { .. } => DeferBlockDepsEmitMode::PerComponent,
        }
    }
}

/// `changeDetection: ChangeDetectionStrategy | o.Expression | null` (union → enum).
#[derive(Debug, Clone, PartialEq)]
pub enum ChangeDetection {
    /// Statically-resolved numeric strategy (global compilation).
    Strategy(ChangeDetectionStrategy),
    /// Unresolved expression (local compilation) — emitted verbatim.
    Expr(Expr),
}

/// `R3ComponentMetadata<DeclarationT>` (`api.ts`, extends directive via composition).
#[derive(Debug, Clone, PartialEq)]
pub struct R3ComponentMetadata<D: R3TemplateDependency> {
    pub base: R3DirectiveMetadata,
    pub template: ComponentTemplate,
    pub declarations: Vec<D>,
    pub defer: R3ComponentDeferMetadata,
    pub declaration_list_emit_mode: DeclarationListEmitMode,
    pub styles: Vec<String>,
    pub external_styles: Option<Vec<String>>,
    /// Mutated in place during compile (null→Emulated→None — see §4.10 / §7).
    pub encapsulation: ViewEncapsulation,
    pub animations: Option<Expr>,
    pub view_providers: Option<Expr>,
    pub relative_context_file_path: String,
    pub i18n_use_external_ids: bool,
    pub change_detection: Option<ChangeDetection>,
    pub relative_template_path: Option<String>,
    pub has_directive_dependencies: bool,
    pub raw_imports: Option<Expr>,
    pub foreign_imports: Option<Vec<R3ForeignComponentMetadata>>,
    /// The class names this component imports (its `imports: [...]` entries plus, for a multi-class
    /// file, the sibling-declared class names). Used to resolve a STANDALONE component's
    /// template-used pipe (`value | pipeName`) to the imported class that declares it, so that
    /// imported pipe is listed in the runtime `dependencies` array. A pipe is referenced by its
    /// registered `name`, not its class, so the class is recovered by matching the imported names
    /// against the pipe name (`percent01` → `Percent01Pipe` / `Percent01`). Empty for the
    /// NgModule-scoped (non-standalone) path, which resolves pipes from module scope instead.
    pub imported_directive_names: Vec<String>,
}

// ---------------------------------------------------------------------------
// Template builder abstraction. The production implementation is
// `compile::RealTemplateBuilder` (in the facade crate `treaty_ivy`), driving
// [`crate::view::template::TemplateDefinitionBuilder`]; [`StubTemplateBuilder`] is the test stub.
// ---------------------------------------------------------------------------

/// The result of running the template pipeline: the emitted `template` function expression plus
/// the slot-allocation summary (`decls`/`vars`), interned `consts` (+ their initializers) and the
/// extracted `ngContentSelectors`.
#[derive(Debug, Clone, PartialEq)]
pub struct TemplateBuilderResult {
    /// `function MyComponent_Template(rf, ctx) { … }`.
    pub template_fn: Expr,
    /// Number of declaration slots (`root.decls`).
    pub decls: u32,
    /// Number of binding slots (`root.vars`).
    pub vars: u32,
    /// The `consts` array entries (interned literals / attribute arrays).
    pub consts: Vec<Expr>,
    /// Statements that must run before the `consts` array is built (arrow-fn form).
    pub consts_initializers: Vec<Stmt>,
    /// `ngContentSelectors`, or `None` when there is no projection.
    pub content_selectors: Option<Expr>,
    /// Hoisted nested-view functions (`@if`/`@for`/`@switch`/`@defer` branch + loop bodies,
    /// projection fallbacks, `ng-template` bodies). Angular declares these as top-level sibling
    /// `function …_Template(rf, ctx) {…}` statements on `ConstantPool.statements`, emitted BEFORE
    /// the `ɵɵdefineComponent({…})` call (never inside the root view body). The orchestrator merges
    /// these into the caller-provided `pool_statements` so they print as leading declarations.
    pub pool_statements: Vec<Stmt>,
}

/// Abstraction over template emission (`ingestComponent` → `transform` → `emitTemplateFn`).
///
/// The production implementation, `compile::RealTemplateBuilder` (in the facade crate `treaty_ivy`), runs the classic
/// [`crate::view::template::TemplateDefinitionBuilder`] over the component's template nodes and
/// returns the emitted function plus its `decls`/`vars`/`consts`/`ngContentSelectors`. The
/// trait keeps that (large) builder out of this orchestrator and lets it be unit-tested with the
/// [`StubTemplateBuilder`] default.
pub trait TemplateBuilder {
    /// Ingest + transform + emit the template for the given component metadata.
    fn build<D: R3TemplateDependency>(
        &mut self,
        meta: &R3ComponentMetadata<D>,
        all_deferrable_deps_fn: Option<&Expr>,
    ) -> TemplateBuilderResult;
}

/// The default placeholder builder: emits `function <Name>_Template(rf, ctx) {}` with no body and
/// zero decls/vars/consts. Faithful in *shape* (the function name + `(rf, ctx)` params) so the
/// orchestration and golden structure around it can be exercised.
#[derive(Debug, Default)]
pub struct StubTemplateBuilder;

impl TemplateBuilder for StubTemplateBuilder {
    fn build<D: R3TemplateDependency>(
        &mut self,
        meta: &R3ComponentMetadata<D>,
        _all_deferrable_deps_fn: Option<&Expr>,
    ) -> TemplateBuilderResult {
        let template_fn = o::fn_(
            vec![
                FnParam::new("rf", None),
                FnParam::new("ctx", None),
            ],
            Vec::new(),
            None,
            Some(format!("{}_Template", meta.base.name)),
        );
        TemplateBuilderResult {
            template_fn,
            decls: 0,
            vars: 0,
            consts: Vec::new(),
            consts_initializers: Vec::new(),
            content_selectors: None,
            pool_statements: Vec::new(),
        }
    }
}

/// Abstraction over host-binding emission (`ingestHostBinding` → `transform` →
/// `emitHostBindingFunction`). Returns the optional `hostBindings` function and sets
/// `hostAttrs`/`hostVars` on the definition map. The production implementation is
/// [`DefaultHostBindingsBuilder`], which generates the host-bindings function directly.
pub trait HostBindingsBuilder {
    /// `createHostBindingsFunction(...)` — returns the host-bindings fn (or `None`) and may set
    /// `hostAttrs`/`hostVars` on the definition map. `host` may be mutated (special attrs folded in).
    fn build(
        &mut self,
        host: &mut R3HostMetadata,
        selector: &str,
        name: &str,
        legacy_optional_chaining: bool,
        definition_map: &mut DefinitionMap,
        pool_statements: &mut Vec<Stmt>,
    ) -> Option<Expr>;
}

/// `createHostBindingsFunction(...)` port. Generates the `hostBindings: function(rf, ctx) {…}`
/// definition field, and as side effects sets `hostAttrs` / `hostVars` on the definition map.
///
/// This is a direct generator (Angular routes the equivalent work through the
/// `ingestHostBinding` → `transform` → `emitHostBindingFunction` pipeline). It:
///   - parses each property/listener value string with [`crate::expression::parser::Parser`],
///   - lowers it via [`crate::expression_converter`] rooted at the `ctx` param,
///   - emits the same instruction set the runtime expects (`ɵɵlistener` in CREATE;
///     `ɵɵdomProperty`/`ɵɵsyntheticHostProperty`/`ɵɵattribute`/`ɵɵclassProp`/`ɵɵstyleProp` in
///     UPDATE),
///   - splits static `class`/`style` host attributes into the `Classes`/`Styles` AttributeMarker
///     groups of `hostAttrs` (see [`host_attrs_array`]).
/// Since there are no slotted host instructions here, `ɵɵadvance` interleaving and host-property
/// slot indices (which the transform pipeline computes) are not modelled; pipe lowering in host
/// bindings is likewise out of scope.
#[derive(Debug, Default)]
pub struct DefaultHostBindingsBuilder;

/// Pure-function allocator for the host-bindings function — the host analogue of the template
/// builder's `BuilderPipes`. A host `[id]="['red', id]"` binding runs Angular's full host-bindings
/// pipeline, which includes `generatePureLiteralStructures`: a literal array/object is extracted into
/// a const-pool `ɵɵpureFunctionN(varOffset, $cN$, …args)` whose factory is hoisted to a module-level
/// `const`. This allocator owns the two responsibilities the converter delegates: assigning each pure
/// function its `varOffset` (Angular's `bindingCount` — pure functions are numbered AFTER every
/// regular host binding, starting at `regular_bindings`, each consuming `1 + num_args` host vars) and
/// hoisting + de-duping the factory declarations.
///
/// Host bindings carry no pipes/arrows in any compliance fixture, so only the pure-function hooks are
/// implemented; `allocate_pipe` is unreachable here.
struct HostPureFunctions {
    state: std::cell::RefCell<HostPureState>,
    /// The number of regular host bindings (Angular `bindingCount`): pure-function var offsets begin
    /// here, after every `ɵɵdomProperty`/`ɵɵattribute`/`ɵɵclassProp`/`ɵɵstyleProp` op.
    regular_bindings: usize,
}

#[derive(Default)]
struct HostPureState {
    /// Running var-offset cursor for the next pure function (seeded at `regular_bindings`).
    var_cursor: usize,
    /// Hoisted factories, de-duped structurally: `(factory_body, minted_name)`.
    interned: Vec<(Expr, String)>,
    /// Counter seeding the minted `$cN$` reference names.
    next_const: usize,
}

impl HostPureFunctions {
    fn new(regular_bindings: usize) -> Self {
        HostPureFunctions {
            state: std::cell::RefCell::new(HostPureState {
                var_cursor: regular_bindings,
                ..HostPureState::default()
            }),
            regular_bindings,
        }
    }

    /// The total host vars consumed once every pure function is assigned (`bindingCount` plus the
    /// `1 + num_args` slots each pure function reserved). Equals the regular-binding count when no
    /// pure functions were extracted.
    fn total_host_vars(&self) -> usize {
        self.state.borrow().var_cursor.max(self.regular_bindings)
    }

    /// The hoisted factory declarations (`const $cN$ = (a0, …) => <literal>;`), in mint order, to be
    /// emitted as siblings of the directive definition (Angular `ConstantPool.statements`).
    fn factory_declarations(&self) -> Vec<Stmt> {
        self.state
            .borrow()
            .interned
            .iter()
            .map(|(factory, name)| {
                Stmt::with_modifiers(
                    StmtKind::DeclareVar {
                        name: name.clone(),
                        value: Some(factory.clone()),
                        ty: None,
                    },
                    StmtModifier::FINAL,
                )
            })
            .collect()
    }
}

impl crate::expression_converter::PipeSlotAllocator for HostPureFunctions {
    fn allocate_pipe(
        &self,
        _name: &str,
        _total_args: usize,
    ) -> crate::expression_converter::PipeSlots {
        // No host compliance fixture pipes through a host binding; the converter only reaches this
        // when a `BindingPipe` is present, which the host path never produces.
        crate::expression_converter::PipeSlots { slot: 0, var_offset: 0 }
    }

    fn allocate_pure_function_slot(&self, num_args: usize) -> Option<usize> {
        let mut state = self.state.borrow_mut();
        let offset = state.var_cursor;
        state.var_cursor = offset + 1 + num_args;
        Some(offset)
    }

    fn intern_pure_function_factory(&self, factory: &Expr, _is_arrow: bool) -> Option<String> {
        let mut state = self.state.borrow_mut();
        if let Some((_, name)) = state.interned.iter().find(|(f, _)| f.is_equivalent(factory)) {
            return Some(name.clone());
        }
        let n = state.next_const;
        state.next_const = n + 1;
        let name = format!("$c{n}$");
        state.interned.push((factory.clone(), name.clone()));
        Some(name)
    }
}

/// `class.X`/`style.X` host bindings reserve TWO host vars; every other regular binding reserves ONE
/// (Angular `bindingCount`). Animation string host attrs and listeners are create-block ops that
/// consume no host vars. Mirrors the per-binding accounting in [`DefaultHostBindingsBuilder::build`].
fn host_binding_var_count(prop: &str) -> usize {
    if prop.starts_with("class.") || prop.starts_with("style.") {
        2
    } else {
        1
    }
}

/// `RenderFlags.Create` / `RenderFlags.Update` (mirrors the runtime bitmask phase selector).
const RENDER_FLAG_CREATE: f64 = 0b01 as f64;
const RENDER_FLAG_UPDATE: f64 = 0b10 as f64;

const HOST_CONTEXT_NAME: &str = "ctx";
const HOST_RENDER_FLAGS: &str = "rf";
const HOST_EVENT_NAME: &str = "$event";

/// `ɵɵfoo(...params)` as an expression statement.
fn host_instruction(reference: R3, params: Vec<Expr>) -> Stmt {
    import_r3(reference).call_fn(params, false).to_stmt()
}

/// `if (rf & flag) { …statements… }` — `renderFlagCheckIfStmt`.
fn host_render_flag_if(flag: f64, statements: Vec<Stmt>) -> Stmt {
    o::if_stmt(
        o::variable(HOST_RENDER_FLAGS, None)
            .bitwise_and(o::literal(LiteralValue::Number(flag), None)),
        statements,
        None,
    )
}

/// Parse + lower a host *property* value (a binding expression) rooted at `ctx`, extracting any
/// literal array/object into a const-pool `ɵɵpureFunctionN(varOffset, $cN$, …args)` through `pures`
/// (Angular's host-bindings `generatePureLiteralStructures` pass). The factory hoisting + var-offset
/// assignment are owned by the [`HostPureFunctions`] allocator.
fn lower_host_property_value(
    value: &str,
    pures: &HostPureFunctions,
) -> crate::expression_converter::ConvertedBinding {
    let parser = crate::expression::parser::Parser::default();
    let parsed = parser.parse_binding(
        value,
        crate::expression::ast::ParseSourceSpan { start: 0, end: 0 },
        0,
    );
    let resolver =
        crate::expression_converter::CtxResolver::new(o::variable(HOST_CONTEXT_NAME, None));
    crate::expression_converter::convert_host_property_binding_with_pure(
        &parsed.ast,
        &resolver,
        pures,
    )
}

/// `LocalResolver` for a host-listener handler body: every implicit-receiver read roots at the
/// component context (`ctx`) EXCEPT `$event`, which Angular's `resolveDollarEvent` keeps as the bare
/// handler parameter (a `$event` read must NOT become `ctx.$event`). Mirrors the template-side
/// `ListenerResolver`, restricted to the host case (no `@for` loop vars / cross-view `@let`s).
struct HostListenerResolver;

impl crate::expression_converter::LocalResolver for HostListenerResolver {
    fn resolve_implicit_receiver(&self) -> Expr {
        o::variable(HOST_CONTEXT_NAME, None)
    }

    fn maybe_resolve_local(&self, name: &str) -> Option<Expr> {
        if name == HOST_EVENT_NAME {
            return Some(o::variable(HOST_EVENT_NAME, None));
        }
        None
    }
}

/// Parse + lower a host *listener* handler (an action) rooted at `ctx`, keeping `$event` a bare
/// parameter read (Angular `resolveDollarEvent`) rather than a `ctx.$event` property access.
fn lower_host_listener_value(value: &str) -> crate::expression_converter::ConvertedBinding {
    let parser = crate::expression::parser::Parser::default();
    let parsed = parser.parse_action(
        value,
        crate::expression::ast::ParseSourceSpan { start: 0, end: 0 },
        0,
    );
    crate::expression_converter::convert_action_binding_with(&parsed.ast, &HostListenerResolver)
}

impl HostBindingsBuilder for DefaultHostBindingsBuilder {
    fn build(
        &mut self,
        host: &mut R3HostMetadata,
        _selector: &str,
        name: &str,
        _legacy_optional_chaining: bool,
        definition_map: &mut DefinitionMap,
        pool_statements: &mut Vec<Stmt>,
    ) -> Option<Expr> {
        // The parser treats `class`/`style` specially — fold them into the attributes map
        // (faithful to `createHostBindingsFunction`'s side effect).
        if let Some(style) = host.special_attributes.style_attr.clone() {
            host.attributes
                .insert("style".to_string(), o::literal(LiteralValue::String(style), None));
        }
        if let Some(class) = host.special_attributes.class_attr.clone() {
            host.attributes
                .insert("class".to_string(), o::literal(LiteralValue::String(class), None));
        }

        // hostAttrs — the static attributes array, grouped by AttributeMarker (plain pairs first,
        // then the Classes=1 group, then the Styles=2 group). The folded `class`/`style` values are
        // split into individual class names / style key-value pairs under their markers by
        // `host_attrs_array` (mirroring `parse_extracted_styles` + `serializeAttributes`). The modern
        // `animate.enter`/`animate.leave` string host keys are NOT static attributes — they reify to
        // a CREATE-block `ɵɵanimateEnter`/`ɵɵanimateLeave` instruction below — so `host_attrs_array`
        // omits them from `hostAttrs`.
        if let Some(attrs) = host_attrs_array(&host.attributes) {
            definition_map.set("hostAttrs", Some(attrs));
        }

        // Build the function body. CREATE: animate-string instructions + listeners. UPDATE:
        // properties + class/style/attr.
        let mut create_stmts: Vec<Stmt> = Vec::new();
        let mut update_stmts: Vec<Stmt> = Vec::new();
        let mut host_vars: u32 = 0;

        // Modern `animate.enter`/`animate.leave` host keys with a STRING value: Angular's view
        // compiler lowers them to a CREATE-block `ɵɵanimateEnter("fade")` / `ɵɵanimateLeave("fade")`
        // instruction (the `animate*` family in `reify.ts`) rather than a static
        // `hostAttrs:["animate.enter","fade"]` entry. The value is a literal class-list string passed
        // verbatim. We read them straight from the attributes map (they are intentionally left there
        // so the shape is non-mutating; `host_attrs_array` filters them out of `hostAttrs`).
        for (key, value) in host.attributes.iter() {
            if !is_animate_host_attr(key) {
                continue;
            }
            let reference = if key == "animate.leave" {
                R3::AnimationLeave
            } else {
                R3::AnimationEnter
            };
            create_stmts.push(host_instruction(reference, vec![value.clone()]));
        }

        // Listeners → `ɵɵlistener(eventName, HostListenerFn)` (CREATE). A modern animation listener
        // (`(animate.enter)`/`(animate.leave)`) instead reifies to
        // `ɵɵanimateEnterListener`/`ɵɵanimateLeaveListener` taking ONLY the handler function (no
        // event-name argument), with the handler named on the SANITIZED event (`.` dropped, so
        // `animate.enter` → `animateenter`) — faithful to Angular's `reify.ts` + `naming.ts`.
        for (event, handler_src) in host.listeners.iter() {
            let converted = lower_host_listener_value(handler_src);
            let mut body = converted.stmts;
            body.push(Stmt::bare(StmtKind::Return(converted.expr)));

            if event == "animate.enter" || event == "animate.leave" {
                let handler_name =
                    format!("{name}_{}_HostBindingHandler", event.replace('.', ""));
                let handler_fn = o::fn_(
                    vec![FnParam::new(HOST_EVENT_NAME, None)],
                    body,
                    None,
                    Some(handler_name),
                );
                let reference = if event == "animate.leave" {
                    R3::AnimationLeaveListener
                } else {
                    R3::AnimationEnterListener
                };
                create_stmts.push(host_instruction(reference, vec![handler_fn]));
                continue;
            }

            // `naming.ts`: `${name}_${event}_HostBindingHandler`.
            let handler_name = format!("{name}_{}_HostBindingHandler", event.replace('.', "_"));
            let handler_fn = o::fn_(
                vec![FnParam::new(HOST_EVENT_NAME, None)],
                body,
                None,
                Some(handler_name),
            );
            create_stmts.push(host_instruction(
                R3::Listener,
                vec![o::literal(LiteralValue::String(event.clone()), None), handler_fn],
            ));
        }

        // Properties → UPDATE. Route by prefix:
        //   `attr.X`  → ɵɵattribute('X', value)
        //   `class.X` → ɵɵclassProp('X', value)
        //   `style.X` → ɵɵstyleProp('X', value)
        //   `@X`      → ɵɵsyntheticHostProperty('X', value)
        //   else      → ɵɵdomProperty('name', value)
        //
        // Angular's `StylingBuilder` buffers `style.X`/`class.X` host bindings SEPARATELY from the
        // regular property bindings and flushes them together at the END of the update block — all
        // `ɵɵstyleProp` first, then all `ɵɵclassProp` — regardless of their source order relative to
        // the regular bindings or each other (`host_bindings.ts` styling flush order). So emit the
        // regular (`attribute`/`domProperty`/`syntheticHostProperty`) bindings in source order, then
        // the buffered style bindings, then the buffered class bindings. `chain_statements` (below)
        // then folds each homogeneous run into a single chained call.
        //
        // Host-var accounting (Angular `bindingCount`): a regular binding reserves ONE host var; a
        // `style.X`/`class.X` styling binding reserves TWO (the bound value plus styling bookkeeping).
        let mut style_stmts: Vec<Stmt> = Vec::new();
        let mut class_stmts: Vec<Stmt> = Vec::new();

        // Pure-literal extraction (`generatePureLiteralStructures`): a literal array/object host
        // binding value becomes a `ɵɵpureFunctionN` whose `varOffset` is assigned AFTER every regular
        // binding (Angular `bindingCount`), so pre-count the regular bindings to seed the allocator.
        let regular_bindings: usize =
            host.properties.keys().map(|p| host_binding_var_count(p)).sum();
        let pures = HostPureFunctions::new(regular_bindings);

        for (prop, value_src) in host.properties.iter() {
            let converted = lower_host_property_value(value_src, &pures);
            let value = converted.expr;
            let spill = converted.stmts;

            if let Some(cls) = prop.strip_prefix("class.") {
                host_vars += 2;
                class_stmts.extend(spill);
                class_stmts.push(host_instruction(
                    R3::ClassProp,
                    vec![o::literal(LiteralValue::String(cls.to_string()), None), value],
                ));
                continue;
            }
            if let Some(sty) = prop.strip_prefix("style.") {
                host_vars += 2;
                style_stmts.extend(spill);
                style_stmts.push(host_instruction(
                    R3::StyleProp,
                    vec![o::literal(LiteralValue::String(sty.to_string()), None), value],
                ));
                continue;
            }

            // Regular binding: spilled temporaries precede the instruction (Angular emits the
            // safe-navigation temp assignment before the consuming op), one host var each.
            host_vars += 1;
            update_stmts.extend(spill);
            let stmt = if let Some(attr) = prop.strip_prefix("attr.") {
                host_instruction(
                    R3::Attribute,
                    vec![o::literal(LiteralValue::String(attr.to_string()), None), value],
                )
            } else if let Some(synthetic) = prop.strip_prefix('@') {
                host_instruction(
                    R3::SyntheticHostProperty,
                    vec![o::literal(LiteralValue::String(synthetic.to_string()), None), value],
                )
            } else {
                host_instruction(
                    R3::DomProperty,
                    vec![o::literal(LiteralValue::String(prop.clone()), None), value],
                )
            };
            update_stmts.push(stmt);
        }
        // Styling flush: all style bindings, then all class bindings (Angular's styling order).
        update_stmts.extend(style_stmts);
        update_stmts.extend(class_stmts);

        // Pure-function var slots (`1 + num_args` each, numbered after the regular bindings) grow the
        // host-var total beyond the per-binding `host_vars` count. `total_host_vars` is that final
        // total (equal to `host_vars` when no pure function was extracted).
        debug_assert_eq!(host_vars as usize, regular_bindings);
        let host_vars = pures.total_host_vars() as u32;

        // Hoist each extracted pure-literal factory (`const $cN$ = (a0, …) => <literal>;`) as a
        // sibling of the directive definition (Angular `ConstantPool.statements`), so the
        // `ɵɵpureFunctionN(slot, $cN$, …)` call references a declared const.
        pool_statements.extend(pures.factory_declarations());

        // hostVars (only when > 0).
        if host_vars > 0 {
            definition_map.set(
                "hostVars",
                Some(o::literal(LiteralValue::Number(host_vars as f64), None)),
            );
        }

        if create_stmts.is_empty() && update_stmts.is_empty() {
            return None;
        }

        // Fold consecutive chainable instructions into a single chained call, exactly as the
        // template builder does for element-level bindings (Angular `chainOperationsInList`):
        // `ɵɵclassProp("a", …)("b", …)("c", …)`, `ɵɵstyleProp(…)(…)`, `ɵɵlistener(…)(…)`, etc.
        // A spilled temporary statement (safe-navigation lowering) breaks a run, matching Angular.
        let create_stmts = chain_statements(create_stmts);
        let update_stmts = chain_statements(update_stmts);

        let mut body: Vec<Stmt> = Vec::new();
        if !create_stmts.is_empty() {
            body.push(host_render_flag_if(RENDER_FLAG_CREATE, create_stmts));
        }
        if !update_stmts.is_empty() {
            body.push(host_render_flag_if(RENDER_FLAG_UPDATE, update_stmts));
        }

        Some(o::fn_(
            vec![
                FnParam::new(HOST_RENDER_FLAGS, None),
                FnParam::new(HOST_CONTEXT_NAME, None),
            ],
            body,
            None,
            Some(format!("{name}_HostBindings")),
        ))
    }
}

/// `core.AttributeMarker` discriminants used in the `hostAttrs` / element-attributes array. Only
/// the markers reachable from host static attributes are reproduced here.
const ATTRIBUTE_MARKER_CLASSES: f64 = 1.0;
const ATTRIBUTE_MARKER_STYLES: f64 = 2.0;

/// Hyphenate a camelCase CSS property name (`backgroundColor` → `background-color`), matching the
/// `hyphenate` helper used by Angular's extracted-style parser (`parse_extracted_styles.ts`).
fn hyphenate_style_prop(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    let mut out = String::with_capacity(chars.len() + 2);
    for (i, &c) in chars.iter().enumerate() {
        if i > 0
            && c.is_ascii_uppercase()
            && chars[i - 1].is_ascii_lowercase()
        {
            out.push('-');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// Port of `parse_extracted_styles.ts`'s `parse(value)`: tokenize a CSS `style` attribute value
/// into a flat `[prop, value, prop, value, …]` list. Honors `()` nesting and `'`/`"` quoting so
/// `:`/`;` inside `url(...)` or quoted strings don't split. Property names are hyphenated.
fn parse_style_value(value: &str) -> Vec<String> {
    let bytes: Vec<char> = value.chars().collect();
    let mut styles: Vec<String> = Vec::new();
    let mut i = 0usize;
    let mut paren_depth: i32 = 0;
    // 0 = none, '\'' or '"' for active quote char.
    let mut quote: char = '\0';
    let mut value_start: Option<usize> = None;
    let mut prop_start = 0usize;
    let mut current_prop: Option<String> = None;

    while i < bytes.len() {
        let c = bytes[i];
        i += 1;
        match c {
            '(' => paren_depth += 1,
            ')' => paren_depth -= 1,
            '\'' => {
                if quote == '\0' {
                    quote = '\'';
                } else if quote == '\'' && (i < 2 || bytes[i - 2] != '\\') {
                    quote = '\0';
                }
            }
            '"' => {
                if quote == '\0' {
                    quote = '"';
                } else if quote == '"' && (i < 2 || bytes[i - 2] != '\\') {
                    quote = '\0';
                }
            }
            ':' => {
                if current_prop.is_none() && paren_depth == 0 && quote == '\0' {
                    let raw: String = bytes[prop_start..i - 1].iter().collect();
                    current_prop = Some(hyphenate_style_prop(raw.trim()));
                    value_start = Some(i);
                }
            }
            ';' => {
                if current_prop.is_some()
                    && value_start.is_some()
                    && paren_depth == 0
                    && quote == '\0'
                {
                    let vs = value_start.unwrap();
                    let style_val: String = bytes[vs..i - 1].iter().collect();
                    styles.push(current_prop.take().unwrap());
                    styles.push(style_val.trim().to_string());
                    prop_start = i;
                    value_start = None;
                }
            }
            _ => {}
        }
    }

    if let (Some(prop), Some(vs)) = (current_prop, value_start) {
        let style_val: String = bytes[vs..].iter().collect();
        styles.push(prop);
        styles.push(style_val.trim().to_string());
    }

    styles
}

/// Whether a host-attribute key is a modern `animate.enter` / `animate.leave` key. Such keys with a
/// string value are routed to a CREATE-block `ɵɵanimateEnter`/`ɵɵanimateLeave` instruction by the
/// host-bindings builder and are NOT emitted as a static `hostAttrs` entry.
fn is_animate_host_attr(key: &str) -> bool {
    key == "animate.enter" || key == "animate.leave"
}

/// Build the `hostAttrs` consts-style array from the static attributes map, applying
/// `AttributeMarker` grouping: plain `name, value` pairs first, then a `Classes` (1) group
/// (individual class names), then a `Styles` (2) group (`name, value` pairs).
///
/// The folded `class` / `style` attributes carry their raw string *values*; faithfully to the
/// transform (`parse_extracted_styles.ts` + `const_collection.ts`'s `serializeAttributes`) the
/// `class` value is whitespace-split into individual class names under the `Classes` marker, and
/// the `style` value is parsed into `[prop, value, …]` pairs under the `Styles` marker. Anything
/// else is emitted as a plain `[name, value]` pair.
fn host_attrs_array(attributes: &OrderedMap<String, Expr>) -> Option<Expr> {
    if attributes.is_empty() {
        return None;
    }

    let mut plain: Vec<Expr> = Vec::new();
    let mut classes: Vec<Expr> = Vec::new();
    let mut styles: Vec<Expr> = Vec::new();

    for (key, value) in attributes.iter() {
        // Modern `animate.enter`/`animate.leave` string host keys are emitted as CREATE-block
        // `ɵɵanimateEnter`/`ɵɵanimateLeave` instructions by the host-bindings builder, not as static
        // `hostAttrs` entries — skip them here so they never reach the attributes array.
        if is_animate_host_attr(key) && matches!(&value.kind, ExprKind::Literal(LiteralValue::String(_))) {
            continue;
        }
        // `class`/`style` are only special-cased when the value is a static string literal — a
        // dynamic expression keeps the plain `[name, expr]` shape.
        let string_value = match (key.as_str(), &value.kind) {
            ("class", ExprKind::Literal(LiteralValue::String(s))) => Some(s.clone()),
            ("style", ExprKind::Literal(LiteralValue::String(s))) => Some(s.clone()),
            _ => None,
        };

        match (key.as_str(), string_value) {
            ("class", Some(raw)) => {
                for token in raw.split_whitespace() {
                    classes.push(o::literal(LiteralValue::String(token.to_string()), None));
                }
            }
            ("style", Some(raw)) => {
                for chunk in parse_style_value(&raw) {
                    styles.push(o::literal(LiteralValue::String(chunk), None));
                }
            }
            _ => {
                plain.push(o::literal(LiteralValue::String(key.clone()), None));
                plain.push(value.clone());
            }
        }
    }

    let mut elements: Vec<Expr> = plain;
    if !classes.is_empty() {
        elements.push(o::literal(LiteralValue::Number(ATTRIBUTE_MARKER_CLASSES), None));
        elements.extend(classes);
    }
    if !styles.is_empty() {
        elements.push(o::literal(LiteralValue::Number(ATTRIBUTE_MARKER_STYLES), None));
        elements.extend(styles);
    }

    if elements.is_empty() {
        None
    } else {
        Some(o::literal_arr(elements, None))
    }
}

/// Backwards-compatible unit struct under the former placeholder name. It is no longer a stub:
/// it delegates to the real [`DefaultHostBindingsBuilder`]. Kept so existing call sites
/// (`compile.rs`) keep compiling without churn.
#[derive(Debug, Default)]
pub struct StubHostBindingsBuilder;

impl HostBindingsBuilder for StubHostBindingsBuilder {
    fn build(
        &mut self,
        host: &mut R3HostMetadata,
        selector: &str,
        name: &str,
        legacy_optional_chaining: bool,
        definition_map: &mut DefinitionMap,
        pool_statements: &mut Vec<Stmt>,
    ) -> Option<Expr> {
        DefaultHostBindingsBuilder.build(
            host,
            selector,
            name,
            legacy_optional_chaining,
            definition_map,
            pool_statements,
        )
    }
}

// ---------------------------------------------------------------------------
// baseDirectiveFields / addFeatures.
// ---------------------------------------------------------------------------

/// Map the compiler-facing [`R3QueryMetadata`] list onto the
/// [`crate::view::queries::R3QueryMetadata`] input type consumed by the query-generation module.
///
/// The two structs differ only in shape: this module's [`QueryPredicate`] carries a bare [`Expr`]
/// (forward-ref handling is resolved upstream), so it maps to a [`MaybeForwardRefExpression`] with
/// [`ForwardRefHandling::None`]; `static_` → `is_static`.
fn map_query_metadata(queries: &[R3QueryMetadata]) -> Vec<crate::view::queries::R3QueryMetadata> {
    use crate::view::queries as q;
    queries
        .iter()
        .map(|query| q::R3QueryMetadata {
            property_name: query.property_name.clone(),
            first: query.first,
            predicate: match &query.predicate {
                QueryPredicate::Selectors(selectors) => q::QueryPredicate::Selectors(selectors.clone()),
                QueryPredicate::Expr(expr) => q::QueryPredicate::Expression(q::MaybeForwardRefExpression {
                    expression: expr.clone(),
                    forward_ref: q::ForwardRefHandling::None,
                }),
            },
            descendants: query.descendants,
            emit_distinct_changes_only: query.emit_distinct_changes_only,
            read: query.read.clone(),
            is_static: query.static_,
            is_signal: query.is_signal,
        })
        .collect()
}

/// `baseDirectiveFields(meta, pool, bindingParser)` — the shared definition fields. Key order is
/// load-bearing (golden output).
fn base_directive_fields<H: HostBindingsBuilder>(
    meta: &R3DirectiveMetadata,
    host_builder: &mut H,
    default_selector: Option<&str>,
    pool_statements: &mut Vec<Stmt>,
) -> DefinitionMap {
    let mut definition_map = DefinitionMap::new();
    // Mirror `extractDirectiveMetadata` (compiler-cli directive/shared.ts): the resolved selector
    // falls back to `defaultSelector` when it is absent or an empty string. Components pass
    // `getDefaultComponentElementName()` ("ng-component"); directives pass `None`. This is what
    // produces the synthetic `selectors: [["ng-component"]]` for a selector-less @Component.
    let selector = match meta.selector.as_deref() {
        Some(s) if !s.is_empty() => Some(s),
        _ => default_selector,
    };
    let selectors = parse_selector_to_r3_selector(selector);

    // e.g. `type: MyDirective`.
    definition_map.set("type", Some(meta.ty.value.clone()));

    // e.g. `selectors: [['', 'someDir', '']]`.
    if !selectors.is_empty() {
        definition_map.set("selectors", Some(as_literal_selectors(&selectors)));
    }

    // contentQueries / viewQuery — generated by `crate::view::queries`
    // (`createContentQueriesFunction` / `createViewQueriesFunction`). A fresh `ConstantPool`
    // stand-in is threaded through both, matching the real entrypoint's `constantPool` arg.
    let mut query_pool = crate::view::queries::ConstantPool::new();

    if !meta.queries.is_empty() {
        // e.g. `contentQueries: (rf, ctx, dirIndex) => { ... }`.
        let queries = map_query_metadata(&meta.queries);
        definition_map.set(
            "contentQueries",
            Some(crate::view::queries::create_content_queries_function(
                &queries,
                &mut query_pool,
                Some(&meta.name),
            )),
        );
    }

    if !meta.view_queries.is_empty() {
        // e.g. `viewQuery: (rf, ctx) => { ... }`.
        let view_queries = map_query_metadata(&meta.view_queries);
        definition_map.set(
            "viewQuery",
            Some(crate::view::queries::create_view_queries_function(
                &view_queries,
                &mut query_pool,
                Some(&meta.name),
            )),
        );
    }

    // The selector-predicate arrays the query functions hoisted into the shared `_cN` pool
    // (`getConstLiteral(..., /*forceShared*/ true)`) are emitted as top-level `const _cN = [...]`
    // declarations BEFORE the definition — Angular's `ConstantPool.statements`. Surface them onto
    // the caller's pool so the referenced `_cN` constants are actually declared (a query whose
    // predicate is an expression token, e.g. `viewChild(SomeDir)`, never interns into the pool, so
    // this is empty for those).
    pool_statements.extend(query_pool.statements());

    // hostBindings (always called — also sets hostAttrs/hostVars as a side effect).
    let mut host = meta.host.clone();
    let host_bindings = host_builder.build(
        &mut host,
        selector.unwrap_or(""),
        &meta.name,
        meta.legacy_optional_chaining,
        &mut definition_map,
        pool_statements,
    );
    definition_map.set("hostBindings", host_bindings);

    // inputs / outputs.
    definition_map.set("inputs", conditionally_create_inputs_literal(&meta.inputs));
    definition_map.set("outputs", conditionally_create_outputs_literal(&meta.outputs));

    if let Some(export_as) = &meta.export_as {
        let arr = export_as
            .iter()
            .map(|e| o::literal(LiteralValue::String(e.clone()), None))
            .collect();
        definition_map.set("exportAs", Some(o::literal_arr(arr, None)));
    }

    // standalone only emitted when false (true is the runtime default).
    if !meta.is_standalone {
        definition_map.set("standalone", Some(o::literal(LiteralValue::Bool(false), None)));
    }
    // signals only when true.
    if meta.is_signal {
        definition_map.set("signals", Some(o::literal(LiteralValue::Bool(true), None)));
    }

    definition_map
}

/// `addFeatures(definitionMap, meta)` — order is load-bearing (HostDirectives must precede
/// InheritDefinition for runtime execution order).
fn add_features(
    definition_map: &mut DefinitionMap,
    meta: &R3DirectiveMetadata,
    view_providers: Option<&Expr>,
    external_styles: Option<&[String]>,
) {
    let mut features: Vec<Expr> = Vec::new();

    // 1. ProvidersFeature.
    if meta.providers.is_some() || view_providers.is_some() {
        let mut args = vec![meta
            .providers
            .clone()
            .unwrap_or_else(|| o::literal_arr(Vec::new(), None))];
        if let Some(vp) = view_providers {
            args.push(vp.clone());
        }
        features.push(import_r3(R3::ProvidersFeature).call_fn(args, false));
    }

    // 2. HostDirectivesFeature (before inheritance).
    if let Some(hds) = &meta.host_directives {
        if !hds.is_empty() {
            features.push(
                import_r3(R3::HostDirectivesFeature)
                    .call_fn(vec![create_host_directives_feature_arg(hds)], false),
            );
        }
    }

    // 3. InheritDefinitionFeature.
    if meta.uses_inheritance {
        features.push(import_r3(R3::InheritDefinitionFeature));
    }

    // 4. NgOnChangesFeature.
    if meta.lifecycle.uses_on_changes {
        features.push(import_r3(R3::NgOnChangesFeature));
    }

    // 5. ControlFeature.
    if let Some(cc) = &meta.control_create {
        let arg = match &cc.pass_through_input {
            Some(s) => o::literal(LiteralValue::String(s.clone()), None),
            None => o::literal(LiteralValue::Null, None),
        };
        features.push(import_r3(R3::ControlFeature).call_fn(vec![arg], false));
    }

    // 6. ExternalStylesFeature (component-only).
    if let Some(styles) = external_styles {
        if !styles.is_empty() {
            let nodes = styles
                .iter()
                .map(|s| o::literal(LiteralValue::String(s.clone()), None))
                .collect();
            features.push(
                import_r3(R3::ExternalStylesFeature).call_fn(vec![o::literal_arr(nodes, None)], false),
            );
        }
    }

    if !features.is_empty() {
        definition_map.set("features", Some(o::literal_arr(features, None)));
    }
}

// ---------------------------------------------------------------------------
// compileDirectiveFromMetadata.
// ---------------------------------------------------------------------------

/// `compileDirectiveFromMetadata(meta, pool, bindingParser)`.
pub fn compile_directive_from_metadata<H: HostBindingsBuilder>(
    meta: &R3DirectiveMetadata,
    host_builder: &mut H,
) -> R3CompiledExpression {
    // Query-predicate `const _cN = [...]` declarations the query functions hoist into the shared
    // pool are returned on `statements` (Angular's `ConstantPool.statements`) for the caller to
    // emit before the definition.
    let mut statements: Vec<Stmt> = Vec::new();
    let mut definition_map = base_directive_fields(meta, host_builder, None, &mut statements);
    add_features(&mut definition_map, meta, None, None);
    let expression = import_r3(R3::DefineDirective)
        // `.callFn([map], undefined, /*pure*/ true)`.
        .call_fn(vec![definition_map.to_literal_map()], true);
    let ty = create_directive_type(meta);
    R3CompiledExpression {
        expression,
        ty,
        statements,
    }
}

// ---------------------------------------------------------------------------
// compileComponentFromMetadata.
// ---------------------------------------------------------------------------

/// `compileComponentFromMetadata(meta, pool, bindingParser)` — the core entry. See spec §4.1.
///
/// The template pipeline is abstracted behind [`TemplateBuilder`] and the host-binding pipeline
/// behind [`HostBindingsBuilder`]. `pool_statements` stands in for `ConstantPool.statements`
/// (the defer-deps const + style consts are pushed onto it).
pub fn compile_component_from_metadata<D, T, H>(
    meta: &mut R3ComponentMetadata<D>,
    template_builder: &mut T,
    host_builder: &mut H,
    pool_statements: &mut Vec<Stmt>,
) -> R3CompiledExpression
where
    D: R3TemplateDependency,
    T: TemplateBuilder,
    H: HostBindingsBuilder,
{
    // Components fall back to the default element name (`ng-component`) when they have no selector,
    // matching `getDefaultComponentElementName()` plumbed through `extractDirectiveMetadata`.
    let mut definition_map =
        base_directive_fields(&meta.base, host_builder, Some("ng-component"), pool_statements);
    add_features(
        &mut definition_map,
        &meta.base,
        meta.view_providers.as_ref(),
        meta.external_styles.as_deref(),
    );

    let template_type_name = meta.base.name.clone();

    // Defer deps fn (PerComponent only).
    let mut all_deferrable_deps_fn: Option<Expr> = None;
    if let R3ComponentDeferMetadata::PerComponent { dependencies_fn } = &meta.defer {
        if let Some(deps_fn) = dependencies_fn {
            let fn_name = format!("{template_type_name}_DeferFn");
            pool_statements.push(Stmt::with_modifiers(
                StmtKind::DeclareVar {
                    name: fn_name.clone(),
                    value: Some(deps_fn.clone()),
                    ty: None,
                },
                StmtModifier::FINAL,
            ));
            all_deferrable_deps_fn = Some(o::variable(fn_name, None));
        }
    }

    // Angular computes `TemplateCompilationMode` (`DomOnly` when standalone with no directive
    // dependencies, else `Full`) and threads it into `ingestComponent`. That mode only gates
    // pipeline-only selectorless optimizations; the classic `TemplateDefinitionBuilder` driven by
    // [`TemplateBuilder`] is mode-agnostic, so there is nothing to thread through here.

    // Ingest + transform + emit (delegated to the template-builder abstraction).
    let tpl = template_builder.build(meta, all_deferrable_deps_fn.as_ref());

    // Hoisted nested-view functions live on `ConstantPool.statements` — top-level siblings emitted
    // before the `ɵɵdefineComponent({…})` call (Angular `ɵɵdefineComponent` is preceded by every
    // `function …_Template(rf, ctx) {…}` it references). Surface them onto the shared pool.
    pool_statements.extend(tpl.pool_statements.iter().cloned());

    if let Some(content_selectors) = &tpl.content_selectors {
        definition_map.set("ngContentSelectors", Some(content_selectors.clone()));
    }

    definition_map.set("decls", Some(o::literal(LiteralValue::Number(tpl.decls as f64), None)));
    definition_map.set("vars", Some(o::literal(LiteralValue::Number(tpl.vars as f64), None)));

    if !tpl.consts.is_empty() {
        if !tpl.consts_initializers.is_empty() {
            // `() => { …initializers…; return [ …consts… ]; }`.
            let mut body = tpl.consts_initializers.clone();
            body.push(Stmt::bare(StmtKind::Return(o::literal_arr(tpl.consts.clone(), None))));
            definition_map.set("consts", Some(o::arrow_fn(Vec::new(), ArrowBody::Block(body), None)));
        } else {
            definition_map.set("consts", Some(o::literal_arr(tpl.consts.clone(), None)));
        }
    }

    definition_map.set("template", Some(tpl.template_fn));

    // Dependencies.
    //
    // The directive declarations are seeded by the front-end (binder-matched `<Foo>`/`@Foo`
    // template usage). Pipes referenced in the template (`value | pipeName`) are NOT carried in
    // `meta.declarations` — Angular collects them from template usage and resolves each registered
    // pipe `name` to its declaring class. We reproduce that collection here (the
    // `compileComponentFromMetadata` definition assembly is where the resolved declaration list is
    // realized) by walking the template AST for pipe references and resolving each to its class
    // identifier. The runtime `dependencies` array lists directive declarations followed by the
    // template-used pipe classes (each pipe contributing its declaring class once, in first-use
    // order). The standard ngtsc declaration order coincides with this for the common case.
    //
    // A pipe contributes a dependency only when it resolves to a class in the component's pipe
    // SCOPE. The scope differs by component kind:
    //
    //   * A STANDALONE component draws its scope from its own `imports: [...]` (plus, in a
    //     multi-class file, its sibling classes). A template pipe `value | pipeName` is a dependency
    //     exactly when one of those imported classes is the pipe registered under `pipeName`. The
    //     pipe is referenced by its registered NAME, not its class, so we recover the class by
    //     matching the imported class names against the pipe name (`percent01` resolves to the
    //     imported `Percent01Pipe` / `Percent01`). A used pipe whose name matches no import (e.g. a
    //     built-in `slice` used without importing it) yields no dependency — Angular reports that as
    //     a template error rather than emitting a bogus dependency.
    //   * A NgModule-declared (NON-standalone) component draws its scope from its declaring module,
    //     so a template pipe `name` resolves to its co-declared class via the name→class convention.
    let pipe_deps = if meta.base.is_standalone {
        resolve_standalone_pipe_dependencies(&meta.template.nodes, &meta.imported_directive_names)
    } else {
        collect_template_pipe_dependencies(&meta.template.nodes)
    };

    if meta.declaration_list_emit_mode != DeclarationListEmitMode::RuntimeResolved
        && (!meta.declarations.is_empty() || !pipe_deps.is_empty())
    {
        let mut entries: Vec<Expr> = meta.declarations.iter().map(|d| d.ty()).collect();
        entries.extend(pipe_deps);
        let list = o::literal_arr(entries, None);
        definition_map.set(
            "dependencies",
            Some(compile_declaration_list(list, meta.declaration_list_emit_mode)),
        );
    } else if meta.declaration_list_emit_mode == DeclarationListEmitMode::RuntimeResolved {
        let mut args = vec![meta.base.ty.value.clone()];
        if let Some(raw) = &meta.raw_imports {
            args.push(raw.clone());
        }
        definition_map.set(
            "dependencies",
            Some(import_r3(R3::GetComponentDepsFactory).call_fn(args, false)),
        );
    }

    // Styles / encapsulation (in-place mutation of `meta.encapsulation`). Note: our enum has no
    // `null` state; we treat the entry default as Emulated (the JS `null → Emulated` normalization).
    let mut has_styles = meta.external_styles.as_ref().is_some_and(|s| !s.is_empty());

    if !meta.styles.is_empty() {
        let style_values: Vec<String> = if meta.encapsulation == ViewEncapsulation::Emulated {
            compile_styles(&meta.styles, &content_attr(), &host_attr())
        } else {
            meta.styles.clone()
        };
        let mut style_nodes: Vec<Expr> = Vec::new();
        for style in &style_values {
            if !style.trim().is_empty() {
                // Mirrors `constantPool.getConstLiteral(o.literal(style))`: a short string literal
                // (below the pool's 50-char inclusion threshold) is emitted inline, which is the
                // common case for component styles. Only long strings would be hoisted into a
                // shared `_cN` constant, and that requires the template's shared `ConstantPool`
                // (the template-builder owns it) — not the directive-level `pool_statements` here.
                style_nodes.push(o::literal(LiteralValue::String(style.clone()), None));
            }
        }
        if !style_nodes.is_empty() {
            has_styles = true;
            definition_map.set("styles", Some(o::literal_arr(style_nodes, None)));
        }
    }

    if !has_styles && meta.encapsulation == ViewEncapsulation::Emulated {
        // No styles → don't generate css selectors on elements.
        meta.encapsulation = ViewEncapsulation::None;
    }

    // Only set encapsulation if it's not the default (Emulated).
    if meta.encapsulation != ViewEncapsulation::Emulated {
        definition_map.set(
            "encapsulation",
            Some(o::literal(LiteralValue::Number(meta.encapsulation.as_number()), None)),
        );
    }

    // Animations → `data: {animation: <expr>}`.
    if let Some(animations) = &meta.animations {
        definition_map.set(
            "data",
            Some(o::literal_map(
                vec![("animation".to_string(), false, animations.clone())],
                None,
            )),
        );
    }

    // Change detection. Angular v21 (`compiler.ts` setting-change-detection block):
    // a numeric strategy is emitted only when it differs from `ChangeDetectionStrategy.Default`
    // (the implicit runtime default). OnPush (= 0) is therefore emitted as `changeDetection: 0`,
    // while Default (= 1) is omitted.
    if let Some(cd) = &meta.change_detection {
        match cd {
            ChangeDetection::Strategy(strategy) => {
                if *strategy != ChangeDetectionStrategy::Default {
                    definition_map.set(
                        "changeDetection",
                        Some(o::literal(LiteralValue::Number(*strategy as i32 as f64), None)),
                    );
                }
            }
            // Unresolved expression (local compilation): emit as-is.
            ChangeDetection::Expr(expr) => {
                definition_map.set("changeDetection", Some(expr.clone()));
            }
        }
    }

    let expression = import_r3(R3::DefineComponent)
        // `.callFn([map], undefined, /*pure*/ true)`.
        .call_fn(vec![definition_map.to_literal_map()], true);
    let ty = create_component_type(meta);

    R3CompiledExpression {
        expression,
        ty,
        statements: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Type creation (.d.ts).
// ---------------------------------------------------------------------------

/// `createComponentType(meta)`.
pub fn create_component_type<D: R3TemplateDependency>(meta: &R3ComponentMetadata<D>) -> Type {
    let mut type_params = create_base_directive_type_params(&meta.base);
    type_params.push(string_array_as_type(&meta.template.ng_content_selectors));
    type_params.push(o::expression_type(
        o::literal(LiteralValue::Bool(meta.base.is_standalone), None),
        None,
        None,
    ));
    type_params.push(create_host_directives_type(&meta.base));
    if meta.base.is_signal {
        type_params.push(o::expression_type(
            o::literal(LiteralValue::Bool(true), None),
            None,
            None,
        ));
    }
    o::expression_type(
        import_r3_with_params(R3::ComponentDeclaration, type_params),
        None,
        None,
    )
}

/// `createDirectiveType(meta)`.
pub fn create_directive_type(meta: &R3DirectiveMetadata) -> Type {
    let mut type_params = create_base_directive_type_params(meta);
    // Directives have no NgContentSelectors slot → `never` (NONE_TYPE).
    type_params.push(o::none_type());
    type_params.push(o::expression_type(
        o::literal(LiteralValue::Bool(meta.is_standalone), None),
        None,
        None,
    ));
    type_params.push(create_host_directives_type(meta));
    if meta.is_signal {
        type_params.push(o::expression_type(
            o::literal(LiteralValue::Bool(true), None),
            None,
            None,
        ));
    }
    o::expression_type(
        import_r3_with_params(R3::DirectiveDeclaration, type_params),
        None,
        None,
    )
}

fn string_as_type(s: &str) -> Type {
    o::expression_type(o::literal(LiteralValue::String(s.to_string()), None), None, None)
}

fn string_array_as_type(arr: &[String]) -> Type {
    if arr.is_empty() {
        o::none_type()
    } else {
        let entries = arr
            .iter()
            .map(|v| o::literal(LiteralValue::String(v.clone()), None))
            .collect();
        o::expression_type(o::literal_arr(entries, None), None, None)
    }
}

fn create_base_directive_type_params(meta: &R3DirectiveMetadata) -> Vec<Type> {
    // Strip newlines from the selector for the `.d.ts` string literal (must be one line).
    let selector_for_type = meta.selector.as_ref().map(|s| s.replace('\n', ""));

    vec![
        type_with_parameters(meta.ty.ty.clone(), meta.type_argument_count),
        match &selector_for_type {
            Some(s) => string_as_type(s),
            None => o::none_type(),
        },
        match &meta.export_as {
            Some(arr) => string_array_as_type(arr),
            None => o::none_type(),
        },
        o::expression_type(get_inputs_type_expression(meta), None, None),
        o::expression_type(string_map_as_literal_expression(Some(&meta.outputs)), None, None),
        string_array_as_type(
            &meta.queries.iter().map(|q| q.property_name.clone()).collect::<Vec<_>>(),
        ),
    ]
}

fn get_inputs_type_expression(meta: &R3DirectiveMetadata) -> Expr {
    let entries = meta
        .inputs
        .iter()
        .map(|(key, value)| {
            let mut values = vec![
                (
                    "alias".to_string(),
                    true,
                    o::literal(LiteralValue::String(value.binding_property_name.clone()), None),
                ),
                (
                    "required".to_string(),
                    true,
                    o::literal(LiteralValue::Bool(value.required), None),
                ),
            ];
            if value.is_signal {
                values.push((
                    "isSignal".to_string(),
                    true,
                    o::literal(LiteralValue::Bool(true), None),
                ));
            }
            (key.clone(), true, o::literal_map(values, None))
        })
        .collect::<Vec<_>>();
    o::literal_map(entries, None)
}

fn create_host_directives_type(meta: &R3DirectiveMetadata) -> Type {
    let host_directives = match &meta.host_directives {
        Some(hds) if !hds.is_empty() => hds,
        _ => return o::none_type(),
    };

    let entries = host_directives
        .iter()
        .map(|hd| {
            o::literal_map(
                vec![
                    ("directive".to_string(), false, o::typeof_expr(hd.directive.ty.clone())),
                    (
                        "inputs".to_string(),
                        false,
                        string_map_as_literal_expression(hd.inputs.as_ref()),
                    ),
                    (
                        "outputs".to_string(),
                        false,
                        string_map_as_literal_expression(hd.outputs.as_ref()),
                    ),
                ],
                None,
            )
        })
        .collect();
    o::expression_type(o::literal_arr(entries, None), None, None)
}

// ---------------------------------------------------------------------------
// Host-directives feature arg + mapping array.
// ---------------------------------------------------------------------------

/// `createHostDirectivesFeatureArg(hostDirectives)`.
fn create_host_directives_feature_arg(host_directives: &[R3HostDirectiveMetadata]) -> Expr {
    let mut expressions: Vec<Expr> = Vec::new();
    let mut has_forward_ref = false;

    for current in host_directives {
        // Shorthand when there are no inputs/outputs.
        if current.inputs.is_none() && current.outputs.is_none() {
            expressions.push(current.directive.ty.clone());
        } else {
            let mut keys = vec![("directive".to_string(), false, current.directive.ty.clone())];
            if let Some(inputs) = &current.inputs {
                if let Some(inputs_literal) = create_host_directives_mapping_array(inputs) {
                    keys.push(("inputs".to_string(), false, inputs_literal));
                }
            }
            if let Some(outputs) = &current.outputs {
                if let Some(outputs_literal) = create_host_directives_mapping_array(outputs) {
                    keys.push(("outputs".to_string(), false, outputs_literal));
                }
            }
            expressions.push(o::literal_map(keys, None));
        }

        if current.is_forward_reference {
            has_forward_ref = true;
        }
    }

    // With a forward ref → `function() { return [HostDir]; }`; else a plain array.
    if has_forward_ref {
        o::fn_(
            Vec::new(),
            vec![Stmt::bare(StmtKind::Return(o::literal_arr(expressions, None)))],
            None,
            None,
        )
    } else {
        o::literal_arr(expressions, None)
    }
}

/// `createHostDirectivesMappingArray(mapping)` — `{a:'b'}` → `['a','b']`, or `None` when empty.
pub fn create_host_directives_mapping_array(mapping: &OrderedMap<String, String>) -> Option<Expr> {
    let mut elements: Vec<Expr> = Vec::new();
    for (public_name, alias) in mapping.iter() {
        elements.push(o::literal(LiteralValue::String(public_name.clone()), None));
        elements.push(o::literal(LiteralValue::String(alias.clone()), None));
    }
    if elements.is_empty() {
        None
    } else {
        Some(o::literal_arr(elements, None))
    }
}

// ---------------------------------------------------------------------------
// compileDeclarationList.
// ---------------------------------------------------------------------------

/// `compileDeclarationList(list, mode)`.
fn compile_declaration_list(list: Expr, mode: DeclarationListEmitMode) -> Expr {
    match mode {
        DeclarationListEmitMode::Direct => list,
        // `() => [MyDir]`.
        DeclarationListEmitMode::Closure => {
            o::arrow_fn(Vec::new(), ArrowBody::Expr(Box::new(list)), None)
        }
        // `() => [MyDir].map(ng.resolveForwardRef)`.
        DeclarationListEmitMode::ClosureResolved => {
            let resolved = list
                .prop("map")
                .call_fn(vec![import_r3(R3::ResolveForwardRef)], false);
            o::arrow_fn(Vec::new(), ArrowBody::Expr(Box::new(resolved)), None)
        }
        DeclarationListEmitMode::RuntimeResolved => {
            panic!("Unsupported with an array of pre-resolved dependencies")
        }
    }
}

// ---------------------------------------------------------------------------
// Template pipe dependency collection.
//
// Angular's `dependencies` array includes every pipe whose registered `name` is referenced in the
// component template, resolved to its declaring class. Our directive declarations are seeded by
// the front-end, but pipes (referenced by their `name` string, e.g. `value | myPipe`) are not.
// We walk the template AST here, collect distinct pipe names in first-use (document) order, and
// resolve each to its declaring class identifier. The pipe `name` → class mapping follows the
// compiler convention that a pipe `name` is the lower-camel form of its PascalCase class
// (`myPipe` → `MyPipe`); `pascal_case_pipe_name` reverses that to recover the class reference
// used in the `dependencies` array.
// ---------------------------------------------------------------------------

use crate::expression::ast::{AstNode, AstVisitor, BindingPipeType, ExprKind as AstExprKind};

/// Walk the component template AST and return one class-reference [`Expr`] per distinct pipe
/// referenced by name in a binding expression, in first-use order.
fn collect_template_pipe_dependencies(nodes: &[t::Node]) -> Vec<Expr> {
    let mut collector = PipeNameCollector::default();
    collect_pipes_in_nodes(nodes, &mut collector);
    collector
        .names
        .into_iter()
        .map(|name| o::variable(pascal_case_pipe_name(&name), None))
        .collect()
}

/// Resolve a STANDALONE component's template-used pipes to the imported classes that declare them.
///
/// A standalone component lists its pipes in `imports: [...]`; a template pipe `value | pipeName`
/// is a dependency exactly when one of those imported classes is the pipe registered under
/// `pipeName`. The pipe is referenced by its registered NAME, not its class, so we recover the
/// class by matching the imported class names against the pipe name: the imported class
/// `Percent01Pipe` (or `Percent01`) is the declarer of the pipe named `percent01`. Used pipes whose
/// name matches no import contribute no dependency (a built-in used without importing it is a
/// template error in Angular, not a synthesized dependency). Resolution is in first-use order to
/// match the directive-declaration ordering of the `dependencies` array, and each imported class is
/// listed at most once even if its pipe is used several times.
fn resolve_standalone_pipe_dependencies(nodes: &[t::Node], imported_names: &[String]) -> Vec<Expr> {
    let mut collector = PipeNameCollector::default();
    collect_pipes_in_nodes(nodes, &mut collector);
    let mut out: Vec<Expr> = Vec::new();
    let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
    for name in &collector.names {
        if let Some(class) = resolve_pipe_name_to_import(name, imported_names) {
            if used.insert(class.clone()) {
                out.push(o::variable(&class, None));
            }
        }
    }
    out
}

/// Find the imported class that declares the pipe registered under `pipe_name`, by name shape.
///
/// A pipe `name` is the lower-camel/kebab form of its PascalCase class. Treaty's convention also
/// permits a trailing `Pipe` on the class (`percent01` → `Percent01Pipe`), so an imported class
/// matches `pipe_name` when it equals the PascalCase of the name, that PascalCase plus a `Pipe`
/// suffix, or — defensively — the class with a trailing `Pipe` stripped equals the PascalCase. The
/// first matching import (in `imports` order) wins.
fn resolve_pipe_name_to_import(pipe_name: &str, imported_names: &[String]) -> Option<String> {
    let pascal = pascal_case_pipe_name(pipe_name);
    let with_suffix = format!("{pascal}Pipe");
    imported_names
        .iter()
        .find(|class| {
            **class == pascal
                || **class == with_suffix
                || class.strip_suffix("Pipe").map(|c| c == pascal).unwrap_or(false)
        })
        .cloned()
}

/// Turn a pipe `name` into its declaring-class identifier (`myPipe` → `MyPipe`,
/// `my-pipe` → `MyPipe`): split on `-`, upper-case the first letter of each segment, and join.
fn pascal_case_pipe_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for segment in name.split('-') {
        let mut chars = segment.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

/// Accumulates distinct pipe names (by reference) in first-encounter order.
#[derive(Default)]
struct PipeNameCollector {
    names: Vec<String>,
    seen: std::collections::HashSet<String>,
}

impl PipeNameCollector {
    fn record(&mut self, name: &str) {
        if self.seen.insert(name.to_string()) {
            self.names.push(name.to_string());
        }
    }
}

impl AstVisitor for PipeNameCollector {
    fn visit_pipe(&mut self, node: &AstNode) {
        if let AstExprKind::BindingPipe { name, exp, args, pipe_type, .. } = &node.kind {
            // Recurse into the piped expression BEFORE recording this pipe's name. In a chained
            // expression `value | inner | outer` the AST nests outer-most first (`outer`'s `exp`
            // is `(value | inner)`), so descending into `exp` first yields names in left-to-right
            // source order (`inner`, then `outer`). Angular orders the `dependencies` array by the
            // component's scope/declaration order — never by AST-traversal (outer-first) order —
            // and for the common case that scope order coincides with the document order in which
            // the pipes are first used. Collecting in document order therefore reproduces the
            // golden's `[innerPipe, outerPipe]` ordering, where recording before recursing would
            // wrongly emit the reversed `[outerPipe, innerPipe]`.
            self.visit(exp);
            self.visit_all(args);
            // Only pipes referenced by `name` (the `value | pipeName` form) participate in the
            // module-scope dependency resolution; `ReferencedDirectly` pipes carry their own
            // class reference and are not name-resolved.
            if *pipe_type == BindingPipeType::ReferencedByName {
                self.record(name);
            }
        }
    }
}

/// Visit every binding expression reachable from `nodes`, descending into element/template/
/// content/component containers and control-flow blocks, feeding each expression to `collector`.
fn collect_pipes_in_nodes(nodes: &[t::Node], collector: &mut PipeNameCollector) {
    for node in nodes {
        match node {
            t::Node::BoundText(n) => collector.visit(&n.value),
            t::Node::BoundAttribute(n) => collector.visit(&n.value),
            t::Node::BoundEvent(n) => collector.visit(&n.handler),
            t::Node::Element(n) => {
                collect_pipes_in_attrs(&n.inputs, &n.outputs, collector);
                collect_pipes_in_nodes(&n.children, collector);
            }
            t::Node::Template(n) => {
                collect_pipes_in_attrs(&n.inputs, &n.outputs, collector);
                for attr in &n.template_attrs {
                    if let t::TemplateAttr::Bound(b) = attr {
                        collector.visit(&b.value);
                    }
                }
                collect_pipes_in_nodes(&n.children, collector);
            }
            t::Node::Content(n) => collect_pipes_in_nodes(&n.children, collector),
            t::Node::Component(n) => collect_pipes_in_nodes(&n.children, collector),
            t::Node::LetDeclaration(n) => collector.visit(&n.value),
            t::Node::DeferredBlock(b) => collect_pipes_in_nodes(&b.children, collector),
            t::Node::DeferredBlockPlaceholder(b) => collect_pipes_in_nodes(&b.children, collector),
            t::Node::DeferredBlockLoading(b) => collect_pipes_in_nodes(&b.children, collector),
            t::Node::DeferredBlockError(b) => collect_pipes_in_nodes(&b.children, collector),
            t::Node::SwitchBlock(b) => {
                collector.visit(&b.expression);
                for group in &b.groups {
                    collect_pipes_in_nodes(&group.children, collector);
                }
            }
            t::Node::ForLoopBlock(b) => {
                collector.visit(&b.expression.ast);
                collect_pipes_in_nodes(&b.children, collector);
                if let Some(empty) = &b.empty {
                    collect_pipes_in_nodes(&empty.children, collector);
                }
            }
            t::Node::IfBlock(b) => {
                for branch in &b.branches {
                    if let Some(expr) = &branch.expression {
                        collector.visit(expr);
                    }
                    collect_pipes_in_nodes(&branch.children, collector);
                }
            }
            _ => {}
        }
    }
}

/// Visit the binding expressions of an element/template's inputs + outputs.
fn collect_pipes_in_attrs(
    inputs: &[t::BoundAttribute],
    outputs: &[t::BoundEvent],
    collector: &mut PipeNameCollector,
) {
    for input in inputs {
        collector.visit(&input.value);
    }
    for output in outputs {
        collector.visit(&output.handler);
    }
}

// ---------------------------------------------------------------------------
// Host-binding parsing helpers (parseHostBindings / verifyHostBindings — leaf helpers).
// ---------------------------------------------------------------------------

/// `ParsedHostBindings` (`compiler.ts`). Structurally identical to [`R3HostMetadata`].
pub type ParsedHostBindings = R3HostMetadata;

/// `parseHostBindings(host)` — classify each key into listener/property/special-attr/attribute.
/// Returns `Err` if a listener/property/class/style value is not a string.
pub fn parse_host_bindings(host: OrderedMap<String, HostValue>) -> Result<ParsedHostBindings, String> {
    let mut out = ParsedHostBindings::default();

    for (key, value) in host {
        if key.starts_with('(') && key.ends_with(')') {
            let s = expect_string(&value, "Event binding must be string")?;
            out.listeners.insert(key[1..key.len() - 1].to_string(), s);
        } else if key.starts_with('[') && key.ends_with(']') {
            let s = expect_string(&value, "Property binding must be string")?;
            // Synthetic (`@`-prefixed) properties stay in the same map.
            out.properties.insert(key[1..key.len() - 1].to_string(), s);
        } else {
            match key.as_str() {
                "class" => {
                    out.special_attributes.class_attr =
                        Some(expect_string(&value, "Class binding must be string")?);
                }
                "style" => {
                    out.special_attributes.style_attr =
                        Some(expect_string(&value, "Style binding must be string")?);
                }
                _ => match value {
                    HostValue::Str(s) => {
                        out.attributes.insert(key, o::literal(LiteralValue::String(s), None));
                    }
                    HostValue::Expr(e) => {
                        out.attributes.insert(key, e);
                    }
                },
            }
        }
    }

    Ok(out)
}

/// `string | o.Expression` host-binding value.
#[derive(Debug, Clone, PartialEq)]
pub enum HostValue {
    Str(String),
    Expr(Expr),
}

fn expect_string(value: &HostValue, msg: &str) -> Result<String, String> {
    match value {
        HostValue::Str(s) => Ok(s.clone()),
        HostValue::Expr(_) => Err(msg.to_string()),
    }
}

/// `validateNoEventBindings` — reject `on*` bound props/attrs (security). Returns error messages.
pub fn validate_no_event_bindings(bindings: &ParsedHostBindings) -> Vec<String> {
    let mut errors = Vec::new();
    for prop in bindings.properties.keys() {
        let is_attr = prop.starts_with("attr.");
        let bound_name = if is_attr { &prop[5..] } else { prop.as_str() };
        if bound_name.to_lowercase().starts_with("on") {
            let error_type = if is_attr { "attribute" } else { "property" };
            let suggestion = format!("({})=...", &bound_name[2..]);
            let mut msg = format!(
                "Binding to event {error_type} '{bound_name}' is disallowed for security reasons, please use {suggestion}"
            );
            if !is_attr {
                msg.push_str(&format!(
                    "\nIf '{prop}' is a directive input, make sure the directive is imported by the current module."
                ));
            }
            errors.push(msg);
        }
    }
    errors
}

// ---------------------------------------------------------------------------
// compileStyles / encapsulateStyle.
// ---------------------------------------------------------------------------

/// `compileStyles(styles, selector, hostSelector)`.
///
/// Angular runs each style through `new ShadowCss().shimCssText(style, selector, hostSelector)`
/// to scope emulated-encapsulation CSS to the component (`[_ngcontent-%COMP%]` / host attrs). This
/// delegates to the ported [`crate::shadow_css`] rewriter so each selector gains the content
/// attribute and `:host` / `:host-context` rules become host-attribute selectors.
fn compile_styles(styles: &[String], selector: &str, host_selector: &str) -> Vec<String> {
    styles
        .iter()
        .map(|style| crate::shadow_css::shim_css_text(style, selector, host_selector))
        .collect()
}

/// `encapsulateStyle(style, componentIdentifier?)`. Scopes a single style via [`crate::shadow_css`]
/// using the component-id token (`%COMP%`) attributes.
pub fn encapsulate_style(style: &str, _component_identifier: Option<&str>) -> String {
    crate::shadow_css::shim_css_text(style, &content_attr(), &host_attr())
}

// ---------------------------------------------------------------------------
// compileDeferResolverFunction.
// ---------------------------------------------------------------------------

/// `R3DeferPerBlockDependency` (`api.ts`).
#[derive(Debug, Clone, PartialEq)]
pub struct R3DeferPerBlockDependency {
    pub type_reference: Expr,
    pub symbol_name: String,
    pub is_deferrable: bool,
    pub import_path: Option<String>,
    pub is_default_import: bool,
}

/// `R3DeferPerComponentDependency` (`api.ts`).
#[derive(Debug, Clone, PartialEq)]
pub struct R3DeferPerComponentDependency {
    pub symbol_name: String,
    pub import_path: String,
    pub is_default_import: bool,
}

/// `R3DeferResolverFunctionMetadata` (`api.ts` discriminated union).
#[derive(Debug, Clone, PartialEq)]
pub enum R3DeferResolverFunctionMetadata {
    PerBlock { dependencies: Vec<R3DeferPerBlockDependency> },
    PerComponent { dependencies: Vec<R3DeferPerComponentDependency> },
}

/// `compileDeferResolverFunction(meta)` → `() => [ <dep imports…> ]`.
pub fn compile_defer_resolver_function(meta: &R3DeferResolverFunctionMetadata) -> Expr {
    let mut dep_expressions: Vec<Expr> = Vec::new();

    match meta {
        R3DeferResolverFunctionMetadata::PerBlock { dependencies } => {
            for dep in dependencies {
                if dep.is_deferrable {
                    dep_expressions.push(dynamic_import_then(
                        dep.import_path.clone().unwrap_or_default(),
                        if dep.is_default_import { "default" } else { &dep.symbol_name },
                    ));
                } else {
                    // Non-deferrable: bare type reference (preserves the original reference).
                    dep_expressions.push(dep.type_reference.clone());
                }
            }
        }
        R3DeferResolverFunctionMetadata::PerComponent { dependencies } => {
            for dep in dependencies {
                dep_expressions.push(dynamic_import_then(
                    dep.import_path.clone(),
                    if dep.is_default_import { "default" } else { &dep.symbol_name },
                ));
            }
        }
    }

    o::arrow_fn(
        Vec::new(),
        ArrowBody::Expr(Box::new(o::literal_arr(dep_expressions, None))),
        None,
    )
}

/// `import('path').then(m => m.<prop>)` with a leading `@ts-ignore` on the call.
fn dynamic_import_then(import_path: String, prop: &str) -> Expr {
    // `m => m.<prop>`.
    let inner_fn = o::arrow_fn(
        vec![FnParam::new("m", Some(o::dynamic_type()))],
        ArrowBody::Expr(Box::new(o::variable("m", None).prop(prop))),
        None,
    );
    let dynamic_import = Expr::bare(ExprKind::DynamicImport {
        url: o::ImportUrl::Str(import_path),
        url_comment: None,
    });
    let mut call = dynamic_import.prop("then").call_fn(vec![inner_fn], false);
    call.meta.leading_comments.push(ts_ignore_comment());
    call
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::emitter::emit_expression;

    fn ref_(name: &str) -> R3Reference {
        R3Reference {
            value: o::variable(name, None),
            ty: o::variable(name, None),
        }
    }

    fn span() -> ParseSourceSpan {
        ParseSourceSpan::new(0, 0)
    }

    fn directive_meta(name: &str, selector: &str) -> R3DirectiveMetadata {
        R3DirectiveMetadata {
            name: name.to_string(),
            ty: ref_(name),
            type_argument_count: 0,
            type_source_span: span(),
            deps: Deps::None,
            selector: Some(selector.to_string()),
            queries: Vec::new(),
            view_queries: Vec::new(),
            host: R3HostMetadata::default(),
            lifecycle: Lifecycle::default(),
            inputs: OrderedMap::new(),
            outputs: OrderedMap::new(),
            uses_inheritance: false,
            control_create: None,
            export_as: None,
            providers: None,
            is_standalone: true,
            is_signal: false,
            host_directives: None,
            legacy_optional_chaining: false,
        }
    }

    fn component_meta(name: &str, selector: &str) -> R3ComponentMetadata<R3TemplateDependencyMetadata> {
        R3ComponentMetadata {
            base: directive_meta(name, selector),
            template: ComponentTemplate::default(),
            declarations: Vec::new(),
            defer: R3ComponentDeferMetadata::PerComponent { dependencies_fn: None },
            declaration_list_emit_mode: DeclarationListEmitMode::Direct,
            styles: Vec::new(),
            external_styles: None,
            encapsulation: ViewEncapsulation::Emulated,
            animations: None,
            view_providers: None,
            relative_context_file_path: String::new(),
            i18n_use_external_ids: false,
            change_detection: None,
            relative_template_path: None,
            has_directive_dependencies: false,
            raw_imports: None,
            foreign_imports: None,
            imported_directive_names: Vec::new(),
        }
    }

    #[test]
    fn trivial_component_emits_define_component_and_template_fn() {
        // `{ selector: 'app-x', template: '<div>{{v}}</div>' }` — the template body itself comes
        // from the (not-yet-ported) pipeline, so we assert the orchestration + shell.
        let mut meta = component_meta("AppX", "app-x");
        let mut tb = StubTemplateBuilder;
        let mut hb = StubHostBindingsBuilder;
        let mut pool = Vec::new();
        let compiled = compile_component_from_metadata(&mut meta, &mut tb, &mut hb, &mut pool);

        let js = emit_expression(&compiled.expression);
        assert!(js.contains("ɵɵdefineComponent"), "missing defineComponent: {js}");
        assert!(js.contains("AppX_Template"), "missing template fn: {js}");
        // selectors are emitted for a non-empty selector.
        assert!(js.contains("app-x"), "missing selector: {js}");
        // standalone defaults to true → omitted; decls/vars present.
        assert!(js.contains("decls"), "missing decls: {js}");
        assert!(js.contains("vars"), "missing vars: {js}");
    }

    fn view_child_query(property: &str) -> R3QueryMetadata {
        R3QueryMetadata {
            property_name: property.to_string(),
            first: true,
            predicate: QueryPredicate::Expr(o::variable("SomeChild", None)),
            descendants: true,
            emit_distinct_changes_only: true,
            read: None,
            static_: false,
            is_signal: false,
        }
    }

    #[test]
    fn component_with_view_child_query_emits_view_query_fn() {
        // A component with one (legacy) viewChild query must wire a real `viewQuery` function
        // into the definition referencing ɵɵviewQuery + the update-phase ɵɵqueryRefresh.
        let mut meta = component_meta("AppX", "app-x");
        meta.base.view_queries = vec![view_child_query("child")];
        let mut tb = StubTemplateBuilder;
        let mut hb = StubHostBindingsBuilder;
        let mut pool = Vec::new();
        let compiled = compile_component_from_metadata(&mut meta, &mut tb, &mut hb, &mut pool);

        let js = emit_expression(&compiled.expression);
        assert!(js.contains("viewQuery"), "missing viewQuery field: {js}");
        // Create phase references ɵɵviewQuery; update phase references ɵɵqueryRefresh.
        assert!(js.contains("ɵɵviewQuery"), "missing ɵɵviewQuery instruction: {js}");
        assert!(js.contains("ɵɵqueryRefresh"), "missing ɵɵqueryRefresh instruction: {js}");
        assert!(js.contains("ɵɵloadQuery"), "missing ɵɵloadQuery instruction: {js}");
        // The query fn is named after the component.
        assert!(js.contains("AppX_Query"), "missing AppX_Query fn name: {js}");
        // No content queries were declared → contentQueries must be absent.
        assert!(!js.contains("contentQueries"), "contentQueries should be absent: {js}");
    }

    #[test]
    fn component_with_content_query_emits_content_queries_fn() {
        let mut meta = component_meta("AppX", "app-x");
        meta.base.queries = vec![view_child_query("items")];
        let mut tb = StubTemplateBuilder;
        let mut hb = StubHostBindingsBuilder;
        let mut pool = Vec::new();
        let compiled = compile_component_from_metadata(&mut meta, &mut tb, &mut hb, &mut pool);

        let js = emit_expression(&compiled.expression);
        assert!(js.contains("contentQueries"), "missing contentQueries field: {js}");
        assert!(js.contains("ɵɵcontentQuery"), "missing ɵɵcontentQuery instruction: {js}");
        assert!(js.contains("AppX_ContentQueries"), "missing AppX_ContentQueries fn name: {js}");
    }

    #[test]
    fn directive_emits_define_directive() {
        let meta = directive_meta("MyDir", "[myDir]");
        let mut hb = StubHostBindingsBuilder;
        let compiled = compile_directive_from_metadata(&meta, &mut hb);
        let js = emit_expression(&compiled.expression);
        assert!(js.contains("ɵɵdefineDirective"), "missing defineDirective: {js}");
        assert!(js.contains("myDir"), "missing selector token: {js}");
    }

    #[test]
    fn non_standalone_emits_standalone_false() {
        let mut meta = directive_meta("D", "d");
        meta.is_standalone = false;
        let mut hb = StubHostBindingsBuilder;
        let compiled = compile_directive_from_metadata(&meta, &mut hb);
        let js = emit_expression(&compiled.expression);
        assert!(js.contains("standalone"), "standalone:false should be emitted: {js}");
    }

    #[test]
    fn onpush_emits_change_detection_zero_default_is_omitted() {
        // Angular v21 emits the numeric strategy only when it differs from
        // ChangeDetectionStrategy.Default. OnPush (= 0) → `changeDetection: 0`.
        let mut meta = component_meta("C", "c");
        meta.change_detection = Some(ChangeDetection::Strategy(ChangeDetectionStrategy::OnPush));
        let mut tb = StubTemplateBuilder;
        let mut hb = StubHostBindingsBuilder;
        let mut pool = Vec::new();
        let compiled = compile_component_from_metadata(&mut meta, &mut tb, &mut hb, &mut pool);
        let js = emit_expression(&compiled.expression);
        assert!(js.contains("changeDetection"), "OnPush should emit changeDetection: {js}");
        assert!(
            js.contains("changeDetection: 0") || js.contains("changeDetection:0"),
            "OnPush should emit changeDetection: 0: {js}"
        );

        // Default (1) is the implicit runtime default → omitted.
        let mut meta = component_meta("C", "c");
        meta.change_detection = Some(ChangeDetection::Strategy(ChangeDetectionStrategy::Default));
        let compiled = compile_component_from_metadata(&mut meta, &mut tb, &mut hb, &mut pool);
        assert!(
            !emit_expression(&compiled.expression).contains("changeDetection"),
            "Default should omit changeDetection"
        );
    }

    #[test]
    fn no_styles_emulated_downgrades_to_none_and_omits_encapsulation() {
        // Emulated + no styles → downgraded to None, and None != Emulated so encapsulation IS set.
        let mut meta = component_meta("C", "c");
        let mut tb = StubTemplateBuilder;
        let mut hb = StubHostBindingsBuilder;
        let mut pool = Vec::new();
        let compiled = compile_component_from_metadata(&mut meta, &mut tb, &mut hb, &mut pool);
        assert_eq!(meta.encapsulation, ViewEncapsulation::None);
        assert!(emit_expression(&compiled.expression).contains("encapsulation"));
    }

    #[test]
    fn styles_keep_emulated_and_omit_encapsulation() {
        let mut meta = component_meta("C", "c");
        meta.styles = vec![".a{color:red}".to_string()];
        let mut tb = StubTemplateBuilder;
        let mut hb = StubHostBindingsBuilder;
        let mut pool = Vec::new();
        let compiled = compile_component_from_metadata(&mut meta, &mut tb, &mut hb, &mut pool);
        // Stays Emulated (has styles) → encapsulation omitted, styles present.
        assert_eq!(meta.encapsulation, ViewEncapsulation::Emulated);
        let js = emit_expression(&compiled.expression);
        assert!(js.contains("styles"), "styles missing: {js}");
        assert!(!js.contains("encapsulation"), "encapsulation should be omitted: {js}");
        // Emulated encapsulation scopes the raw `.a{...}` to the component via the content attr.
        assert!(
            js.contains(".a[_ngcontent-%COMP%]"),
            "style not scoped via ShadowCss: {js}"
        );
    }

    #[test]
    fn emulated_host_style_scopes_to_host_attr() {
        let mut meta = component_meta("C", "c");
        meta.styles = vec![":host{display:block}".to_string()];
        let mut tb = StubTemplateBuilder;
        let mut hb = StubHostBindingsBuilder;
        let mut pool = Vec::new();
        let compiled = compile_component_from_metadata(&mut meta, &mut tb, &mut hb, &mut pool);
        let js = emit_expression(&compiled.expression);
        assert!(
            js.contains("[_nghost-%COMP%]"),
            ":host not rewritten to host attr: {js}"
        );
    }

    #[test]
    fn defer_per_component_pushes_defer_fn_const() {
        let mut meta = component_meta("C", "c");
        meta.defer = R3ComponentDeferMetadata::PerComponent {
            dependencies_fn: Some(o::arrow_fn(
                Vec::new(),
                ArrowBody::Expr(Box::new(o::literal_arr(Vec::new(), None))),
                None,
            )),
        };
        let mut tb = StubTemplateBuilder;
        let mut hb = StubHostBindingsBuilder;
        let mut pool = Vec::new();
        let _ = compile_component_from_metadata(&mut meta, &mut tb, &mut hb, &mut pool);
        assert_eq!(pool.len(), 1);
        match &pool[0].kind {
            StmtKind::DeclareVar { name, .. } => assert_eq!(name, "C_DeferFn"),
            other => panic!("expected DeclareVar, got {other:?}"),
        }
        assert!(pool[0].meta.modifiers.has_modifier(StmtModifier::FINAL));
    }

    #[test]
    fn features_order_host_directives_before_inheritance() {
        let mut meta = directive_meta("D", "d");
        meta.uses_inheritance = true;
        meta.host_directives = Some(vec![R3HostDirectiveMetadata {
            directive: ref_("HostDir"),
            is_forward_reference: false,
            inputs: None,
            outputs: None,
        }]);
        let mut hb = StubHostBindingsBuilder;
        let compiled = compile_directive_from_metadata(&meta, &mut hb);
        let js = emit_expression(&compiled.expression);
        let hd = js.find("HostDirectivesFeature").expect("host directives feature");
        let inh = js.find("InheritDefinitionFeature").expect("inherit feature");
        assert!(hd < inh, "HostDirectives must precede InheritDefinition: {js}");
    }

    #[test]
    fn inputs_literal_tracks_alias_and_flags() {
        let mut meta = directive_meta("D", "d");
        meta.inputs.insert(
            "field".to_string(),
            R3InputMetadata {
                class_property_name: "field".to_string(),
                binding_property_name: "alias".to_string(),
                required: false,
                is_signal: true,
                transform_function: None,
            },
        );
        let mut hb = StubHostBindingsBuilder;
        let compiled = compile_directive_from_metadata(&meta, &mut hb);
        let js = emit_expression(&compiled.expression);
        // Different declaring name + signal flag → array form `[flags, 'alias', 'field']`.
        assert!(js.contains("inputs"), "inputs missing: {js}");
        assert!(js.contains("alias"), "public name missing: {js}");
        assert!(js.contains("field"), "declared name missing: {js}");
    }

    #[test]
    fn declaration_list_closure_wraps_in_arrow() {
        let mut meta = component_meta("C", "c");
        meta.declaration_list_emit_mode = DeclarationListEmitMode::Closure;
        meta.declarations = vec![R3TemplateDependencyMetadata {
            kind: R3TemplateDependencyKind::Directive,
            ty: o::variable("Dep", None),
        }];
        let mut tb = StubTemplateBuilder;
        let mut hb = StubHostBindingsBuilder;
        let mut pool = Vec::new();
        let compiled = compile_component_from_metadata(&mut meta, &mut tb, &mut hb, &mut pool);
        let js = emit_expression(&compiled.expression);
        assert!(js.contains("dependencies"), "dependencies missing: {js}");
        assert!(js.contains("Dep"), "Dep missing: {js}");
    }

    #[test]
    fn defer_resolver_per_component_emits_dynamic_import() {
        let meta = R3DeferResolverFunctionMetadata::PerComponent {
            dependencies: vec![R3DeferPerComponentDependency {
                symbol_name: "MyCmp".to_string(),
                import_path: "./a".to_string(),
                is_default_import: false,
            }],
        };
        let expr = compile_defer_resolver_function(&meta);
        let js = emit_expression(&expr);
        assert!(js.contains("import("), "dynamic import missing: {js}");
        assert!(js.contains("MyCmp"), "symbol missing: {js}");
    }

    #[test]
    fn parse_host_bindings_classifies_keys() {
        let mut host = OrderedMap::new();
        host.insert("(click)".to_string(), HostValue::Str("onClick()".to_string()));
        host.insert("[id]".to_string(), HostValue::Str("myId".to_string()));
        host.insert("class".to_string(), HostValue::Str("foo".to_string()));
        host.insert("role".to_string(), HostValue::Str("button".to_string()));
        let parsed = parse_host_bindings(host).unwrap();
        assert_eq!(parsed.listeners.get("click").map(String::as_str), Some("onClick()"));
        assert_eq!(parsed.properties.get("id").map(String::as_str), Some("myId"));
        assert_eq!(parsed.special_attributes.class_attr.as_deref(), Some("foo"));
        assert!(parsed.attributes.contains_key("role"));
    }

    #[test]
    fn validate_no_event_bindings_rejects_on_prefixed() {
        let mut parsed = ParsedHostBindings::default();
        parsed.properties.insert("onclick".to_string(), "x".to_string());
        let errors = validate_no_event_bindings(&parsed);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("disallowed for security reasons"));
    }

    // ---------------------------------------------------------------------------
    // Host-bindings generation (DefaultHostBindingsBuilder).
    // ---------------------------------------------------------------------------

    #[test]
    fn host_property_binding_emits_dom_property_and_host_vars() {
        // `host: { '[title]': 't' }` → UPDATE block with `ɵɵdomProperty('title', ctx.t)`
        // and `hostVars: 1`.
        let mut meta = directive_meta("D", "[d]");
        meta.host.properties.insert("title".to_string(), "t".to_string());
        let mut hb = DefaultHostBindingsBuilder;
        let compiled = compile_directive_from_metadata(&meta, &mut hb);
        let js = emit_expression(&compiled.expression);
        assert!(js.contains("hostBindings"), "missing hostBindings fn: {js}");
        assert!(js.contains("ɵɵdomProperty"), "missing ɵɵdomProperty: {js}");
        assert!(js.contains("title"), "missing property name: {js}");
        // Lowered against ctx.
        assert!(js.contains("ctx.t"), "property value should be lowered to ctx.t: {js}");
        // hostVars >= 1 (one property binding).
        assert!(
            js.contains("hostVars: 1") || js.contains("hostVars:1"),
            "expected hostVars: 1: {js}"
        );
    }

    #[test]
    fn host_property_array_literal_extracts_pure_function() {
        // `host: { '[id]': '["red", id]' }` — a literal array host-binding value runs Angular's
        // host-bindings `generatePureLiteralStructures`: the array is extracted into a hoisted
        // factory const and the binding becomes `ɵɵdomProperty("id", ɵɵpureFunction1(1, $c0$, ctx.id))`
        // with `hostVars: 3` (1 regular binding + 1+1 pure-function slots).
        let mut meta = directive_meta("D", "[d]");
        meta.host.properties.insert("id".to_string(), "[\"red\", id]".to_string());
        let mut hb = DefaultHostBindingsBuilder;
        let compiled = compile_directive_from_metadata(&meta, &mut hb);
        let js = emit_expression(&compiled.expression);

        assert!(js.contains("ɵɵpureFunction1(1, $c0$, ctx.id)"), "missing pureFunction call: {js}");
        assert!(
            js.contains("hostVars: 3") || js.contains("hostVars:3"),
            "expected hostVars: 3: {js}"
        );
        // The literal must NOT be emitted inline anymore.
        assert!(!js.contains("ɵɵdomProperty(\"id\", [\"red\""), "literal not extracted: {js}");
        // The factory const is hoisted as a sibling statement (ConstantPool.statements).
        let stmts = crate::output::emitter::emit_statements(&compiled.statements);
        assert!(stmts.contains("$c0$"), "factory const not hoisted: {stmts}");
        assert!(stmts.contains("[\"red\", a0]"), "factory body wrong: {stmts}");
    }

    #[test]
    fn host_listener_emits_listener_instruction_in_create() {
        // `host: { '(click)': 'f()' }` → CREATE block with `ɵɵlistener('click', fn)`.
        let mut meta = directive_meta("D", "[d]");
        meta.host.listeners.insert("click".to_string(), "f()".to_string());
        let mut hb = DefaultHostBindingsBuilder;
        let compiled = compile_directive_from_metadata(&meta, &mut hb);
        let js = emit_expression(&compiled.expression);
        assert!(js.contains("hostBindings"), "missing hostBindings fn: {js}");
        assert!(js.contains("ɵɵlistener"), "missing ɵɵlistener: {js}");
        assert!(js.contains("click"), "missing event name: {js}");
        // Handler invokes ctx.f().
        assert!(js.contains("ctx.f()"), "handler should call ctx.f(): {js}");
        // A listener alone has no host vars.
        assert!(!js.contains("hostVars"), "listener-only should not emit hostVars: {js}");
    }

    #[test]
    fn host_static_class_attr_emits_host_attrs() {
        // `host: { 'class': 'foo bar' }` → `hostAttrs: [1 /*Classes*/, 'foo', 'bar']`, no
        // hostBindings fn. The class value is whitespace-split under the Classes (1) marker.
        let mut meta = directive_meta("D", "[d]");
        meta.host.special_attributes.class_attr = Some("foo bar".to_string());
        let mut hb = DefaultHostBindingsBuilder;
        let compiled = compile_directive_from_metadata(&meta, &mut hb);
        let js = emit_expression(&compiled.expression);
        assert!(js.contains("hostAttrs"), "missing hostAttrs: {js}");
        // Classes marker (1) followed by the individual class names — the literal "class" key is
        // not emitted (it becomes the marker).
        assert!(js.contains("\"foo\"") || js.contains("'foo'"), "missing class name foo: {js}");
        assert!(js.contains("\"bar\"") || js.contains("'bar'"), "missing class name bar: {js}");
        assert!(!js.contains("\"class\"") && !js.contains("'class'"), "class key should be a marker: {js}");
        // No dynamic bindings → no hostBindings fn.
        assert!(!js.contains("hostBindings"), "static-only should not emit hostBindings: {js}");
    }

    #[test]
    fn host_static_style_attr_splits_into_styles_marker() {
        // `host: { 'style': 'width: 100px; height: 200px' }` → `hostAttrs: [2 /*Styles*/,
        // 'width', '100px', 'height', '200px']`.
        let mut meta = directive_meta("D", "[d]");
        meta.host.special_attributes.style_attr =
            Some("width: 100px; height: 200px".to_string());
        let mut hb = DefaultHostBindingsBuilder;
        let compiled = compile_directive_from_metadata(&meta, &mut hb);
        let js = emit_expression(&compiled.expression);
        assert!(js.contains("hostAttrs"), "missing hostAttrs: {js}");
        assert!(js.contains("width"), "missing style prop width: {js}");
        assert!(js.contains("100px"), "missing style value 100px: {js}");
        assert!(js.contains("height"), "missing style prop height: {js}");
        assert!(js.contains("200px"), "missing style value 200px: {js}");
    }

    #[test]
    fn host_attrs_plain_attrs_precede_class_and_style_groups() {
        // Plain attrs come first, then the Classes (1) group, then the Styles (2) group.
        let mut meta = directive_meta("D", "[d]");
        meta.host
            .attributes
            .insert("role".to_string(), o::literal(LiteralValue::String("button".to_string()), None));
        meta.host.special_attributes.class_attr = Some("a".to_string());
        meta.host.special_attributes.style_attr = Some("color: red".to_string());
        let arr = host_attrs_array(&{
            // Replicate the fold the builder performs before calling host_attrs_array.
            let mut attrs = meta.host.attributes.clone();
            attrs.insert("style".to_string(), o::literal(LiteralValue::String("color: red".to_string()), None));
            attrs.insert("class".to_string(), o::literal(LiteralValue::String("a".to_string()), None));
            attrs
        })
        .expect("host attrs array");
        let entries = match &arr.kind {
            ExprKind::LiteralArray(e) => e,
            other => panic!("expected literal array, got {other:?}"),
        };
        // role, "button", 1, "a", 2, "color", "red".
        let nums: Vec<Option<f64>> = entries
            .iter()
            .map(|e| match &e.kind {
                ExprKind::Literal(LiteralValue::Number(n)) => Some(*n),
                _ => None,
            })
            .collect();
        let classes_marker = nums.iter().position(|n| *n == Some(1.0)).expect("classes marker");
        let styles_marker = nums.iter().position(|n| *n == Some(2.0)).expect("styles marker");
        // Plain `role`/`button` pair occupies indices 0,1 → markers come after.
        assert!(classes_marker >= 2, "plain attrs must precede classes marker");
        assert!(classes_marker < styles_marker, "classes marker must precede styles marker");
    }

    #[test]
    fn host_attr_class_style_route_to_specialized_instructions() {
        // `[attr.role]`, `[class.active]`, `[style.width]` route to ɵɵattribute / ɵɵclassProp /
        // ɵɵstyleProp respectively. Host-var accounting (Angular `bindingCount`): a regular binding
        // (attribute) reserves ONE host var; a `class.X`/`style.X` styling binding reserves TWO. So
        // 1 + 2 + 2 = 5 (this is why the `host_class_binding_special_chars` golden — 3 class bindings
        // — reports `hostVars: 6`).
        let mut meta = directive_meta("D", "[d]");
        meta.host.properties.insert("attr.role".to_string(), "r".to_string());
        meta.host.properties.insert("class.active".to_string(), "isActive".to_string());
        meta.host.properties.insert("style.width".to_string(), "w".to_string());
        let mut hb = DefaultHostBindingsBuilder;
        let compiled = compile_directive_from_metadata(&meta, &mut hb);
        let js = emit_expression(&compiled.expression);
        assert!(js.contains("ɵɵattribute"), "missing ɵɵattribute: {js}");
        assert!(js.contains("ɵɵclassProp"), "missing ɵɵclassProp: {js}");
        assert!(js.contains("ɵɵstyleProp"), "missing ɵɵstyleProp: {js}");
        assert!(
            js.contains("hostVars: 5") || js.contains("hostVars:5"),
            "expected hostVars: 5 (attr=1 + class=2 + style=2): {js}"
        );
        // The styling flush groups styleProp before classProp regardless of source order.
        let style_at = js.find("ɵɵstyleProp").expect("styleProp");
        let class_at = js.find("ɵɵclassProp").expect("classProp");
        assert!(style_at < class_at, "styleProp must precede classProp in the flush: {js}");
    }

    #[test]
    fn host_animate_enter_string_emits_animate_enter_in_create_not_host_attrs() {
        // `host: { 'animate.enter': 'fade' }` → CREATE-block `ɵɵanimateEnter("fade")`, NOT a static
        // `hostAttrs: ["animate.enter", "fade"]` entry (mirrors
        // r3_view_compiler/animations/animate_enter_with_string_host_bindings).
        let mut meta = directive_meta("ChildComponent", "child-component");
        meta.host.attributes.insert(
            "animate.enter".to_string(),
            o::literal(LiteralValue::String("fade".to_string()), None),
        );
        let mut hb = DefaultHostBindingsBuilder;
        let compiled = compile_directive_from_metadata(&meta, &mut hb);
        let js = emit_expression(&compiled.expression);
        assert!(js.contains("hostBindings"), "missing hostBindings fn: {js}");
        assert!(
            js.contains("ɵɵanimateEnter(\"fade\")") || js.contains("ɵɵanimateEnter('fade')"),
            "missing ɵɵanimateEnter(\"fade\"): {js}"
        );
        // It must NOT be lowered to a static hostAttrs entry.
        assert!(!js.contains("hostAttrs"), "animate.enter must not become hostAttrs: {js}");
        assert!(!js.contains("animate.enter"), "raw key must not appear: {js}");
        // String animate is not a binding → no host vars.
        assert!(!js.contains("hostVars"), "animate string should not emit hostVars: {js}");
    }

    #[test]
    fn host_animate_leave_string_emits_animate_leave_in_create() {
        // `host: { 'animate.leave': 'fade' }` → CREATE-block `ɵɵanimateLeave("fade")`.
        let mut meta = directive_meta("ChildComponent", "child-component");
        meta.host.attributes.insert(
            "animate.leave".to_string(),
            o::literal(LiteralValue::String("fade".to_string()), None),
        );
        let mut hb = DefaultHostBindingsBuilder;
        let compiled = compile_directive_from_metadata(&meta, &mut hb);
        let js = emit_expression(&compiled.expression);
        assert!(
            js.contains("ɵɵanimateLeave(\"fade\")") || js.contains("ɵɵanimateLeave('fade')"),
            "missing ɵɵanimateLeave(\"fade\"): {js}"
        );
        assert!(!js.contains("hostAttrs"), "animate.leave must not become hostAttrs: {js}");
    }

    #[test]
    fn host_animate_enter_event_emits_animate_enter_listener() {
        // `host: { '(animate.enter)': 'fadeFn($event)' }` → CREATE-block
        // `ɵɵanimateEnterListener(function ChildComponent_animateenter_HostBindingHandler($event) {
        //   return ctx.fadeFn($event); })` — only the handler fn, no event-name argument; the
        // handler name sanitizes the `.` away, and `$event` stays a bare parameter (mirrors
        // animate_enter_with_event_host_bindings).
        let mut meta = directive_meta("ChildComponent", "child-component");
        meta.host
            .listeners
            .insert("animate.enter".to_string(), "fadeFn($event)".to_string());
        let mut hb = DefaultHostBindingsBuilder;
        let compiled = compile_directive_from_metadata(&meta, &mut hb);
        let js = emit_expression(&compiled.expression);
        assert!(js.contains("ɵɵanimateEnterListener"), "missing ɵɵanimateEnterListener: {js}");
        assert!(
            js.contains("ChildComponent_animateenter_HostBindingHandler"),
            "missing sanitized handler name: {js}"
        );
        // `$event` must stay a bare parameter (resolveDollarEvent), not become ctx.$event.
        assert!(js.contains("ctx.fadeFn($event)"), "handler should call ctx.fadeFn($event): {js}");
        assert!(!js.contains("ctx.$event"), "$event must not resolve against ctx: {js}");
        // The animate listener takes ONLY the handler — no event-name argument and no ɵɵlistener.
        assert!(!js.contains("ɵɵlistener("), "must not use ɵɵlistener: {js}");
        assert!(
            !js.contains("\"animate.enter\"") && !js.contains("'animate.enter'"),
            "must not pass an event-name argument: {js}"
        );
    }

    #[test]
    fn host_animate_leave_event_emits_animate_leave_listener() {
        // `host: { '(animate.leave)': 'fadeFn($event)' }` → `ɵɵanimateLeaveListener(...)`.
        let mut meta = directive_meta("ChildComponent", "child-component");
        meta.host
            .listeners
            .insert("animate.leave".to_string(), "fadeFn($event)".to_string());
        let mut hb = DefaultHostBindingsBuilder;
        let compiled = compile_directive_from_metadata(&meta, &mut hb);
        let js = emit_expression(&compiled.expression);
        assert!(js.contains("ɵɵanimateLeaveListener"), "missing ɵɵanimateLeaveListener: {js}");
        assert!(
            js.contains("ChildComponent_animateleave_HostBindingHandler"),
            "missing sanitized handler name: {js}"
        );
        assert!(!js.contains("ɵɵlistener("), "must not use ɵɵlistener: {js}");
    }
}
