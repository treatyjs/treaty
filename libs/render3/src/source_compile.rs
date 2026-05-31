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
use oxc_span::{GetSpan, SourceType};

use crate::compile::{CompiledComponent, RealTemplateBuilder};
use crate::factory::{
    compile_factory_function, FactoryDeps, FactoryTarget, R3ConstructorFactoryMetadata,
    R3FactoryMetadata,
};
use crate::identifiers::R3;
use crate::output_ast::{self as o, Expr, FnParam, LiteralValue, ParseSourceSpan};
use crate::template::template_transform::{
    html_ast_to_render3_ast, BindingParser, Render3ParseOptions,
};
use crate::util::{R3CompiledExpression, R3Reference};
use crate::view::compiler::{
    compile_component_from_metadata, compile_directive_from_metadata, parse_host_bindings,
    ChangeDetection, ChangeDetectionStrategy, ComponentTemplate, DeclarationListEmitMode,
    DefaultHostBindingsBuilder, Deps, HostValue, Lifecycle, OrderedMap, QueryPredicate,
    R3ComponentDeferMetadata, R3ComponentMetadata, R3DirectiveMetadata,
    R3ForeignComponentMetadata, R3HostDirectiveMetadata, R3HostMetadata, R3InputMetadata,
    R3QueryMetadata, R3TemplateDependency, R3TemplateDependencyMetadata, TemplateBuilder,
    TemplateBuilderResult, ViewEncapsulation,
};
use crate::util::R3Reference as DirRef;

/// Helper to build a `CompiledComponent` carrying a single fatal error and no code.
fn err(msg: impl Into<String>) -> CompiledComponent {
    CompiledComponent {
        code: String::new(),
        errors: vec![msg.into()],
    }
}

/// Build the class self-reference (`value`/`ty`), stamping the original-source `span` of
/// the class name onto the `value` read. The `value` read is the one cloned into the
/// emitted `type: <ClassName>` field (`view::compiler` `definition_map.set("type", ...)`),
/// so this is the anchor the additive source map uses to map the component definition back
/// to its class declaration. Stamping a span is value-preserving: `ExprMeta.span` does not
/// affect emitted text, so the plain and map paths emit identical bytes.
fn class_ref_spanned(class_name: &str, span: ParseSourceSpan) -> R3Reference {
    let mut value = o::variable(class_name, None);
    value.meta.span = Some(span);
    R3Reference {
        value,
        ty: o::variable(class_name, None),
    }
}

