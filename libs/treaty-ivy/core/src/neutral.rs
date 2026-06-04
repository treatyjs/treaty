//! Engine-neutral PARSE IR — the pre-lowered, structurally-walkable data the Ivy front-end reads
//! WITHOUT naming an `oxc_`/`swc_` type.
//!
//! These structs are the shared metadata-extraction surface drawn by `migration/SWC-BACKEND-PLAN.md`
//! §3.2: a parse backend fills them (in SOURCE order) from its native AST, and the decorator →
//! definition layer ([`treaty_ivy_decorators`]) consumes them through its public API
//! (`treaty_ivy_decorators::registry::ClassMeta`). They carry NO engine types — only owned data
//! (`String` / `f64` / `bool` / [`TreatySpan`]) — so the same IR is produced byte-identically by
//! either backend and the decorators crate can name it without an oxc dependency.
//!
//! Source order is load-bearing: Angular copies several metadata blobs (`host`, `animations`, …)
//! through preserving authoring order, so the emit is only byte-identical if the walk preserves it.
//!
//! The facade's `crate::parse` module re-exports every type below from its historical path
//! (`crate::parse::ObjLit`, …) and adds the parse-channel pieces (`SourceKind`, `ParseOutput`,
//! `ParseBackend`) that are specific to the parse seam rather than the decorator surface.

/// An engine-neutral byte range `[start, end)` into the original source. Absolute byte offsets on the
/// oxc backend; recover the covered text via the parse backend's `span_text` rather than slicing
/// directly (the swc backend's `BytePos` are `SourceMap`-relative).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TreatySpan {
    pub start: u32,
    pub end: u32,
}

impl TreatySpan {
    pub fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }
}

/// An engine-neutral object-literal: its properties in SOURCE order plus the literal's own span.
///
/// Source order is load-bearing — Angular copies several metadata blobs (`host`, `animations`, …)
/// through preserving authoring order, so the emit is only byte-identical if the walk preserves it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ObjLit {
    /// `(key, value)` pairs in the order they appear in the source object literal. Only
    /// object-property entries with a static (identifier / string-literal) key are captured here;
    /// spreads and computed keys are dropped (they never appear in the metadata the front-end reads).
    pub props: Vec<(String, LitValue)>,
    /// The byte span of the whole `{ … }` literal.
    pub span: TreatySpan,
    /// The LOSSLESS full-expression view of the SAME object literal: EVERY property in source order —
    /// `key: value` (value as a full [`NExpr`], not the lossy [`LitValue`]), spreads, computed keys —
    /// element-for-element with the AST (see [`NObjectProp`]). This is the surface the partial-link
    /// walk needs: `linker::link_partial_walk`'s `first_object_expression` + `convert_expr` lower
    /// ARBITRARY `ɵɵngDeclare` object values (`providers`, `useFactory` arrows, `transform` fns) into
    /// full `o::Expr`, which the lossy `props` (an `NExpr::Arrow`/`Call` degrades to `LitValue::Other`)
    /// cannot carry. `props` stays the metadata-subset fast path; `nprops` is the complete one. Filled
    /// identically by both backends, in source order.
    pub nprops: Vec<NObjectProp>,
}

impl ObjLit {
    /// The value of the property named `name`, if present (first match in source order).
    pub fn get(&self, name: &str) -> Option<&LitValue> {
        self.props
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v)
    }
}

/// An engine-neutral literal/expression value pre-lowered from the parsed AST. Covers the subset of
/// expression shapes the facade's object-literal metadata walk reads structurally; richer expression
/// shapes (arrow/function bodies, member chains, calls) that flow into `output_ast` conversion remain
/// handled against the live AST in the `oxc` backend.
#[derive(Debug, Clone, PartialEq)]
pub enum LitValue {
    /// A string literal or no-substitution template literal.
    String(String),
    /// A numeric literal (kept as the parsed `f64`).
    Number(f64),
    /// A boolean literal.
    Boolean(bool),
    /// `null`.
    Null,
    /// A bare identifier / member-expression name (e.g. `ChangeDetectionStrategy.OnPush` keeps the
    /// trailing property name; consumers that need the full path use the live-AST escape hatch).
    Identifier(String),
    /// An array literal, element values in source order.
    Array(Vec<LitValue>),
    /// A nested object literal.
    Object(ObjLit),
    /// Any expression shape NOT pre-lowered above (arrow, call, conditional, …). Carries its span so
    /// the consumer can recover the source text or re-walk the live AST. The variant exists so the
    /// neutral walk never silently drops a property.
    Other(TreatySpan),
}

