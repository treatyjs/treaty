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
    Argument, CallExpression, Expression, ObjectExpression, ObjectPropertyKind, Program,
    PropertyKey, Statement,
};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType};

use crate::factory::{
    compile_factory_function, compile_injectable, FactoryDeps, FactoryTarget, ForwardRefHandling,
    MaybeForwardRef, R3ConstructorFactoryMetadata, R3DependencyMetadata, R3FactoryMetadata,
    R3InjectableMetadata,
};
use crate::output::emitter::emit_expression;
use crate::output_ast::{self as o, Expr, LiteralValue};
use crate::pipe_module_injector::{
    compile_injector, compile_ng_module, compile_pipe_from_metadata, R3InjectorMetadata,
    R3NgModuleCommon, R3NgModuleMetadata, R3NgModuleMetadataGlobal, R3PipeMetadata,
    R3SelectorScopeMode,
};
use crate::util::R3Reference;

/// The result of linking a partial-declaration module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkResult {
    /// The module source with every linkable `ɵɵngDeclare*(...)` call rewritten to `ɵɵdefine*(...)`.
    /// Byte-identical to the input outside the rewritten call spans.
    pub code: String,
    /// Diagnostics for declarations that could not be linked (the offending call is left as-is).
    pub errors: Vec<String>,
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

/// Whether the object literal carries a property `name` (`metaObj.has(name)`).
fn has_prop(obj: &ObjectExpression, name: &str) -> bool {
    find_prop(obj, name).is_some()
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

/// `depObj.getBoolean(name)` — a property that is the boolean literal `true`.
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

/// The "symbol name" of a `type`/token expression — its bare identifier (`Foo`) or, for
/// `ns.Foo`, the final property name. Mirrors `AstValue.getSymbolName()` for the cases that appear
/// as a partial-declaration `type`.
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
            let callee: Expr = convert_expr(&new_expr.callee)?;
            let mut args: Vec<Expr> = Vec::with_capacity(new_expr.arguments.len());
            for a in &new_expr.arguments {
                let inner = a.as_expression()?;
                args.push(convert_expr(inner)?);
            }
            // `new callee(...args)` via the `Expr::instantiate` builder.
            let built: Expr = Expr::instantiate(callee, args);
            Some(built)
        }
        Expression::ParenthesizedExpression(p) => convert_expr(&p.expression),
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
    let is_forward_ref = matches!(&call.callee, Expression::Identifier(id) if id.name == "forwardRef");
    if !is_forward_ref {
        return None;
    }
    let arg = call.arguments.first().and_then(|a| a.as_expression())?;
    arrow_or_fn_return(arg)
}

/// The expression returned by a `() => X` arrow (or `function(){ return X; }`).
fn arrow_or_fn_return(expr: &Expression) -> Option<Expr> {
    use oxc_ast::ast::Statement;
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
/// wrapper is unwrapped (the elements come from the returned array). Non-identifier elements that
/// nonetheless convert (e.g. `ns.Foo`) are kept; unconvertible elements are skipped.
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
    use oxc_ast::ast::Statement;
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
            "\u{0275}\u{0275}ngDeclareClassMetadata" => Some(DeclareKind::ClassMetadata),
            _ => None,
        }
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
        name: name.clone(),
        ty: class_ref(&name),
        type_argument_count: 0,
        deps: get_dependencies(obj)?,
        target,
    });
    let compiled = compile_factory_function(&meta);
    Ok(emit_expression(&compiled.expression))
}

/// `toR3InjectableMeta` + `compileInjectable(meta, false)` → the `ɵɵdefineInjectable({…})` text.
fn link_injectable(obj: &ObjectExpression) -> Result<String, String> {
    let type_expr =
        find_prop(obj, "type").ok_or_else(|| "ɵɵngDeclareInjectable missing `type`".to_string())?;
    let name = symbol_name(type_expr)
        .ok_or_else(|| "ɵɵngDeclareInjectable `type` has no symbol name".to_string())?;

    let provided_in = match find_prop(obj, "providedIn") {
        Some(e) => extract_forward_ref(e)
            .ok_or_else(|| "unsupported `providedIn` expression".to_string())?,
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
        Some(Expression::ArrayExpression(arr)) => {
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
            Some(out)
        }
        Some(_) => return Err("injectable `deps` must be an array".to_string()),
        None => None,
    };

    let meta = R3InjectableMetadata {
        name: name.clone(),
        ty: class_ref(&name),
        type_argument_count: 0,
        provided_in,
        use_class,
        use_factory,
        use_existing,
        use_value,
        deps,
    };
    let compiled = compile_injectable(&meta, false);
    Ok(emit_expression(&compiled.expression))
}

/// Read a `useClass`/`useExisting`/`useValue` option into a [`MaybeForwardRef`] (absent → `None`).
fn forward_ref_option(
    obj: &ObjectExpression,
    key: &str,
) -> Result<Option<MaybeForwardRef>, String> {
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
    Ok(emit_expression(&compiled.expression))
}

