//! The `oxc` PARSE backend — the ONLY facade file permitted to `use oxc_`.
//!
//! Owns an `oxc_allocator::Allocator` for the lifetime of [`OxcParseBackend::parse_module`]'s
//! callback, parses the source with `oxc_parser::Parser`, and exposes the parsed
//! `oxc_ast::Program` through [`OxcModule`] so the front-end's existing metadata walk runs against
//! the live AST UNCHANGED (and therefore byte-identical). It additionally PRE-LOWERS the
//! object-literal / decorator / `ɵɵngDeclare*` surface into the engine-neutral
//! [`super::ParseOutput`] structs in SOURCE order, so the parts of the walk that read structurally
//! can do so without naming an `oxc_` type.
//!
//! Pre-lowering is faithful to the historical helpers it replaces (`key_name`, `decorator_name`,
//! `decorator_object`, `string_value`, `string_array_value`): only static (identifier / string)
//! object keys are captured; spreads / computed keys are dropped; numeric literals keep their parsed
//! `f64`; a no-substitution template literal reads as its cooked string.

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, ArrayExpressionElement, Class, ClassElement, Decorator, Expression,
    ExportDefaultDeclarationKind, FormalParameters, ImportOrExportKind, MethodDefinitionKind,
    ObjectPropertyKind, Program, PropertyKey, Statement, TSType, TSTypeName,
};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType};

use super::{
    ClassWithDecorators, DecoratorInfo, ImportInfo, LitValue, MemberInfo, MemberKind, NArg,
    NArrayElement, NArrowBody, NAssignment, NCtorParam, NExpr, NObjectProp, NParam, NStmt, NTopStmt,
    NTypeRef, NVarDeclarator, NgDeclareCall, ObjLit, ParseBackend, ParseOutput, SourceKind,
    TreatySpan,
};

/// The oxc parse backend. Zero-sized; the parse arena is created per [`Self::parse_module`] call so
/// each parse is independent (no shared mutable state — mirrors the historical per-call
/// `Allocator::default()`).
#[derive(Debug, Default, Clone, Copy)]
pub struct OxcParseBackend;

/// The borrowed parsed-module handle the [`ParseBackend::parse_module`] callback receives. Wraps the
/// arena-allocated `oxc_ast::Program` and the pre-lowered engine-neutral [`ParseOutput`].
///
/// The front-end reads `summary` for the structural surface and `program` for the live-AST walk that
/// genuinely needs oxc nodes (arbitrary `Expression` → `output_ast` conversion, ctor-dep extraction,
/// surgical span rewrites). Both view the SAME parse.
pub struct OxcModule<'a> {
    /// The parsed program, arena-allocated; valid for the callback's lifetime.
    pub program: Program<'a>,
    /// The engine-neutral pre-lowered summary (classes / `ɵɵngDeclare*` calls / errors).
    pub summary: ParseOutput,
}

impl OxcModule<'_> {
    /// The parsed program (live oxc AST) — the escape hatch for the parts of the walk that still need
    /// oxc nodes. Named explicitly so call sites document that they reach past the neutral surface.
    pub fn program(&self) -> &Program<'_> {
        &self.program
    }

    /// The engine-neutral pre-lowered summary.
    pub fn summary(&self) -> &ParseOutput {
        &self.summary
    }
}

impl ParseBackend for OxcParseBackend {
    type Module<'a> = OxcModule<'a>;

    fn parse_module<'src, R>(
        &self,
        source: &'src str,
        kind: SourceKind<'_>,
        f: impl FnOnce(&Self::Module<'_>) -> R,
    ) -> R {
        let allocator = Allocator::default();
        let source_type = source_type_for(kind);
        let ret = Parser::new(&allocator, source, source_type).parse();

        let summary = if ret.errors.is_empty() {
            lower_program(&ret.program, source)
        } else {
            ParseOutput {
                classes: Vec::new(),
                ng_declare_calls: Vec::new(),
                imports: Vec::new(),
                top_level: Vec::new(),
                errors: ret.errors.iter().map(|e| e.to_string()).collect(),
            }
        };

        let module = OxcModule {
            program: ret.program,
            summary,
        };
        f(&module)
    }

    fn span_text<'src>(&self, source: &'src str, span: TreatySpan) -> &'src str {
        &source[span.start as usize..span.end as usize]
    }
}

