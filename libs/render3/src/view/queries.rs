//! View/content query generation — `createViewQueriesFunction` / `createContentQueriesFunction`.
//!
//! PORT TARGET: `migration/render3-specs/13-queries.md`
//! Source: `tools/angular-ref/packages/compiler/src/render3/view/query_generation.ts`
//!
//! Generates the `viewQuery` and `contentQueries` host/template functions that the render3
//! runtime invokes to wire up decorator- and signal-based queries on a directive/component
//! definition. Supports both legacy decorator queries (`@ViewChild`/`@ViewChildren`/
//! `@ContentChild`/`@ContentChildren`) and signal queries (`viewChild()`/`viewChildren()`/
//! `contentChild()`/`contentChildren()`).
//!
//! This is a **pure `output_ast` IR producer**: it consumes already-resolved
//! [`R3QueryMetadata`] and emits `ɵɵviewQuery` / `ɵɵviewQuerySignal` / `ɵɵcontentQuery` /
//! `ɵɵcontentQuerySignal` / `ɵɵqueryRefresh` / `ɵɵloadQuery` / `ɵɵqueryAdvance` instructions
//! (via [`crate::identifiers::R3`]). No template parsing, no type checking.
//!
//! Each generated function has two phases driven by the render-flags param (`rf`):
//! - **Create** (`rf & 1`): registers the queries with the runtime.
//! - **Update** (`rf & 2`): legacy queries refresh the `QueryList` and assign to the directive
//!   property; signal queries merely emit `ɵɵqueryAdvance(n)`.

use crate::identifiers::R3;
use crate::output_ast::{
    self as o, Expr, FnParam, LiteralValue, Stmt, StmtKind,
};

// ---------------------------------------------------------------------------
// Shared template-function constants (from `render3/view/util.ts`).
//
// `CONTEXT_NAME`/`RENDER_FLAGS`/`TEMPORARY_NAME` and the lazy `temporaryAllocator` are reproduced
// here as local constants + a minimal allocator so the query algorithm is self-contained and
// faithful. (`crate::view::template` defines its own equivalents for the template builder.)
// ---------------------------------------------------------------------------

/// `CONTEXT_NAME` — the component instance binding (`'ctx'`).
const CONTEXT_NAME: &str = "ctx";
/// `RENDER_FLAGS` — the render-flags param (`'rf'`).
const RENDER_FLAGS: &str = "rf";
/// `TEMPORARY_NAME` — the lazily-declared temporary (`'_t'`).
const TEMPORARY_NAME: &str = "_t";

// ---------------------------------------------------------------------------
// `core.RenderFlags` (from `../../core`). Only the two discriminants used by query generation
// (`Create`/`Update`) are reproduced.
// ---------------------------------------------------------------------------

/// `core.RenderFlags` — the bitmask phase selector passed as the `rf` param.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderFlags {
    /// `Create = 0b01`.
    Create = 0b01,
    /// `Update = 0b10`.
    Update = 0b10,
}

// ---------------------------------------------------------------------------
// `ForwardRefHandling` / `MaybeForwardRefExpression` (from `render3/util.ts`).
// ---------------------------------------------------------------------------

/// `ForwardRefHandling` (`render3/util.ts` `const enum`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForwardRefHandling {
    /// `None = 0` — never wrapped.
    None,
    /// `Wrapped = 1` — still wrapped in `forwardRef()`.
    Wrapped,
    /// `Unwrapped = 2` — was wrapped, since unwrapped.
    Unwrapped,
}

/// `MaybeForwardRefExpression<T>` (`render3/util.ts`).
#[derive(Debug, Clone, PartialEq)]
pub struct MaybeForwardRefExpression {
    pub expression: Expr,
    pub forward_ref: ForwardRefHandling,
}

// ---------------------------------------------------------------------------
// `ConstantPool` (from `../../constant_pool`).
//
// Query generation only ever calls `get_const_literal(predicate, /*forceShared*/ true)` to
// (potentially) hoist the selector-predicate array. The caller in `view::compiler` threads a
// transient pool that it discards — query predicates are therefore emitted inline rather than
// hoisted into shared `_cN` constants (hoisting would dangle without the pool's statements being
// collected into the definition). This stand-in models exactly that inline behavior.
// ---------------------------------------------------------------------------

