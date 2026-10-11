//! `@Pipe`, `@Injectable`, `@NgModule` and constructor-dependency adapters.

use ng_analyze::types::metadata::{
    ClassMetadata, ConstructorParamMetadata, DependencyMetadata, InjectableMetadata,
    NgModuleMetadata, PipeMetadata, ProviderField, ReferenceMetadata,
};

use super::{reference_name, Ctx};
use treaty_ivy_facts::facts::{
    DecoratorFacts, DepFact, ExprFact, InjectableFacts, NgModuleFacts, PipeFacts,
};

/// Fills every `@Pipe`, `@Injectable` and `@NgModule` section ng-analyze reported for `class`.
pub(super) fn adapt(ctx: &Ctx<'_>, class: &ClassMetadata, facts: &mut DecoratorFacts) {
    if let Some(p) = class.pipe.as_ref() {
        facts.pipe = Some(pipe_facts(p));
    }
    if let Some(i) = class.injectable.as_ref() {
        facts.injectable = Some(injectable_facts(ctx, i));
    }
    if let Some(m) = class.ng_module.as_ref() {
        facts.ng_module = Some(ng_module_facts(ctx, m));
    }
}

fn pipe_facts(p: &PipeMetadata) -> PipeFacts {
    PipeFacts { name: Some(p.name.clone()), pure: p.pure, standalone: p.standalone }
}

/// A provider option's source text. ng-analyze's span excludes a `forwardRef(() => X)` wrapper and
/// flags it instead, so the wrapper is restored here to match what the author wrote.
fn provider_expr(ctx: &Ctx<'_>, p: Option<&ProviderField>) -> Option<ExprFact> {
    let p = p?;
    let inner = ctx.expr(Some(&p.span))?;
    if p.is_forward_ref {
        Some(ExprFact::new(format!("forwardRef(() => {})", inner.text)))
    } else {
        Some(inner)
    }
}

fn injectable_facts(ctx: &Ctx<'_>, i: &InjectableMetadata) -> InjectableFacts {
    InjectableFacts {
        provided_in: provider_expr(ctx, i.provided_in.as_ref()),
        use_class: provider_expr(ctx, i.use_class.as_ref()),
        use_factory: provider_expr(ctx, i.use_factory.as_ref()),
        use_value: provider_expr(ctx, i.use_value.as_ref()),
        use_existing: provider_expr(ctx, i.use_existing.as_ref()),
        deps: i
            .deps
            .as_ref()
            .map(|v| v.iter().map(|d| dependency_fact(ctx, d)).collect()),
    }
}

fn dependency_fact(ctx: &Ctx<'_>, d: &DependencyMetadata) -> DepFact {
    DepFact {
        token: ctx.expr(d.token_span.as_ref()),
        attribute: None,
        optional: d.optional,
        host: d.host,
        self_: d.self_qualifier,
        skip_self: d.skip_self,
    }
}

fn ng_module_facts(ctx: &Ctx<'_>, m: &NgModuleMetadata) -> NgModuleFacts {
    let names = |v: &Option<Vec<ReferenceMetadata>>| {
        v.as_ref().map(|list| list.iter().filter_map(reference_name).collect::<Vec<_>>())
    };
    NgModuleFacts {
        declarations: names(&m.declarations),
        imports: names(&m.imports),
        exports: names(&m.exports),
        bootstrap: names(&m.bootstrap),
        providers: ctx.expr(m.providers_span.as_ref()),
        schemas: m.schemas.clone(),
    }
}

/// Constructor dependencies of `class`, in parameter order. `None` when the class has no constructor.
pub(super) fn constructor_deps(ctx: &Ctx<'_>, class: &ClassMetadata) -> Option<Vec<DepFact>> {
    class
        .constructor_params
        .as_ref()
        .map(|params| params.iter().map(|p| param_dep(ctx, p)).collect())
}

fn param_dep(_ctx: &Ctx<'_>, p: &ConstructorParamMetadata) -> DepFact {
    let angular = |name: &str| p.decorators.iter().any(|d| d.is_angular && d.name == name);
    // ng-analyze unquotes a string-literal argument and sets `is_literal`; the source text is restored
    // so the token reads as the author wrote it.
    let arg0 = |name: &str| {
        p.decorators
            .iter()
            .find(|d| d.is_angular && d.name == name)
            .and_then(|d| d.args.as_ref())
            .and_then(|args| args.first())
            .map(|a| if a.is_literal { quote(&a.value) } else { a.value.clone() })
    };
    // `@Attribute('name')` injects the literal name: the token is that literal, as Treaty reads it.
    let attribute_raw = arg0("Attribute");
    let token = arg0("Inject")
        .or_else(|| attribute_raw.clone())
        .or_else(|| p.type_name.clone())
        .map(ExprFact::new);
    DepFact {
        token,
        attribute: attribute_raw.map(|raw| unquote(&raw)),
        optional: angular("Optional"),
        host: angular("Host"),
        self_: angular("Self"),
        skip_self: angular("SkipSelf"),
    }
}

fn quote(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

fn unquote(raw: &str) -> String {
    let t = raw.trim();
    for q in ['\'', '"', '`'] {
        if t.len() >= 2 && t.starts_with(q) && t.ends_with(q) {
            return t[1..t.len() - 1].to_string();
        }
    }
    t.to_string()
}