/// Build a class [`R3Reference`] from a `type` expression, carrying the exact `value`/`type`
/// expression (`ns.Foo` is preserved) rather than just its symbol name.
fn class_ref_from(type_expr: &Expression) -> Result<R3Reference, String> {
    let value = convert_expr(type_expr).ok_or_else(|| "unsupported `type` expression".to_string())?;
    Ok(R3Reference {
        ty: value.clone(),
        value,
    })
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
        crate::output::emitter::emit_statements(&compiled.statements)
    };
    Ok(LinkedDef {
        expr_text: emit_expression(&compiled.expression),
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
        name: name.clone(),
        r#type: class_ref(&name),
        type_argument_count: 0,
        pipe_name,
        deps: None,
        pure,
        is_standalone,
    };
    let compiled = compile_pipe_from_metadata(&meta);
    Ok(emit_expression(&compiled.expression))
}

/// One linked declaration: the replacement expression text plus any trailing sibling statements
/// (only NgModule produces a non-empty `suffix`).
struct LinkedDef {
    expr_text: String,
    suffix: String,
}

// ---------------------------------------------------------------------------
// Module walk + surgical span rewrite.
// ---------------------------------------------------------------------------

/// One `ɵɵngDeclare*(...)` call located in the source: its kind, byte span, and (for the
/// statement-dropping `ClassMetadata` case) whether it is the whole expression statement.
struct DeclareCall {
    kind: DeclareKind,
    /// The call expression's byte span `[start, end)`.
    start: u32,
    end: u32,
    /// Byte offset of the single object-literal argument's span start (for re-parsing the arg).
    obj_start: u32,
    obj_end: u32,
}

/// Collect every `ɵɵngDeclare*` call in the program. `ɵɵngDeclare*` calls appear as class static
/// member initializers (`static ɵfac = i0.ɵɵngDeclareFactory({...})`) and as top-level expression
/// statements (`i0.ɵɵngDeclareClassMetadata({...})`). A focused recursive walk over the relevant
/// expression positions finds them all without pulling in the `oxc_ast_visit` dependency.
fn collect_declares(program: &Program) -> Vec<DeclareCall> {
    let mut calls: Vec<DeclareCall> = Vec::new();
    for stmt in &program.body {
        collect_in_statement(stmt, &mut calls);
    }
    calls
}