impl LitValue {
    /// The string payload, if this is a [`LitValue::String`].
    pub fn as_str(&self) -> Option<&str> {
        match self {
            LitValue::String(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// The identifier/member name, if this is a [`LitValue::Identifier`].
    pub fn as_identifier(&self) -> Option<&str> {
        match self {
            LitValue::Identifier(s) => Some(s.as_str()),
            _ => None,
        }
    }
}

// ===========================================================================
// Neutral EXPRESSION / STATEMENT / PARAM tree (the FULL walk surface).
//
// `LitValue` above covers the object-literal METADATA subset the structural walk reads. The nodes
// below extend the engine-neutral IR to mirror the FULL `oxc_ast::Expression` / `Statement` surface
// the AST→Ivy walk's `convert_expr` / `convert_statement` consume (`source_compile::convert_expr` +
// `linker::convert_expr`): the 18 `Expression` variants — including arrow/function bodies — plus the
// factory/transform-body statement subset and constructor/method parameters.
//
// They are ADDITIVE: this phase fills them from BOTH backends in SOURCE order and gates them at
// parity, but the walk still runs against the live oxc AST (so emit stays byte-identical). The shapes
// mirror oxc structurally so a later phase can switch `convert_expr` to consume `NExpr` mechanically.
//
// Operator parity: binary / logical / unary operators are carried as their SOURCE SPELLING string
// (`==`, `&&`, `??`, `!`, `typeof`, …) rather than an engine enum. oxc's `*Operator::as_str()` and
// swc's `StringEnum`-derived `BinaryOp`/`UnaryOp` string forms are identical for every operator, and
// oxc's separate `LogicalExpression` collapses into the same `NExpr::Binary` as swc's `BinaryOp`
// logical variants (`&&`/`||`/`??`) — so the neutral tree is byte-identical across engines. The
// existing `convert_expr`s already match operators on `.as_str()`, so this is faithful.
// ===========================================================================

/// A function/arrow formal PARAMETER in the neutral tree. Only the simple identifier-binding shape is
/// modelled (the only shape the transform/factory-body converters accept); destructuring / defaults /
/// rest are flagged so the consumer declines exactly as the live-AST walk does.
#[derive(Debug, Clone, PartialEq)]
pub struct NParam {
    /// The binding identifier's name, when the parameter is a plain identifier binding.
    pub name: Option<String>,
    /// Whether this is a `...rest` parameter (oxc stores it out of `items`; swc as a `Pat::Rest` —
    /// flagged here so both backends agree and a later consumer can decline a rest signature exactly
    /// as `linker::convert_params` does).
    pub is_rest: bool,
}

/// A neutral STATEMENT — the factory/transform block-body subset the walk's `convert_statement`
/// handles (`const`/`let`/`var`, expression, `if`/`else`, `return`, block). Anything richer is
/// [`NStmt::Other`] carrying its span (the live-AST walk declines it; the neutral tree never drops
/// it).
#[derive(Debug, Clone, PartialEq)]
pub enum NStmt {
    /// `const`/`let`/`var name = init;` — `is_const` distinguishes `const` (emits `const`/FINAL) from
    /// `let`/`var`. `init` is `None` for an uninitialised declaration. Captures every declarator in
    /// source order (the converter only accepts a single one, but the neutral tree mirrors the AST).
    VarDecl {
        is_const: bool,
        decls: Vec<NVarDeclarator>,
    },
    /// A bare expression statement (`foo();`).
    Expr(NExpr),
    /// `return expr;` (or value-less `return;` → `None`).
    Return(Option<NExpr>),
    /// `if (test) { … } else { … }`. Each branch is the statements of its block (or a one-element
    /// list for a bare branch statement), mirroring `convert_branch`.
    If {
        test: NExpr,
        consequent: Vec<NStmt>,
        alternate: Vec<NStmt>,
    },
    /// `{ … }` block statement — its inner statements in source order.
    Block(Vec<NStmt>),
    /// Any statement shape NOT modelled above, carrying its span so nothing is silently dropped.
    Other(TreatySpan),
}

/// One declarator of an [`NStmt::VarDecl`] — a binding name + optional initializer.
#[derive(Debug, Clone, PartialEq)]
pub struct NVarDeclarator {
    /// The binding identifier's name, when the declarator binds a plain identifier.
    pub name: Option<String>,
    /// The initializer expression, if present.
    pub init: Option<NExpr>,
}

/// The body of a neutral arrow function: an expression body (`x => expr`) or a block body
/// (`x => { … }`), mirroring `o::ArrowBody` and the live-AST `convert_arrow_body`.
#[derive(Debug, Clone, PartialEq)]
pub enum NArrowBody {
    /// `x => expr`.
    Expr(Box<NExpr>),
    /// `x => { …stmts… }`.
    Block(Vec<NStmt>),
}

/// A neutral EXPRESSION mirroring the `oxc_ast::Expression` variants the AST→Ivy walk's `convert_expr`
/// consumes. Faithful to the union of `source_compile::convert_expr` + `linker::convert_expr`: the
/// literal subset, references, member/computed-member access, calls, `new`, parenthesised grouping,
/// conditional, binary/logical (unified by source-spelling operator), unary, array (with spreads),
/// object (with spread properties), and inline arrow / function expressions (with bodies). Anything
/// outside this surface is [`NExpr::Other`] carrying its span — never silently dropped.
#[derive(Debug, Clone, PartialEq)]
pub enum NExpr {
    /// A string literal or no-substitution template literal (cooked value).
    String(String),
    /// A numeric literal.
    Number(f64),
    /// A boolean literal.
    Boolean(bool),
    /// `null`.
    Null,
    /// A bare identifier reference.
    Identifier(String),
    /// `object.property` static member access.
    Member {
        object: Box<NExpr>,
        property: String,
    },
    /// `object[index]` computed member access.
    ComputedMember {
        object: Box<NExpr>,
        index: Box<NExpr>,
    },
    /// `callee(...args)`.
    Call {
        callee: Box<NExpr>,
        args: Vec<NArg>,
    },
    /// `new callee(...args)`.
    New {
        callee: Box<NExpr>,
        args: Vec<NArg>,
    },
    /// `( inner )` — explicit grouping preserved (the linker keeps a `Parenthesized` node).
    Parenthesized(Box<NExpr>),
    /// `test ? consequent : alternate`.
    Conditional {
        test: Box<NExpr>,
        consequent: Box<NExpr>,
        alternate: Box<NExpr>,
    },
    /// A binary OR logical expression, the operator carried as its source spelling (`+`, `==`, `&&`,
    /// `??`, …). Unifies oxc's `BinaryExpression` + `LogicalExpression` and swc's `Expr::Bin` (whose
    /// `BinaryOp` already includes the logical operators) so the neutral tree is engine-identical.
    Binary {
        op: String,
        left: Box<NExpr>,
        right: Box<NExpr>,
    },
    /// A unary expression, operator carried as its source spelling (`!`, `-`, `+`, `typeof`, `void`,
    /// `delete`, `~`).
    Unary {
        op: String,
        argument: Box<NExpr>,
    },
    /// An array literal, its elements in source order.
    Array(Vec<NArrayElement>),
    /// An object literal, its properties in source order (key/value + spreads).
    Object(Vec<NObjectProp>),
    /// An inline arrow function `(params) => body`.
    Arrow {
        params: Vec<NParam>,
        body: Box<NArrowBody>,
    },
    /// An inline function expression `function (params) { …body… }`.
    Function {
        params: Vec<NParam>,
        body: Vec<NStmt>,
    },
    /// Any expression shape NOT modelled above, carrying its span (consumer re-walks the live AST or
    /// recovers the source text). Mirrors the `_ => None` arm of `convert_expr`.
    Other(TreatySpan),
}

/// An element of an [`NExpr::Array`]: a plain value, a `...spread`, or a hole (`[, x]`).
#[derive(Debug, Clone, PartialEq)]
pub enum NArrayElement {
    /// `value`.
    Expr(NExpr),
    /// `...value`.
    Spread(NExpr),
    /// A hole in a sparse array (`[, x]`).
    Hole,
}

/// A property of an [`NExpr::Object`]: a `key: value` pair (with a `quoted` flag and a `computed`
/// flag) or a `...spread`. Non-static keys / shorthand / methods carry their span as
/// [`NObjectProp::Other`] so the neutral tree stays element-for-element with the AST.
#[derive(Debug, Clone, PartialEq)]
pub enum NObjectProp {
    /// `key: value`. `quoted` is set when the key is not a bare-identifier-safe name; `computed` is
    /// set when the source wrote a computed key (`['k']: v`). `value_span` is the byte span of the
    /// VALUE expression — the verbatim-source surface `partial_emit::prop_source` reads (it slices the
    /// trimmed source of `providedIn`/`providers`/`useFactory`/… to round-trip an opaque value byte
    /// for byte). Filled identically by both backends.
    KeyValue {
        key: String,
        value: NExpr,
        quoted: bool,
        computed: bool,
        value_span: TreatySpan,
    },
    /// `...expr`.
    Spread(NExpr),
    /// A property NOT modelled above (shorthand / method / getter / setter / non-static computed key).
    Other(TreatySpan),
}

/// A call/new ARGUMENT: a plain expression or a `...spread`, each carrying the byte span of its
/// argument expression. The span is the verbatim-source surface the partial emit's factory-body
/// decompile reads — `partial_emit::dep_entry_from_inject` / `arg_source` slice the trimmed source of
/// each `ɵɵinject(Token, …)` argument to round-trip an opaque DI token byte for byte. Filled
/// identically by both backends.
#[derive(Debug, Clone, PartialEq)]
pub enum NArg {
    /// `expr` (with its byte span).
    Expr(NExpr, TreatySpan),
    /// `...expr` (the span covers the inner expression).
    Spread(NExpr, TreatySpan),
}

impl NArg {
    /// The argument's expression, regardless of spread-ness.
    pub fn expr(&self) -> &NExpr {
        match self {
            NArg::Expr(e, _) | NArg::Spread(e, _) => e,
        }
    }

    /// The argument expression's byte span.
    pub fn span(&self) -> TreatySpan {
        match self {
            NArg::Expr(_, s) | NArg::Spread(_, s) => *s,
        }
    }
}

// ===========================================================================
// Neutral TOP-LEVEL PROGRAM surface (the AOT→partial emitter's walk surface).
//
// `partial_emit::collect_rewrites` walks the AOT module's TOP-LEVEL statements: each is an
// `X.ɵfac = function …` / `X.ɵprov = i0.ɵɵdefineInjectable({…})` ASSIGNMENT (rewritten to its
// `ɵɵngDeclare*` form, the RHS span overwritten), a `ɵɵsetNgModuleScope(X, {…})` side-effect CALL
// (read for an NgModule's declarations/imports/exports), or some other statement (left verbatim).
// The nodes below model exactly that surface — a top-level statement list with spans + `NExpr`
// values — so the emitter's walk can run neutrally. They are ADDITIVE: this phase fills them in
// SOURCE order from BOTH backends and gates them at parity; the emitter still walks the live oxc AST.
// ===========================================================================

/// One TOP-LEVEL program statement, in source order — the surface
/// [`partial_emit::collect_rewrites`] walks. Only the shapes that walk distinguishes are modelled
/// richly (an `X.member = rhs` assignment statement, a bare call statement, a `var`/`let`/`const`
/// declaration whose initializer the `ɵɵngDeclare*` scan reads); every other statement is
/// [`NTopStmt::Other`], carrying its span so nothing is dropped.
#[derive(Debug, Clone, PartialEq)]
pub enum NTopStmt {
    /// An `X.member = rhs;` assignment expression statement (the `ɵfac`/`ɵprov`/`ɵpipe`/`ɵmod`/`ɵinj`/
    /// `ɵcmp`/`ɵdir` definition scaffold the AOT emit produces).
    Assignment(NAssignment),
    /// A bare expression statement whose expression is a call (`ɵɵsetNgModuleScope(X, {…})`, possibly
    /// wrapped in a `typeof ngJitMode … && …` guard). Carries the call as an [`NExpr`] + the statement
    /// span.
    ExprStmt { expr: NExpr, span: TreatySpan },
    /// A `var`/`let`/`const name = init;` declaration (its declarators mirror [`NStmt::VarDecl`]) —
    /// the linker also surfaces `ɵɵngDeclare*` initializers here. Carries the statement span.
    VarDecl {
        is_const: bool,
        decls: Vec<NVarDeclarator>,
        span: TreatySpan,
    },
    /// A top-level `function f(…): RetType {…}` declaration (bare or `export function …`) — the surface
    /// `source_compile::collect_module_with_providers_returns` walks: it records every
    /// `function f(): ModuleWithProviders<T>` as `f → T` so a jit-mode NgModule's `imports`-referenced
    /// factory CALL resolves to that ngModule type. Carries the function NAME + its annotated RETURN
    /// TYPE (as an [`NTypeRef`], `None` for an un-annotated / non-reference return). Both the bare
    /// `function …` and the `export function …` forms surface here identically on both backends.
    FnDecl {
        name: Option<String>,
        return_type: Option<NTypeRef>,
        span: TreatySpan,
    },
    /// Any other top-level statement (import, class declaration, …) — span only.
    Other(TreatySpan),
}

/// A top-level `X.member = rhs;` assignment — the definition-scaffold shape
/// [`partial_emit::collect_rewrites`] rewrites. Carries the LHS's `<Ident>.<member>` parts (the
/// `assignment_member` read), the RHS as a full [`NExpr`] (so the factory-body decompile — `new`
/// args, inject calls, `ɵɵgetInheritedFactory`/`ɵɵinvalidFactory` references — is reachable
/// neutrally), and the byte spans the surgical rewrite needs (the RHS span is overwritten with the
/// `ɵɵngDeclare*` text).
#[derive(Debug, Clone, PartialEq)]
pub struct NAssignment {
    /// The LHS object identifier (`X` in `X.ɵfac = …`), when the target is `<Ident>.<member>`.
    pub target_object: Option<String>,
    /// The LHS member name (`ɵfac`/`ɵprov`/…), when the target is `<Ident>.<member>`.
    pub target_member: Option<String>,
    /// The right-hand side, as a full neutral expression.
    pub value: NExpr,
    /// The byte span of the right-hand side (the bytes the surgical rewrite overwrites).
    pub value_span: TreatySpan,
    /// The byte span of the whole assignment statement.
    pub span: TreatySpan,
}

/// A pre-lowered Angular DECORATOR on a class: its callee name and (when called with an object
/// literal) the pre-lowered object argument.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DecoratorInfo {
    /// The callee identifier — `Component`, `Directive`, `Pipe`, `NgModule`, `Injectable`, … — for
    /// both the bare `@Foo` and the call `@Foo({…})` forms.
    pub name: String,
    /// The first object-literal argument of `@Foo({…})`, pre-lowered. `None` for a bare `@Foo`.
    pub object: Option<ObjLit>,
    /// The decorator call's FULL argument list as neutral expressions, in source order. Empty for a
    /// bare `@Foo`. Captures the args the structural `object` does not — e.g. `@HostListener('click',
    /// ['$event'])`'s event name + arg-binding array, and `@Inject(TOKEN)`'s token.
    pub arguments: Vec<NArg>,
    /// The byte span of the WHOLE decorator node (`@Foo({…})`, leading `@` included). Load-bearing for
    /// the AOT front-end's surgical decorator EXCISION: `source_compile::recognized_decorator_strip_span`
    /// spans first..last recognized class decorator (`@Component`+`@Injectable`) to cut the exact bytes
    /// for the kept `export class X {…}`, and `member_decorator_strip_spans` cuts each inert member
    /// decorator (`@Input`/`@HostBinding`/…) off the kept class-body slice. Filled identically by both
    /// backends (oxc `Decorator::span` / swc `Decorator.span`, rebased).
    pub span: TreatySpan,
}

/// The kind of a class member, mirroring the `oxc_ast::ClassElement` discriminants the walk
/// distinguishes (a property vs a method vs a getter/setter vs the constructor vs an accessor), so the
/// neutral surface can drive ctor-dep extraction (the `constructor` member's `params`) and
/// signal/query detection (a property's `initializer` call) without naming an oxc type.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MemberKind {
    /// A `name = init;` field / `static name = …;` (oxc `PropertyDefinition`).
    #[default]
    Property,
    /// A `name(...) { … }` method (oxc `MethodDefinition` of `kind: Method`).
    Method,
    /// A `get name() { … }` accessor.
    Getter,
    /// A `set name(v) { … }` accessor.
    Setter,
    /// The `constructor(...) { … }` — its `params` drive constructor-dependency extraction.
    Constructor,
    /// An `accessor name = …;` auto-accessor (oxc `AccessorProperty`).
    Accessor,
    /// Any other class element (a static block, a TS index signature, a stray `;` that oxc still
    /// surfaces as a nameless element, …).
    Other,
}

/// A pre-lowered class MEMBER carrying the full shape the AST→Ivy walk reads structurally: name + own
/// decorators (kept from the metadata phase) PLUS the kind, static flag, parameters (for the
/// constructor / methods — drives constructor-dependency extraction), and the initializer expression
/// (drives signal `input()` / query `viewChild()` detection). Filled identically by both backends.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemberInfo {
    /// The member's name, when it is a statically-known identifier/string key.
    pub name: Option<String>,
    /// The member's own decorators (`@Input()`, `@Output()`, `@HostBinding(...)`, …), pre-lowered.
    pub decorators: Vec<DecoratorInfo>,
    /// Which kind of class element this is.
    pub kind: MemberKind,
    /// Whether the member is `static`.
    pub is_static: bool,
    /// The member's formal parameters (constructor / method / accessor), in source order. The
    /// constructor's params carry their own decorators (`@Inject(...)`, `@Optional()`, …) in
    /// [`NCtorParam::decorators`] — the surface constructor-dependency extraction consumes.
    pub params: Vec<NCtorParam>,
    /// The property/accessor initializer expression (`x = input(0)` → the `input(0)` call), if any.
    pub initializer: Option<NExpr>,
    /// Whether a method / CONSTRUCTOR member has a BODY (`{ … }`). A TS overload SIGNATURE
    /// (`constructor(a: A);` with no body) is `false`; the IMPLEMENTATION (`constructor(a: A, b: B) {}`)
    /// is `true`. Constructor-dependency extraction reads the IMPLEMENTATION signature's params (ngtsc's
    /// `getConstructorDependencies`), so it prefers the body-bearing constructor over the bodiless
    /// overloads. A property / accessor carries `false` (no method body). Both backends fill it from the
    /// member's body presence (oxc `MethodDefinition.value.body` / swc `Constructor.body` /
    /// `Function.body`).
    pub has_body: bool,
    /// The byte span of the whole member node. Carried so a consumer can recover the member's source
    /// slice (the kept class-body emit excises inert member decorators from within this range). Filled
    /// identically by both backends.
    pub span: TreatySpan,
}

