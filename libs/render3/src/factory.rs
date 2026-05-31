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
use crate::util::{
    ts_ignore_comment, type_with_parameters, R3CompiledExpression, R3Reference,
};

// `FactoryTarget` / `InjectFlags` are the shared render3 "core" types; they live in `crate::util`
// and are re-exported here for the many callers that reach for `crate::factory::FactoryTarget`.
pub use crate::util::{FactoryTarget, InjectFlags};

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
// Local emit helpers.
// ---------------------------------------------------------------------------

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
// @Injectable / @Service compile path.
//
// PORT TARGETS:
//   * `injectable_compiler_2.ts` → `compileInjectable` (`ɵprov = ɵɵdefineInjectable({...})`).
//   * `service_compiler.ts`       → `compileService`    (`ɵprov = ɵɵdefineService({...})`).
//
// Both reuse [`compile_factory_function`] above (the `ɵfac`) and emit a *provider definition*
// (`ɵprov`) that points at the type's factory — either `Type.ɵfac` (the default / `useClass:Self`),
// a delegated factory (`useClass`/`useFactory` WITH `deps`), a direct provider expression
// (`useValue`/`useExisting`), or an arrow that forwards to an alternate type's `.ɵfac`
// (`useClass`/`useFactory` WITHOUT `deps`, incl. `forwardRef`). The provider-def is a pure call so
// it tree-shakes when unused.
// ---------------------------------------------------------------------------

/// `ForwardRefHandling` (`render3/util.ts`) — whether a `MaybeForwardRefExpression` is still wrapped
/// in a `forwardRef(() => …)` call and, if not, whether the wrap must be re-introduced on emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForwardRefHandling {
    /// Never wrapped in `forwardRef()` — the expression is safe to use as-is.
    None,
    /// Still wrapped in a `forwardRef()` call (use as-is).
    Wrapped,
    /// Was unwrapped from a `forwardRef()` — must be re-wrapped as `forwardRef(() => expr)` on emit.
    Unwrapped,
}

/// `MaybeForwardRefExpression` (`render3/util.ts`) — an expression that may reference a not-yet
/// defined type, tracking whether it was/needs to be wrapped in `forwardRef()`.
#[derive(Debug, Clone, PartialEq)]
pub struct MaybeForwardRef {
    /// The unwrapped expression.
    pub expression: Expr,
    /// How the `forwardRef()` wrap is/was handled.
    pub forward_ref: ForwardRefHandling,
}

impl MaybeForwardRef {
    /// `createMayBeForwardRefExpression(expression, ForwardRefHandling.None)` — the common case for
    /// a value that was never a forward ref.
    pub fn none(expression: Expr) -> MaybeForwardRef {
        MaybeForwardRef {
            expression,
            forward_ref: ForwardRefHandling::None,
        }
    }
}

/// `generateForwardRef(expr)` → `forwardRef(() => expr)`.
fn generate_forward_ref(expr: Expr) -> Expr {
    import_r3(R3::ForwardRef).call_fn(
        vec![o::arrow_fn(vec![], ArrowBody::Expr(Box::new(expr)), None)],
        false,
    )
}

/// `convertFromMaybeForwardRefExpression(meta)` — re-wrap an `Unwrapped` forward ref as
/// `forwardRef(() => expr)`; `None`/`Wrapped` pass the expression through unchanged.
fn convert_from_maybe_forward_ref(meta: &MaybeForwardRef) -> Expr {
    match meta.forward_ref {
        ForwardRefHandling::None | ForwardRefHandling::Wrapped => meta.expression.clone(),
        ForwardRefHandling::Unwrapped => generate_forward_ref(meta.expression.clone()),
    }
}

/// `R3InjectableMetadata` (`injectable_compiler_2.ts`). `providedIn` is always present (a `null`
/// literal expression when absent — its presence guards whether the `providedIn` key is emitted).
#[derive(Debug, Clone, PartialEq)]
pub struct R3InjectableMetadata {
    /// String name of the injectable type (names the factory function).
    pub name: String,
    /// The type being provided (`token` + `.d.ts` type).
    pub ty: R3Reference,
    /// Number of type arguments for the `.d.ts` declaration.
    pub type_argument_count: u32,
    /// `providedIn` — a `MaybeForwardRef`; a `null`-literal expression means "no providedIn".
    pub provided_in: MaybeForwardRef,
    /// `useClass` delegate (mutually exclusive with the other `use*`).
    pub use_class: Option<MaybeForwardRef>,
    /// `useFactory` function expression.
    pub use_factory: Option<Expr>,
    /// `useExisting` token.
    pub use_existing: Option<MaybeForwardRef>,
    /// `useValue` value.
    pub use_value: Option<MaybeForwardRef>,
    /// Explicit `deps: [...]` (only meaningful with `useClass`/`useFactory`). `None` ⇒ not given.
    pub deps: Option<Vec<R3DependencyMetadata>>,
}