/// Walk a statement for `ɵɵngDeclare*` calls (class declarations + their static-member
/// initializers, exported declarations, and top-level expression statements).
fn collect_in_statement(stmt: &Statement, calls: &mut Vec<DeclareCall>) {
    match stmt {
        Statement::ExpressionStatement(es) => collect_in_expression(&es.expression, calls),
        Statement::ClassDeclaration(class) => collect_in_class(class, calls),
        Statement::ExportNamedDeclaration(export) => {
            if let Some(decl) = &export.declaration {
                if let oxc_ast::ast::Declaration::ClassDeclaration(class) = decl {
                    collect_in_class(class, calls);
                }
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

/// Record `expr` if it is a `ɵɵngDeclare*` call. Also recurse into the positions a declaration
/// call inhabits: the RHS of an assignment (`X.ɵprov = i0.ɵɵngDeclareInjectable({...})`), a
/// parenthesized/sequence wrapper, and the `/* @__PURE__ */`-style wrapping that some bundlers
/// leave around the call (a parenthesized call). This is intentionally shallow — a `ɵɵngDeclare*`
/// call never nests inside another expression beyond these wrappers.
fn collect_in_expression(expr: &Expression, calls: &mut Vec<DeclareCall>) {
    match expr {
        Expression::CallExpression(call) => record_declare_call(call, calls),
        // `X.ɵprov = <call>` — the declaration is the assignment's right-hand side.
        Expression::AssignmentExpression(assign) => collect_in_expression(&assign.right, calls),
        Expression::ParenthesizedExpression(p) => collect_in_expression(&p.expression, calls),
        // `(a, b)` sequence — walk each part (defensive; not produced by Angular here).
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

    // Build the replacement for each located call (re-parsing its object argument in isolation so
    // the per-kind readers operate on a fresh, lifetime-clean AST).
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
        match link_one(call.kind, obj_src, filename) {
            Ok(def) => replacements.push(Replacement {
                start: call.start as usize,
                end: call.end as usize,
                text: def.expr_text,
                suffix: def.suffix,
            }),
            Err(msg) => errors.push(msg),
        }
    }

    // Apply replacements back-to-front so earlier byte offsets remain valid. A non-empty `suffix`
    // (NgModule scope side effects) is inserted after the end of the statement that contains the
    // call — found by scanning forward to the statement terminator.
    replacements.sort_by(|a, b| b.start.cmp(&a.start));
    let mut out = code.to_string();
    for r in &replacements {
        if !r.suffix.is_empty() {
            let insert_at = statement_end_after(&out, r.end);
            let mut suffix = String::from("\n");
            suffix.push_str(r.suffix.trim_end_matches('\n'));
            out.insert_str(insert_at, &suffix);
        }
        out.replace_range(r.start..r.end, &r.text);
    }

    LinkResult { code: out, errors }
}

/// Link a single declaration object (re-parsed from its source slice) to its replacement def.
fn link_one(kind: DeclareKind, obj_src: &str, _filename: &str) -> Result<LinkedDef, String> {
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
        DeclareKind::ClassMetadata => unreachable!("ClassMetadata handled before link_one"),
    }
}

/// Wrap a plain expression text (no trailing statements) as a [`LinkedDef`].
fn plain(expr_text: String) -> LinkedDef {
    LinkedDef {
        expr_text,
        suffix: String::new(),
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

/// The byte offset just past the statement terminator (`;` and following newline) that ends the
/// statement containing the call ending at `from`. Falls back to `from` when none is found.
fn statement_end_after(code: &str, from: usize) -> usize {
    let bytes = code.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        match bytes[i] {
            b';' => {
                i += 1;
                // Swallow a trailing newline so the inserted suffix lands on its own line.
                if i < bytes.len() && bytes[i] == b'\n' {
                    i += 1;
                }
                return i;
            }
            b'\n' => {
                i += 1;
                return i;
            }
            _ => i += 1,
        }
    }
    from
}

/// Pick the parse `SourceType` for a filename (always a module; TS for `.ts`/`.mts`/`.cts`).
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

    /// Re-parse `code` and assert it has no syntax errors (the linked output must be valid JS/TS).
    fn assert_reparses(code: &str) {
        let allocator = Allocator::default();
        let ret = Parser::new(&allocator, code, SourceType::default().with_typescript(true)).parse();
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
        assert!(out.code.contains("new (__ngFactoryType__ || Svc)()"), "got: {}", out.code);
        assert_no_declare(&out.code);
        assert_reparses(&out.code);
    }

    #[test]
    fn links_factory_with_deps_and_target_directive() {
        let src = r#"X.ɵfac = i0.ɵɵngDeclareFactory({ version: "21.2.15", ngImport: i0, type: X, deps: [{ token: Dep1 }, { token: Dep2, optional: true }], target: i0.ɵɵFactoryTarget.Directive });"#;
        let out = link_partial(src, "x.mjs");
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert!(out.code.contains("\u{0275}\u{0275}directiveInject(Dep1)"), "got: {}", out.code);
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
        assert!(out.code.contains("\u{0275}\u{0275}defineInjectable"), "got: {}", out.code);
        assert!(out.code.contains("token: Svc"), "got: {}", out.code);
        assert!(out.code.contains("factory: Svc.\u{0275}fac"), "got: {}", out.code);
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
        assert!(out.code.contains("\u{0275}\u{0275}defineInjector"), "got: {}", out.code);
        assert!(out.code.contains("providers: [SomeService]"), "got: {}", out.code);
        assert!(out.code.contains("imports: [CommonModule]"), "got: {}", out.code);
        assert_no_declare(&out.code);
        assert_reparses(&out.code);
    }

    #[test]
    fn links_ng_module_with_scope_side_effect() {
        let src = r#"Mod.ɵmod = i0.ɵɵngDeclareNgModule({ minVersion: "14.0.0", version: "21.2.15", ngImport: i0, type: Mod, declarations: [Foo], imports: [CommonModule], exports: [Foo] });"#;
        let out = link_partial(src, "x.mjs");
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert!(out.code.contains("\u{0275}\u{0275}defineNgModule({ type: Mod })"), "got: {}", out.code);
        // Scope is emitted as a tree-shakeable side effect.
        assert!(out.code.contains("\u{0275}\u{0275}setNgModuleScope"), "got: {}", out.code);
        assert!(out.code.contains("Foo"), "got: {}", out.code);
        assert_no_declare(&out.code);
        assert_reparses(&out.code);
    }

    #[test]
    fn links_pipe() {
        let src = r#"P.ɵpipe = i0.ɵɵngDeclarePipe({ minVersion: "14.0.0", version: "21.2.15", ngImport: i0, type: P, isStandalone: true, name: "myPipe" });"#;
        let out = link_partial(src, "x.mjs");
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        assert!(out.code.contains("\u{0275}\u{0275}definePipe"), "got: {}", out.code);
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
        assert_eq!(out.code, src, "non-declare module must pass through byte-identical");
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
        assert!(out.code.contains("NavigationAdapterForLocation_Factory"), "got: {}", out.code);
        assert!(out.code.contains("\u{0275}\u{0275}defineInjectable"), "got: {}", out.code);
        assert_reparses(&out.code);
    }
}
