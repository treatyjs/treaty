//! Pipe (`ɵpipe`), NgModule (`ɵmod`/`ɵinj`), injector, and class-metadata codegen.
//!
//! PORT TARGET: `migration/render3-specs/16-pipe_module_injector.md`
//! Sources (Angular 22.1.0-next.0), under `packages/compiler/src/render3/`:
//!   * `r3_pipe_compiler.ts`         — `compilePipeFromMetadata` / `createPipeType`
//!   * `r3_module_compiler.ts`       — `compileNgModule` / `createNgModuleType` (+ scope IIFE)
//!   * `r3_injector_compiler.ts`     — `compileInjector` / `createInjectorType`
//!   * `r3_class_metadata_compiler.ts` — `compileClassMetadata` & async/defer variants
//!
//! These are the "small definition" emitters: each takes a metadata struct and produces an
//! [`R3CompiledExpression`] (`{expression, type, statements}`) describing a static
//! `ɵfac`-adjacent def field (`ɵpipe`, `ɵmod`, `ɵinj`) or a side-effecting metadata call
//! (`setClassMetadata`). No template instruction stream is built here.
//!
//! Everything is constructed from [`crate::output_ast`] builders + [`crate::identifiers::R3`].
//!
//! ## Shared sibling types/helpers
//! `DefinitionMap` (the `render3/view/util.ts` object-literal builder) is re-exported from its
//! canonical home in [`crate::view::compiler`]. The remaining `view/util.ts` helpers (`refsToArray`,
//! `jitOnlyGuardedExpression`, `devOnlyGuardedExpression`) are still defined locally below until a
//! dedicated `view/util` module is split out. The `render3/util.ts` types (`R3Reference`,
//! `R3CompiledExpression`, `typeWithParameters`, `tsIgnoreComment`) live in [`crate::util`].
//! `R3DependencyMetadata` (from `r3_factory.ts`) is shared via [`crate::factory`];
//! `R3DeferPerComponentDependency` (from `view/api.ts`) via [`crate::view::compiler`].

use crate::identifiers::R3;
use crate::output_ast::{
    arrow_fn, dynamic_type, expression_type, import_expr, literal, literal_arr,
    literal_map, none_type, typeof_expr, variable, ArrowBody, Expr, ExprKind, ExternalReference,
    FnParam, ImportUrl, LeadingComment, LiteralMapEntry, LiteralValue, Stmt, Type,
    WrappedNodeHandle,
};
use crate::util::{ts_ignore_comment, type_with_parameters, R3CompiledExpression, R3Reference};

// Shared sibling types (no longer local stubs):
//   * `R3DependencyMetadata` is the real factory type (`r3_factory.ts`). It is declared on
//     `R3PipeMetadata` for struct parity but **never read** by these emitters (the factory compiler
//     consumes it).
//   * `R3DeferPerComponentDependency` is the real `view/api.ts` type, used by
//     `compileComponentMetadataAsyncResolver` (only `symbol_name` / `import_path` /
//     `is_default_import` are read).
pub use crate::factory::R3DependencyMetadata;
pub use crate::view::compiler::R3DeferPerComponentDependency;

// ---------------------------------------------------------------------------
// util.ts helpers (local). `typeWithParameters` / `tsIgnoreComment` now live in `crate::util`;
// the guard / refs helpers below remain local until `render3/view/util.ts` lands.
// ---------------------------------------------------------------------------

/// `util.ts` `refsToArray(refs, shouldForwardDeclare)` — `literalArr(refs.map(r => r.value))`,
/// wrapped in `() => [...]` when forward declaration is required.
pub fn refs_to_array(refs: &[R3Reference], should_forward_declare: bool) -> Expr {
    let values = literal_arr(refs.iter().map(|r| r.value.clone()).collect(), None);
    if should_forward_declare {
        arrow_fn(vec![], ArrowBody::Expr(Box::new(values)), None)
    } else {
        values
    }
}

/// `util.ts` `guardedExpression(guard, expr)` →
/// `(typeof <guard> === 'undefined' || <guard>) && expr`.
///
/// The guard is referenced as an `ExternalExpr` with **no module** (`moduleName: null`), i.e. a
/// bare global identifier such as `ngJitMode` / `ngDevMode`.
fn guarded_expression(guard: &str, expr: Expr) -> Expr {
    let guard_expr = || {
        import_expr(
            ExternalReference {
                name: guard.to_string(),
                module_name: None,
            },
            None,
        )
    };
    // typeof guard === 'undefined'
    let guard_not_defined = typeof_expr(guard_expr())
        .identical(literal(LiteralValue::String("undefined".to_string()), None));
    // (typeof guard === 'undefined') || guard
    let guard_undefined_or_true = guard_not_defined.or(guard_expr());
    // (typeof guard === 'undefined' || guard) && expr
    guard_undefined_or_true.and(expr)
}

/// `util.ts` `jitOnlyGuardedExpression(expr)` — guards with `ngJitMode`.
pub fn jit_only_guarded_expression(expr: Expr) -> Expr {
    guarded_expression("ngJitMode", expr)
}

/// `util.ts` `devOnlyGuardedExpression(expr)` — guards with `ngDevMode`.
pub fn dev_only_guarded_expression(expr: Expr) -> Expr {
    guarded_expression("ngDevMode", expr)
}

// ---------------------------------------------------------------------------
// `view/util.ts` `DefinitionMap` — the ordered, string-keyed object-literal builder
// (no-op on `None`, upsert by key). The canonical port lives in `crate::view::compiler`;
// re-exported here so these emitters share the one implementation.
// ---------------------------------------------------------------------------