/// Inline `ConstantPool` for query generation. Only `get_const_literal` is modelled.
#[derive(Debug, Clone, Default)]
pub struct ConstantPool;

impl ConstantPool {
    pub fn new() -> ConstantPool {
        ConstantPool
    }

    /// `getConstLiteral(literal, forceShared)` — returns the literal inline (see the module
    /// comment for why query predicates are not hoisted into shared `_cN` constants here).
    pub fn get_const_literal(&mut self, literal: Expr, _force_shared: bool) -> Expr {
        literal
    }
}

// ---------------------------------------------------------------------------
// `R3QueryMetadata` (from `view/api.ts`).
// ---------------------------------------------------------------------------

/// The query predicate: either a list of selector strings (`string[]`) or a single
/// expression possibly wrapped in `forwardRef()`.
#[derive(Debug, Clone, PartialEq)]
pub enum QueryPredicate {
    /// `string[]` form — local-ref / template-ref selectors.
    Selectors(Vec<String>),
    /// `MaybeForwardRefExpression` form — a directive/token reference.
    Expression(MaybeForwardRefExpression),
}

/// `R3QueryMetadata` (`view/api.ts`) — fully-resolved metadata for a single query.
#[derive(Debug, Clone, PartialEq)]
pub struct R3QueryMetadata {
    /// Name of the property on the directive instance the query reflects into.
    pub property_name: String,
    /// Whether the query reflects a single result (`first`) or a `QueryList`.
    pub first: bool,
    /// The predicate (selectors or expression).
    pub predicate: QueryPredicate,
    /// Whether the query should descend into the view/content children.
    pub descendants: bool,
    /// Whether to emit change events only on actual change.
    pub emit_distinct_changes_only: bool,
    /// Optional `read` token (`o.Expression | null`).
    pub read: Option<Expr>,
    /// Whether the query is static (ViewEngine BC). `static` is a Rust keyword → `is_static`.
    pub is_static: bool,
    /// Whether this is a signal query (`viewChild()` etc.).
    pub is_signal: bool,
}

// ---------------------------------------------------------------------------
// QueryFlags (kept bit-compatible with `core/src/render3/interfaces/query.ts`).
// ---------------------------------------------------------------------------

/// `QueryFlags` — bit flags OR'd into the integer literal emitted as the query's `flags`
/// argument. Modelled as a `u32` newtype (the values are OR'd into a single literal, so a
/// real enum is wrong — we need bit ops).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QueryFlags(pub u32);

impl QueryFlags {
    /// No flags.
    pub const NONE: QueryFlags = QueryFlags(0b0000);
    /// Descend into children.
    pub const DESCENDANTS: QueryFlags = QueryFlags(0b0001);
    /// Computed statically / assigned eagerly (ViewEngine BC).
    pub const IS_STATIC: QueryFlags = QueryFlags(0b0010);
    /// Emit change only when the query actually changed.
    pub const EMIT_DISTINCT_CHANGES_ONLY: QueryFlags = QueryFlags(0b0100);

    #[inline]
    pub fn bits(self) -> u32 {
        self.0
    }
}

impl std::ops::BitOr for QueryFlags {
    type Output = QueryFlags;
    fn bitor(self, rhs: QueryFlags) -> QueryFlags {
        QueryFlags(self.0 | rhs.0)
    }
}

/// `toQueryFlags(query)` — OR the descendants / static / emitDistinctChangesOnly bits.
///
/// NOTE: `first` is intentionally NOT encoded here; and `static` is OR'd unconditionally even
/// for signal queries (per source — do not special-case `is_signal`).
fn to_query_flags(query: &R3QueryMetadata) -> QueryFlags {
    (if query.descendants {
        QueryFlags::DESCENDANTS
    } else {
        QueryFlags::NONE
    }) | (if query.is_static {
        QueryFlags::IS_STATIC
    } else {
        QueryFlags::NONE
    }) | (if query.emit_distinct_changes_only {
        QueryFlags::EMIT_DISTINCT_CHANGES_ONLY
    } else {
        QueryFlags::NONE
    })
}

