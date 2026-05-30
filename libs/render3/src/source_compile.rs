//! `@Component`/`@Directive` SOURCE front-end.
//!
//! Parses a TypeScript source string with `oxc_parser`, finds the single class carrying an
//! `@Component` (or `@Directive`) decorator, extracts the *common-case* metadata
//! (class name, selector, inline `template`, `standalone`, `changeDetection`, inputs, outputs)
//! into [`R3ComponentMetadata`], and drives the existing
//! [`crate::view::compiler::compile_component_from_metadata`] emitter (via the same
//! [`crate::compile::RealTemplateBuilder`] glue used by [`crate::compile::compile_component`]).
//!
//! Metadata kinds that are NOT yet extractable (`providers`, `viewProviders`, `@ViewChild`/query
//! decorators, `host` bindings, `hostDirectives`, multi-class files, external `templateUrl`)
//! cause this function to return a [`CompiledComponent`] with a clear `errors` entry rather than
//! silently mis-compiling — the harness can then skip/relog those cases.

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, Class, ClassElement, Decorator, Expression, ObjectPropertyKind, Program,
    PropertyDefinition, PropertyKey, Statement,
};
use oxc_parser::Parser;
use oxc_span::SourceType;

use crate::compile::{CompiledComponent, RealTemplateBuilder};
use crate::output::emitter::emit_expression;
use crate::output_ast::{self as o, ParseSourceSpan};
use crate::template::template_transform::{
    html_ast_to_render3_ast, BindingParser, Render3ParseOptions,
};
use crate::util::{R3CompiledExpression, R3Reference};
use crate::view::compiler::{
    compile_component_from_metadata, ChangeDetection, ChangeDetectionStrategy, ComponentTemplate,
    DeclarationListEmitMode, Deps, Lifecycle, OrderedMap, R3ComponentDeferMetadata,
    R3ComponentMetadata, R3DirectiveMetadata, R3HostMetadata, R3InputMetadata,
    R3TemplateDependencyMetadata, StubHostBindingsBuilder, ViewEncapsulation,
};

/// Helper to build a `CompiledComponent` carrying a single fatal error and no code.
fn err(msg: impl Into<String>) -> CompiledComponent {
    CompiledComponent {
        code: String::new(),
        errors: vec![msg.into()],
    }
}

fn class_ref(class_name: &str) -> R3Reference {
    R3Reference {
        value: o::variable(class_name, None),
        ty: o::variable(class_name, None),
    }
}

/// The recognized top-level decorator on the class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TopLevel {
    Component,
    Directive,
}

/// Returns the callee identifier name of a decorator's expression, whether it's a bare
/// `@Foo` (identifier) or a call `@Foo({...})` (call expression). Mirrors the legacy parser's
/// recognition in `apps/rust/authoring/src/angular/decorators`.
fn decorator_name<'a>(dec: &'a Decorator<'a>) -> Option<&'a str> {
    match &dec.expression {
        Expression::CallExpression(call) => match &call.callee {
            Expression::Identifier(id) => Some(id.name.as_str()),
            _ => None,
        },
        Expression::Identifier(id) => Some(id.name.as_str()),
        _ => None,
    }
}

/// Returns the object literal argument of a decorator call `@Foo({...})`, if present.
fn decorator_object<'a>(dec: &'a Decorator<'a>) -> Option<&'a oxc_ast::ast::ObjectExpression<'a>> {
    if let Expression::CallExpression(call) = &dec.expression {
        for arg in &call.arguments {
            if let Argument::ObjectExpression(obj) = arg {
                return Some(obj);
            }
        }
    }
    None
}

/// Reads the static-identifier name of a property key (the common case: `selector`, `template`).
fn key_name<'a>(key: &'a PropertyKey<'a>) -> Option<&'a str> {
    match key {
        PropertyKey::StaticIdentifier(id) => Some(id.name.as_str()),
        PropertyKey::StringLiteral(s) => Some(s.value.as_str()),
        _ => None,
    }
}

/// Reads a string-literal value (used for `selector` / inline `template`).
fn string_value<'a>(expr: &'a Expression<'a>) -> Option<String> {
    match expr {
        Expression::StringLiteral(s) => Some(s.value.to_string()),
        // A no-substitution template literal `` `...` `` (single quasi, no exprs).
        Expression::TemplateLiteral(t) if t.expressions.is_empty() && t.quasis.len() == 1 => {
            t.quasis[0].value.cooked.as_ref().map(|c| c.to_string())
        }
        _ => None,
    }
}

