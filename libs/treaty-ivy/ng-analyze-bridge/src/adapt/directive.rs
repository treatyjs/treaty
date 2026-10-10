//! `@Directive` and `@Component` adapter: decorator options, host bindings and listeners, inputs,
//! outputs, queries, host directives, and the component-only fields.

use ng_analyze::types::metadata::{
    ClassMetadata, ComponentMetadata, DirectiveMetadata, HostDirectiveMetadata, InputMetadata,
    OutputMetadata, QueryMetadata,
};

use super::{reference_name, Ctx};
use treaty_ivy_facts::facts::{
    ComponentFacts, DecoratorFacts, DirectiveFacts, ExprFact, HostBindingFact,
    HostDirectiveFact, HostListenerFact, HostValue, InputFact, OutputFact, QueryFact,
    QueryPredicateFact,
};

/// Fills `facts.directive` (and `facts.component` for a component) from `class`.
pub(super) fn adapt(ctx: &Ctx<'_>, class: &ClassMetadata, facts: &mut DecoratorFacts) {
    let mut directive = match class.component.as_ref() {
        Some(c) => {
            let (dir, comp) = component_options(ctx, c, class);
            if let Some(err) = comp.as_ref().err() {
                facts.unsupported.push(err.clone());
            }
            if let Ok(component) = comp {
                facts.component = Some(component);
            }
            dir
        }
        None => match class.directive.as_ref() {
            Some(d) => directive_options(ctx, d, class),
            None => return,
        },
    };
    if !facts.unsupported.is_empty() {
        return;
    }

    // Host bindings and listeners: ng-analyze reports member decorators separately from the `host`
    // object, so they are kept apart here and merged by the consumer in Treaty's order.
    directive.host_bindings = class
        .host_bindings
        .iter()
        .map(|b| HostBindingFact {
            host_prop: b
                .host_property_name
                .clone()
                .or_else(|| ctx.text(&b.member_name))
                .unwrap_or_default(),
            member: ctx.text(&b.member_name).unwrap_or_default(),
            member_start: ctx.span(&b.member_span).map_or(0, |s| s.start),
        })
        .collect();
    directive.host_listeners = class
        .host_listeners
        .iter()
        .map(|l| HostListenerFact {
            event: l
                .resolved_event_name
                .clone()
                .or_else(|| l.event_name.as_ref().and_then(|e| ctx.text(e)))
                .unwrap_or_default(),
            method: ctx.text(&l.method_name).unwrap_or_default(),
            args: l.args.iter().filter_map(|a| ctx.text(a)).collect(),
            member_start: ctx.span(&l.member_span).map_or(0, |s| s.start),
        })
        .collect();

    directive.inputs = class.inputs.iter().map(|i| input_fact(ctx, i)).collect();
    directive.outputs = class.outputs.iter().map(|o| output_fact(o)).collect();
    directive.content_queries = class.queries.iter().map(|q| query_fact(ctx, q, false)).collect();
    directive.view_queries = class.view_queries.iter().map(|q| query_fact(ctx, q, true)).collect();
    directive.uses_on_changes = class.uses_on_changes;

    facts.directive = Some(directive);
}

fn directive_options(ctx: &Ctx<'_>, d: &DirectiveMetadata, class: &ClassMetadata) -> DirectiveFacts {
    DirectiveFacts {
        selector: d.selector.clone(),
        standalone: d.standalone,
        signals: d.signals,
        export_as: d.export_as.clone(),
        host_object: host_object(ctx, d.host_span.is_some(), d.host_metadata.as_deref(), &d.host_properties),
        host_directives: host_directives(d.host_directives.as_deref()),
        providers: ctx.expr(d.providers_span.as_ref()),
        ..base_from_class(class)
    }
}

fn component_options(
    ctx: &Ctx<'_>,
    c: &ComponentMetadata,
    class: &ClassMetadata,
) -> (DirectiveFacts, Result<ComponentFacts, String>) {
    let directive = DirectiveFacts {
        selector: component_selector(c.selector.as_deref()),
        standalone: c.standalone,
        signals: c.signals,
        export_as: c.export_as.clone(),
        host_object: host_object(ctx, c.host_span.is_some(), c.host_metadata.as_deref(), &c.host_properties),
        host_directives: host_directives(c.host_directives.as_deref()),
        providers: ctx.expr(c.providers_span.as_ref()),
        ..base_from_class(class)
    };
    let comp = if c.foreign_imports.is_some() {
        Err("foreignImports is a Treaty-only key; the bridge does not model it".to_string())
    } else if c.template_dynamic {
        Err("component template is not statically evaluable".to_string())
    } else if c.template.is_none() && c.template_url.is_none() {
        Err("component has no inline string `template`".to_string())
    } else {
        Ok(ComponentFacts {
            // ng-analyze also reports the LOADED content of a `templateUrl` as `template`; the inline
            // string exists only when no `templateUrl` is declared.
            template: if c.template_url.is_none() { c.template.clone() } else { None },
            template_url: c.template_url.as_ref().map(|u| u.url.clone()),
            styles: c.styles.clone().unwrap_or_default(),
            style_urls: c
                .style_urls
                .as_ref()
                .map(|v| v.iter().map(|u| u.url.clone()).collect())
                .unwrap_or_default(),
            change_detection: c.change_detection.as_deref().and_then(change_detection_name),
            encapsulation: c.encapsulation,
            preserve_whitespaces: c.preserve_whitespaces,
            animations: ctx.expr(c.animations_span.as_ref()),
            view_providers: ctx.expr(c.view_providers_span.as_ref()),
            imports: c
                .imports
                .as_ref()
                .map(|v| v.iter().filter_map(reference_name).collect()),
        })
    };
    (directive, comp)
}