/// A constructor / method PARAMETER carrying its own decorators — the shape constructor-dependency
/// extraction reads (`constructor(@Inject(TOKEN) @Optional() private dep: Dep)`). The decorators are
/// modelled here (unlike the plain [`NParam`] used for transform/factory function bodies) because
/// Angular's DI metadata is derived from them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NCtorParam {
    /// The parameter's binding identifier name, when it is a plain identifier binding.
    pub name: Option<String>,
    /// The parameter's decorators (`@Inject(...)`, `@Optional()`, `@Self()`, `@Host()`,
    /// `@SkipSelf()`), pre-lowered, in source order.
    pub decorators: Vec<DecoratorInfo>,
    /// Whether this is a `...rest` parameter (see [`NParam::is_rest`]).
    pub is_rest: bool,
    /// The parameter's declared TYPE, when it is a TS type REFERENCE (`dep: Foo` → `Foo`;
    /// `dep: ns.Foo` → `ns.Foo`) — the DEFAULT injection token constructor-dependency extraction
    /// derives (`source_compile::extract_ctor_dep` reads `param.type_annotation` → `type_token_expr`
    /// → `type_name_expr`, which lowers a `TSTypeReference`'s qualified name to a value-read). `None`
    /// for an un-annotated parameter OR a non-reference type (primitives, unions, `any`, `this`, …),
    /// which carry no usable token — exactly the cases `type_token_expr` returns `None` for. The token
    /// is overridable by an `@Inject`/`@Attribute` param decorator (carried in [`Self::decorators`]),
    /// so this is only the fallback. Filled identically by both backends (oxc `TSType::TSTypeReference`
    /// / `TSTypeName`, swc `TsType::TsTypeRef` / `TsEntityName`).
    pub type_ref: Option<NTypeRef>,
}

