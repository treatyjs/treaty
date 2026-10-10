//! Treaty's OWN decorator facts: the same reads the emit path performs, reshaped into the plain
//! [`treaty_ivy_ng_bridge::DecoratorFacts`] that the ng-analyze bridge also produces.
//!
//! This is a child module of [`crate::source_compile`] so it reuses the private extraction helpers
//! (`collect_io`, the query collectors, `parse_host_directives`, `extract_ctor_deps`, ...) directly:
//! the facts describe what the existing emit path actually reads, not a second interpretation.
//! Nothing here changes emitted code; the feature-off build calls it only from the parity test.
//!
//! Raw-fact conventions (see the bridge's `facts` module): `signals` is the explicit option only,
//! `standalone`/`pure` are recorded as written (absent = the Angular default is applied by the
//! consumer, not here), and every expression is printed in its LOWERED form via [`lower_to_text`].

use crate::parse::NArrayElement;
use treaty_ivy_facts::{
    ClassKind, ComponentFacts, DecoratorFacts, DepFact, DirectiveFacts, ExprFact,
    HostBindingFact, HostDirectiveFact, HostListenerFact, HostValue, InjectableFacts, InputFact,
    NgModuleFacts, OutputFact, PipeFacts, QueryFact, QueryPredicateFact,
};

use super::*;

/// The Angular decorators Treaty recognises, in the order `class_top_level` checks them.
const RECOGNISED: &[(&str, ClassKind)] = &[
    ("Component", ClassKind::Component),
    ("Directive", ClassKind::Directive),
    ("Pipe", ClassKind::Pipe),
    ("NgModule", ClassKind::NgModule),
    ("Injectable", ClassKind::Injectable),
];

/// Facts for every class in `source` that carries a recognised Angular decorator, in source order.
pub fn decorator_facts_of_source(source: &str) -> Vec<DecoratorFacts> {
    let summary = ParsingBackend::default().parse_module(source, SourceKind::TypeScriptModule, |m| {
        m.summary().clone()
    });
    summary.classes.iter().filter_map(facts_of_class).collect()
}

/// Prints `text` (a JavaScript expression) through Treaty's own lowering: parse it, convert it with
/// [`convert_expr`], and print the result. Both extractors' expression facts are passed through this
/// before the parity comparison, so the comparison is between lowered forms. `None` when the text
/// does not parse as a single expression or does not convert.
pub fn lower_to_text(text: &str) -> Option<String> {
    let wrapped = format!("const __treaty_fact = {text};");
    let init = ParsingBackend::default().parse_module(&wrapped, SourceKind::TypeScriptModule, |m| {
        match m.summary().top_level.first()? {
            NTopStmt::VarDecl { decls, .. } => decls.first()?.init.clone(),
            _ => None,
        }
    })?;
    // Grouping parentheses carry no meaning, and ng-analyze's spans never include them, so they are
    // dropped before printing.
    let mut init = init;
    while let NExpr::Parenthesized(inner) = init {
        init = *inner;
    }
    convert_expr(&init).map(|e| print_text(&e))
}

/// The printed JavaScript of an output expression. The emitter prints an expression as a statement,
/// so the statement terminator is removed, and codegen's parentheses around a bare string literal
/// (`("name");`) are removed too; both are presentation, not part of the expression.
fn print_text(expr: &Expr) -> String {
    let printed = crate::output::emitter::emit_expression(expr);
    strip_outer_parens(printed.trim().trim_end_matches(';').trim()).to_string()
}

/// Strips parentheses that wrap the WHOLE text, i.e. whose opening `(` closes at the final character.
/// `(a)(b)` and `(a) + (b)` are left alone.
pub fn strip_outer_parens(text: &str) -> &str {
    let mut t = text.trim();
    while t.len() >= 2 && t.starts_with('(') && t.ends_with(')') {
        let mut depth = 0i32;
        let mut wraps = false;
        for (i, c) in t.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        wraps = i == t.len() - 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        if !wraps {
            break;
        }
        t = t[1..t.len() - 1].trim();
    }
    t
}