/// Map a [`SourceKind`] onto the oxc `SourceType` the historical call site used.
fn source_type_for(kind: SourceKind<'_>) -> SourceType {
    match kind {
        SourceKind::TypeScriptModule => SourceType::default().with_typescript(true),
        SourceKind::TypeScriptEsModule => {
            SourceType::default().with_typescript(true).with_module(true)
        }
        SourceKind::ByFilename(filename) => {
            // Mirrors the linker's `source_type_for`: TypeScript by extension, always a module.
            let lower = filename.to_ascii_lowercase();
            let ts = lower.ends_with(".ts")
                || lower.ends_with(".mts")
                || lower.ends_with(".cts")
                || lower.ends_with(".tsx");
            SourceType::default().with_typescript(ts).with_module(true)
        }
    }
}

// ---------------------------------------------------------------------------
// Pre-lowering: oxc AST -> engine-neutral ParseOutput, in SOURCE order.
// ---------------------------------------------------------------------------

/// An engine-neutral `TreatySpan` from any spanned oxc node.
fn span_of<T: GetSpan>(node: &T) -> TreatySpan {
    let s = node.span();
    TreatySpan::new(s.start, s.end)
}

/// Lower a parsed program into the neutral [`ParseOutput`] (decorated classes + `ɵɵngDeclare*` calls,
/// source order).
fn lower_program(program: &Program, source: &str) -> ParseOutput {
    let mut classes = Vec::new();
    let mut ng_declare_calls = Vec::new();
    let mut imports = Vec::new();
    let mut top_level = Vec::new();

    for stmt in &program.body {
        if let Some(class) = statement_class(stmt) {
            if !class.decorators.is_empty() {
                classes.push(lower_class(class, span_of(stmt)));
            }
            collect_ng_declares_in_class(class, &mut ng_declare_calls);
        }
        // `ɵɵngDeclare*` can also sit as a free top-level statement (e.g. a re-parsed slice or a
        // non-class declaration shape); cover the expression-statement form too.
        collect_ng_declares_in_stmt(stmt, &mut ng_declare_calls);
        // Top-level `import` bindings (foreign-import / imported-name surface).
        collect_imports_in_stmt(stmt, &mut imports);
        // The neutral top-level statement surface the AOT→partial emitter walks.
        top_level.push(lower_top_stmt(stmt));
    }

    let _ = source; // span_text recovers source text on demand; not needed during lowering.
    ParseOutput {
        classes,
        ng_declare_calls,
        imports,
        top_level,
        errors: Vec::new(),
    }
}

/// Lower one TOP-LEVEL statement to the neutral [`NTopStmt`] — the surface the AOT→partial emitter
/// (`partial_emit::collect_rewrites`) walks. An `X.member = rhs;` assignment expression statement
/// becomes [`NTopStmt::Assignment`] (the definition scaffold); a bare expression statement becomes
/// [`NTopStmt::ExprStmt`] (the `ɵɵsetNgModuleScope` side effect); a `var`/`let`/`const` becomes
/// [`NTopStmt::VarDecl`]; everything else carries its span as [`NTopStmt::Other`].
fn lower_top_stmt(stmt: &Statement) -> NTopStmt {
    match stmt {
        Statement::ExpressionStatement(es) => {
            if let Expression::AssignmentExpression(assign) = &es.expression {
                let (target_object, target_member) = assignment_member(assign);
                return NTopStmt::Assignment(NAssignment {
                    target_object,
                    target_member,
                    value: lower_expr(&assign.right),
                    value_span: span_of(&assign.right),
                    span: span_of(stmt),
                });
            }
            NTopStmt::ExprStmt {
                expr: lower_expr(&es.expression),
                span: span_of(stmt),
            }
        }
        Statement::VariableDeclaration(decl) => {
            let is_const = matches!(decl.kind, oxc_ast::ast::VariableDeclarationKind::Const);
            let decls = decl
                .declarations
                .iter()
                .map(|d| NVarDeclarator {
                    name: d.id.get_binding_identifier().map(|id| id.name.to_string()),
                    init: d.init.as_ref().map(lower_expr),
                })
                .collect();
            NTopStmt::VarDecl {
                is_const,
                decls,
                span: span_of(stmt),
            }
        }
        // A top-level `function f(): RetType {…}` declaration — bare or `export function …`. Surfaced
        // so `collect_module_with_providers_returns` can resolve a `ModuleWithProviders<T>` return type
        // neutrally. Mirrors that helper's own bare / `ExportNamedDeclaration` function-extraction.
        _ => {
            if let Some(func) = statement_function(stmt) {
                return NTopStmt::FnDecl {
                    name: func.id.as_ref().map(|id| id.name.to_string()),
                    return_type: func
                        .return_type
                        .as_ref()
                        .and_then(|ann| lower_type_ref(&ann.type_annotation)),
                    span: span_of(stmt),
                };
            }
            NTopStmt::Other(span_of(stmt))
        }
    }
}

