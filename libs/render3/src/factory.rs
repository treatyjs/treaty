//! r3 factory codegen (`ɵfac`) — `compileFactoryFunction`.
//!
//! PORT TARGET: `migration/render3-specs/15-factory.md`
//! Source: `tools/angular-ref/packages/compiler/src/render3/r3_factory.ts`
//!
//! Generates the **factory function** for any Angular type that participates in dependency
//! injection (components, directives, pipes, injectables, NgModules). The factory becomes the
//! `ɵfac` static member of a class, e.g.
//!
//! ```text
//! function MyCmp_Factory(t) { return new (t || MyCmp)(i0.ɵɵdirectiveInject(Dep)); }
//! ```
//!
//! This module is a **pure producer of `output_ast`**: it does no parsing and reads no template
//! AST. It returns an [`R3CompiledExpression`] (expression + type + side-effect statements) that
//! the caller wires into a class; it never mutates a class itself.
//!
//! Builds the owned `output_ast` IR via the [`crate::output_ast`] builder fns and references
//! runtime symbols through [`crate::identifiers::R3`].

use crate::identifiers::R3;
use crate::output_ast::{
    self as o, ArrowBody, BinaryOperator, Expr, ExprKind, FnParam, LeadingComment, LiteralValue,
    Stmt, StmtKind, Type,
};

// ---------------------------------------------------------------------------
// Local placeholders for not-yet-ported sibling types (`render3/util.ts`,
// `compiler_facade_interface.ts`, `core.ts`). These mirror the real shapes so the
// algorithm is reproduced faithfully; replace with the real ports when available.
// ---------------------------------------------------------------------------

/// Placeholder for `render3/util.ts`'s `R3Reference` (`{value, type}`). `type` is renamed `ty`
/// because `type` is reserved in Rust. Both fields are `output_ast` expressions.
///
/// NOTE(port): real `R3Reference` lives in `render3/util.ts` (not yet ported).
#[derive(Debug, Clone, PartialEq)]
pub struct R3Reference {
    pub value: Expr,
    pub ty: Expr,
}

/// Placeholder for `render3/util.ts`'s `R3CompiledExpression` (`{expression, type, statements}`).
/// `type` renamed `ty` (it is an `output_ast` [`Type`], not a TS type annotation).
///
/// NOTE(port): real `R3CompiledExpression` lives in `render3/util.ts` (not yet ported).
#[derive(Debug, Clone, PartialEq)]
pub struct R3CompiledExpression {
    pub expression: Expr,
    pub ty: Type,
    /// Always empty for the factory, but part of the shared return shape.
    pub statements: Vec<Stmt>,
}

/// Placeholder for `compiler_facade_interface.ts`'s `FactoryTarget` enum. Discriminants pinned to
/// the v22.1 source (`Directive = 0 … Service = 5`).
///
/// NOTE(port): real `FactoryTarget` lives in `compiler_facade_interface.ts` (not yet ported).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactoryTarget {
    Directive = 0,
    Component = 1,
    Injectable = 2,
    Pipe = 3,
    NgModule = 4,
    Service = 5,
}

/// Placeholder for `core.ts`'s `InjectFlags` const enum (bitflags). `bitflags` is unavailable, so
/// a plain `u8` newtype with `BitOr`, mirroring how the source ORs flags together.
///
/// NOTE(port): real `InjectFlags` lives in `core.ts` (not yet ported).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InjectFlags(pub u8);

impl InjectFlags {
    pub const DEFAULT: InjectFlags = InjectFlags(0b0_0000);
    pub const HOST: InjectFlags = InjectFlags(1 << 0);
    pub const SELF: InjectFlags = InjectFlags(1 << 1);
    pub const SKIP_SELF: InjectFlags = InjectFlags(1 << 2);
    pub const OPTIONAL: InjectFlags = InjectFlags(1 << 3);
    /// `@internal` flag used for pipe-target dependencies.
    pub const FOR_PIPE: InjectFlags = InjectFlags(1 << 4);

    #[inline]
    pub fn bits(self) -> u8 {
        self.0
    }
}

impl std::ops::BitOr for InjectFlags {
    type Output = InjectFlags;
    fn bitor(self, rhs: InjectFlags) -> InjectFlags {
        InjectFlags(self.0 | rhs.0)
    }
}

// ---------------------------------------------------------------------------
// Factory metadata input types (`r3_factory.ts` interfaces/enums).
// ---------------------------------------------------------------------------