fn print(expr: &Expr) -> ExprFact {
    ExprFact::new(print_text(expr))
}

fn facts_of_class(class: &ClassWithDecorators) -> Option<DecoratorFacts> {
    let recognised: Vec<(&DecoratorInfo, ClassKind)> = class
        .decorators
        .iter()
        .filter_map(|dec| {
            let name = decorator_name(dec)?;
            RECOGNISED.iter().find(|(n, _)| *n == name).map(|(_, k)| (dec, *k))
        })
        .collect();
    let (_, first_kind) = *recognised.first()?;

    let mut facts = DecoratorFacts {
        class_name: class.name.clone(),
        class_start: class.span.start as usize,
        kind: Some(first_kind),
        ..Default::default()
    };
    // Every recognised decorator is read (Treaty compiles each trait, e.g. `@Pipe` + `@Injectable`).
    for (dec, kind) in &recognised {
        let obj = dec.object.as_ref();
        match kind {
            ClassKind::Component => {
                let (dir, comp) = component_facts(obj);
                facts.directive = Some(dir.unwrap_or_else(|e| {
                    facts.unsupported.push(e);
                    DirectiveFacts::default()
                }));
                if let Some(c) = comp {
                    facts.component = Some(c);
                }
            }
            ClassKind::Directive => match directive_facts(obj) {
                Ok(d) => facts.directive = Some(d),
                Err(e) => facts.unsupported.push(e),
            },
            ClassKind::Pipe => facts.pipe = Some(pipe_facts(obj)),
            ClassKind::Injectable => match injectable_facts(obj) {
                Ok(i) => facts.injectable = Some(i),
                Err(e) => facts.unsupported.push(e),
            },
            ClassKind::NgModule => facts.ng_module = Some(ng_module_facts(obj)),
        }
    }
    if facts.unsupported.is_empty() {
        facts.constructor_deps = ctor_deps(class);
    }
    if facts.unsupported.is_empty() {
        fill_members(class, &mut facts);
    }
    Some(facts)
}

/// Directive-level options shared by `@Directive` and `@Component`.
fn directive_options(obj: Option<&ObjLit>, is_component: bool) -> Result<DirectiveFacts, String> {
    if let Some(obj) = obj {
        for k in UNSUPPORTED_DECORATOR_KEYS {
            if find_prop(obj, k).is_some() {
                return Err(format!("unsupported @{} metadata key: {k}", if is_component { "Component" } else { "Directive" }));
            }
        }
    }
    let selector = obj.and_then(|o| find_prop(o, "selector")).and_then(string_value);
    // Absent = true; present but not a boolean literal = false (Treaty's reading, see `compile_component_or_directive`).
    let standalone = match obj.and_then(|o| find_prop(o, "standalone")) {
        None => true,
        Some(NExpr::Boolean(b)) => *b,
        Some(_) => false,
    };
    let signals = matches!(obj.and_then(|o| find_prop(o, "signals")), Some(NExpr::Boolean(true)));
    let export_as = obj
        .and_then(|o| find_prop(o, "exportAs"))
        .and_then(string_value)
        .map(|s| s.split(',').map(|p| p.trim().to_string()).collect());
    let host_object = match obj.and_then(|o| find_prop(o, "host")) {
        None => None,
        Some(expr) => Some(host_object(expr)?),
    };
    let host_directives = obj
        .and_then(|o| find_prop(o, "hostDirectives"))
        .and_then(parse_host_directives)
        .map(|list| list.iter().map(host_directive_fact).collect());
    let providers = match obj.and_then(|o| find_prop(o, "providers")) {
        None => None,
        Some(e) => Some(print(&convert_expr(e).ok_or("unsupported `providers` expression form")?)),
    };
    Ok(DirectiveFacts {
        selector,
        standalone,
        signals,
        export_as,
        host_object,
        host_bindings: Vec::new(),
        host_listeners: Vec::new(),
        host_directives,
        providers,
        inputs: Vec::new(),
        outputs: Vec::new(),
        content_queries: Vec::new(),
        view_queries: Vec::new(),
        uses_on_changes: false,
    })
}