/// The function declared (directly or via `export`) by a top-level statement, or `None`. Mirrors
/// `source_compile::collect_module_with_providers_returns`'s bare / `ExportNamedDeclaration`
/// function-declaration extraction (an `export default function` is intentionally NOT a
/// module-with-providers factory and stays `Other`, matching that helper).
fn statement_function<'a>(stmt: &'a Statement<'a>) -> Option<&'a oxc_ast::ast::Function<'a>> {
    match stmt {
        Statement::FunctionDeclaration(f) => Some(f.as_ref()),
        Statement::ExportNamedDeclaration(export) => match &export.declaration {
            Some(oxc_ast::ast::Declaration::FunctionDeclaration(f)) => Some(f.as_ref()),
            _ => None,
        },
        _ => None,
    }
}

/// The `<Ident>.<member>` LHS parts of an assignment target (`X.ɵfac = …` → `(Some("X"),
/// Some("ɵfac"))`), or `(None, None)` when the target is not a static-member-on-identifier. Mirrors
/// `partial_emit::assignment_member`.
fn assignment_member(assign: &oxc_ast::ast::AssignmentExpression) -> (Option<String>, Option<String>) {
    use oxc_ast::ast::AssignmentTarget;
    if let AssignmentTarget::StaticMemberExpression(member) = &assign.left {
        if let Expression::Identifier(obj) = &member.object {
            return (
                Some(obj.name.to_string()),
                Some(member.property.name.to_string()),
            );
        }
    }
    (None, None)
}

/// Collect the local binding names of a top-level `import` declaration (skipping whole-declaration and
/// inline `type`-only specifiers), mirroring `source_compile::collect_imported_names`.
fn collect_imports_in_stmt(stmt: &Statement, out: &mut Vec<ImportInfo>) {
    let Statement::ImportDeclaration(import) = stmt else {
        return;
    };
    let type_only_decl = import.import_kind == ImportOrExportKind::Type;
    let Some(specifiers) = &import.specifiers else {
        return;
    };
    for spec in specifiers {
        use oxc_ast::ast::ImportDeclarationSpecifier as Spec;
        let (local, inline_type) = match spec {
            Spec::ImportSpecifier(s) => {
                (s.local.name.to_string(), s.import_kind == ImportOrExportKind::Type)
            }
            Spec::ImportDefaultSpecifier(s) => (s.local.name.to_string(), false),
            Spec::ImportNamespaceSpecifier(s) => (s.local.name.to_string(), false),
        };
        out.push(ImportInfo {
            local_name: local,
            type_only: type_only_decl || inline_type,
        });
    }
}

