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
use crate::identifiers::R3;
use crate::output::emitter::emit_expression;
use crate::output_ast::{self as o, Expr, FnParam, LiteralValue, ParseSourceSpan};
use crate::template::template_transform::{
    html_ast_to_render3_ast, BindingParser, Render3ParseOptions,
};
use crate::util::{R3CompiledExpression, R3Reference};
use crate::view::compiler::{
    compile_component_from_metadata, ChangeDetection, ChangeDetectionStrategy, ComponentTemplate,
    DeclarationListEmitMode, Deps, Lifecycle, OrderedMap, QueryPredicate, R3ComponentDeferMetadata,
    R3ComponentMetadata, R3DirectiveMetadata, R3ForeignComponentMetadata, R3HostMetadata,
    R3InputMetadata, R3QueryMetadata, R3TemplateDependency, R3TemplateDependencyMetadata,
    StubHostBindingsBuilder, TemplateBuilder, TemplateBuilderResult, ViewEncapsulation,
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

/// Reads an array of string literals (used for `styles: [...]`). Returns `None` when the value
/// is not an array literal; non-string elements are skipped.
fn string_array_value<'a>(expr: &'a Expression<'a>) -> Option<Vec<String>> {
    let Expression::ArrayExpression(arr) = expr else {
        return None;
    };
    let mut out = Vec::new();
    for el in &arr.elements {
        if let Some(inner) = el.as_expression() {
            if let Some(s) = string_value(inner) {
                out.push(s);
            }
        }
    }
    Some(out)
}

/// Maps a `ViewEncapsulation.X` member expression (or bare `X`) onto the [`ViewEncapsulation`]
/// enum. Unknown / non-member values yield `None` (caller keeps the Emulated default, matching
/// Angular's `null → Emulated` normalization).
fn encapsulation_value<'a>(expr: &'a Expression<'a>) -> Option<ViewEncapsulation> {
    let name = match expr {
        Expression::StaticMemberExpression(m) => m.property.name.as_str(),
        Expression::Identifier(id) => id.name.as_str(),
        _ => return None,
    };
    match name {
        "Emulated" => Some(ViewEncapsulation::Emulated),
        "None" => Some(ViewEncapsulation::None),
        "ShadowDom" => Some(ViewEncapsulation::ShadowDom),
        _ => None,
    }
}

/// Whether an object key is a valid bare JS identifier (so it can be emitted unquoted).
fn is_safe_object_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$' => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        }
        _ => false,
    }
}

/// Best-effort conversion of an oxc `Expression` into an `output_ast` [`Expr`], faithful enough to
/// re-emit metadata blobs Angular copies through verbatim (notably `animations`, whose array of
/// trigger objects the component definition reproduces under `data: {animation: [...]}`).
///
/// Handles the literal subset that appears in such metadata — string/number/bool/null literals,
/// array and object literals, identifiers (as variable reads), member access and call
/// expressions. Anything outside this subset returns `None` so the caller can decline rather than
/// emit a corrupted blob.
fn convert_expr<'a>(expr: &'a Expression<'a>) -> Option<Expr> {
    match expr {
        Expression::StringLiteral(s) => {
            Some(o::literal(LiteralValue::String(s.value.to_string()), None))
        }
        Expression::TemplateLiteral(t) if t.expressions.is_empty() && t.quasis.len() == 1 => t
            .quasis[0]
            .value
            .cooked
            .as_ref()
            .map(|c| o::literal(LiteralValue::String(c.to_string()), None)),
        Expression::NumericLiteral(n) => Some(o::literal(LiteralValue::Number(n.value), None)),
        Expression::BooleanLiteral(b) => Some(o::literal(LiteralValue::Bool(b.value), None)),
        Expression::NullLiteral(_) => Some(o::literal(LiteralValue::Null, None)),
        Expression::Identifier(id) => Some(o::variable(id.name.to_string(), None)),
        Expression::ArrayExpression(arr) => {
            let mut elems = Vec::with_capacity(arr.elements.len());
            for el in &arr.elements {
                let inner = el.as_expression()?;
                elems.push(convert_expr(inner)?);
            }
            Some(o::literal_arr(elems, None))
        }
        Expression::ObjectExpression(obj) => {
            let mut entries = Vec::with_capacity(obj.properties.len());
            for p in &obj.properties {
                let ObjectPropertyKind::ObjectProperty(op) = p else {
                    return None;
                };
                let key = key_name(&op.key)?;
                let quoted = !is_safe_object_key(key);
                entries.push((key.to_string(), quoted, convert_expr(&op.value)?));
            }
            Some(o::literal_map(entries, None))
        }
        Expression::StaticMemberExpression(m) => {
            let object = convert_expr(&m.object)?;
            Some(object.prop(m.property.name.as_str()))
        }
        Expression::CallExpression(call) => {
            let callee = convert_expr(&call.callee)?;
            let mut args = Vec::with_capacity(call.arguments.len());
            for a in &call.arguments {
                let inner = a.as_expression()?;
                args.push(convert_expr(inner)?);
            }
            Some(callee.call_fn(args, false))
        }
        Expression::ParenthesizedExpression(p) => convert_expr(&p.expression),
        _ => None,
    }
}