/// ng-analyze fills an absent component `selector` with its own default tag (`ng-component`), which
/// the Angular runtime also uses. The facts record only what the decorator wrote, so that default is
/// reported as absent. (A literal `selector: 'ng-component'` is therefore read as absent too.)
fn component_selector(selector: Option<&str>) -> Option<String> {
    match selector {
        Some("ng-component") | None => None,
        Some(s) => Some(s.to_string()),
    }
}

/// The `changeDetection` member name. ng-analyze reports an evaluated strategy as its number
/// (`ChangeDetectionStrategy.Default` -> `1`) and reports an explicit `OnPush` as absent, so the
/// number is mapped back to a name here; `None` (absent or OnPush) stays `None`.
fn change_detection_name(text: &str) -> Option<String> {
    let t = text.trim();
    match t {
        "1" => Some("Default".to_string()),
        "0" => None,
        _ => Some(t.rsplit('.').next().unwrap_or(t).trim().to_string()),
    }
}

fn base_from_class(class: &ClassMetadata) -> DirectiveFacts {
    DirectiveFacts {
        selector: None,
        standalone: true,
        signals: false,
        export_as: None,
        host_object: None,
        host_bindings: Vec::new(),
        host_listeners: Vec::new(),
        host_directives: None,
        providers: None,
        inputs: Vec::new(),
        outputs: Vec::new(),
        content_queries: Vec::new(),
        view_queries: Vec::new(),
        uses_on_changes: class.uses_on_changes,
    }
}

/// The `host` object's entries in source order. ng-analyze's folded `host_metadata` OMITS an entry whose
/// value does not evaluate (it is not an error for it), so the complete `host_properties` list is the
/// source of truth: an entry the evaluator folded takes its folded value, and one it did not is an
/// expression carrying its source text. `has_key` is whether the decorator carries a `host` property.
fn host_object(
    ctx: &Ctx<'_>,
    has_key: bool,
    folded: Option<&[ng_analyze::types::metadata::HostMetadataEntry]>,
    properties: &[ng_analyze::types::metadata::HostPropertyMetadata],
) -> Option<Vec<(String, HostValue)>> {
    if !has_key {
        return None;
    }
    let folded = folded.unwrap_or(&[]);
    Some(
        properties
            .iter()
            .filter_map(|p| {
                let key = p.key.text.clone()?;
                let value = match folded.iter().find(|e| e.key == key) {
                    Some(e) => match (&e.value, &e.expression) {
                        (Some(v), _) => HostValue::Str(v.clone()),
                        (None, Some(x)) => HostValue::Expr(x.clone()),
                        (None, None) => HostValue::Expr(ctx.slice(&p.value.source_span).unwrap_or_default()),
                    },
                    None => HostValue::Expr(ctx.slice(&p.value.source_span).unwrap_or_default()),
                };
                Some((key, value))
            })
            .collect(),
    )
}

fn host_directives(entries: Option<&[HostDirectiveMetadata]>) -> Option<Vec<HostDirectiveFact>> {
    let entries = entries?;
    Some(
        entries
            .iter()
            .map(|h| HostDirectiveFact {
                // Only a LOCAL binding is a usable name. A namespace member (`ns.Dir`) has no local alias;
                // its imported symbol alone (`Dir`) is not in scope in the file, so it is left empty and the
                // consumer refuses the entry rather than emitting an unbound identifier.
                name: h.directive.local_alias.clone().unwrap_or_default(),
                forward_ref: h.is_forward_ref,
                inputs: h.inputs.as_ref().map(|v| {
                    v.iter().map(|b| (b.public_name.clone(), b.binding_name.clone())).collect()
                }),
                outputs: h.outputs.as_ref().map(|v| {
                    v.iter().map(|b| (b.public_name.clone(), b.binding_name.clone())).collect()
                }),
            })
            .collect(),
    )
}

fn input_fact(ctx: &Ctx<'_>, i: &InputMetadata) -> InputFact {
    InputFact {
        property: i.name.clone(),
        public_name: i.alias.clone().unwrap_or_else(|| i.name.clone()),
        required: i.required,
        is_signal: i.is_signal,
        from_decorator: i.decorator_span.is_some(),
        // Both transform kinds carry the transform's value span (`type` additionally has a type span).
        transform: i.transform.as_ref().and_then(|t| ctx.expr(Some(&t.span))),
    }
}

fn output_fact(o: &OutputMetadata) -> OutputFact {
    OutputFact {
        property: o.name.clone(),
        public_name: o.alias.clone().unwrap_or_else(|| o.name.clone()),
    }
}

fn query_fact(ctx: &Ctx<'_>, q: &QueryMetadata, is_view: bool) -> QueryFact {
    let predicate = match &q.predicate_selectors {
        Some(list) => QueryPredicateFact::Selectors(list.clone()),
        None => match ctx.expr(Some(&q.predicate_span)) {
            Some(e) => QueryPredicateFact::Expr(e),
            None => QueryPredicateFact::Expr(ExprFact::default()),
        },
    };
    QueryFact {
        property: q.property_name.clone(),
        first: q.first,
        is_content: !is_view,
        predicate,
        descendants: q.descendants,
        read: ctx.expr(q.read_span.as_ref()),
        is_static: q.is_static,
        is_signal: q.is_signal,
    }
}