/// The recognized top-level decorator on the class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TopLevel {
    Component,
    Directive,
    Pipe,
    NgModule,
    Injectable,
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
    // The legacy `queries: {...}` decorator-object form is distinct from the `@ViewChild`/
    // `@ContentChild` member decorators handled by `collect_decorator_queries`; still unsupported.
    "queries",
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
                match name {
                    "Input" => {
                        decorated_input = true;
                        decorator_alias = decorator_string_alias(dec);
                    }
                    "Output" => {
                        decorated_output = true;
                        decorator_alias = decorator_string_alias(dec);
                    }
                    // `@ViewChild`/`@ContentChild`/`@HostBinding`/`@HostListener` member decorators
                    // are handled by `collect_decorator_queries` / `collect_member_host_bindings`.
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
/// (`@ViewChild` &c.) are collected separately by [`collect_decorator_queries`].
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

// ---------------------------------------------------------------------------
// R3 — decorator-based queries (`@ViewChild`/`@ViewChildren`/`@ContentChild`/`@ContentChildren`).
// ---------------------------------------------------------------------------

/// The four decorator-query names (`@angular/core`) → `(first, is_content)`. Mirrors ngtsc's
/// `directive/src/{query,shared}.ts`: `ViewChild`/`ContentChild` are single (`first = true`);
/// `ViewChildren`/`ContentChildren` are multi. `ContentChild`/`ContentChildren` are content
/// queries; the `View*` variants are view queries. The legacy decorator `descendants` default is
/// `true` for `ContentChild`/`ViewChild`/`ViewChildren` and `false` for `ContentChildren`
/// (faithful to `extractContentQueriesFromDecorators` / `parseDirectiveDecoratorQueries`).
fn decorator_query_kind(name: &str) -> Option<(bool /*first*/, bool /*is_content*/)> {
    match name {
        "ViewChild" => Some((true, false)),
        "ViewChildren" => Some((false, false)),
        "ContentChild" => Some((true, true)),
        "ContentChildren" => Some((false, true)),
        _ => None,
    }
}

/// The `static` boolean from a query decorator options object (2nd arg). Faithful to ngtsc's
/// `parseQueryStaticness`: only a `true`/`false` literal counts (default `false`); for multi
/// queries `static` is always `false`.
fn decorator_query_static(options: Option<&Expression>) -> bool {
    let Some(Expression::ObjectExpression(obj)) = options else {
        return false;
    };
    matches!(find_prop(obj, "static"), Some(Expression::BooleanLiteral(b)) if b.value)
}

/// Build the [`R3QueryMetadata`] for a single decorator query.
///
/// Mirrors ngtsc's `extractQueryMetadata`: arg0 is the locator (a string of comma-separated
/// reference names, OR a type/expression token); arg1 (optional) an options object carrying
/// `read`/`descendants`/`static`. A string locator splits on `,` into a `Selectors([...])`
/// predicate (each entry trimmed); any other expression becomes an `Expr` predicate. Decorator
/// queries are always `isSignal: false`, `emitDistinctChangesOnly: true`.
fn decorator_query_metadata(
    member_name: &str,
    dec: &Decorator,
) -> Option<(bool /*is_content*/, R3QueryMetadata)> {
    let name = decorator_name(dec)?;
    let (first, is_content) = decorator_query_kind(name)?;
    let Expression::CallExpression(call) = &dec.expression else {
        // A bare `@ViewChild` with no arguments has no locator — skip.
        return None;
    };
    let predicate_node = call.arguments.first().and_then(|a| a.as_expression())?;
    let options_node = call.arguments.get(1).and_then(|a| a.as_expression());

    let predicate = match predicate_node {
        Expression::StringLiteral(s) => split_query_selectors(s.value.as_str()),
        Expression::TemplateLiteral(t) if t.expressions.is_empty() && t.quasis.len() == 1 => {
            let text = t.quasis[0]
                .value
                .cooked
                .as_ref()
                .map(|c| c.to_string())
                .unwrap_or_default();
            split_query_selectors(&text)
        }
        other => QueryPredicate::Expr(convert_expr(other)?),
    };

    // descendants default: ContentChildren → false, the rest → true.
    let descendants = query_descendants(options_node, name != "ContentChildren");
    let read = query_read(options_node);
    // `static` only applies to single-result queries; multi queries are never static.
    let static_ = first && decorator_query_static(options_node);

    Some((
        is_content,
        R3QueryMetadata {
            property_name: member_name.to_string(),
            first,
            predicate,
            descendants,
            emit_distinct_changes_only: true,
            read,
            static_,
            is_signal: false,
        },
    ))
}

/// Split a query string locator (`'a, b, c'`) into a `Selectors([...])` predicate, trimming each
/// reference name (faithful to ngtsc's `node.text.split(',').map(s => s.trim())`).
fn split_query_selectors(text: &str) -> QueryPredicate {
    QueryPredicate::Selectors(text.split(',').map(|s| s.trim().to_string()).collect())
}

/// Walk the class body and collect every `@ViewChild`/`@ViewChildren`/`@ContentChild`/
/// `@ContentChildren` member into the content/view query lists (in declaration order). Both
/// property members (the common case) and accessor/method members carrying the decorator are
/// considered.
fn collect_decorator_queries(
    class: &Class,
    content_queries: &mut Vec<R3QueryMetadata>,
    view_queries: &mut Vec<R3QueryMetadata>,
) {
    for element in &class.body.body {
        let (decorators, key) = match element {
            ClassElement::PropertyDefinition(p) => (&p.decorators, &p.key),
            ClassElement::AccessorProperty(p) => (&p.decorators, &p.key),
            _ => continue,
        };
        let Some(member_name) = key_name(key) else {
            continue;
        };
        for dec in decorators.iter() {
            if let Some((is_content, meta)) = decorator_query_metadata(member_name, dec) {
                if is_content {
                    content_queries.push(meta);
                } else {
                    view_queries.push(meta);
                }
            }
        }
    }
}

/// Order a mixed content/view query list to match Angular's emit order.
///
/// ngtsc emits SIGNAL queries before LEGACY (decorator) queries (`createQueryCreateCalls` walks the
/// signal queries first, then the legacy ones); within the legacy group, single-result (`first`)
/// queries precede multi-result ones; SIGNAL queries keep their declaration order (they are not
/// single-first sorted — see `signal_queries/query_in_directive`). All groupings are STABLE
/// (relative declaration order preserved within each bucket).
fn order_queries_for_emit(queries: &mut Vec<R3QueryMetadata>) {
    let mut signal: Vec<R3QueryMetadata> = Vec::new();
    let mut legacy_single: Vec<R3QueryMetadata> = Vec::new();
    let mut legacy_multi: Vec<R3QueryMetadata> = Vec::new();
    for q in queries.drain(..) {
        if q.is_signal {
            signal.push(q);
        } else if q.first {
            legacy_single.push(q);
        } else {
            legacy_multi.push(q);
        }
    }
    let mut out = signal;
    out.extend(legacy_single);
    out.extend(legacy_multi);
    *queries = out;
}

// ---------------------------------------------------------------------------
// R2 — host bindings (`host: {...}` object, `@HostBinding`/`@HostListener` members).
// ---------------------------------------------------------------------------

/// Parse the `@Component`/`@Directive` `host: {...}` object literal into the
/// `OrderedMap<String, HostValue>` that [`parse_host_bindings`] consumes. Faithful to ngtsc's
/// `extractHostBindings`: each property key is the raw host key (`'(click)'`, `'[id]'`, `'class'`,
/// `'role'`, …) and the value is its string (the binding expression / static attribute value).
/// Returns `Err` for a non-object `host` or a non-string value (ngtsc diagnoses both).
fn parse_host_object(expr: &Expression) -> Result<OrderedMap<String, HostValue>, String> {
    let Expression::ObjectExpression(obj) = expr else {
        return Err("`host` must be an object literal".to_string());
    };
    let mut out: OrderedMap<String, HostValue> = OrderedMap::new();
    for p in &obj.properties {
        let ObjectPropertyKind::ObjectProperty(op) = p else {
            return Err("unsupported `host` spread/shorthand".to_string());
        };
        let key = key_name(&op.key)
            .ok_or_else(|| "unsupported `host` computed key".to_string())?
            .to_string();
        let value = string_value(&op.value)
            .ok_or_else(|| format!("`host` value for '{key}' must be a string"))?;
        out.insert(key, HostValue::Str(value));
    }
    Ok(out)
}

/// Accumulator for member-level `@HostBinding`/`@HostListener` host entries, merged on top of the
/// decorator-object `host` map (ngtsc folds both into one `ParsedHostBindings`).
#[derive(Default)]
struct MemberHost {
    entries: OrderedMap<String, HostValue>,
}

/// Collect `@HostBinding('prop')` (property/accessor members) and `@HostListener('event', [args])`
/// (method members) into raw host entries.
///
/// Faithful to ngtsc's `extractHostBindings`:
///   * `@HostBinding('hostProp') member` → key `[hostProp]` (defaulting `hostProp` to the member
///     name), value the member read `member` (e.g. `[id]: 'dirId'`).
///   * `@HostListener('event', ['$event.target'])` on `method()` → key `(event)`, value
///     `method($event.target)` (the listener invokes the handler with the declared args; the bare
///     `@HostListener('event')` form invokes `method()`).
fn collect_member_host_bindings(class: &Class, host: &mut MemberHost) {
    for element in &class.body.body {
        // `@HostBinding` rides on property/accessor members; `@HostListener` on method members.
        let (decorators, key) = match element {
            ClassElement::PropertyDefinition(p) => (&p.decorators, &p.key),
            ClassElement::AccessorProperty(p) => (&p.decorators, &p.key),
            ClassElement::MethodDefinition(m) => (&m.decorators, &m.key),
            _ => continue,
        };
        let Some(member_name) = key_name(key) else { continue };
        for dec in decorators.iter() {
            match decorator_name(dec) {
                Some("HostBinding") => {
                    // `@HostBinding('hostProp')` → bound prop named `hostProp` (or the member
                    // name); value is the member read expression.
                    let host_prop =
                        decorator_string_alias(dec).unwrap_or_else(|| member_name.to_string());
                    host.entries.insert(
                        format!("[{host_prop}]"),
                        HostValue::Str(member_name.to_string()),
                    );
                }
                Some("HostListener") => {
                    if let Some((event, handler)) = host_listener_entry(dec, member_name) {
                        host.entries
                            .insert(format!("({event})"), HostValue::Str(handler));
                    }
                }
                _ => {}
            }
        }
    }
}

/// Build the `(event) -> handler-invocation` pair for a `@HostListener('event', [args])` decorator.
/// The handler text is `member(arg0, arg1, …)` where each arg is the raw source of the string entry
/// in the (optional) 2nd-argument array (ngtsc's `bindingPropertyName`/`args` handling). Returns
/// `None` when the event name is missing.
fn host_listener_entry(dec: &Decorator, member_name: &str) -> Option<(String, String)> {
    let Expression::CallExpression(call) = &dec.expression else {
        return None;
    };
    let event = match call.arguments.first().and_then(|a| a.as_expression())? {
        Expression::StringLiteral(s) => s.value.to_string(),
        _ => return None,
    };
    // Optional args array (each entry a string of source-expression text).
    let mut args: Vec<String> = Vec::new();
    if let Some(Expression::ArrayExpression(arr)) =
        call.arguments.get(1).and_then(|a| a.as_expression())
    {
        for el in &arr.elements {
            if let Some(s) = el.as_expression().and_then(string_value) {
                args.push(s);
            }
        }
    }
    let handler = format!("{member_name}({})", args.join(","));
    Some((event, handler))
}

/// Parse the `@Component`/`@Directive` `hostDirectives: [...]` array into
/// [`R3HostDirectiveMetadata`]. Faithful to ngtsc's `extractHostDirectives`: each entry is either
///   * a bare identifier `HostDir` → `{directive: HostDir}` (no input/output mapping), or
///   * an object `{directive: HostDir, inputs: ['a','b: c'], outputs: ['x: y']}` → the directive
///     plus parsed input/output public-name → alias maps (`'a'` aliases to itself; `'a: b'`
///     maps public `a` to alias `b`).
/// Entries we cannot name are skipped. Returns `None` when nothing usable is parsed.
fn parse_host_directives(expr: &Expression) -> Option<Vec<R3HostDirectiveMetadata>> {
    let Expression::ArrayExpression(arr) = expr else {
        return None;
    };
    let mut out: Vec<R3HostDirectiveMetadata> = Vec::new();
    for el in &arr.elements {
        let Some(inner) = el.as_expression() else { continue };
        if let Some(meta) = host_directive_entry(inner) {
            out.push(meta);
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn host_directive_entry(expr: &Expression) -> Option<R3HostDirectiveMetadata> {
    match expr {
        Expression::Identifier(id) => Some(R3HostDirectiveMetadata {
            directive: directive_ref(id.name.as_str()),
            is_forward_reference: false,
            inputs: None,
            outputs: None,
        }),
        Expression::ObjectExpression(obj) => {
            let directive_expr = find_prop(obj, "directive")?;
            let (name, is_forward) = directive_name_maybe_forward(directive_expr)?;
            let inputs = find_prop(obj, "inputs").and_then(host_directive_mapping);
            let outputs = find_prop(obj, "outputs").and_then(host_directive_mapping);
            Some(R3HostDirectiveMetadata {
                directive: directive_ref(&name),
                is_forward_reference: is_forward,
                inputs,
                outputs,
            })
        }
        Expression::ParenthesizedExpression(p) => host_directive_entry(&p.expression),
        // A bare `forwardRef(() => Dir)` host-directive entry (no input/output mapping).
        Expression::CallExpression(_) => {
            let (name, is_forward) = directive_name_maybe_forward(expr)?;
            Some(R3HostDirectiveMetadata {
                directive: directive_ref(&name),
                is_forward_reference: is_forward,
                inputs: None,
                outputs: None,
            })
        }
        _ => None,
    }
}

/// Resolve a directive reference expression to its class name plus whether it was wrapped in
/// `forwardRef(() => X)` (faithful to ngtsc's `forwardRefResolver`). A bare identifier `X` →
/// `(X, false)`; `forwardRef(() => X)` → `(X, true)`.
fn directive_name_maybe_forward(expr: &Expression) -> Option<(String, bool)> {
    match expr {
        Expression::Identifier(id) => Some((id.name.to_string(), false)),
        Expression::ParenthesizedExpression(p) => directive_name_maybe_forward(&p.expression),
        Expression::CallExpression(call) => {
            // `forwardRef(() => X)`: the callee is `forwardRef`; the single arg is an arrow/fn
            // whose returned expression is the target identifier.
            let is_forward_ref = matches!(&call.callee, Expression::Identifier(id) if id.name == "forwardRef");
            if !is_forward_ref {
                return None;
            }
            let arg = call.arguments.first().and_then(|a| a.as_expression())?;
            let name = arrow_returned_identifier(arg)?;
            Some((name, true))
        }
        _ => None,
    }
}

/// The identifier returned by a `() => X` arrow (expression body) or `() => { return X; }` arrow /
/// function body. Used to unwrap `forwardRef(() => X)`.
fn arrow_returned_identifier(expr: &Expression) -> Option<String> {
    match expr {
        Expression::ArrowFunctionExpression(arrow) => {
            // Expression-bodied arrow: the body is a single `ExpressionStatement`.
            if arrow.expression {
                if let Some(Statement::ExpressionStatement(stmt)) = arrow.body.statements.first() {
                    return identifier_of(&stmt.expression);
                }
            }
            // Block-bodied arrow: find the `return X;`.
            for stmt in &arrow.body.statements {
                if let Statement::ReturnStatement(ret) = stmt {
                    if let Some(arg) = &ret.argument {
                        return identifier_of(arg);
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// The bare identifier name of an expression (unwrapping parentheses), if it is one.
fn identifier_of(expr: &Expression) -> Option<String> {
    match expr {
        Expression::Identifier(id) => Some(id.name.to_string()),
        Expression::ParenthesizedExpression(p) => identifier_of(&p.expression),
        _ => None,
    }
}

/// Parse a `hostDirectives` `inputs`/`outputs` mapping array (`['a', 'b: c']`) into an ordered
/// public-name → alias map. `'a'` aliases to itself; `'b: c'` maps public `b` to alias `c`.
fn host_directive_mapping(expr: &Expression) -> Option<OrderedMap<String, String>> {
    let Expression::ArrayExpression(arr) = expr else {
        return None;
    };
    let mut map: OrderedMap<String, String> = OrderedMap::new();
    for el in &arr.elements {
        let Some(s) = el.as_expression().and_then(string_value) else { continue };
        let (public_name, alias) = match s.split_once(':') {
            Some((p, a)) => (p.trim().to_string(), a.trim().to_string()),
            None => (s.trim().to_string(), s.trim().to_string()),
        };
        map.insert(public_name, alias);
    }
    if map.is_empty() {
        None
    } else {
        Some(map)
    }
}

/// Whether the class declares an `ngOnChanges` lifecycle method (faithful to ngtsc's
/// `lifecycle.usesOnChanges` detection driving the `NgOnChangesFeature`). Recognizes the method on
/// a `MethodDefinition` or a property/accessor whose name is `ngOnChanges`.
fn class_uses_on_changes(class: &Class) -> bool {
    class.body.body.iter().any(|element| {
        let key = match element {
            ClassElement::MethodDefinition(m) => &m.key,
            ClassElement::PropertyDefinition(p) => &p.key,
            ClassElement::AccessorProperty(p) => &p.key,
            _ => return false,
        };
        key_name(key) == Some("ngOnChanges")
    })
}

/// A directive self-reference (`value`/`ty` both `Foo`) for `hostDirectives` / `@Directive`
/// metadata.
fn directive_ref(name: &str) -> DirRef {
    DirRef {
        value: o::variable(name, None),
        ty: o::variable(name, None),
    }
}

/// Build the combined [`R3HostMetadata`] from the decorator-object `host: {...}` (if any) and the
/// member `@HostBinding`/`@HostListener` entries. The decorator-object entries come first (ngtsc
/// processes the `host` object then folds member bindings on top), preserving source order.
fn build_host_metadata(
    host_obj: Option<&Expression>,
    class: &Class,
) -> Result<R3HostMetadata, String> {
    let mut raw: OrderedMap<String, HostValue> = match host_obj {
        Some(expr) => parse_host_object(expr)?,
        None => OrderedMap::new(),
    };
    let mut member = MemberHost::default();
    collect_member_host_bindings(class, &mut member);
    for (k, v) in member.entries.iter() {
        raw.insert(k.clone(), v.clone());
    }
    parse_host_bindings(raw)
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

    compile_program_with_source(&ret.program, Some(ts_source), None)
}

/// Context for additive source-map emission: the original authoring source text plus the
/// names used in the emitted v3 map (`file` for the generated artifact, `source_name` for
/// the original). Threaded into the compile pipeline so the emitter can map span-carrying
/// nodes back into `source_content` WITHOUT changing any emitted byte.
struct MapContext<'a> {
    file_name: &'a str,
    source_name: &'a str,
    source_content: &'a str,
}

/// A compiled component plus its additive Source Map v3 JSON. `code` is BYTE-IDENTICAL to
/// [`CompiledComponent::code`] from [`compile_component_source`]; `map` is a SEPARATE,
/// additive artifact (the Ivy-JS <-> original-authoring-source mapping).
#[derive(Debug, Clone)]
pub struct CompiledComponentWithMap {
    /// The emitted `ɵɵdefineComponent({...})` source (plus any hoisted pool statements).
    pub code: String,
    /// The Source Map v3 JSON mapping `code` back to the original authoring source. Empty
    /// (`{}` is never emitted) when compilation failed; see `errors`.
    pub map: String,
    /// Fatal/diagnostic messages (same semantics as [`CompiledComponent::errors`]).
    pub errors: Vec<String>,
}

/// Compile a single standalone `@Component` class from TypeScript source AND emit an
/// additive v3 source map.
///
/// `file_name` is the generated artifact name (the map's `file`); `source_name` is the
/// original source's name (the map's `sources[0]`). The original `ts_source` is embedded as
/// `sourcesContent[0]`. The returned `code` is byte-identical to
/// [`compile_component_source`]'s — the map is purely additive.
pub fn compile_component_source_with_map(
    ts_source: &str,
    file_name: &str,
    source_name: &str,
) -> CompiledComponentWithMap {
    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true);
    let ret = Parser::new(&allocator, ts_source, source_type).parse();

    if !ret.errors.is_empty() {
        let msgs: Vec<String> = ret.errors.iter().map(|e| e.to_string()).collect();
        let e = err(format!("parse error: {}", msgs.join("; ")));
        return CompiledComponentWithMap {
            code: e.code,
            map: String::new(),
            errors: e.errors,
        };
    }

    let ctx = MapContext {
        file_name,
        source_name,
        source_content: ts_source,
    };
    let mut map_out = String::new();
    let compiled =
        compile_program_with_source(&ret.program, Some(ts_source), Some((&ctx, &mut map_out)));
    CompiledComponentWithMap {
        code: compiled.code,
        map: map_out,
        errors: compiled.errors,
    }
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

/// A top-level statement classified for source-order re-assembly.
enum TopStmt<'a> {
    /// A class carrying a recognized Angular decorator (the class, its kind, the decorator node).
    Decorated(&'a Class<'a>, TopLevel, &'a Decorator<'a>),
}

/// Recognize a class's top-level Angular decorator kind (the FIRST recognized one wins, mirroring
/// ngtsc's single-trait-per-class rule). Returns the kind and the decorator node.
fn class_top_level<'a>(class: &'a Class<'a>) -> Option<(TopLevel, &'a Decorator<'a>)> {
    for dec in &class.decorators {
        if let Some(name) = decorator_name(dec) {
            let kind = match name {
                "Component" => Some(TopLevel::Component),
                "Directive" => Some(TopLevel::Directive),
                "Pipe" => Some(TopLevel::Pipe),
                "NgModule" => Some(TopLevel::NgModule),
                "Injectable" => Some(TopLevel::Injectable),
                _ => None,
            };
            if let Some(kind) = kind {
                return Some((kind, dec));
            }
        }
    }
    None
}

/// The class declaration of a top-level statement (plain, `export`, or `export default`).
fn statement_class<'a>(stmt: &'a Statement<'a>) -> Option<&'a Class<'a>> {
    match stmt {
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
    }
}

/// The Ivy emit of ONE decorated class, decomposed so the original module can be re-assembled
/// around it (rather than replaced by it).
///
/// `def_expression` is the `ɵɵdefine*({...})` call render3 already produces — byte-identical to the
/// historical bare-expression emit. `extra_statements` are the hoisted constant-pool consts /
/// nested template functions (and, for `@NgModule`, the `ɵɵsetNgModuleScope` / `ɵɵregisterNgModuleType`
/// side-effect statements). `factory` is the `ɵfac` metadata when the kind carries one. The caller
/// stitches these AFTER the kept (decorator-stripped) class declaration as
/// `<pool…>; X.ɵfac = <factory>; X.<static_member> = <def_expression>;`.
struct ClassEmit {
    class_name: String,
    /// The Ivy static property name the definition is assigned to (`ɵcmp`/`ɵdir`/`ɵpipe`/`ɵmod`).
    static_member: &'static str,
    def_expression: Expr,
    extra_statements: Vec<o::Stmt>,
    /// Whether `extra_statements` must be emitted AFTER the `X.<member> =` assignment. Component /
    /// directive / pipe hoist constant-pool consts the definition REFERENCES, so they come BEFORE
    /// (`false`); `@NgModule` emits `ɵɵsetNgModuleScope` / `ɵɵregisterNgModuleType` SIDE EFFECTS that
    /// run after the definition exists, so they come AFTER (`true`).
    extra_after_def: bool,
    /// `ɵfac` factory metadata, when the kind declares a factory (Component/Directive/Pipe/NgModule).
    factory: Option<R3FactoryMetadata>,
    /// Non-fatal diagnostics (e.g. template parse warnings) gathered while compiling this class.
    errors: Vec<String>,
}

/// Build a no-dependency constructor [`R3FactoryMetadata`] for a source-front-end class. The SOURCE
/// front-end does not yet resolve constructor parameters, so the emitted `ɵfac` is the empty-deps
/// form Angular generates for a parameterless constructor: `function X_Factory(t) { return new (t || X)(); }`.
fn empty_factory(class_name: &str, target: FactoryTarget) -> R3FactoryMetadata {
    R3FactoryMetadata::Constructor(R3ConstructorFactoryMetadata {
        name: class_name.to_string(),
        ty: directive_ref(class_name),
        type_argument_count: 0,
        deps: FactoryDeps::Deps(Vec::new()),
        target,
    })
}

/// Lower a [`ClassEmit`] into the `output_ast` statements appended AFTER its kept class declaration:
/// the hoisted pool/side-effect statements, then `X.ɵfac = <factory>;` (when present), then
/// `X.<static_member> = <def_expression>;`. The `def_expression`'s `ɵɵdefine*({...})` argument block
/// is preserved byte-for-byte — only the `X.<member> =` assignment is added around it.
fn class_static_statements(emit: ClassEmit) -> Vec<o::Stmt> {
    let ClassEmit {
        class_name,
        static_member,
        def_expression,
        extra_statements,
        extra_after_def,
        factory,
        ..
    } = emit;

    let mut stmts: Vec<o::Stmt> = Vec::new();

    // Constant-pool consts the definition references are hoisted BEFORE the assignments.
    if !extra_after_def {
        stmts.extend(extra_statements.clone());
    }

    if let Some(factory) = factory {
        let fac = compile_factory_function(&factory);
        // The factory may itself hoist a `ɵX_BaseFactory` const (inherited-deps case); emit those
        // first so the `ɵfac` assignment that references them is well-formed.
        stmts.extend(fac.statements);
        stmts.push(
            o::variable(&class_name, None)
                .prop("\u{0275}fac")
                .set(fac.expression)
                .to_stmt(),
        );
    }

    stmts.push(
        o::variable(&class_name, None)
            .prop(static_member)
            .set(def_expression)
            .to_stmt(),
    );

    // `@NgModule` scope side effects run AFTER the definition is assigned.
    if extra_after_def {
        stmts.extend(extra_statements);
    }
    stmts
}

/// Assemble the COMPLETE ES module: the original source with every Angular decorator stripped and
/// each decorated class's Ivy statics appended after it, matching Angular Ivy's emit shape.
///
/// Shape (per ngtsc's `DecoratorHandler` + the TS class transformer):
///   * every original `import` declaration is KEPT verbatim;
///   * `import * as i0 from "@angular/core";` is prepended (the namespace the Ivy statics reference);
///   * every top-level statement is emitted in SOURCE ORDER — a decorated class keeps its
///     `export`/`class X { … }` declaration with ONLY the recognized Angular decorator removed, and
///     its `ɵfac`/`ɵcmp`/… statics follow it; every other statement is copied through verbatim.
///
/// `source` is the original authoring TypeScript. `class_emits` maps a class's source start offset
/// (the decorator's start) to its compiled [`ClassEmit`]. The decorator span is excised from the
/// kept declaration so the emitted class is plain TS the bundler accepts.
fn assemble_module(
    source: &str,
    program: &Program,
    mut class_emits: std::collections::HashMap<usize, (ClassEmit, u32, u32)>,
) -> String {
    // Locate the byte position after the last original import declaration, so the synthetic
    // `import * as i0` line sits with the other imports (ngtsc groups it there). When there are no
    // imports it goes to the very top.
    let mut import_insert_at: usize = 0;
    for stmt in &program.body {
        if let Statement::ImportDeclaration(import) = stmt {
            import_insert_at = import.span().end as usize;
        }
    }

    let mut out = String::new();
    let mut cursor: usize = 0;
    let i0_line = "import * as i0 from \"@angular/core\";\n";

    for stmt in &program.body {
        let span = stmt.span();
        let stmt_start = span.start as usize;
        let stmt_end = span.end as usize;

        let decorated = class_emits.remove(&stmt_start);

        // The first byte of THIS statement's source. For a decorated class the recognized Angular
        // decorator sits BEFORE the class statement span (oxc does not include leading decorators in
        // the class/statement span), so the content begins at the decorator; otherwise at the
        // statement start. Flush the inter-statement source (leading whitespace/comments) up to that
        // point verbatim.
        let content_start = match &decorated {
            Some((_, dec_start, _)) => (*dec_start as usize).min(stmt_start),
            None => stmt_start,
        };
        if cursor < content_start {
            out.push_str(&source[cursor..content_start]);
        }

        if let Some((emit, dec_start, dec_end)) = decorated {
            // Kept class declaration with the recognized Angular decorator excised: emit the source
            // from the content start up to the decorator (leading non-Angular decorators / modifiers,
            // usually empty), SKIP the Angular decorator span, then emit from after it to the
            // statement end (`export class X { … }`).
            let dec_start = dec_start as usize;
            let dec_end = dec_end as usize;
            if content_start < dec_start {
                out.push_str(&source[content_start..dec_start]);
            }
            out.push_str(&source[dec_end..stmt_end]);
            // Statics: emitted via the shared lowering, then spliced in WITHOUT the leading
            // `import * as i0` line (added once at module scope below).
            let statics = class_static_statements(emit);
            let block = crate::output::emitter::emit_statements(&statics);
            let block = strip_i0_import_line(&block);
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(block.trim_end_matches('\n'));
            out.push('\n');
        } else {
            // A non-Angular statement (or a non-decorated class): copy through verbatim.
            out.push_str(&source[stmt_start..stmt_end]);
        }
        cursor = stmt_end;

        // After emitting the last import, inject the i0 namespace import on its own line.
        if stmt_end == import_insert_at {
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(i0_line);
        }
    }

    // Trailing source after the final statement (comments / whitespace).
    if cursor < source.len() {
        out.push_str(&source[cursor..]);
    }

    // No imports at all: the i0 line was never injected above — prepend it.
    if import_insert_at == 0 {
        let mut prefixed = String::with_capacity(i0_line.len() + out.len());
        prefixed.push_str(i0_line);
        prefixed.push_str(&out);
        out = prefixed;
    }

    out
}

/// Strip the single leading `import * as i0 from "@angular/core";` line `emit_statements` prepends,
/// leaving just the statement bodies (the module-scope import is emitted once by [`assemble_module`]).
fn strip_i0_import_line(block: &str) -> String {
    let lines = block.lines();
    if let Some(first) = lines.clone().next() {
        if first.trim_start().starts_with("import * as i0 from") {
            return lines.skip(1).collect::<Vec<_>>().join("\n");
        }
    }
    block.to_string()
}

/// The module-aware compile. When `source` is `Some`, the emitted `code` is the COMPLETE original
/// ES module augmented with the Ivy statics (the production path, fixing the missing-export bug);
/// when `None`, the legacy bare-definition emit is produced (used only where the original source
/// text is unavailable, e.g. internal callers that pre-parsed without retaining the text).
fn compile_program_with_source(
    program: &Program,
    source: Option<&str>,
    map: Option<(&MapContext, &mut String)>,
) -> CompiledComponent {
    let imported_names = collect_imported_names(program);

    // Walk every top-level statement, classifying each decorated class in SOURCE ORDER. R1:
    // collect EVERY decorated class (not just the first) so multi-class files emit each definition.
    let mut decorated: Vec<TopStmt> = Vec::new();
    let mut sibling_class_names: Vec<String> = Vec::new();
    for stmt in &program.body {
        let Some(class) = statement_class(stmt) else { continue };
        if let Some((kind, dec)) = class_top_level(class) {
            if let Some(id) = &class.id {
                sibling_class_names.push(id.name.to_string());
            }
            decorated.push(TopStmt::Decorated(class, kind, dec));
        }
    }

    if decorated.is_empty() {
        return err("no @Component/@Directive/@Pipe/@NgModule decorated class found".to_string());
    }

    // Cross-class selectorless auto-import: a component in a multi-class file can reference a
    // sibling-declared component/directive directly by its class name in the template (no `imports`
    // array, no selector). Seed the auto-import candidate set with both the file's imported names
    // AND the sibling class names so `resolve_template_dependencies` can match them.
    let mut auto_import_candidates = imported_names.clone();
    for name in &sibling_class_names {
        if !auto_import_candidates.contains(name) {
            auto_import_candidates.push(name.clone());
        }
    }

    // Cross-class CSS-SELECTOR directive matching: a sibling-declared `@Directive`/`@Component`
    // whose selector matches via an attribute/property/output binding, on `ng-template`/
    // `ng-container`, or as a structural directive is matched by SELECTOR (not class name), so the
    // selectorless candidate set above cannot find it. Collect every sibling's `(name, selector,
    // isComponent)` so [`crate::binder::resolve_selector_dependencies`] can match them against the
    // bound template and add the matches to the component's `dependencies` (see
    // `compile_component_meta`).
    let sibling_directives = collect_sibling_directives(&decorated);

    // Compile EVERY decorated class to a structured [`ClassEmit`]. The def block render3 produces is
    // unchanged; we only decompose it (def expression + pool/side-effect statements + factory) so the
    // ORIGINAL module can be re-assembled around the kept class declarations.
    let mut emits: std::collections::HashMap<usize, (ClassEmit, u32, u32)> =
        std::collections::HashMap::new();
    let mut errors: Vec<String> = Vec::new();
    let mut produced = 0usize;
    for item in &decorated {
        let TopStmt::Decorated(class, kind, dec) = *item;
        match compile_decorated_class(
            class,
            kind,
            dec,
            &auto_import_candidates,
            &sibling_directives,
        ) {
            Ok(emit) => {
                errors.extend(emit.errors.clone());
                let stmt_start = decorated_stmt_start(program, class);
                emits.insert(
                    stmt_start,
                    (emit, dec.span().start, dec.span().end),
                );
                produced += 1;
            }
            Err(msg) => errors.push(msg),
        }
    }

    if produced == 0 {
        if errors.is_empty() {
            errors.push("no emittable definition produced".to_string());
        }
        return CompiledComponent {
            code: String::new(),
            errors,
        };
    }

    // Production path: emit the COMPLETE original module augmented with the Ivy statics.
    let Some(source) = source else {
        // Legacy bare-definition emit (source text unavailable): concatenate each class's statics
        // (pool + `X.ɵfac`/`X.ɵcmp` assignments) through the standard emitter. Kept for robustness;
        // the public entry points always supply the source.
        let mut all_stmts: Vec<o::Stmt> = Vec::new();
        let mut keys: Vec<usize> = emits.keys().copied().collect();
        keys.sort_unstable();
        for k in keys {
            if let Some((emit, _, _)) = emits.remove(&k) {
                all_stmts.extend(class_static_statements(emit));
            }
        }
        let code = crate::output::emitter::emit_statements(&all_stmts);
        return CompiledComponent { code, errors };
    };

    // For a single decorated class with a map request, recover that class's def expression + the
    // static member it's assigned to so the additive map can anchor the emitted `type: <ClassName>`
    // read to the class declaration AFTER assembly (the map is additive — it never changes `code`).
    let single_def: Option<(Expr, &'static str)> = if produced == 1 {
        emits
            .values()
            .next()
            .map(|(e, _, _)| (e.def_expression.clone(), e.static_member))
    } else {
        None
    };

    let code = assemble_module(source, program, emits);

    if let Some((ctx, map_out)) = map {
        if let Some((def_expr, member)) = single_def {
            // Locate the def block in the assembled module: the `ɵɵdefine*` marker the member maps
            // to. Searching the map anchors forward from there yields the correct generated offsets.
            let marker = define_marker_for(member);
            let from = code.find(marker).unwrap_or(0);
            *map_out = crate::output::emitter::build_definition_map(
                ctx.file_name,
                ctx.source_name,
                ctx.source_content,
                &code,
                from,
                &def_expr,
            );
        }
    }

    CompiledComponent { code, errors }
}

/// The `ɵɵdefine*` marker text the given Ivy static member's definition uses, so the source-map
/// builder can locate the definition's start in the assembled module.
fn define_marker_for(member: &str) -> &'static str {
    match member {
        "\u{0275}cmp" => "\u{0275}\u{0275}defineComponent",
        "\u{0275}dir" => "\u{0275}\u{0275}defineDirective",
        "\u{0275}pipe" => "\u{0275}\u{0275}definePipe",
        "\u{0275}mod" => "\u{0275}\u{0275}defineNgModule",
        _ => "\u{0275}\u{0275}define",
    }
}

/// The byte start of the top-level statement that declares `class` (its decorator, or the
/// `export`/`class` keyword when no decorator leads). Used as the assembly key. Falls back to the
/// class node's own span when the declaring statement cannot be located (never expected).
fn decorated_stmt_start(program: &Program, class: &Class) -> usize {
    let class_span = class.span();
    for stmt in &program.body {
        if let Some(c) = statement_class(stmt) {
            if c.span() == class_span {
                return stmt.span().start as usize;
            }
        }
    }
    class_span.start as usize
}

/// Collect the `(name, selector, is_component)` of every sibling `@Directive`/`@Component` class
/// that carries a non-empty `selector`, for cross-class CSS-selector directive matching. A
/// directive/component without a selector (selectorless / class-name-only) is excluded — it is
/// resolved through the selectorless candidate set instead.
fn collect_sibling_directives(decorated: &[TopStmt]) -> Vec<crate::binder::SelectorDirective> {
    let mut out = Vec::new();
    for TopStmt::Decorated(class, kind, dec) in decorated {
        let is_component = match kind {
            TopLevel::Component => true,
            TopLevel::Directive => false,
            _ => continue,
        };
        let Some(id) = &class.id else { continue };
        let name = id.name.to_string();
        let selector = decorator_object(dec)
            .and_then(|o| find_prop(o, "selector"))
            .and_then(string_value);
        if let Some(selector) = selector {
            if !selector.trim().is_empty() {
                out.push(crate::binder::SelectorDirective::new(
                    name,
                    selector,
                    is_component,
                ));
            }
        }
    }
    out
}

/// Compile ONE decorated class to its [`ClassEmit`], dispatching on the decorator kind. The def
/// block render3 produces is unchanged — `ClassEmit` decomposes it (def expression + pool/side-effect
/// statements + factory metadata) so the caller can re-assemble the original module around it.
fn compile_decorated_class(
    class: &Class,
    kind: TopLevel,
    dec: &Decorator,
    auto_import_candidates: &[String],
    sibling_directives: &[crate::binder::SelectorDirective],
) -> Result<ClassEmit, String> {
    let (class_name, class_name_span) = match &class.id {
        Some(id) => (
            id.name.to_string(),
            ParseSourceSpan::new(id.span.start as usize, id.span.end as usize),
        ),
        None => return Err("decorated class has no name".to_string()),
    };

    let obj = decorator_object(dec);

    match kind {
        TopLevel::Component | TopLevel::Directive => compile_component_or_directive(
            class,
            kind,
            obj,
            class_name,
            class_name_span,
            auto_import_candidates,
            sibling_directives,
        ),
        TopLevel::Pipe => compile_pipe_class(obj, &class_name),
        TopLevel::NgModule => compile_ng_module_class(obj, &class_name),
        // `@Injectable` carries no template-facing definition we model yet; the `ɵfac`/`ɵprov`
        // factory + provider emit lives in the factory compiler, out of this front-end's scope.
        TopLevel::Injectable => {
            Err("@Injectable emission not yet supported by the source front-end".to_string())
        }
    }
}

/// Compile a `@Component` or `@Directive` class. Shares the common metadata extraction (selector,
/// inputs/outputs, queries, host bindings, `hostDirectives`, `exportAs`) and then routes to the
/// component emitter (`compile_component_from_metadata`, with template) or the directive emitter
/// (`compile_directive_from_metadata`, no template).
#[allow(clippy::too_many_arguments)]
fn compile_component_or_directive(
    class: &Class,
    kind: TopLevel,
    obj: Option<&oxc_ast::ast::ObjectExpression>,
    class_name: String,
    class_name_span: ParseSourceSpan,
    auto_import_candidates: &[String],
    sibling_directives: &[crate::binder::SelectorDirective],
) -> Result<ClassEmit, String> {
    // Reject decorator-level metadata we cannot yet model.
    if let Some(obj) = obj {
        for k in UNSUPPORTED_DECORATOR_KEYS {
            if find_prop(obj, k).is_some() {
                return Err(format!("unsupported @{:?} metadata key: {k}", kind));
            }
        }
        if find_prop(obj, "templateUrl").is_some() {
            return Err("external templateUrl unsupported (inline `template` only)".to_string());
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
        return Err("component has no inline string `template`".to_string());
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

    // R2: host bindings — the `host: {...}` object merged with `@HostBinding`/`@HostListener`
    // members, parsed into the `R3HostMetadata` the `DefaultHostBindingsBuilder` consumes.
    let host = match build_host_metadata(obj.and_then(|o| find_prop(o, "host")), class) {
        Ok(host) => host,
        Err(e) => return Err(e),
    };

    // R2: hostDirectives -> HostDirectivesFeature.
    let host_directives = obj
        .and_then(|o| find_prop(o, "hostDirectives"))
        .and_then(parse_host_directives);

    // exportAs: 'a' | 'a, b' — the directive's template-reference export names.
    let export_as = obj
        .and_then(|o| find_prop(o, "exportAs"))
        .and_then(string_value)
        .map(|s| s.split(',').map(|p| p.trim().to_string()).collect::<Vec<_>>());

    // providers: [...] — the directive/component injectable providers. Angular folds this into a
    // `features: [ɵɵProvidersFeature(providers[, viewProviders])]` entry on the define block; the
    // emitter (`view::compiler::add_features`) consumes `base.providers` as the FIRST feature
    // argument. The array expression is copied through verbatim (`WrappedNodeExpr`-style) via
    // `convert_expr`, matching ngtsc's provider emit. A present-but-unconvertible providers value
    // bails rather than silently dropping the feature.
    let providers = match obj.and_then(|o| find_prop(o, "providers")) {
        None => None,
        Some(e) => match convert_expr(e) {
            Some(expr) => Some(expr),
            None => return Err("unsupported `providers` expression form".to_string()),
        },
    };

    // viewProviders: [...] — component-only; folded into the SECOND `ɵɵProvidersFeature` argument.
    // Carried on the component metadata (`R3ComponentMetadata::view_providers`); directives have no
    // view-provider scope, so Angular ignores it there and so do we.
    let view_providers = match obj.and_then(|o| find_prop(o, "viewProviders")) {
        None => None,
        Some(e) => match convert_expr(e) {
            Some(expr) => Some(expr),
            None => return Err("unsupported `viewProviders` expression form".to_string()),
        },
    };

    // inputs / outputs.
    let mut inputs: OrderedMap<String, R3InputMetadata> = OrderedMap::new();
    let mut outputs: OrderedMap<String, String> = OrderedMap::new();
    if let Err(e) = collect_io(class, &mut inputs, &mut outputs) {
        return Err(e);
    }

    // Queries: signal-based member initializers (`viewChild`/…) AND R3 decorator members
    // (`@ViewChild`/…). Both feed the SAME `R3QueryMetadata` lists. Angular emits single-result
    // (`first`) queries' calls before multi-result ones, so partition stably.
    let mut content_queries: Vec<R3QueryMetadata> = Vec::new();
    let mut view_queries: Vec<R3QueryMetadata> = Vec::new();
    collect_signal_queries(class, &mut content_queries, &mut view_queries);
    collect_decorator_queries(class, &mut content_queries, &mut view_queries);
    order_queries_for_emit(&mut content_queries);
    order_queries_for_emit(&mut view_queries);

    let has_signal_query = content_queries
        .iter()
        .chain(view_queries.iter())
        .any(|q| q.is_signal);
    let is_signal = inputs.iter().any(|(_, m)| m.is_signal) || has_signal_query;

    // Build the base directive metadata. The class-name reference carries the original
    // class-id span so the additive source map can anchor the emitted `type: <ClassName>`
    // back to the class declaration. Stamping the span is value-preserving (it never
    // affects emitted text), so this is identical for the plain and the map paths.
    let base = R3DirectiveMetadata {
        name: class_name.clone(),
        ty: class_ref_spanned(&class_name, class_name_span),
        type_argument_count: 0,
        type_source_span: ParseSourceSpan::new(0, 0),
        deps: Deps::None,
        selector: selector.clone(),
        queries: content_queries,
        view_queries,
        host,
        lifecycle: Lifecycle {
            uses_on_changes: class_uses_on_changes(class),
        },
        inputs,
        outputs,
        uses_inheritance: false,
        control_create: None,
        export_as,
        providers,
        is_standalone: standalone,
        is_signal,
        host_directives,
        legacy_optional_chaining: false,
    };

    match kind {
        TopLevel::Component => {
            // CSS-selector directive candidates for THIS component: every sibling directive/
            // component (excluding self) that carries a selector. Fed to the binder's real
            // CssSelector matcher in `compile_component_meta` to populate `dependencies`.
            let selector_candidates: Vec<crate::binder::SelectorDirective> = sibling_directives
                .iter()
                .filter(|d| d.name != class_name)
                .cloned()
                .collect();
            compile_component_meta(
                base,
                &template_html.unwrap_or_default(),
                change_detection,
                auto_import_candidates,
                &selector_candidates,
                styles,
                encapsulation,
                animations,
                foreign_imports,
                view_providers,
            )
        }
        // R4: @Directive — drive the existing `compile_directive_from_metadata` emitter (no
        // template; host bindings + queries + hostDirectives + exportAs come from `base`).
        // `viewProviders` has no meaning on a directive (no view scope) and Angular drops it there.
        TopLevel::Directive => {
            let _ = (
                change_detection,
                styles,
                encapsulation,
                animations,
                foreign_imports,
                view_providers,
            );
            compile_directive_meta(base)
        }
        _ => unreachable!("compile_component_or_directive only handles Component/Directive"),
    }
}

/// R4: emit a `@Directive` class via the existing [`compile_directive_from_metadata`] +
/// [`DefaultHostBindingsBuilder`]. The hoisted query-predicate `_cN` pool consts are printed
/// before the `ɵɵdefineDirective({...})` expression, mirroring the component path.
fn compile_directive_meta(base: R3DirectiveMetadata) -> Result<ClassEmit, String> {
    let class_name = base.name.clone();
    let mut host_builder = DefaultHostBindingsBuilder;
    let compiled = compile_directive_from_metadata(&base, &mut host_builder);

    Ok(ClassEmit {
        class_name: class_name.clone(),
        static_member: "\u{0275}dir",
        def_expression: compiled.expression,
        extra_statements: compiled.statements,
        extra_after_def: false,
        factory: Some(empty_factory(&class_name, FactoryTarget::Directive)),
        errors: Vec::new(),
    })
}

/// R4: emit a `@Pipe({name, pure?, standalone?})` class via [`compile_pipe_from_metadata`].
fn compile_pipe_class(
    obj: Option<&oxc_ast::ast::ObjectExpression>,
    class_name: &str,
) -> Result<ClassEmit, String> {
    let pipe_name = obj
        .and_then(|o| find_prop(o, "name"))
        .and_then(string_value);
    // `pure` defaults to `true` (Angular's `@Pipe` default).
    let pure = obj
        .and_then(|o| find_prop(o, "pure"))
        .map(|e| matches!(e, Expression::BooleanLiteral(b) if b.value))
        .unwrap_or(true);
    // `standalone` defaults to `true`.
    let is_standalone = obj
        .and_then(|o| find_prop(o, "standalone"))
        .map(|e| matches!(e, Expression::BooleanLiteral(b) if b.value))
        .unwrap_or(true);

    let meta = crate::pipe_module_injector::R3PipeMetadata {
        name: class_name.to_string(),
        r#type: directive_ref(class_name),
        type_argument_count: 0,
        pipe_name,
        deps: None,
        pure,
        is_standalone,
    };
    let compiled = crate::pipe_module_injector::compile_pipe_from_metadata(&meta);
    Ok(ClassEmit {
        class_name: class_name.to_string(),
        static_member: "\u{0275}pipe",
        def_expression: compiled.expression,
        extra_statements: compiled.statements,
        extra_after_def: false,
        factory: Some(empty_factory(class_name, FactoryTarget::Pipe)),
        errors: Vec::new(),
    })
}

/// R4: emit an `@NgModule({declarations, imports, exports, bootstrap, id})` class via
/// [`compile_ng_module`]. Drives the FULL-compilation shape Angular's full/local goldens carry:
/// the `ɵɵdefineNgModule({...})` call holds ONLY `{type[, bootstrap][, id]}`, the selector scope
/// (declarations/imports/exports) is emitted as a tree-shakeable `ɵɵsetNgModuleScope` side effect
/// (`R3SelectorScopeMode::SideEffect`), and an `@NgModule({id})` additionally drives a trailing
/// `ɵɵregisterNgModuleType(Type, id)` statement.
fn compile_ng_module_class(
    obj: Option<&oxc_ast::ast::ObjectExpression>,
    class_name: &str,
) -> Result<ClassEmit, String> {
    use crate::pipe_module_injector::{
        compile_ng_module, R3NgModuleCommon, R3NgModuleMetadata, R3NgModuleMetadataGlobal,
        R3SelectorScopeMode,
    };

    // Each scope array (`declarations`/`imports`/`exports`/`bootstrap`) is a list of bare class
    // identifiers; resolve each to its self-reference. Non-identifier entries are skipped.
    let refs_of = |key: &str| -> Vec<DirRef> {
        obj.and_then(|o| find_prop(o, key))
            .map(identifier_refs)
            .unwrap_or_default()
    };
    let declarations = refs_of("declarations");
    let imports = refs_of("imports");
    let exports = refs_of("exports");
    let bootstrap = refs_of("bootstrap");

    // `id: '<string>'` -> a string-literal expression on the module def. Its presence also drives
    // the trailing `ɵɵregisterNgModuleType(Type, id)` side effect emitted by `compile_ng_module`.
    let id = obj
        .and_then(|o| find_prop(o, "id"))
        .and_then(string_value)
        .map(|s| o::literal(LiteralValue::String(s), None));

    let meta = R3NgModuleMetadata::Global(R3NgModuleMetadataGlobal {
        common: R3NgModuleCommon {
            r#type: directive_ref(class_name),
            selector_scope_mode: R3SelectorScopeMode::SideEffect,
            schemas: None,
            id,
        },
        bootstrap,
        declarations,
        public_declaration_types: None,
        imports,
        include_import_types: true,
        exports,
        contains_forward_decls: false,
    });
    let compiled = compile_ng_module(&meta);

    Ok(ClassEmit {
        class_name: class_name.to_string(),
        static_member: "\u{0275}mod",
        def_expression: compiled.expression,
        // `compile_ng_module` emits the `ɵɵsetNgModuleScope` / `ɵɵregisterNgModuleType` side-effect
        // statements; they belong AFTER the class's `ɵmod` assignment.
        extra_statements: compiled.statements,
        extra_after_def: true,
        factory: Some(empty_factory(class_name, FactoryTarget::NgModule)),
        errors: Vec::new(),
    })
}

/// Resolve an array literal of bare class identifiers into their self-references (`Foo` →
/// `{value: Foo, ty: Foo}`). Nested arrays are flattened; non-identifier entries are skipped.
fn identifier_refs(expr: &Expression) -> Vec<DirRef> {
    let mut out: Vec<DirRef> = Vec::new();
    collect_identifier_refs(expr, &mut out);
    out
}

fn collect_identifier_refs(expr: &Expression, out: &mut Vec<DirRef>) {
    let Expression::ArrayExpression(arr) = expr else {
        return;
    };
    for el in &arr.elements {
        let Some(inner) = el.as_expression() else { continue };
        match inner {
            Expression::ArrayExpression(_) => collect_identifier_refs(inner, out),
            Expression::Identifier(id) => out.push(directive_ref(id.name.as_str())),
            Expression::ParenthesizedExpression(p) => collect_identifier_refs(&p.expression, out),
            _ => {}
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
    selector_candidates: &[crate::binder::SelectorDirective],
    styles: Vec<String>,
    encapsulation: ViewEncapsulation,
    animations: Option<Expr>,
    foreign_imports: Option<Vec<R3ForeignComponentMetadata>>,
    view_providers: Option<Expr>,
) -> Result<ClassEmit, String> {
    let class_name = base.name.clone();
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
    let mut declarations =
        crate::compile::resolve_template_dependencies(&candidates, &selectorless_nodes);

    // CROSS-CLASS CSS-SELECTOR MATCHING: feed every sibling-declared/imported directive selector
    // into the binder's real CssSelector matcher over the SAME bound template the component emits.
    // Directives matched by an attribute/property/output binding, on `ng-template`/`ng-container`,
    // or as a structural directive (`*dir`) are added to `dependencies` HERE — the selectorless
    // pass above only finds class-name (`<Foo>`/`@Foo`) references. Matches already present from
    // the selectorless pass are not duplicated.
    if !selector_candidates.is_empty() {
        let already: std::collections::HashSet<String> = declarations
            .iter()
            .filter_map(|d| match &d.ty.kind {
                crate::output_ast::ExprKind::ReadVar { name } => Some(name.clone()),
                _ => None,
            })
            .collect();
        let matched = crate::binder::resolve_selector_dependencies(&r3.nodes, selector_candidates);
        for name in matched {
            if !already.contains(&name) {
                declarations.push(R3TemplateDependencyMetadata {
                    kind: crate::view::compiler::R3TemplateDependencyKind::Directive,
                    ty: o::variable(&name, None),
                });
            }
        }
    }

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
        view_providers,
        relative_context_file_path: String::new(),
        i18n_use_external_ids: false,
        change_detection: Some(ChangeDetection::Strategy(change_detection)),
        relative_template_path: None,
        has_directive_dependencies,
        raw_imports: None,
        foreign_imports,
    };

    let mut template_builder = ForeignAwareTemplateBuilder::default();
    // R2: the production host-bindings generator (was `StubHostBindingsBuilder`). For a component
    // with no host bindings it is a no-op (emits no `hostBindings`/`hostAttrs`/`hostVars`), so this
    // is byte-identical to the stub for the existing no-host cases.
    let mut host_builder = DefaultHostBindingsBuilder;
    let mut pool_statements = Vec::new();
    let compiled: R3CompiledExpression = compile_component_from_metadata(
        &mut meta,
        &mut template_builder,
        &mut host_builder,
        &mut pool_statements,
    );

    // Angular emits the `ConstantPool.statements` (hoisted query-predicate `const _cN = [...]`
    // declarations and nested-view `function …_Template` functions) as top-level siblings BEFORE
    // the `ɵɵdefineComponent({…})` call: the definition references the `_cN`/template names, so they
    // must be declared first. `class_static_statements` keeps that ordering (`extra_after_def:false`),
    // and `assemble_module` places the whole block right after the kept class declaration.
    Ok(ClassEmit {
        class_name: class_name.clone(),
        static_member: "\u{0275}cmp",
        def_expression: compiled.expression,
        extra_statements: pool_statements,
        extra_after_def: false,
        factory: Some(empty_factory(&class_name, FactoryTarget::Component)),
        errors,
    })
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const ZWS: &str = "\u{0275}\u{0275}defineComponent";

    /// Collapse all runs of ASCII whitespace (incl. the emitter's tabs/newlines used to
    /// pretty-print object/array literals) to a single space so multi-line emitted code can be
    /// matched against the single-line golden `features:` excerpts.
    fn normalize_ws(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

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
        // `Bar` import must NOT. (The emit is now a COMPLETE module, so the ORIGINAL `Bar` import
        // statement is correctly preserved verbatim — only the `dependencies` array must exclude it.)
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
        // The complete module keeps the original imports verbatim (Angular does not tree-shake
        // unused imports — the bundler does), and adds the i0 namespace import.
        assert!(
            code.contains("import { Foo } from \"./foo\""),
            "original Foo import dropped; got: {code}"
        );
        assert!(
            code.contains("import { Bar } from \"./bar\""),
            "original Bar import dropped; got: {code}"
        );
        assert!(
            code.contains("import * as i0 from \"@angular/core\""),
            "missing i0 namespace import; got: {code}"
        );
        // The kept class declaration (decorator stripped) and the appended Ivy statics.
        assert!(code.contains("export class C"), "class C not kept; got: {code}");
        assert!(code.contains("C.\u{0275}cmp ="), "no ɵcmp assignment; got: {code}");
        // Used `<Foo>` lands in `dependencies`; unused `Bar` must NOT appear in that array.
        let deps = extract_balanced(code, "dependencies:")
            .unwrap_or_else(|| panic!("no dependencies array; got: {code}"));
        assert!(deps.contains("Foo"), "Foo not in dependencies; got: {deps}");
        assert!(
            !deps.contains("Bar"),
            "unused import Bar leaked into dependencies; got: {deps}"
        );
    }

    /// Extract the balanced `[...]` array value that follows `key` in `code` (e.g.
    /// `dependencies:[...]`). Returns the slice including the brackets, or `None` if absent.
    fn extract_balanced(code: &str, key: &str) -> Option<String> {
        let at = code.find(key)?;
        let open = code[at..].find('[')? + at;
        let bytes = code.as_bytes();
        let mut depth = 0i32;
        let mut i = open;
        while i < bytes.len() {
            match bytes[i] {
                b'[' => depth += 1,
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(code[open..=i].to_string());
                    }
                }
                _ => {}
            }
            i += 1;
        }
        None
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
    fn component_providers_only_emits_providers_feature() {
        // Mirrors r3_view_compiler_providers/providers_feature_providers_only.ts. Angular folds the
        // `providers` array into a single `ɵɵProvidersFeature([...])` feature entry.
        let src = r#"@Component({
            selector:"my-component",
            template:"<div></div>",
            providers:[GreeterEN, { provide: Greeter, useClass: GreeterEN }],
            standalone:false
        }) export class MyComponent {}"#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        // The emitter pretty-prints the provider object across lines; collapse whitespace before
        // matching (the compliance harness normalizes the same way against the golden excerpt).
        let flat = normalize_ws(&out.code);
        assert!(
            flat.contains(
                "\u{0275}\u{0275}ProvidersFeature([GreeterEN, { provide: Greeter, useClass: GreeterEN }])"
            ),
            "expected providers-only ProvidersFeature; got: {flat}"
        );
    }

    #[test]
    fn component_view_providers_only_emits_empty_first_arg() {
        // Mirrors providers_feature_view_providers_only.ts: viewProviders with no providers emits an
        // empty array as the first `ɵɵProvidersFeature` argument and the viewProviders as the second.
        let src = r#"@Component({
            selector:"my-component",
            template:"<div></div>",
            viewProviders:[GreeterEN],
            standalone:false
        }) export class MyComponent {}"#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let flat = normalize_ws(&out.code);
        assert!(
            flat.contains("\u{0275}\u{0275}ProvidersFeature([], [GreeterEN])"),
            "expected empty-providers + viewProviders ProvidersFeature; got: {flat}"
        );
    }

    #[test]
    fn component_providers_and_view_providers_emits_both_args() {
        // Mirrors providers_feature_providers_and_view_providers.ts.
        let src = r#"@Component({
            selector:"my-component",
            template:"<div></div>",
            providers:[GreeterEN, { provide: Greeter, useClass: GreeterEN }],
            viewProviders:[GreeterEN],
            standalone:false
        }) export class MyComponent {}"#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let flat = normalize_ws(&out.code);
        assert!(
            flat.contains(
                "\u{0275}\u{0275}ProvidersFeature([GreeterEN, { provide: Greeter, useClass: GreeterEN }], [GreeterEN])"
            ),
            "expected providers + viewProviders ProvidersFeature; got: {flat}"
        );
    }

    #[test]
    fn directive_providers_emits_providers_feature() {
        // A `@Directive` carrying `providers` also folds into a ProvidersFeature on defineDirective.
        let src = r#"@Directive({
            selector:"[my-dir]",
            providers:[Svc],
            standalone:false
        }) export class MyDir {}"#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let flat = normalize_ws(&out.code);
        assert!(
            flat.contains("\u{0275}\u{0275}ProvidersFeature([Svc])"),
            "expected directive ProvidersFeature; got: {flat}"
        );
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

    // -----------------------------------------------------------------------
    // Additive source-map emission.
    // -----------------------------------------------------------------------

    #[test]
    fn with_map_code_is_byte_identical_to_plain() {
        // HARD GATE: the additive map path must not change a single byte of the emitted
        // code. Compile the same source both ways and byte-compare.
        let srcs = [
            r#"@Component({selector:"a",template:"<div>{{x}}</div>"}) export class C { x = 1; }"#,
            r#"@Component({selector:"a",template:"<div></div>"}) export class C { name = input(); }"#,
            r#"@Component({selector:"a",template:"<p></p>"})
                export class Widget {
                    @Input() foo = 1;
                    @Output() bar = new EventEmitter();
                }"#,
        ];
        for src in srcs {
            let plain = compile_component_source(src);
            let mapped = compile_component_source_with_map(src, "widget.js", "widget.ts");
            assert!(plain.errors.is_empty(), "plain errors: {:?}", plain.errors);
            assert!(mapped.errors.is_empty(), "mapped errors: {:?}", mapped.errors);
            assert_eq!(
                plain.code, mapped.code,
                "map path changed emitted bytes for src: {src}"
            );
        }
    }

    #[test]
    fn with_map_emits_valid_v3_json() {
        let src =
            r#"@Component({selector:"app-x",template:"<div>{{x}}</div>"}) export class MyComp { x = 1; }"#;
        let out = compile_component_source_with_map(src, "my-comp.js", "my-comp.ts");
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        let map = &out.map;
        // version 3.
        assert!(map.contains("\"version\":3"), "no version 3; got: {map}");
        // sources non-empty and names the original.
        assert!(map.contains("\"sources\":[\"my-comp.ts\"]"), "bad sources; got: {map}");
        // sourcesContent present and carries the original source.
        assert!(
            map.contains("export class MyComp"),
            "sourcesContent missing original; got: {map}"
        );
        // file names the generated artifact.
        assert!(map.contains("\"file\":\"my-comp.js\""), "bad file; got: {map}");
        // mappings field present.
        assert!(map.contains("\"mappings\":\""), "no mappings; got: {map}");
    }

    #[test]
    fn with_map_maps_class_name_back_to_source() {
        // The emitted `type: MyComp` token must map back to the class declaration's
        // `MyComp` position in the original source.
        let src =
            r#"@Component({selector:"app-x",template:"<div></div>"}) export class MyComp { }"#;
        let out = compile_component_source_with_map(src, "my-comp.js", "my-comp.ts");
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);

        let mappings = extract_mappings(&out.map);
        let decoded =
            crate::output::source_map::decode_mappings(&mappings).expect("decode mappings");
        assert!(!decoded.is_empty(), "expected at least one segment; map: {}", out.map);

        // The original `MyComp` token starts at this byte offset in `src`.
        let class_byte = src.find("MyComp").expect("MyComp in source");
        let expected = crate::output::source_map::byte_offset_to_line_col(src, class_byte);

        // At least one segment must point at the class-name original position.
        let found = decoded
            .iter()
            .any(|&(_, _, _, sl, sc)| sl == expected.line && sc == expected.column);
        assert!(
            found,
            "no segment maps to class-name position (line {}, col {}); decoded: {:?}; map: {}",
            expected.line, expected.column, decoded, out.map
        );
    }

    /// Pull the `mappings` string value out of a v3 JSON blob (test helper — avoids a JSON
    /// dependency; the blob shape is the small, fixed one `SourceMapBuilder::to_json` emits).
    fn extract_mappings(map_json: &str) -> String {
        let key = "\"mappings\":\"";
        let start = map_json.find(key).expect("mappings key") + key.len();
        let rest = &map_json[start..];
        let end = rest.find('"').expect("mappings close quote");
        rest[..end].to_string()
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

    /// Whitespace-free canonical form for substring assertions on emitted code.
    fn flat(code: &str) -> String {
        code.chars().filter(|c| !c.is_whitespace()).collect()
    }

    // -----------------------------------------------------------------------
    // R1 — multi-class files.
    // -----------------------------------------------------------------------

    #[test]
    fn multi_class_emits_each_definition() {
        // A component + an NgModule in one file: BOTH definitions emit.
        let src = r#"
            @Component({selector: 'a', template: '<div></div>', standalone: false})
            export class CompA {}

            @NgModule({declarations: [CompA]})
            export class MyModule {}
        "#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(out.code.contains(ZWS), "no defineComponent; got: {}", out.code);
        assert!(
            out.code.contains("\u{0275}\u{0275}defineNgModule"),
            "no defineNgModule; got: {}",
            out.code
        );
        // The module declarations reference the component class.
        assert!(
            flat(&out.code).contains("declarations:[CompA]"),
            "module declarations missing CompA; got: {}",
            out.code
        );
    }

    #[test]
    fn multi_class_cross_class_auto_import() {
        // A sibling-declared directive used by class name in another component's template lands in
        // that component's `dependencies` array WITHOUT any imports array or selector.
        let src = r#"
            @Directive({selector: '[sib]', standalone: false})
            export class Sib {}

            @Component({selector: 'host', template: '<Sib></Sib>', standalone: false})
            export class HostCmp {}
        "#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(
            out.code.contains("dependencies"),
            "no dependencies array; got: {}",
            out.code
        );
        assert!(out.code.contains("Sib"), "Sib not referenced; got: {}", out.code);
    }

    // -----------------------------------------------------------------------
    // R2 — host bindings.
    // -----------------------------------------------------------------------

    #[test]
    fn host_object_listener_and_property() {
        // `host: { '(click)': 'onClick()', '[id]': "x" }` emits a hostBindings fn with a listener
        // (create) and a domProperty (update), plus hostVars.
        let src = r#"
            @Component({
                selector: 'my-cmp',
                host: { '(click)': 'onClick()', '[id]': 'x' },
                template: '<div></div>',
                standalone: false
            })
            export class MyComponent { x = 1; onClick() {} }
        "#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let f = flat(&out.code);
        assert!(f.contains("hostBindings"), "no hostBindings; got: {}", out.code);
        assert!(
            out.code.contains("\u{0275}\u{0275}listener"),
            "no listener instruction; got: {}",
            out.code
        );
        assert!(
            out.code.contains("\u{0275}\u{0275}domProperty"),
            "no domProperty instruction; got: {}",
            out.code
        );
        assert!(f.contains("hostVars:1"), "hostVars:1 missing; got: {}", out.code);
    }

    #[test]
    fn host_member_decorators_fold_into_host() {
        // `@HostBinding('id') dirId = ...` becomes a `[id]` host property bound to `ctx.dirId`.
        let src = r#"
            @Directive({selector: '[hostBindingDir]', standalone: false})
            export class HostBindingDir {
                @HostBinding('id') dirId = 'some id';
            }
        "#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(
            out.code.contains("\u{0275}\u{0275}defineDirective"),
            "no defineDirective; got: {}",
            out.code
        );
        let f = flat(&out.code);
        assert!(f.contains("hostVars:1"), "hostVars:1 missing; got: {}", out.code);
        assert!(
            out.code.contains("\u{0275}\u{0275}domProperty(\"id\",ctx.dirId)")
                || flat(&out.code).contains("domProperty(\"id\",ctx.dirId)"),
            "domProperty(id, ctx.dirId) missing; got: {}",
            out.code
        );
    }

    #[test]
    fn host_directives_emit_feature() {
        let src = r#"
            @Component({
                selector: 'my-cmp',
                template: '<div></div>',
                hostDirectives: [DirA, {directive: DirB, inputs: ['value: alias'], outputs: ['ev']}],
                standalone: false
            })
            export class MyComponent {}
        "#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(
            out.code.contains("\u{0275}\u{0275}HostDirectivesFeature"),
            "no HostDirectivesFeature; got: {}",
            out.code
        );
        let f = flat(&out.code);
        // DirA shorthand; DirB object with input/output mapping arrays.
        assert!(f.contains("DirA"), "DirA missing; got: {}", out.code);
        assert!(
            f.contains("directive:DirB"),
            "DirB object missing; got: {}",
            out.code
        );
        assert!(
            f.contains("inputs:[\"value\",\"alias\"]"),
            "input mapping wrong; got: {}",
            out.code
        );
        assert!(
            f.contains("outputs:[\"ev\",\"ev\"]"),
            "output mapping wrong; got: {}",
            out.code
        );
    }

    // -----------------------------------------------------------------------
    // R3 — decorator queries.
    // -----------------------------------------------------------------------

    #[test]
    fn decorator_view_queries_emit_view_query_fn() {
        let src = r#"
            @Component({
                selector: 'view-query-component',
                template: '<div #myRef></div><div #myRef1></div>',
                standalone: false
            })
            export class ViewQueryComponent {
                @ViewChild('myRef') myRef: any;
                @ViewChildren('myRef1, myRef2, myRef3') myRefs!: any;
            }
        "#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(
            out.code.contains("\u{0275}\u{0275}viewQuery"),
            "no viewQuery instruction; got: {}",
            out.code
        );
        // Multi-selector locator splits into three refs.
        let f = flat(&out.code);
        assert!(
            f.contains("[\"myRef1\",\"myRef2\",\"myRef3\"]"),
            "multi-selector split missing; got: {}",
            out.code
        );
        // Single-result query refreshes `.first`; multi assigns the QueryList directly.
        assert!(f.contains("ctx.myRef="), "myRef refresh missing; got: {}", out.code);
        assert!(f.contains("ctx.myRefs="), "myRefs refresh missing; got: {}", out.code);
    }

    #[test]
    fn decorator_content_query_with_read_token() {
        let src = r#"
            @Directive({selector: '[d]', standalone: false})
            export class D {
                @ContentChild('ref', {read: ElementRef}) item: any;
            }
        "#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(
            out.code.contains("\u{0275}\u{0275}contentQuery"),
            "no contentQuery; got: {}",
            out.code
        );
        // The `read` token is emitted as the 3rd creation arg.
        assert!(out.code.contains("ElementRef"), "read token missing; got: {}", out.code);
    }

    #[test]
    fn decorator_queries_single_first_ordering() {
        // Declaration order: single, multi, single. Creation order must be all-singles then multi.
        let src = r#"
            @Component({selector: 'c', template: '<div></div>', standalone: false})
            export class C {
                @ViewChild('a') a: any;
                @ViewChildren('b') b!: any;
                @ViewChild('c2') c2: any;
            }
        "#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let f = flat(&out.code);
        // The `a` and `c2` predicates (singles) must precede `b` (multi) in the create chain.
        let pos_a = f.find("[\"a\"]").expect("a predicate");
        let pos_c2 = f.find("[\"c2\"]").expect("c2 predicate");
        let pos_b = f.find("[\"b\"]").expect("b predicate");
        assert!(pos_a < pos_b && pos_c2 < pos_b, "single-first ordering wrong; got: {}", out.code);
    }

    // -----------------------------------------------------------------------
    // R4 — @Directive / @Pipe / @NgModule emission.
    // -----------------------------------------------------------------------

    #[test]
    fn directive_only_emits_define_directive() {
        let src = r#"
            @Directive({selector: '[myDir]', standalone: false})
            export class MyDir {
                @Input() foo = 1;
                @Output() bar = new EventEmitter();
            }
        "#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(
            out.code.contains("\u{0275}\u{0275}defineDirective"),
            "no defineDirective; got: {}",
            out.code
        );
        let f = flat(&out.code);
        assert!(
            f.contains("selectors:[[\"\",\"myDir\",\"\"]]"),
            "directive selector wrong; got: {}",
            out.code
        );
        assert!(f.contains("inputs:"), "no inputs; got: {}", out.code);
        assert!(f.contains("outputs:"), "no outputs; got: {}", out.code);
        assert!(f.contains("standalone:false"), "standalone:false missing; got: {}", out.code);
    }

    #[test]
    fn directive_export_as_emitted() {
        let src = r#"
            @Directive({selector: '[d]', exportAs: 'foo', standalone: false})
            export class D {}
        "#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(
            flat(&out.code).contains("exportAs:[\"foo\"]"),
            "exportAs missing; got: {}",
            out.code
        );
    }

    #[test]
    fn pipe_emits_define_pipe() {
        let src = r#"
            @Pipe({name: 'myPipe', standalone: false})
            export class MyPipe { transform(v: any) { return v; } }
        "#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let f = flat(&out.code);
        assert!(
            out.code.contains("\u{0275}\u{0275}definePipe"),
            "no definePipe; got: {}",
            out.code
        );
        assert!(f.contains("name:\"myPipe\""), "pipe name wrong; got: {}", out.code);
        assert!(f.contains("type:MyPipe"), "pipe type wrong; got: {}", out.code);
        assert!(f.contains("pure:true"), "pure default wrong; got: {}", out.code);
        assert!(f.contains("standalone:false"), "standalone:false missing; got: {}", out.code);
    }

    #[test]
    fn ng_module_emits_define_ng_module() {
        let src = r#"
            @NgModule({declarations: [CompA, CompB], imports: [CommonModule], exports: [CompA]})
            export class MyModule {}
        "#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let f = flat(&out.code);
        assert!(
            out.code.contains("\u{0275}\u{0275}defineNgModule"),
            "no defineNgModule; got: {}",
            out.code
        );
        assert!(f.contains("declarations:[CompA,CompB]"), "declarations wrong; got: {}", out.code);
        assert!(f.contains("imports:[CommonModule]"), "imports wrong; got: {}", out.code);
        assert!(f.contains("exports:[CompA]"), "exports wrong; got: {}", out.code);
    }

    #[test]
    fn ng_on_changes_emits_feature() {
        let src = r#"
            @Component({selector: 'c', template: '<div></div>', standalone: false})
            export class C { ngOnChanges() {} }
        "#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(
            out.code.contains("\u{0275}\u{0275}NgOnChangesFeature"),
            "no NgOnChangesFeature; got: {}",
            out.code
        );
    }

    #[test]
    fn no_ng_on_changes_no_feature() {
        let src = r#"
            @Component({selector: 'c', template: '<div></div>', standalone: false})
            export class C {}
        "#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(
            !out.code.contains("NgOnChangesFeature"),
            "spurious NgOnChangesFeature; got: {}",
            out.code
        );
    }

    #[test]
    fn directive_no_host_is_byte_identical_to_legacy_stub() {
        // A @Directive with no host bindings must NOT emit a hostBindings/hostVars/hostAttrs field
        // (the DefaultHostBindingsBuilder is a no-op when there is nothing to bind).
        let src = r#"
            @Directive({selector: '[d]', standalone: false})
            export class D { @Input() foo = 1; }
        "#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(!out.code.contains("hostBindings"), "spurious hostBindings; got: {}", out.code);
        assert!(!out.code.contains("hostVars"), "spurious hostVars; got: {}", out.code);
    }

    // -----------------------------------------------------------------------
    // Complete-ES-module emit — the front-end must emit the ORIGINAL module
    // augmented with the Ivy statics, NOT a synthetic module that replaces the
    // class + imports (the live `doesn't provide an export named: AppRoot` bug).
    // -----------------------------------------------------------------------

    #[test]
    fn emits_complete_module_keeps_class_imports_and_def() {
        // Reproduction of the live bug: the original `export class AppRoot` and the original
        // `RouterOutlet` import must survive in the emitted module; the Ivy statics are appended.
        let src = r#"import { RouterOutlet } from "@angular/router";
@Component({selector:"app-root",template:"<router-outlet></router-outlet>",imports:[RouterOutlet]})
export class AppRoot {}
"#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        // (1) original import kept verbatim.
        assert!(
            code.contains("import { RouterOutlet } from \"@angular/router\""),
            "original RouterOutlet import dropped; got: {code}"
        );
        // (2) i0 namespace import prepended.
        assert!(
            code.contains("import * as i0 from \"@angular/core\""),
            "missing i0 namespace import; got: {code}"
        );
        // (3) the kept class declaration (decorator stripped — no `@Component` left).
        assert!(code.contains("export class AppRoot"), "class AppRoot not kept; got: {code}");
        assert!(
            !code.contains("@Component"),
            "Angular decorator not stripped; got: {code}"
        );
        // (4) the appended Ivy statics referencing the kept class.
        assert!(code.contains("AppRoot.\u{0275}fac ="), "no ɵfac assignment; got: {code}");
        assert!(code.contains("AppRoot.\u{0275}cmp ="), "no ɵcmp assignment; got: {code}");
        // (5) the def block is preserved (the harness extracts THIS unchanged).
        assert!(code.contains(ZWS), "no defineComponent; got: {code}");
        assert!(code.contains("type: AppRoot"), "def missing type ref; got: {code}");
        // The assignment wraps the def block: `AppRoot.ɵcmp = i0.ɵɵdefineComponent({`.
        assert!(
            flat(code).contains(&format!("AppRoot.{}=i0.{}({{", "\u{0275}cmp", ZWS)),
            "ɵcmp assignment does not wrap the def block; got: {code}"
        );
        // (6) the emitted module is syntactically valid TS (it parses with zero errors) — the bug it
        // fixes was a module that referenced undeclared `AppRoot`/`RouterOutlet`; a complete module
        // both declares/imports them and parses cleanly.
        assert_parses(code);
    }

    /// Assert the emitted module is syntactically valid TypeScript (parses with zero errors).
    fn assert_parses(code: &str) {
        let allocator = Allocator::default();
        let source_type = SourceType::default().with_typescript(true);
        let ret = Parser::new(&allocator, code, source_type).parse();
        assert!(
            ret.errors.is_empty(),
            "emitted module is not valid TS: {:?}\n--- module ---\n{code}",
            ret.errors.iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn complete_module_directive_keeps_class_and_appends_dir() {
        let src = r#"import { Foo } from "./foo";
@Directive({selector:"[bar]"})
export class BarDir {}
"#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(code.contains("import { Foo } from \"./foo\""), "import dropped; got: {code}");
        assert!(code.contains("export class BarDir"), "class not kept; got: {code}");
        assert!(code.contains("BarDir.\u{0275}fac ="), "no ɵfac; got: {code}");
        assert!(code.contains("BarDir.\u{0275}dir ="), "no ɵdir; got: {code}");
        assert!(
            code.contains("\u{0275}\u{0275}defineDirective"),
            "no defineDirective; got: {code}"
        );
        assert_parses(code);
    }

    #[test]
    fn complete_module_pipe_keeps_class_and_appends_pipe() {
        let src = r#"@Pipe({name:"up"})
export class UpPipe { transform(x: string) { return x; } }
"#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(code.contains("export class UpPipe"), "class not kept; got: {code}");
        // The method body of the kept class survives verbatim.
        assert!(code.contains("transform(x: string)"), "class body dropped; got: {code}");
        assert!(code.contains("UpPipe.\u{0275}fac ="), "no ɵfac; got: {code}");
        assert!(code.contains("UpPipe.\u{0275}pipe ="), "no ɵpipe; got: {code}");
        assert!(code.contains("\u{0275}\u{0275}definePipe"), "no definePipe; got: {code}");
        assert_parses(code);
    }

    #[test]
    fn complete_module_ng_module_keeps_class_and_scope_side_effect_after_def() {
        let src = r#"@NgModule({declarations:[CompA], id:"m"})
export class MyModule {}
"#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(code.contains("export class MyModule"), "class not kept; got: {code}");
        assert!(code.contains("MyModule.\u{0275}fac ="), "no ɵfac; got: {code}");
        assert!(code.contains("MyModule.\u{0275}mod ="), "no ɵmod; got: {code}");
        assert!(
            code.contains("\u{0275}\u{0275}defineNgModule"),
            "no defineNgModule; got: {code}"
        );
        // The selector-scope side effect / registration runs AFTER the `ɵmod` assignment.
        let mod_at = code.find("MyModule.\u{0275}mod =").expect("ɵmod present");
        let scope_at = code
            .find("\u{0275}\u{0275}setNgModuleScope")
            .or_else(|| code.find("\u{0275}\u{0275}registerNgModuleType"))
            .expect("a scope side effect present");
        assert!(
            scope_at > mod_at,
            "NgModule scope side effect must follow the ɵmod assignment; got: {code}"
        );
    }

    #[test]
    fn complete_module_multi_class_interleaves_each_class_with_its_statics() {
        // Two decorated classes + a plain (non-Angular) statement in between: each class is kept in
        // source order with its statics appended right after it, and the plain statement survives.
        let src = r#"import { NgIf } from "@angular/common";
@Component({selector:"a-cmp",template:"<div></div>"})
export class ACmp {}
export const VERSION = "1.0";
@Component({selector:"b-cmp",template:"<span></span>"})
export class BCmp {}
"#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        // Both classes kept, both defs emitted, the plain const preserved.
        assert!(code.contains("export class ACmp"), "ACmp not kept; got: {code}");
        assert!(code.contains("export class BCmp"), "BCmp not kept; got: {code}");
        assert!(code.contains("export const VERSION = \"1.0\""), "VERSION dropped; got: {code}");
        assert!(code.contains("ACmp.\u{0275}cmp ="), "no ACmp def; got: {code}");
        assert!(code.contains("BCmp.\u{0275}cmp ="), "no BCmp def; got: {code}");
        // Source order: ACmp decl -> ACmp statics -> VERSION -> BCmp decl -> BCmp statics.
        let a_class = code.find("export class ACmp").unwrap();
        let a_cmp = code.find("ACmp.\u{0275}cmp =").unwrap();
        let version = code.find("export const VERSION").unwrap();
        let b_class = code.find("export class BCmp").unwrap();
        let b_cmp = code.find("BCmp.\u{0275}cmp =").unwrap();
        assert!(
            a_class < a_cmp && a_cmp < version && version < b_class && b_class < b_cmp,
            "module statements out of source order; got: {code}"
        );
        assert_parses(code);
    }

    #[test]
    fn complete_module_no_imports_prepends_i0_at_top() {
        // With no original imports, the i0 namespace import must still be prepended at the top.
        let src = r#"@Component({selector:"a",template:"<div></div>"}) export class C {}"#;
        let out = compile_component_source(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(
            code.starts_with("import * as i0 from \"@angular/core\""),
            "i0 import not at top; got: {code}"
        );
        assert!(code.contains("export class C"), "class not kept; got: {code}");
    }
}

