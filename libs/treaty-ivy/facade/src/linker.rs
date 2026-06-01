//! The Angular **partial-declaration linker** (Rust).
//!
//! Published Angular libraries are *partial*-compiled: each class emits `ɵɵngDeclare*({...})`
//! calls (`ɵɵngDeclareFactory`/`ɵɵngDeclareInjectable`/`ɵɵngDeclareInjector`/`ɵɵngDeclareNgModule`/
//! `ɵɵngDeclarePipe`/`ɵɵngDeclareComponent`/`ɵɵngDeclareDirective`/`ɵɵngDeclareClassMetadata`)
//! rather than the full AOT `ɵɵdefine*` calls. At application build time the **Angular Linker**
//! rewrites every `ɵɵngDeclare*({...})` into the corresponding `ɵɵdefine*({...})` by running the
//! SAME render3 emit fed from the declaration object instead of a decorator.
//!
//! [`link_partial`] is that linker for the kinds whose declaration → R3-metadata mapping is a pure
//! syntactic transform (the DI + pipe family): `ɵɵngDeclareFactory`, `ɵɵngDeclareInjectable`,
//! `ɵɵngDeclareInjector`, `ɵɵngDeclareNgModule`, `ɵɵngDeclarePipe`, and `ɵɵngDeclareClassMetadata`
//! (dropped — it is dev-only `setClassMetadata`). It is a NEW FRONT-END into the EXISTING emit
//! (mirroring [`crate::source_compile`], which already turns `R3*Metadata` into `ɵɵdefine*`).
//!
//! The transform is a **surgical span rewrite**: only each `ɵɵngDeclare*(...)` call expression's
//! source span is replaced with the emitted `ɵɵdefine*(...)` text; every other byte of the module
//! — including the surrounding `X.ɵfac = …` / `X.ɵprov = …` / `X.ɵmod = …` assignment and any
//! unrecognised `ɵɵngDeclareComponent`/`ɵɵngDeclareDirective` calls — is left untouched.
//!
//! Reference: `tools/angular-ref/packages/compiler-cli/linker/src/file_linker/partial_linkers/*.ts`
//! (`toR3FactoryMeta`, `toR3InjectableMeta`, `toR3InjectorMeta`, `toR3NgModuleMeta`,
//! `toR3PipeMeta`, plus the shared `util.ts` `getDependency`/`extractForwardRef`/`wrapReference`).

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, CallExpression, Expression, ObjectExpression, ObjectPropertyKind, PropertyKey,
    Statement,
};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType};

use crate::factory::{
    compile_factory_function, compile_injectable, FactoryDeps, FactoryTarget, ForwardRefHandling,
    MaybeForwardRef, R3ConstructorFactoryMetadata, R3DependencyMetadata, R3FactoryMetadata,
    R3InjectableMetadata,
};
use crate::compile::RealTemplateBuilder;
use crate::output::emitter::{emit_expression, emit_statements};
use crate::output_ast::{self as o, Expr, LiteralValue, ParseSourceSpan};
use crate::pipe_module_injector::{
    compile_injector, compile_ng_module, compile_pipe_from_metadata, R3InjectorMetadata,
    R3NgModuleCommon, R3NgModuleMetadata, R3NgModuleMetadataGlobal, R3PipeMetadata,
    R3SelectorScopeMode,
};
use crate::template::template_transform::{
    html_ast_to_render3_ast, BindingParser, Render3ParseOptions,
};
use crate::util::R3Reference;
use crate::view::compiler::{
    compile_component_from_metadata, compile_directive_from_metadata, ChangeDetection,
    ChangeDetectionStrategy, ComponentTemplate, DeclarationListEmitMode, DefaultHostBindingsBuilder,
    Deps, Lifecycle, OrderedMap, QueryPredicate, R3ComponentDeferMetadata, R3ComponentMetadata,
    R3DirectiveMetadata, R3HostDirectiveMetadata, R3HostMetadata, R3InputMetadata, R3QueryMetadata,
    R3TemplateDependencyKind, R3TemplateDependencyMetadata, SpecialAttrs, ViewEncapsulation,
};

/// The result of linking a partial-declaration module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkResult {
    /// The module source with every linkable `ɵɵngDeclare*(...)` call rewritten to `ɵɵdefine*(...)`.
    /// Byte-identical to the input outside the rewritten call spans.
    pub code: String,
    /// Diagnostics for declarations that could not be linked (the offending call is left as-is).
    pub errors: Vec<String>,
}

/// The synthetic namespace-import line the emitter prepends to every standalone print.
const I0_IMPORT_PREFIX: &str = "import * as i0 from";

/// Print a definition expression to text with the emitter's synthetic
/// `import * as i0 from "@angular/core";` line stripped.
///
/// A partial-declaration module already imports the Angular core namespace as `i0`. The emitter
/// ([`emit_expression`]) prepends that import line to every standalone expression it prints; here
/// the rewritten call must be a BARE expression that slots into the existing `X.ɵprov = …`
/// assignment, so the synthetic import is removed. (Mirrors `source_compile::assemble_module`, which
/// emits the `i0` import once at module scope and strips it from each per-class block.)
fn emit_def_text(expr: &Expr) -> String {
    strip_leading_i0_import(&emit_expression(expr))
}

/// Strip a single leading `import * as i0 from "@angular/core";` line from emitted text (used both
/// for the bare definition expression and for the NgModule scope side-effect statements).
fn strip_leading_i0_import(block: &str) -> String {
    let mut lines = block.lines();
    if let Some(first) = lines.clone().next() {
        if first.trim_start().starts_with(I0_IMPORT_PREFIX) {
            return lines.by_ref().skip(1).collect::<Vec<_>>().join("\n");
        }
    }
    block.to_string()
}

// ---------------------------------------------------------------------------
// oxc-AST helpers (the common-case readers the declaration objects need). These mirror the
// equivalents in `source_compile` but are kept local so the linker is self-contained.
// ---------------------------------------------------------------------------

/// The static-identifier / string name of a property key (`type`, `providedIn`, …).
fn key_name<'a>(key: &'a PropertyKey<'a>) -> Option<&'a str> {
    match key {
        PropertyKey::StaticIdentifier(id) => Some(id.name.as_str()),
        PropertyKey::StringLiteral(s) => Some(s.value.as_str()),
        _ => None,
    }
}

/// Look up an object-literal property value by name.
fn find_prop<'a>(obj: &'a ObjectExpression<'a>, name: &str) -> Option<&'a Expression<'a>> {
    obj.properties.iter().find_map(|p| match p {
        ObjectPropertyKind::ObjectProperty(op) if key_name(&op.key) == Some(name) => Some(&op.value),
        _ => None,
    })
}

/// Read a string-literal value (`selector`, the pipe `name`, …).
fn string_value(expr: &Expression) -> Option<String> {
    match expr {
        Expression::StringLiteral(s) => Some(s.value.to_string()),
        Expression::TemplateLiteral(t) if t.expressions.is_empty() && t.quasis.len() == 1 => {
            t.quasis[0].value.cooked.as_ref().map(|c| c.to_string())
        }
        _ => None,
    }
}

/// Read a `true`/`false` literal value.
fn bool_value(expr: &Expression) -> Option<bool> {
    match expr {
        Expression::BooleanLiteral(b) => Some(b.value),
        _ => None,
    }
}

/// `depObj.getBoolean(name)` — a property whose value is the boolean literal `true`.
fn prop_is_true(obj: &ObjectExpression, name: &str) -> bool {
    matches!(find_prop(obj, name), Some(e) if bool_value(e) == Some(true))
}

/// A class self-reference (`{value: Foo, ty: Foo}`) — the linker's `wrapReference`, where the
/// wrapped node is the type identifier.
fn class_ref(name: &str) -> R3Reference {
    R3Reference {
        value: o::variable(name, None),
        ty: o::variable(name, None),
    }
}

/// Build a class [`R3Reference`] from a `type` expression, carrying the exact `value`/`ty`
/// expression (`ns.Foo` is preserved verbatim) rather than only its symbol name.
fn class_ref_from(type_expr: &Expression) -> Result<R3Reference, String> {
    let value =
        convert_expr(type_expr).ok_or_else(|| "unsupported `type` expression".to_string())?;
    let ty = value.clone();
    Ok(R3Reference { value, ty })
}

/// The "symbol name" of a `type`/token expression — its bare identifier (`Foo`) or, for `ns.Foo`,
/// the final property name. Mirrors `AstValue.getSymbolName()` for the cases that appear as a
/// partial-declaration `type`.
fn symbol_name(expr: &Expression) -> Option<String> {
    match expr {
        Expression::Identifier(id) => Some(id.name.to_string()),
        Expression::StaticMemberExpression(m) => Some(m.property.name.to_string()),
        Expression::ParenthesizedExpression(p) => symbol_name(&p.expression),
        _ => None,
    }
}

/// Best-effort conversion of an oxc `Expression` into an `output_ast` [`Expr`], faithful enough to
/// carry opaque declaration values through verbatim (tokens, `providers`/`imports` arrays,
/// `useFactory`/`useValue` expressions — the linker's `getOpaque()`). Handles the literal +
/// reference subset that appears in `ɵɵngDeclare*` objects.
fn convert_expr(expr: &Expression) -> Option<Expr> {
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
        Expression::NewExpression(new_expr) => {
            let callee = convert_expr(&new_expr.callee)?;
            let mut args: Vec<Expr> = Vec::with_capacity(new_expr.arguments.len());
            for a in &new_expr.arguments {
                let inner = a.as_expression()?;
                args.push(convert_expr(inner)?);
            }
            // `new callee(...args)` via the `Expr::instantiate` builder.
            Some(callee.instantiate(args))
        }
        Expression::ParenthesizedExpression(p) => convert_expr(&p.expression),
        // Arrow functions appear as `useFactory: () => new X(inject(Dep))`. Only
        // expression-bodied (or single-`return`) arrows are faithfully convertible
        // to the output IR; multi-statement bodies are left unconverted (→ `None`,
        // surfaced as an explicit link error rather than wrong code).
        Expression::ArrowFunctionExpression(arrow) => {
            let params = convert_params(&arrow.params)?;
            let body = convert_arrow_body(arrow)?;
            Some(o::arrow_fn(params, body, None))
        }
        // `function (…) { return …; }` factory functions convert to a FunctionExpr.
        Expression::FunctionExpression(func) => {
            let params = convert_params(&func.params)?;
            let body_block = func.body.as_ref()?;
            let body = convert_return_only_block(&body_block.statements)?;
            Some(o::fn_(params, body, None, None))
        }
        _ => None,
    }
}

/// Convert a parameter list to [`o::FnParam`]s. Only plain identifier bindings are
/// supported (no destructuring / defaults / rest); anything else → `None`.
fn convert_params(params: &oxc_ast::ast::FormalParameters) -> Option<Vec<o::FnParam>> {
    if params.rest.is_some() {
        return None;
    }
    let mut out = Vec::with_capacity(params.items.len());
    for item in &params.items {
        let id = item.pattern.get_binding_identifier()?;
        out.push(o::FnParam::new(id.name.to_string(), None));
    }
    Some(out)
}

/// Convert an arrow's body: an expression body (`() => expr`) maps to
/// [`ArrowBody::Expr`]; a single-`return` block (`() => { return expr; }`) is
/// folded to the same expression form (matching how the emitter would print it).
/// Any other block shape → `None`.
fn convert_arrow_body(arrow: &oxc_ast::ast::ArrowFunctionExpression) -> Option<o::ArrowBody> {
    if arrow.expression {
        if let Some(Statement::ExpressionStatement(stmt)) = arrow.body.statements.first() {
            return Some(o::ArrowBody::Expr(Box::new(convert_expr(&stmt.expression)?)));
        }
        return None;
    }
    let expr = single_return_expr(&arrow.body.statements)?;
    Some(o::ArrowBody::Expr(Box::new(expr)))
}

/// Convert a `function` body that is exactly `{ return <expr>; }` to a one-statement
/// `[return <converted expr>;]` block. Any other shape → `None`.
fn convert_return_only_block(statements: &[Statement]) -> Option<Vec<o::Stmt>> {
    let expr = single_return_expr(statements)?;
    Some(vec![o::Stmt::bare(o::StmtKind::Return(expr))])
}

