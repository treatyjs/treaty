//! Adapter tests: ng-analyze's output for small sources and for the assessment fixtures, read into the
//! plain facts. Feature-gated: the adapters need ng-analyze (`--features analyze`).
#![cfg(feature = "analyze")]

use std::fs;
use std::path::Path;

use treaty_ivy_ng_bridge::facts::{
    ClassKind, DecoratorFacts, HostValue, QueryPredicateFact,
};
use treaty_ivy_ng_bridge::{extract, FileFacts};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/project/src");

/// Every file of the fixture project as `(path relative to src, content)`.
fn fixture_files() -> Vec<(String, String)> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, String)>) {
        for e in fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, base, out);
            } else {
                let rel = p.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/");
                out.push((rel, fs::read_to_string(&p).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(Path::new(FIXTURES), Path::new(FIXTURES), &mut out);
    out
}

/// Extracts one fixture file, with the rest of the project available as virtual files.
fn fixture(name: &str) -> FileFacts {
    let files = fixture_files();
    let source = files.iter().find(|(n, _)| n == name).expect("fixture").1.clone();
    let others: Vec<(String, String)> = files.iter().filter(|(n, _)| n != name).cloned().collect();
    extract(name, &source, &others)
}

fn only(file: &FileFacts) -> &DecoratorFacts {
    assert!(file.file_errors.is_empty(), "file errors: {:?}", file.file_errors);
    assert_eq!(file.classes.len(), 1, "classes: {:?}", file.classes.iter().map(|c| &c.class_name).collect::<Vec<_>>());
    &file.classes[0]
}

fn snippet(src: &str) -> FileFacts {
    extract("snippet.ts", src, &[])
}

// ---------------------------------------------------------------------------
// Components and directives (assessment fixtures).
// ---------------------------------------------------------------------------

#[test]
fn stat_card_signals_queries_host_and_options() {
    let f = fixture("stat-card.component.ts");
    let c = only(&f);
    assert_eq!(c.kind, Some(ClassKind::Component));
    let d = c.directive.as_ref().unwrap();
    assert_eq!(d.selector.as_deref(), Some("app-stat-card"));
    assert!(d.standalone);
    // The explicit `signals` option only; member-derived signal detection is the consumer's convention.
    assert!(!d.signals);
    assert_eq!(d.export_as.as_deref(), Some(&["statCard".to_string()][..]));
    // An explicit OnPush is reported as absent (the facts' documented ambiguity).
    assert_eq!(c.component.as_ref().unwrap().change_detection, None);
    let host = d.host_object.as_ref().unwrap();
    assert_eq!(host[1], ("[class.active]".to_string(), HostValue::Str("isActive()".to_string())));
    assert_eq!(d.host_bindings.len(), 1);
    assert_eq!((d.host_bindings[0].host_prop.as_str(), d.host_bindings[0].member.as_str()), ("attr.data-kind", "kind"));
    // `input.required<string>()` is a required signal input; `input(0, {alias})` renames it.
    let label = d.inputs.iter().find(|i| i.property == "label").unwrap();
    assert!(label.required && label.is_signal);
    let value = d.inputs.iter().find(|i| i.property == "value").unwrap();
    assert_eq!((value.public_name.as_str(), value.is_signal), ("count", true));
    // `output<void>()` and the `model()` change output.
    let outs: Vec<(&str, &str)> = d.outputs.iter().map(|o| (o.property.as_str(), o.public_name.as_str())).collect();
    assert!(outs.contains(&("reset", "reset")) && outs.contains(&("selection", "selectionChange")));
    // `viewChild.required<...>('valueEl')` is a single-result SIGNAL view query with a string locator.
    let q = d.view_queries.iter().find(|q| q.property == "valueEl").unwrap();
    assert!(q.first && q.is_signal && !q.is_content);
    assert_eq!(q.predicate, QueryPredicateFact::Selectors(vec!["valueEl".to_string()]));
    assert_eq!(q.read, None);
    assert_eq!(c.component.as_ref().unwrap().imports.as_deref(), Some(&["ShortNumberPipe".to_string()][..]));
    assert_eq!(c.component.as_ref().unwrap().style_urls, vec!["./stat-card.component.css".to_string()]);
}

#[test]
fn legacy_list_decorator_inputs_outputs_and_queries() {
    let f = fixture("legacy-list.component.ts");
    let c = only(&f);
    let d = c.directive.as_ref().unwrap();
    // `@Input({required: true, alias: 'items'}) rows`: the alias is the binding name, and it is required.
    let rows = d.inputs.iter().find(|i| i.property == "rows").unwrap();
    assert_eq!((rows.public_name.as_str(), rows.required, rows.is_signal, rows.from_decorator), ("items", true, false, true));
    // `@Output('picked') pick`.
    assert_eq!(d.outputs[0].public_name, "picked");
    // `@ContentChild(TemplateRef)` is a content query with an identifier predicate.
    let empty = d.content_queries.iter().find(|q| q.property == "emptyTpl").unwrap();
    assert!(empty.first && empty.is_content);
    assert_eq!(empty.predicate, QueryPredicateFact::Expr(treaty_ivy_ng_bridge::facts::ExprFact::new("TemplateRef")));
    // `@ViewChildren('row')` is a multi-result view query.
    let rows_q = d.view_queries.iter().find(|q| q.property == "rowEls").unwrap();
    assert!(!rows_q.first && !rows_q.is_content);
    // `@HostListener('window:resize', ['$event.target'])`.
    assert_eq!(d.host_listeners[0].event, "window:resize");
    assert_eq!(d.host_listeners[0].method, "onResize");
    assert_eq!(d.host_listeners[0].args, vec!["$event.target".to_string()]);
    // The template is the external file's content, so the inline template is absent.
    let comp = c.component.as_ref().unwrap();
    assert_eq!(comp.template, None);
    assert_eq!(comp.template_url.as_deref(), Some("./legacy-list.component.html"));
    assert_eq!(comp.encapsulation, Some(2)); // ViewEncapsulation.None
}

#[test]
fn cross_file_constant_selector_and_options_resolve() {
    let f = fixture("cross-file.component.ts");
    let c = only(&f);
    let d = c.directive.as_ref().unwrap();
    // `selector: CARD_SELECTOR` (imported const) resolves through the project; Treaty's extractor cannot.
    assert_eq!(d.selector.as_deref(), Some("app-shared-card"));
    // `@Input(CARD_INPUT_OPTIONS)` with an imported options object.
    let heading = d.inputs.iter().find(|i| i.property == "heading").unwrap();
    assert_eq!((heading.public_name.as_str(), heading.required), ("cardTitle", true));
}

#[test]
fn cross_file_directive_import_is_a_component_import() {
    let f = fixture("cross-import.component.ts");
    let c = only(&f);
    assert_eq!(c.component.as_ref().unwrap().imports.as_deref(), Some(&["BadgeDirective".to_string()][..]));
}

#[test]
fn explicit_default_change_detection_is_reported() {
    let f = fixture("default-cd.component.ts");
    let c = only(&f);
    assert_eq!(c.component.as_ref().unwrap().change_detection.as_deref(), Some("Default"));
}

#[test]
fn imported_directive_facts_use_the_decorator_selector_and_signal_input() {
    let f = fixture("shared/badge.directive.ts");
    let c = only(&f);
    assert_eq!(c.kind, Some(ClassKind::Directive));
    let d = c.directive.as_ref().unwrap();
    assert_eq!(d.selector.as_deref(), Some("[appBadge]"));
    assert_eq!(d.export_as.as_deref(), Some(&["badge".to_string()][..]));
    assert!(d.inputs[0].is_signal && !d.inputs[0].from_decorator);
}

// ---------------------------------------------------------------------------
// Pipes, injectables, NgModules, constructor dependencies.
// ---------------------------------------------------------------------------

#[test]
fn pipe_name_and_options() {
    let f = fixture("shared/short-number.pipe.ts");
    let p = only(&f).pipe.as_ref().unwrap();
    assert_eq!(p.name.as_deref(), Some("shortNum"));
    assert_eq!(p.standalone, Some(true));
}

#[test]
fn injectable_provided_in_and_constructor_dependencies() {
    let f = fixture("shared/counter.service.ts");
    let c = only(&f);
    let inj = c.injectable.as_ref().unwrap();
    assert_eq!(inj.provided_in.as_ref().unwrap().text, "'root'");
    let deps = c.constructor_deps.as_ref().unwrap();
    // `@Inject(COUNTER_START) @Optional()`: the token is the injected constant, optional.
    assert_eq!(deps[0].token.as_ref().unwrap().text, "COUNTER_START");
    assert!(deps[0].optional);
    // A plain typed parameter: the token is its type.
    assert_eq!(deps[1].token.as_ref().unwrap().text, "LegacyDep");
    assert!(!deps[1].optional);
}

#[test]
fn ng_module_reference_lists_and_providers() {
    let f = snippet(
        "import {NgModule} from '@angular/core';\n\
         @NgModule({ declarations: [A, B], imports: [C], exports: [A], providers: [D] })\n\
         export class M {}\n\
         class A {} class B {} class C {} class D {}",
    );
    let m = only(&f).ng_module.as_ref().unwrap();
    assert_eq!(m.declarations.as_deref(), Some(&["A".to_string(), "B".to_string()][..]));
    assert_eq!(m.imports.as_deref(), Some(&["C".to_string()][..]));
    assert_eq!(m.exports.as_deref(), Some(&["A".to_string()][..]));
    assert_eq!(m.providers.as_ref().unwrap().text, "[D]");
}

#[test]
fn constructor_attribute_and_qualifiers() {
    let f = snippet(
        "import {Directive, Attribute, Optional, Self, Host, SkipSelf} from '@angular/core';\n\
         @Directive({selector: '[x]'})\n\
         export class D {\n\
           constructor(@Attribute('name') n: string, @Optional() @Self() s: Svc, @Host() @SkipSelf() h: Svc) {}\n\
         }\n\
         class Svc {}",
    );
    let deps = only(&f).constructor_deps.clone().unwrap();
    assert_eq!(deps[0].attribute.as_deref(), Some("name"));
    assert_eq!(deps[0].token.as_ref().unwrap().text, "\"name\"");
    assert!(deps[1].optional && deps[1].self_ && !deps[1].host);
    assert!(deps[2].host && deps[2].skip_self);
}

// ---------------------------------------------------------------------------
// Targeted shapes from the assessment list and the parity findings.
// ---------------------------------------------------------------------------

#[test]
fn multi_decorator_class_fills_every_section() {
    let f = snippet(
        "import {Pipe, Injectable} from '@angular/core';\n\
         @Pipe({name: 'twice'}) @Injectable({providedIn: 'root'})\n\
         export class TwicePipe { transform(v: number) { return v * 2; } }",
    );
    let c = only(&f);
    assert_eq!(c.pipe.as_ref().unwrap().name.as_deref(), Some("twice"));
    assert_eq!(c.injectable.as_ref().unwrap().provided_in.as_ref().unwrap().text, "'root'");
}

#[test]
fn input_and_output_on_one_member_are_both_reported() {
    let f = snippet(
        "import {Directive, Input, Output, EventEmitter} from '@angular/core';\n\
         @Directive({selector: '[m]'})\n\
         export class M { @Input() @Output() mixed = ''; }",
    );
    let d = only(&f).directive.as_ref().unwrap();
    assert_eq!(d.inputs[0].property, "mixed");
    assert_eq!(d.outputs[0].property, "mixed");
}

#[test]
fn identifier_valued_template_is_evaluated_to_its_string() {
    let f = snippet(
        "import {Component} from '@angular/core';\n\
         const T = '<p>hi</p>';\n\
         @Component({selector: 'x-y', template: T})\n\
         export class C {}",
    );
    let comp = only(&f).component.as_ref().unwrap().clone();
    assert_eq!(comp.template.as_deref(), Some("<p>hi</p>"));
}

#[test]
fn dynamic_template_is_refused_for_fallback() {
    let f = snippet(
        "import {Component} from '@angular/core';\n\
         declare function make(): string;\n\
         @Component({selector: 'x-y', template: make()})\n\
         export class C {}",
    );
    // ng-analyze reports the non-string template as a file-level error, which makes the facade fall back
    // for every class in the file. (If it were attached to the class, the class would carry it instead.)
    let refused = !f.file_errors.is_empty()
        || f.classes.iter().any(|c| !c.unsupported.is_empty() || c.component.as_ref().is_some_and(|x| x.template.is_none() && x.template_url.is_none()));
    assert!(refused, "a dynamic template must not be extracted as a usable component: {f:?}");
}

#[test]
fn non_literal_host_value_is_an_expression() {
    let f = snippet(
        "import {Component} from '@angular/core';\n\
         declare const v: string;\n\
         @Component({selector: 'x-y', template: '', host: {'role': v, 'title': 'ok'}})\n\
         export class C {}",
    );
    let d = only(&f).directive.as_ref().unwrap();
    // The unevaluated entry is kept (as an expression), not dropped as ng-analyze's folded map does.
    let host = d.host_object.as_ref().unwrap();
    assert_eq!(host[0], ("role".to_string(), HostValue::Expr("v".to_string())));
    assert_eq!(host[1], ("title".to_string(), HostValue::Str("ok".to_string())));
}

#[test]
fn spans_are_byte_offsets_after_multibyte_text() {
    // A 4-byte and a 2-byte character before the class: UTF-16 offsets differ from byte offsets.
    let src = "import {Component} from '@angular/core';\nconst s = '𝄞é';\n@Component({selector: 'x-y', template: '<p>é𝄞</p>'})\nexport class Accent {}\n";
    let f = snippet(src);
    let c = only(&f);
    // The class span starts at the `class` keyword (after `export` and the decorator), the same byte
    // Treaty's ClassWithDecorators::span reports, which is what the facade matches facts on.
    assert_eq!(c.class_start, src.find("class Accent").unwrap());
    assert_eq!(c.component.as_ref().unwrap().template.as_deref(), Some("<p>é𝄞</p>"));
}

#[test]
fn non_angular_decorators_are_not_facts() {
    let f = snippet("function dec(c: unknown) {}\n@dec export class NotAngular {}\n");
    assert!(f.classes.is_empty());
}

#[test]
fn host_directive_names_are_local_bindings() {
    // A named import is a local binding, usable even when the module is not in the project (the facade
    // supplies no sibling files); a namespace member is not, so it has no name.
    let f = snippet(
        "import {Directive} from '@angular/core';\n\
         import {Base} from './base';\n\
         import * as ns from './ns';\n\
         @Directive({selector: '[h]', hostDirectives: [Base, ns.Dir]})\n\
         export class H {}",
    );
    let names: Vec<String> = only(&f).directive.as_ref().unwrap().host_directives.as_ref().unwrap()
        .iter().map(|h| h.name.clone()).collect();
    assert_eq!(names, vec!["Base".to_string(), String::new()]);
}