// ---------------------------------------------------------------------------
// Small `output_ast` helpers.
// ---------------------------------------------------------------------------

/// `o.literal(<number>)`.
fn number_literal(n: f64) -> Expr {
    o::literal(LiteralValue::Number(n), None)
}

/// `o.importExpr(R3.x)`.
fn import_expr(id: R3) -> Expr {
    o::import_expr(id.reference(), None)
}

/// `o.variable(name)`.
fn var(name: &str) -> Expr {
    o::variable(name, None)
}

/// `if (rf & flags) { .. }` — `renderFlagCheckIfStmt`.
fn render_flag_check_if_stmt(flags: RenderFlags, statements: Vec<Stmt>) -> Stmt {
    o::if_stmt(
        var(RENDER_FLAGS).bitwise_and(number_literal(flags as u32 as f64)),
        statements,
        None,
    )
}

// ---------------------------------------------------------------------------
// getQueryPredicate.
// ---------------------------------------------------------------------------

/// `getQueryPredicate(query, constantPool)` — builds the predicate argument expression.
pub fn get_query_predicate(query: &R3QueryMetadata, constant_pool: &mut ConstantPool) -> Expr {
    match &query.predicate {
        QueryPredicate::Selectors(selectors) => {
            // Each entry may contain comma-separated refs (`'ref, ref1, refN'`); split &
            // trim each token into its own literal.
            let mut predicate: Vec<Expr> = Vec::new();
            for selector in selectors {
                for token in selector.split(',') {
                    predicate.push(o::literal(
                        LiteralValue::String(token.trim().to_string()),
                        None,
                    ));
                }
            }
            constant_pool.get_const_literal(o::literal_arr(predicate, None), true)
        }
        QueryPredicate::Expression(maybe_fwd) => match maybe_fwd.forward_ref {
            ForwardRefHandling::None | ForwardRefHandling::Unwrapped => {
                maybe_fwd.expression.clone()
            }
            ForwardRefHandling::Wrapped => import_expr(R3::ResolveForwardRef)
                .call_fn(vec![maybe_fwd.expression.clone()], false),
        },
    }
}

// ---------------------------------------------------------------------------
// getQueryCreateParameters.
// ---------------------------------------------------------------------------

/// `getQueryCreateParameters(query, constantPool, prependParams?)` — builds the argument list
/// for the create-phase runtime call.
fn get_query_create_parameters(
    query: &R3QueryMetadata,
    constant_pool: &mut ConstantPool,
    prepend_params: Option<Vec<Expr>>,
) -> Vec<Expr> {
    let mut parameters: Vec<Expr> = Vec::new();
    if let Some(prepend) = prepend_params {
        parameters.extend(prepend);
    }
    if query.is_signal {
        // `ctx.<propertyName>` (ReadPropExpr) so the runtime can write into the signal field.
        parameters.push(var(CONTEXT_NAME).prop(query.property_name.clone()));
    }
    parameters.push(get_query_predicate(query, constant_pool));
    parameters.push(number_literal(to_query_flags(query).bits() as f64));
    if let Some(read) = &query.read {
        parameters.push(read.clone());
    }
    parameters
}

// ---------------------------------------------------------------------------
// collapseAdvanceStatements.
// ---------------------------------------------------------------------------

/// Heterogeneous update-statement element: a real statement or the `queryAdvancePlaceholder`
/// sentinel (modelled as an explicit enum rather than a JS `Symbol`).
#[derive(Debug, Clone)]
enum UpdateStmt {
    Stmt(Stmt),
    QueryAdvance,
}