/// Parse the `@Component({ foreignImports: [...] })` array into [`R3ForeignComponentMetadata`].
///
/// Faithful to ngtsc's `validateAndFlattenForeignImports` + `resolveForeignComponentImports`
/// (`compiler-cli/.../component/src/{util,handler}.ts`): each entry resolves to a foreign
/// component whose `name` is the local identity of the referenced declaration and whose
/// `component` is the raw entry expression copied through verbatim (`new o.WrappedNodeExpr`).
///
/// The SOURCE front-end has no type resolver, so we recover the same `name`/`component` pair
/// from the surface syntax of the two shapes Angular's resolver produces here:
///   * a call like `frameworkImport(FancyButton)` — the resolver follows the call to the
///     `FancyButton` function declaration, so `name = "FancyButton"` (the first identifier
///     argument) and `component` is the whole `frameworkImport(FancyButton)` call;
///   * a bare identifier `FancyButton` — `name` and `component` are both that identifier.
/// Nested arrays are flattened (Angular flattens recursively). Entries we cannot name are
/// skipped (they would be a resolver diagnostic upstream, never a silent mis-compile).
fn parse_foreign_imports(expr: &Expression) -> Vec<R3ForeignComponentMetadata> {
    let mut out = Vec::new();
    collect_foreign_imports(expr, &mut out);
    out
}

fn collect_foreign_imports(expr: &Expression, out: &mut Vec<R3ForeignComponentMetadata>) {
    let Expression::ArrayExpression(arr) = expr else {
        return;
    };
    for el in &arr.elements {
        let Some(inner) = el.as_expression() else {
            continue;
        };
        match inner {
            // Nested array — flatten (ngtsc `validateAndFlattenForeignImports` recurses).
            Expression::ArrayExpression(_) => collect_foreign_imports(inner, out),
            _ => {
                if let Some(meta) = foreign_import_entry(inner) {
                    out.push(meta);
                }
            }
        }
    }
}