/// An engine-neutral TS TYPE REFERENCE — the name PATH of a type reference plus its type arguments.
/// The only TS-type surface the AOT source front-end reads structurally:
///   * a constructor parameter's DEFAULT injection token ([`NCtorParam::type_ref`] — `dep: Foo` /
///     `dep: ns.Foo`); only the [`Self::name_path`] is consumed there (the args are ignored);
///   * a `function f(): ModuleWithProviders<T>` RETURN type ([`NTopStmt::FnDecl::return_type`]); the
///     jit-mode NgModule import resolver unwraps the `ModuleWithProviders<T>` to its `T`
///     ([`Self::name_path`] == `["ModuleWithProviders"]` and the first [`Self::type_args`] entry's
///     `name_path` == `["T"]`).
///
/// Only the TS *type-reference* shape is modelled; every other type form (keyword / union / literal /
/// function / array / …) lowers to `None` at the [`NCtorParam`]/[`NTopStmt::FnDecl`] surface (those
/// carry an `Option<NTypeRef>`), exactly the cases the live-AST walk declines. Filled identically by
/// both backends.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NTypeRef {
    /// The dotted type-NAME segments in source order: a bare reference `Foo` → `["Foo"]`; a qualified
    /// `ns.Foo` → `["ns", "Foo"]`; `a.b.C` → `["a", "b", "C"]`. EMPTY when the type name is `this`
    /// (`this`-types carry no injectable token — `type_name_expr` returns `None`); the consumer treats
    /// an empty path as "no token".
    pub name_path: Vec<String>,
    /// The type arguments (`ModuleWithProviders<T>` → one `NTypeRef { name_path: ["T"], … }`), in
    /// source order. Only type-REFERENCE arguments are captured (a non-reference type argument is
    /// dropped — the `module_with_providers_type_arg` consumer only matches a bare type-reference
    /// argument and returns `None` otherwise; mirroring that keeps the args list byte-identical and the
    /// consumer's match faithful).
    pub type_args: Vec<NTypeRef>,
}