/// Build the shared `R3FactoryMetadata::Constructor` base (`deps: []`, target Injectable) used as
/// the seed for every injectable provider variant (`{...factoryMeta, …}` spreads in the TS).
fn injectable_factory_base(meta: &R3InjectableMetadata) -> R3ConstructorFactoryMetadata {
    R3ConstructorFactoryMetadata {
        name: meta.name.clone(),
        ty: meta.ty.clone(),
        type_argument_count: meta.type_argument_count,
        deps: FactoryDeps::Deps(Vec::new()),
        target: FactoryTarget::Injectable,
    }
}

/// `createFactoryFunction(type)` (`injectable_compiler_2.ts`) →
/// `__ngFactoryType__ => type.ɵfac(__ngFactoryType__)`.
fn create_delegate_factory_function(ty: Expr) -> Expr {
    const T_NAME: &str = "__ngFactoryType__";
    let body = ty
        .prop("\u{0275}fac")
        .call_fn(vec![o::variable(T_NAME, None)], false);
    o::arrow_fn(
        vec![FnParam::new(T_NAME, Some(o::dynamic_type()))],
        ArrowBody::Expr(Box::new(body)),
        None,
    )
}

/// `delegateToFactory(type, useType, unwrapForwardRefs)` (`injectable_compiler_2.ts`). When `type`
/// and `useType` denote the same symbol the provider delegates straight to `useType.ɵfac`; otherwise
/// it forwards through an arrow (optionally resolving a `forwardRef` first).
fn delegate_to_factory(ty: &Expr, use_ty: &Expr, unwrap_forward_refs: bool) -> Expr {
    if ty.is_equivalent(use_ty) {
        // `factory: type.ɵfac`.
        return use_ty.clone().prop("\u{0275}fac");
    }
    if !unwrap_forward_refs {
        // `factory: __ngFactoryType__ => useType.ɵfac(__ngFactoryType__)`.
        return create_delegate_factory_function(use_ty.clone());
    }
    // `factory: __ngFactoryType__ => resolveForwardRef(useType).ɵfac(__ngFactoryType__)`.
    let unwrapped = import_r3(R3::ResolveForwardRef).call_fn(vec![use_ty.clone()], false);
    create_delegate_factory_function(unwrapped)
}