fn directive_facts(obj: Option<&ObjLit>) -> Result<DirectiveFacts, String> {
    directive_options(obj, false)
}

/// The `host: {...}` object's entries in source order. Treaty refuses a non-object value, a computed
/// or spread key, and a non-string value; those refusals are reported as the class's error.
fn host_object(expr: &NExpr) -> Result<Vec<(String, HostValue)>, String> {
    let NExpr::Object(props) = expr else {
        return Err("`host` must be an object literal".to_string());
    };
    let mut out = Vec::new();
    for p in props {
        match p {
            NObjectProp::KeyValue { key, value, .. } => {
                let s = string_value(value)
                    .ok_or_else(|| format!("`host` value for '{key}' must be a string"))?;
                out.push((key.clone(), HostValue::Str(s)));
            }
            NObjectProp::Other(_) => return Err("unsupported `host` computed key".to_string()),
            NObjectProp::Spread(_) => return Err("unsupported `host` spread/shorthand".to_string()),
        }
    }
    Ok(out)
}

fn host_directive_fact(h: &R3HostDirectiveMetadata) -> HostDirectiveFact {
    let to_pairs = |m: &Option<OrderedMap<String, String>>| {
        m.as_ref().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
    };
    HostDirectiveFact {
        name: print_text(&h.directive.value),
        forward_ref: h.is_forward_reference,
        inputs: to_pairs(&h.inputs),
        outputs: to_pairs(&h.outputs),
    }
}

fn component_facts(obj: Option<&ObjLit>) -> (Result<DirectiveFacts, String>, Option<ComponentFacts>) {
    let dir = match directive_options(obj, true) {
        Ok(d) => d,
        Err(e) => return (Err(e), None),
    };
    let Some(obj) = obj else {
        return (Err("component has no inline string `template`".to_string()), None);
    };
    let template = find_prop(obj, "template").and_then(string_value);
    let template_url = find_prop(obj, "templateUrl").and_then(string_value);
    if find_prop(obj, "foreignImports").is_some() {
        return (Err("foreignImports is a Treaty-only key; not modelled as facts".to_string()), None);
    }
    if template.is_none() && find_prop(obj, "templateUrl").is_none() {
        return (Err("component has no inline string `template`".to_string()), None);
    }
    let change_detection = match find_prop(obj, "changeDetection") {
        Some(NExpr::Member { property, .. }) => Some(property.clone()),
        _ => None,
    };
    let encapsulation = find_prop(obj, "encapsulation").and_then(encapsulation_value).map(|e| e as i32);
    let animations = match find_prop(obj, "animations") {
        None => None,
        Some(e) => convert_expr(e).map(|x| print(&x)),
    };
    let view_providers = match find_prop(obj, "viewProviders") {
        None => None,
        Some(e) => match convert_expr(e) {
            Some(x) => Some(print(&x)),
            None => return (Err("unsupported `viewProviders` expression form".to_string()), None),
        },
    };
    let imports = find_prop(obj, "imports").and_then(|e| match e {
        NExpr::Array(elements) => Some(
            elements
                .iter()
                .filter_map(|el| match el {
                    NArrayElement::Expr(inner) => directive_name_maybe_forward(inner).map(|(n, _)| n),
                    _ => None,
                })
                .collect(),
        ),
        _ => None,
    });
    let comp = ComponentFacts {
        template,
        template_url,
        styles: find_prop(obj, "styles").and_then(string_array_value).unwrap_or_default(),
        style_urls: find_prop(obj, "styleUrls").and_then(string_array_value).unwrap_or_default(),
        change_detection,
        encapsulation,
        preserve_whitespaces: match find_prop(obj, "preserveWhitespaces") {
            Some(NExpr::Boolean(b)) => Some(*b),
            _ => None,
        },
        animations,
        view_providers,
        imports,
    };
    (Ok(dir), Some(comp))
}