/// `R3FactoryDelegateType` — whether the delegate is instantiated (`new delegate(...)`) or invoked
/// (`delegate(...)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum R3FactoryDelegateType {
    Class = 0,
    Function = 1,
}

/// Tri-state for constructor dependencies, encoding the TS `R3DependencyMetadata[] | 'invalid' |
/// null` union explicitly (see spec §7 — do NOT collapse to `Option<Vec<...>>`).
///
/// - [`FactoryDeps::Inherit`] (`null`)  — no constructor; inherit the factory from the base class.
/// - [`FactoryDeps::Invalid`] (`'invalid'`) — at least one dep unresolvable; emit `ɵɵinvalidFactory`.
/// - [`FactoryDeps::Deps`] (array)      — resolved dependency list (may be empty).
#[derive(Debug, Clone, PartialEq)]
pub enum FactoryDeps {
    Inherit,
    Invalid,
    Deps(Vec<R3DependencyMetadata>),
}

/// The shared `R3ConstructorFactoryMetadata` fields, common to every metadata kind.
#[derive(Debug, Clone, PartialEq)]
pub struct R3ConstructorFactoryMetadata {
    /// String name of the type being generated (used to name the factory function).
    pub name: String,
    /// An expression/`R3Reference` representing the type being constructed.
    pub ty: R3Reference,
    /// Number of type arguments for the `type` (used for the `.d.ts` declaration type).
    pub type_argument_count: u32,
    /// Constructor dependencies (tri-state, see [`FactoryDeps`]).
    pub deps: FactoryDeps,
    /// Type of the target being created by the factory.
    pub target: FactoryTarget,
}

/// The `R3FactoryMetadata` union (`r3_factory.ts`). The TS union is duck-typed (discriminated by
/// presence of `delegateType` / `expression`); the port models it as a proper enum, with the shared
/// fields living in a `base: R3ConstructorFactoryMetadata`.
#[derive(Debug, Clone, PartialEq)]
pub enum R3FactoryMetadata {
    /// Created by direct constructor invocation (or base-class inheritance when deps == Inherit).
    Constructor(R3ConstructorFactoryMetadata),
    /// Created by delegating to another class (`new delegate(...)`) or function (`delegate(...)`).
    Delegated {
        base: R3ConstructorFactoryMetadata,
        delegate: Expr,
        delegate_type: R3FactoryDelegateType,
        delegate_deps: Vec<R3DependencyMetadata>,
    },
    /// Created by evaluating an arbitrary user expression (e.g. `useFactory`/`useValue`).
    Expression {
        base: R3ConstructorFactoryMetadata,
        expression: Expr,
    },
}

impl R3FactoryMetadata {
    /// The shared `R3ConstructorFactoryMetadata` for any kind.
    pub fn base(&self) -> &R3ConstructorFactoryMetadata {
        match self {
            R3FactoryMetadata::Constructor(base)
            | R3FactoryMetadata::Delegated { base, .. }
            | R3FactoryMetadata::Expression { base, .. } => base,
        }
    }

    /// `isDelegatedFactoryMetadata(meta)` — the `delegateType !== undefined` guard.
    pub fn is_delegated(&self) -> bool {
        matches!(self, R3FactoryMetadata::Delegated { .. })
    }

    /// `isExpressionFactoryMetadata(meta)` — the `expression !== undefined` guard.
    pub fn is_expression(&self) -> bool {
        matches!(self, R3FactoryMetadata::Expression { .. })
    }
}

/// `R3DependencyMetadata` — metadata for a single constructor dependency.
#[derive(Debug, Clone, PartialEq)]
pub struct R3DependencyMetadata {
    /// The token/value to inject, or `None` if it could not be resolved (→ `ɵɵinvalidFactoryDep`).
    pub token: Option<Expr>,
    /// `Some` for an `@Attribute()` dependency (its literal-name type, used only for typings).
    pub attribute_name_type: Option<Expr>,
    /// `@Host` qualifier.
    pub host: bool,
    /// `@Optional` qualifier.
    pub optional: bool,
    /// `@Self` qualifier. (`self` is reserved in Rust.)
    pub self_: bool,
    /// `@SkipSelf` qualifier.
    pub skip_self: bool,
}