/// Whether a class-shaped declaration was authored as a plain `class`, an (unshared) `struct`, or a
/// `shared struct` — the TC39 "JavaScript Structs: Fixed Layout Objects" proposal (Stage 2,
/// <https://github.com/tc39/proposal-structs>).
///
/// `struct`/`shared struct` are class-SHAPED declarations (same field/method/getter/setter member
/// grammar), so a parse backend lowers them into the SAME [`ClassWithDecorators`] as a `class` and
/// records only which form it saw here. The distinction drives later lowering (an unshared `struct`
/// → a sealed `class`; a `shared struct` → a shared-heap target / an explicit diagnostic), but is
/// inert for the current emit: the field defaults to [`StructKind::None`], so a class is byte-identical
/// to before and `matchGolden` is unaffected.
///
/// `struct` and `shared` are CONTEXTUAL keywords (legal identifiers elsewhere); the recognizer that
/// sets this respects the proposal's `[no LineTerminator here]` ASI rule after `struct`/`shared`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum StructKind {
    /// A plain `class` — the default; carries no struct semantics.
    #[default]
    None,
    /// An unshared `struct Name { … }` — fixed layout (sealed), fields pre-initialized to `undefined`.
    Struct,
    /// A `shared struct Name { … }` — the cross-agent variant (null prototype, data-only, fields hold
    /// only primitives / other shared values).
    SharedStruct,
}