// ---------------------------------------------------------------------------
// Corpus dump (CARGO-based compliance verification, NOT the live NAPI addon).
//
// This `#[cfg(test)]` helper walks Angular's vendored compliance corpus, runs
// `compile_component_source` over every single-input case, and writes a JSON dump
// (`{ "<corpus-rel-input-path>": {code, errors} }`) to the path named by the
// `RENDER3_CORPUS_DUMP` env var. `libs/render3/compliance/run-compliance.mjs`
// consumes it via `--cargo-dump=<path>`, applying the SAME canonicalize/matchGolden
// logic — so the score is verified against a freshly-built render3 WITHOUT rebuilding
// the `authoring_node` addon (which links the sibling-edited `treaty_runtime`).
//
// It is gated on the env var so a normal `cargo test -p render3` does not require the
// corpus to be present. Run with:
//   RENDER3_CORPUS_DUMP=<abs path> cargo test -p render3 corpus_dump -- --ignored --nocapture
// ---------------------------------------------------------------------------
#[cfg(test)]
mod corpus_dump {
    use super::compile_component_source;
    use std::path::{Path, PathBuf};

    /// JSON-escape a string into `out`.
    fn json_escape(s: &str, out: &mut String) {
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('"');
    }

    /// Minimal extraction of `"inputFiles": ["x.ts"]` arrays from a TEST_CASES.json blob, returning
    /// every referenced single input-file name. We avoid a JSON dependency: the schema is fixed and
    /// we only need the input-file string list per case.
    fn input_files_in(json: &str) -> Vec<String> {
        let mut files = Vec::new();
        let needle = "\"inputFiles\"";
        let mut idx = 0;
        while let Some(found) = json[idx..].find(needle) {
            let start = idx + found + needle.len();
            // Find the '[' then ']'.
            let Some(open_rel) = json[start..].find('[') else { break };
            let open = start + open_rel;
            let Some(close_rel) = json[open..].find(']') else { break };
            let close = open + close_rel;
            let arr = &json[open + 1..close];
            for piece in arr.split(',') {
                let t = piece.trim().trim_matches('"');
                if !t.is_empty() {
                    files.push(t.to_string());
                }
            }
            idx = close;
        }
        files
    }