/// `compileInjectable(meta, resolveForwardRefs)` — the `ɵprov = ɵɵdefineInjectable({...})` provider
/// definition. The matching `ɵfac` is produced separately by [`compile_factory_function`].
pub fn compile_injectable(
    meta: &R3InjectableMetadata,
    resolve_forward_refs: bool,
) -> R3CompiledExpression {
    let base = injectable_factory_base(meta);

    // Resolve the provider `factory` expression by provider kind, mirroring the TS branch order
    // (useClass → useFactory → useValue → useExisting → default).
    let (factory_expr, statements): (Expr, Vec<Stmt>) = if let Some(use_class) = &meta.use_class {
        let use_class_on_self = use_class.expression.is_equivalent(&meta.ty.value);
        match &meta.deps {
            // `deps` present → `new useClass(...deps)` delegated factory.
            Some(deps) => {
                let m = R3FactoryMetadata::Delegated {
                    base: base.clone(),
                    delegate: use_class.expression.clone(),
                    delegate_type: R3FactoryDelegateType::Class,
                    delegate_deps: deps.clone(),
                };
                let c = compile_factory_function(&m);
                (c.expression, c.statements)
            }
            // `useClass: Self` with no deps → ignore `useClass`, use the plain constructor factory.
            None if use_class_on_self => {
                let m = R3FactoryMetadata::Constructor(base.clone());
                let c = compile_factory_function(&m);
                (c.expression, c.statements)
            }
            // `useClass: Other` with no deps → forward to `Other.ɵfac`.
            None => (
                delegate_to_factory(&meta.ty.value, &use_class.expression, resolve_forward_refs),
                Vec::new(),
            ),
        }
    } else if let Some(use_factory) = &meta.use_factory {
        match &meta.deps {
            // `deps` present → call the user factory with the injected deps.
            Some(deps) => {
                let m = R3FactoryMetadata::Delegated {
                    base: base.clone(),
                    delegate: use_factory.clone(),
                    delegate_type: R3FactoryDelegateType::Function,
                    delegate_deps: deps.clone(),
                };
                let c = compile_factory_function(&m);
                (c.expression, c.statements)
            }
            // No `deps` → `() => useFactory()`.
            None => (
                o::arrow_fn(
                    vec![],
                    ArrowBody::Expr(Box::new(use_factory.clone().call_fn(vec![], false))),
                    None,
                ),
                Vec::new(),
            ),
        }
    } else if let Some(use_value) = &meta.use_value {
        let m = R3FactoryMetadata::Expression {
            base: base.clone(),
            expression: use_value.expression.clone(),
        };
        let c = compile_factory_function(&m);
        (c.expression, c.statements)
    } else if let Some(use_existing) = &meta.use_existing {
        // `useExisting` → `inject(token)` provider expression.
        let m = R3FactoryMetadata::Expression {
            base: base.clone(),
            expression: import_r3(R3::Inject).call_fn(vec![use_existing.expression.clone()], false),
        };
        let c = compile_factory_function(&m);
        (c.expression, c.statements)
    } else {
        // Default: delegate to the type's own factory.
        (
            delegate_to_factory(&meta.ty.value, &meta.ty.value, resolve_forward_refs),
            Vec::new(),
        )
    };

    let mut entries: Vec<(String, bool, Expr)> = Vec::new();
    entries.push(("token".to_string(), false, meta.ty.value.clone()));
    entries.push(("factory".to_string(), false, factory_expr));

    // `providedIn` is emitted only when its expression is a non-null value.
    if !is_null_literal(&meta.provided_in.expression) {
        entries.push((
            "providedIn".to_string(),
            false,
            convert_from_maybe_forward_ref(&meta.provided_in),
        ));
    }

    let expression = import_r3(R3::DefineInjectableField)
        .call_fn(vec![o::literal_map(entries, None)], /* pure */ true);

    R3CompiledExpression {
        expression,
        ty: create_injectable_type(&meta.ty.ty, meta.type_argument_count),
        statements,
    }
}

/// `R3ServiceMetadata` (`service_compiler.ts`).
#[derive(Debug, Clone, PartialEq)]
pub struct R3ServiceMetadata {
    /// String name of the service type.
    pub name: String,
    /// The type being provided (`token` + `.d.ts` type).
    pub ty: R3Reference,
    /// Number of type arguments for the `.d.ts` declaration.
    pub type_argument_count: u32,
    /// `autoProvided` — `Some(false)` emits `autoProvided: false`; `None`/`Some(true)` omit it.
    pub auto_provided: Option<bool>,
    /// `factory: () => …` override (the `@Service({factory})` form).
    pub factory: Option<Expr>,
}

/// `compileService(meta, resolveForwardRefs)` — the `ɵprov = ɵɵdefineService({...})` provider
/// definition for an `@Service` class. The matching `ɵfac` is produced separately by
/// [`compile_factory_function`].
pub fn compile_service(meta: &R3ServiceMetadata, resolve_forward_refs: bool) -> R3CompiledExpression {
    let factory_expr = match &meta.factory {
        // `factory: () => userFactory()`.
        Some(f) => o::arrow_fn(
            vec![],
            ArrowBody::Expr(Box::new(f.clone().call_fn(vec![], false))),
            None,
        ),
        // No factory override → delegate to the type's own `ɵfac`.
        None => delegate_to_factory(&meta.ty.value, &meta.ty.value, resolve_forward_refs),
    };

    let mut entries: Vec<(String, bool, Expr)> = Vec::new();
    entries.push(("token".to_string(), false, meta.ty.value.clone()));
    entries.push(("factory".to_string(), false, factory_expr));

    // `autoProvided: false` is emitted only on a strict `=== false`.
    if meta.auto_provided == Some(false) {
        entries.push(("autoProvided".to_string(), false, bool_literal(false)));
    }

    let expression = import_r3(R3::DefineService)
        .call_fn(vec![o::literal_map(entries, None)], /* pure */ true);

    R3CompiledExpression {
        expression,
        ty: create_injectable_type(&meta.ty.ty, meta.type_argument_count),
        statements: Vec::new(),
    }
}

