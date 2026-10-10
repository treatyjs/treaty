//! Differential test for the ng-analyze facts path: each snippet is compiled twice in one process, once
//! with the facts path (the default under `--features ng-analyze`) and once with Treaty's own extraction
//! forced (`with_treaty_extraction`). The emitted code and the errors must be identical.
#![cfg(feature = "ng-analyze")]

use treaty_ivy::source_compile::{
    compile_component_source, compile_component_source_with_resolved, ng_analyze_counters,
    reset_ng_analyze_counters, with_treaty_extraction,
};
use treaty_ivy::decorators::registry::{ResolvedComponentContent, ResolvedContentMap};

/// Compiles `src` both ways, asserting identical output, and returns the facts-path code.
fn both_ways(src: &str) -> String {
    let facts = compile_component_source(src);
    let treaty = with_treaty_extraction(|| compile_component_source(src));
    assert_eq!(facts.code, treaty.code, "facts path diverged from Treaty's extraction for:\n{src}");
    assert_eq!(facts.errors, treaty.errors);
    facts.code
}

#[test]
fn decorator_inputs_outputs_and_host_members_match() {
    let code = both_ways(
        "import {Component, Input, Output, EventEmitter, HostBinding, HostListener} from '@angular/core';\n\
         @Component({selector: 'app-a', template: '<p>{{ x }}</p>', host: {'role': 'button', '[class.on]': 'on'}})\n\
         export class A {\n\
           @Input({required: true, alias: 'items'}) rows: string[] = [];\n\
           @Input() title = 'x';\n\
           @Output('picked') pick = new EventEmitter<string>();\n\
           @HostBinding('attr.data-kind') kind = 'stat';\n\
           @HostListener('click', ['$event']) onClick(e: Event) { return e; }\n\
         }",
    );
    assert!(code.contains("items"), "{code}");
}

#[test]
fn signal_inputs_queries_and_explicit_signals_match() {
    both_ways(
        "import {Component, input, model, output, viewChild, contentChildren, ElementRef} from '@angular/core';\n\
         @Component({selector: 'app-s', template: '<span #v></span>', signals: true})\n\
         export class S {\n\
           label = input.required<string>();\n\
           value = input(0, {alias: 'count'});\n\
           selection = model<number>(0);\n\
           reset = output<void>();\n\
           v = viewChild.required<ElementRef>('v');\n\
           items = contentChildren('a, b');\n\
         }",
    );
}

#[test]
fn queries_decorator_form_match() {
    both_ways(
        "import {Component, ViewChild, ViewChildren, ContentChild, ElementRef} from '@angular/core';\n\
         @Component({selector: 'app-q', template: '<div #x></div>'})\n\
         export class Q {\n\
           @ViewChild('x', {static: true}) x!: ElementRef;\n\
           @ViewChildren('a, b') many: unknown;\n\
           @ContentChild(ElementRef) first: unknown;\n\
         }",
    );
}

#[test]
fn constructor_dependencies_and_providers_match() {
    both_ways(
        "import {Component, Inject, Optional, Self, SkipSelf, Host, Attribute, InjectionToken} from '@angular/core';\n\
         export const TOKEN = new InjectionToken<number>('t');\n\
         @Component({selector: 'app-d', template: '', providers: [{provide: TOKEN, useValue: 1}], viewProviders: []})\n\
         export class D {\n\
           constructor(@Attribute('name') n: string, @Inject(TOKEN) @Optional() t: unknown, @Self() @SkipSelf() s: D, @Host() h: D) {}\n\
         }",
    );
}

#[test]
fn encapsulation_change_detection_and_standalone_match() {
    both_ways(
        "import {Component, ViewEncapsulation, ChangeDetectionStrategy} from '@angular/core';\n\
         @Component({selector: 'app-e', template: '<p></p>', encapsulation: ViewEncapsulation.None, changeDetection: ChangeDetectionStrategy.Default, standalone: false, preserveWhitespaces: true})\n\
         export class E {}",
    );
}

#[test]
fn directive_with_host_directives_and_export_as_match() {
    both_ways(
        "import {Directive, Input} from '@angular/core';\n\
         import {Base} from './base';\n\
         @Directive({selector: '[appD]', exportAs: 'd, dd', hostDirectives: [Base, {directive: Base, inputs: ['a: b'], outputs: ['c']}]})\n\
         export class D { @Input() a = 1; }",
    );
}

#[test]
fn animations_and_selectorless_component_match() {
    both_ways(
        "import {Component} from '@angular/core';\n\
         @Component({template: '<p></p>', animations: [{type: 1}]})\n\
         export class NoSelector {}",
    );
}

#[test]
fn external_template_and_styles_resolve_identically() {
    let src = "import {Component} from '@angular/core';\n\
               @Component({selector: 'app-t', templateUrl: './t.html', styleUrls: ['./t.css']})\n\
               export class T {}";
    let mut resolved: ResolvedContentMap = ResolvedContentMap::new();
    resolved.insert(
        "T".to_string(),
        ResolvedComponentContent { template: Some("<p>{{ v }}</p>".to_string()), styles: vec!["p { color: red; }".to_string()] },
    );
    let facts = compile_component_source_with_resolved(src, &resolved);
    let treaty = with_treaty_extraction(|| compile_component_source_with_resolved(src, &resolved));
    assert_eq!(facts.code, treaty.code);
    assert_eq!(facts.errors, treaty.errors);
}

#[test]
fn refused_classes_fall_back_and_are_counted() {
    reset_ng_analyze_counters();
    // `foreignImports` is Treaty-only: the bridge refuses the class, so the facts path falls back.
    let src = "import {Component} from '@angular/core';\n\
               @Component({selector: 'app-f', template: '<p></p>', foreignImports: []})\n\
               export class F {}";
    let facts = compile_component_source(src);
    let treaty = with_treaty_extraction(|| compile_component_source(src));
    assert_eq!(facts.code, treaty.code);
    let counters = ng_analyze_counters();
    assert!(counters.fallbacks.keys().any(|k| k.contains("foreignImports")), "{counters:?}");
}

#[test]
fn facts_path_is_used_for_supported_components() {
    reset_ng_analyze_counters();
    let out = compile_component_source("import {Component} from '@angular/core';\n@Component({selector: 'app-u', template: '<p></p>'})\nexport class U {}");
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert!(ng_analyze_counters().facts_used >= 1);
}