/// `collapseAdvanceStatements(statements)` — coalesces consecutive query-advance placeholders
/// into a single `ɵɵqueryAdvance(count)` call (count omitted when 1).
///
/// Faithful to the source: iterate in reverse, accumulate the advance count, flush on hitting a
/// real statement (and once more at the end), and `unshift` to preserve original order.
fn collapse_advance_statements(statements: Vec<UpdateStmt>) -> Vec<Stmt> {
    let mut result: std::collections::VecDeque<Stmt> = std::collections::VecDeque::new();
    let mut advance_collapse_count: u32 = 0;

    let flush = |count: &mut u32, result: &mut std::collections::VecDeque<Stmt>| {
        if *count > 0 {
            let args = if *count == 1 {
                vec![]
            } else {
                vec![number_literal(*count as f64)]
            };
            result.push_front(import_expr(R3::QueryAdvance).call_fn(args, false).to_stmt());
            *count = 0;
        }
    };

    for st in statements.into_iter().rev() {
        match st {
            UpdateStmt::QueryAdvance => advance_collapse_count += 1,
            UpdateStmt::Stmt(stmt) => {
                flush(&mut advance_collapse_count, &mut result);
                result.push_front(stmt);
            }
        }
    }
    flush(&mut advance_collapse_count, &mut result);

    result.into()
}

// ---------------------------------------------------------------------------
// Shared query-function builder.
// ---------------------------------------------------------------------------

/// The runtime-instruction differences between view and content queries.
struct QueryKind {
    /// Signal-query create instruction (`ɵɵviewQuerySignal` / `ɵɵcontentQuerySignal`).
    signal: R3,
    /// Legacy-query create instruction (`ɵɵviewQuery` / `ɵɵcontentQuery`).
    legacy: R3,
}

/// Builds either the view-query or content-query function. The only deltas between the two are
/// captured by `kind`, `prepend_params`, the extra `dirIndex` fn-param, and the name suffix.
fn create_queries_function(
    queries: &[R3QueryMetadata],
    constant_pool: &mut ConstantPool,
    kind: &QueryKind,
    prepend_params: Option<Vec<Expr>>,
    extra_params: Vec<FnParam>,
    name_suffix: &str,
    name: Option<&str>,
) -> Expr {
    let mut create_statements: Vec<Stmt> = Vec::new();
    let mut update_statements: Vec<UpdateStmt> = Vec::new();
    // `temporaryAllocator` is lazy: it declares `let _t;` on first use. Track whether the
    // declaration has been pushed.
    let mut temp_declared = false;

    let mut signal_call: Option<Expr> = None;
    let mut legacy_call: Option<Expr> = None;

    for query in queries {
        // creation call params, e.g. (predicate, flags) or (ctx.prop, predicate, flags).
        let params =
            get_query_create_parameters(query, constant_pool, prepend_params.clone());

        if query.is_signal {
            let base = signal_call.take().unwrap_or_else(|| import_expr(kind.signal));
            signal_call = Some(base.call_fn(params, false));
        } else {
            let base = legacy_call.take().unwrap_or_else(|| import_expr(kind.legacy));
            legacy_call = Some(base.call_fn(params, false));
        }

        // Signal queries update lazily and we just advance the index.
        if query.is_signal {
            update_statements.push(UpdateStmt::QueryAdvance);
            continue;
        }

        // Lazily declare the temporary `let _t;`.
        if !temp_declared {
            temp_declared = true;
            update_statements.push(UpdateStmt::Stmt(Stmt::bare(StmtKind::DeclareVar {
                name: TEMPORARY_NAME.to_string(),
                value: None,
                ty: None,
            })));
        }

        // update, e.g. (ɵɵqueryRefresh(_t = ɵɵloadQuery()) && (ctx.someDir = _t[.first]));
        let temporary = var(TEMPORARY_NAME);
        let get_query_list = import_expr(R3::LoadQuery).call_fn(vec![], false);
        let refresh = import_expr(R3::QueryRefresh)
            .call_fn(vec![temporary.clone().set(get_query_list)], false);
        let assigned = if query.first {
            temporary.prop("first")
        } else {
            temporary
        };
        let update_directive = var(CONTEXT_NAME)
            .prop(query.property_name.clone())
            .set(assigned);
        update_statements.push(UpdateStmt::Stmt(refresh.and(update_directive).to_stmt()));
    }

    // Signal calls are emitted BEFORE legacy calls.
    if let Some(call) = signal_call {
        create_statements.push(call.to_stmt());
    }
    if let Some(call) = legacy_call {
        create_statements.push(call.to_stmt());
    }

    let fn_name = name.map(|n| format!("{n}{name_suffix}"));

    let mut params = vec![
        FnParam::new(RENDER_FLAGS, Some(o::number_type())),
        FnParam::new(CONTEXT_NAME, Some(o::dynamic_type())),
    ];
    params.extend(extra_params);

    o::fn_(
        params,
        vec![
            render_flag_check_if_stmt(RenderFlags::Create, create_statements),
            render_flag_check_if_stmt(
                RenderFlags::Update,
                collapse_advance_statements(update_statements),
            ),
        ],
        Some(o::inferred_type()),
        fn_name,
    )
}

