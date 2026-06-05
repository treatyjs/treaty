//! The **dev-only class-metadata emitter** (Rust) for PARTIAL mode.
//!
//! ng-packagr publishes, alongside each class's `ɵɵngDeclare*` definition, a companion
//! `i0.ɵɵngDeclareClassMetadata({ … })` STATEMENT carrying the ORIGINAL decorator(s), the
//! constructor-parameter decorators, and the property decorators. It is the partial form of the AOT
//! compiler's `ngDevMode && ɵsetClassMetadata(...)` side effect: a dev/HMR reflection aid, fully
//! tree-shaken in production (and DROPPED to `void 0` by the linker — see
//! [`crate::linker`] `DeclareKind::ClassMetadata`). Treaty's classic Full emit omits it (it never
//! emits the `ngDevMode`-guarded `setClassMetadata`), so this is emitted ONLY in partial mode, for
//! byte-parity with a real ng-packagr partial build.
//!
//! This module reconstructs the declaration from the engine-neutral [`ClassWithDecorators`] + the
//! original source text, carrying every decorator argument VERBATIM (a source-span slice — the
//! neutral [`DecoratorInfo::arguments`] / [`NArg::span`] surface) so opaque expressions (`forwardRef(()
//! => X)`, `dynamicAttrName()`, an `InjectionToken`) round-trip exactly. Because the result links back
//! to `void 0`, the round-trip is inert; this exists purely to match the published bytes.
//!
//! NOTE: this module names ZERO live `oxc_` types — it reads the engine-neutral parse IR (the same
//! surface the swc backend fills byte-identically), with verbatim slices recovered from the neutral
//! decorator-argument byte spans against the original `source`.

use crate::parse::{ClassWithDecorators, DecoratorInfo, MemberKind, NArg, NCtorParam};

/// The Angular version a class-metadata declaration is stamped with — the in-repo placeholder the
/// linker treats as "newest behaviour". Mirrors the rest of the partial family.
const PARTIAL_VERSION: &str = "0.0.0-PLACEHOLDER";
/// The `minVersion` a `ɵɵngDeclareClassMetadata` records (`"12.0.0"`, matching ng-packagr).
const MIN_VERSION: &str = "12.0.0";

/// The decorator names this compiler models as Angular CLASS decorators (the ones that drive an Ivy
/// definition). Their ORIGINAL form is reproduced in the metadata `decorators` array.
const CLASS_DECORATORS: &[&str] = &["Component", "Directive", "Pipe", "NgModule", "Injectable"];

/// The Angular MEMBER decorators reproduced in `propDecorators` (the property-level reflection set).
const MEMBER_DECORATORS: &[&str] = &[
    "Input",
    "Output",
    "HostBinding",
    "HostListener",
    "ViewChild",
    "ViewChildren",
    "ContentChild",
    "ContentChildren",
];

/// The Angular constructor-PARAMETER decorators reproduced in each `ctorParameters` entry.
const PARAM_DECORATORS: &[&str] = &[
    "Inject", "Optional", "Self", "SkipSelf", "Host", "Attribute",
];

/// Emit the `i0.ɵɵngDeclareClassMetadata({ … });` STATEMENT for `class` (named `class_name`), reading
/// decorator args / constructor parameters verbatim from `source`. Returns `None` when the class
/// carries no recognised Angular class decorator (nothing to reflect).
///
/// Field order matches ng-packagr's `compileClassMetadata`:
///   `minVersion, version, ngImport: i0, type, decorators[, ctorParameters][, propDecorators]`.
pub fn emit_class_metadata(
    class: &ClassWithDecorators,
    class_name: &str,
    source: &str,
) -> Option<String> {
    let decorators = class_decorator_entries(class, source);
    if decorators.is_empty() {
        return None;
    }

    let mut fields = format!(
        "minVersion: \"{MIN_VERSION}\", version: \"{PARTIAL_VERSION}\", ngImport: i0, type: {class_name}, decorators: [{}]",
        decorators.join(", ")
    );

    // ctorParameters: `() => [ <entry>, … ]` — emitted only when a constructor with parameters
    // exists (matching ng-packagr; a parameterless class omits it, an explicit empty `constructor(){}`
    // emits `() => []`).
    if let Some(ctor_params) = constructor_param_entries(class, source) {
        fields.push_str(&format!(
            ", ctorParameters: () => [{}]",
            ctor_params.join(", ")
        ));
    }

    // propDecorators: `{ prop: [ <decorator>, … ], … }` — the member-level reflection map.
    if let Some(prop_decorators) = prop_decorator_entries(class, source) {
        fields.push_str(&format!(", propDecorators: {{ {prop_decorators} }}"));
    }

    Some(format!("i0.\u{0275}\u{0275}ngDeclareClassMetadata({{ {fields} }});"))
}