impl Default for R3DependencyMetadata {
    fn default() -> Self {
        R3DependencyMetadata {
            token: None,
            attribute_name_type: None,
            host: false,
            optional: false,
            self_: false,
            skip_self: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Local emit helpers (`tsIgnoreComment`, `typeWithParameters`).
// ---------------------------------------------------------------------------

/// `tsIgnoreComment()` (`render3/util.ts`) — a leading, multiline `@ts-ignore` comment with a
/// trailing newline. It must sit on a *statement* (the newline would break a `return` if placed on
/// an expression — see spec §7).
///
/// NOTE(port): real `tsIgnoreComment` lives in `render3/util.ts` (not yet ported).
fn ts_ignore_comment() -> LeadingComment {
    o::leading_comment("@ts-ignore", true, true)
}

/// `typeWithParameters(expr, numParams)` (`render3/util.ts`). The full helper appends
/// `numParams` `any` type parameters; the port stub passes the base type expression through
/// unchanged (type-argument expansion is `.d.ts`-only and not yet needed for JS emission).
///
/// NOTE(port): real `typeWithParameters` lives in `render3/util.ts` (not yet ported).
fn type_with_parameters(ty: Expr, _num_params: u32) -> Type {
    o::expression_type(ty, None, None)
}

/// Convenience: `o.importExpr(id)` → an `ExternalExpr` for an [`R3`] identifier.
fn import_r3(id: R3) -> Expr {
    o::import_expr(id.reference(), None)
}

/// Convenience: a numeric literal expression (`o.literal(n)`).
fn number_literal(n: f64) -> Expr {
    o::literal(LiteralValue::Number(n), None)
}

/// Convenience: a boolean literal expression (`o.literal(true|false)`).
fn bool_literal(b: bool) -> Expr {
    o::literal(LiteralValue::Bool(b), None)
}

// ---------------------------------------------------------------------------
// compileFactoryFunction.
// ---------------------------------------------------------------------------

/// `compileFactoryFunction(meta)` — construct the factory function expression for the given
/// metadata, plus the `ɵɵFactoryDeclaration<...>` type. See spec §4.
pub fn compile_factory_function(meta: &R3FactoryMetadata) -> R3CompiledExpression {
    let base = meta.base();
    const T_NAME: &str = "__ngFactoryType__";
    let t = o::variable(T_NAME, None);

    // `ɵ${name}_BaseFactory` — set only when there is no own constructor (deps == Inherit).
    let mut base_factory_var: Option<Expr> = None;
    let base_factory_name = format!("\u{0275}{}_BaseFactory", base.name);

    // The type to instantiate via constructor invocation. With no delegated factory this is
    // `t || meta.type.value`; with a delegated factory it is just `t`.
    let type_for_ctor: Expr = if !meta.is_delegated() {
        // `new BinaryOperatorExpr(Or, t, meta.type.value)`.
        Expr::bare(ExprKind::Binary {
            op: BinaryOperator::Or,
            lhs: Box::new(t.clone()),
            rhs: Box::new(base.ty.value.clone()),
        })
    } else {
        t.clone()
    };

    let mut ctor_expr: Option<Expr> = None;

    // `@ts-ignore` on the main factory only when there are real, non-empty deps.
    let factory_comments: Vec<LeadingComment> = match &base.deps {
        FactoryDeps::Deps(deps) if !deps.is_empty() => vec![ts_ignore_comment()],
        _ => Vec::new(),
    };

    match &base.deps {
        // There is a constructor (either explicitly or implicitly defined).
        FactoryDeps::Deps(deps) => {
            ctor_expr = Some(
                type_for_ctor
                    .clone()
                    .instantiate(inject_dependencies(deps, base.target)),
            );
        }
        FactoryDeps::Invalid => {
            // Leave `ctor_expr` as None — handled later as an invalid factory.
        }
        FactoryDeps::Inherit => {
            // No constructor: use the base class' factory to construct `typeForCtor`.
            let bfv = o::variable(base_factory_name.clone(), None);
            ctor_expr = Some(bfv.clone().call_fn(vec![type_for_ctor.clone()], false));
            base_factory_var = Some(bfv);
        }
    }

    let mut body: Vec<Stmt> = Vec::new();
    let ret_expr: Option<Expr>;

    // -- inner closure `makeConditionalFactory(nonCtorExpr)` -> ReadVarExpr ----------------------
    // Builds the `var r = null; if (t) { r = <ctor>; } else { r = <nonCtor>; }` pattern, returning
    // `r`. Implemented as a local fn (Rust has no closures capturing `&mut body` ergonomically here).
    fn make_conditional_factory(
        body: &mut Vec<Stmt>,
        ctor_expr: &Option<Expr>,
        factory_comments: &[LeadingComment],
        t: &Expr,
        non_ctor_expr: Expr,
    ) -> Expr {
        const R_NAME: &str = "__ngConditionalFactory__";
        let r = o::variable(R_NAME, None);

        // `var __ngConditionalFactory__ = null;` (DYNAMIC_TYPE).
        body.push(Stmt::bare(StmtKind::DeclareVar {
            name: R_NAME.to_string(),
            value: Some(o::null_expr()),
            ty: Some(o::dynamic_type()),
        }));

        // then-branch: `r = <ctorExpr>;` (with factory_comments) OR `ɵɵinvalidFactory();`.
        let ctor_stmt = match ctor_expr {
            Some(ce) => {
                let mut s = r.clone().set(ce.clone()).to_stmt();
                s.meta.leading_comments = factory_comments.to_vec();
                s
            }
            None => import_r3(R3::InvalidFactory).call_fn(vec![], false).to_stmt(),
        };

        // else-branch: `r = <nonCtorExpr>;` — always carries a `@ts-ignore`.
        let mut else_stmt = r.clone().set(non_ctor_expr).to_stmt();
        else_stmt.meta.leading_comments = vec![ts_ignore_comment()];

        body.push(o::if_stmt(t.clone(), vec![ctor_stmt], Some(vec![else_stmt])));
        r
    }

    match meta {
        R3FactoryMetadata::Delegated {
            delegate,
            delegate_type,
            delegate_deps,
            ..
        } => {
            // Created with a delegated factory; if no type param is supplied, call the factory.
            let delegate_args = inject_dependencies(delegate_deps, base.target);
            // `new delegate(...)` (Class) or `delegate(...)` (Function).
            let factory_expr = match delegate_type {
                R3FactoryDelegateType::Class => delegate.clone().instantiate(delegate_args),
                R3FactoryDelegateType::Function => delegate.clone().call_fn(delegate_args, false),
            };
            ret_expr = Some(make_conditional_factory(
                &mut body,
                &ctor_expr,
                &factory_comments,
                &t,
                factory_expr,
            ));
        }
        R3FactoryMetadata::Expression { expression, .. } => {
            ret_expr = Some(make_conditional_factory(
                &mut body,
                &ctor_expr,
                &factory_comments,
                &t,
                expression.clone(),
            ));
        }
        R3FactoryMetadata::Constructor(_) => {
            ret_expr = ctor_expr.clone();
        }
    }

    if ret_expr.is_none() {
        // The expression cannot be formed → render an `ɵɵinvalidFactory()` call.
        body.push(import_r3(R3::InvalidFactory).call_fn(vec![], false).to_stmt());
    } else if let Some(bfv) = &base_factory_var {
        // Uses a base factory → memoize via `ɵɵgetInheritedFactory()`:
        // `return (baseFactory || (baseFactory = ɵɵgetInheritedFactory(Type)))(typeForCtor);`
        let get_inherited = import_r3(R3::GetInheritedFactory).call_fn(vec![base.ty.value.clone()], false);
        let base_factory = Expr::bare(ExprKind::Binary {
            op: BinaryOperator::Or,
            lhs: Box::new(bfv.clone()),
            rhs: Box::new(bfv.clone().set(get_inherited)),
        });
        body.push(Stmt::bare(StmtKind::Return(
            base_factory.call_fn(vec![type_for_ctor.clone()], false),
        )));
    } else {
        // Straightforward factory: `return <retExpr>;` with the (possible) factory comments.
        let mut ret_stmt = Stmt::bare(StmtKind::Return(ret_expr.clone().unwrap()));
        ret_stmt.meta.leading_comments = factory_comments.clone();
        body.push(ret_stmt);
    }

    // `function ${name}_Factory(t) { <body> }`.
    let mut factory_fn: Expr = o::fn_(
        vec![FnParam::new(T_NAME, Some(o::dynamic_type()))],
        body,
        Some(o::inferred_type()),
        Some(format!("{}_Factory", base.name)),
    );

    if let Some(bfv) = &base_factory_var {
        // Wrap the base-factory declaration + the factory fn into a pure IIFE:
        // `(() => { let ɵ..._BaseFactory; return function ..._Factory(t){...}; })()`.
        let bfv_name = match &bfv.kind {
            ExprKind::ReadVar { name } => name.clone(),
            _ => base_factory_name.clone(),
        };
        let iife_body = vec![
            Stmt::bare(StmtKind::DeclareVar {
                name: bfv_name,
                value: None,
                ty: Some(o::dynamic_type()),
            }),
            Stmt::bare(StmtKind::Return(factory_fn)),
        ];
        factory_fn = o::arrow_fn(vec![], ArrowBody::Block(iife_body), None)
            // `.callFn([], undefined, /* pure */ true)`.
            .call_fn(vec![], true);
    }

    R3CompiledExpression {
        expression: factory_fn,
        statements: Vec::new(),
        ty: create_factory_type(meta),
    }
}

/// `createFactoryType(meta)` — the `ɵɵFactoryDeclaration<Type, [dep-type-tuple]>` type for `.d.ts`
/// emission. Also called internally by [`compile_factory_function`].
pub fn create_factory_type(meta: &R3FactoryMetadata) -> Type {
    let base = meta.base();
    let ctor_deps_type = match &base.deps {
        FactoryDeps::Deps(deps) => create_ctor_deps_type(deps),
        _ => o::none_type(),
    };
    o::expression_type(
        import_r3_with_params(
            R3::FactoryDeclaration,
            vec![
                type_with_parameters(base.ty.ty.clone(), base.type_argument_count),
                ctor_deps_type,
            ],
        ),
        None,
        None,
    )
}

/// `o.importExpr(id, typeParams)` for an [`R3`] identifier with explicit type parameters.
fn import_r3_with_params(id: R3, type_params: Vec<Type>) -> Expr {
    o::import_expr(id.reference(), Some(type_params))
}

// ---------------------------------------------------------------------------
// Dependency injection expression generation.
// ---------------------------------------------------------------------------

/// `injectDependencies(deps, target)` — compile each dep into its inject call.
fn inject_dependencies(deps: &[R3DependencyMetadata], target: FactoryTarget) -> Vec<Expr> {
    deps.iter()
        .enumerate()
        .map(|(index, dep)| compile_inject_dependency(dep, target, index))
        .collect()
}

/// `compileInjectDependency(dep, target, index)` — compile a single dependency. See spec §4.
fn compile_inject_dependency(
    dep: &R3DependencyMetadata,
    target: FactoryTarget,
    index: usize,
) -> Expr {
    match &dep.token {
        // Unresolvable dep → `ɵɵinvalidFactoryDep(index)`.
        None => import_r3(R3::InvalidFactoryDep).call_fn(vec![number_literal(index as f64)], false),
        Some(token) => {
            if dep.attribute_name_type.is_none() {
                // Build up the injection flags from the metadata.
                let mut flags = InjectFlags::DEFAULT;
                if dep.self_ {
                    flags = flags | InjectFlags::SELF;
                }
                if dep.skip_self {
                    flags = flags | InjectFlags::SKIP_SELF;
                }
                if dep.host {
                    flags = flags | InjectFlags::HOST;
                }
                if dep.optional {
                    flags = flags | InjectFlags::OPTIONAL;
                }
                if target == FactoryTarget::Pipe {
                    flags = flags | InjectFlags::FOR_PIPE;
                }

                // Emit a flags param only for non-default flags (or, defensively, optional).
                let flags_param: Option<Expr> = if flags != InjectFlags::DEFAULT || dep.optional {
                    Some(number_literal(flags.bits() as f64))
                } else {
                    None
                };

                let mut inject_args = vec![token.clone()];
                if let Some(fp) = flags_param {
                    inject_args.push(fp);
                }
                let inject_fn = get_inject_fn(target);
                import_r3(inject_fn).call_fn(inject_args, false)
            } else {
                // `@Attribute()` dep — use the runtime `token` value (attributeNameType is typings
                // only): `ɵɵinjectAttribute(token)`.
                import_r3(R3::InjectAttribute).call_fn(vec![token.clone()], false)
            }
        }
    }
}

/// `getInjectFn(target)` — `ɵɵdirectiveInject` for Component/Directive/Pipe, else `ɵɵinject`
/// (NgModule/Injectable/Service/default).
fn get_inject_fn(target: FactoryTarget) -> R3 {
    match target {
        FactoryTarget::Component | FactoryTarget::Directive | FactoryTarget::Pipe => {
            R3::DirectiveInject
        }
        _ => R3::Inject,
    }
}

// ---------------------------------------------------------------------------
// Constructor-deps type generation (`.d.ts`).
// ---------------------------------------------------------------------------

/// `createCtorDepsType(deps)` — build the dep-type tuple. Returns `expressionType(literalArr(...))`
/// if any dep produced a map, else `NONE_TYPE`.
fn create_ctor_deps_type(deps: &[R3DependencyMetadata]) -> Type {
    let mut has_types = false;
    let attribute_types: Vec<Expr> = deps
        .iter()
        .map(|dep| match create_ctor_dep_type(dep) {
            Some(map) => {
                has_types = true;
                map
            }
            None => o::literal(LiteralValue::Null, None),
        })
        .collect();

    if has_types {
        o::expression_type(o::literal_arr(attribute_types, None), None, None)
    } else {
        o::none_type()
    }
}

/// `createCtorDepType(dep)` — a `LiteralMapExpr` of the set flags, or `None` if no flags. Key order
/// is `attribute, optional, host, self, skipSelf`; keys are unquoted (spec §7).
fn create_ctor_dep_type(dep: &R3DependencyMetadata) -> Option<Expr> {
    let mut entries: Vec<(String, bool, Expr)> = Vec::new();

    if let Some(attr) = &dep.attribute_name_type {
        entries.push(("attribute".to_string(), false, attr.clone()));
    }
    if dep.optional {
        entries.push(("optional".to_string(), false, bool_literal(true)));
    }
    if dep.host {
        entries.push(("host".to_string(), false, bool_literal(true)));
    }
    if dep.self_ {
        entries.push(("self".to_string(), false, bool_literal(true)));
    }
    if dep.skip_self {
        entries.push(("skipSelf".to_string(), false, bool_literal(true)));
    }

    if entries.is_empty() {
        None
    } else {
        Some(o::literal_map(entries, None))
    }
}

// ---------------------------------------------------------------------------
// Tests — assert the structure of the produced output_ast.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn external_ref(name: &str) -> Expr {
        // A stand-in "class reference" expression (the type being constructed).
        o::variable(name, None)
    }

    fn ctor_meta(name: &str, deps: FactoryDeps, target: FactoryTarget) -> R3FactoryMetadata {
        R3FactoryMetadata::Constructor(R3ConstructorFactoryMetadata {
            name: name.to_string(),
            ty: R3Reference {
                value: external_ref(name),
                ty: external_ref(name),
            },
            type_argument_count: 0,
            deps,
            target,
        })
    }

    fn dep(token: &str) -> R3DependencyMetadata {
        R3DependencyMetadata {
            token: Some(o::variable(token, None)),
            ..Default::default()
        }
    }

    /// Pull `(name, params, statements)` out of a FunctionExpr.
    fn as_function(e: &Expr) -> (&Option<String>, &Vec<FnParam>, &Vec<Stmt>) {
        match &e.kind {
            ExprKind::Function {
                name,
                params,
                statements,
            } => (name, params, statements),
            other => panic!("expected FunctionExpr, got {other:?}"),
        }
    }

    #[test]
    fn two_dep_directive_factory_structure() {
        let meta = ctor_meta(
            "FooBar",
            FactoryDeps::Deps(vec![dep("Dep1"), dep("Dep2")]),
            FactoryTarget::Directive,
        );
        let compiled = compile_factory_function(&meta);

        // The expression is `function FooBar_Factory(t) { ... }`.
        let (name, params, body) = as_function(&compiled.expression);
        assert_eq!(name.as_deref(), Some("FooBar_Factory"));
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].name, "__ngFactoryType__");