/// The class declared (directly or via `export` / `export default`) by a top-level statement.
fn statement_class<'a>(stmt: &'a Statement<'a>) -> Option<&'a Class<'a>> {
    match stmt {
        Statement::ClassDeclaration(c) => Some(c.as_ref()),
        Statement::ExportNamedDeclaration(export) => match &export.declaration {
            Some(oxc_ast::ast::Declaration::ClassDeclaration(c)) => Some(c.as_ref()),
            _ => None,
        },
        Statement::ExportDefaultDeclaration(export) => {
            if let ExportDefaultDeclarationKind::ClassDeclaration(c) = &export.declaration {
                Some(c.as_ref())
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Lower a class to the neutral [`ClassWithDecorators`] (name + decorators + members, source order).
///
/// `pub(crate)` so the AOT driver (`crate::source_compile`) can build the engine-neutral
/// `treaty_ivy_decorators::ClassMeta` fields from the SAME live oxc class it walks, without re-deriving
/// the pre-lowering (and so the neutral fields it hands a plugin are byte-identical to the parse
/// backend's `ParseOutput` ones).
pub(crate) fn lower_class(class: &Class, stmt_span: TreatySpan) -> ClassWithDecorators {
    let name = class.id.as_ref().map(|id| id.name.to_string());
    // The class NAME identifier span (`class X` → the `X` token) — the additive source map's anchor.
    let name_span = class
        .id
        .as_ref()
        .map(|id| TreatySpan::new(id.span.start, id.span.end))
        .unwrap_or_default();
    let decorators = class.decorators.iter().map(lower_decorator).collect();
    let members = class
        .body
        .body
        .iter()
        .map(lower_member)
        .collect::<Vec<_>>();
    ClassWithDecorators {
        name,
        name_span,
        decorators,
        members,
        span: span_of(class),
        stmt_span,
    }
}

/// Lower a class member (property / accessor / method / constructor) to the neutral [`MemberInfo`]
/// — name + own decorators + kind + static flag + params (for the constructor / methods, with their
/// own decorators) + initializer (for properties / accessors).
fn lower_member(element: &ClassElement) -> MemberInfo {
    match element {
        ClassElement::PropertyDefinition(p) => MemberInfo {
            name: key_name(&p.key).map(str::to_string),
            decorators: p.decorators.iter().map(lower_decorator).collect(),
            kind: MemberKind::Property,
            is_static: p.r#static,
            params: Vec::new(),
            initializer: p.value.as_ref().map(lower_expr),
            has_body: false,
            span: span_of(element),
        },
        ClassElement::MethodDefinition(m) => {
            let (kind, is_ctor) = match m.kind {
                MethodDefinitionKind::Constructor => (MemberKind::Constructor, true),
                MethodDefinitionKind::Get => (MemberKind::Getter, false),
                MethodDefinitionKind::Set => (MemberKind::Setter, false),
                MethodDefinitionKind::Method => (MemberKind::Method, false),
            };
            let _ = is_ctor;
            MemberInfo {
                name: key_name(&m.key).map(str::to_string),
                decorators: m.decorators.iter().map(lower_decorator).collect(),
                kind,
                is_static: m.r#static,
                params: lower_ctor_params(&m.value.params),
                initializer: None,
                // A bodiless TS overload signature has `value.body == None`; the implementation has a
                // body. Constructor-dependency extraction prefers the body-bearing constructor.
                has_body: m.value.body.is_some(),
                span: span_of(element),
            }
        }
        ClassElement::AccessorProperty(a) => MemberInfo {
            name: key_name(&a.key).map(str::to_string),
            decorators: a.decorators.iter().map(lower_decorator).collect(),
            kind: MemberKind::Accessor,
            is_static: a.r#static,
            params: Vec::new(),
            initializer: a.value.as_ref().map(lower_expr),
            has_body: false,
            span: span_of(element),
        },
        // Static block / TS index signature / etc.: oxc still surfaces a (nameless) class element.
        _ => MemberInfo {
            kind: MemberKind::Other,
            span: span_of(element),
            ..MemberInfo::default()
        },
    }
}

/// Lower formal parameters carrying their own decorators (the constructor / method param surface that
/// drives constructor-dependency extraction). A `...rest` parameter (which oxc stores OUT of `items`,
/// in `params.rest`) is appended last with `is_rest: true` so the neutral list matches swc, which
/// keeps the rest inline as a `Pat::Rest`.
fn lower_ctor_params(params: &FormalParameters) -> Vec<NCtorParam> {
    let mut out: Vec<NCtorParam> = params
        .items
        .iter()
        .map(|item| NCtorParam {
            name: item
                .pattern
                .get_binding_identifier()
                .map(|id| id.name.to_string()),
            decorators: item.decorators.iter().map(lower_decorator).collect(),
            is_rest: false,
            type_ref: item
                .type_annotation
                .as_ref()
                .and_then(|ann| lower_type_ref(&ann.type_annotation)),
        })
        .collect();
    if let Some(rest) = &params.rest {
        out.push(NCtorParam {
            name: rest
                .rest
                .argument
                .get_binding_identifier()
                .map(|id| id.name.to_string()),
            decorators: rest.decorators.iter().map(lower_decorator).collect(),
            is_rest: true,
            // A `...rest` parameter is never a DI token (ngtsc's `getConstructorDependencies` reads only
            // the fixed leading params), and oxc 0.133 surfaces no annotation on the
            // `BindingRestElement` (the annotation rides on the inner pattern, which the rest walk does
            // not descend). Carry `None` on both backends so the neutral list stays byte-identical.
            type_ref: None,
        });
    }
    out
}

/// Lower a TS `TSType` to the neutral [`NTypeRef`] — `Some` ONLY for a type REFERENCE (`Foo` /
/// `ns.Foo` / `ModuleWithProviders<T>`), `None` for every other type form (keyword / union / literal /
/// function / array / …). Faithful to `source_compile::type_token_expr` (which only produces a token
/// for a `TSTypeReference`) and `module_with_providers_type_arg` (which only matches a type reference).
fn lower_type_ref(ty: &TSType) -> Option<NTypeRef> {
    let TSType::TSTypeReference(reference) = ty else {
        return None;
    };
    Some(NTypeRef {
        name_path: type_name_path(&reference.type_name),
        // The type arguments (`ModuleWithProviders<T>` → its `T`), keeping ONLY type-reference args
        // (matching `module_with_providers_type_arg`, which drops a non-reference argument).
        type_args: reference
            .type_arguments
            .as_ref()
            .map(|args| args.params.iter().filter_map(lower_type_ref).collect())
            .unwrap_or_default(),
    })
}

/// The dotted NAME segments of a `TSTypeName` (`Foo` → `["Foo"]`; `ns.Foo` → `["ns", "Foo"]`;
/// `a.b.C` → `["a", "b", "C"]`). EMPTY for a `this`-type (no injectable token — matches
/// `type_name_expr`'s `None`). Faithful to `type_name_expr`'s recursive qualified-name walk.
fn type_name_path(name: &TSTypeName) -> Vec<String> {
    match name {
        TSTypeName::IdentifierReference(id) => vec![id.name.to_string()],
        TSTypeName::QualifiedName(q) => {
            let mut path = type_name_path(&q.left);
            path.push(q.right.name.to_string());
            path
        }
        TSTypeName::ThisExpression(_) => Vec::new(),
    }
}

/// Lower a decorator to the neutral [`DecoratorInfo`] (callee name + first object-literal argument +
/// full neutral argument list). `pub(crate)` for the AOT driver (see [`lower_class`]).
pub(crate) fn lower_decorator(dec: &Decorator) -> DecoratorInfo {
    let name = decorator_name(dec).unwrap_or_default().to_string();
    let object = decorator_object(dec).map(lower_object);
    let arguments = decorator_arguments(dec);
    DecoratorInfo {
        name,
        object,
        arguments,
        span: span_of(dec),
    }
}

/// The full neutral argument list of a decorator call `@Foo(a, b, …)` (empty for a bare `@Foo`).
fn decorator_arguments(dec: &Decorator) -> Vec<NArg> {
    if let Expression::CallExpression(call) = &dec.expression {
        lower_args(&call.arguments)
    } else {
        Vec::new()
    }
}

/// Returns the callee identifier name of a decorator's expression — bare `@Foo` or call `@Foo({…})`.
/// Faithful to the historical `decorator_name`.
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

/// The first object-literal argument of a decorator call `@Foo({…})`. Faithful to the historical
/// `decorator_object`.
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

/// A property key's static name (identifier or string literal). Faithful to the historical
/// `key_name`.
fn key_name<'a>(key: &'a PropertyKey<'a>) -> Option<&'a str> {
    match key {
        PropertyKey::StaticIdentifier(id) => Some(id.name.as_str()),
        PropertyKey::StringLiteral(s) => Some(s.value.as_str()),
        _ => None,
    }
}

/// Lower an oxc `ObjectExpression` to the neutral [`ObjLit`] — static-keyed properties in SOURCE
/// order. `pub(crate)` for the AOT driver (see [`lower_class`]).
pub(crate) fn lower_object(obj: &oxc_ast::ast::ObjectExpression) -> ObjLit {
    let mut props = Vec::with_capacity(obj.properties.len());
    for p in &obj.properties {
        if let ObjectPropertyKind::ObjectProperty(op) = p {
            if let Some(key) = key_name(&op.key) {
                props.push((key.to_string(), lower_value(&op.value)));
            }
        }
    }
    ObjLit {
        props,
        span: span_of(obj),
        // The lossless full-expression view of the SAME literal (every property, full `NExpr` values).
        nprops: lower_object_props(obj),
    }
}

/// Lower an oxc `ObjectExpression`'s properties to the LOSSLESS [`NObjectProp`] list (every property
/// in source order, key/value as full `NExpr`, spreads + computed keys preserved). Shared by
/// [`lower_object`] (the `ObjLit::nprops` channel) and the `NExpr::Object` arm of [`lower_expr`].
fn lower_object_props(obj: &oxc_ast::ast::ObjectExpression) -> Vec<NObjectProp> {
    obj.properties
        .iter()
        .map(|p| match p {
            ObjectPropertyKind::ObjectProperty(op) => match key_name(&op.key) {
                Some(key) => NObjectProp::KeyValue {
                    key: key.to_string(),
                    value: lower_expr(&op.value),
                    quoted: !is_safe_object_key(key),
                    computed: op.computed,
                    value_span: span_of(&op.value),
                },
                None => NObjectProp::Other(span_of(&op.key)),
            },
            ObjectPropertyKind::SpreadProperty(sp) => NObjectProp::Spread(lower_expr(&sp.argument)),
        })
        .collect()
}

/// Lower an oxc `Expression` to the neutral [`LitValue`]. Faithful to `string_value` /
/// `string_array_value` / the literal subset; anything richer becomes [`LitValue::Other`] carrying
/// its span (so the consumer can re-walk the live AST or recover the source text).
fn lower_value(expr: &Expression) -> LitValue {
    match expr {
        Expression::StringLiteral(s) => LitValue::String(s.value.to_string()),
        Expression::TemplateLiteral(t) if t.expressions.is_empty() && t.quasis.len() == 1 => {
            match t.quasis[0].value.cooked.as_ref() {
                Some(c) => LitValue::String(c.to_string()),
                None => LitValue::Other(span_of(expr)),
            }
        }
        Expression::NumericLiteral(n) => LitValue::Number(n.value),
        Expression::BooleanLiteral(b) => LitValue::Boolean(b.value),
        Expression::NullLiteral(_) => LitValue::Null,
        Expression::Identifier(id) => LitValue::Identifier(id.name.to_string()),
        // `Foo.Bar` member access keeps the trailing property name (matches `encapsulation_value`'s
        // member-or-identifier read). Consumers needing the full path use the live-AST escape hatch.
        Expression::StaticMemberExpression(m) => LitValue::Identifier(m.property.name.to_string()),
        Expression::ArrayExpression(arr) => {
            let mut out = Vec::with_capacity(arr.elements.len());
            for el in &arr.elements {
                match el {
                    ArrayExpressionElement::SpreadElement(_)
                    | ArrayExpressionElement::Elision(_) => out.push(LitValue::Other(el.span().into_treaty())),
                    _ => {
                        if let Some(inner) = el.as_expression() {
                            out.push(lower_value(inner));
                        } else {
                            out.push(LitValue::Other(el.span().into_treaty()));
                        }
                    }
                }
            }
            LitValue::Array(out)
        }
        Expression::ObjectExpression(obj) => LitValue::Object(lower_object(obj)),
        Expression::ParenthesizedExpression(p) => lower_value(&p.expression),
        _ => LitValue::Other(span_of(expr)),
    }
}

/// Local extension turning an `oxc_span::Span` into a [`TreatySpan`] (so array-element span recovery
/// reads cleanly above).
trait IntoTreatySpan {
    fn into_treaty(self) -> TreatySpan;
}
impl IntoTreatySpan for oxc_span::Span {
    fn into_treaty(self) -> TreatySpan {
        TreatySpan::new(self.start, self.end)
    }
}

// ---------------------------------------------------------------------------
// Neutral EXPRESSION / STATEMENT / PARAM lowering (the full walk surface).
//
// Mirrors the union of `source_compile::convert_expr` + `linker::convert_expr` shape-for-shape, but
// NEVER declines: shapes outside the converted subset become `NExpr::Other` / `NStmt::Other` carrying
// a span (the additive neutral tree records everything; the live-AST walk still decides what to emit).
// ---------------------------------------------------------------------------

/// Lower an oxc `Expression` to the neutral [`NExpr`], mirroring the `convert_expr` surface.
fn lower_expr(expr: &Expression) -> NExpr {
    match expr {
        Expression::StringLiteral(s) => NExpr::String(s.value.to_string()),
        Expression::TemplateLiteral(t) if t.expressions.is_empty() && t.quasis.len() == 1 => {
            match t.quasis[0].value.cooked.as_ref() {
                Some(c) => NExpr::String(c.to_string()),
                None => NExpr::Other(span_of(expr)),
            }
        }
        Expression::NumericLiteral(n) => NExpr::Number(n.value),
        Expression::BooleanLiteral(b) => NExpr::Boolean(b.value),
        Expression::NullLiteral(_) => NExpr::Null,
        Expression::Identifier(id) => NExpr::Identifier(id.name.to_string()),
        Expression::StaticMemberExpression(m) => NExpr::Member {
            object: Box::new(lower_expr(&m.object)),
            property: m.property.name.to_string(),
        },
        Expression::ComputedMemberExpression(m) => NExpr::ComputedMember {
            object: Box::new(lower_expr(&m.object)),
            index: Box::new(lower_expr(&m.expression)),
        },
        Expression::CallExpression(call) => NExpr::Call {
            callee: Box::new(lower_expr(&call.callee)),
            args: lower_args(&call.arguments),
        },
        Expression::NewExpression(new_expr) => NExpr::New {
            callee: Box::new(lower_expr(&new_expr.callee)),
            args: lower_args(&new_expr.arguments),
        },
        Expression::ParenthesizedExpression(p) => {
            NExpr::Parenthesized(Box::new(lower_expr(&p.expression)))
        }
        Expression::ConditionalExpression(c) => NExpr::Conditional {
            test: Box::new(lower_expr(&c.test)),
            consequent: Box::new(lower_expr(&c.consequent)),
            alternate: Box::new(lower_expr(&c.alternate)),
        },
        Expression::BinaryExpression(b) => NExpr::Binary {
            op: b.operator.as_str().to_string(),
            left: Box::new(lower_expr(&b.left)),
            right: Box::new(lower_expr(&b.right)),
        },
        Expression::LogicalExpression(l) => NExpr::Binary {
            op: l.operator.as_str().to_string(),
            left: Box::new(lower_expr(&l.left)),
            right: Box::new(lower_expr(&l.right)),
        },
        Expression::UnaryExpression(u) => NExpr::Unary {
            op: u.operator.as_str().to_string(),
            argument: Box::new(lower_expr(&u.argument)),
        },
        Expression::ArrayExpression(arr) => {
            let elems = arr
                .elements
                .iter()
                .map(|el| match el {
                    ArrayExpressionElement::SpreadElement(s) => {
                        NArrayElement::Spread(lower_expr(&s.argument))
                    }
                    ArrayExpressionElement::Elision(_) => NArrayElement::Hole,
                    other => match other.as_expression() {
                        Some(inner) => NArrayElement::Expr(lower_expr(inner)),
                        None => NArrayElement::Expr(NExpr::Other(other.span().into_treaty())),
                    },
                })
                .collect();
            NExpr::Array(elems)
        }
        Expression::ObjectExpression(obj) => NExpr::Object(lower_object_props(obj)),
        Expression::ArrowFunctionExpression(arrow) => NExpr::Arrow {
            params: lower_params(&arrow.params),
            body: Box::new(lower_arrow_body(arrow)),
        },
        Expression::FunctionExpression(func) => NExpr::Function {
            params: lower_params(&func.params),
            body: func
                .body
                .as_ref()
                .map(|b| lower_stmts(&b.statements))
                .unwrap_or_default(),
        },
        _ => NExpr::Other(span_of(expr)),
    }
}

/// Whether an object key is a valid bare JS identifier (so it can be emitted unquoted). Faithful to
/// the `is_safe_object_key` helper the `convert_expr` walks use.
fn is_safe_object_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$' => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        }
        _ => false,
    }
}

/// Lower a call/new argument list, mapping a spread argument to [`NArg::Spread`].
fn lower_args(args: &oxc_allocator::Vec<Argument>) -> Vec<NArg> {
    args.iter()
        .map(|a| match a {
            Argument::SpreadElement(s) => {
                NArg::Spread(lower_expr(&s.argument), span_of(&s.argument))
            }
            other => match other.as_expression() {
                Some(inner) => NArg::Expr(lower_expr(inner), span_of(inner)),
                None => {
                    NArg::Expr(NExpr::Other(other.span().into_treaty()), other.span().into_treaty())
                }
            },
        })
        .collect()
}

/// Lower function/arrow formal parameters to the simple-binding neutral [`NParam`]s (non-identifier
/// bindings carry `name: None` so the consumer declines exactly as the live-AST walk does).
fn lower_params(params: &FormalParameters) -> Vec<NParam> {
    let mut out: Vec<NParam> = params
        .items
        .iter()
        .map(|item| NParam {
            name: item
                .pattern
                .get_binding_identifier()
                .map(|id| id.name.to_string()),
            is_rest: false,
        })
        .collect();
    if let Some(rest) = &params.rest {
        out.push(NParam {
            name: rest
                .rest
                .argument
                .get_binding_identifier()
                .map(|id| id.name.to_string()),
            is_rest: true,
        });
    }
    out
}

/// Lower an arrow body to a neutral [`NArrowBody`]: an expression body (`x => expr`) when the arrow is
/// expression-bodied with a single leading expression statement; otherwise a block body.
fn lower_arrow_body(arrow: &oxc_ast::ast::ArrowFunctionExpression) -> NArrowBody {
    if arrow.expression {
        if let Some(Statement::ExpressionStatement(stmt)) = arrow.body.statements.first() {
            return NArrowBody::Expr(Box::new(lower_expr(&stmt.expression)));
        }
    }
    NArrowBody::Block(lower_stmts(&arrow.body.statements))
}

/// Lower a list of statements to neutral [`NStmt`]s, in source order.
fn lower_stmts(stmts: &oxc_allocator::Vec<Statement>) -> Vec<NStmt> {
    stmts.iter().map(lower_stmt).collect()
}

/// Lower one statement to the neutral [`NStmt`], mirroring the `convert_statement` subset.
fn lower_stmt(stmt: &Statement) -> NStmt {
    match stmt {
        Statement::VariableDeclaration(decl) => {
            let is_const =
                matches!(decl.kind, oxc_ast::ast::VariableDeclarationKind::Const);
            let decls = decl
                .declarations
                .iter()
                .map(|d| NVarDeclarator {
                    name: d.id.get_binding_identifier().map(|id| id.name.to_string()),
                    init: d.init.as_ref().map(lower_expr),
                })
                .collect();
            NStmt::VarDecl { is_const, decls }
        }
        Statement::ExpressionStatement(es) => NStmt::Expr(lower_expr(&es.expression)),
        Statement::ReturnStatement(ret) => NStmt::Return(ret.argument.as_ref().map(lower_expr)),
        Statement::IfStatement(if_stmt) => NStmt::If {
            test: lower_expr(&if_stmt.test),
            consequent: lower_branch(&if_stmt.consequent),
            alternate: if_stmt
                .alternate
                .as_ref()
                .map(lower_branch)
                .unwrap_or_default(),
        },
        Statement::BlockStatement(block) => NStmt::Block(lower_stmts(&block.body)),
        _ => NStmt::Other(span_of(stmt)),
    }
}

/// Lower an `if`/`else` branch — a `{ … }` block's statements, or a one-element list for a bare
/// branch statement (mirrors `convert_branch`).
fn lower_branch(stmt: &Statement) -> Vec<NStmt> {
    match stmt {
        Statement::BlockStatement(block) => lower_stmts(&block.body),
        other => vec![lower_stmt(other)],
    }
}

// ---------------------------------------------------------------------------
// ɵɵngDeclare* collection (neutral surface for the linker).
// ---------------------------------------------------------------------------

/// Collect every `ɵɵngDeclare*({…})` call that sits as a `static X = …` member of a class.
fn collect_ng_declares_in_class(class: &Class, out: &mut Vec<NgDeclareCall>) {
    for element in &class.body.body {
        if let ClassElement::PropertyDefinition(prop) = element {
            if let Some(init) = &prop.value {
                push_if_ng_declare(init, out);
            }
        }
    }
}

/// Collect a `ɵɵngDeclare*({…})` call that sits as a free top-level expression statement.
fn collect_ng_declares_in_stmt(stmt: &Statement, out: &mut Vec<NgDeclareCall>) {
    if let Statement::ExpressionStatement(es) = stmt {
        push_if_ng_declare(&es.expression, out);
    }
    if let Statement::VariableDeclaration(decl) = stmt {
        for d in &decl.declarations {
            if let Some(init) = &d.init {
                push_if_ng_declare(init, out);
            }
        }
    }
}

/// If `expr` is a `ɵɵngDeclare*({…})` call with an object-literal argument, push its neutral form;
/// otherwise recurse through the wrappers a declaration call inhabits — the RHS of an assignment
/// (`X.ɵprov = <call>`), a parenthesized group, and a comma sequence — mirroring the linker's
/// `collect_in_expression` so the assignment-statement / member form is captured identically.
fn push_if_ng_declare(expr: &Expression, out: &mut Vec<NgDeclareCall>) {
    match expr {
        Expression::CallExpression(call) => {
            let Some(kind) = declare_callee_kind(&call.callee) else {
                return;
            };
            for arg in &call.arguments {
                if let Argument::ObjectExpression(obj) = arg {
                    out.push(NgDeclareCall {
                        kind,
                        object: lower_object(obj),
                        call_span: span_of(call.as_ref()),
                    });
                    return;
                }
            }
        }
        Expression::AssignmentExpression(assign) => push_if_ng_declare(&assign.right, out),
        Expression::ParenthesizedExpression(p) => push_if_ng_declare(&p.expression, out),
        Expression::SequenceExpression(seq) => {
            for part in &seq.expressions {
                push_if_ng_declare(part, out);
            }
        }
        _ => {}
    }
}

/// The `ɵɵngDeclare*` callee suffix (`Component`, `Factory`, …) for the bare-identifier
/// (`ɵɵngDeclareX(...)`) and namespaced (`i0.ɵɵngDeclareX(...)`) call forms.
fn declare_callee_kind(callee: &Expression) -> Option<String> {
    let name = match callee {
        Expression::Identifier(id) => id.name.as_str(),
        Expression::StaticMemberExpression(m) => m.property.name.as_str(),
        _ => return None,
    };
    name.strip_prefix("\u{0275}\u{0275}ngDeclare")
        .filter(|suffix| !suffix.is_empty())
        .map(str::to_string)
}
