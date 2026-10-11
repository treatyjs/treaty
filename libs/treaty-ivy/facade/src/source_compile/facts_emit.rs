//! The decorator-FACTS path for `@Component` / `@Directive` (the opt-in `ng-analyze` feature).
//!
//! With the feature on, the metadata of each component and directive is taken from the bridge's facts
//! ([`treaty_ivy_ng_bridge`], Angular's own ng-analyze) and converted into the same
//! [`DirectiveExtract`] that Treaty's extraction produces. The emit tail is shared, so only the source of
//! the metadata changes. Anything the bridge cannot give (a class ng-analyze refused, a file-level
//! diagnostic, a shape the conversion cannot lower) FALLS BACK to Treaty's extraction for that class, and
//! every fallback is counted by reason so it can be measured ([`ng_analyze_counters`]).
//!
//! Conventions applied here, on top of the RAW facts (see the bridge's `facts` module):
//!   * the filename-derived default selector for a selectorless `@Component`;
//!   * `signals` = the explicit option only (ngtsc's `isSignal`);
//!   * `changeDetection`: Treaty's OnPush-only convention (`change_detection_strategy`, shared with the
//!     source front-end). ng-analyze reports an absent option and an explicit `OnPush` alike as `None`, so the
//!     decorator object is read for the explicit member;
//!   * `modernize.signal_inputs` lowers decorator inputs to signal inputs;
//!   * `forwardRef` in `imports:` closure-wraps the dependencies, read from the decorator object.

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::parse::NArrayElement;
use treaty_ivy_facts::{
    DecoratorFacts, DepFact, DirectiveFacts, HostValue as FactHostValue,
    HostDirectiveFact, QueryFact, QueryPredicateFact,
};

use super::*;

/// The bridge's facts for ONE source, as the compile entry sees them.
pub(super) enum NgFacts<'a> {
    /// The feature is off, or no bridge was consulted: Treaty's extraction for every class.
    Off,
    /// ng-analyze could not analyse the file (its reason). Every component/directive falls back.
    FileError(&'a str),
    /// The bridge's facts for every decorated class of the file.
    Classes(&'a [DecoratorFacts]),
}

/// Counters for the facts path, read by tests and by measurement tooling.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NgAnalyzeCounters {
    /// Component/directive classes whose metadata came from the facts.
    pub facts_used: usize,
    /// Component/directive classes that fell back to Treaty's extraction, by reason.
    pub fallbacks: BTreeMap<String, usize>,
}

static COUNTERS: Mutex<NgAnalyzeCounters> = Mutex::new(NgAnalyzeCounters {
    facts_used: 0,
    fallbacks: BTreeMap::new(),
});

fn with_counters(f: impl FnOnce(&mut NgAnalyzeCounters)) {
    if let Ok(mut c) = COUNTERS.lock() {
        f(&mut c);
    }
}

/// A snapshot of the facts-path counters (process-wide).
pub fn ng_analyze_counters() -> NgAnalyzeCounters {
    COUNTERS.lock().map(|c| c.clone()).unwrap_or_default()
}

/// Resets the facts-path counters.
pub fn reset_ng_analyze_counters() {
    with_counters(|c| *c = NgAnalyzeCounters::default());
}

fn fallback(reason: &str) {
    with_counters(|c| *c.fallbacks.entry(reason.to_string()).or_insert(0) += 1);
}

fn hit() {
    with_counters(|c| c.facts_used += 1);
}