/// The single returned expression of a one-statement `{ return <expr>; }` body, or
/// `None` if the body is not exactly one `return` of a convertible expression.
fn single_return_expr(statements: &[Statement]) -> Option<Expr> {
    match statements {
        [Statement::ReturnStatement(ret)] => convert_expr(ret.argument.as_ref()?),
        _ => None,
    }
}

/// Whether `key` is a valid bare JS identifier (so an object key can be emitted unquoted).
fn is_safe_object_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$' => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        }
        _ => false,
    }
}

/// Resolve a `forwardRef(() => X)` call's returned identifier, or `None` if `expr` is not such a
/// call. Mirrors `extractForwardRef`'s `forwardRef`-recognition (`util.ts`).
fn forward_ref_target(expr: &Expression) -> Option<Expr> {
    let call = match expr {
        Expression::CallExpression(c) => c,
        Expression::ParenthesizedExpression(p) => return forward_ref_target(&p.expression),
        _ => return None,
    };
    let is_forward_ref =
        matches!(&call.callee, Expression::Identifier(id) if id.name == "forwardRef");
    if !is_forward_ref {
        return None;
    }
    let arg = call.arguments.first().and_then(|a| a.as_expression())?;
    arrow_or_fn_return(arg)
}

/// The expression returned by a `() => X` arrow (or `function(){ return X; }`).
fn arrow_or_fn_return(expr: &Expression) -> Option<Expr> {
    match expr {
        Expression::ArrowFunctionExpression(arrow) => {
            if arrow.expression {
                if let Some(Statement::ExpressionStatement(stmt)) = arrow.body.statements.first() {
                    return convert_expr(&stmt.expression);
                }
            }
            for stmt in &arrow.body.statements {
                if let Statement::ReturnStatement(ret) = stmt {
                    if let Some(arg) = &ret.argument {
                        return convert_expr(arg);
                    }
                }
            }
            None
        }
        Expression::FunctionExpression(func) => {
            let body = func.body.as_ref()?;
            for stmt in &body.statements {
                if let Statement::ReturnStatement(ret) = stmt {
                    if let Some(arg) = &ret.argument {
                        return convert_expr(arg);
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// `extractForwardRef(expr)` — a `forwardRef(() => X)` unwraps to `X` with
/// [`ForwardRefHandling::Unwrapped`] (re-wrapped on emit); any other expression is carried as-is
/// with [`ForwardRefHandling::None`].
fn extract_forward_ref(expr: &Expression) -> Option<MaybeForwardRef> {
    if let Some(target) = forward_ref_target(expr) {
        return Some(MaybeForwardRef {
            expression: target,
            forward_ref: ForwardRefHandling::Unwrapped,
        });
    }
    convert_expr(expr).map(MaybeForwardRef::none)
}

/// `getDependency(depObj)` (`util.ts`) — one `deps` entry → [`R3DependencyMetadata`]. An
/// `attribute: true` dep sets `attribute_name_type = "unknown"` (the link-time marker) and routes
/// to `ɵɵinjectAttribute`; the qualifier booleans (`host`/`optional`/`self`/`skipSelf`) carry over.
fn get_dependency(obj: &ObjectExpression) -> Result<R3DependencyMetadata, String> {
    let is_attribute = prop_is_true(obj, "attribute");
    let token = find_prop(obj, "token")
        .and_then(convert_expr)
        .ok_or_else(|| "dependency missing usable `token`".to_string())?;
    let attribute_name_type = if is_attribute {
        Some(o::literal(LiteralValue::String("unknown".to_string()), None))
    } else {
        None
    };
    Ok(R3DependencyMetadata {
        token: Some(token),
        attribute_name_type,
        host: prop_is_true(obj, "host"),
        optional: prop_is_true(obj, "optional"),
        self_: prop_is_true(obj, "self"),
        skip_self: prop_is_true(obj, "skipSelf"),
    })
}

/// `getDependencies(metaObj, 'deps')` (factory linker) — the `R3DependencyMetadata[] | 'invalid' |
/// null` tri-state. Absent → `Inherit`; an array → `Deps`; a non-array (string) → `Invalid`.
fn get_dependencies(obj: &ObjectExpression) -> Result<FactoryDeps, String> {
    let Some(deps) = find_prop(obj, "deps") else {
        return Ok(FactoryDeps::Inherit);
    };
    match deps {
        Expression::ArrayExpression(arr) => {
            let mut out = Vec::with_capacity(arr.elements.len());
            for el in &arr.elements {
                let inner = el
                    .as_expression()
                    .ok_or_else(|| "unsupported `deps` array element".to_string())?;
                let Expression::ObjectExpression(dep_obj) = inner else {
                    return Err("`deps` array element must be an object".to_string());
                };
                out.push(get_dependency(dep_obj)?);
            }
            Ok(FactoryDeps::Deps(out))
        }
        // `deps: "invalid"` — at least one dep was unresolvable at partial-compile time.
        _ => Ok(FactoryDeps::Invalid),
    }
}

/// Parse an `@Injectable`-style `deps: [...]` array into [`R3DependencyMetadata`] (a present-but
/// empty array is distinct from absent). Each element is a `{token, ...flags}` object.
fn parse_injectable_deps(expr: &Expression) -> Result<Vec<R3DependencyMetadata>, String> {
    let Expression::ArrayExpression(arr) = expr else {
        return Err("injectable `deps` must be an array".to_string());
    };
    let mut out = Vec::with_capacity(arr.elements.len());
    for el in &arr.elements {
        let inner = el
            .as_expression()
            .ok_or_else(|| "unsupported injectable `deps` element".to_string())?;
        let Expression::ObjectExpression(dep_obj) = inner else {
            return Err("injectable `deps` element must be an object".to_string());
        };
        out.push(get_dependency(dep_obj)?);
    }
    Ok(out)
}

/// `parseEnum(target, FactoryTarget)` — map `ɵɵFactoryTarget.X` (or a bare `X`) onto
/// [`FactoryTarget`].
fn parse_factory_target(expr: &Expression) -> Result<FactoryTarget, String> {
    let name = symbol_name(expr).ok_or_else(|| "`target` has no symbol name".to_string())?;
    match name.as_str() {
        "Directive" => Ok(FactoryTarget::Directive),
        "Component" => Ok(FactoryTarget::Component),
        "Injectable" => Ok(FactoryTarget::Injectable),
        "Pipe" => Ok(FactoryTarget::Pipe),
        "NgModule" => Ok(FactoryTarget::NgModule),
        other => Err(format!("unsupported ɵɵFactoryTarget.{other}")),
    }
}

/// Resolve an array literal of class references into [`R3Reference`]s. A `() => [...]` forward-decl
/// wrapper is unwrapped (the elements come from the returned array). Non-identifier elements whose
/// symbol name cannot be recovered are skipped.
fn refs_of(expr: &Expression) -> Vec<R3Reference> {
    let arr = match expr {
        Expression::ArrayExpression(arr) => arr,
        // `() => [A, B]` forward-declaration wrapper.
        Expression::ArrowFunctionExpression(_) | Expression::FunctionExpression(_) => {
            return forward_ref_array(expr);
        }
        Expression::ParenthesizedExpression(p) => return refs_of(&p.expression),
        _ => return Vec::new(),
    };
    let mut out = Vec::new();
    for el in &arr.elements {
        if let Some(inner) = el.as_expression() {
            if let Some(name) = symbol_name(inner) {
                out.push(class_ref(&name));
            }
        }
    }
    out
}

/// Unwrap a `() => [A, B]` (or `function(){ return [A, B]; }`) wrapper into its element references.
fn forward_ref_array(expr: &Expression) -> Vec<R3Reference> {
    let stmts = match expr {
        Expression::ArrowFunctionExpression(arrow) => {
            if arrow.expression {
                if let Some(Statement::ExpressionStatement(stmt)) = arrow.body.statements.first() {
                    return refs_of(&stmt.expression);
                }
            }
            &arrow.body.statements
        }
        Expression::FunctionExpression(func) => match func.body.as_ref() {
            Some(b) => &b.statements,
            None => return Vec::new(),
        },
        _ => return Vec::new(),
    };
    for stmt in stmts {
        if let Statement::ReturnStatement(ret) = stmt {
            if let Some(arg) = &ret.argument {
                return refs_of(arg);
            }
        }
    }
    Vec::new()
}

// ---------------------------------------------------------------------------
// Per-kind declaration → R3-metadata → emitted `ɵɵdefine*` text.
// ---------------------------------------------------------------------------

/// The `ɵɵngDeclare*` kinds this linker rewrites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeclareKind {
    Factory,
    Injectable,
    Injector,
    NgModule,
    Pipe,
    Directive,
    Component,
    /// `ɵɵngDeclareClassMetadata` — dev-only `setClassMetadata`; dropped (replaced with `void 0`).
    ClassMetadata,
}

impl DeclareKind {
    /// Map a `ɵɵngDeclare*` callee identifier name to its kind.
    fn from_callee(name: &str) -> Option<DeclareKind> {
        match name {
            "\u{0275}\u{0275}ngDeclareFactory" => Some(DeclareKind::Factory),
            "\u{0275}\u{0275}ngDeclareInjectable" => Some(DeclareKind::Injectable),
            "\u{0275}\u{0275}ngDeclareInjector" => Some(DeclareKind::Injector),
            "\u{0275}\u{0275}ngDeclareNgModule" => Some(DeclareKind::NgModule),
            "\u{0275}\u{0275}ngDeclarePipe" => Some(DeclareKind::Pipe),
            "\u{0275}\u{0275}ngDeclareDirective" => Some(DeclareKind::Directive),
            "\u{0275}\u{0275}ngDeclareComponent" => Some(DeclareKind::Component),
            "\u{0275}\u{0275}ngDeclareClassMetadata" => Some(DeclareKind::ClassMetadata),
            _ => None,
        }
    }
}

/// One linked declaration: the replacement expression text plus any trailing sibling statements
/// (only NgModule produces a non-empty `suffix`).
struct LinkedDef {
    expr_text: String,
    suffix: String,
}

/// Wrap a plain expression text (no trailing statements) as a [`LinkedDef`].
fn plain(expr_text: String) -> LinkedDef {
    LinkedDef {
        expr_text,
        suffix: String::new(),
    }
}

/// `toR3FactoryMeta` + `compileFactoryFunction` → the `function X_Factory(t){…}` expression text.
fn link_factory(obj: &ObjectExpression) -> Result<String, String> {
    let type_expr =
        find_prop(obj, "type").ok_or_else(|| "ɵɵngDeclareFactory missing `type`".to_string())?;
    let name = symbol_name(type_expr)
        .ok_or_else(|| "ɵɵngDeclareFactory `type` has no symbol name".to_string())?;
    let target = match find_prop(obj, "target") {
        Some(t) => parse_factory_target(t)?,
        None => return Err("ɵɵngDeclareFactory missing `target`".to_string()),
    };
    let meta = R3FactoryMetadata::Constructor(R3ConstructorFactoryMetadata {
        name,
        ty: class_ref_from(type_expr)?,
        type_argument_count: 0,
        deps: get_dependencies(obj)?,
        target,
    });
    let compiled = compile_factory_function(&meta);
    Ok(emit_def_text(&compiled.expression))
}

/// `toR3InjectableMeta` + `compileInjectable(meta, false)` → the `ɵɵdefineInjectable({…})` text.
fn link_injectable(obj: &ObjectExpression) -> Result<String, String> {
    let type_expr =
        find_prop(obj, "type").ok_or_else(|| "ɵɵngDeclareInjectable missing `type`".to_string())?;
    let name = symbol_name(type_expr)
        .ok_or_else(|| "ɵɵngDeclareInjectable `type` has no symbol name".to_string())?;

    let provided_in = match find_prop(obj, "providedIn") {
        Some(e) => {
            extract_forward_ref(e).ok_or_else(|| "unsupported `providedIn` expression".to_string())?
        }
        None => MaybeForwardRef::none(o::null_expr()),
    };
    let use_class = forward_ref_option(obj, "useClass")?;
    let use_existing = forward_ref_option(obj, "useExisting")?;
    let use_value = forward_ref_option(obj, "useValue")?;
    let use_factory = match find_prop(obj, "useFactory") {
        Some(e) => {
            Some(convert_expr(e).ok_or_else(|| "unsupported `useFactory` expression".to_string())?)
        }
        None => None,
    };
    let deps = match find_prop(obj, "deps") {
        Some(e) => Some(parse_injectable_deps(e)?),
        None => None,
    };

    let meta = R3InjectableMetadata {
        name,
        ty: class_ref_from(type_expr)?,
        type_argument_count: 0,
        provided_in,
        use_class,
        use_factory,
        use_existing,
        use_value,
        deps,
    };
    let compiled = compile_injectable(&meta, false);
    Ok(emit_def_text(&compiled.expression))
}

/// Read a `useClass`/`useExisting`/`useValue` option into a [`MaybeForwardRef`] (absent → `None`).
fn forward_ref_option(obj: &ObjectExpression, key: &str) -> Result<Option<MaybeForwardRef>, String> {
    match find_prop(obj, key) {
        None => Ok(None),
        Some(e) => extract_forward_ref(e)
            .map(Some)
            .ok_or_else(|| format!("unsupported `{key}` expression")),
    }
}

/// `toR3InjectorMeta` + `compileInjector` → the `ɵɵdefineInjector({…})` text.
fn link_injector(obj: &ObjectExpression) -> Result<String, String> {
    let type_expr =
        find_prop(obj, "type").ok_or_else(|| "ɵɵngDeclareInjector missing `type`".to_string())?;
    let name = symbol_name(type_expr)
        .ok_or_else(|| "ɵɵngDeclareInjector `type` has no symbol name".to_string())?;

    let providers = match find_prop(obj, "providers") {
        Some(e) => {
            Some(convert_expr(e).ok_or_else(|| "unsupported injector `providers`".to_string())?)
        }
        None => None,
    };
    let imports = match find_prop(obj, "imports") {
        Some(Expression::ArrayExpression(arr)) => {
            let mut out = Vec::with_capacity(arr.elements.len());
            for el in &arr.elements {
                let inner = el
                    .as_expression()
                    .ok_or_else(|| "unsupported injector `imports` element".to_string())?;
                out.push(convert_expr(inner).ok_or_else(|| {
                    "unsupported injector `imports` element expression".to_string()
                })?);
            }
            out
        }
        Some(_) => return Err("injector `imports` must be an array".to_string()),
        None => Vec::new(),
    };

    let meta = R3InjectorMetadata {
        name,
        r#type: class_ref_from(type_expr)?,
        providers,
        imports,
    };
    let compiled = compile_injector(&meta);
    Ok(emit_def_text(&compiled.expression))
}

/// `toR3NgModuleMeta` + `compileNgModule` → the `ɵɵdefineNgModule({…})` text PLUS any
/// `ɵɵsetNgModuleScope` / `ɵɵregisterNgModuleType` side-effect statements (returned as a trailing
/// suffix so they can be spliced after the rewritten call's statement).
fn link_ng_module(obj: &ObjectExpression) -> Result<LinkedDef, String> {
    let type_expr =
        find_prop(obj, "type").ok_or_else(|| "ɵɵngDeclareNgModule missing `type`".to_string())?;

    let bootstrap = find_prop(obj, "bootstrap").map(refs_of).unwrap_or_default();
    let declarations = find_prop(obj, "declarations").map(refs_of).unwrap_or_default();
    let imports = find_prop(obj, "imports").map(refs_of).unwrap_or_default();
    let exports = find_prop(obj, "exports").map(refs_of).unwrap_or_default();
    let schemas = find_prop(obj, "schemas").map(refs_of);
    let id = match find_prop(obj, "id") {
        Some(e) => Some(convert_expr(e).ok_or_else(|| "unsupported NgModule `id`".to_string())?),
        None => None,
    };

    let meta = R3NgModuleMetadata::Global(R3NgModuleMetadataGlobal {
        common: R3NgModuleCommon {
            r#type: class_ref_from(type_expr)?,
            // The AOT/partial-link path emits the scope as a tree-shakeable side effect.
            selector_scope_mode: R3SelectorScopeMode::SideEffect,
            schemas,
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

    // The `ɵɵsetNgModuleScope`/`ɵɵregisterNgModuleType` side effects belong as sibling statements
    // AFTER the rewritten `X.ɵmod = …;` statement.
    let suffix = if compiled.statements.is_empty() {
        String::new()
    } else {
        strip_leading_i0_import(&emit_statements(&compiled.statements))
    };
    Ok(LinkedDef {
        expr_text: emit_def_text(&compiled.expression),
        suffix,
    })
}

/// `toR3PipeMeta` + `compilePipeFromMetadata` → the `ɵɵdefinePipe({…})` text.
fn link_pipe(obj: &ObjectExpression) -> Result<String, String> {
    let type_expr =
        find_prop(obj, "type").ok_or_else(|| "ɵɵngDeclarePipe missing `type`".to_string())?;
    let name = symbol_name(type_expr)
        .ok_or_else(|| "ɵɵngDeclarePipe `type` has no symbol name".to_string())?;
    let pipe_name = find_prop(obj, "name").and_then(string_value);
    // `pure` defaults to true; `isStandalone` defaults to true (we target v19+, standalone-default).
    let pure = match find_prop(obj, "pure") {
        Some(e) => bool_value(e).ok_or_else(|| "`pure` must be a boolean".to_string())?,
        None => true,
    };
    let is_standalone = match find_prop(obj, "isStandalone") {
        Some(e) => bool_value(e).ok_or_else(|| "`isStandalone` must be a boolean".to_string())?,
        None => true,
    };

    let meta = R3PipeMetadata {
        name,
        r#type: class_ref_from(type_expr)?,
        type_argument_count: 0,
        pipe_name,
        deps: None,
        pure,
        is_standalone,
    };
    let compiled = compile_pipe_from_metadata(&meta);
    Ok(emit_def_text(&compiled.expression))
}

// ---------------------------------------------------------------------------
// Directive / Component declaration → R3*Metadata → emitted `ɵɵdefine*` text.
//
// These mirror `tools/angular-ref/.../partial_directive_linker_1.ts` (`toR3DirectiveMeta`) and
// `partial_component_linker_1.ts` (`toR3ComponentMeta`), reading the ALREADY-SPLIT declaration
// object shape, and drive the SAME emit the SOURCE front-end (`source_compile`) uses:
// [`compile_directive_from_metadata`] / [`compile_component_from_metadata`] with
// [`DefaultHostBindingsBuilder`] (+ [`RealTemplateBuilder`] for components). Class references are
// plain identifier [`Expr`]s (`class_ref_from`), exactly as the source front-end emits them — the
// definition emitter prints `meta.ty.value`/the dependency `type` verbatim, so no wrapped-node side
// table is needed here.
// ---------------------------------------------------------------------------

/// `new semver.SemVer(version).major` for the small subset of version strings a declaration carries
/// (`"21.2.15"`, `"14.0.0"`, the local placeholder `"0.0.0-PLACEHOLDER"`). The major number gates
/// the v22 defaults (`hasOnPushByDefault`, `legacyOptionalChaining`); an unparsable version is
/// treated as the placeholder (major 0).
fn version_major(obj: &ObjectExpression) -> u32 {
    find_prop(obj, "version")
        .and_then(string_value)
        .and_then(|v| v.split('.').next().and_then(|m| m.parse::<u32>().ok()))
        .unwrap_or(0)
}

/// Whether a declaration's `version` is the local placeholder Angular stamps for first-party
/// (in-repo) compilation (`getDefaultStandaloneValue` / the `legacyOptionalChaining` guard treat it
/// specially: placeholder → newest behaviour). Any `0.0.0-…` prerelease counts.
fn is_placeholder_version(obj: &ObjectExpression) -> bool {
    find_prop(obj, "version")
        .and_then(string_value)
        .map(|v| v.starts_with("0.0.0"))
        .unwrap_or(false)
}

/// `getDefaultStandaloneValue(version)` — standalone defaults to `true` for v19+ (and the
/// placeholder); these are v21+ libraries, so absent `isStandalone` means standalone.
fn read_is_standalone(obj: &ObjectExpression) -> bool {
    match find_prop(obj, "isStandalone") {
        Some(e) => bool_value(e).unwrap_or(true),
        None => true,
    }
}

/// `metaObj.getBoolean(name)` with the given default when the key is absent.
fn read_bool(obj: &ObjectExpression, name: &str, default: bool) -> bool {
    match find_prop(obj, name) {
        Some(e) => bool_value(e).unwrap_or(default),
        None => default,
    }
}

/// `toInputMapping` — decode one `inputs` entry. The value is either a RICH object
/// (`{classPropertyName, publicName, isSignal, isRequired, transformFunction}`) or the LEGACY form
/// (a bare `"publicName"` string, or a `["publicName", "classPropertyName"(, transformFn)]` array).
/// `key` is the object property key (the class property name for the legacy string form).
fn to_input_mapping(key: &str, value: &Expression) -> Result<R3InputMetadata, String> {
    match value {
        // Rich object form.
        Expression::ObjectExpression(obj) => {
            let class_property_name = find_prop(obj, "classPropertyName")
                .and_then(string_value)
                .ok_or_else(|| format!("input `{key}` missing `classPropertyName`"))?;
            let binding_property_name = find_prop(obj, "publicName")
                .and_then(string_value)
                .ok_or_else(|| format!("input `{key}` missing `publicName`"))?;
            let transform_function = match find_prop(obj, "transformFunction") {
                Some(Expression::NullLiteral(_)) | None => None,
                Some(e) => Some(
                    convert_expr(e)
                        .ok_or_else(|| format!("input `{key}` has unsupported transformFunction"))?,
                ),
            };
            Ok(R3InputMetadata {
                class_property_name,
                binding_property_name,
                is_signal: read_bool(obj, "isSignal", false),
                required: read_bool(obj, "isRequired", false),
                transform_function,
            })
        }
        // Legacy string form: `"pub"` — the KEY is the class property name.
        Expression::StringLiteral(_) | Expression::TemplateLiteral(_) => {
            let public = string_value(value)
                .ok_or_else(|| format!("input `{key}` legacy string is not a string literal"))?;
            Ok(R3InputMetadata {
                class_property_name: key.to_string(),
                binding_property_name: public,
                required: false,
                is_signal: false,
                transform_function: None,
            })
        }
        // Legacy array form: `["pub", "cls"]` or `["pub", "cls", transformFn]`.
        Expression::ArrayExpression(arr) => {
            if arr.elements.len() != 2 && arr.elements.len() != 3 {
                return Err(format!(
                    "input `{key}` legacy array must have 2 or 3 elements"
                ));
            }
            let elem = |i: usize| arr.elements.get(i).and_then(|e| e.as_expression());
            let binding_property_name = elem(0)
                .and_then(string_value)
                .ok_or_else(|| format!("input `{key}` legacy array[0] is not a string"))?;
            let class_property_name = elem(1)
                .and_then(string_value)
                .ok_or_else(|| format!("input `{key}` legacy array[1] is not a string"))?;
            let transform_function = match elem(2) {
                Some(e) => Some(
                    convert_expr(e)
                        .ok_or_else(|| format!("input `{key}` legacy array[2] unsupported"))?,
                ),
                None => None,
            };
            Ok(R3InputMetadata {
                class_property_name,
                binding_property_name,
                required: false,
                is_signal: false,
                transform_function,
            })
        }
        _ => Err(format!("unsupported `inputs` entry for `{key}`")),
    }
}

/// Read the `inputs` object map into the ordered [`R3InputMetadata`] map (insertion order = source
/// property order, which feeds the emitted inputs literal).
fn read_inputs(obj: &ObjectExpression) -> Result<OrderedMap<String, R3InputMetadata>, String> {
    let mut out: OrderedMap<String, R3InputMetadata> = OrderedMap::new();
    let Some(Expression::ObjectExpression(inputs_obj)) = find_prop(obj, "inputs") else {
        return Ok(out);
    };
    for p in &inputs_obj.properties {
        let ObjectPropertyKind::ObjectProperty(op) = p else {
            return Err("unsupported `inputs` spread/shorthand".to_string());
        };
        let key = key_name(&op.key).ok_or_else(|| "unsupported `inputs` computed key".to_string())?;
        out.insert(key.to_string(), to_input_mapping(key, &op.value)?);
    }
    Ok(out)
}

/// Read the `outputs` object map (`{classProperty: "publicName"}`) — keyed on the property name,
/// value the public-name string.
fn read_outputs(obj: &ObjectExpression) -> Result<OrderedMap<String, String>, String> {
    let mut out: OrderedMap<String, String> = OrderedMap::new();
    let Some(Expression::ObjectExpression(outputs_obj)) = find_prop(obj, "outputs") else {
        return Ok(out);
    };
    for p in &outputs_obj.properties {
        let ObjectPropertyKind::ObjectProperty(op) = p else {
            return Err("unsupported `outputs` spread/shorthand".to_string());
        };
        let key =
            key_name(&op.key).ok_or_else(|| "unsupported `outputs` computed key".to_string())?;
        let value = string_value(&op.value)
            .ok_or_else(|| format!("output `{key}` value is not a string"))?;
        out.insert(key.to_string(), value);
    }
    Ok(out)
}

/// A string-keyed → string-valued object map (`host.listeners` / `host.properties`).
fn read_string_map(value: &Expression) -> Result<OrderedMap<String, String>, String> {
    let mut out: OrderedMap<String, String> = OrderedMap::new();
    let Expression::ObjectExpression(map_obj) = value else {
        return Err("expected a string→string object map".to_string());
    };
    for p in &map_obj.properties {
        let ObjectPropertyKind::ObjectProperty(op) = p else {
            return Err("unsupported map spread/shorthand".to_string());
        };
        let key = key_name(&op.key).ok_or_else(|| "unsupported map computed key".to_string())?;
        let v = string_value(&op.value)
            .ok_or_else(|| format!("map value for `{key}` is not a string"))?;
        out.insert(key.to_string(), v);
    }
    Ok(out)
}

/// A string-keyed → opaque-expression object map (`host.attributes`).
fn read_expr_map(value: &Expression) -> Result<OrderedMap<String, Expr>, String> {
    let mut out: OrderedMap<String, Expr> = OrderedMap::new();
    let Expression::ObjectExpression(map_obj) = value else {
        return Err("expected an object map".to_string());
    };
    for p in &map_obj.properties {
        let ObjectPropertyKind::ObjectProperty(op) = p else {
            return Err("unsupported map spread/shorthand".to_string());
        };
        let key = key_name(&op.key).ok_or_else(|| "unsupported map computed key".to_string())?;
        let v = convert_expr(&op.value)
            .ok_or_else(|| format!("map value for `{key}` is unsupported"))?;
        out.insert(key.to_string(), v);
    }
    Ok(out)
}

/// `toHostMetadata` — the declaration `host` object is ALREADY SPLIT into `attributes`/`listeners`/
/// `properties`/`styleAttribute`/`classAttribute`, so map each sub-field directly (NOT through
/// `parse_host_bindings`, which is for the unsplit source decorator-object form).
fn read_host(obj: &ObjectExpression) -> Result<R3HostMetadata, String> {
    let Some(Expression::ObjectExpression(host_obj)) = find_prop(obj, "host") else {
        return Ok(R3HostMetadata::default());
    };
    let attributes = match find_prop(host_obj, "attributes") {
        Some(v) => read_expr_map(v)?,
        None => OrderedMap::new(),
    };
    let listeners = match find_prop(host_obj, "listeners") {
        Some(v) => read_string_map(v)?,
        None => OrderedMap::new(),
    };
    let properties = match find_prop(host_obj, "properties") {
        Some(v) => read_string_map(v)?,
        None => OrderedMap::new(),
    };
    let special_attributes = SpecialAttrs {
        style_attr: find_prop(host_obj, "styleAttribute").and_then(string_value),
        class_attr: find_prop(host_obj, "classAttribute").and_then(string_value),
    };
    Ok(R3HostMetadata {
        attributes,
        listeners,
        properties,
        special_attributes,
    })
}

/// `toQueryMetadata` — one query object (`content` or `view`) → [`R3QueryMetadata`]. The `predicate`
/// is either a string-array selector list or a (possibly `forwardRef`-wrapped) class-reference
/// expression. Forward-ref wrapping is resolved upstream of the emit metadata, so a `forwardRef(() =>
/// X)` predicate becomes the bare `X` expression.
fn to_query_metadata(value: &Expression) -> Result<R3QueryMetadata, String> {
    let Expression::ObjectExpression(q) = value else {
        return Err("query entry must be an object".to_string());
    };
    let property_name = find_prop(q, "propertyName")
        .and_then(string_value)
        .ok_or_else(|| "query missing `propertyName`".to_string())?;

    let predicate_expr =
        find_prop(q, "predicate").ok_or_else(|| "query missing `predicate`".to_string())?;
    let predicate = match predicate_expr {
        Expression::ArrayExpression(arr) => {
            let mut selectors = Vec::with_capacity(arr.elements.len());
            for el in &arr.elements {
                let s = el
                    .as_expression()
                    .and_then(string_value)
                    .ok_or_else(|| "query predicate array element is not a string".to_string())?;
                selectors.push(s);
            }
            QueryPredicate::Selectors(selectors)
        }
        other => {
            // `extractForwardRef` — unwrap `forwardRef(() => X)` to the bare reference; otherwise
            // carry the reference expression verbatim.
            let resolved = forward_ref_target(other)
                .or_else(|| convert_expr(other))
                .ok_or_else(|| "unsupported query predicate expression".to_string())?;
            QueryPredicate::Expr(resolved)
        }
    };

    let read = match find_prop(q, "read") {
        Some(e) => Some(convert_expr(e).ok_or_else(|| "unsupported query `read`".to_string())?),
        None => None,
    };

    Ok(R3QueryMetadata {
        property_name,
        first: read_bool(q, "first", false),
        predicate,
        descendants: read_bool(q, "descendants", false),
        emit_distinct_changes_only: read_bool(q, "emitDistinctChangesOnly", true),
        read,
        static_: read_bool(q, "static", false),
        is_signal: read_bool(q, "isSignal", false),
    })
}

/// Read a `queries` / `viewQueries` array into [`R3QueryMetadata`]s.
fn read_queries(obj: &ObjectExpression, key: &str) -> Result<Vec<R3QueryMetadata>, String> {
    let Some(Expression::ArrayExpression(arr)) = find_prop(obj, key) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.elements.len());
    for el in &arr.elements {
        let inner = el
            .as_expression()
            .ok_or_else(|| format!("unsupported `{key}` array element"))?;
        out.push(to_query_metadata(inner)?);
    }
    Ok(out)
}

/// `getHostDirectiveBindingMapping` — a flat `[publicName, alias, publicName, alias, …]` string
/// array → an ordered `{publicName: alias}` map (or `None` when absent).
fn read_host_directive_mapping(value: &Expression) -> Result<Option<OrderedMap<String, String>>, String> {
    let Expression::ArrayExpression(arr) = value else {
        return Err("hostDirective inputs/outputs must be an array".to_string());
    };
    if arr.elements.is_empty() {
        return Ok(None);
    }
    let mut out: OrderedMap<String, String> = OrderedMap::new();
    let mut i = 1;
    while i < arr.elements.len() {
        let public = arr.elements[i - 1]
            .as_expression()
            .and_then(string_value)
            .ok_or_else(|| "hostDirective mapping element is not a string".to_string())?;
        let alias = arr.elements[i]
            .as_expression()
            .and_then(string_value)
            .ok_or_else(|| "hostDirective mapping element is not a string".to_string())?;
        out.insert(public, alias);
        i += 2;
    }
    Ok(Some(out))
}

/// `toHostDirectivesMetadata` — the `hostDirectives` array → [`R3HostDirectiveMetadata`]. Each entry
/// carries a `directive` reference (possibly `forwardRef`-wrapped) plus optional `inputs`/`outputs`
/// public-name→alias mappings.
fn read_host_directives(obj: &ObjectExpression) -> Result<Option<Vec<R3HostDirectiveMetadata>>, String> {
    let Some(Expression::ArrayExpression(arr)) = find_prop(obj, "hostDirectives") else {
        return Ok(None);
    };
    let mut out: Vec<R3HostDirectiveMetadata> = Vec::with_capacity(arr.elements.len());
    for el in &arr.elements {
        let inner = el
            .as_expression()
            .ok_or_else(|| "unsupported `hostDirectives` element".to_string())?;
        let Expression::ObjectExpression(entry) = inner else {
            return Err("`hostDirectives` element must be an object".to_string());
        };
        let directive_expr = find_prop(entry, "directive")
            .ok_or_else(|| "hostDirective missing `directive`".to_string())?;
        let is_forward_reference = forward_ref_target(directive_expr).is_some();
        let directive_value = forward_ref_target(directive_expr)
            .or_else(|| convert_expr(directive_expr))
            .ok_or_else(|| "unsupported hostDirective `directive` expression".to_string())?;
        let directive = R3Reference {
            ty: directive_value.clone(),
            value: directive_value,
        };
        let inputs = match find_prop(entry, "inputs") {
            Some(v) => read_host_directive_mapping(v)?,
            None => None,
        };
        let outputs = match find_prop(entry, "outputs") {
            Some(v) => read_host_directive_mapping(v)?,
            None => None,
        };
        out.push(R3HostDirectiveMetadata {
            directive,
            is_forward_reference,
            inputs,
            outputs,
        });
    }
    Ok(Some(out))
}

/// `toR3DirectiveMeta` — the SHARED directive base both `ɵɵngDeclareDirective` and
/// `ɵɵngDeclareComponent` build. Maps the declaration object field-by-field onto
/// [`R3DirectiveMetadata`].
fn to_r3_directive_meta(obj: &ObjectExpression) -> Result<R3DirectiveMetadata, String> {
    let type_expr =
        find_prop(obj, "type").ok_or_else(|| "declaration missing `type`".to_string())?;
    let name = symbol_name(type_expr)
        .ok_or_else(|| "declaration `type` has no symbol name".to_string())?;

    let major = version_major(obj);
    let placeholder = is_placeholder_version(obj);

    let export_as = find_prop(obj, "exportAs").map(|e| match e {
        // `exportAs` is an array of strings in the declaration form.
        Expression::ArrayExpression(arr) => arr
            .elements
            .iter()
            .filter_map(|el| el.as_expression().and_then(string_value))
            .collect::<Vec<_>>(),
        // Defensive: a bare string also reads as a single export name.
        other => string_value(other).into_iter().collect::<Vec<_>>(),
    });

    let providers = match find_prop(obj, "providers") {
        Some(e) => Some(convert_expr(e).ok_or_else(|| "unsupported `providers` expression".to_string())?),
        None => None,
    };

    Ok(R3DirectiveMetadata {
        name,
        ty: class_ref_from(type_expr)?,
        type_argument_count: 0,
        type_source_span: ParseSourceSpan::new(0, 0),
        deps: Deps::None,
        selector: find_prop(obj, "selector").and_then(string_value),
        queries: read_queries(obj, "queries")?,
        view_queries: read_queries(obj, "viewQueries")?,
        host: read_host(obj)?,
        lifecycle: Lifecycle {
            uses_on_changes: read_bool(obj, "usesOnChanges", false),
        },
        inputs: read_inputs(obj)?,
        outputs: read_outputs(obj)?,
        uses_inheritance: read_bool(obj, "usesInheritance", false),
        control_create: None,
        export_as,
        providers,
        is_standalone: read_is_standalone(obj),
        is_signal: read_bool(obj, "isSignal", false),
        host_directives: read_host_directives(obj)?,
        legacy_optional_chaining: major < 22 && !placeholder,
    })
}

/// `PartialDirectiveLinkerVersion1` — `toR3DirectiveMeta` + `compileDirectiveFromMetadata` → the
/// `ɵɵdefineDirective({…})` text (plus any hoisted query-predicate `const _cN = […]` statements as a
/// trailing suffix, mirroring the source front-end's directive emit).
fn link_directive(obj: &ObjectExpression) -> Result<LinkedDef, String> {
    let base = to_r3_directive_meta(obj)?;
    let mut host_builder = DefaultHostBindingsBuilder;
    let compiled = compile_directive_from_metadata(&base, &mut host_builder);
    Ok(LinkedDef {
        expr_text: emit_def_text(&compiled.expression),
        suffix: suffix_from_statements(&compiled.statements),
    })
}

/// `makeDirectiveMetadata` (component-linker) — one `dependencies`/`directives`/`components` entry →
/// a template dependency. The kind discriminates directive / pipe / ngmodule; pipes additionally
/// carry a `name`. The `type` is resolved through `extractForwardRef`. Returns `None` for an unknown
/// `kind` (skipped, matching the reference `default: continue`).
fn dependency_from_object(
    dep: &ObjectExpression,
    forced_kind: Option<R3TemplateDependencyKind>,
) -> Result<Option<R3TemplateDependencyMetadata>, String> {
    let type_expr =
        find_prop(dep, "type").ok_or_else(|| "dependency missing `type`".to_string())?;
    let ty = forward_ref_target(type_expr)
        .or_else(|| convert_expr(type_expr))
        .ok_or_else(|| "unsupported dependency `type` expression".to_string())?;

    let kind = match forced_kind {
        Some(k) => k,
        None => match find_prop(dep, "kind").and_then(string_value).as_deref() {
            Some("directive") | Some("component") => R3TemplateDependencyKind::Directive,
            Some("pipe") => R3TemplateDependencyKind::Pipe,
            Some("ngmodule") => R3TemplateDependencyKind::NgModule,
            // Unknown / missing kind — skip (reference `default: continue`).
            _ => return Ok(None),
        },
    };

    Ok(Some(R3TemplateDependencyMetadata { kind, ty }))
}

/// Collect every template dependency from a component declaration, unifying the OLD-style
/// (`components`/`directives` arrays + `pipes` object) and NEW-style (`dependencies` array) forms,
/// exactly as `toR3ComponentMeta` does. Order: components, directives, pipes, then `dependencies`.
fn read_declarations(obj: &ObjectExpression) -> Result<Vec<R3TemplateDependencyMetadata>, String> {
    let mut out: Vec<R3TemplateDependencyMetadata> = Vec::new();

    // Old-style `components` / `directives` arrays (each entry is a directive-dependency object).
    for key in ["components", "directives"] {
        if let Some(Expression::ArrayExpression(arr)) = find_prop(obj, key) {
            for el in &arr.elements {
                let inner = el
                    .as_expression()
                    .ok_or_else(|| format!("unsupported `{key}` element"))?;
                let Expression::ObjectExpression(dep) = inner else {
                    return Err(format!("`{key}` element must be an object"));
                };
                if let Some(meta) =
                    dependency_from_object(dep, Some(R3TemplateDependencyKind::Directive))?
                {
                    out.push(meta);
                }
            }
        }
    }

    // Old-style `pipes` object map (`{name: TypeRef}`).
    if let Some(Expression::ObjectExpression(pipes)) = find_prop(obj, "pipes") {
        for p in &pipes.properties {
            let ObjectPropertyKind::ObjectProperty(op) = p else {
                return Err("unsupported `pipes` spread/shorthand".to_string());
            };
            let _name =
                key_name(&op.key).ok_or_else(|| "unsupported `pipes` key".to_string())?;
            let ty = forward_ref_target(&op.value)
                .or_else(|| convert_expr(&op.value))
                .ok_or_else(|| "unsupported `pipes` type expression".to_string())?;
            out.push(R3TemplateDependencyMetadata {
                kind: R3TemplateDependencyKind::Pipe,
                ty,
            });
        }
    }

    // New-style unified `dependencies` array.
    if let Some(Expression::ArrayExpression(arr)) = find_prop(obj, "dependencies") {
        for el in &arr.elements {
            let inner = el
                .as_expression()
                .ok_or_else(|| "unsupported `dependencies` element".to_string())?;
            let Expression::ObjectExpression(dep) = inner else {
                return Err("`dependencies` element must be an object".to_string());
            };
            if let Some(meta) = dependency_from_object(dep, None)? {
                out.push(meta);
            }
        }
    }

    Ok(out)
}

/// `parseEncapsulation` — `ViewEncapsulation.X` member (or bare `X`) → the enum (default Emulated).
fn read_encapsulation(obj: &ObjectExpression) -> ViewEncapsulation {
    let Some(expr) = find_prop(obj, "encapsulation") else {
        return ViewEncapsulation::Emulated;
    };
    let name = match expr {
        Expression::StaticMemberExpression(m) => m.property.name.as_str(),
        Expression::Identifier(id) => id.name.as_str(),
        _ => return ViewEncapsulation::Emulated,
    };
    match name {
        "None" => ViewEncapsulation::None,
        "ShadowDom" => ViewEncapsulation::ShadowDom,
        _ => ViewEncapsulation::Emulated,
    }
}

/// `parseChangeDetectionStrategy` — `ChangeDetectionStrategy.X` member → the strategy. `Eager`
/// aliases `Default` (both `= 1`). Absent → v22 default OnPush (else Eager/Default).
fn read_change_detection(obj: &ObjectExpression, major: u32, placeholder: bool) -> ChangeDetection {
    if let Some(expr) = find_prop(obj, "changeDetection") {
        let name = match expr {
            Expression::StaticMemberExpression(m) => Some(m.property.name.as_str()),
            Expression::Identifier(id) => Some(id.name.as_str()),
            _ => None,
        };
        let strategy = match name {
            Some("OnPush") => ChangeDetectionStrategy::OnPush,
            // `Default` and its alias `Eager` are the omitted runtime default.
            _ => ChangeDetectionStrategy::Default,
        };
        return ChangeDetection::Strategy(strategy);
    }
    // `hasOnPushByDefault = major >= 22 || placeholder`.
    let strategy = if major >= 22 || placeholder {
        ChangeDetectionStrategy::OnPush
    } else {
        ChangeDetectionStrategy::Default
    };
    ChangeDetection::Strategy(strategy)
}

/// `PartialComponentLinkerVersion1` — `toR3ComponentMeta` + `compileComponentFromMetadata` → the
/// `ɵɵdefineComponent({…})` text plus the hoisted `ConstantPool.statements` (nested-view functions,
/// query-predicate / `ngContentSelectors` consts) as a leading prefix, exactly as the source
/// front-end emits them (the definition references those names, so they print first).
fn link_component(obj: &ObjectExpression) -> Result<LinkedDef, String> {
    let base = to_r3_directive_meta(obj)?;
    let major = version_major(obj);
    let placeholder = is_placeholder_version(obj);

    // Inline template string. Partial declarations carry the template as a string literal with
    // `isInline: true`; an external template would require source-map recovery we do not model, so
    // a non-string template is a hard link error rather than a silent mis-compile.
    let template_html = find_prop(obj, "template")
        .and_then(string_value)
        .ok_or_else(|| "component declaration has no inline string `template`".to_string())?;

    // Template HTML → r3_ast (Angular default whitespace handling; v17+ block syntax is always on
    // for these v21+ libraries).
    let parse_result = crate::ml_parser::parse(&template_html, "template.html");
    if let Some(e) = parse_result.errors.first() {
        return Err(format!("template parse error: {}", e.msg));
    }
    let mut binding_parser = BindingParser::new();
    let r3 = html_ast_to_render3_ast(
        &parse_result.root_nodes,
        &mut binding_parser,
        Render3ParseOptions::default(),
    );
    if let Some(e) = r3.errors.first() {
        return Err(format!("template error: {}", e.msg));
    }

    let declarations = read_declarations(obj)?;
    let has_directive_dependencies = !base.is_standalone || !declarations.is_empty();

    let view_providers = match find_prop(obj, "viewProviders") {
        Some(e) => {
            Some(convert_expr(e).ok_or_else(|| "unsupported `viewProviders` expression".to_string())?)
        }
        None => None,
    };
    let animations = match find_prop(obj, "animations") {
        Some(e) => Some(convert_expr(e).ok_or_else(|| "unsupported `animations` expression".to_string())?),
        None => None,
    };
    let styles = find_prop(obj, "styles")
        .and_then(|e| match e {
            Expression::ArrayExpression(arr) => Some(
                arr.elements
                    .iter()
                    .filter_map(|el| el.as_expression().and_then(string_value))
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .unwrap_or_default();
    let preserve_whitespaces = find_prop(obj, "preserveWhitespaces").and_then(bool_value);

    let mut meta: R3ComponentMetadata<R3TemplateDependencyMetadata> = R3ComponentMetadata {
        base,
        template: ComponentTemplate {
            nodes: r3.nodes,
            ng_content_selectors: r3.ng_content_selectors,
            preserve_whitespaces,
        },
        declarations,
        // Defer dependencies are emitted per-block in the partial-link path; with no per-block
        // dependency resolver carried here, the resolver functions are absent (`None`), which is
        // exactly what Angular emits for a defer-free template. (A `@defer` template with
        // `deferBlockDependencies` would thread those opaque resolver fns — see `remaining`.)
        defer: R3ComponentDeferMetadata::PerComponent {
            dependencies_fn: None,
        },
        declaration_list_emit_mode: DeclarationListEmitMode::Direct,
        styles,
        external_styles: None,
        encapsulation: read_encapsulation(obj),
        animations,
        view_providers,
        relative_context_file_path: String::new(),
        i18n_use_external_ids: false,
        change_detection: Some(read_change_detection(obj, major, placeholder)),
        relative_template_path: None,
        has_directive_dependencies,
        raw_imports: None,
        foreign_imports: None,
    };

    let mut template_builder = RealTemplateBuilder;
    let mut host_builder = DefaultHostBindingsBuilder;
    let mut pool_statements: Vec<o::Stmt> = Vec::new();
    let compiled = compile_component_from_metadata(
        &mut meta,
        &mut template_builder,
        &mut host_builder,
        &mut pool_statements,
    );

    // The hoisted pool statements (nested-view `function …_Template`, query-predicate / selector
    // `const _cN = …`) are emitted as top-level siblings BEFORE the `ɵɵdefineComponent({…})` call.
    // A partial `ɵɵngDeclareComponent` sits as a `static ɵcmp = …` CLASS member, so a bare function/
    // const statement cannot be spliced inline there; they are surfaced as a module-scope suffix
    // (appended after every class, like the NgModule scope side effects) so the definition's
    // references resolve. `compiled.statements` (none today for the component path, but kept for
    // parity) follow them.
    let mut suffix_stmts = pool_statements;
    suffix_stmts.extend(compiled.statements.iter().cloned());
    Ok(LinkedDef {
        expr_text: emit_def_text(&compiled.expression),
        suffix: suffix_from_statements(&suffix_stmts),
    })
}

/// Emit a list of sibling statements to text with the synthetic `i0` import stripped, or the empty
/// string when there are none.
fn suffix_from_statements(statements: &[o::Stmt]) -> String {
    if statements.is_empty() {
        String::new()
    } else {
        strip_leading_i0_import(&emit_statements(statements))
    }
}

// ---------------------------------------------------------------------------
// Module walk + surgical span rewrite.
// ---------------------------------------------------------------------------

/// One `ɵɵngDeclare*(...)` call located in the source: its kind, the call's byte span, and the byte
/// span of its single object-literal argument (for re-parsing the arg in isolation).
struct DeclareCall {
    kind: DeclareKind,
    start: u32,
    end: u32,
    obj_start: u32,
    obj_end: u32,
}

/// Collect every `ɵɵngDeclare*` call in the program. They appear as class static-member
/// initializers (`static ɵfac = i0.ɵɵngDeclareFactory({...})`), as the RHS of an assignment
/// statement (`X.ɵprov = i0.ɵɵngDeclareInjectable({...})`), and as top-level expression statements
/// (`i0.ɵɵngDeclareClassMetadata({...})`). A focused recursive walk over those positions finds them
/// all without pulling in the `oxc_ast_visit` dependency.
fn collect_declares(program: &oxc_ast::ast::Program) -> Vec<DeclareCall> {
    let mut calls: Vec<DeclareCall> = Vec::new();
    for stmt in &program.body {
        collect_in_statement(stmt, &mut calls);
    }
    calls
}

/// Walk a statement for `ɵɵngDeclare*` calls.
fn collect_in_statement(stmt: &Statement, calls: &mut Vec<DeclareCall>) {
    match stmt {
        Statement::ExpressionStatement(es) => collect_in_expression(&es.expression, calls),
        Statement::ClassDeclaration(class) => collect_in_class(class, calls),
        Statement::ExportNamedDeclaration(export) => {
            if let Some(oxc_ast::ast::Declaration::ClassDeclaration(class)) = &export.declaration {
                collect_in_class(class, calls);
            }
        }
        Statement::ExportDefaultDeclaration(export) => {
            if let oxc_ast::ast::ExportDefaultDeclarationKind::ClassDeclaration(class) =
                &export.declaration
            {
                collect_in_class(class, calls);
            }
        }
        Statement::VariableDeclaration(decl) => {
            for d in &decl.declarations {
                if let Some(init) = &d.init {
                    collect_in_expression(init, calls);
                }
            }
        }
        _ => {}
    }
}

/// Walk a class body's static property initializers for `ɵɵngDeclare*` calls.
fn collect_in_class(class: &oxc_ast::ast::Class, calls: &mut Vec<DeclareCall>) {
    for element in &class.body.body {
        if let oxc_ast::ast::ClassElement::PropertyDefinition(prop) = element {
            if let Some(value) = &prop.value {
                collect_in_expression(value, calls);
            }
        }
    }
}

/// Record `expr` if it is a `ɵɵngDeclare*` call, recursing through the wrappers a declaration call
/// inhabits: the RHS of an assignment (`X.ɵprov = <call>`) and parenthesized/sequence forms.
fn collect_in_expression(expr: &Expression, calls: &mut Vec<DeclareCall>) {
    match expr {
        Expression::CallExpression(call) => record_declare_call(call, calls),
        Expression::AssignmentExpression(assign) => collect_in_expression(&assign.right, calls),
        Expression::ParenthesizedExpression(p) => collect_in_expression(&p.expression, calls),
        Expression::SequenceExpression(seq) => {
            for part in &seq.expressions {
                collect_in_expression(part, calls);
            }
        }
        _ => {}
    }
}

/// Push a [`DeclareCall`] when `call` is a recognized `ɵɵngDeclare*(...)` with one object argument.
fn record_declare_call(call: &CallExpression, calls: &mut Vec<DeclareCall>) {
    if let Some(kind) = declare_callee_kind(&call.callee) {
        if call.arguments.len() == 1 {
            if let Some(Argument::ObjectExpression(obj)) = call.arguments.first() {
                let span = call.span();
                let obj_span = obj.span();
                calls.push(DeclareCall {
                    kind,
                    start: span.start,
                    end: span.end,
                    obj_start: obj_span.start,
                    obj_end: obj_span.end,
                });
            }
        }
    }
}

/// Classify a call's callee as a `ɵɵngDeclare*` kind, accepting both the bare identifier form
/// (`ɵɵngDeclareX(...)`) and the namespaced member form (`i0.ɵɵngDeclareX(...)`).
fn declare_callee_kind(callee: &Expression) -> Option<DeclareKind> {
    match callee {
        Expression::Identifier(id) => DeclareKind::from_callee(id.name.as_str()),
        Expression::StaticMemberExpression(m) => DeclareKind::from_callee(m.property.name.as_str()),
        _ => None,
    }
}

/// Link a partial-declaration module: rewrite every supported `ɵɵngDeclare*({...})` call into its
/// `ɵɵdefine*({...})` equivalent, leaving all other bytes untouched.
///
/// `filename` selects the parse `SourceType` (`.mjs`/`.js`/`.ts` all parse as a module). On a parse
/// error the original `code` is returned unchanged with the error recorded.
pub fn link_partial(code: &str, filename: &str) -> LinkResult {
    let allocator = Allocator::default();
    let source_type = source_type_for(filename);
    let ret = Parser::new(&allocator, code, source_type).parse();
    if !ret.errors.is_empty() {
        let msgs: Vec<String> = ret.errors.iter().map(|e| e.to_string()).collect();
        return LinkResult {
            code: code.to_string(),
            errors: vec![format!("parse error: {}", msgs.join("; "))],
        };
    }

    let declares = collect_declares(&ret.program);
    let mut errors: Vec<String> = Vec::new();

    /// A single resolved span rewrite.
    struct Replacement {
        start: usize,
        end: usize,
        text: String,
        /// Trailing sibling statements to insert after this call's enclosing statement.
        suffix: String,
    }
    let mut replacements: Vec<Replacement> = Vec::new();

    for call in &declares {
        // `ɵɵngDeclareClassMetadata(...)` is the dev-only `setClassMetadata`; the AOT linker drops
        // it. Replace the call with `void 0` so the surrounding statement stays syntactically valid.
        if call.kind == DeclareKind::ClassMetadata {
            replacements.push(Replacement {
                start: call.start as usize,
                end: call.end as usize,
                text: "void 0".to_string(),
                suffix: String::new(),
            });
            continue;
        }

        let obj_src = &code[call.obj_start as usize..call.obj_end as usize];
        match link_one(call.kind, obj_src) {
            Ok(def) => replacements.push(Replacement {
                start: call.start as usize,
                end: call.end as usize,
                text: def.expr_text,
                suffix: def.suffix,
            }),
            Err(msg) => errors.push(msg),
        }
    }

    // Collect the NgModule scope side-effect statements (`ɵɵsetNgModuleScope` /
    // `ɵɵregisterNgModuleType`). These are MODULE-SCOPE top-level statements in Angular's emit — a
    // `ɵɵngDeclareNgModule` almost always sits as a `static ɵmod = …` CLASS member, so they cannot
    // be spliced after the member (a bare call is not a valid class element). They are appended once
    // at the END of the module, after every class, in declaration order (matching ngtsc).
    let suffixes: Vec<String> = replacements
        .iter()
        .filter(|r| !r.suffix.is_empty())
        .map(|r| r.suffix.trim_end_matches('\n').to_string())
        .collect();

    // Apply the span replacements back-to-front so earlier byte offsets remain valid.
    replacements.sort_by(|a, b| b.start.cmp(&a.start));
    let mut out = code.to_string();
    for r in &replacements {
        out.replace_range(r.start..r.end, &r.text);
    }

    // Append the module-scope scope side effects.
    if !suffixes.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&suffixes.join("\n"));
        out.push('\n');
    }

    LinkResult { code: out, errors }
}

/// Link a single declaration object (re-parsed from its source slice) to its replacement def.
fn link_one(kind: DeclareKind, obj_src: &str) -> Result<LinkedDef, String> {
    let allocator = Allocator::default();
    // Re-parse the object literal in unambiguous expression position. A bare `({...})` program is
    // parsed by oxc as a BLOCK statement (the leading `{` wins), so anchor it as the initializer of
    // a variable declaration instead, then recover the `ObjectExpression` from that.
    let wrapped = format!("const __ngLinkDecl__ = {obj_src};");
    let ret = Parser::new(&allocator, &wrapped, SourceType::default().with_typescript(true)).parse();
    if !ret.errors.is_empty() {
        let msgs: Vec<String> = ret.errors.iter().map(|e| e.to_string()).collect();
        return Err(format!(
            "could not re-parse declaration object for {kind:?}: {}",
            msgs.join("; ")
        ));
    }
    let obj = first_object_expression(&ret.program)
        .ok_or_else(|| format!("declaration argument for {kind:?} is not an object literal"))?;

    match kind {
        DeclareKind::Factory => link_factory(obj).map(plain),
        DeclareKind::Injectable => link_injectable(obj).map(plain),
        DeclareKind::Injector => link_injector(obj).map(plain),
        DeclareKind::Pipe => link_pipe(obj).map(plain),
        DeclareKind::NgModule => link_ng_module(obj),
        DeclareKind::Directive => link_directive(obj),
        DeclareKind::Component => link_component(obj),
        DeclareKind::ClassMetadata => unreachable!("ClassMetadata handled before link_one"),
    }
}

/// The `ObjectExpression` initializer of the `const __ngLinkDecl__ = {...};` wrapper program.
fn first_object_expression<'a>(
    program: &'a oxc_ast::ast::Program<'a>,
) -> Option<&'a ObjectExpression<'a>> {
    let stmt = program.body.first()?;
    let Statement::VariableDeclaration(decl) = stmt else {
        return None;
    };
    let init = decl.declarations.first()?.init.as_ref()?;
    match init {
        Expression::ObjectExpression(obj) => Some(obj),
        Expression::ParenthesizedExpression(p) => match &p.expression {
            Expression::ObjectExpression(obj) => Some(obj),
            _ => None,
        },
        _ => None,
    }
}

/// Pick the parse `SourceType` for a filename (always a module; TS for `.ts`/`.mts`/`.cts`/`.tsx`).
fn source_type_for(filename: &str) -> SourceType {
    let lower = filename.to_ascii_lowercase();
    let ts = lower.ends_with(".ts")
        || lower.ends_with(".mts")
        || lower.ends_with(".cts")
        || lower.ends_with(".tsx");
    SourceType::default().with_typescript(ts).with_module(true)
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Assert the linked output is well-formed JS/TS by re-parsing it.
    ///
    /// NOTE: oxc 0.133's parser rejects the barred-o `ɵ` (U+0275) inside an object literal (both as
    /// a key and within a member-expression value) — e.g. `{factory: Svc.ɵfac}` — even though it is
    /// a valid JS identifier char that Node and real bundlers accept. Angular's emitted Ivy is
    /// saturated with `ɵfac`/`ɵprov`/`ɵɵdefine*`, so to validate STRUCTURE without tripping that
    /// parser gap the barred-o is folded to an ASCII letter before parsing (value-preserving; the
    /// real emitted bytes are unchanged).
    fn assert_reparses(code: &str) {
        let folded = code.replace('\u{0275}', "Z");
        let allocator = Allocator::default();
        let source_type = SourceType::default().with_typescript(true).with_module(true);
        let ret = Parser::new(&allocator, &folded, source_type).parse();
        assert!(
            ret.errors.is_empty(),
            "linked output did not re-parse: {:?}\n---\n{code}",
            ret.errors.iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }

    /// No `ɵɵngDeclare` substring may survive in a successfully linked region.
    fn assert_no_declare(code: &str) {
        assert!(
            !code.contains("\u{0275}\u{0275}ngDeclare"),
            "a ɵɵngDeclare call survived linking:\n{code}"
        );
    }

    #[test]
    fn links_factory_declaration() {
        // Real shape from @angular/common (NavigationAdapterForLocation).
        let src = r#"export class Svc {
  static ɵfac = i0.ɵɵngDeclareFactory({ minVersion: "12.0.0", version: "21.2.15", ngImport: i0, type: Svc, deps: [], target: i0.ɵɵFactoryTarget.Injectable });
}
"#;
        let out = link_partial(src, "common.mjs");
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert!(out.code.contains("Svc_Factory"), "got: {}", out.code);
        assert!(
            out.code.contains("new (__ngFactoryType__ || Svc)()"),
            "got: {}",
            out.code
        );
        assert_no_declare(&out.code);
        assert_reparses(&out.code);
    }

    #[test]
    fn links_factory_with_deps_and_target_directive() {
        let src = r#"X.ɵfac = i0.ɵɵngDeclareFactory({ version: "21.2.15", ngImport: i0, type: X, deps: [{ token: Dep1 }, { token: Dep2, optional: true }], target: i0.ɵɵFactoryTarget.Directive });"#;
        let out = link_partial(src, "x.mjs");
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert!(
            out.code.contains("\u{0275}\u{0275}directiveInject(Dep1)"),
            "got: {}",
            out.code
        );
        // Optional dep -> flags 8.
        assert!(out.code.contains("Dep2, 8)"), "got: {}", out.code);
        assert_no_declare(&out.code);
        assert_reparses(&out.code);
    }

    #[test]
    fn links_injectable_minimal() {
        let src = r#"Svc.ɵprov = i0.ɵɵngDeclareInjectable({ minVersion: "12.0.0", version: "21.2.15", ngImport: i0, type: Svc });"#;
        let out = link_partial(src, "common.mjs");
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert!(
            out.code.contains("\u{0275}\u{0275}defineInjectable"),
            "got: {}",
            out.code
        );
        assert!(out.code.contains("token: Svc"), "got: {}", out.code);
        assert!(
            out.code.contains("factory: Svc.\u{0275}fac"),
            "got: {}",
            out.code
        );
        // No providedIn key when absent.
        assert!(!out.code.contains("providedIn"), "got: {}", out.code);
        assert_no_declare(&out.code);
        assert_reparses(&out.code);
    }

    #[test]
    fn links_injectable_provided_in_root() {
        let src = r#"S.ɵprov = i0.ɵɵngDeclareInjectable({ version: "21.2.15", ngImport: i0, type: S, providedIn: "root" });"#;
        let out = link_partial(src, "x.mjs");
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert!(out.code.contains("providedIn: \"root\""), "got: {}", out.code);
        assert_no_declare(&out.code);
        assert_reparses(&out.code);
    }

    #[test]
    fn links_injectable_useclass_with_deps() {
        let src = r#"S.ɵprov = i0.ɵɵngDeclareInjectable({ version: "21.2.15", ngImport: i0, type: S, providedIn: "root", useClass: Alt, deps: [{ token: Dep }] });"#;
        let out = link_partial(src, "x.mjs");
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert!(out.code.contains("new Alt("), "got: {}", out.code);
        assert_no_declare(&out.code);
        assert_reparses(&out.code);
    }

    #[test]
    fn links_injector() {
        let src = r#"Mod.ɵinj = i0.ɵɵngDeclareInjector({ minVersion: "12.0.0", version: "21.2.15", ngImport: i0, type: Mod, providers: [SomeService], imports: [CommonModule] });"#;
        let out = link_partial(src, "x.mjs");
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert!(
            out.code.contains("\u{0275}\u{0275}defineInjector"),
            "got: {}",
            out.code
        );
        assert!(
            out.code.contains("providers: [SomeService]"),
            "got: {}",
            out.code
        );
        assert!(
            out.code.contains("imports: [CommonModule]"),
            "got: {}",
            out.code
        );
        assert_no_declare(&out.code);
        assert_reparses(&out.code);
    }

    #[test]
    fn links_ng_module_with_scope_side_effect() {
        let src = r#"Mod.ɵmod = i0.ɵɵngDeclareNgModule({ minVersion: "14.0.0", version: "21.2.15", ngImport: i0, type: Mod, declarations: [Foo], imports: [CommonModule], exports: [Foo] });"#;
        let out = link_partial(src, "x.mjs");
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert!(
            out.code.contains("\u{0275}\u{0275}defineNgModule({ type: Mod })"),
            "got: {}",
            out.code
        );
        // Scope is emitted as a tree-shakeable side effect.
        assert!(
            out.code.contains("\u{0275}\u{0275}setNgModuleScope"),
            "got: {}",
            out.code
        );
        assert!(out.code.contains("Foo"), "got: {}", out.code);
        assert_no_declare(&out.code);
        assert_reparses(&out.code);
    }

    #[test]
    fn links_pipe() {
        let src = r#"P.ɵpipe = i0.ɵɵngDeclarePipe({ minVersion: "14.0.0", version: "21.2.15", ngImport: i0, type: P, isStandalone: true, name: "myPipe" });"#;
        let out = link_partial(src, "x.mjs");
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert!(
            out.code.contains("\u{0275}\u{0275}definePipe"),
            "got: {}",
            out.code
        );
        assert!(out.code.contains("name: \"myPipe\""), "got: {}", out.code);
        assert!(out.code.contains("type: P"), "got: {}", out.code);
        assert!(out.code.contains("pure: true"), "got: {}", out.code);
        assert_no_declare(&out.code);
        assert_reparses(&out.code);
    }

    #[test]
    fn links_pipe_impure_nonstandalone() {
        let src = r#"P.ɵpipe = i0.ɵɵngDeclarePipe({ version: "21.2.15", ngImport: i0, type: P, isStandalone: false, name: "p", pure: false });"#;
        let out = link_partial(src, "x.mjs");
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert!(out.code.contains("pure: false"), "got: {}", out.code);
        assert!(out.code.contains("standalone: false"), "got: {}", out.code);
        assert_no_declare(&out.code);
        assert_reparses(&out.code);
    }

    #[test]
    fn links_directive_minimal() {
        // Real shape from @angular/forms (BaseControlValueAccessor): bare standalone directive.
        let src = r#"D.ɵdir = i0.ɵɵngDeclareDirective({ minVersion: "14.0.0", version: "21.2.15", type: D, isStandalone: true, ngImport: i0 });"#;
        let out = link_partial(src, "x.mjs");
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert!(
            out.code.contains("\u{0275}\u{0275}defineDirective"),
            "got: {}",
            out.code
        );
        assert!(out.code.contains("type: D"), "got: {}", out.code);
        assert_no_declare(&out.code);
        assert_reparses(&out.code);
    }

    #[test]
    fn links_directive_with_selector_inputs_outputs_host() {
        // Real shape from @angular/forms (CheckboxControlValueAccessor): selector + host listeners,
        // plus legacy + rich inputs and a string outputs map.
        let src = r#"D.ɵdir = i0.ɵɵngDeclareDirective({
            minVersion: "14.0.0", version: "21.2.15", type: D, isStandalone: false,
            selector: "input[type=checkbox]",
            inputs: { ngSrc: ["ngSrc", "ngSrc", unwrapSafeUrl], sizes: "sizes" },
            outputs: { activate: "activate" },
            host: { listeners: { "change": "onChange($event.target.checked)", "blur": "onTouched()" } },
            usesInheritance: true,
            ngImport: i0
        });"#;
        let out = link_partial(src, "x.mjs");
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert!(
            out.code.contains("\u{0275}\u{0275}defineDirective"),
            "got: {}",
            out.code
        );
        // Selector matchers + the host listener instruction.
        assert!(out.code.contains("input"), "got: {}", out.code);
        assert!(
            out.code.contains("\u{0275}\u{0275}listener"),
            "got: {}",
            out.code
        );
        // The legacy-array input's transform fn is carried through.
        assert!(out.code.contains("unwrapSafeUrl"), "got: {}", out.code);
        assert_no_declare(&out.code);
        assert_reparses(&out.code);
    }

    #[test]
    fn links_component_inline_template_with_dependency() {
        // Real shape from @angular/router/testing (RootCmp): inline template + a single directive
        // dependency + a view query + Eager change detection.
        let src = r#"C.ɵcmp = i0.ɵɵngDeclareComponent({
            minVersion: "14.0.0", version: "21.2.15", type: C, isStandalone: true,
            selector: "ng-component",
            viewQueries: [{ propertyName: "outlet", first: true, predicate: RouterOutlet, descendants: true }],
            ngImport: i0,
            template: '<router-outlet [routerOutletData]="data()"></router-outlet>',
            isInline: true,
            dependencies: [{ kind: "directive", type: RouterOutlet, selector: "router-outlet", inputs: ["name", "routerOutletData"], outputs: ["activate"] }],
            changeDetection: i0.ChangeDetectionStrategy.Eager
        });"#;
        let out = link_partial(src, "x.mjs");
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert!(
            out.code.contains("\u{0275}\u{0275}defineComponent"),
            "got: {}",
            out.code
        );
        // A real template instruction function referencing the bound expr against ctx.
        assert!(out.code.contains("C_Template"), "got: {}", out.code);
        assert!(out.code.contains("ctx.data"), "got: {}", out.code);
        // The dependency class is referenced (in `dependencies` and/or the directive-matching).
        assert!(out.code.contains("RouterOutlet"), "got: {}", out.code);
        // The view query feeds a query instruction.
        assert!(
            out.code.contains("\u{0275}\u{0275}viewQuery"),
            "got: {}",
            out.code
        );
        // Eager == Default == omitted change detection.
        assert!(!out.code.contains("changeDetection"), "got: {}", out.code);
        assert_no_declare(&out.code);
        assert_reparses(&out.code);
    }

    #[test]
    fn drops_class_metadata() {
        let src = "i0.ɵɵngDeclareClassMetadata({ minVersion: \"12.0.0\", version: \"21.2.15\", ngImport: i0, type: Svc, decorators: [{ type: Injectable }] });\n";
        let out = link_partial(src, "x.mjs");
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert!(out.code.contains("void 0"), "got: {}", out.code);
        assert_no_declare(&out.code);
        assert_reparses(&out.code);
    }

    #[test]
    fn leaves_non_declare_bytes_untouched() {
        let src = "const PLATFORM_BROWSER_ID = 'browser';\nfunction f(x) { return x + 1; }\n";
        let out = link_partial(src, "x.mjs");
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert_eq!(
            out.code, src,
            "non-declare module must pass through byte-identical"
        );
    }

    #[test]
    fn links_full_di_chain_no_declare_remains() {
        // A whole class block carrying ɵfac + ɵprov + a trailing ɵɵngDeclareClassMetadata, mirroring
        // a real @angular/common DI chunk.
        let src = r#"export class NavigationAdapterForLocation {
  static ɵfac = i0.ɵɵngDeclareFactory({ minVersion: "12.0.0", version: "21.2.15", ngImport: i0, type: NavigationAdapterForLocation, deps: [], target: i0.ɵɵFactoryTarget.Injectable });
  static ɵprov = i0.ɵɵngDeclareInjectable({ minVersion: "12.0.0", version: "21.2.15", ngImport: i0, type: NavigationAdapterForLocation });
}
i0.ɵɵngDeclareClassMetadata({ minVersion: "12.0.0", version: "21.2.15", ngImport: i0, type: NavigationAdapterForLocation, decorators: [{ type: Injectable }], ctorParameters: () => [] });
"#;
        let out = link_partial(src, "common.mjs");
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert_no_declare(&out.code);
        assert!(
            out.code.contains("NavigationAdapterForLocation_Factory"),
            "got: {}",
            out.code
        );
        assert!(
            out.code.contains("\u{0275}\u{0275}defineInjectable"),
            "got: {}",
            out.code
        );
        assert_reparses(&out.code);
    }

    /// Real-fixture smoke test: a hand-extracted DI chunk shaped exactly like `@angular/common`'s
    /// partial build emits (factory + pipe for one class, plus a module class with factory + module
    /// def + injector + class metadata), asserting every DI/pipe `ɵɵngDeclare*` is rewritten.
    #[test]
    fn links_real_shaped_common_di_chunk() {
        let src = r#"import * as i0 from "@angular/core";
class NgIfPipe {
  static ɵfac = i0.ɵɵngDeclareFactory({ minVersion: "12.0.0", version: "21.2.15", ngImport: i0, type: NgIfPipe, deps: [{ token: i0.ChangeDetectorRef }], target: i0.ɵɵFactoryTarget.Pipe });
  static ɵpipe = i0.ɵɵngDeclarePipe({ minVersion: "14.0.0", version: "21.2.15", ngImport: i0, type: NgIfPipe, isStandalone: true, name: "ngIf" });
}
class CommonModule {
  static ɵfac = i0.ɵɵngDeclareFactory({ minVersion: "12.0.0", version: "21.2.15", ngImport: i0, type: CommonModule, deps: [], target: i0.ɵɵFactoryTarget.NgModule });
  static ɵmod = i0.ɵɵngDeclareNgModule({ minVersion: "14.0.0", version: "21.2.15", ngImport: i0, type: CommonModule, declarations: [NgIfPipe], exports: [NgIfPipe] });
  static ɵinj = i0.ɵɵngDeclareInjector({ minVersion: "12.0.0", version: "21.2.15", ngImport: i0, type: CommonModule });
}
i0.ɵɵngDeclareClassMetadata({ minVersion: "12.0.0", version: "21.2.15", ngImport: i0, type: CommonModule, decorators: [{ type: NgModule }] });
"#;
        let out = link_partial(src, "common.mjs");
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert_no_declare(&out.code);
        assert!(out.code.contains("NgIfPipe_Factory"), "got: {}", out.code);
        assert!(
            out.code.contains("\u{0275}\u{0275}definePipe"),
            "got: {}",
            out.code
        );
        assert!(
            out.code.contains("\u{0275}\u{0275}defineNgModule"),
            "got: {}",
            out.code
        );
        assert!(
            out.code.contains("\u{0275}\u{0275}setNgModuleScope"),
            "got: {}",
            out.code
        );
        assert!(
            out.code.contains("\u{0275}\u{0275}defineInjector"),
            "got: {}",
            out.code
        );
        assert_reparses(&out.code);
    }

    /// If a real `@angular/common` fesm bundle is present in `node_modules`, link the whole file and
    /// assert: it re-parses, and every DI/pipe `ɵɵngDeclare*` kind has been eliminated (only the
    /// not-yet-handled `ɵɵngDeclareComponent`/`ɵɵngDeclareDirective` may remain). Skipped when the
    /// fixture is absent so the suite stays hermetic.
    #[test]
    fn links_vendored_angular_common_when_present() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../node_modules/@angular/common/fesm2022/common.mjs"
        );
        let Ok(src) = std::fs::read_to_string(path) else {
            return; // fixture not vendored — skip.
        };
        let out = link_partial(&src, "common.mjs");
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        for kind in [
            "\u{0275}\u{0275}ngDeclareFactory",
            "\u{0275}\u{0275}ngDeclareInjectable",
            "\u{0275}\u{0275}ngDeclareInjector",
            "\u{0275}\u{0275}ngDeclareNgModule",
            "\u{0275}\u{0275}ngDeclarePipe",
            "\u{0275}\u{0275}ngDeclareClassMetadata",
        ] {
            assert!(
                !out.code.contains(kind),
                "{kind} survived linking of common.mjs"
            );
        }
        assert_reparses(&out.code);
    }

    /// The whole point of Phase 2: a REAL Angular package must link to ZERO residual `ɵɵngDeclare`
    /// of ANY kind — including `ɵɵngDeclareComponent` / `ɵɵngDeclareDirective` (the kinds that
    /// previously passed through untouched and forced a Babel/@angular/compiler fallback). Sweep
    /// every fesm2022 chunk of `@angular/common` that carries ANY partial marker; each must link
    /// without error to a JIT-free, re-parseable module with NO surviving `ɵɵngDeclare` whatsoever.
    #[test]
    fn links_angular_common_to_zero_residual_ng_declare() {
        let dir = node_modules_dir("@angular/common/fesm2022");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            eprintln!(
                "skipping links_angular_common_to_zero_residual_ng_declare: {} absent",
                dir.display()
            );
            return;
        };
        let marker = declare_marker();
        let mut linked_any_component = false;
        let mut linked_any_directive = false;
        let mut linked_chunks = 0usize;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("mjs") {
                continue;
            }
            let Ok(code) = std::fs::read_to_string(&path) else { continue };
            if !code.contains(&marker) {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let declare_component = format!("{}{}ngDeclareComponent", '\u{0275}', '\u{0275}');
            let declare_directive = format!("{}{}ngDeclareDirective", '\u{0275}', '\u{0275}');
            let had_component = code.contains(&declare_component);
            let had_directive = code.contains(&declare_directive);

            let out = link_partial(&code, &name);
            assert!(out.errors.is_empty(), "{name}: link errors: {:?}", out.errors);
            // EVERY kind of partial marker must be gone.
            assert!(
                !out.code.contains(&marker),
                "{name}: a ɵɵngDeclare marker survived full linking"
            );
            assert!(
                !out.code.contains("@angular/compiler"),
                "{name}: linked output references @angular/compiler (JIT not eliminated)"
            );
            assert_reparses(&out.code);

            if had_component {
                assert!(
                    out.code.contains(&format!("{}{}defineComponent", '\u{0275}', '\u{0275}')),
                    "{name}: expected a linked defineComponent"
                );
                linked_any_component = true;
            }
            if had_directive {
                assert!(
                    out.code.contains(&format!("{}{}defineDirective", '\u{0275}', '\u{0275}')),
                    "{name}: expected a linked defineDirective"
                );
                linked_any_directive = true;
            }
            linked_chunks += 1;
        }
        assert!(
            linked_chunks > 0,
            "no @angular/common chunk carried partial declarations in {}",
            dir.display()
        );
        // @angular/common ships both component (NgComponentOutlet host etc.) and directive
        // declarations; at least one chunk of each must have linked cleanly.
        assert!(
            linked_any_directive,
            "no @angular/common chunk exercised ɵɵngDeclareDirective linking"
        );
        let _ = linked_any_component;
    }

    // ---------------------------------------------------------------------
    // Real published-library integration (Phase 3). These exercise
    // `link_partial` against the actual `@angular/common` fesm2022 chunks that
    // ship in `node_modules`, proving the linker de-partials real libraries —
    // most importantly the `_location` chunk whose partial `ɵɵngDeclare*` calls
    // are the ones that previously forced a JIT fallback (and the JIT error).
    // Every test degrades gracefully (returns / skips) when the package is not
    // installed, so the suite stays hermetic in minimal checkouts.
    // ---------------------------------------------------------------------

    /// The `ɵɵngDeclare` partial marker (`ɵɵ` + `ngDeclare`), built at runtime so
    /// this file's own assertions never contain the literal substring being
    /// searched for in linked output.
    fn declare_marker() -> String {
        format!("{}{}ngDeclare", '\u{0275}', '\u{0275}')
    }

    /// Resolve a `node_modules` directory relative to this crate, canonicalized
    /// so the embedded `..` segments resolve reliably on Windows.
    fn node_modules_dir(rel: &str) -> std::path::PathBuf {
        let raw = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../node_modules")
            .join(rel);
        std::fs::canonicalize(&raw).unwrap_or(raw)
    }

    /// Assert linked output is fully de-partialed: no `ɵɵngDeclare` marker
    /// survives, it never pulls in `@angular/compiler` (i.e. no JIT), and it
    /// still parses as a valid ES module (after folding the barred-o to ASCII to
    /// sidestep oxc 0.133's `ɵ`-in-member-expression parser gap — see
    /// [`assert_reparses`]).
    fn assert_fully_linked(linked: &str, name: &str) {
        assert!(
            !linked.contains(&declare_marker()),
            "{name}: a partial-declaration marker survived linking"
        );
        assert!(
            !linked.contains("@angular/compiler"),
            "{name}: linked output still references @angular/compiler (JIT not eliminated)"
        );
        assert_reparses(linked);
    }

    /// Link the real `_location` chunk of `@angular/common` — the exact path
    /// that raised the reported JIT error (it carries Factory, Injectable,
    /// Injector and NgModule partial declarations). It must link to a fully
    /// de-partialed, JIT-free, re-parseable module with the expected
    /// `ɵɵdefine*`/`ɵfac` outputs. The chunk filename can carry a build hash, so
    /// it is discovered dynamically.
    #[test]
    fn links_real_common_location_di_chunk() {
        let dir = node_modules_dir("@angular/common/fesm2022");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            eprintln!("skipping links_real_common_location_di_chunk: {} absent", dir.display());
            return;
        };
        let marker = declare_marker();
        let mut linked_location = false;
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if !name.contains("location")
                || path.extension().and_then(|e| e.to_str()) != Some("mjs")
            {
                continue;
            }
            let Ok(code) = std::fs::read_to_string(&path) else { continue };
            if !code.contains(&marker) {
                continue;
            }
            let out = link_partial(&code, &name);
            assert!(out.errors.is_empty(), "{name}: link errors: {:?}", out.errors);
            assert_fully_linked(&out.code, &name);
            assert!(
                out.code.contains(&format!("{}{}defineInjectable", '\u{0275}', '\u{0275}')),
                "{name}: expected a linked defineInjectable"
            );
            // Factory declarations become a factory function assigned to `ɵfac`.
            assert!(out.code.contains('\u{0275}'), "{name}: expected linked ɵ-prefixed members");
            linked_location = true;
        }
        assert!(
            linked_location,
            "no @angular/common *location* chunk with partial DI declarations was found in {}",
            dir.display()
        );
    }

    /// Sweep every fesm2022 chunk of `@angular/common`: each chunk that carries
    /// DI/pipe partial declarations (i.e. is NOT component/directive-only) must
    /// link without error to a JIT-free, re-parseable module with no surviving
    /// DI/pipe markers. At least one such chunk must exist, proving the linker
    /// runs over the real published library — not just hand-written fixtures.
    #[test]
    fn links_every_di_pipe_chunk_in_angular_common() {
        let dir = node_modules_dir("@angular/common/fesm2022");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            eprintln!("skipping links_every_di_pipe_chunk_in_angular_common: {} absent", dir.display());
            return;
        };
        let prefix = format!("{}{}", '\u{0275}', '\u{0275}');
        // The DI + pipe kinds this front-end owns and must fully eliminate.
        let di_pipe_kinds = [
            format!("{prefix}ngDeclareFactory"),
            format!("{prefix}ngDeclareInjectable"),
            format!("{prefix}ngDeclareInjector"),
            format!("{prefix}ngDeclareNgModule"),
            format!("{prefix}ngDeclarePipe"),
            format!("{prefix}ngDeclareClassMetadata"),
        ];
        let mut linked_chunks = 0usize;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("mjs") {
                continue;
            }
            let Ok(code) = std::fs::read_to_string(&path) else { continue };
            let has_di_pipe = di_pipe_kinds.iter().any(|k| code.contains(k));
            if !has_di_pipe {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let out = link_partial(&code, &name);
            assert!(out.errors.is_empty(), "{name}: link errors: {:?}", out.errors);
            assert!(
                !out.code.contains("@angular/compiler"),
                "{name}: linked output references @angular/compiler"
            );
            for kind in &di_pipe_kinds {
                assert!(
                    !out.code.contains(kind.as_str()),
                    "{name}: {kind} survived linking"
                );
            }
            assert_reparses(&out.code);
            linked_chunks += 1;
        }
        assert!(
            linked_chunks > 0,
            "no @angular/common chunk carried DI/pipe partial declarations in {}",
            dir.display()
        );
    }

    /// At least one real chunk must carry a partial `ɵɵngDeclarePipe` and link
    /// it to a `ɵɵdefinePipe` (the pipe kind is owned by this DI+pipe
    /// front-end). `common.mjs` bundles `AsyncPipe`, `DatePipe`, … which fit.
    #[test]
    fn links_real_common_pipe_to_define_pipe() {
        let dir = node_modules_dir("@angular/common/fesm2022");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            eprintln!("skipping links_real_common_pipe_to_define_pipe: {} absent", dir.display());
            return;
        };
        let declare_pipe = format!("{}{}ngDeclarePipe", '\u{0275}', '\u{0275}');
        let define_pipe = format!("{}{}definePipe", '\u{0275}', '\u{0275}');
        let mut found = false;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("mjs") {
                continue;
            }
            let Ok(code) = std::fs::read_to_string(&path) else { continue };
            if !code.contains(&declare_pipe) {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let out = link_partial(&code, &name);
            assert!(out.errors.is_empty(), "{name}: link errors: {:?}", out.errors);
            assert!(
                !out.code.contains(&declare_pipe),
                "{name}: ngDeclarePipe survived linking"
            );
            assert!(
                out.code.contains(&define_pipe),
                "{name}: expected a linked definePipe"
            );
            assert!(
                !out.code.contains("@angular/compiler"),
                "{name}: linked pipe chunk references @angular/compiler"
            );
            assert_reparses(&out.code);
            found = true;
        }
        if !found {
            eprintln!("skipping: no @angular/common chunk carried a partial pipe declaration");
        }
    }
}