// ---------------------------------------------------------------------------
// Public entry points.
// ---------------------------------------------------------------------------

/// `createViewQueriesFunction(viewQueries, constantPool, name?)` — define and update any view
/// queries. Returns a `FunctionExpr` (`(rf, ctx) => { ... }`).
pub fn create_view_queries_function(
    view_queries: &[R3QueryMetadata],
    constant_pool: &mut ConstantPool,
    name: Option<&str>,
) -> Expr {
    create_queries_function(
        view_queries,
        constant_pool,
        &QueryKind {
            signal: R3::ViewQuerySignal,
            legacy: R3::ViewQuery,
        },
        None,
        vec![],
        "_Query",
        name,
    )
}

/// `createContentQueriesFunction(queries, constantPool, name?)` — define and update any content
/// queries. Returns a `FunctionExpr` (`(rf, ctx, dirIndex) => { ... }`). Prepends `dirIndex` to
/// each create call and adds the `dirIndex` fn-param.
pub fn create_content_queries_function(
    queries: &[R3QueryMetadata],
    constant_pool: &mut ConstantPool,
    name: Option<&str>,
) -> Expr {
    create_queries_function(
        queries,
        constant_pool,
        &QueryKind {
            signal: R3::ContentQuerySignal,
            legacy: R3::ContentQuery,
        },
        Some(vec![var("dirIndex")]),
        vec![FnParam::new("dirIndex", Some(o::number_type()))],
        "_ContentQueries",
        name,
    )
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::emitter::emit_expression;
    use crate::output_ast::ExprKind;

    fn pull_fn(e: &Expr) -> (&Option<String>, &Vec<FnParam>, &Vec<Stmt>) {
        match &e.kind {
            ExprKind::Function {
                name,
                params,
                statements,
            } => (name, params, statements),
            other => panic!("expected FunctionExpr, got {other:?}"),
        }
    }

    /// Extract the statement list inside `if (rf & flags) { .. }`.
    fn if_body(stmt: &Stmt) -> &Vec<Stmt> {
        match &stmt.kind {
            StmtKind::If { true_case, .. } => true_case,
            other => panic!("expected IfStmt, got {other:?}"),
        }
    }

    fn signal_view_child() -> R3QueryMetadata {
        R3QueryMetadata {
            property_name: "myRef".to_string(),
            first: true,
            predicate: QueryPredicate::Expression(MaybeForwardRefExpression {
                expression: o::variable("SomeDir", None),
                forward_ref: ForwardRefHandling::None,
            }),
            descendants: true,
            emit_distinct_changes_only: true,
            read: None,
            is_static: false,
            is_signal: true,
        }
    }

    #[test]
    fn view_child_signal_function_structure() {
        let mut pool = ConstantPool::new();
        let f = create_view_queries_function(&[signal_view_child()], &mut pool, Some("MyCmp"));

        let (name, params, body) = pull_fn(&f);
        assert_eq!(name.as_deref(), Some("MyCmp_Query"));
        // (rf, ctx) — two params, no dirIndex.
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].name, "rf");
        assert_eq!(params[1].name, "ctx");

        // Two top-level statements: create-phase if, update-phase if.
        assert_eq!(body.len(), 2);

        // Create phase: single ɵɵviewQuerySignal(ctx.myRef, SomeDir, <flags>) statement.
        let create = if_body(&body[0]);
        assert_eq!(create.len(), 1);
        let call = match &create[0].kind {
            StmtKind::Expression(e) => e,
            other => panic!("expected expr stmt, got {other:?}"),
        };
        let (callee, args) = match &call.kind {
            ExprKind::Invoke { callee, args, .. } => (callee, args),
            other => panic!("expected invoke, got {other:?}"),
        };
        assert!(matches!(callee.kind, ExprKind::External { .. }));
        // ctx.myRef, SomeDir (predicate), flags. No read.
        assert_eq!(args.len(), 3);
        // First arg is ctx.myRef (ReadProp on ctx).
        match &args[0].kind {
            ExprKind::ReadProp { name, .. } => assert_eq!(name, "myRef"),
            other => panic!("expected ctx.myRef, got {other:?}"),
        }
        // Flags literal = descendants(1) | emitDistinctChangesOnly(4) = 5.
        match &args[2].kind {
            ExprKind::Literal(LiteralValue::Number(n)) => assert_eq!(*n, 5.0),
            other => panic!("expected flags literal, got {other:?}"),
        }

        // Update phase: single ɵɵqueryAdvance() (no args, count == 1).
        let update = if_body(&body[1]);
        assert_eq!(update.len(), 1);
        let adv = match &update[0].kind {
            StmtKind::Expression(e) => e,
            other => panic!("expected expr stmt, got {other:?}"),
        };
        match &adv.kind {
            ExprKind::Invoke { args, .. } => assert_eq!(args.len(), 0),
            other => panic!("expected invoke, got {other:?}"),
        }

        // Sanity: it emits something resembling the instructions.
        let js = emit_expression(&f);
        assert!(js.contains("viewQuerySignal"), "got: {js}");
        assert!(js.contains("queryAdvance"), "got: {js}");
    }

    #[test]
    fn content_children_signal_function_structure() {
        let query = R3QueryMetadata {
            property_name: "items".to_string(),
            first: false,
            predicate: QueryPredicate::Expression(MaybeForwardRefExpression {
                expression: o::variable("SomeDir", None),
                forward_ref: ForwardRefHandling::None,
            }),
            descendants: false,
            emit_distinct_changes_only: true,
            read: None,
            is_static: false,
            is_signal: true,
        };
        let mut pool = ConstantPool::new();
        let f = create_content_queries_function(&[query], &mut pool, Some("MyCmp"));

        let (name, params, body) = pull_fn(&f);
        assert_eq!(name.as_deref(), Some("MyCmp_ContentQueries"));
        // (rf, ctx, dirIndex) — three params.
        assert_eq!(params.len(), 3);
        assert_eq!(params[2].name, "dirIndex");

        // Create phase: ɵɵcontentQuerySignal(dirIndex, ctx.items, SomeDir, <flags>).
        let create = if_body(&body[0]);
        assert_eq!(create.len(), 1);
        let call = match &create[0].kind {
            StmtKind::Expression(e) => e,
            other => panic!("expected expr stmt, got {other:?}"),
        };
        let args = match &call.kind {
            ExprKind::Invoke { args, .. } => args,
            other => panic!("expected invoke, got {other:?}"),
        };
        // dirIndex, ctx.items, predicate, flags = 4 args.
        assert_eq!(args.len(), 4);
        // arg0 is dirIndex variable.
        match &args[0].kind {
            ExprKind::ReadVar { name } => assert_eq!(name, "dirIndex"),
            other => panic!("expected dirIndex, got {other:?}"),
        }
        // arg1 is ctx.items.
        match &args[1].kind {
            ExprKind::ReadProp { name, .. } => assert_eq!(name, "items"),
            other => panic!("expected ctx.items, got {other:?}"),
        }
        // Flags = emitDistinctChangesOnly only = 4.
        match &args[3].kind {
            ExprKind::Literal(LiteralValue::Number(n)) => assert_eq!(*n, 4.0),
            other => panic!("expected flags literal, got {other:?}"),
        }

        // Update phase still emits a single queryAdvance().
        let update = if_body(&body[1]);
        assert_eq!(update.len(), 1);

        let js = emit_expression(&f);
        assert!(js.contains("contentQuerySignal"), "got: {js}");
    }

    #[test]
    fn legacy_view_query_update_phase() {
        let query = R3QueryMetadata {
            property_name: "myRef".to_string(),
            first: true,
            predicate: QueryPredicate::Selectors(vec!["ref, ref1".to_string()]),
            descendants: false,
            emit_distinct_changes_only: false,
            read: None,
            is_static: false,
            is_signal: false,
        };
        let mut pool = ConstantPool::new();
        let f = create_view_queries_function(&[query], &mut pool, Some("MyCmp"));
        let (_, _, body) = pull_fn(&f);

        // Create phase: single ɵɵviewQuery(predicate, flags) statement.
        let create = if_body(&body[0]);
        assert_eq!(create.len(), 1);
        let call = match &create[0].kind {
            StmtKind::Expression(e) => e,
            other => panic!("expected expr stmt, got {other:?}"),
        };
        let args = match &call.kind {
            ExprKind::Invoke { args, .. } => args,
            other => panic!("expected invoke, got {other:?}"),
        };
        // predicate (literal array), flags = 2 args (no ctx.prop for legacy).
        assert_eq!(args.len(), 2);
        // Predicate is a literal array of two trimmed selectors: "ref", "ref1".
        match &args[0].kind {
            ExprKind::LiteralArray(entries) => {
                assert_eq!(entries.len(), 2);
                match (&entries[0].kind, &entries[1].kind) {
                    (
                        ExprKind::Literal(LiteralValue::String(a)),
                        ExprKind::Literal(LiteralValue::String(b)),
                    ) => {
                        assert_eq!(a, "ref");
                        assert_eq!(b, "ref1");
                    }
                    other => panic!("expected two string literals, got {other:?}"),
                }
            }
            other => panic!("expected literal array predicate, got {other:?}"),
        }

        // Update phase: `let _t;` then the refresh && assign statement.
        let update = if_body(&body[1]);
        assert_eq!(update.len(), 2);
        assert!(matches!(update[0].kind, StmtKind::DeclareVar { .. }));
        let js = emit_expression(&f);
        assert!(js.contains("queryRefresh"), "got: {js}");
        assert!(js.contains("loadQuery"), "got: {js}");
        assert!(js.contains(".first"), "got: {js}");
    }

    #[test]
    fn multi_signal_advance_collapses() {
        let mut q = signal_view_child();
        q.first = false;
        let queries = vec![q.clone(), q.clone()];
        let mut pool = ConstantPool::new();
        let f = create_view_queries_function(&queries, &mut pool, Some("MyCmp"));
        let (_, _, body) = pull_fn(&f);

        // Update phase collapses two advances into a single ɵɵqueryAdvance(2).
        let update = if_body(&body[1]);
        assert_eq!(update.len(), 1);
        let adv = match &update[0].kind {
            StmtKind::Expression(e) => e,
            other => panic!("expected expr stmt, got {other:?}"),
        };
        match &adv.kind {
            ExprKind::Invoke { args, .. } => {
                assert_eq!(args.len(), 1);
                match &args[0].kind {
                    ExprKind::Literal(LiteralValue::Number(n)) => assert_eq!(*n, 2.0),
                    other => panic!("expected count literal, got {other:?}"),
                }
            }
            other => panic!("expected invoke, got {other:?}"),
        }

        // Create phase: signal calls chain fluently: ɵɵviewQuerySignal(...)(...).
        let create = if_body(&body[0]);
        assert_eq!(create.len(), 1);
        let outer = match &create[0].kind {
            StmtKind::Expression(e) => e,
            other => panic!("expected expr stmt, got {other:?}"),
        };
        // outer is Invoke whose callee is itself an Invoke (chained call).
        match &outer.kind {
            ExprKind::Invoke { callee, .. } => {
                assert!(matches!(callee.kind, ExprKind::Invoke { .. }), "expected chained call");
            }
            other => panic!("expected invoke, got {other:?}"),
        }
    }
}