#[cfg(feature = "ng-analyze")]
thread_local! {
    /// Set by [`with_treaty_extraction`]: when true the facts path is bypassed for this thread.
    static TREATY_ONLY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs `f` with Treaty's own extraction forced (the facts path disabled on this thread). The
/// differential test uses it to compile the same source both ways in one process.
#[cfg(feature = "ng-analyze")]
pub fn with_treaty_extraction<R>(f: impl FnOnce() -> R) -> R {
    let previous = TREATY_ONLY.with(|c| c.replace(true));
    let result = f();
    TREATY_ONLY.with(|c| c.set(previous));
    result
}

/// Runs the bridge over one source. `resolved` supplies the template and style contents Treaty's
/// bundler resolves; they are handed to ng-analyze as virtual files so both extractors read identical
/// bytes. Returns the facts for the file, or the file-level reason ng-analyze refused it.
#[cfg(feature = "ng-analyze")]
pub(super) fn bridge_facts(
    source: &str,
    summary: &ParseOutput,
    resolved: Option<&ResolvedContentMap>,
) -> (Option<Vec<DecoratorFacts>>, Option<String>) {
    if TREATY_ONLY.with(|c| c.get()) {
        return (None, None);
    }
    let resources = resource_files(summary, resolved);
    let file = treaty_ivy_ng_bridge::extract("input.ts", source, &resources);
    if let Some(err) = file.file_errors.first() {
        return (None, Some(err.clone()));
    }
    (Some(file.classes), None)
}

/// Without the feature there is no bridge: the facts are never consulted.
#[cfg(not(feature = "ng-analyze"))]
pub(super) fn bridge_facts(
    _source: &str,
    _summary: &ParseOutput,
    _resolved: Option<&ResolvedContentMap>,
) -> (Option<Vec<DecoratorFacts>>, Option<String>) {
    (None, None)
}

#[cfg(feature = "ng-analyze")]
/// The virtual resource files ng-analyze reads for `templateUrl` / `styleUrls`, keyed by the URL as
/// written (without a leading `./`). Contents come from the host-resolved channel, so the bridge reads
/// the same bytes Treaty emits.
fn resource_files(summary: &ParseOutput, resolved: Option<&ResolvedContentMap>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Some(resolved) = resolved else { return out };
    for class in &summary.classes {
        let Some(name) = class.name.as_ref() else { continue };
        let Some(content) = resolved.get(name) else { continue };
        let Some((_, dec)) = class_top_level(class) else { continue };
        if !matches!(decorator_name(dec), Some("Component")) {
            continue;
        }
        let Some(obj) = decorator_object(dec) else { continue };
        if let (Some(url), Some(template)) = (
            find_prop(obj, "templateUrl").and_then(string_value),
            content.template.as_ref(),
        ) {
            out.push((url.trim_start_matches("./").to_string(), template.clone()));
        }
        let style_urls = find_prop(obj, "styleUrls")
            .and_then(string_array_value)
            .or_else(|| find_prop(obj, "styleUrl").and_then(string_value).map(|u| vec![u]));
        if let Some(urls) = style_urls {
            if urls.len() == content.styles.len() {
                for (url, css) in urls.iter().zip(content.styles.iter()) {
                    out.push((url.trim_start_matches("./").to_string(), css.clone()));
                }
            }
        }
    }
    out
}

/// Lowers a fact's expression text through Treaty's own converter. `None` when it does not lower.
fn lower_expr(text: &str) -> Option<Expr> {
    let wrapped = format!("const __treaty_fact = {text};");
    let init = ParsingBackend::default().parse_module(&wrapped, SourceKind::TypeScriptModule, |m| {
        match m.summary().top_level.first()? {
            NTopStmt::VarDecl { decls, .. } => decls.first()?.init.clone(),
            _ => None,
        }
    })?;
    let mut init = init;
    while let NExpr::Parenthesized(inner) = init {
        init = *inner;
    }
    convert_expr(&init)
}

fn lower_opt(text: &Option<treaty_ivy_facts::ExprFact>, what: &str) -> Result<Option<Expr>, String> {
    match text {
        None => Ok(None),
        Some(e) => lower_expr(&e.text)
            .map(Some)
            .ok_or_else(|| format!("unsupported `{what}` expression form")),
    }
}

fn encapsulation_of(v: i32) -> Option<ViewEncapsulation> {
    match v {
        0 => Some(ViewEncapsulation::Emulated),
        2 => Some(ViewEncapsulation::None),
        3 => Some(ViewEncapsulation::ShadowDom),
        4 => Some(ViewEncapsulation::ExperimentalIsolatedShadowDom),
        _ => None,
    }
}

/// The directive extraction from one class's facts. Mirrors [`treaty_directive_extract`] field by field
/// on the raw facts, applying the conventions listed in the module docs.
#[allow(clippy::too_many_arguments)]
pub(super) fn facts_directive_extract(
    facts: &DecoratorFacts,
    kind: TopLevel,
    obj: Option<&ObjLit>,
    resolved: Option<&crate::decorators::registry::ResolvedComponentContent>,
    default_selector: Option<&str>,
    modernize: ModernizeOptions,
) -> Result<DirectiveExtract, String> {
    if !facts.unsupported.is_empty() {
        return Err(facts.unsupported.join("; "));
    }
    let d: &DirectiveFacts = facts.directive.as_ref().ok_or("no directive facts")?;
    let c = facts.component.as_ref();
    if kind == TopLevel::Component && c.is_none() {
        return Err("no component facts".to_string());
    }

    // selector: the declared one, else (components only) the filename-derived default.
    let selector = d.selector.clone().or_else(|| match kind {
        TopLevel::Component => default_selector.map(str::to_string),
        _ => None,
    });

    // template: inline, else the host-resolved templateUrl content.
    let (template_html, inline_template_is_some) = match c {
        None => (None, false),
        Some(c) => match (&c.template, &c.template_url) {
            (Some(t), _) => (Some(t.clone()), true),
            (None, Some(_)) => match resolved.and_then(|r| r.template.clone()) {
                Some(t) => (Some(t), false),
                None => {
                    return Err(
                        "external templateUrl has no resolved template (the host must supply the \
                         resolved template content for this component)"
                            .to_string(),
                    )
                }
            },
            (None, None) => return Err("component has no inline string `template`".to_string()),
        },
    };

    // The bridge's `changeDetection` is `Default`/`Eager`/`1` when written, and `None` for both an absent
    // option and an explicit `OnPush` (ng-analyze reports the two alike). The decorator object tells them
    // apart, so the explicit member is read from it when the facts carry none.
    let explicit_cd = c.and_then(|c| c.change_detection.clone()).or_else(|| {
        obj.and_then(|o| find_prop(o, "changeDetection"))
            .and_then(member_name)
            .map(str::to_string)
    });
    let change_detection = change_detection_strategy(explicit_cd.as_deref(), modernize.on_push);

    let mut styles = c.map(|c| c.styles.clone()).unwrap_or_default();
    if let Some(resolved) = resolved {
        styles.extend(resolved.styles.iter().cloned());
    }

    let encapsulation = match c.and_then(|c| c.encapsulation) {
        Some(v) => encapsulation_of(v).unwrap_or(ViewEncapsulation::Emulated),
        None => ViewEncapsulation::Emulated,
    };

    let animations = lower_opt(&c.and_then(|c| c.animations.clone()), "animations")?;
    let view_providers = lower_opt(&c.and_then(|c| c.view_providers.clone()), "viewProviders")?;
    let providers = lower_opt(&d.providers, "providers")?;

    let host = build_host(d)?;

    // ng-analyze drops an object-form `hostDirectives` entry whose `directive:` reference it cannot resolve
    // (silently, with no diagnostic). The declared entry count comes from this front-end's own parse, so
    // a shortfall is caught here and the class falls back instead of emitting a partial list.
    if let (Some(o), Some(list)) = (obj, d.host_directives.as_ref()) {
        if let Some(NExpr::Array(elements)) = find_prop(o, "hostDirectives") {
            let declared = elements.iter().filter(|e| matches!(e, NArrayElement::Expr(_))).count();
            if declared != list.len() {
                return Err(format!("hostDirectives: {} of {declared} entries resolved", list.len()));
            }
        }
    }

    let host_directives = d
        .host_directives
        .as_ref()
        .map(|list| list.iter().map(host_directive).collect::<Result<Vec<_>, _>>())
        .transpose()?
        .filter(|v| !v.is_empty());

    let mut inputs: OrderedMap<String, R3InputMetadata> = OrderedMap::new();
    for i in &d.inputs {
        // Signal inputs are signal-based; decorator inputs are lowered to signals only under the
        // modernizer (`signal_inputs`), exactly as `collect_io` does.
        let is_signal = if i.from_decorator { modernize.signal_inputs } else { true };
        inputs.insert(
            i.property.clone(),
            R3InputMetadata {
                class_property_name: i.property.clone(),
                binding_property_name: i.public_name.clone(),
                required: i.required,
                is_signal,
                // A decorator input's `transform` is captured; a signal input's is not. Angular's
                // definition omits it (golden `signal_inputs/should not capture the transform function`).
                transform_function: if i.from_decorator {
                    lower_opt(&i.transform, "transform")?
                } else {
                    None
                },
            },
        );
    }
    let mut outputs: OrderedMap<String, String> = OrderedMap::new();
    for o in &d.outputs {
        outputs.insert(o.property.clone(), o.public_name.clone());
    }

    let mut content_queries: Vec<R3QueryMetadata> = d.content_queries.iter().map(query).collect::<Result<_, _>>()?;
    let mut view_queries: Vec<R3QueryMetadata> = d.view_queries.iter().map(query).collect::<Result<_, _>>()?;
    order_queries_for_emit(&mut content_queries);
    order_queries_for_emit(&mut view_queries);

    // The explicit `signals` option only (ngtsc's `isSignal`; see `treaty_directive_extract`).
    let is_signal = d.signals;

    // The `imports:` closure-wrap decision reads the decorator object (as Treaty's does).
    let import_has_forward_ref = imports_has_forward_ref(obj);

    let ctor_deps = match &facts.constructor_deps {
        None => FactoryDeps::Deps(Vec::new()),
        Some(deps) => FactoryDeps::Deps(deps.iter().map(dep).collect::<Result<_, _>>()?),
    };

    Ok(DirectiveExtract {
        selector,
        template_html,
        inline_template_is_some,
        standalone: d.standalone,
        change_detection,
        styles,
        encapsulation,
        animations,
        foreign_imports: None,
        view_providers,
        preserve_whitespaces: c.and_then(|c| c.preserve_whitespaces),
        host,
        host_directives,
        export_as: d.export_as.clone(),
        providers,
        inputs,
        outputs,
        content_queries,
        view_queries,
        is_signal,
        uses_on_changes: d.uses_on_changes,
        import_has_forward_ref,
        ctor_deps,
    })
}

/// Treaty's raw-entry reading of the `host` object plus the member bindings, in source order, merged the
/// way `build_host_metadata` merges them (object entries first, member entries inserted over them).
fn build_host(d: &DirectiveFacts) -> Result<R3HostMetadata, String> {
    let mut raw: OrderedMap<String, HostValue> = OrderedMap::new();
    for (key, value) in d.host_object.iter().flatten() {
        match value {
            FactHostValue::Str(s) => {
                raw.insert(host_property_key_or_plain(key), HostValue::Str(s.clone()));
            }
            // An expression value is lowered and kept, as `parse_host_object` keeps it.
            FactHostValue::Expr(text) => {
                let expr = lower_expr(text)
                    .ok_or_else(|| format!("`host` value for '{key}' must be a string or an expression"))?;
                raw.insert(host_property_key_or_plain(key), HostValue::Expr(expr));
            }
        }
    }
    // Member entries in declaration order, bindings and listeners interleaved by member position.
    enum Member<'a> {
        Binding(&'a treaty_ivy_facts::HostBindingFact),
        Listener(&'a treaty_ivy_facts::HostListenerFact),
    }
    let mut members: Vec<(usize, Member)> = d
        .host_bindings
        .iter()
        .map(|b| (b.member_start, Member::Binding(b)))
        .chain(d.host_listeners.iter().map(|l| (l.member_start, Member::Listener(l))))
        .collect();
    members.sort_by_key(|(start, _)| *start);
    for (_, m) in members {
        match m {
            Member::Binding(b) => {
                let host_prop = b.host_prop.clone();
                let value = if !is_safe_object_key(&b.member) {
                    format!("this[\"{}\"]", b.member.replace('"', "\\\""))
                } else if is_reserved_literal_word(&b.member) {
                    format!("this.{}", b.member)
                } else {
                    b.member.clone()
                };
                raw.insert(host_property_key(&host_prop), HostValue::Str(value));
            }
            Member::Listener(l) => {
                let handler = format!("{}({})", l.method, l.args.join(","));
                raw.insert(format!("({})", l.event), HostValue::Str(handler));
            }
        }
    }
    parse_host_bindings(raw)
}

fn host_directive(h: &HostDirectiveFact) -> Result<R3HostDirectiveMetadata, String> {
    let to_map = |pairs: &Option<Vec<(String, String)>>| {
        pairs.as_ref().map(|list| {
            let mut m: OrderedMap<String, String> = OrderedMap::new();
            for (k, v) in list {
                m.insert(k.clone(), v.clone());
            }
            m
        })
    };
    if h.name.is_empty() {
        return Err("hostDirectives entry with no resolvable name".to_string());
    }
    Ok(R3HostDirectiveMetadata {
        directive: directive_ref(&h.name),
        is_forward_reference: h.forward_ref,
        inputs: to_map(&h.inputs),
        outputs: to_map(&h.outputs),
    })
}

fn query(q: &QueryFact) -> Result<R3QueryMetadata, String> {
    let predicate = match &q.predicate {
        QueryPredicateFact::Selectors(list) => QueryPredicate::Selectors(
            list.iter().flat_map(|s| s.split(',').map(|p| p.trim().to_string())).collect(),
        ),
        QueryPredicateFact::Expr(e) => {
            // ng-analyze reports the predicate with `forwardRef` already stripped, and Treaty's reader
            // records the bare name, so the text is the same on both paths.
            QueryPredicate::Expr(lower_expr(&e.text).ok_or("unsupported query predicate expression form")?)
        }
    };
    // `read` is kept only in the identifier / member shapes Treaty's own reader accepts.
    let read = match q.read.as_ref().and_then(|r| lower_expr(&r.text)) {
        Some(e) if matches!(e.kind, o::ExprKind::ReadVar { .. } | o::ExprKind::ReadProp { .. }) => Some(e),
        _ => None,
    };
    Ok(R3QueryMetadata {
        property_name: q.property.clone(),
        first: q.first,
        predicate,
        descendants: q.descendants,
        emit_distinct_changes_only: true,
        read,
        static_: q.is_static,
        is_signal: q.is_signal,
    })
}

fn dep(d: &DepFact) -> Result<R3DependencyMetadata, String> {
    let token = match &d.token {
        None => None,
        Some(t) => Some(lower_expr(&t.text).ok_or("unsupported constructor dependency token")?),
    };
    Ok(R3DependencyMetadata {
        attribute_name_type: if d.attribute.is_some() { token.clone() } else { None },
        token,
        host: d.host,
        optional: d.optional,
        self_: d.self_,
        skip_self: d.skip_self,
    })
}

/// Finds the facts for the class declared at byte `class_start`.
pub(super) fn facts_for_class(facts: &[DecoratorFacts], class_start: usize) -> Option<&DecoratorFacts> {
    facts.iter().find(|f| f.class_start == class_start)
}

pub(super) fn record_fallback(reason: &str) {
    fallback(reason);
}

pub(super) fn record_hit() {
    hit();
}