/// Walks an object literal property by name, returning its value expression.
fn find_prop<'a>(
    obj: &'a oxc_ast::ast::ObjectExpression<'a>,
    name: &str,
) -> Option<&'a Expression<'a>> {
    obj.properties.iter().find_map(|p| match p {
        ObjectPropertyKind::ObjectProperty(op) => {
            if key_name(&op.key) == Some(name) {
                Some(&op.value)
            } else {
                None
            }
        }
        _ => None,
    })
}

/// Recognizes a signal-member initializer call: `input()`, `input.required()`, `model()`,
/// `model.required()`, `output()`. Returns the base callee identifier (`input`/`model`/`output`)
/// and whether `.required` was used.
fn signal_call<'a>(expr: &'a Expression<'a>) -> Option<(&'a str, bool)> {
    let Expression::CallExpression(call) = expr else {
        return None;
    };
    match &call.callee {
        // `input(...)`, `output(...)`, `model(...)`
        Expression::Identifier(id) => Some((id.name.as_str(), false)),
        // `input.required(...)`, `model.required(...)`
        Expression::StaticMemberExpression(member) => {
            if let Expression::Identifier(base) = &member.object {
                let required = member.property.name.as_str() == "required";
                Some((base.name.as_str(), required))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Detected metadata that this front-end refuses to mis-compile. Presence of any of these in the
/// decorator object means we return an error so the harness can skip the case.
const UNSUPPORTED_DECORATOR_KEYS: &[&str] = &[
    "providers",
    "viewProviders",
    "host",
    "hostDirectives",
    "queries",
];

/// Property decorators we cannot yet model — their presence on a member is fatal.
const UNSUPPORTED_PROPERTY_DECORATORS: &[&str] = &[
    "ViewChild",
    "ViewChildren",
    "ContentChild",
    "ContentChildren",
    "HostBinding",
    "HostListener",
];

/// Extract inputs/outputs from the class body and populate the metadata maps.
///
/// Recognizes:
///   * `@Input()` members  → input (non-signal)
///   * `@Output()` members → output (non-signal)
///   * `x = input()` / `input.required()` / `model()` → signal input
///   * `x = output()` → output
fn collect_io(
    class: &Class,
    inputs: &mut OrderedMap<String, R3InputMetadata>,
    outputs: &mut OrderedMap<String, String>,
) -> Result<(), String> {
    for element in &class.body.body {
        let ClassElement::PropertyDefinition(prop) = element else {
            continue;
        };
        let prop: &PropertyDefinition = prop;
        let Some(member_name) = key_name(&prop.key).map(|s| s.to_string()) else {
            continue;
        };

        // Decorator-based @Input/@Output (and rejection of unsupported member decorators).
        let mut decorated_input = false;
        let mut decorated_output = false;
        for dec in &prop.decorators {
            if let Some(name) = decorator_name(dec) {
                if UNSUPPORTED_PROPERTY_DECORATORS.contains(&name) {
                    return Err(format!(
                        "unsupported member decorator @{name} on '{member_name}'"
                    ));
                }
                match name {
                    "Input" => decorated_input = true,
                    "Output" => decorated_output = true,
                    _ => {}
                }
            }
        }

        if decorated_input {
            inputs.insert(
                member_name.clone(),
                R3InputMetadata {
                    class_property_name: member_name.clone(),
                    binding_property_name: member_name.clone(),
                    required: false,
                    is_signal: false,
                    transform_function: None,
                },
            );
            continue;
        }
        if decorated_output {
            outputs.insert(member_name.clone(), member_name.clone());
            continue;
        }

        // Signal-based members: `x = input()` / `input.required()` / `model()` / `output()`.
        if let Some(init) = &prop.value {
            if let Some((base, required)) = signal_call(init) {
                match base {
                    "input" | "model" => {
                        inputs.insert(
                            member_name.clone(),
                            R3InputMetadata {
                                class_property_name: member_name.clone(),
                                binding_property_name: member_name.clone(),
                                required,
                                is_signal: true,
                                transform_function: None,
                            },
                        );
                        // `model()` also produces a paired output `<name>Change`.
                        if base == "model" {
                            let change = format!("{member_name}Change");
                            outputs.insert(change.clone(), change);
                        }
                    }
                    "output" => {
                        outputs.insert(member_name.clone(), member_name.clone());
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

/// Compile a single standalone `@Component`/`@Directive` class from its TypeScript source.
///
/// On success the returned [`CompiledComponent::code`] is the emitted `ɵɵdefineComponent({...})`
/// expression. On any unsupported / un-extractable shape, `code` is empty and `errors` carries a
/// single descriptive message.
pub fn compile_component_source(ts_source: &str) -> CompiledComponent {
    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true);
    let ret = Parser::new(&allocator, ts_source, source_type).parse();

    if !ret.errors.is_empty() {
        let msgs: Vec<String> = ret.errors.iter().map(|e| e.to_string()).collect();
        return err(format!("parse error: {}", msgs.join("; ")));
    }

    compile_program(&ret.program)
}

fn compile_program(program: &Program) -> CompiledComponent {
    // Find all classes (top-level + exported) carrying a recognized decorator.
    let mut decorated: Vec<(&Class, TopLevel, &Decorator)> = Vec::new();

    for stmt in &program.body {
        let class_opt: Option<&Class> = match stmt {
            Statement::ClassDeclaration(c) => Some(c.as_ref()),
            Statement::ExportNamedDeclaration(export) => match &export.declaration {
                Some(oxc_ast::ast::Declaration::ClassDeclaration(c)) => Some(c.as_ref()),
                _ => None,
            },
            Statement::ExportDefaultDeclaration(export) => {
                if let oxc_ast::ast::ExportDefaultDeclarationKind::ClassDeclaration(c) =
                    &export.declaration
                {
                    Some(c.as_ref())
                } else {
                    None
                }
            }
            _ => None,
        };

        let Some(class) = class_opt else { continue };
        for dec in &class.decorators {
            if let Some(name) = decorator_name(dec) {
                let kind = match name {
                    "Component" => Some(TopLevel::Component),
                    "Directive" => Some(TopLevel::Directive),
                    _ => None,
                };
                if let Some(kind) = kind {
                    decorated.push((class, kind, dec));
                }
            }
        }
    }

    if decorated.is_empty() {
        return err("no @Component or @Directive decorated class found".to_string());
    }
    if decorated.len() > 1 {
        return err(format!(
            "multi-class files unsupported: found {} decorated classes",
            decorated.len()
        ));
    }

    let (class, kind, dec) = decorated[0];

    let class_name = match &class.id {
        Some(id) => id.name.to_string(),
        None => return err("decorated class has no name".to_string()),
    };

    let obj = decorator_object(dec);

    // Reject decorator-level metadata we cannot yet model.
    if let Some(obj) = obj {
        for k in UNSUPPORTED_DECORATOR_KEYS {
            if find_prop(obj, k).is_some() {
                return err(format!("unsupported @{:?} metadata key: {k}", kind));
            }
        }
        if find_prop(obj, "templateUrl").is_some() {
            return err("external templateUrl unsupported (inline `template` only)".to_string());
        }
    }

    // selector.
    let selector = obj
        .and_then(|o| find_prop(o, "selector"))
        .and_then(string_value);

    // template (components only). Directives have no template.
    let template_html = obj
        .and_then(|o| find_prop(o, "template"))
        .and_then(string_value);

    if kind == TopLevel::Component && template_html.is_none() {
        // A component with a non-string `template` (or none) — bail rather than mis-compile.
        return err("component has no inline string `template`".to_string());
    }

    // standalone (default true).
    let standalone = obj
        .and_then(|o| find_prop(o, "standalone"))
        .map(|e| matches!(e, Expression::BooleanLiteral(b) if b.value))
        .unwrap_or(true);

    // changeDetection (OnPush vs Default; default OnPush keeps output minimal, matching
    // `crate::compile::compile_component`).
    let change_detection = obj
        .and_then(|o| find_prop(o, "changeDetection"))
        .and_then(|e| match e {
            Expression::StaticMemberExpression(m) => Some(m.property.name.as_str()),
            _ => None,
        })
        .map(|name| match name {
            "Default" => ChangeDetectionStrategy::Default,
            _ => ChangeDetectionStrategy::OnPush,
        })
        .unwrap_or(ChangeDetectionStrategy::OnPush);

    // inputs / outputs.
    let mut inputs: OrderedMap<String, R3InputMetadata> = OrderedMap::new();
    let mut outputs: OrderedMap<String, String> = OrderedMap::new();
    if let Err(e) = collect_io(class, &mut inputs, &mut outputs) {
        return err(e);
    }

    let is_signal = inputs.iter().any(|(_, m)| m.is_signal);

    // Build the base directive metadata.
    let base = R3DirectiveMetadata {
        name: class_name.clone(),
        ty: class_ref(&class_name),
        type_argument_count: 0,
        type_source_span: ParseSourceSpan::new(0, 0),
        deps: Deps::None,
        selector: selector.clone(),
        queries: Vec::new(),
        view_queries: Vec::new(),
        host: R3HostMetadata::default(),
        lifecycle: Lifecycle::default(),
        inputs,
        outputs,
        uses_inheritance: false,
        control_create: None,
        export_as: None,
        providers: None,
        is_standalone: standalone,
        is_signal,
        host_directives: None,
        legacy_optional_chaining: false,
    };

    match kind {
        TopLevel::Component => {
            compile_component_meta(base, &template_html.unwrap_or_default(), change_detection)
        }
        // Directives reuse the component emitter is NOT correct — directives go through a
        // different define. Not supported by the existing emitter, so bail clearly.
        TopLevel::Directive => {
            let _ = change_detection;
            err("@Directive emission not yet supported (only @Component)".to_string())
        }
    }
}

fn compile_component_meta(
    base: R3DirectiveMetadata,
    template_html: &str,
    change_detection: ChangeDetectionStrategy,
) -> CompiledComponent {
    let mut errors: Vec<String> = Vec::new();

    // Template HTML -> r3_ast.
    let parse_result = crate::ml_parser::parse(template_html, "template.html");
    for e in &parse_result.errors {
        errors.push(e.msg.clone());
    }

    let mut binding_parser = BindingParser::new();
    let r3 = html_ast_to_render3_ast(
        &parse_result.root_nodes,
        &mut binding_parser,
        Render3ParseOptions::default(),
    );
    for e in &r3.errors {
        errors.push(e.msg.clone());
    }

    let mut meta: R3ComponentMetadata<R3TemplateDependencyMetadata> = R3ComponentMetadata {
        base,
        template: ComponentTemplate {
            nodes: r3.nodes,
            ng_content_selectors: r3.ng_content_selectors,
            preserve_whitespaces: None,
        },
        declarations: Vec::new(),
        defer: R3ComponentDeferMetadata::PerComponent {
            dependencies_fn: None,
        },
        declaration_list_emit_mode: DeclarationListEmitMode::Direct,
        styles: Vec::new(),
        external_styles: None,
        encapsulation: ViewEncapsulation::Emulated,
        animations: None,
        view_providers: None,
        relative_context_file_path: String::new(),
        i18n_use_external_ids: false,
        change_detection: Some(ChangeDetection::Strategy(change_detection)),
        relative_template_path: None,
        has_directive_dependencies: false,
        raw_imports: None,
        foreign_imports: None,
    };

    let mut template_builder = RealTemplateBuilder;
    let mut host_builder = StubHostBindingsBuilder;
    let mut pool_statements = Vec::new();
    let compiled: R3CompiledExpression = compile_component_from_metadata(
        &mut meta,
        &mut template_builder,
        &mut host_builder,
        &mut pool_statements,
    );

    let code = emit_expression(&compiled.expression);
    CompiledComponent { code, errors }
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const ZWS: &str = "\u{0275}\u{0275}defineComponent";

    #[test]
    fn compiles_basic_component_from_source() {
        let src = r#"@Component({selector:"a",template:"<div>{{x}}</div>"}) export class C { x = 1; }"#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(code.contains(ZWS), "no defineComponent; got: {code}");
        // selectors: [["a"]]
        assert!(code.contains("[[\"a\"]]") || code.contains("[[\"a\"]"), "got: {code}");
        // template fn present.
        assert!(code.contains("C_Template"), "no template fn; got: {code}");
        assert!(code.contains("ctx.x"), "template did not bind ctx.x; got: {code}");
    }

    #[test]
    fn signal_input_emits_inputs_entry() {
        let src = r#"@Component({selector:"a",template:"<div></div>"}) export class C { name = input(); }"#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(code.contains(ZWS), "no defineComponent; got: {code}");
        assert!(code.contains("inputs"), "no inputs map; got: {code}");
        assert!(code.contains("name"), "inputs missing 'name'; got: {code}");
    }

    #[test]
    fn decorator_input_output_extracted() {
        let src = r#"@Component({selector:"a",template:"<p></p>"})
            export class C {
                @Input() foo = 1;
                @Output() bar = new EventEmitter();
                baz = output();
            }"#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(code.contains("inputs"), "no inputs; got: {code}");
        assert!(code.contains("foo"), "missing input foo; got: {code}");
        assert!(code.contains("outputs"), "no outputs; got: {code}");
        assert!(code.contains("bar"), "missing output bar; got: {code}");
        assert!(code.contains("baz"), "missing output baz; got: {code}");
    }

    #[test]
    fn unsupported_providers_returns_error() {
        let src = r#"@Component({selector:"a",template:"<p></p>",providers:[X]}) export class C {}"#;
        let out = compile_component_source(src);
        assert!(out.code.is_empty(), "expected no code; got: {}", out.code);
        assert!(!out.errors.is_empty(), "expected an error");
    }

    #[test]
    fn template_url_returns_error() {
        let src = r#"@Component({selector:"a",templateUrl:"./c.html"}) export class C {}"#;
        let out = compile_component_source(src);
        assert!(out.code.is_empty(), "expected no code; got: {}", out.code);
        assert!(
            out.errors.iter().any(|e| e.contains("templateUrl")),
            "expected templateUrl error; got {:?}",
            out.errors
        );
    }
}