        // Body is a single `return new (t || FooBar)(i0.ɵɵdirectiveInject(Dep1), ...);`.
        assert_eq!(body.len(), 1);
        let ret = match &body[0].kind {
            StmtKind::Return(e) => e,
            other => panic!("expected ReturnStatement, got {other:?}"),
        };
        // The return statement carries a `@ts-ignore` because there are real deps.
        assert_eq!(body[0].meta.leading_comments.len(), 1);

        // `new (...)(args)`.
        let (class_expr, args) = match &ret.kind {
            ExprKind::New { class_expr, args } => (class_expr, args),
            other => panic!("expected InstantiateExpr, got {other:?}"),
        };
        // class_expr is `t || FooBar`.
        match &class_expr.kind {
            ExprKind::Binary {
                op: BinaryOperator::Or,
                lhs,
                rhs,
            } => {
                assert!(matches!(&lhs.kind, ExprKind::ReadVar { name } if name == "__ngFactoryType__"));
                assert!(matches!(&rhs.kind, ExprKind::ReadVar { name } if name == "FooBar"));
            }
            other => panic!("expected `t || FooBar`, got {other:?}"),
        }

        // Two args, each `i0.ɵɵdirectiveInject(DepN)`.
        assert_eq!(args.len(), 2);
        for (i, arg) in args.iter().enumerate() {
            let (callee, call_args) = match &arg.kind {
                ExprKind::Invoke { callee, args, .. } => (callee, args),
                other => panic!("expected inject call, got {other:?}"),
            };
            match &callee.kind {
                ExprKind::External { value, .. } => {
                    assert_eq!(value.name, "ɵɵdirectiveInject");
                }
                other => panic!("expected ɵɵdirectiveInject external, got {other:?}"),
            }
            assert_eq!(call_args.len(), 1);
            let expected = format!("Dep{}", i + 1);
            assert!(matches!(&call_args[0].kind, ExprKind::ReadVar { name } if *name == expected));
        }
    }

    #[test]
    fn no_dep_injectable_uses_inject_and_no_ts_ignore() {
        let meta = ctor_meta("Svc", FactoryDeps::Deps(vec![]), FactoryTarget::Injectable);
        let compiled = compile_factory_function(&meta);
        let (_name, _params, body) = as_function(&compiled.expression);
        // No deps → no `@ts-ignore` on the return.
        assert_eq!(body.len(), 1);
        assert!(body[0].meta.leading_comments.is_empty());
        let ret = match &body[0].kind {
            StmtKind::Return(e) => e,
            other => panic!("expected return, got {other:?}"),
        };
        let args = match &ret.kind {
            ExprKind::New { args, .. } => args,
            other => panic!("expected new, got {other:?}"),
        };
        assert!(args.is_empty());
    }

    #[test]
    fn injectable_dep_uses_inject_not_directive_inject() {
        let meta = ctor_meta(
            "Svc",
            FactoryDeps::Deps(vec![dep("Dep1")]),
            FactoryTarget::Injectable,
        );
        let compiled = compile_factory_function(&meta);
        let (_n, _p, body) = as_function(&compiled.expression);
        let ret = match &body[0].kind {
            StmtKind::Return(e) => e,
            _ => unreachable!(),
        };
        let args = match &ret.kind {
            ExprKind::New { args, .. } => args,
            _ => unreachable!(),
        };
        let callee = match &args[0].kind {
            ExprKind::Invoke { callee, .. } => callee,
            _ => unreachable!(),
        };
        match &callee.kind {
            ExprKind::External { value, .. } => assert_eq!(value.name, "ɵɵinject"),
            other => panic!("expected ɵɵinject, got {other:?}"),
        }
    }

    #[test]
    fn invalid_deps_emit_invalid_factory() {
        let meta = ctor_meta("Bad", FactoryDeps::Invalid, FactoryTarget::Component);
        let compiled = compile_factory_function(&meta);
        let (_n, _p, body) = as_function(&compiled.expression);
        // Single expression statement: `ɵɵinvalidFactory();`.
        assert_eq!(body.len(), 1);
        let expr = match &body[0].kind {
            StmtKind::Expression(e) => e,
            other => panic!("expected expression statement, got {other:?}"),
        };
        let callee = match &expr.kind {
            ExprKind::Invoke { callee, .. } => callee,
            other => panic!("expected invoke, got {other:?}"),
        };
        assert!(matches!(&callee.kind, ExprKind::External { value, .. } if value.name == "ɵɵinvalidFactory"));
    }

    #[test]
    fn invalid_dep_token_emits_invalid_factory_dep() {
        let bad_dep = R3DependencyMetadata {
            token: None,
            ..Default::default()
        };
        let meta = ctor_meta(
            "C",
            FactoryDeps::Deps(vec![bad_dep]),
            FactoryTarget::Component,
        );
        let compiled = compile_factory_function(&meta);
        let (_n, _p, body) = as_function(&compiled.expression);
        let ret = match &body[0].kind {
            StmtKind::Return(e) => e,
            _ => unreachable!(),
        };
        let args = match &ret.kind {
            ExprKind::New { args, .. } => args,
            _ => unreachable!(),
        };
        let (callee, call_args) = match &args[0].kind {
            ExprKind::Invoke { callee, args, .. } => (callee, args),
            _ => unreachable!(),
        };
        assert!(matches!(&callee.kind, ExprKind::External { value, .. } if value.name == "ɵɵinvalidFactoryDep"));
        // arg is the index literal 0.
        assert!(matches!(&call_args[0].kind, ExprKind::Literal(LiteralValue::Number(n)) if *n == 0.0));
    }

    #[test]
    fn optional_dep_emits_flags_param() {
        let mut d = dep("Dep1");
        d.optional = true;
        let meta = ctor_meta("C", FactoryDeps::Deps(vec![d]), FactoryTarget::Injectable);
        let compiled = compile_factory_function(&meta);
        let (_n, _p, body) = as_function(&compiled.expression);
        let ret = match &body[0].kind {
            StmtKind::Return(e) => e,
            _ => unreachable!(),
        };
        let args = match &ret.kind {
            ExprKind::New { args, .. } => args,
            _ => unreachable!(),
        };
        let call_args = match &args[0].kind {
            ExprKind::Invoke { args, .. } => args,
            _ => unreachable!(),
        };
        // [token, flags] where flags = OPTIONAL = 8.
        assert_eq!(call_args.len(), 2);
        assert!(matches!(&call_args[1].kind, ExprKind::Literal(LiteralValue::Number(n)) if *n == 8.0));
    }

    #[test]
    fn inherited_factory_wraps_in_pure_iife() {
        let meta = ctor_meta("Sub", FactoryDeps::Inherit, FactoryTarget::Directive);
        let compiled = compile_factory_function(&meta);
        // The whole thing is a pure invoke of an arrow function.
        let (callee, _args, pure) = match &compiled.expression.kind {
            ExprKind::Invoke { callee, args, pure, .. } => (callee, args, *pure),
            other => panic!("expected IIFE invoke, got {other:?}"),
        };
        assert!(pure, "IIFE must be marked pure");
        // callee is an arrow fn whose body declares the base factory var + returns the function.
        let iife_body = match &callee.kind {
            ExprKind::Arrow {
                body: ArrowBody::Block(stmts),
                ..
            } => stmts,
            other => panic!("expected arrow fn, got {other:?}"),
        };
        assert_eq!(iife_body.len(), 2);
        match &iife_body[0].kind {
            StmtKind::DeclareVar { name, value, .. } => {
                assert_eq!(name, "\u{0275}Sub_BaseFactory");
                assert!(value.is_none());
            }
            other => panic!("expected base factory var decl, got {other:?}"),
        }
        // The returned function's body returns `(baseFactory || (baseFactory = ...))(t || Sub)`.
        let inner_fn = match &iife_body[1].kind {
            StmtKind::Return(e) => e,
            other => panic!("expected return of factory fn, got {other:?}"),
        };
        let (_n, _p, fbody) = as_function(inner_fn);
        let ret = match &fbody[0].kind {
            StmtKind::Return(e) => e,
            other => panic!("expected return, got {other:?}"),
        };
        // ret is an invoke of `(baseFactory || (baseFactory = ɵɵgetInheritedFactory(Sub)))`.
        let callee2 = match &ret.kind {
            ExprKind::Invoke { callee, .. } => callee,
            other => panic!("expected invoke, got {other:?}"),
        };
        assert!(matches!(
            &callee2.kind,
            ExprKind::Binary { op: BinaryOperator::Or, .. }
        ));
    }

    #[test]
    fn delegated_class_factory_uses_conditional_pattern() {
        let meta = R3FactoryMetadata::Delegated {
            base: R3ConstructorFactoryMetadata {
                name: "D".to_string(),
                ty: R3Reference {
                    value: external_ref("D"),
                    ty: external_ref("D"),
                },
                type_argument_count: 0,
                deps: FactoryDeps::Deps(vec![dep("Dep1")]),
                target: FactoryTarget::Injectable,
            },
            delegate: external_ref("Other"),
            delegate_type: R3FactoryDelegateType::Class,
            delegate_deps: vec![dep("Dep1")],
        };
        let compiled = compile_factory_function(&meta);
        let (_n, _p, body) = as_function(&compiled.expression);
        // var __ngConditionalFactory__ = null; if (t) {...} else {...} return __ngConditionalFactory__;
        assert_eq!(body.len(), 3);
        assert!(matches!(
            &body[0].kind,
            StmtKind::DeclareVar { name, .. } if name == "__ngConditionalFactory__"
        ));
        let (then_b, else_b) = match &body[1].kind {
            StmtKind::If {
                true_case,
                false_case,
                ..
            } => (true_case, false_case),
            other => panic!("expected if, got {other:?}"),
        };
        assert_eq!(then_b.len(), 1);
        assert_eq!(else_b.len(), 1);
        // else branch always carries a @ts-ignore.
        assert_eq!(else_b[0].meta.leading_comments.len(), 1);
        // final return of the conditional var.
        assert!(matches!(
            &body[2].kind,
            StmtKind::Return(e) if matches!(&e.kind, ExprKind::ReadVar { name } if name == "__ngConditionalFactory__")
        ));
    }
}