pub use crate::view::compiler::DefinitionMap;

// ---------------------------------------------------------------------------
// Small local builders mirroring the `o.*` constructors / fluent methods used
// here but not exposed as free fns by the output_ast port.
// ---------------------------------------------------------------------------

/// `new o.WrappedNodeExpr(node)` — wraps a foreign host AST node (referenced by handle).
fn wrapped_node_expr(node: WrappedNodeHandle) -> Expr {
    Expr::bare(ExprKind::WrappedNode(node))
}

/// `new o.DynamicImportExpr(url)` — `import('<url>')` from a string path.
fn dynamic_import_str(url: &str) -> Expr {
    Expr::bare(ExprKind::DynamicImport {
        url: ImportUrl::Str(url.to_string()),
        url_comment: None,
    })
}

/// `expr.callFn(args, _, pure)` but additionally attaching `leadingComments` to the produced
/// `InvokeFunctionExpr` (the 4th `callFn` arg). Mirrors `Expression.callFn(..., leadingComments)`.
fn call_fn_with_comments(callee: Expr, args: Vec<Expr>, comments: Vec<LeadingComment>) -> Expr {
    let mut e = callee.call_fn(args, false);
    e.meta.leading_comments = comments;
    e
}

// ===========================================================================
// Pipe — r3_pipe_compiler.ts
// ===========================================================================

/// `r3_pipe_compiler.ts` `R3PipeMetadata`.
#[derive(Debug, Clone, PartialEq)]
pub struct R3PipeMetadata {
    /// Name of the pipe *type* (class name).
    pub name: String,
    /// Reference to the pipe type itself.
    pub r#type: R3Reference,
    /// Number of generic type parameters of the type.
    pub type_argument_count: u32,
    /// The `@Pipe({name})` value; `None` for some standalone/anon cases.
    pub pipe_name: Option<String>,
    /// Constructor deps. Present for parity; **unused** by this emitter (factory consumes it).
    pub deps: Option<Vec<R3DependencyMetadata>>,
    /// Whether the pipe is pure.
    pub pure: bool,
    /// Whether the pipe is standalone.
    pub is_standalone: bool,
}