/// `createInjectableType(type, typeArgumentCount)` (`injectable_compiler_2.ts`) →
/// `ɵɵInjectableDeclaration<Type>` (shared by `@Injectable` and `@Service` `.d.ts` emit).
pub fn create_injectable_type(ty: &Expr, type_argument_count: u32) -> Type {
    o::expression_type(
        import_r3_with_params(
            R3::InjectableDeclaration,
            vec![type_with_parameters(ty.clone(), type_argument_count)],
        ),
        None,
        None,
    )
}

/// Is `expr` the `null` literal? (Guards `providedIn` emission.)
fn is_null_literal(expr: &Expr) -> bool {
    matches!(&expr.kind, ExprKind::Literal(LiteralValue::Null))
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

    // -----------------------------------------------------------------------
    // @Injectable / @Service emit tests.
    //
    // These canonicalise the EMITTED `ɵprov`/`ɵfac` JS the same way the compliance harness
    // (`run-compliance.mjs::canonicalize`) does — strip imports/comments, unify the Ivy ref
    // prefix (`i0.`/`$r3$.` → ``), normalise quotes, collapse whitespace — and assert the
    // load-bearing slice of Angular's golden appears verbatim.
    // -----------------------------------------------------------------------

    use crate::output::emitter::emit_expression;

    /// Mirror of the harness `canonicalize`, restricted to the transforms the DI goldens exercise.
    fn canon(code: &str) -> String {
        // Strip import lines.
        let mut s = String::new();
        for line in code.lines() {
            if line.trim_start().starts_with("import ") {
                continue;
            }
            s.push_str(line);
            s.push('\n');
        }
        // Strip block comments (`/* … */`, incl. `/*@__PURE__*/` and `/* @ts-ignore */`).
        let chars: Vec<char> = s.chars().collect();
        let mut out = String::new();
        let mut i = 0;
        while i < chars.len() {
            if i + 1 < chars.len() && chars[i] == '/' && chars[i + 1] == '*' {
                i += 2;
                while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
                continue;
            }
            out.push(chars[i]);
            i += 1;
        }
        // Normalise single→double quotes, drop the `$r3$.`/`$i0$.` Ivy ref prefix.
        out = out.replace('\'', "\"");
        out = out.replace("$r3$.", "").replace("$i0$.", "");
        // Strip a leading `iN.` namespace prefix on Ivy symbols.
        let cs: Vec<char> = out.chars().collect();
        let mut cleaned = String::new();
        let mut j = 0;
        while j < cs.len() {
            if cs[j] == 'i' && (j == 0 || (!cs[j - 1].is_alphanumeric() && cs[j - 1] != '_')) {
                let mut k = j + 1;
                while k < cs.len() && cs[k].is_ascii_digit() {
                    k += 1;
                }
                if k > j + 1 && k < cs.len() && cs[k] == '.' {
                    j = k + 1;
                    continue;
                }
            }
            cleaned.push(cs[j]);
            j += 1;
        }
        let no_ws: String = cleaned.chars().filter(|c| !c.is_whitespace()).collect();
        // Drop the trailing expression-statement terminator (the harness does `;+$ -> ''`).
        no_ws.trim_end_matches(';').to_string()
    }

    fn ref_(name: &str) -> R3Reference {
        R3Reference {
            value: o::variable(name, None),
            ty: o::variable(name, None),
        }
    }

    fn injectable_meta(name: &str) -> R3InjectableMetadata {
        R3InjectableMetadata {
            name: name.to_string(),
            ty: ref_(name),
            type_argument_count: 0,
            provided_in: MaybeForwardRef::none(o::null_expr()),
            use_class: None,
            use_factory: None,
            use_existing: None,
            use_value: None,
            deps: None,
        }
    }

    fn inject_dep(token: &str) -> R3DependencyMetadata {
        R3DependencyMetadata {
            token: Some(o::variable(token, None)),
            ..Default::default()
        }
    }

    #[test]
    fn injectable_factory_prov_default_delegates_to_self_fac() {
        // injectable_factory.ts → injectable_factory_prov.js:
        //   ɵɵdefineInjectable({ token: MyService, factory: MyService.ɵfac })
        let meta = injectable_meta("MyService");
        let compiled = compile_injectable(&meta, false);
        let got = canon(&emit_expression(&compiled.expression));
        assert_eq!(
            got,
            canon("$r3$.ɵɵdefineInjectable({ token: MyService, factory: MyService.ɵfac })"),
            "got: {got}"
        );
        assert!(compiled.statements.is_empty());
    }

    #[test]
    fn injectable_factory_fac_uses_inject_for_ctor_dep() {
        // injectable_factory_fac.js — the SEPARATE ɵfac with the constructor dep, target Injectable
        // (so `ɵɵinject`, not `ɵɵdirectiveInject`), carrying the `@ts-ignore`.
        let meta = R3FactoryMetadata::Constructor(R3ConstructorFactoryMetadata {
            name: "MyService".to_string(),
            ty: ref_("MyService"),
            type_argument_count: 0,
            deps: FactoryDeps::Deps(vec![inject_dep("MyDependency")]),
            target: FactoryTarget::Injectable,
        });
        let fac = compile_factory_function(&meta);
        let got = canon(&emit_expression(&fac.expression));
        assert!(
            got.contains(&canon(
                "function MyService_Factory(__ngFactoryType__){ return new (__ngFactoryType__ || MyService)($r3$.ɵɵinject(MyDependency)); }"
            )),
            "got: {got}"
        );
    }

    #[test]
    fn injectable_useclass_with_deps_emits_delegated_class_factory() {
        // useclass_with_deps.ts → providedIn:'root', useClass:MyAlternateService, deps:[SomeDep].
        let mut meta = injectable_meta("MyService");
        meta.provided_in =
            MaybeForwardRef::none(o::literal(LiteralValue::String("root".to_string()), None));
        meta.use_class = Some(MaybeForwardRef::none(o::variable("MyAlternateService", None)));
        meta.deps = Some(vec![inject_dep("SomeDep")]);
        let compiled = compile_injectable(&meta, false);
        let got = canon(&emit_expression(&compiled.expression));
        assert!(got.contains(&canon("ɵɵdefineInjectable({token:MyService,factory:")), "got: {got}");
        assert!(got.contains(&canon("__ngConditionalFactory__=new__ngFactoryType__()")), "got: {got}");
        assert!(
            got.contains(&canon("__ngConditionalFactory__=newMyAlternateService(ɵɵinject(SomeDep))")),
            "got: {got}"
        );
        assert!(got.contains(&canon("providedIn:\"root\"")), "got: {got}");
    }

    #[test]
    fn injectable_usefactory_with_deps_emits_function_delegate() {
        // usefactory_with_deps.ts → factory function delegate, optional dep gets flags 8.
        let mut meta = injectable_meta("MyService");
        meta.provided_in =
            MaybeForwardRef::none(o::literal(LiteralValue::String("root".to_string()), None));
        let factory = o::arrow_fn(
            vec![FnParam::new("dep", None), FnParam::new("optional", None)],
            ArrowBody::Expr(Box::new(
                o::variable("MyAlternateService", None)
                    .instantiate(vec![o::variable("dep", None), o::variable("optional", None)]),
            )),
            None,
        );
        meta.use_factory = Some(factory);
        let optional = R3DependencyMetadata {
            token: Some(o::variable("SomeDep", None)),
            optional: true,
            ..Default::default()
        };
        meta.deps = Some(vec![inject_dep("SomeDep"), optional]);
        let compiled = compile_injectable(&meta, false);
        let got = canon(&emit_expression(&compiled.expression));
        assert!(
            got.contains(&canon(
                "((dep, optional) => new MyAlternateService(dep, optional))($r3$.ɵɵinject(SomeDep), $r3$.ɵɵinject(SomeDep, 8))"
            )),
            "got: {got}"
        );
        assert!(got.contains(&canon("providedIn:\"root\"")), "got: {got}");
    }

    #[test]
    fn injectable_useclass_without_deps_forwards_to_alternate_fac() {
        // useclass_without_deps.js → factory: __ngFactoryType__ => MyAlternateService.ɵfac(__ngFactoryType__)
        let mut meta = injectable_meta("MyService");
        meta.provided_in =
            MaybeForwardRef::none(o::literal(LiteralValue::String("root".to_string()), None));
        meta.use_class = Some(MaybeForwardRef::none(o::variable("MyAlternateService", None)));
        let compiled = compile_injectable(&meta, false);
        let got = canon(&emit_expression(&compiled.expression));
        assert!(
            got.contains(&canon(
                "factory: __ngFactoryType__ => MyAlternateService.ɵfac(__ngFactoryType__)"
            )),
            "got: {got}"
        );
    }

    #[test]
    fn injectable_providedin_forwardref_rewraps() {
        // providedin_forwardref.js → providedIn: $i0$.forwardRef(() => Mod), factory: Service.ɵfac
        let mut meta = injectable_meta("Service");
        meta.provided_in = MaybeForwardRef {
            expression: o::variable("Mod", None),
            forward_ref: ForwardRefHandling::Unwrapped,
        };
        let compiled = compile_injectable(&meta, false);
        let got = canon(&emit_expression(&compiled.expression));
        assert!(got.contains(&canon("token: Service, factory: Service.ɵfac")), "got: {got}");
        assert!(
            got.contains(&canon("providedIn: $i0$.forwardRef(() => Mod)")),
            "got: {got}"
        );
    }

    #[test]
    fn injectable_usevalue_emits_expression_factory() {
        let mut meta = injectable_meta("MyService");
        meta.use_value = Some(MaybeForwardRef::none(o::literal(
            LiteralValue::String("v".to_string()),
            None,
        )));
        let compiled = compile_injectable(&meta, false);
        let got = canon(&emit_expression(&compiled.expression));
        assert!(got.contains(&canon("__ngConditionalFactory__ = \"v\"")), "got: {got}");
    }

    #[test]
    fn injectable_useexisting_injects_token() {
        let mut meta = injectable_meta("MyService");
        meta.use_existing = Some(MaybeForwardRef::none(o::variable("Other", None)));
        let compiled = compile_injectable(&meta, false);
        let got = canon(&emit_expression(&compiled.expression));
        assert!(got.contains(&canon("__ngConditionalFactory__ = $r3$.ɵɵinject(Other)")), "got: {got}");
    }

    #[test]
    fn service_basic_defines_service_delegating_to_self_fac() {
        // basic_service.ts → ɵɵdefineService({ token: MyService, factory: MyService.ɵfac })
        let meta = R3ServiceMetadata {
            name: "MyService".to_string(),
            ty: ref_("MyService"),
            type_argument_count: 0,
            auto_provided: None,
            factory: None,
        };
        let compiled = compile_service(&meta, false);
        let got = canon(&emit_expression(&compiled.expression));
        assert_eq!(
            got,
            canon("$r3$.ɵɵdefineService({ token: MyService, factory: MyService.ɵfac })"),
            "got: {got}"
        );
    }

    #[test]
    fn service_autoprovided_false_emits_key() {
        // not_provided_service.ts → autoProvided: false.
        let meta = R3ServiceMetadata {
            name: "MyService".to_string(),
            ty: ref_("MyService"),
            type_argument_count: 0,
            auto_provided: Some(false),
            factory: None,
        };
        let compiled = compile_service(&meta, false);
        let got = canon(&emit_expression(&compiled.expression));
        assert_eq!(
            got,
            canon(
                "$r3$.ɵɵdefineService({ token: MyService, factory: MyService.ɵfac, autoProvided: false })"
            ),
            "got: {got}"
        );
    }

    #[test]
    fn service_autoprovided_true_omits_key() {
        // explicitly_provided_service.ts (autoProvided:true) → key OMITTED.
        let meta = R3ServiceMetadata {
            name: "MyService".to_string(),
            ty: ref_("MyService"),
            type_argument_count: 0,
            auto_provided: Some(true),
            factory: None,
        };
        let compiled = compile_service(&meta, false);
        let got = canon(&emit_expression(&compiled.expression));
        assert!(!got.contains("autoProvided"), "autoProvided must be omitted; got: {got}");
    }

    #[test]
    fn service_with_factory_wraps_user_factory() {
        // service_with_factory.ts → factory: () => (() => new Alternate())()
        let user_factory = o::arrow_fn(
            vec![],
            ArrowBody::Expr(Box::new(o::variable("Alternate", None).instantiate(vec![]))),
            None,
        );
        let meta = R3ServiceMetadata {
            name: "MyService".to_string(),
            ty: ref_("MyService"),
            type_argument_count: 0,
            auto_provided: None,
            factory: Some(user_factory),
        };
        let compiled = compile_service(&meta, false);
        let got = canon(&emit_expression(&compiled.expression));
        assert!(
            got.contains(&canon("factory: () => (() => new Alternate())()")),
            "got: {got}"
        );
    }

    #[test]
    fn injectable_type_is_injectable_declaration() {
        let meta = injectable_meta("MyService");
        let compiled = compile_injectable(&meta, false);
        match &compiled.ty {
            Type::Expression { value, .. } => {
                let printed = canon(&emit_expression(value));
                assert!(printed.contains("ɵɵInjectableDeclaration"), "got: {printed}");
            }
            other => panic!("expected expression type, got {other:?}"),
        }
    }
}