/// A pre-lowered class carrying an Angular decorator: name + its decorators + its members.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClassWithDecorators {
    /// The class identifier, if present.
    pub name: Option<String>,
    /// The byte span of the class's NAME identifier (`class X` → the `X` token), when the class has a
    /// name. This is the anchor the additive source map maps the emitted `type: <ClassName>` read back
    /// to (`source_compile`'s `class_name_span` → `class_ref_spanned`): a value-preserving span that
    /// never changes emitted bytes, only the map. `Default` (a zero span) for an anonymous class. Both
    /// backends fill it from the class's binding identifier (oxc `Class::id.span` / swc
    /// `ClassDecl::ident.span` / `ClassExpr::ident.span`), rebased to absolute byte offsets.
    pub name_span: TreatySpan,
    /// The class's leading decorators in source order.
    pub decorators: Vec<DecoratorInfo>,
    /// The class's members in source order.
    pub members: Vec<MemberInfo>,
    /// The byte span of the CLASS node itself (the `class X {…}`/`@Dec class X {…}` declaration). Its
    /// `span.start` is the forward-reference anchor: `source_compile` keys `class_decl_positions` on it
    /// (`isExpressionForwardReference`: a dependency declared LATER — `context.pos < node.pos` — forces
    /// the `dependencies` array into a `() => [...]` closure, `DeclarationListEmitMode::Closure`). Both
    /// backends fill the class node's own span (decorators included, as oxc's `Class::span` does).
    pub span: TreatySpan,
    /// The byte span of the ENCLOSING top-level statement that declares this class — its leading
    /// decorator, or the `export`/`class` keyword when undecorated. `source_compile::decorated_stmt_start`
    /// uses `stmt_span.start` as the assembly key when re-stitching the kept class declarations around
    /// the emitted Ivy statics. Equals [`Self::span`] for a bare `class X {}`, and starts at the
    /// `export`/decorator for an exported / decorated class. Filled identically by both backends.
    pub stmt_span: TreatySpan,
    /// Whether the declaration was authored as a plain `class`, an unshared `struct`, or a
    /// `shared struct` (TC39 JavaScript Structs, Stage 2). [`StructKind::None`] for a `class` — the
    /// default — so this is ADDITIVE: a class lowers byte-identically to before and `matchGolden` is
    /// unaffected. A struct recognizer (the oxc backend's pre-scan) sets it to
    /// [`StructKind::Struct`] / [`StructKind::SharedStruct`] after bridge-rewriting the `struct` /
    /// `shared struct` keyword to `class` so the engine parses the class body unchanged. Filled by
    /// both backends (the swc recognizer is deferred to M3).
    pub struct_kind: StructKind,
}