fn pipe_facts(obj: Option<&ObjLit>) -> PipeFacts {
    let literal_bool = |key: &str| match obj.and_then(|o| find_prop(o, key)) {
        Some(NExpr::Boolean(b)) => Some(*b),
        _ => None,
    };
    PipeFacts {
        name: obj.and_then(|o| find_prop(o, "name")).and_then(string_value),
        pure: literal_bool("pure"),
        standalone: literal_bool("standalone"),
    }
}

fn injectable_facts(obj: Option<&ObjLit>) -> Result<InjectableFacts, String> {
    let opt = |key: &str| -> Result<Option<ExprFact>, String> {
        match obj.and_then(|o| find_prop(o, key)) {
            None => Ok(None),
            Some(e) => {
                let x = injectable_maybe_forward_ref(e)
                    .ok_or_else(|| format!("unsupported `{key}` expression form"))?;
                Ok(Some(print(&x.expression)))
            }
        }
    };
    let deps = match obj.and_then(|o| find_prop(o, "deps")) {
        None => None,
        Some(e) => Some(parse_injectable_deps(e)?.iter().map(dep_fact).collect()),
    };
    let use_factory = match obj.and_then(|o| find_prop(o, "useFactory")) {
        None => None,
        Some(e) => Some(print(&convert_expr(e).ok_or("unsupported `useFactory` expression form")?)),
    };
    Ok(InjectableFacts {
        provided_in: opt("providedIn")?,
        use_class: opt("useClass")?,
        use_factory,
        use_value: opt("useValue")?,
        use_existing: opt("useExisting")?,
        deps,
    })
}

fn ng_module_facts(obj: Option<&ObjLit>) -> NgModuleFacts {
    // Only references are names (an identifier or `forwardRef(() => X)`); a call such as
    // `provideX()` is not a reference and is not listed, matching what ng-analyze reports.
    let names = |key: &str| {
        obj.and_then(|o| find_prop(o, key)).and_then(|e| match e {
            NExpr::Array(elements) => Some(
                elements
                    .iter()
                    .filter_map(|el| match el {
                        NArrayElement::Expr(inner) => directive_name_maybe_forward(inner).map(|(n, _)| n),
                        _ => None,
                    })
                    .collect(),
            ),
            _ => None,
        })
    };
    NgModuleFacts {
        declarations: names("declarations"),
        imports: names("imports"),
        exports: names("exports"),
        bootstrap: names("bootstrap"),
        providers: obj
            .and_then(|o| find_prop(o, "providers"))
            .and_then(convert_expr)
            .map(|x| print(&x)),
        schemas: names("schemas"),
    }
}

fn dep_fact(dep: &R3DependencyMetadata) -> DepFact {
    DepFact {
        token: dep.token.as_ref().map(print),
        attribute: dep
            .attribute_name_type
            .as_ref()
            .map(|e| unquote(&print_text(e))),
        optional: dep.optional,
        host: dep.host,
        self_: dep.self_,
        skip_self: dep.skip_self,
    }
}

fn unquote(raw: &str) -> String {
    let t = raw.trim();
    for q in ['"', '\'', '`'] {
        if t.len() >= 2 && t.starts_with(q) && t.ends_with(q) {
            return t[1..t.len() - 1].to_string();
        }
    }
    t.to_string()
}

/// The constructor dependencies: `None` when the class has no constructor (Treaty's
/// [`extract_ctor_deps`] reads the body-bearing constructor first, then the first with parameters).
fn ctor_deps(class: &ClassWithDecorators) -> Option<Vec<DepFact>> {
    let has_ctor = class.members.iter().any(|m| m.kind == MemberKind::Constructor);
    if !has_ctor {
        return None;
    }
    match extract_ctor_deps(class) {
        FactoryDeps::Deps(deps) => Some(deps.iter().map(dep_fact).collect()),
        _ => None,
    }
}