/// `compilePipeFromMetadata(metadata)` → `ɵpipe = ɵɵdefinePipe({ name, type, pure, [standalone] })`.
///
/// NOTE: this compiler uses a *raw array* of map entries, **not** a `DefinitionMap` (see spec §7).
pub fn compile_pipe_from_metadata(metadata: &R3PipeMetadata) -> R3CompiledExpression {
    let mut entries: Vec<(String, bool, Expr)> = Vec::new();

    // e.g. `name: 'myPipe'` — falls back to the class name when `pipeName` is null.
    let name_value = metadata
        .pipe_name
        .clone()
        .unwrap_or_else(|| metadata.name.clone());
    entries.push((
        "name".to_string(),
        false,
        literal(LiteralValue::String(name_value), None),
    ));

    // e.g. `type: MyPipe`
    entries.push(("type".to_string(), false, metadata.r#type.value.clone()));

    // e.g. `pure: true`
    entries.push((
        "pure".to_string(),
        false,
        literal(LiteralValue::Bool(metadata.pure), None),
    ));

    // `standalone: false` is emitted *only* on a strict `=== false`; standalone-true is default.
    if !metadata.is_standalone {
        entries.push((
            "standalone".to_string(),
            false,
            literal(LiteralValue::Bool(false), None),
        ));
    }

    // `ɵɵdefinePipe({...})` — pure call.
    let expression = import_expr(R3::DefinePipe.reference(), None)
        .call_fn(vec![literal_map(entries, None)], /* pure */ true);
    let ty = create_pipe_type(metadata);

    R3CompiledExpression {
        expression,
        ty,
        statements: vec![],
    }
}

/// `createPipeType(metadata)` →
/// `ExpressionType(importExpr(ɵɵPipeDeclaration, [typeWithParameters(type.type, n), <pipeName>, <isStandalone>]))`.
pub fn create_pipe_type(metadata: &R3PipeMetadata) -> Type {
    let pipe_name_literal = match &metadata.pipe_name {
        Some(name) => LiteralValue::String(name.clone()),
        None => LiteralValue::Null,
    };
    let type_params = vec![
        type_with_parameters(metadata.r#type.ty.clone(), metadata.type_argument_count),
        expression_type(literal(pipe_name_literal, None), None, None),
        expression_type(
            literal(LiteralValue::Bool(metadata.is_standalone), None),
            None,
            None,
        ),
    ];
    expression_type(
        import_expr(R3::PipeDeclaration.reference(), Some(type_params)),
        None,
        None,
    )
}

// ===========================================================================
// NgModule — r3_module_compiler.ts
// ===========================================================================

/// `R3SelectorScopeMode` — how the selector scope (declarations/imports/exports) is emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum R3SelectorScopeMode {
    /// Inline the scope into the `ɵɵdefineNgModule` call (JIT-required, blocks tree-shaking).
    Inline,
    /// Emit a `ngJitMode`-guarded `ɵɵsetNgModuleScope` side effect (tree-shakeable).
    SideEffect,
    /// Don't emit selector scope at all.
    Omit,
}

/// `R3NgModuleMetadataKind` — note: encoded by the Rust enum tag of [`R3NgModuleMetadata`],
/// retained here for parity / readability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum R3NgModuleMetadataKind {
    Global,
    Local,
    Isolated,
}

/// `R3NgModuleMetadataCommon` — the base fields shared by all three NgModule variants.
#[derive(Debug, Clone, PartialEq)]
pub struct R3NgModuleCommon {
    /// The module type being compiled.
    pub r#type: R3Reference,
    /// How to emit the selector scope values.
    pub selector_scope_mode: R3SelectorScopeMode,
    /// Schemas declaring allowed elements (`None` = none).
    pub schemas: Option<Vec<R3Reference>>,
    /// Unique ID expression of the NgModule (`None` = none).
    pub id: Option<Expr>,
}

/// `R3NgModuleMetadataGlobal` — full/partial compilation (R3References).
#[derive(Debug, Clone, PartialEq)]
pub struct R3NgModuleMetadataGlobal {
    pub common: R3NgModuleCommon,
    pub bootstrap: Vec<R3Reference>,
    pub declarations: Vec<R3Reference>,
    /// Declarations visible downstream; `None` = all declarations public.
    pub public_declaration_types: Option<Vec<Expr>>,
    pub imports: Vec<R3Reference>,
    pub include_import_types: bool,
    pub exports: Vec<R3Reference>,
    pub contains_forward_decls: bool,
}

/// `R3NgModuleMetadataLocal` — local compilation (raw decorator expressions). Scope mode is
/// always `SideEffect` (invariant pinned via the enum tag; `common.selector_scope_mode` should be
/// set to `SideEffect`).
#[derive(Debug, Clone, PartialEq)]
pub struct R3NgModuleMetadataLocal {
    pub common: R3NgModuleCommon,
    pub bootstrap_expression: Option<Expr>,
    pub declarations_expression: Option<Expr>,
    pub imports_expression: Option<Expr>,
    pub exports_expression: Option<Expr>,
}

/// `R3NgModuleMetadataIsolated` — isolated-declarations mode. Scope mode is always `Omit`.
#[derive(Debug, Clone, PartialEq)]
pub struct R3NgModuleMetadataIsolated {
    pub common: R3NgModuleCommon,
    pub imports_expression: Option<Expr>,
    pub exports_expression: Option<Expr>,
}

/// `R3NgModuleMetadata` — discriminated by `kind`. The Rust enum tag *is* the `kind`, so the
/// Global-only fields are not exposed on the other variants.
#[derive(Debug, Clone, PartialEq)]
pub enum R3NgModuleMetadata {
    Global(R3NgModuleMetadataGlobal),
    Local(R3NgModuleMetadataLocal),
    Isolated(R3NgModuleMetadataIsolated),
}

impl R3NgModuleMetadata {
    pub fn kind(&self) -> R3NgModuleMetadataKind {
        match self {
            R3NgModuleMetadata::Global(_) => R3NgModuleMetadataKind::Global,
            R3NgModuleMetadata::Local(_) => R3NgModuleMetadataKind::Local,
            R3NgModuleMetadata::Isolated(_) => R3NgModuleMetadataKind::Isolated,
        }
    }

    fn common(&self) -> &R3NgModuleCommon {
        match self {
            R3NgModuleMetadata::Global(m) => &m.common,
            R3NgModuleMetadata::Local(m) => &m.common,
            R3NgModuleMetadata::Isolated(m) => &m.common,
        }
    }
}

/// `compileNgModule(meta)` — emit `ɵmod = ɵɵdefineNgModule({...})` plus optional scope side
/// effects and `ɵɵregisterNgModuleType`.
pub fn compile_ng_module(meta: &R3NgModuleMetadata) -> R3CompiledExpression {
    let mut statements: Vec<Stmt> = Vec::new();
    let mut definition_map = DefinitionMap::new();
    let common = meta.common();

    definition_map.set("type", Some(common.r#type.value.clone()));

    // bootstrap — Global only, and only when non-empty.
    if let R3NgModuleMetadata::Global(g) = meta {
        if !g.bootstrap.is_empty() {
            definition_map.set(
                "bootstrap",
                Some(refs_to_array(&g.bootstrap, g.contains_forward_decls)),
            );
        }
    }

    match common.selector_scope_mode {
        // Scope emission. Both `Inline` and `SideEffect` route the selector scope
        // (declarations/imports/exports) through the `ngJitMode`-guarded `ɵɵsetNgModuleScope` side
        // effect rather than inlining it into the `ɵɵdefineNgModule({...})` call.
        //
        // Angular's `Inline` mode literally inlines the scope arrays (a JIT-only, tree-shaking-
        // hostile form). It is never used for the **Global** (full/partial AOT) kind that the source
        // front-end produces: the full/local goldens emit `ɵɵdefineNgModule({type[, bootstrap]
        // [, id]})` with the scope in a separate guarded `ɵɵsetNgModuleScope` so unused declarations
        // can be tree-shaken. The genuine JIT-inline facade has its own emitter
        // (`compile_ng_module_declaration_expression`). Treating `Inline` and `SideEffect`
        // identically here makes the AOT module def match Angular's full/local define-block shape.
        R3SelectorScopeMode::Inline | R3SelectorScopeMode::SideEffect => {
            if let Some(call) = generate_set_ng_module_scope_call(meta) {
                statements.push(call);
            }
        }
        R3SelectorScopeMode::Omit => {}
    }

    // schemas — if present and non-empty.
    if let Some(schemas) = &common.schemas {
        if !schemas.is_empty() {
            let arr = literal_arr(schemas.iter().map(|r| r.value.clone()).collect(), None);
            definition_map.set("schemas", Some(arr));
        }
    }

    // id — set the field AND emit a `ɵɵregisterNgModuleType(type, id)` side effect.
    if let Some(id) = &common.id {
        definition_map.set("id", Some(id.clone()));
        let register = import_expr(R3::RegisterNgModuleType.reference(), None)
            .call_fn(vec![common.r#type.value.clone(), id.clone()], false);
        statements.push(register.to_stmt());
    }

    let expression = import_expr(R3::DefineNgModule.reference(), None)
        .call_fn(vec![definition_map.to_literal_map()], /* pure */ true);
    let ty = create_ng_module_type(meta);

    R3CompiledExpression {
        expression,
        ty,
        statements,
    }
}

/// `compileNgModuleDeclarationExpression(meta)` — the JIT/linker path that lowers a
/// `ɵɵngDeclareNgModule` facade to a `ɵɵdefineNgModule` call. Each raw TS node is wrapped in
/// `WrappedNodeExpr`. **No pure flag** (unlike [`compile_ng_module`]).
///
/// The facade carries already-resolved host AST node handles. `type` is always present; the rest
/// are `Some` iff defined.
pub fn compile_ng_module_declaration_expression(
    type_node: WrappedNodeHandle,
    bootstrap_node: Option<WrappedNodeHandle>,
    declarations_node: Option<WrappedNodeHandle>,
    imports_node: Option<WrappedNodeHandle>,
    exports_node: Option<WrappedNodeHandle>,
    schemas_node: Option<WrappedNodeHandle>,
    id_node: Option<WrappedNodeHandle>,
) -> Expr {
    let mut definition_map = DefinitionMap::new();
    definition_map.set("type", Some(wrapped_node_expr(type_node)));
    definition_map.set("bootstrap", bootstrap_node.map(wrapped_node_expr));
    definition_map.set("declarations", declarations_node.map(wrapped_node_expr));
    definition_map.set("imports", imports_node.map(wrapped_node_expr));
    definition_map.set("exports", exports_node.map(wrapped_node_expr));
    definition_map.set("schemas", schemas_node.map(wrapped_node_expr));
    definition_map.set("id", id_node.map(wrapped_node_expr));
    // NOTE: no pure flag on this path (replicates the TS asymmetry — see spec §7).
    import_expr(R3::DefineNgModule.reference(), None)
        .call_fn(vec![definition_map.to_literal_map()], false)
}

/// `createNgModuleType(meta)` — the `.d.ts` `ɵɵNgModuleDeclaration<...>` type (Local/Isolated/Global).
pub fn create_ng_module_type(meta: &R3NgModuleMetadata) -> Type {
    match meta {
        R3NgModuleMetadata::Local(m) => expression_type(m.common.r#type.value.clone(), None, None),
        R3NgModuleMetadata::Isolated(m) => {
            let type_params = vec![
                expression_type(m.common.r#type.ty.clone(), None, None),
                none_type(),
                match &m.imports_expression {
                    Some(e) => expression_type(e.clone(), None, None),
                    None => none_type(),
                },
                match &m.exports_expression {
                    Some(e) => expression_type(e.clone(), None, None),
                    None => none_type(),
                },
            ];
            expression_type(
                import_expr(R3::NgModuleDeclaration.reference(), Some(type_params)),
                None,
                None,
            )
        }
        R3NgModuleMetadata::Global(m) => {
            let declarations_type = match &m.public_declaration_types {
                None => tuple_type_of(&m.declarations),
                Some(types) => tuple_of_types(types),
            };
            let imports_type = if m.include_import_types {
                tuple_type_of(&m.imports)
            } else {
                none_type()
            };
            let type_params = vec![
                expression_type(m.common.r#type.ty.clone(), None, None),
                declarations_type,
                imports_type,
                tuple_type_of(&m.exports),
            ];
            expression_type(
                import_expr(R3::NgModuleDeclaration.reference(), Some(type_params)),
                None,
                None,
            )
        }
    }
}

/// `generateSetNgModuleScopeCall(meta)` — the `ngJitMode`-guarded `ɵɵsetNgModuleScope` IIFE.
/// Returns `None` when there is no scope information to register.
fn generate_set_ng_module_scope_call(meta: &R3NgModuleMetadata) -> Option<Stmt> {
    let mut scope_map = DefinitionMap::new();

    match meta {
        R3NgModuleMetadata::Global(g) => {
            if !g.declarations.is_empty() {
                scope_map.set(
                    "declarations",
                    Some(refs_to_array(&g.declarations, g.contains_forward_decls)),
                );
            }
            if !g.imports.is_empty() {
                scope_map.set(
                    "imports",
                    Some(refs_to_array(&g.imports, g.contains_forward_decls)),
                );
            }
            if !g.exports.is_empty() {
                scope_map.set(
                    "exports",
                    Some(refs_to_array(&g.exports, g.contains_forward_decls)),
                );
            }
        }
        R3NgModuleMetadata::Local(l) => {
            scope_map.set("declarations", l.declarations_expression.clone());
            scope_map.set("imports", l.imports_expression.clone());
            scope_map.set("exports", l.exports_expression.clone());
            scope_map.set("bootstrap", l.bootstrap_expression.clone());
        }
        R3NgModuleMetadata::Isolated(_) => {}
    }

    if scope_map.values.is_empty() {
        return None;
    }

    let common = meta.common();
    // ɵɵsetNgModuleScope(type, { ... })
    let fn_call = import_expr(R3::SetNgModuleScope.reference(), None).call_fn(
        vec![common.r#type.value.clone(), scope_map.to_literal_map()],
        false,
    );
    // (ngJitMode guard) && ɵɵsetNgModuleScope(...)
    let guarded_call = jit_only_guarded_expression(fn_call);
    // function() { (guard) && setNgModuleScope(...); }
    let iife = Expr::bare(ExprKind::Function {
        params: vec![],
        statements: vec![guarded_call.to_stmt()],
        name: None,
    });
    // (function() { ... })()
    let iife_call = iife.call_fn(vec![], false);
    Some(iife_call.to_stmt())
}

/// `tupleTypeOf(refs)` — `expressionType(literalArr(refs.map(r => typeofExpr(r.type))))`, or
/// `NONE_TYPE` when empty.
fn tuple_type_of(refs: &[R3Reference]) -> Type {
    if refs.is_empty() {
        none_type()
    } else {
        let types = refs
            .iter()
            .map(|r| typeof_expr(r.ty.clone()))
            .collect();
        expression_type(literal_arr(types, None), None, None)
    }
}

/// `tupleOfTypes(types)` — like [`tuple_type_of`] but over raw expressions.
fn tuple_of_types(types: &[Expr]) -> Type {
    if types.is_empty() {
        none_type()
    } else {
        let typeof_types = types.iter().map(|t| typeof_expr(t.clone())).collect();
        expression_type(literal_arr(typeof_types, None), None, None)
    }
}

// ===========================================================================
// Injector — r3_injector_compiler.ts
// ===========================================================================

/// `r3_injector_compiler.ts` `R3InjectorMetadata`.
#[derive(Debug, Clone, PartialEq)]
pub struct R3InjectorMetadata {
    /// Carried for parity; **unused** by the emitter.
    pub name: String,
    pub r#type: R3Reference,
    pub providers: Option<Expr>,
    pub imports: Vec<Expr>,
}

/// `compileInjector(meta)` → `ɵinj = ɵɵdefineInjector({ [providers], [imports] })`.
pub fn compile_injector(meta: &R3InjectorMetadata) -> R3CompiledExpression {
    let mut definition_map = DefinitionMap::new();

    if let Some(providers) = &meta.providers {
        definition_map.set("providers", Some(providers.clone()));
    }
    if !meta.imports.is_empty() {
        definition_map.set("imports", Some(literal_arr(meta.imports.clone(), None)));
    }

    let expression = import_expr(R3::DefineInjector.reference(), None)
        .call_fn(vec![definition_map.to_literal_map()], /* pure */ true);
    let ty = create_injector_type(meta);

    R3CompiledExpression {
        expression,
        ty,
        statements: vec![],
    }
}

/// `createInjectorType(meta)` → `ExpressionType(importExpr(ɵɵInjectorDeclaration, [ExpressionType(type.type)]))`.
pub fn create_injector_type(meta: &R3InjectorMetadata) -> Type {
    let type_params = vec![expression_type(meta.r#type.ty.clone(), None, None)];
    expression_type(
        import_expr(R3::InjectorDeclaration.reference(), Some(type_params)),
        None,
        None,
    )
}

// ===========================================================================
// Class metadata — r3_class_metadata_compiler.ts
// ===========================================================================

/// `r3_class_metadata_compiler.ts` `R3ClassMetadata`.
#[derive(Debug, Clone, PartialEq)]
pub struct R3ClassMetadata {
    pub r#type: Expr,
    pub decorators: Expr,
    pub ctor_parameters: Option<Expr>,
    pub prop_decorators: Option<Expr>,
}

/// `compileClassMetadata(metadata)` → `(() => { ngDevMode && ɵsetClassMetadata(...); })()`.
pub fn compile_class_metadata(metadata: &R3ClassMetadata) -> Expr {
    let fn_call = internal_compile_class_metadata(metadata);
    arrow_fn(
        vec![],
        ArrowBody::Block(vec![dev_only_guarded_expression(fn_call).to_stmt()]),
        None,
    )
    .call_fn(vec![], false)
}

/// `internalCompileClassMetadata(metadata)` — the bare `ɵsetClassMetadata(type, decorators,
/// ctorParameters ?? null, propDecorators ?? null)` call (no dev-mode wrapper).
fn internal_compile_class_metadata(metadata: &R3ClassMetadata) -> Expr {
    let ctor_parameters = metadata
        .ctor_parameters
        .clone()
        .unwrap_or_else(|| literal(LiteralValue::Null, None));
    let prop_decorators = metadata
        .prop_decorators
        .clone()
        .unwrap_or_else(|| literal(LiteralValue::Null, None));
    import_expr(R3::SetClassMetadata.reference(), None).call_fn(
        vec![
            metadata.r#type.clone(),
            metadata.decorators.clone(),
            ctor_parameters,
            prop_decorators,
        ],
        false,
    )
}

/// `compileComponentClassMetadata(metadata, dependencies)` — emits a plain `setClassMetadata`
/// call when there are no deferrable deps, otherwise the async `setClassMetadataAsync` form.
pub fn compile_component_class_metadata(
    metadata: &R3ClassMetadata,
    dependencies: Option<&[R3DeferPerComponentDependency]>,
) -> Expr {
    match dependencies {
        None => compile_class_metadata(metadata),
        Some(deps) if deps.is_empty() => compile_class_metadata(metadata),
        Some(deps) => {
            let wrapper_params = deps
                .iter()
                .map(|dep| FnParam::new(dep.symbol_name.clone(), Some(dynamic_type())))
                .collect();
            internal_compile_set_class_metadata_async(
                metadata,
                wrapper_params,
                compile_component_metadata_async_resolver(deps),
            )
        }
    }
}

/// `compileOpaqueAsyncClassMetadata(metadata, deferResolver, deferredDependencyNames)` — the
/// async form using a pre-compiled resolver function.
pub fn compile_opaque_async_class_metadata(
    metadata: &R3ClassMetadata,
    defer_resolver: Expr,
    deferred_dependency_names: &[String],
) -> Expr {
    let wrapper_params = deferred_dependency_names
        .iter()
        .map(|name| FnParam::new(name.clone(), Some(dynamic_type())))
        .collect();
    internal_compile_set_class_metadata_async(metadata, wrapper_params, defer_resolver)
}

/// `internalCompileSetClassMetadataAsync(metadata, wrapperParams, dependencyResolverFn)` →
/// `(() => { ngDevMode && ɵsetClassMetadataAsync(type, resolverFn, (deps...) => { setClassMetadata(...); }); })()`.
fn internal_compile_set_class_metadata_async(
    metadata: &R3ClassMetadata,
    wrapper_params: Vec<FnParam>,
    dependency_resolver_fn: Expr,
) -> Expr {
    let set_class_metadata_call = internal_compile_class_metadata(metadata);
    let set_class_meta_wrapper = arrow_fn(
        wrapper_params,
        ArrowBody::Block(vec![set_class_metadata_call.to_stmt()]),
        None,
    );
    let set_class_meta_async = import_expr(R3::SetClassMetadataAsync.reference(), None).call_fn(
        vec![
            metadata.r#type.clone(),
            dependency_resolver_fn,
            set_class_meta_wrapper,
        ],
        false,
    );
    arrow_fn(
        vec![],
        ArrowBody::Block(vec![dev_only_guarded_expression(set_class_meta_async).to_stmt()]),
        None,
    )
    .call_fn(vec![], false)
}

/// `compileComponentMetadataAsyncResolver(dependencies)` →
/// `() => [ import('./cmp-a').then(m => m.CmpA), ... ]` (each `.then` arg carries a `@ts-ignore`).
pub fn compile_component_metadata_async_resolver(
    dependencies: &[R3DeferPerComponentDependency],
) -> Expr {
    let dynamic_imports = dependencies
        .iter()
        .map(|dep| {
            // (m) => m.<symbol> | m.default
            let prop = if dep.is_default_import {
                "default"
            } else {
                dep.symbol_name.as_str()
            };
            let inner_fn = arrow_fn(
                vec![FnParam::new("m", Some(dynamic_type()))],
                ArrowBody::Expr(Box::new(variable("m", None).prop(prop))),
                None,
            );
            // import('<path>').then(innerFn) /* @ts-ignore on the call */
            let then_callee = dynamic_import_str(&dep.import_path).prop("then");
            call_fn_with_comments(then_callee, vec![inner_fn], vec![ts_ignore_comment()])
        })
        .collect();

    arrow_fn(
        vec![],
        ArrowBody::Expr(Box::new(literal_arr(dynamic_imports, None))),
        None,
    )
}

// ===========================================================================
// Tests — assert produced output_ast structure.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output_ast::BinaryOperator;

    fn ref_to(name: &str) -> R3Reference {
        // A simple R3Reference whose value/type are both `ReadVar(name)`.
        R3Reference::new(variable(name, None), variable(name, None))
    }

    /// Extract the entries of a `LiteralMapExpr` (the argument to a define* call).
    fn map_entries_of_call_arg(expr: &Expr) -> Vec<(String, Expr)> {
        let arg = match &expr.kind {
            ExprKind::Invoke { args, .. } => &args[0],
            _ => panic!("expected an InvokeFunctionExpr, got {:?}", expr.kind),
        };
        match &arg.kind {
            ExprKind::LiteralMap { entries, .. } => entries
                .iter()
                .map(|e| match e {
                    LiteralMapEntry::Property { key, value, .. } => (key.clone(), value.clone()),
                    LiteralMapEntry::Spread { .. } => panic!("unexpected spread"),
                })
                .collect(),
            other => panic!("expected LiteralMap arg, got {other:?}"),
        }
    }

    fn callee_ref(expr: &Expr) -> &ExternalReference {
        match &expr.kind {
            ExprKind::Invoke { callee, .. } => match &callee.kind {
                ExprKind::External { value, .. } => value,
                other => panic!("expected External callee, got {other:?}"),
            },
            other => panic!("expected Invoke, got {other:?}"),
        }
    }

    #[test]
    fn pipe_emits_define_pipe_with_name_type_pure() {
        let meta = R3PipeMetadata {
            name: "MyPipe".to_string(),
            r#type: ref_to("MyPipe"),
            type_argument_count: 0,
            pipe_name: Some("myPipe".to_string()),
            deps: None,
            pure: true,
            is_standalone: true,
        };
        let compiled = compile_pipe_from_metadata(&meta);

        // Calls ɵɵdefinePipe, pure.
        assert_eq!(callee_ref(&compiled.expression).name, "ɵɵdefinePipe");
        assert!(matches!(
            compiled.expression.kind,
            ExprKind::Invoke { pure: true, .. }
        ));
        assert!(compiled.statements.is_empty());

        let entries = map_entries_of_call_arg(&compiled.expression);
        // standalone:true is the default -> omitted. So name, type, pure only.
        let keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["name", "type", "pure"]);

        // name falls back through pipeName.
        assert_eq!(
            entries[0].1.kind,
            ExprKind::Literal(LiteralValue::String("myPipe".to_string()))
        );
        // pure: true
        assert_eq!(entries[2].1.kind, ExprKind::Literal(LiteralValue::Bool(true)));
    }

    #[test]
    fn pipe_name_falls_back_to_class_name() {
        let meta = R3PipeMetadata {
            name: "MyPipe".to_string(),
            r#type: ref_to("MyPipe"),
            type_argument_count: 0,
            pipe_name: None,
            deps: None,
            pure: false,
            is_standalone: false,
        };
        let compiled = compile_pipe_from_metadata(&meta);
        let entries = map_entries_of_call_arg(&compiled.expression);
        // name = class name fallback.
        assert_eq!(
            entries[0].1.kind,
            ExprKind::Literal(LiteralValue::String("MyPipe".to_string()))
        );
        // non-standalone -> standalone:false appended last.
        let keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["name", "type", "pure", "standalone"]);
        assert_eq!(
            entries[3].1.kind,
            ExprKind::Literal(LiteralValue::Bool(false))
        );
    }

    #[test]
    fn pipe_type_uses_pipe_declaration() {
        let meta = R3PipeMetadata {
            name: "MyPipe".to_string(),
            r#type: ref_to("MyPipe"),
            type_argument_count: 2,
            pipe_name: Some("myPipe".to_string()),
            deps: None,
            pure: true,
            is_standalone: true,
        };
        let ty = create_pipe_type(&meta);
        match &ty {
            Type::Expression { value, .. } => match &value.kind {
                ExprKind::External {
                    value: ext,
                    type_params: Some(params),
                } => {
                    assert_eq!(ext.name, "ɵɵPipeDeclaration");
                    assert_eq!(params.len(), 3);
                }
                other => panic!("expected External, got {other:?}"),
            },
            other => panic!("expected ExpressionType, got {other:?}"),
        }
    }

    #[test]
    fn simple_global_module_emits_define_ng_module() {
        let meta = R3NgModuleMetadata::Global(R3NgModuleMetadataGlobal {
            common: R3NgModuleCommon {
                r#type: ref_to("MyModule"),
                selector_scope_mode: R3SelectorScopeMode::Inline,
                schemas: None,
                id: None,
            },
            bootstrap: vec![],
            declarations: vec![ref_to("MyCmp")],
            public_declaration_types: None,
            imports: vec![ref_to("CommonModule")],
            include_import_types: true,
            exports: vec![],
            contains_forward_decls: false,
        });
        let compiled = compile_ng_module(&meta);

        assert_eq!(callee_ref(&compiled.expression).name, "ɵɵdefineNgModule");
        assert!(matches!(
            compiled.expression.kind,
            ExprKind::Invoke { pure: true, .. }
        ));
        // Global modules route their selector scope to a guarded `ɵɵsetNgModuleScope` side effect
        // (full/local AOT shape) under BOTH `Inline` and `SideEffect`, so a non-empty
        // declarations/imports set produces exactly one IIFE side-effect statement.
        assert_eq!(compiled.statements.len(), 1);

        let entries = map_entries_of_call_arg(&compiled.expression);
        let keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
        // The define block carries ONLY `type` (bootstrap empty -> omitted; scope is side-effected).
        assert_eq!(keys, vec!["type"]);
    }

    #[test]
    fn module_id_emits_register_side_effect() {
        let meta = R3NgModuleMetadata::Global(R3NgModuleMetadataGlobal {
            common: R3NgModuleCommon {
                r#type: ref_to("MyModule"),
                selector_scope_mode: R3SelectorScopeMode::Omit,
                schemas: None,
                id: Some(literal(LiteralValue::String("mod-id".to_string()), None)),
            },
            bootstrap: vec![],
            declarations: vec![],
            public_declaration_types: None,
            imports: vec![],
            include_import_types: false,
            exports: vec![],
            contains_forward_decls: false,
        });
        let compiled = compile_ng_module(&meta);
        // One statement: ɵɵregisterNgModuleType(type, id).
        assert_eq!(compiled.statements.len(), 1);
        let stmt = &compiled.statements[0];
        match &stmt.kind {
            crate::output_ast::StmtKind::Expression(e) => {
                assert_eq!(callee_ref(e).name, "ɵɵregisterNgModuleType");
            }
            other => panic!("expected ExpressionStatement, got {other:?}"),
        }
        // id present in the def map.
        let entries = map_entries_of_call_arg(&compiled.expression);
        assert!(entries.iter().any(|(k, _)| k == "id"));
    }

    #[test]
    fn side_effect_mode_emits_jit_guarded_iife() {
        let meta = R3NgModuleMetadata::Global(R3NgModuleMetadataGlobal {
            common: R3NgModuleCommon {
                r#type: ref_to("MyModule"),
                selector_scope_mode: R3SelectorScopeMode::SideEffect,
                schemas: None,
                id: None,
            },
            bootstrap: vec![],
            declarations: vec![ref_to("MyCmp")],
            public_declaration_types: None,
            imports: vec![],
            include_import_types: false,
            exports: vec![],
            contains_forward_decls: false,
        });
        let compiled = compile_ng_module(&meta);
        // One IIFE statement.
        assert_eq!(compiled.statements.len(), 1);
        // The def map has no declarations/imports/exports (those went to setNgModuleScope).
        let entries = map_entries_of_call_arg(&compiled.expression);
        let keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["type"]);
    }

    /// Full-compile emit (the shape Angular's full/local goldens carry, e.g. `all_options.ts` /
    /// `basic_full.ts`): a `SideEffect` scope module that ALSO has `bootstrap` + `id`. The
    /// `ɵɵdefineNgModule` call must carry ONLY `{type, bootstrap, id}` — declarations/imports/
    /// exports are routed to the `ɵɵsetNgModuleScope` side effect (NOT inlined) and an `id`
    /// additionally drives a trailing `ɵɵregisterNgModuleType(Type, id)` statement.
    #[test]
    fn full_mode_define_carries_only_type_bootstrap_id() {
        let meta = R3NgModuleMetadata::Global(R3NgModuleMetadataGlobal {
            common: R3NgModuleCommon {
                r#type: ref_to("MyModule"),
                selector_scope_mode: R3SelectorScopeMode::SideEffect,
                schemas: None,
                id: Some(literal(LiteralValue::String("my-module-id".to_string()), None)),
            },
            bootstrap: vec![ref_to("MyBootstrap")],
            declarations: vec![ref_to("MyDecl"), ref_to("MyExport")],
            public_declaration_types: None,
            imports: vec![ref_to("MyImport")],
            include_import_types: true,
            exports: vec![ref_to("MyExport")],
            contains_forward_decls: false,
        });
        let compiled = compile_ng_module(&meta);

        // The define block carries ONLY type + bootstrap + id — never inline scope arrays.
        let entries = map_entries_of_call_arg(&compiled.expression);
        let keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["type", "bootstrap", "id"]);

        // Two side effects, in order: the JIT-guarded setNgModuleScope IIFE, then the
        // registerNgModuleType(type, id) call.
        assert_eq!(compiled.statements.len(), 2);
        match &compiled.statements[0].kind {
            crate::output_ast::StmtKind::Expression(e) => match &e.kind {
                ExprKind::Invoke { callee, .. } => {
                    assert!(matches!(callee.kind, ExprKind::Function { .. }));
                }
                other => panic!("expected IIFE invoke, got {other:?}"),
            },
            other => panic!("expected expr stmt, got {other:?}"),
        }
        match &compiled.statements[1].kind {
            crate::output_ast::StmtKind::Expression(e) => {
                assert_eq!(callee_ref(e).name, "ɵɵregisterNgModuleType");
            }
            other => panic!("expected register expr stmt, got {other:?}"),
        }
    }

    #[test]
    fn injector_emits_define_injector() {
        let meta = R3InjectorMetadata {
            name: "MyModule".to_string(),
            r#type: ref_to("MyModule"),
            providers: Some(literal_arr(vec![variable("SomeService", None)], None)),
            imports: vec![variable("OtherModule", None)],
        };
        let compiled = compile_injector(&meta);
        assert_eq!(callee_ref(&compiled.expression).name, "ɵɵdefineInjector");
        assert!(matches!(
            compiled.expression.kind,
            ExprKind::Invoke { pure: true, .. }
        ));
        let entries = map_entries_of_call_arg(&compiled.expression);
        let keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["providers", "imports"]);
    }

    #[test]
    fn injector_omits_null_providers() {
        let meta = R3InjectorMetadata {
            name: "MyModule".to_string(),
            r#type: ref_to("MyModule"),
            providers: None,
            imports: vec![],
        };
        let compiled = compile_injector(&meta);
        let entries = map_entries_of_call_arg(&compiled.expression);
        assert!(entries.is_empty());
    }

    #[test]
    fn class_metadata_is_dev_guarded_iife() {
        let meta = R3ClassMetadata {
            r#type: variable("MyCmp", None),
            decorators: literal_arr(vec![], None),
            ctor_parameters: None,
            prop_decorators: None,
        };
        let expr = compile_class_metadata(&meta);
        // Outer: (arrow fn)().
        match &expr.kind {
            ExprKind::Invoke { callee, args, .. } => {
                assert!(args.is_empty());
                // Callee is an arrow with a block body containing a guarded setClassMetadata.
                match &callee.kind {
                    ExprKind::Arrow {
                        body: ArrowBody::Block(stmts),
                        ..
                    } => {
                        assert_eq!(stmts.len(), 1);
                        match &stmts[0].kind {
                            crate::output_ast::StmtKind::Expression(e) => {
                                // ngDevMode && setClassMetadata(...)
                                assert!(matches!(
                                    e.kind,
                                    ExprKind::Binary {
                                        op: BinaryOperator::And,
                                        ..
                                    }
                                ));
                            }
                            other => panic!("expected expr stmt, got {other:?}"),
                        }
                    }
                    other => panic!("expected arrow callee, got {other:?}"),
                }
            }
            other => panic!("expected Invoke, got {other:?}"),
        }
    }

    #[test]
    fn async_resolver_emits_dynamic_imports_with_ts_ignore() {
        let deps = vec![
            R3DeferPerComponentDependency {
                symbol_name: "CmpA".to_string(),
                import_path: "./cmp-a".to_string(),
                is_default_import: false,
            },
            R3DeferPerComponentDependency {
                symbol_name: "CmpB".to_string(),
                import_path: "./cmp-b".to_string(),
                is_default_import: true,
            },
        ];
        let resolver = compile_component_metadata_async_resolver(&deps);
        // () => [ ... ]
        match &resolver.kind {
            ExprKind::Arrow {
                body: ArrowBody::Expr(body),
                params,
            } => {
                assert!(params.is_empty());
                match &body.kind {
                    ExprKind::LiteralArray(entries) => {
                        assert_eq!(entries.len(), 2);
                        // Each entry: import('...').then(fn) with a @ts-ignore comment.
                        for entry in entries {
                            assert_eq!(entry.meta.leading_comments.len(), 1);
                            match &entry.kind {
                                ExprKind::Invoke { callee, .. } => match &callee.kind {
                                    ExprKind::ReadProp { name, receiver, .. } => {
                                        assert_eq!(name, "then");
                                        assert!(matches!(
                                            receiver.kind,
                                            ExprKind::DynamicImport { .. }
                                        ));
                                    }
                                    other => panic!("expected ReadProp then, got {other:?}"),
                                },
                                other => panic!("expected Invoke, got {other:?}"),
                            }
                        }
                    }
                    other => panic!("expected LiteralArray body, got {other:?}"),
                }
            }
            other => panic!("expected arrow with expr body, got {other:?}"),
        }
    }
}