/// The `decorators` array entries: each recognised Angular CLASS decorator reproduced as
/// `{ type: <Name> }` (bare `@Foo`) or `{ type: <Name>, args: [<verbatim arg src>, …] }` (`@Foo(...)`).
fn class_decorator_entries(class: &ClassWithDecorators, source: &str) -> Vec<String> {
    class
        .decorators
        .iter()
        .filter_map(|dec| {
            let name = decorator_callee_name(dec)?;
            if !CLASS_DECORATORS.contains(&name) {
                return None;
            }
            Some(decorator_entry(dec, name, source))
        })
        .collect()
}

/// One decorator → `{ type: <Name> }` or `{ type: <Name>, args: [<arg src>, …] }`. The args are the
/// VERBATIM source slices of the call arguments (so an object-literal decorator arg, a `forwardRef`,
/// etc. round-trip exactly).
fn decorator_entry(dec: &DecoratorInfo, name: &str, source: &str) -> String {
    let args = decorator_arg_sources(dec, source);
    if args.is_empty() {
        format!("{{ type: {name} }}")
    } else {
        format!("{{ type: {name}, args: [{}] }}", args.join(", "))
    }
}

/// The verbatim source slices of a decorator call's arguments (`@Foo(a, b)` → `["a src", "b src"]`).
/// A bare `@Foo` (or `@Foo()`) yields an empty vec. Each argument's byte span is the neutral
/// [`NArg::span`] (the span of the argument EXPRESSION), recovered verbatim from `source`.
fn decorator_arg_sources(dec: &DecoratorInfo, source: &str) -> Vec<String> {
    dec.arguments
        .iter()
        // A `...spread` decorator argument had no `arg.as_expression()` and was dropped by the
        // historical `filter_map`; keep only plain (non-spread) argument expressions.
        .filter_map(|arg| match arg {
            NArg::Expr(_, _) => Some(arg_source(arg, source)),
            NArg::Spread(_, _) => None,
        })
        .collect()
}

/// The constructor-parameter entries (`ctorParameters: () => [ … ]`), or `None` when the class has
/// no constructor with parameters. Each entry is `{ type: <Type or undefined>[, decorators: [ … ]] }`:
///   * `type`: the parameter's declared type identifier, or `undefined` when it has none / cannot be
///     named (an `@Inject`/`@Attribute`-only param) — matching ng-packagr;
///   * `decorators`: the param's Angular parameter decorators (`@Inject(TOKEN)`, `@Optional()`, …),
///     each `{ type: <Name>[, args: [<arg src>] ] }`. A param with a custom (non-Angular) decorator
///     still emits a (possibly empty) `decorators: []` — ng-packagr records the slot.
fn constructor_param_entries(class: &ClassWithDecorators, source: &str) -> Option<Vec<String>> {
    let ctor = class
        .members
        .iter()
        .find(|m| m.kind == MemberKind::Constructor)?;
    // Only the IMPLEMENTATION signature (the one with a body) carries parameters; bodiless overloads
    // have none, and the parse backend surfaces only the implementation's params. ngtsc reflects the
    // implementation params.
    if ctor.params.is_empty() {
        // An explicit parameterless `constructor() {}` → `() => []`; ng-packagr emits the empty array.
        return Some(Vec::new());
    }
    Some(
        ctor.params
            .iter()
            .map(|p| ctor_param_entry(p, source))
            .collect(),
    )
}