/// Member-level facts: inputs and outputs (through `collect_io`), queries (through the query
/// collectors), `@HostBinding`/`@HostListener` entries, and `ngOnChanges`.
fn fill_members(class: &ClassWithDecorators, facts: &mut DecoratorFacts) {
    let Some(dir) = facts.directive.as_mut() else { return };
    dir.uses_on_changes = class_uses_on_changes(class);

    let mut inputs: OrderedMap<String, R3InputMetadata> = OrderedMap::new();
    let mut outputs: OrderedMap<String, String> = OrderedMap::new();
    if collect_io(class, &mut inputs, &mut outputs, ModernizeOptions::default()).is_err() {
        facts.unsupported.push("unsupported member decorator".to_string());
        return;
    }
    dir.inputs = inputs
        .iter()
        .map(|(_, i)| InputFact {
            property: i.class_property_name.clone(),
            public_name: i.binding_property_name.clone(),
            required: i.required,
            is_signal: i.is_signal,
            // Under the default modernizer a decorator input is never signal-based (`is_signal` false).
            from_decorator: !i.is_signal,
            transform: i.transform_function.as_ref().map(print),
        })
        .collect();
    dir.outputs = outputs
        .iter()
        .map(|(prop, public)| OutputFact { property: prop.clone(), public_name: public.clone() })
        .collect();

    let mut content: Vec<R3QueryMetadata> = Vec::new();
    let mut view: Vec<R3QueryMetadata> = Vec::new();
    collect_signal_queries(class, &mut content, &mut view);
    collect_decorator_queries(class, &mut content, &mut view);
    dir.content_queries = content.iter().map(|q| query_fact(q, true)).collect();
    dir.view_queries = view.iter().map(|q| query_fact(q, false)).collect();

    let mut bindings = Vec::new();
    let mut listeners = Vec::new();
    for member in &class.members {
        let Some(member_name) = member.name.as_deref() else { continue };
        for dec in &member.decorators {
            match decorator_name(dec) {
                Some("HostBinding") if matches!(member.kind, MemberKind::Property | MemberKind::Accessor | MemberKind::Method) => {
                    bindings.push(HostBindingFact {
                        host_prop: decorator_string_alias(dec).unwrap_or_else(|| member_name.to_string()),
                        member: member_name.to_string(),
                        member_start: member.span.start as usize,
                    });
                }
                Some("HostListener")
                    if matches!(
                        member.kind,
                        MemberKind::Property | MemberKind::Accessor | MemberKind::Method
                    ) =>
                {
                    if let Some(NExpr::String(event)) = dec.arguments.first().map(NArg::expr) {
                        let args = match dec.arguments.get(1).map(NArg::expr) {
                            Some(NExpr::Array(elements)) => elements
                                .iter()
                                .filter_map(|el| match el {
                                    NArrayElement::Expr(inner) => string_value(inner),
                                    _ => None,
                                })
                                .collect(),
                            _ => Vec::new(),
                        };
                        listeners.push(HostListenerFact {
                            event: event.clone(),
                            method: member_name.to_string(),
                            args,
                            member_start: member.span.start as usize,
                        });
                    }
                }
                _ => {}
            }
        }
    }
    dir.host_bindings = bindings;
    dir.host_listeners = listeners;
}

fn query_fact(q: &R3QueryMetadata, is_content: bool) -> QueryFact {
    let predicate = match &q.predicate {
        QueryPredicate::Selectors(list) => QueryPredicateFact::Selectors(list.clone()),
        QueryPredicate::Expr(e) => QueryPredicateFact::Expr(print(e)),
    };
    QueryFact {
        property: q.property_name.clone(),
        first: q.first,
        is_content,
        predicate,
        descendants: q.descendants,
        read: q.read.as_ref().map(print),
        is_static: q.static_,
        is_signal: q.is_signal,
    }
}
