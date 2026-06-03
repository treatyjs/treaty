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
    /// set when the source wrote a computed key (`['k']: v`).
    KeyValue {
        key: String,
        value: NExpr,
        quoted: bool,
        computed: bool,
    },
    /// `...expr`.
    Spread(NExpr),
    /// A property NOT modelled above (shorthand / method / getter / setter / non-static computed key).
    Other(TreatySpan),
}

/// A call/new ARGUMENT: a plain expression or a `...spread`.
#[derive(Debug, Clone, PartialEq)]
pub enum NArg {
    /// `expr`.
    Expr(NExpr),
    /// `...expr`.
    Spread(NExpr),
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
}

/// A pre-lowered class carrying an Angular decorator: name + its decorators + its members.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClassWithDecorators {
    /// The class identifier, if present.
    pub name: Option<String>,
    /// The class's leading decorators in source order.
    pub decorators: Vec<DecoratorInfo>,
    /// The class's members in source order.
    pub members: Vec<MemberInfo>,
}