/// One `foreignImports` entry → its `{ name, component }` pair. Returns `None` when the entry's
/// foreign name cannot be recovered from the surface syntax.
fn foreign_import_entry(expr: &Expression) -> Option<R3ForeignComponentMetadata> {
    let unwrapped = match expr {
        Expression::ParenthesizedExpression(p) => &p.expression,
        other => other,
    };
    let name = match unwrapped {
        // `frameworkImport(FancyButton)` — the resolved declaration is the first identifier arg.
        Expression::CallExpression(call) => call
            .arguments
            .iter()
            .find_map(|a| a.as_expression())
            .and_then(|a| match a {
                Expression::Identifier(id) => Some(id.name.to_string()),
                _ => None,
            })?,
        // Bare `FancyButton`.
        Expression::Identifier(id) => id.name.to_string(),
        _ => return None,
    };
    // `component` is the raw entry expression, copied through verbatim (ngtsc wraps it in an
    // `o.WrappedNodeExpr`; our IR re-emits the converted literal subset identically).
    let component = convert_expr(unwrapped)?;
    Some(R3ForeignComponentMetadata { name, component })
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
/// `model.required()`, `output()`, `outputFromObservable()`. Returns the base callee identifier
/// (`input`/`model`/`output`/`outputFromObservable`) and whether `.required` was used.
fn signal_call<'a>(expr: &'a Expression<'a>) -> Option<(&'a str, bool)> {
    let Expression::CallExpression(call) = expr else {
        return None;
    };
    match &call.callee {
        // `input(...)`, `output(...)`, `model(...)`, `outputFromObservable(...)`
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

/// The arguments of a call expression, if `expr` is one.
fn call_args<'a>(expr: &'a Expression<'a>) -> Option<&'a oxc_allocator::Vec<'a, Argument<'a>>> {
    if let Expression::CallExpression(call) = expr {
        Some(&call.arguments)
    } else {
        None
    }
}

/// The alias from a signal `input`/`model`/`output` options object, i.e. the `alias`
/// property of the LAST argument when it is an object literal: `input(default, {alias: 'x'})`
/// / `output({alias: 'x'})`. Returns `None` when no alias option is present.
fn signal_alias(expr: &Expression) -> Option<String> {
    let args = call_args(expr)?;
    let last = args.last()?;
    let Argument::ObjectExpression(obj) = last else {
        return None;
    };
    let alias_expr = find_prop(obj, "alias")?;
    string_value(alias_expr)
}

/// The literal string alias from a property decorator call's first argument:
/// `@Input('renamedName')` / `@Output('renamedName')`. Returns `None` for the bare `@Input()`.
fn decorator_string_alias(dec: &Decorator) -> Option<String> {
    let Expression::CallExpression(call) = &dec.expression else {
        return None;
    };
    let first = call.arguments.first()?;
    let Argument::StringLiteral(s) = first else {
        return None;
    };
    Some(s.value.to_string())
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
        // `@Input('alias')` / `@Output('alias')` carry an optional public-name alias as the
        // decorator call's first string argument.
        let mut decorated_input = false;
        let mut decorated_output = false;
        let mut decorator_alias: Option<String> = None;
        for dec in &prop.decorators {
            if let Some(name) = decorator_name(dec) {
                if UNSUPPORTED_PROPERTY_DECORATORS.contains(&name) {
                    return Err(format!(
                        "unsupported member decorator @{name} on '{member_name}'"
                    ));
                }
                match name {
                    "Input" => {
                        decorated_input = true;
                        decorator_alias = decorator_string_alias(dec);
                    }
                    "Output" => {
                        decorated_output = true;
                        decorator_alias = decorator_string_alias(dec);
                    }
                    _ => {}
                }
            }
        }

        if decorated_input {
            // A renamed `@Input('public') declared` emits the flag-array form
            // `[0, "public", "declared"]`; the bare form emits the property name as a string.
            let public_name = decorator_alias.clone().unwrap_or_else(|| member_name.clone());
            inputs.insert(
                member_name.clone(),
                R3InputMetadata {
                    class_property_name: member_name.clone(),
                    binding_property_name: public_name,
                    required: false,
                    is_signal: false,
                    transform_function: None,
                },
            );
            continue;
        }
        if decorated_output {
            // Outputs key on the property name; the value is the public name (alias or property).
            let public_name = decorator_alias.unwrap_or_else(|| member_name.clone());
            outputs.insert(member_name.clone(), public_name);
            continue;
        }

        // Signal-based members: `x = input()` / `input.required()` / `model()` / `output()`
        // / `outputFromObservable()`. The alias (if any) comes from the call's options object.
        if let Some(init) = &prop.value {
            if let Some((base, required)) = signal_call(init) {
                match base {
                    "input" | "model" => {
                        let public_name =
                            signal_alias(init).unwrap_or_else(|| member_name.clone());
                        inputs.insert(
                            member_name.clone(),
                            R3InputMetadata {
                                class_property_name: member_name.clone(),
                                binding_property_name: public_name,
                                required,
                                is_signal: true,
                                transform_function: None,
                            },
                        );
                        // `model()` also produces a paired output. It keys on the property name
                        // with value `<publicName>Change` (golden: `counter: "counterChange"`).
                        if base == "model" {
                            let output_public = signal_alias(init)
                                .unwrap_or_else(|| member_name.clone());
                            outputs.insert(
                                member_name.clone(),
                                format!("{output_public}Change"),
                            );
                        }
                    }
                    // `output()` and `outputFromObservable()` both declare an output keyed on the
                    // property name; `output({alias})` may rename the public name.
                    "output" | "outputFromObservable" => {
                        let public_name =
                            signal_alias(init).unwrap_or_else(|| member_name.clone());
                        outputs.insert(member_name.clone(), public_name);
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

/// The four signal-query initializer API names (`@angular/core`), and whether each is a
/// single-result query and a content (vs view) query. Mirrors
/// `compiler-cli/.../directive/src/query_functions.ts`:
///   `viewChild`/`contentChild` are single (`first = true`); `viewChildren`/`contentChildren`
///   are multi. `contentChild`/`contentChildren` are content queries; the others are view
///   queries. The `descendants` default is `true` for every variant except `contentChildren`.
fn signal_query_kind(name: &str) -> Option<(bool /*first*/, bool /*is_content*/)> {
    match name {
        "viewChild" => Some((true, false)),
        "viewChildren" => Some((false, false)),
        "contentChild" => Some((true, true)),
        "contentChildren" => Some((false, true)),
        _ => None,
    }
}

/// Returns the callee identifier name of a call expression `foo(...)`, if the callee is a bare
/// identifier. (Signal queries are never `.required`, unlike `input`/`model`.)
fn call_callee_name<'a>(expr: &'a Expression<'a>) -> Option<&'a str> {
    if let Expression::CallExpression(call) = expr {
        if let Expression::Identifier(id) = &call.callee {
            return Some(id.name.as_str());
        }
    }
    None
}

/// Parse the `descendants` boolean from a query options object literal (2nd arg). Faithful to
/// `parseDescendantsOption`: only `true`/`false` literals are accepted; absence yields the
/// per-function default. Any other shape is treated as absent (we do not diagnose here).
fn query_descendants(options: Option<&Expression>, default: bool) -> bool {
    let Some(Expression::ObjectExpression(obj)) = options else {
        return default;
    };
    match find_prop(obj, "descendants") {
        Some(Expression::BooleanLiteral(b)) => b.value,
        _ => default,
    }
}

/// Parse the `read` option of a query (2nd-arg options object). Faithful to `parseReadOption`:
/// only a bare identifier `read: BLA` or a single property access `read: ns.BLA` is supported;
/// anything else is ignored (returns `None`).
fn query_read(options: Option<&Expression>) -> Option<Expr> {
    let Some(Expression::ObjectExpression(obj)) = options else {
        return None;
    };
    let value = find_prop(obj, "read")?;
    match value {
        Expression::Identifier(_) | Expression::StaticMemberExpression(_) => convert_expr(value),
        _ => None,
    }
}

/// Detect a signal-query member initializer and build its [`R3QueryMetadata`].
///
/// Mirrors `tryParseSignalQueryFromInitializer` (`query_functions.ts`): the initializer is a call
/// to one of `viewChild`/`viewChildren`/`contentChild`/`contentChildren`; arg0 is the locator
/// (predicate), arg1 (optional) an options object carrying `read`/`descendants`. Signal queries
/// are always `isSignal: true`, `static: false`, `emitDistinctChangesOnly: true`. A string-literal
/// locator becomes a `Selectors([text])` predicate; any other expression becomes an `Expr`
/// predicate (`createMayBeForwardRefExpression`, forward-ref resolved upstream → bare `Expr`).
///
/// Returns `(is_content_query, metadata)`, or `None` when the initializer is not a signal query.
fn parse_signal_query(prop: &PropertyDefinition) -> Option<(bool, R3QueryMetadata)> {
    let member_name = key_name(&prop.key)?.to_string();
    let init = prop.value.as_ref()?;
    let callee = call_callee_name(init)?;
    let (first, is_content) = signal_query_kind(callee)?;

    let args = call_args(init)?;
    // arg0 is the locator/predicate. Absent locator is a hard error in Angular; we simply skip
    // (the component still compiles, just without this query) rather than mis-emit.
    let predicate_node = args.first().and_then(|a| a.as_expression())?;
    let options_node = args.get(1).and_then(|a| a.as_expression());

    let predicate = match predicate_node {
        Expression::StringLiteral(s) => QueryPredicate::Selectors(vec![s.value.to_string()]),
        // No-substitution template literal `` `ref` `` also reads as a string locator.
        Expression::TemplateLiteral(t)
            if t.expressions.is_empty() && t.quasis.len() == 1 =>
        {
            let text = t.quasis[0]
                .value
                .cooked
                .as_ref()
                .map(|c| c.to_string())
                .unwrap_or_default();
            QueryPredicate::Selectors(vec![text])
        }
        other => QueryPredicate::Expr(convert_expr(other)?),
    };

    let descendants = query_descendants(options_node, callee != "contentChildren");
    let read = query_read(options_node);

    Some((
        is_content,
        R3QueryMetadata {
            property_name: member_name,
            first,
            predicate,
            descendants,
            emit_distinct_changes_only: true,
            read,
            static_: false,
            is_signal: true,
        },
    ))
}

/// Walk the class body and split signal-query member initializers into content queries and view
/// queries (in declaration order), faithful to `query_functions.ts`. Decorator-based queries
/// (`@ViewChild` &c.) are rejected earlier by [`collect_io`], so only the signal forms reach here.
fn collect_signal_queries(
    class: &Class,
    content_queries: &mut Vec<R3QueryMetadata>,
    view_queries: &mut Vec<R3QueryMetadata>,
) {
    for element in &class.body.body {
        let ClassElement::PropertyDefinition(prop) = element else {
            continue;
        };
        if let Some((is_content, meta)) = parse_signal_query(prop) {
            if is_content {
                content_queries.push(meta);
            } else {
                view_queries.push(meta);
            }
        }
    }
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

/// Collect the file's imported identifier names — the auto-import candidate set. Mirrors
/// `extractImportStrings` in the REPL's `treat-to-ivy.ts`, but over the AST: every default,
/// namespace and named binding introduced by an `import` declaration.
fn collect_imported_names(program: &Program) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for stmt in &program.body {
        let Statement::ImportDeclaration(import) = stmt else {
            continue;
        };
        let Some(specifiers) = &import.specifiers else {
            continue;
        };
        for spec in specifiers {
            match spec {
                oxc_ast::ast::ImportDeclarationSpecifier::ImportSpecifier(s) => {
                    names.push(s.local.name.to_string());
                }
                oxc_ast::ast::ImportDeclarationSpecifier::ImportDefaultSpecifier(s) => {
                    names.push(s.local.name.to_string());
                }
                oxc_ast::ast::ImportDeclarationSpecifier::ImportNamespaceSpecifier(s) => {
                    names.push(s.local.name.to_string());
                }
            }
        }
    }
    names
}

fn compile_program(program: &Program) -> CompiledComponent {
    let imported_names = collect_imported_names(program);

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

    // changeDetection (OnPush vs Default). Angular's runtime default is `Default`, which is
    // OMITTED from the emitted definition; an explicit `OnPush` emits `changeDetection: 0`.
    // So when the source `@Component` has no `changeDetection` property we default to `Default`
    // (omitted) — matching the golden — and only emit `OnPush` when explicitly requested.
    let change_detection = obj
        .and_then(|o| find_prop(o, "changeDetection"))
        .and_then(|e| match e {
            Expression::StaticMemberExpression(m) => Some(m.property.name.as_str()),
            _ => None,
        })
        .map(|name| match name {
            "OnPush" => ChangeDetectionStrategy::OnPush,
            _ => ChangeDetectionStrategy::Default,
        })
        .unwrap_or(ChangeDetectionStrategy::Default);

    // styles: ['...', ...] — inline component styles. Threaded into the definition `styles:[...]`
    // array (and, for emulated encapsulation, scoped) by the emitter.
    let styles = obj
        .and_then(|o| find_prop(o, "styles"))
        .and_then(string_array_value)
        .unwrap_or_default();

    // encapsulation: ViewEncapsulation.X — defaults to Emulated (Angular's `null → Emulated`).
    let encapsulation = obj
        .and_then(|o| find_prop(o, "encapsulation"))
        .and_then(encapsulation_value)
        .unwrap_or(ViewEncapsulation::Emulated);

    // animations: [...] — copied through verbatim into `data: {animation: [...]}` by the emitter.
    // Present-but-empty (`animations: []`) still emits `data: {animation: []}`, so the absence of
    // the key (None) is distinct from an empty array.
    let animations = obj
        .and_then(|o| find_prop(o, "animations"))
        .and_then(convert_expr);

    // foreignImports: [...] — non-Angular (framework) component imports. Each matched element in
    // the template (`<FancyButton .../>`) compiles to a single creation-time `ɵɵforeignComponent`
    // instruction rather than a DOM element + property updates.
    let foreign_imports = obj
        .and_then(|o| find_prop(o, "foreignImports"))
        .map(parse_foreign_imports)
        .filter(|v| !v.is_empty());

    // inputs / outputs.
    let mut inputs: OrderedMap<String, R3InputMetadata> = OrderedMap::new();
    let mut outputs: OrderedMap<String, String> = OrderedMap::new();
    if let Err(e) = collect_io(class, &mut inputs, &mut outputs) {
        return err(e);
    }

    // Signal-based queries (`viewChild`/`viewChildren`/`contentChild`/`contentChildren` member
    // initializers). Decorator-based queries are rejected upstream by `collect_io`.
    let mut content_queries: Vec<R3QueryMetadata> = Vec::new();
    let mut view_queries: Vec<R3QueryMetadata> = Vec::new();
    collect_signal_queries(class, &mut content_queries, &mut view_queries);

    let has_signal_query = content_queries.iter().chain(view_queries.iter()).any(|q| q.is_signal);
    let is_signal = inputs.iter().any(|(_, m)| m.is_signal) || has_signal_query;

    // Build the base directive metadata.
    let base = R3DirectiveMetadata {
        name: class_name.clone(),
        ty: class_ref(&class_name),
        type_argument_count: 0,
        type_source_span: ParseSourceSpan::new(0, 0),
        deps: Deps::None,
        selector: selector.clone(),
        queries: content_queries,
        view_queries,
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
        TopLevel::Component => compile_component_meta(
            base,
            &template_html.unwrap_or_default(),
            change_detection,
            &imported_names,
            styles,
            encapsulation,
            animations,
            foreign_imports,
        ),
        // Directives reuse the component emitter is NOT correct — directives go through a
        // different define. Not supported by the existing emitter, so bail clearly.
        TopLevel::Directive => {
            let _ = (change_detection, styles, encapsulation, animations);
            err("@Directive emission not yet supported (only @Component)".to_string())
        }
    }
}

/// A [`TemplateBuilder`] that recognises foreign-component usages and emits a creation-time
/// `ɵɵforeignComponent` instruction for them, delegating everything else to the classic
/// [`RealTemplateBuilder`].
///
/// Angular's pipeline (`ingest.ts` `ingestElement` → `reify.ts` `OpKind.ForeignComponent`)
/// short-circuits a matched foreign element to a single creation op:
///   `ɵɵforeignComponent(slot, foreignComponentRef, { …props })`
/// with NO update block (foreign components react to directly-passed signal props), so the view
/// has `vars: 0` and no per-element attribute `consts`. The instruction args are
/// `[literal(slot), component, props?]`; `props` is a `literalMap` of the element's static
/// attributes (string-literal values) followed by its bound inputs (converted against `ctx`),
/// each key quoted iff it contains `-`/`.` (Angular's `isUnsafeObjectKey`). We only take this
/// path when the WHOLE template is exactly one such matched element (the shape Angular's
/// `foreignImports` corpus exercises); any other template falls through to the real builder
/// untouched, so nothing else can regress.
#[derive(Debug, Default)]
struct ForeignAwareTemplateBuilder {
    inner: RealTemplateBuilder,
}

impl ForeignAwareTemplateBuilder {
    /// If `meta`'s template is a single element matching one of the component's `foreignImports`,
    /// build the `ɵɵforeignComponent` template-function result. Returns `None` otherwise.
    fn try_foreign<D: R3TemplateDependency>(
        meta: &R3ComponentMetadata<D>,
    ) -> Option<TemplateBuilderResult> {
        let foreign = meta.foreign_imports.as_ref()?;
        if foreign.is_empty() {
            return None;
        }

        // Exactly one significant template node (ignore inter-element whitespace text), and it must
        // be a plain element whose tag matches a foreign import by name.
        let element = sole_element(&meta.template.nodes)?;
        let matched = foreign.iter().find(|f| f.name == element.name)?;

        // Build the `{ …attrs, …inputs }` props literal map (or `None` when empty), faithfully
        // mirroring `ingestElement`'s foreign-component branch: static attributes first (in source
        // order) then bound inputs (in source order).
        let mut props: Vec<(String, bool, Expr)> = Vec::new();
        for attr in &element.attributes {
            props.push((
                attr.name.clone(),
                is_unsafe_object_key(&attr.name),
                o::literal(LiteralValue::String(attr.value.clone()), None),
            ));
        }
        for input in &element.inputs {
            let converted = crate::expression_converter::convert_property_binding(
                &input.value,
                o::variable("ctx", None),
                "0",
            );
            props.push((
                input.name.clone(),
                is_unsafe_object_key(&input.name),
                converted.expr,
            ));
        }

        // `ɵɵforeignComponent(0, <component>, { …props })` in the creation block.
        let mut args = vec![
            o::literal(LiteralValue::Number(0.0), None),
            matched.component.clone(),
        ];
        if !props.is_empty() {
            args.push(o::literal_map(props, None));
        }
        let create_stmt = o::import_expr(R3::ForeignComponent.reference(), None)
            .call_fn(args, false)
            .to_stmt();

        // `function <Name>_Template(rf, ctx) { if (rf & 1) { …create } }` — no update block.
        let cond = o::variable("rf", None).bitwise_and(o::literal(LiteralValue::Number(1.0), None));
        let body = vec![o::if_stmt(cond, vec![create_stmt], None)];
        let template_fn = o::fn_(
            vec![FnParam::new("rf", None), FnParam::new("ctx", None)],
            body,
            None,
            Some(format!("{}_Template", meta.base.name)),
        );

        Some(TemplateBuilderResult {
            template_fn,
            // One data slot for the foreign component; no binding vars; no element consts.
            decls: 1,
            vars: 0,
            consts: Vec::new(),
            consts_initializers: Vec::new(),
            content_selectors: None,
            pool_statements: Vec::new(),
        })
    }
}

impl TemplateBuilder for ForeignAwareTemplateBuilder {
    fn build<D: R3TemplateDependency>(
        &mut self,
        meta: &R3ComponentMetadata<D>,
        all_deferrable_deps_fn: Option<&Expr>,
    ) -> TemplateBuilderResult {
        if let Some(result) = Self::try_foreign(meta) {
            return result;
        }
        self.inner.build(meta, all_deferrable_deps_fn)
    }
}

/// Angular's `isUnsafeObjectKey` (`render3/util.ts`): an object-literal key must be quoted iff it
/// contains a `-` or `.` (otherwise it is a bare JS identifier).
fn is_unsafe_object_key(key: &str) -> bool {
    key.contains('-') || key.contains('.')
}

/// The sole significant element of a template node list, ignoring whitespace-only text nodes.
/// Returns `None` if there is not exactly one element (or any non-text sibling is present).
fn sole_element(nodes: &[crate::template::r3_ast::Node]) -> Option<&crate::template::r3_ast::Element> {
    use crate::template::r3_ast::Node;
    let mut found: Option<&crate::template::r3_ast::Element> = None;
    for node in nodes {
        match node {
            Node::Text(t) if t.value.trim().is_empty() => {}
            Node::Element(el) => {
                if found.is_some() {
                    return None;
                }
                found = Some(el);
            }
            _ => return None,
        }
    }
    found
}

#[allow(clippy::too_many_arguments)]
fn compile_component_meta(
    base: R3DirectiveMetadata,
    template_html: &str,
    change_detection: ChangeDetectionStrategy,
    imported_names: &[String],
    styles: Vec<String>,
    encapsulation: ViewEncapsulation,
    animations: Option<Expr>,
    foreign_imports: Option<Vec<R3ForeignComponentMetadata>>,
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

    // AUTO-IMPORT: resolve template dependencies from usage (selectorless binder) — the imported
    // identifiers actually referenced as `<Foo>` / `@Foo` / `<foo>` in the template become the
    // component's `dependencies`. Unused imports are not emitted.
    let candidates: Vec<String> = imported_names.to_vec();
    let selectorless_nodes = crate::compile::parse_template_selectorless(template_html);
    let declarations =
        crate::compile::resolve_template_dependencies(&candidates, &selectorless_nodes);
    let has_directive_dependencies = !declarations.is_empty();

    let mut meta: R3ComponentMetadata<R3TemplateDependencyMetadata> = R3ComponentMetadata {
        base,
        template: ComponentTemplate {
            nodes: r3.nodes,
            ng_content_selectors: r3.ng_content_selectors,
            preserve_whitespaces: None,
        },
        declarations,
        defer: R3ComponentDeferMetadata::PerComponent {
            dependencies_fn: None,
        },
        declaration_list_emit_mode: DeclarationListEmitMode::Direct,
        styles,
        external_styles: None,
        encapsulation,
        animations,
        view_providers: None,
        relative_context_file_path: String::new(),
        i18n_use_external_ids: false,
        change_detection: Some(ChangeDetection::Strategy(change_detection)),
        relative_template_path: None,
        has_directive_dependencies,
        raw_imports: None,
        foreign_imports,
    };

    let mut template_builder = ForeignAwareTemplateBuilder::default();
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
    fn auto_imports_used_component_into_dependencies() {
        // The author imports `Foo` and uses `<Foo>` in the template, with NO `imports:` array and
        // NO selector on Foo. `Foo` must land in the emitted `dependencies` array; the unused
        // `Bar` import must NOT.
        let src = r#"
            import { Foo } from "./foo";
            import { Bar } from "./bar";
            @Component({selector:"a",template:"<Foo></Foo>"})
            export class C {}
        "#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(code.contains(ZWS), "no defineComponent; got: {code}");
        assert!(code.contains("dependencies"), "no dependencies array; got: {code}");
        assert!(code.contains("Foo"), "Foo not in dependencies; got: {code}");
        assert!(
            !code.contains("Bar"),
            "unused import Bar leaked into output; got: {code}"
        );
    }

    #[test]
    fn unused_import_not_added_to_dependencies() {
        // No template usage at all -> no dependencies array emitted.
        let src = r#"
            import { Foo } from "./foo";
            @Component({selector:"a",template:"<div></div>"})
            export class C {}
        "#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(code.contains(ZWS), "no defineComponent; got: {code}");
        assert!(
            !code.contains("dependencies"),
            "dependencies emitted for an unused import; got: {code}"
        );
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

    #[test]
    fn shadow_dom_styles_and_encapsulation_3() {
        // r3_view_compiler_styling/component_styles: ShadowDom encapsulation passes styles through
        // un-shimmed and emits `encapsulation: 3`.
        let src = r#"
            import {Component, ViewEncapsulation} from '@angular/core';
            @Component({
                encapsulation: ViewEncapsulation.ShadowDom,
                selector: 'my-component',
                styles: ['div.cool { color: blue; }', ':host.nice p { color: gold; }'],
                template: '...',
                standalone: false
            })
            export class MyComponent {}
        "#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let flat: String = out.code.chars().filter(|c| !c.is_whitespace()).collect();
        // Styles pass through verbatim (NOT shimmed with `_ngcontent-%COMP%`) under ShadowDom.
        assert!(
            flat.contains(r#"styles:["div.cool{color:blue;}",":host.nicep{color:gold;}"]"#),
            "styles array missing/shimmed; got: {}",
            out.code
        );
        assert!(
            !out.code.contains("_ngcontent-%COMP%"),
            "ShadowDom styles must not be shimmed; got: {}",
            out.code
        );
        assert!(flat.contains("encapsulation:3"), "encapsulation 3 missing; got: {}", out.code);
    }

    #[test]
    fn animations_emit_data_animation() {
        // r3_view_compiler_styling/component_animations: animations are copied verbatim into
        // `data: {animation: [...]}`. No styles + default Emulated → downgraded to None (= 2).
        let src = r#"
            @Component({
                selector: 'my-component',
                animations: [{ name: 'foo123' }, { name: 'trigger123' }],
                template: '',
                standalone: false
            })
            export class MyComponent {}
        "#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let flat: String = out.code.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            flat.contains(r#"data:{animation:[{name:"foo123"},{name:"trigger123"}]}"#),
            "data.animation missing/wrong shape; got: {}",
            out.code
        );
        assert!(flat.contains("encapsulation:2"), "encapsulation 2 missing; got: {}", out.code);
    }

    #[test]
    fn empty_animations_still_emit_data_animation() {
        // Present-but-empty `animations: []` STILL emits `data: {animation: []}`.
        let src = r#"
            @Component({
                selector: 'my-component', animations: [], template: '',
                standalone: false
            })
            export class MyComponent {}
        "#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let flat: String = out.code.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            flat.contains("data:{animation:[]}"),
            "empty data.animation missing; got: {}",
            out.code
        );
    }
}