    /// Recursively collect every `TEST_CASES.json` under `root`.
    fn collect_test_cases(root: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(root) else { return };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                collect_test_cases(&p, out);
            } else if p.file_name().and_then(|n| n.to_str()) == Some("TEST_CASES.json") {
                out.push(p);
            }
        }
    }

    #[test]
    #[ignore = "corpus dump; run explicitly with RENDER3_CORPUS_DUMP set"]
    fn dump_corpus() {
        let Ok(dump_path) = std::env::var("RENDER3_CORPUS_DUMP") else {
            eprintln!("RENDER3_CORPUS_DUMP not set; skipping corpus dump");
            return;
        };
        // libs/render3 -> repo root -> tools/angular-ref/.../test_cases.
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let corpus = manifest
            .join("..")
            .join("..")
            .join("tools/angular-ref/packages/compiler-cli/test/compliance/test_cases");
        let corpus = corpus.canonicalize().unwrap_or(corpus);

        let mut test_case_files = Vec::new();
        collect_test_cases(&corpus, &mut test_case_files);

        let mut json = String::from("{\n");
        let mut first = true;
        let mut count = 0usize;
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for tc in &test_case_files {
            let Ok(content) = std::fs::read_to_string(tc) else { continue };
            let dir = tc.parent().unwrap();
            for input in input_files_in(&content) {
                let input_path = dir.join(&input);
                let Ok(src) = std::fs::read_to_string(&input_path) else { continue };
                let rel = input_path
                    .strip_prefix(&corpus)
                    .unwrap_or(&input_path)
                    .to_string_lossy()
                    .replace('\\', "/");
                if !seen.insert(rel.clone()) {
                    continue;
                }
                let out = compile_component_source(&src);
                if !first {
                    json.push_str(",\n");
                }
                first = false;
                json_escape(&rel, &mut json);
                json.push_str(":{\"code\":");
                json_escape(&out.code, &mut json);
                json.push_str(",\"errors\":[");
                for (i, e) in out.errors.iter().enumerate() {
                    if i > 0 {
                        json.push(',');
                    }
                    json_escape(e, &mut json);
                }
                json.push_str("]}");
                count += 1;
            }
        }
        json.push_str("\n}\n");
        std::fs::write(&dump_path, json).expect("write corpus dump");
        eprintln!("wrote {count} corpus entries to {dump_path}");
    }
}