/// One constructor parameter → its `ctorParameters` entry.
fn ctor_param_entry(param: &NCtorParam, source: &str) -> String {
    let type_src = param_type_name(param).unwrap_or_else(|| "undefined".to_string());
    let mut entry = format!("{{ type: {type_src}");

    // The param's Angular decorators (and a slot for any decorator at all — ng-packagr records
    // `decorators: []` for a custom-only param). We reproduce the Angular ones; a param with any
    // decorator gets the `decorators` key.
    let decorators: Vec<String> = param
        .decorators
        .iter()
        .filter_map(|dec| {
            let name = decorator_callee_name(dec)?;
            if !PARAM_DECORATORS.contains(&name) {
                return None;
            }
            Some(decorator_entry(dec, name, source))
        })
        .collect();
    if !param.decorators.is_empty() {
        entry.push_str(&format!(", decorators: [{}]", decorators.join(", ")));
    }
    entry.push_str(" }");
    entry
}

/// The `propDecorators` map source (`prop: [ {type: Input}, … ], …`), or `None` when no member
/// carries a recognised Angular member decorator.
fn prop_decorator_entries(class: &ClassWithDecorators, source: &str) -> Option<String> {
    let mut entries: Vec<String> = Vec::new();
    for member in &class.members {
        // `propDecorators` reflects property/accessor members and non-constructor methods (the
        // surface the original `PropertyDefinition` / non-ctor `MethodDefinition` walk covered).
        let eligible = match member.kind {
            MemberKind::Property | MemberKind::Accessor | MemberKind::Getter | MemberKind::Setter => {
                true
            }
            MemberKind::Method => true,
            MemberKind::Constructor | MemberKind::Other => false,
        };
        if !eligible {
            continue;
        }
        let Some(name) = member.name.as_deref() else {
            continue;
        };
        let decs: Vec<String> = member
            .decorators
            .iter()
            .filter_map(|dec| {
                let dname = decorator_callee_name(dec)?;
                if !MEMBER_DECORATORS.contains(&dname) {
                    return None;
                }
                Some(decorator_entry(dec, dname, source))
            })
            .collect();
        if decs.is_empty() {
            continue;
        }
        let key = if is_safe_object_key(name) {
            name.to_string()
        } else {
            format!("\"{name}\"")
        };
        entries.push(format!("{key}: [{}]", decs.join(", ")));
    }
    if entries.is_empty() {
        None
    } else {
        Some(entries.join(", "))
    }
}

/// The callee identifier of a decorator (`@Foo` / `@Foo(...)` → `"Foo"`), or `None` for a
/// member-access / computed decorator we do not model (the neutral [`DecoratorInfo::name`] is empty).
fn decorator_callee_name(dec: &DecoratorInfo) -> Option<&str> {
    if dec.name.is_empty() {
        None
    } else {
        Some(dec.name.as_str())
    }
}

/// The declared TYPE identifier of a constructor parameter (`dep: Foo` → `"Foo"`; `ns.Foo` →
/// `"ns.Foo"`), or `None` for a non-reference / primitive / absent type. Reads the neutral
/// [`NCtorParam::type_ref`] dotted name path (already `None` for a non-reference / `this`-type).
fn param_type_name(param: &NCtorParam) -> Option<String> {
    let type_ref = param.type_ref.as_ref()?;
    if type_ref.name_path.is_empty() {
        return None;
    }
    Some(type_ref.name_path.join("."))
}

/// The trimmed source slice of a decorator-call argument's byte span (carried verbatim). The neutral
/// [`NArg::span`] is the argument EXPRESSION span (absolute byte offsets), so `source[span]` is the
/// same verbatim slice the live-AST `arg.as_expression().span()` recovered.
fn arg_source(arg: &NArg, source: &str) -> String {
    let span = arg.span();
    source[span.start as usize..span.end as usize]
        .trim()
        .to_string()
}

/// Whether `key` is a valid bare JS identifier (so a `propDecorators` key prints unquoted).
fn is_safe_object_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$' => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        }
        _ => false,
    }
}
