//! Output AST — the language-agnostic `o.*` IR (Expr/Stmt/Type) that every render3 code
//! generator emits into. Owned & arena-free (Box/Vec/String) so builders can mutate/clone/dedup.
//!
//! PORT TARGET: see `migration/render3-specs/01-output_ast.md`
//! Source: `tools/angular-ref/packages/compiler/src/output/output_ast.ts`
//!
//! This is the keystone IR for the port. It mirrors Angular's `output_ast.ts` class
//! hierarchy as a pair of enums (`Expr`/`ExprKind`, `Stmt`/`StmtKind`, `Type`) with the
//! cross-cutting algorithms `is_equivalent` (structural equality used by the constant pool),
//! `is_constant` (hoistability), and `clone` (derived).

#![allow(clippy::needless_lifetimes)]

use std::rc::Rc;

// ---------------------------------------------------------------------------
// ParseSourceSpan placeholder (real one lives in `parse_util`; minimal here).
// ---------------------------------------------------------------------------

/// Minimal placeholder for `parse_util::ParseSourceSpan`. Real port carries
/// `ParseLocation` start/end + details; here we only need a structurally-comparable
/// span so nodes can hold `Option<ParseSourceSpan>`. Equality of spans is *ignored*
/// by `is_equivalent`, so its `PartialEq` is for completeness only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseSourceSpan {
    pub start: usize,
    pub end: usize,
}

impl ParseSourceSpan {
    pub fn new(start: usize, end: usize) -> Self {
        ParseSourceSpan { start, end }
    }
}

// ---------------------------------------------------------------------------
// i18n stub types (real ones live in `../i18n/i18n_ast` + `../render3/view/i18n/meta`).
// `LocalizedString` carries these; the i18n subsystem is deferred (see spec §4.5).
// ---------------------------------------------------------------------------

/// Placeholder for `../render3/view/i18n/meta`'s `I18nMeta`. Only the fields read by
/// `serialize_i18n_head` are modelled.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct I18nMeta {
    pub description: Option<String>,
    pub meaning: Option<String>,
    pub custom_id: Option<String>,
    pub legacy_ids: Vec<String>,
}

/// Placeholder for `../i18n/i18n_ast`'s `Message`. Referenced by `PlaceholderPiece`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Message {
    pub message_string: String,
    pub meaning: Option<String>,
    pub legacy_ids: Vec<String>,
}

// ---------------------------------------------------------------------------
// Type modifiers (bitflags-style — `bitflags` crate is unavailable, so a newtype).
// ---------------------------------------------------------------------------

/// `TypeModifier` — `None = 0`, `Const = 1 << 0`. Implemented as a `u8` newtype with
/// const flags + `has_modifier`, mirroring Angular's `hasModifier`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TypeModifier(pub u8);

impl TypeModifier {
    pub const NONE: TypeModifier = TypeModifier(0);
    pub const CONST: TypeModifier = TypeModifier(1 << 0);

    #[inline]
    pub fn has_modifier(self, modifier: TypeModifier) -> bool {
        (self.0 & modifier.0) != 0
    }

    #[inline]
    pub fn bits(self) -> u8 {
        self.0
    }
}

impl std::ops::BitOr for TypeModifier {
    type Output = TypeModifier;
    fn bitor(self, rhs: TypeModifier) -> TypeModifier {
        TypeModifier(self.0 | rhs.0)
    }
}

// ---------------------------------------------------------------------------
// Types — the `o.Type` hierarchy.
// ---------------------------------------------------------------------------

/// `BuiltinTypeName` (`output_ast.ts` enum). Order preserved (discriminants matter for
/// any code matching against the underlying integer in downstream emitters).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinTypeName {
    Dynamic,
    Bool,
    String,
    Int,
    Number,
    Function,
    Inferred,
    None,
}

/// The `o.Type` enum. Every variant folds the abstract base's `modifiers: TypeModifier`.
///
/// - `Builtin`           ← `BuiltinType { name }`
/// - `Expression`        ← `ExpressionType { value, typeParams }`
/// - `Array`             ← `ArrayType { of }`
/// - `Map`               ← `MapType { valueType }`  (undefined→None normalised in ctor)
/// - `Transplanted`      ← `TransplantedType<T> { type }` (foreign host node, by handle)
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Builtin {
        name: BuiltinTypeName,
        modifiers: TypeModifier,
    },
    Expression {
        value: Box<Expr>,
        type_params: Option<Vec<Type>>,
        modifiers: TypeModifier,
    },
    Array {
        of: Box<Type>,
        modifiers: TypeModifier,
    },
    Map {
        value_type: Option<Box<Type>>,
        modifiers: TypeModifier,
    },
    Transplanted {
        node: WrappedNodeHandle,
        modifiers: TypeModifier,
    },
}

impl Type {
    /// `BuiltinType` constructor.
    pub fn builtin(name: BuiltinTypeName) -> Type {
        Type::Builtin {
            name,
            modifiers: TypeModifier::NONE,
        }
    }

    /// `hasModifier(modifier)` — reads the per-variant `modifiers`.
    pub fn has_modifier(&self, modifier: TypeModifier) -> bool {
        self.modifiers().has_modifier(modifier)
    }

    pub fn modifiers(&self) -> TypeModifier {
        match self {
            Type::Builtin { modifiers, .. }
            | Type::Expression { modifiers, .. }
            | Type::Array { modifiers, .. }
            | Type::Map { modifiers, .. }
            | Type::Transplanted { modifiers, .. } => *modifiers,
        }
    }
}

// Builtin-type singletons (`DYNAMIC_TYPE` … `NONE_TYPE`) as const-fn constructors.
pub fn dynamic_type() -> Type {
    Type::builtin(BuiltinTypeName::Dynamic)
}
pub fn inferred_type() -> Type {
    Type::builtin(BuiltinTypeName::Inferred)
}
pub fn bool_type() -> Type {
    Type::builtin(BuiltinTypeName::Bool)
}
pub fn int_type() -> Type {
    Type::builtin(BuiltinTypeName::Int)
}
pub fn number_type() -> Type {
    Type::builtin(BuiltinTypeName::Number)
}
pub fn string_type() -> Type {
    Type::builtin(BuiltinTypeName::String)
}
pub fn function_type() -> Type {
    Type::builtin(BuiltinTypeName::Function)
}
pub fn none_type() -> Type {
    Type::builtin(BuiltinTypeName::None)
}

// ---------------------------------------------------------------------------
// Operators.
// ---------------------------------------------------------------------------

/// `UnaryOperator` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOperator {
    Minus,
    Plus,
}

/// `BinaryOperator` enum — ALL 31 variants in source order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOperator {
    Equals,
    NotEquals,
    Assign,
    Identical,
    NotIdentical,
    Minus,
    Plus,
    Divide,
    Multiply,
    Modulo,
    And,
    Or,
    BitwiseOr,
    BitwiseAnd,
    Lower,
    LowerEquals,
    Bigger,
    BiggerEquals,
    NullishCoalesce,
    Exponentiation,
    In,
    InstanceOf,
    AdditionAssignment,
    SubtractionAssignment,
    MultiplicationAssignment,
    DivisionAssignment,
    RemainderAssignment,
    ExponentiationAssignment,
    AndAssignment,
    OrAssignment,
    NullishCoalesceAssignment,
}

impl BinaryOperator {
    /// `BinaryOperatorExpr.isAssignment()` — true for `Assign` and all compound-assignment ops.
    pub fn is_assignment(self) -> bool {
        matches!(
            self,
            BinaryOperator::Assign
                | BinaryOperator::AdditionAssignment
                | BinaryOperator::SubtractionAssignment
                | BinaryOperator::MultiplicationAssignment
                | BinaryOperator::DivisionAssignment
                | BinaryOperator::RemainderAssignment
                | BinaryOperator::ExponentiationAssignment
                | BinaryOperator::AndAssignment
                | BinaryOperator::OrAssignment
                | BinaryOperator::NullishCoalesceAssignment
        )
    }
}

// ---------------------------------------------------------------------------
// WrappedNode handle (foreign host AST node, e.g. an oxc node).
// ---------------------------------------------------------------------------

/// Opaque handle into a side table of foreign host AST nodes (the `T` in
/// `WrappedNodeExpr<T>` / `TransplantedType<T>`). The IR is arena-free, so we store an
/// identity-preserving index rather than threading an oxc `'a` lifetime through the tree.
///
/// `isEquivalent` for `WrappedNodeExpr` is **reference identity** (`this.node === e.node`),
/// hence handles compare by index — preserve identity at the side table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WrappedNodeHandle(pub u32);

// ---------------------------------------------------------------------------
// Literal value.
// ---------------------------------------------------------------------------

/// `LiteralExpr.value: number | string | boolean | null | undefined`.
///
/// JS uses `===` for equality: `null !== undefined`, `NaN !== NaN`, `-0 === 0`.
/// We therefore implement `PartialEq` manually (NOT derived) to match JS strict equality:
/// numbers compare with f64 `==` (so `NaN != NaN` and `-0.0 == 0.0`), and Null/Undefined
/// are distinct variants.
#[derive(Debug, Clone)]
pub enum LiteralValue {
    Number(f64),
    String(String),
    Bool(bool),
    Null,
    Undefined,
}

impl PartialEq for LiteralValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            // f64 `==` already gives JS `===` semantics: NaN != NaN, -0.0 == 0.0.
            (LiteralValue::Number(a), LiteralValue::Number(b)) => a == b,
            (LiteralValue::String(a), LiteralValue::String(b)) => a == b,
            (LiteralValue::Bool(a), LiteralValue::Bool(b)) => a == b,
            (LiteralValue::Null, LiteralValue::Null) => true,
            (LiteralValue::Undefined, LiteralValue::Undefined) => true,
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Supporting structs.
// ---------------------------------------------------------------------------

/// `FnParam` — function parameter (`name`, optional `type`).
#[derive(Debug, Clone, PartialEq)]
pub struct FnParam {
    pub name: String,
    pub ty: Option<Type>,
}

impl FnParam {
    pub fn new(name: impl Into<String>, ty: Option<Type>) -> FnParam {
        FnParam {
            name: name.into(),
            ty,
        }
    }

    /// `FnParam.isEquivalent` — compares **only the name** (per source).
    pub fn is_equivalent(&self, other: &FnParam) -> bool {
        self.name == other.name
    }
}

/// `ExternalReference`. CRITICAL: the identifiers module depends on this exact shape —
/// `name: String`, `module_name: Option<String>`. (Angular's source types both as
/// `string | null`; the port pins `name` to a required `String`.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalReference {
    pub name: String,
    pub module_name: Option<String>,
}

impl ExternalReference {
    pub fn new(module_name: Option<String>, name: impl Into<String>) -> ExternalReference {
        ExternalReference {
            name: name.into(),
            module_name,
        }
    }
}

/// `TemplateLiteralElementExpr` — `text` + derived `raw_text`. Also surfaces as an `Expr`
/// variant (`ExprKind::TemplateLiteralElement`).
#[derive(Debug, Clone, PartialEq)]
pub struct TemplateLiteralElement {
    pub text: String,
    /// If not supplied, derived via `escape_for_template_literal(escape_slashes(text))`.
    pub raw_text: String,
}

impl TemplateLiteralElement {
    /// Mirrors the `TemplateLiteralElementExpr` ctor: derive `raw_text` from `text` when absent.
    pub fn new(text: impl Into<String>, raw_text: Option<String>) -> TemplateLiteralElement {
        let text = text.into();
        let raw_text =
            raw_text.unwrap_or_else(|| escape_for_template_literal(&escape_slashes(&text)));
        TemplateLiteralElement { text, raw_text }
    }
}

/// `LiteralMapEntry` union: `LiteralMapPropertyAssignment` | `LiteralMapSpreadAssignment`.
#[derive(Debug, Clone, PartialEq)]
pub enum LiteralMapEntry {
    /// `LiteralMapPropertyAssignment { key, value, quoted }`.
    Property {
        key: String,
        value: Expr,
        quoted: bool,
    },
    /// `LiteralMapSpreadAssignment { expression }`.
    Spread { expression: Expr },
}

impl LiteralMapEntry {
    /// Per-entry `isEquivalent` (property compares key + value; spread compares expression).
    pub fn is_equivalent(&self, other: &LiteralMapEntry) -> bool {
        match (self, other) {
            (
                LiteralMapEntry::Property {
                    key: k1, value: v1, ..
                },
                LiteralMapEntry::Property {
                    key: k2, value: v2, ..
                },
            ) => k1 == k2 && v1.is_equivalent(v2),
            (
                LiteralMapEntry::Spread { expression: e1 },
                LiteralMapEntry::Spread { expression: e2 },
            ) => e1.is_equivalent(e2),
            _ => false,
        }
    }

    /// Per-entry `isConstant` (delegates to the value/expression).
    pub fn is_constant(&self) -> bool {
        match self {
            LiteralMapEntry::Property { value, .. } => value.is_constant(),
            LiteralMapEntry::Spread { expression } => expression.is_constant(),
        }
    }
}

/// `LiteralPiece` — i18n message part with its source span.
#[derive(Debug, Clone, PartialEq)]
pub struct LiteralPiece {
    pub text: String,
    pub source_span: ParseSourceSpan,
}

/// `PlaceholderPiece` — i18n placeholder with optional associated message.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaceholderPiece {
    pub text: String,
    pub source_span: ParseSourceSpan,
    pub associated_message: Option<Rc<Message>>,
}

/// `CookedRawString` — cooked/raw pair for `$localize` tagged-template emission.
#[derive(Debug, Clone, PartialEq)]
pub struct CookedRawString {
    pub cooked: String,
    pub raw: String,
    pub range: Option<ParseSourceSpan>,
}

/// `ArrowFunctionExpr.body` union: `() => expr` vs `() => { stmts }`.
#[derive(Debug, Clone, PartialEq)]
pub enum ArrowBody {
    Expr(Box<Expr>),
    Block(Vec<Stmt>),
}

/// `DynamicImportExpr.url` union: `string | Expression`.
#[derive(Debug, Clone, PartialEq)]
pub enum ImportUrl {
    Str(String),
    Expr(Box<Expr>),
}

// ---------------------------------------------------------------------------
// Comments / JSDoc.
// ---------------------------------------------------------------------------

/// `LeadingComment` / `JSDocComment`.
#[derive(Debug, Clone, PartialEq)]
pub enum LeadingComment {
    /// `LeadingComment { text, multiline, trailingNewline }`.
    Plain {
        text: String,
        multiline: bool,
        trailing_newline: bool,
    },
    /// `JSDocComment { tags }` — `multiline=true`, `trailingNewline=true` implicitly.
    JsDoc { tags: Vec<JsDocTag> },
}

impl LeadingComment {
    /// `LeadingComment.toString()` / `JSDocComment.toString()`.
    pub fn to_comment_string(&self) -> String {
        match self {
            LeadingComment::Plain {
                text, multiline, ..
            } => {
                if *multiline {
                    format!(" {text} ")
                } else {
                    text.clone()
                }
            }
            LeadingComment::JsDoc { tags } => serialize_tags(tags),
        }
    }
}

/// `JSDocTag` — `{ tagName?, text? }` (at least one present in valid usage).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsDocTag {
    pub tag_name: Option<String>,
    pub text: Option<String>,
}

// ---------------------------------------------------------------------------
// Expression.
// ---------------------------------------------------------------------------

/// Common metadata carried by every expression node (`type`, `sourceSpan`, `leadingComments`).
/// `is_equivalent` ignores all of these.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExprMeta {
    pub ty: Option<Box<Type>>,
    pub span: Option<ParseSourceSpan>,
    pub leading_comments: Vec<LeadingComment>,
}

/// The `o.Expression` node: kind + shared metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub meta: ExprMeta,
}

/// All ~28 `ExpressionVisitor` leaf variants. Field names mirror the source.
#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    /// `ReadVarExpr { name }`.
    ReadVar { name: String },
    /// `TypeofExpr { expr }`.
    Typeof(Box<Expr>),
    /// `VoidExpr { expr }`.
    Void(Box<Expr>),
    /// `WrappedNodeExpr<T> { node }`.
    WrappedNode(WrappedNodeHandle),
    /// `InvokeFunctionExpr { fn, args, pure, isOptional }` (`receiver` aliases `fn`).
    Invoke {
        callee: Box<Expr>,
        args: Vec<Expr>,
        pure: bool,
        optional: bool,
    },
    /// `TaggedTemplateLiteralExpr { tag, template }`.
    TaggedTemplate {
        tag: Box<Expr>,
        template: Box<Expr>,
    },
    /// `InstantiateExpr { classExpr, args }`.
    New {
        class_expr: Box<Expr>,
        args: Vec<Expr>,
    },
    /// `RegularExpressionLiteralExpr { body, flags }`.
    RegExpLiteral {
        body: String,
        flags: Option<String>,
    },
    /// `LiteralExpr { value }`.
    Literal(LiteralValue),
    /// `TemplateLiteralExpr { elements, expressions }`.
    TemplateLiteral {
        elements: Vec<TemplateLiteralElement>,
        expressions: Vec<Expr>,
    },
    /// `TemplateLiteralElementExpr { text, rawText }`.
    TemplateLiteralElement(TemplateLiteralElement),
    /// `LocalizedString { metaBlock, messageParts, placeHolderNames, expressions }`.
    LocalizedString {
        meta: I18nMeta,
        message_parts: Vec<LiteralPiece>,
        placeholders: Vec<PlaceholderPiece>,
        expressions: Vec<Expr>,
    },
    /// `ExternalExpr { value, typeParams }`.
    External {
        value: ExternalReference,
        type_params: Option<Vec<Type>>,
    },
    /// `ConditionalExpr { condition, trueCase, falseCase }`.
    Conditional {
        condition: Box<Expr>,
        true_case: Box<Expr>,
        false_case: Option<Box<Expr>>,
    },
    /// `DynamicImportExpr { url, urlComment }`.
    DynamicImport {
        url: ImportUrl,
        url_comment: Option<String>,
    },
    /// `NotExpr { condition }`.
    Not(Box<Expr>),
    /// `FunctionExpr { params, statements, name }`.
    Function {
        params: Vec<FnParam>,
        statements: Vec<Stmt>,
        name: Option<String>,
    },
    /// `ArrowFunctionExpr { params, body }`.
    Arrow {
        params: Vec<FnParam>,
        body: ArrowBody,
    },
    /// `UnaryOperatorExpr { operator, expr, parens }`.
    Unary {
        op: UnaryOperator,
        expr: Box<Expr>,
        parens: bool,
    },
    /// `ParenthesizedExpr { expr }`.
    Parenthesized(Box<Expr>),
    /// `BinaryOperatorExpr { operator, lhs, rhs }`.
    Binary {
        op: BinaryOperator,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// `ReadPropExpr { receiver, name, isOptional }` (`index` aliases `name`).
    ReadProp {
        receiver: Box<Expr>,
        name: String,
        optional: bool,
    },
    /// `ReadKeyExpr { receiver, index, isOptional }`.
    ReadKey {
        receiver: Box<Expr>,
        index: Box<Expr>,
        optional: bool,
    },
    /// `LiteralArrayExpr { entries }`.
    LiteralArray(Vec<Expr>),
    /// `LiteralMapExpr { entries, valueType }`.
    LiteralMap {
        entries: Vec<LiteralMapEntry>,
        value_type: Option<Box<Type>>,
    },
    /// `CommaExpr { parts }`.
    Comma(Vec<Expr>),
    /// `SpreadElementExpr { expression }`.
    Spread(Box<Expr>),
}

impl Expr {
    /// Construct an expression with no metadata.
    pub fn bare(kind: ExprKind) -> Expr {
        Expr {
            kind,
            meta: ExprMeta::default(),
        }
    }

    /// Construct with an explicit (optional) type.
    pub fn with_type(kind: ExprKind, ty: Option<Type>) -> Expr {
        Expr {
            kind,
            meta: ExprMeta {
                ty: ty.map(Box::new),
                ..ExprMeta::default()
            },
        }
    }

    // -- Fluent builders (return new nodes), mirroring `Expression`'s methods. --

    /// `.prop(name)` → `ReadPropExpr`.
    pub fn prop(self, name: impl Into<String>) -> Expr {
        Expr::bare(ExprKind::ReadProp {
            receiver: Box::new(self),
            name: name.into(),
            optional: false,
        })
    }

    /// `.key(index)` → `ReadKeyExpr`.
    pub fn key(self, index: Expr) -> Expr {
        Expr::bare(ExprKind::ReadKey {
            receiver: Box::new(self),
            index: Box::new(index),
            optional: false,
        })
    }

    /// `.callFn(args, _, pure)` → `InvokeFunctionExpr`.
    pub fn call_fn(self, args: Vec<Expr>, pure: bool) -> Expr {
        Expr::bare(ExprKind::Invoke {
            callee: Box::new(self),
            args,
            pure,
            optional: false,
        })
    }

    /// `.instantiate(args)` → `InstantiateExpr`.
    pub fn instantiate(self, args: Vec<Expr>) -> Expr {
        Expr::bare(ExprKind::New {
            class_expr: Box::new(self),
            args,
        })
    }

    /// `.conditional(trueCase, falseCase)` → `ConditionalExpr`. Default type inherited from
    /// `trueCase` (per ctor `super(type || trueCase.type)`).
    pub fn conditional(self, true_case: Expr, false_case: Option<Expr>) -> Expr {
        let ty = true_case.meta.ty.clone();
        Expr {
            kind: ExprKind::Conditional {
                condition: Box::new(self),
                true_case: Box::new(true_case),
                false_case: false_case.map(Box::new),
            },
            meta: ExprMeta {
                ty,
                ..ExprMeta::default()
            },
        }
    }

    fn binary(self, op: BinaryOperator, rhs: Expr) -> Expr {
        // BinaryOperatorExpr inherits lhs.type when no explicit type.
        let ty = self.meta.ty.clone();
        Expr {
            kind: ExprKind::Binary {
                op,
                lhs: Box::new(self),
                rhs: Box::new(rhs),
            },
            meta: ExprMeta {
                ty,
                ..ExprMeta::default()
            },
        }
    }

    pub fn equals(self, rhs: Expr) -> Expr {
        self.binary(BinaryOperator::Equals, rhs)
    }
    pub fn not_equals(self, rhs: Expr) -> Expr {
        self.binary(BinaryOperator::NotEquals, rhs)
    }
    pub fn identical(self, rhs: Expr) -> Expr {
        self.binary(BinaryOperator::Identical, rhs)
    }
    pub fn not_identical(self, rhs: Expr) -> Expr {
        self.binary(BinaryOperator::NotIdentical, rhs)
    }
    pub fn minus(self, rhs: Expr) -> Expr {
        self.binary(BinaryOperator::Minus, rhs)
    }
    pub fn plus(self, rhs: Expr) -> Expr {
        self.binary(BinaryOperator::Plus, rhs)
    }
    pub fn divide(self, rhs: Expr) -> Expr {
        self.binary(BinaryOperator::Divide, rhs)
    }
    pub fn multiply(self, rhs: Expr) -> Expr {
        self.binary(BinaryOperator::Multiply, rhs)
    }
    pub fn modulo(self, rhs: Expr) -> Expr {
        self.binary(BinaryOperator::Modulo, rhs)
    }
    pub fn power(self, rhs: Expr) -> Expr {
        self.binary(BinaryOperator::Exponentiation, rhs)
    }
    pub fn and(self, rhs: Expr) -> Expr {
        self.binary(BinaryOperator::And, rhs)
    }
    pub fn bitwise_or(self, rhs: Expr) -> Expr {
        self.binary(BinaryOperator::BitwiseOr, rhs)
    }
    pub fn bitwise_and(self, rhs: Expr) -> Expr {
        self.binary(BinaryOperator::BitwiseAnd, rhs)
    }
    pub fn or(self, rhs: Expr) -> Expr {
        self.binary(BinaryOperator::Or, rhs)
    }
    pub fn lower(self, rhs: Expr) -> Expr {
        self.binary(BinaryOperator::Lower, rhs)
    }
    pub fn lower_equals(self, rhs: Expr) -> Expr {
        self.binary(BinaryOperator::LowerEquals, rhs)
    }
    pub fn bigger(self, rhs: Expr) -> Expr {
        self.binary(BinaryOperator::Bigger, rhs)
    }
    pub fn bigger_equals(self, rhs: Expr) -> Expr {
        self.binary(BinaryOperator::BiggerEquals, rhs)
    }
    pub fn nullish_coalesce(self, rhs: Expr) -> Expr {
        self.binary(BinaryOperator::NullishCoalesce, rhs)
    }

    /// `.isBlank()` == `this.equals(TYPED_NULL_EXPR)`.
    pub fn is_blank(self) -> Expr {
        self.equals(typed_null_expr())
    }

    /// `ReadVarExpr.set(value)` / generic assign helper → `BinaryOperatorExpr(Assign, this, value)`.
    pub fn set(self, value: Expr) -> Expr {
        Expr::bare(ExprKind::Binary {
            op: BinaryOperator::Assign,
            lhs: Box::new(self),
            rhs: Box::new(value),
        })
    }

    /// `.toStmt()` → wraps in an `ExpressionStatement`.
    pub fn to_stmt(self) -> Stmt {
        Stmt::bare(StmtKind::Expression(self))
    }

    /// `Expression.isConstant()` — hoistability predicate (see spec §4.2).
    pub fn is_constant(&self) -> bool {
        match &self.kind {
            // Literals are constant.
            ExprKind::Literal(_)
            | ExprKind::RegExpLiteral { .. }
            | ExprKind::TemplateLiteralElement(_) => true,
            // Arrays/maps are constant iff every entry is.
            ExprKind::LiteralArray(entries) => entries.iter().all(Expr::is_constant),
            ExprKind::LiteralMap { entries, .. } => {
                entries.iter().all(LiteralMapEntry::is_constant)
            }
            // Delegating wrappers.
            ExprKind::Typeof(e)
            | ExprKind::Void(e)
            | ExprKind::Parenthesized(e)
            | ExprKind::Spread(e) => e.is_constant(),
            // Everything else is non-constant.
            _ => false,
        }
    }

    /// `Expression.isEquivalent(e)` — structural equality (see spec §4.1). Ignores
    /// `type`, `sourceSpan`, `leadingComments`.
    pub fn is_equivalent(&self, other: &Expr) -> bool {
        use ExprKind::*;
        match (&self.kind, &other.kind) {
            (ReadVar { name: a }, ReadVar { name: b }) => a == b,
            (Typeof(a), Typeof(b)) => a.is_equivalent(b),
            (Void(a), Void(b)) => a.is_equivalent(b),
            // WrappedNodeExpr: reference identity on the node handle.
            (WrappedNode(a), WrappedNode(b)) => a == b,
            (
                Invoke {
                    callee: c1,
                    args: a1,
                    pure: p1,
                    ..
                },
                Invoke {
                    callee: c2,
                    args: a2,
                    pure: p2,
                    ..
                },
            ) => {
                // Note: compares `pure` but NOT `isOptional` (per source asymmetry).
                c1.is_equivalent(c2) && are_all_equivalent(a1, a2) && p1 == p2
            }
            (
                TaggedTemplate {
                    tag: t1,
                    template: tpl1,
                },
                TaggedTemplate {
                    tag: t2,
                    template: tpl2,
                },
            ) => t1.is_equivalent(t2) && tpl1.is_equivalent(tpl2),
            (
                New {
                    class_expr: c1,
                    args: a1,
                },
                New {
                    class_expr: c2,
                    args: a2,
                },
            ) => c1.is_equivalent(c2) && are_all_equivalent(a1, a2),
            (
                RegExpLiteral {
                    body: b1,
                    flags: f1,
                },
                RegExpLiteral {
                    body: b2,
                    flags: f2,
                },
            ) => b1 == b2 && f1 == f2,
            (Literal(a), Literal(b)) => a == b,
            (
                TemplateLiteral {
                    elements: e1,
                    expressions: x1,
                },
                TemplateLiteral {
                    elements: e2,
                    expressions: x2,
                },
            ) => {
                // Source compares element `text` only (not rawText) for TemplateLiteralExpr.
                e1.len() == e2.len()
                    && e1.iter().zip(e2).all(|(a, b)| a.text == b.text)
                    && are_all_equivalent(x1, x2)
            }
            (TemplateLiteralElement(a), TemplateLiteralElement(b)) => {
                a.text == b.text && a.raw_text == b.raw_text
            }
            // LocalizedString.isEquivalent always returns false (intentional; see spec §4.1/§7.3).
            (LocalizedString { .. }, _) => false,
            (
                External {
                    value: v1, ..
                },
                External {
                    value: v2, ..
                },
            ) => v1.name == v2.name && v1.module_name == v2.module_name,
            (
                Conditional {
                    condition: c1,
                    true_case: t1,
                    false_case: f1,
                },
                Conditional {
                    condition: c2,
                    true_case: t2,
                    false_case: f2,
                },
            ) => {
                c1.is_equivalent(c2)
                    && t1.is_equivalent(t2)
                    && null_safe_is_equivalent(f1.as_deref(), f2.as_deref())
            }
            (
                DynamicImport {
                    url: u1,
                    url_comment: c1,
                },
                DynamicImport {
                    url: u2,
                    url_comment: c2,
                },
            ) => import_url_eq(u1, u2) && c1 == c2,
            (Not(a), Not(b)) => a.is_equivalent(b),
            // FunctionExpr matches FunctionExpr OR DeclareFunctionStmt (handled via stmt path
            // too); here we compare params + statements.
            (
                Function {
                    params: p1,
                    statements: s1,
                    ..
                },
                Function {
                    params: p2,
                    statements: s2,
                    ..
                },
            ) => fn_params_equivalent(p1, p2) && stmts_equivalent(s1, s2),
            (Arrow { params: p1, body: b1 }, Arrow { params: p2, body: b2 }) => {
                if !fn_params_equivalent(p1, p2) {
                    return false;
                }
                match (b1, b2) {
                    (ArrowBody::Expr(e1), ArrowBody::Expr(e2)) => e1.is_equivalent(e2),
                    (ArrowBody::Block(s1), ArrowBody::Block(s2)) => stmts_equivalent(s1, s2),
                    _ => false,
                }
            }
            (
                Unary {
                    op: o1, expr: e1, ..
                },
                Unary {
                    op: o2, expr: e2, ..
                },
            ) => o1 == o2 && e1.is_equivalent(e2),
            (Parenthesized(a), Parenthesized(b)) => a.is_equivalent(b),
            (
                Binary {
                    op: o1,
                    lhs: l1,
                    rhs: r1,
                },
                Binary {
                    op: o2,
                    lhs: l2,
                    rhs: r2,
                },
            ) => o1 == o2 && l1.is_equivalent(l2) && r1.is_equivalent(r2),
            (
                ReadProp {
                    receiver: r1,
                    name: n1,
                    optional: o1,
                },
                ReadProp {
                    receiver: r2,
                    name: n2,
                    optional: o2,
                },
            ) => r1.is_equivalent(r2) && n1 == n2 && o1 == o2,
            (
                ReadKey {
                    receiver: r1,
                    index: i1,
                    optional: o1,
                },
                ReadKey {
                    receiver: r2,
                    index: i2,
                    optional: o2,
                },
            ) => r1.is_equivalent(r2) && i1.is_equivalent(i2) && o1 == o2,
            (LiteralArray(a), LiteralArray(b)) => are_all_equivalent(a, b),
            (LiteralMap { entries: a, .. }, LiteralMap { entries: b, .. }) => {
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.is_equivalent(y))
            }
            (Comma(a), Comma(b)) => are_all_equivalent(a, b),
            (Spread(a), Spread(b)) => a.is_equivalent(b),
            _ => false,
        }
    }
}

fn import_url_eq(a: &ImportUrl, b: &ImportUrl) -> bool {
    match (a, b) {
        (ImportUrl::Str(s1), ImportUrl::Str(s2)) => s1 == s2,
        (ImportUrl::Expr(e1), ImportUrl::Expr(e2)) => e1.is_equivalent(e2),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Statements.
// ---------------------------------------------------------------------------

/// `StmtModifier` — `None=0, Final=1<<0, Private=1<<1, Exported=1<<2, Static=1<<3`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StmtModifier(pub u8);

impl StmtModifier {
    pub const NONE: StmtModifier = StmtModifier(0);
    pub const FINAL: StmtModifier = StmtModifier(1 << 0);
    pub const PRIVATE: StmtModifier = StmtModifier(1 << 1);
    pub const EXPORTED: StmtModifier = StmtModifier(1 << 2);
    pub const STATIC: StmtModifier = StmtModifier(1 << 3);

    #[inline]
    pub fn has_modifier(self, modifier: StmtModifier) -> bool {
        (self.0 & modifier.0) != 0
    }

    #[inline]
    pub fn bits(self) -> u8 {
        self.0
    }
}

impl std::ops::BitOr for StmtModifier {
    type Output = StmtModifier;
    fn bitor(self, rhs: StmtModifier) -> StmtModifier {
        StmtModifier(self.0 | rhs.0)
    }
}

/// Common metadata shared by every statement (`modifiers`, `sourceSpan`, `leadingComments`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StmtMeta {
    pub modifiers: StmtModifier,
    pub span: Option<ParseSourceSpan>,
    pub leading_comments: Vec<LeadingComment>,
}

/// The `o.Statement` node: kind + shared metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub meta: StmtMeta,
}

/// The 5 `StatementVisitor` leaf variants (no loops/switch/try at this layer).
#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    /// `DeclareVarStmt { name, value, type }`.
    DeclareVar {
        name: String,
        value: Option<Expr>,
        ty: Option<Type>,
    },
    /// `DeclareFunctionStmt { name, params, statements, type }`.
    DeclareFunction {
        name: String,
        params: Vec<FnParam>,
        statements: Vec<Stmt>,
        ty: Option<Type>,
    },
    /// `ExpressionStatement { expr }`.
    Expression(Expr),
    /// `ReturnStatement { value }`.
    Return(Expr),
    /// `IfStmt { condition, trueCase, falseCase }`.
    If {
        condition: Expr,
        true_case: Vec<Stmt>,
        false_case: Vec<Stmt>,
    },
}

impl Stmt {
    pub fn bare(kind: StmtKind) -> Stmt {
        Stmt {
            kind,
            meta: StmtMeta::default(),
        }
    }

    pub fn with_modifiers(kind: StmtKind, modifiers: StmtModifier) -> Stmt {
        Stmt {
            kind,
            meta: StmtMeta {
                modifiers,
                ..StmtMeta::default()
            },
        }
    }

    pub fn has_modifier(&self, modifier: StmtModifier) -> bool {
        self.meta.modifiers.has_modifier(modifier)
    }

    /// `addLeadingComment(comment)`.
    pub fn add_leading_comment(&mut self, comment: LeadingComment) {
        self.meta.leading_comments.push(comment);
    }

    /// `Statement.isEquivalent(stmt)` — structural equality. Note `DeclareFunctionStmt`
    /// is also `isEquivalent` to a `FunctionExpr` in source via `FunctionExpr.isEquivalent`;
    /// the cross-kind comparison is provided by [`Stmt::is_equivalent_to_function`].
    pub fn is_equivalent(&self, other: &Stmt) -> bool {
        use StmtKind::*;
        match (&self.kind, &other.kind) {
            (
                DeclareVar {
                    name: n1,
                    value: v1,
                    ..
                },
                DeclareVar {
                    name: n2,
                    value: v2,
                    ..
                },
            ) => {
                n1 == n2
                    && match (v1, v2) {
                        (Some(a), Some(b)) => a.is_equivalent(b),
                        (None, None) => true,
                        _ => false,
                    }
            }
            (
                DeclareFunction {
                    params: p1,
                    statements: s1,
                    ..
                },
                DeclareFunction {
                    params: p2,
                    statements: s2,
                    ..
                },
            ) => fn_params_equivalent(p1, p2) && stmts_equivalent(s1, s2),
            (Expression(a), Expression(b)) => a.is_equivalent(b),
            (Return(a), Return(b)) => a.is_equivalent(b),
            (
                If {
                    condition: c1,
                    true_case: t1,
                    false_case: f1,
                },
                If {
                    condition: c2,
                    true_case: t2,
                    false_case: f2,
                },
            ) => c1.is_equivalent(c2) && stmts_equivalent(t1, t2) && stmts_equivalent(f1, f2),
            _ => false,
        }
    }

    /// `DeclareFunctionStmt`/`FunctionExpr` cross-equivalence (params + statements).
    pub fn is_equivalent_to_function(
        &self,
        params: &[FnParam],
        statements: &[Stmt],
    ) -> bool {
        match &self.kind {
            StmtKind::DeclareFunction {
                params: p,
                statements: s,
                ..
            } => fn_params_equivalent(p, params) && stmts_equivalent(s, statements),
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Equality helpers (`nullSafeIsEquivalent`, `areAllEquivalent`).
// ---------------------------------------------------------------------------

/// `nullSafeIsEquivalent` — both None → equal; one None → not equal; else delegate.
pub fn null_safe_is_equivalent(base: Option<&Expr>, other: Option<&Expr>) -> bool {
    match (base, other) {
        (None, None) => true,
        (Some(a), Some(b)) => a.is_equivalent(b),
        _ => false,
    }
}

/// `areAllEquivalent` over expression slices (length check + pairwise).
pub fn are_all_equivalent(base: &[Expr], other: &[Expr]) -> bool {
    base.len() == other.len() && base.iter().zip(other).all(|(a, b)| a.is_equivalent(b))
}

fn fn_params_equivalent(base: &[FnParam], other: &[FnParam]) -> bool {
    base.len() == other.len() && base.iter().zip(other).all(|(a, b)| a.is_equivalent(b))
}

fn stmts_equivalent(base: &[Stmt], other: &[Stmt]) -> bool {
    base.len() == other.len() && base.iter().zip(other).all(|(a, b)| a.is_equivalent(b))
}

// ---------------------------------------------------------------------------
// Singleton literal expr consts (`NULL_EXPR`, `TYPED_NULL_EXPR`).
// ---------------------------------------------------------------------------

/// `NULL_EXPR = new LiteralExpr(null, null, null)`.
pub fn null_expr() -> Expr {
    Expr::bare(ExprKind::Literal(LiteralValue::Null))
}

/// `TYPED_NULL_EXPR = new LiteralExpr(null, INFERRED_TYPE, null)`.
pub fn typed_null_expr() -> Expr {
    Expr::with_type(ExprKind::Literal(LiteralValue::Null), Some(inferred_type()))
}

// ---------------------------------------------------------------------------
// Free factory functions (§2.4).
// ---------------------------------------------------------------------------

/// `leadingComment(text, multiline=false, trailingNewline=true)`.
pub fn leading_comment(
    text: impl Into<String>,
    multiline: bool,
    trailing_newline: bool,
) -> LeadingComment {
    LeadingComment::Plain {
        text: text.into(),
        multiline,
        trailing_newline,
    }
}

/// `jsDocComment(tags=[])`.
pub fn js_doc_comment(tags: Vec<JsDocTag>) -> LeadingComment {
    LeadingComment::JsDoc { tags }
}

/// `variable(name, type?)` → `ReadVarExpr`.
pub fn variable(name: impl Into<String>, ty: Option<Type>) -> Expr {
    Expr::with_type(ExprKind::ReadVar { name: name.into() }, ty)
}

/// `importExpr(id, typeParams=null)` → `ExternalExpr`.
pub fn import_expr(id: ExternalReference, type_params: Option<Vec<Type>>) -> Expr {
    Expr::bare(ExprKind::External {
        value: id,
        type_params,
    })
}

/// `expressionType(expr, typeModifiers?, typeParams?)` → `ExpressionType`.
pub fn expression_type(
    expr: Expr,
    type_modifiers: Option<TypeModifier>,
    type_params: Option<Vec<Type>>,
) -> Type {
    Type::Expression {
        value: Box::new(expr),
        type_params,
        modifiers: type_modifiers.unwrap_or(TypeModifier::NONE),
    }
}

/// `importType(id, typeParams?, typeModifiers?)` → `ExpressionType`.
pub fn import_type(
    id: ExternalReference,
    type_params: Option<Vec<Type>>,
    type_modifiers: Option<TypeModifier>,
) -> Type {
    expression_type(import_expr(id, type_params), type_modifiers, None)
}

/// `transplantedType(type, typeModifiers?)` → `TransplantedType`.
pub fn transplanted_type(node: WrappedNodeHandle, type_modifiers: Option<TypeModifier>) -> Type {
    Type::Transplanted {
        node,
        modifiers: type_modifiers.unwrap_or(TypeModifier::NONE),
    }
}

/// `typeofExpr(expr)` → `TypeofExpr`.
pub fn typeof_expr(expr: Expr) -> Expr {
    Expr::bare(ExprKind::Typeof(Box::new(expr)))
}

/// `literalArr(values, type?)` → `LiteralArrayExpr`.
pub fn literal_arr(values: Vec<Expr>, ty: Option<Type>) -> Expr {
    Expr::with_type(ExprKind::LiteralArray(values), ty)
}

/// `spread(expr)` → `SpreadElementExpr` (`...expr`), used as an entry of a `LiteralArrayExpr`
/// or as a call argument. Object-property spreads use [`LiteralMapEntry::Spread`] instead.
pub fn spread(expr: Expr) -> Expr {
    Expr::bare(ExprKind::Spread(Box::new(expr)))
}

/// `literalMap(values, type=null)` → `LiteralMapExpr` of property assignments.
pub fn literal_map(values: Vec<(String, bool, Expr)>, value_type: Option<Type>) -> Expr {
    let entries = values
        .into_iter()
        .map(|(key, quoted, value)| LiteralMapEntry::Property { key, value, quoted })
        .collect();
    Expr::bare(ExprKind::LiteralMap {
        entries,
        value_type: value_type.map(Box::new),
    })
}

/// `unary(operator, expr, type?)` → `UnaryOperatorExpr` (defaults type to NUMBER_TYPE).
pub fn unary(op: UnaryOperator, expr: Expr, ty: Option<Type>) -> Expr {
    Expr::with_type(
        ExprKind::Unary {
            op,
            expr: Box::new(expr),
            parens: true,
        },
        Some(ty.unwrap_or_else(number_type)),
    )
}

/// `not(expr)` → `NotExpr` (forces BOOL_TYPE).
pub fn not(expr: Expr) -> Expr {
    Expr::with_type(ExprKind::Not(Box::new(expr)), Some(bool_type()))
}

/// `fn(params, body, type?, _, name?)` → `FunctionExpr`. (`fn` is a Rust keyword → `fn_`.)
pub fn fn_(
    params: Vec<FnParam>,
    body: Vec<Stmt>,
    ty: Option<Type>,
    name: Option<String>,
) -> Expr {
    Expr::with_type(
        ExprKind::Function {
            params,
            statements: body,
            name,
        },
        ty,
    )
}

/// `arrowFn(params, body, type?)` → `ArrowFunctionExpr`.
pub fn arrow_fn(params: Vec<FnParam>, body: ArrowBody, ty: Option<Type>) -> Expr {
    Expr::with_type(ExprKind::Arrow { params, body }, ty)
}

/// `ifStmt(condition, thenClause, elseClause?)` → `IfStmt`.
pub fn if_stmt(condition: Expr, then_clause: Vec<Stmt>, else_clause: Option<Vec<Stmt>>) -> Stmt {
    Stmt::bare(StmtKind::If {
        condition,
        true_case: then_clause,
        false_case: else_clause.unwrap_or_default(),
    })
}

/// `taggedTemplate(tag, template, type?)` → `TaggedTemplateLiteralExpr`.
pub fn tagged_template(tag: Expr, template: Expr, ty: Option<Type>) -> Expr {
    Expr::with_type(
        ExprKind::TaggedTemplate {
            tag: Box::new(tag),
            template: Box::new(template),
        },
        ty,
    )
}

/// `literal(value, type?)` → `LiteralExpr`.
pub fn literal(value: LiteralValue, ty: Option<Type>) -> Expr {
    Expr::with_type(ExprKind::Literal(value), ty)
}

/// `localizedString(metaBlock, messageParts, placeholderNames, expressions)` → `LocalizedString`
/// (forces STRING_TYPE).
pub fn localized_string(
    meta: I18nMeta,
    message_parts: Vec<LiteralPiece>,
    placeholders: Vec<PlaceholderPiece>,
    expressions: Vec<Expr>,
) -> Expr {
    Expr::with_type(
        ExprKind::LocalizedString {
            meta,
            message_parts,
            placeholders,
            expressions,
        },
        Some(string_type()),
    )
}

/// `isNull(exp)` — true iff `exp` is a `LiteralExpr` whose value is `null`.
pub fn is_null(exp: &Expr) -> bool {
    matches!(exp.kind, ExprKind::Literal(LiteralValue::Null))
}

// ---------------------------------------------------------------------------
// `$localize` cooked/raw + JSDoc string serialization (golden-test sensitive).
// ---------------------------------------------------------------------------

const MEANING_SEPARATOR: &str = "|";
const ID_SEPARATOR: &str = "@@";
const LEGACY_ID_INDICATOR: char = '\u{241F}'; // ␟

fn escape_slashes(s: &str) -> String {
    s.replace('\\', "\\\\")
}

fn escape_starting_colon(s: &str) -> String {
    if let Some(rest) = s.strip_prefix(':') {
        format!("\\:{rest}")
    } else {
        s.to_string()
    }
}

fn escape_colons(s: &str) -> String {
    s.replace(':', "\\:")
}

fn escape_for_template_literal(s: &str) -> String {
    s.replace('`', "\\`").replace("${", "$\\{")
}

/// `createCookedRawString(metaBlock, messagePart, range)` — see spec §4.5.
pub fn create_cooked_raw_string(
    meta_block: &str,
    message_part: &str,
    range: Option<ParseSourceSpan>,
) -> CookedRawString {
    if meta_block.is_empty() {
        CookedRawString {
            cooked: message_part.to_string(),
            raw: escape_for_template_literal(&escape_starting_colon(&escape_slashes(message_part))),
            range,
        }
    } else {
        CookedRawString {
            cooked: format!(":{meta_block}:{message_part}"),
            raw: escape_for_template_literal(&format!(
                ":{}:{}",
                escape_colons(&escape_slashes(meta_block)),
                escape_slashes(message_part)
            )),
            range,
        }
    }
}

/// Build the `$localize` head metablock from an `I18nMeta` (mirrors `serializeI18nHead`).
pub fn serialize_i18n_meta_block(meta: &I18nMeta) -> String {
    let mut block = meta.description.clone().unwrap_or_default();
    if let Some(meaning) = &meta.meaning {
        block = format!("{meaning}{MEANING_SEPARATOR}{block}");
    }
    if let Some(custom_id) = &meta.custom_id {
        block = format!("{block}{ID_SEPARATOR}{custom_id}");
    }
    for legacy_id in &meta.legacy_ids {
        block = format!("{block}{LEGACY_ID_INDICATOR}{legacy_id}");
    }
    block
}

/// `tagToString(tag)` — `" @foo {bar} baz"`. Returns `Err` if text contains `/*` or `*/`.
pub fn tag_to_string(tag: &JsDocTag) -> Result<String, String> {
    let mut out = String::new();
    if let Some(tag_name) = &tag.tag_name
        && !tag_name.is_empty()
    {
        out.push_str(&format!(" @{tag_name}"));
    }
    if let Some(text) = &tag.text
        && !text.is_empty()
    {
        if text.contains("/*") || text.contains("*/") {
            return Err(r#"JSDoc text cannot contain "/*" and "*/""#.to_string());
        }
        out.push(' ');
        out.push_str(&text.replace('@', "\\@"));
    }
    Ok(out)
}

/// `serializeTags(tags)` — see spec §6. Panics on the same condition Angular throws on.
pub fn serialize_tags(tags: &[JsDocTag]) -> String {
    if tags.is_empty() {
        return String::new();
    }

    if tags.len() == 1
        && tags[0]
            .tag_name
            .as_ref()
            .is_some_and(|n| !n.is_empty())
        && tags[0].text.as_ref().is_none_or(|t| t.is_empty())
    {
        // `/** @tagname */` single-line form.
        return format!("*{} ", tag_to_string(&tags[0]).expect("invalid jsdoc tag text"));
    }

    let mut out = String::from("*\n");
    for tag in tags {
        out.push_str(" *");
        let tag_str = tag_to_string(tag).expect("invalid jsdoc tag text");
        out.push_str(&tag_str.replace('\n', "\n * "));
        out.push('\n');
    }
    out.push(' ');
    out
}

// ---------------------------------------------------------------------------
// Visitor trait + recursive walk (default traversal), mirroring RecursiveAstVisitor.
// ---------------------------------------------------------------------------

/// Visitor over the output AST. Default methods walk children (see [`walk_expr`]/[`walk_stmt`]).
/// Mirrors `RecursiveAstVisitor`. Note: `Conditional` guards a `None` `false_case` (Angular
/// non-null-asserts and would crash — see spec §4.4/§7.6).
pub trait Visitor {
    fn visit_expr(&mut self, expr: &Expr) {
        walk_expr(self, expr);
    }
    fn visit_stmt(&mut self, stmt: &Stmt) {
        walk_stmt(self, stmt);
    }
}

/// Default recursive descent over an expression's children.
pub fn walk_expr<V: Visitor + ?Sized>(visitor: &mut V, expr: &Expr) {
    match &expr.kind {
        ExprKind::ReadVar { .. }
        | ExprKind::WrappedNode(_)
        | ExprKind::Literal(_)
        | ExprKind::RegExpLiteral { .. }
        | ExprKind::TemplateLiteralElement(_)
        | ExprKind::External { .. } => {}
        ExprKind::Typeof(e)
        | ExprKind::Void(e)
        | ExprKind::Not(e)
        | ExprKind::Parenthesized(e)
        | ExprKind::Spread(e) => visitor.visit_expr(e),
        ExprKind::Unary { expr, .. } => visitor.visit_expr(expr),
        ExprKind::Invoke { callee, args, .. } => {
            visitor.visit_expr(callee);
            for a in args {
                visitor.visit_expr(a);
            }
        }
        ExprKind::TaggedTemplate { tag, template } => {
            visitor.visit_expr(tag);
            visitor.visit_expr(template);
        }
        ExprKind::New { class_expr, args } => {
            visitor.visit_expr(class_expr);
            for a in args {
                visitor.visit_expr(a);
            }
        }
        ExprKind::TemplateLiteral { expressions, .. } => {
            for e in expressions {
                visitor.visit_expr(e);
            }
        }
        ExprKind::LocalizedString { expressions, .. } => {
            for e in expressions {
                visitor.visit_expr(e);
            }
        }
        ExprKind::Conditional {
            condition,
            true_case,
            false_case,
        } => {
            visitor.visit_expr(condition);
            visitor.visit_expr(true_case);
            if let Some(fc) = false_case {
                visitor.visit_expr(fc);
            }
        }
        ExprKind::DynamicImport { url, .. } => {
            if let ImportUrl::Expr(e) = url {
                visitor.visit_expr(e);
            }
        }
        ExprKind::Function { statements, .. } => {
            for s in statements {
                visitor.visit_stmt(s);
            }
        }
        ExprKind::Arrow { body, .. } => match body {
            ArrowBody::Expr(e) => visitor.visit_expr(e),
            ArrowBody::Block(stmts) => {
                for s in stmts {
                    visitor.visit_stmt(s);
                }
            }
        },
        ExprKind::Binary { lhs, rhs, .. } => {
            visitor.visit_expr(lhs);
            visitor.visit_expr(rhs);
        }
        ExprKind::ReadProp { receiver, .. } => visitor.visit_expr(receiver),
        ExprKind::ReadKey { receiver, index, .. } => {
            visitor.visit_expr(receiver);
            visitor.visit_expr(index);
        }
        ExprKind::LiteralArray(entries) => {
            for e in entries {
                visitor.visit_expr(e);
            }
        }
        ExprKind::LiteralMap { entries, .. } => {
            for entry in entries {
                match entry {
                    LiteralMapEntry::Property { value, .. } => visitor.visit_expr(value),
                    LiteralMapEntry::Spread { expression } => visitor.visit_expr(expression),
                }
            }
        }
        ExprKind::Comma(parts) => {
            for p in parts {
                visitor.visit_expr(p);
            }
        }
    }
}

/// Default recursive descent over a statement's children.
pub fn walk_stmt<V: Visitor + ?Sized>(visitor: &mut V, stmt: &Stmt) {
    match &stmt.kind {
        StmtKind::DeclareVar { value, .. } => {
            if let Some(v) = value {
                visitor.visit_expr(v);
            }
        }
        StmtKind::DeclareFunction { statements, .. } => {
            for s in statements {
                visitor.visit_stmt(s);
            }
        }
        StmtKind::Expression(e) | StmtKind::Return(e) => visitor.visit_expr(e),
        StmtKind::If {
            condition,
            true_case,
            false_case,
        } => {
            visitor.visit_expr(condition);
            for s in true_case {
                visitor.visit_stmt(s);
            }
            for s in false_case {
                visitor.visit_stmt(s);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests — mirror the algorithms the spec calls out (isEquivalent, isConstant, clone,
// is_assignment, JSDoc/$localize serialization).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn lit_num(n: f64) -> Expr {
        Expr::bare(ExprKind::Literal(LiteralValue::Number(n)))
    }
    fn lit_str(s: &str) -> Expr {
        Expr::bare(ExprKind::Literal(LiteralValue::String(s.to_string())))
    }

    #[test]
    fn is_assignment_covers_all_compound_ops() {
        assert!(BinaryOperator::Assign.is_assignment());
        assert!(BinaryOperator::AdditionAssignment.is_assignment());
        assert!(BinaryOperator::NullishCoalesceAssignment.is_assignment());
        assert!(BinaryOperator::OrAssignment.is_assignment());
        assert!(!BinaryOperator::Plus.is_assignment());
        assert!(!BinaryOperator::Equals.is_assignment());
        assert!(!BinaryOperator::In.is_assignment());
        assert!(!BinaryOperator::InstanceOf.is_assignment());
    }

    #[test]
    fn binary_operator_has_all_31_variants() {
        // Spot-check the boundary variants exist (compile-time enumeration).
        let _ = [
            BinaryOperator::Equals,
            BinaryOperator::NullishCoalesceAssignment,
            BinaryOperator::In,
            BinaryOperator::InstanceOf,
            BinaryOperator::Exponentiation,
        ];
    }

    #[test]
    fn literal_equality_matches_js_strict() {
        // null !== undefined
        assert_ne!(LiteralValue::Null, LiteralValue::Undefined);
        // NaN !== NaN
        assert_ne!(
            LiteralValue::Number(f64::NAN),
            LiteralValue::Number(f64::NAN)
        );
        // -0 === 0
        assert_eq!(LiteralValue::Number(-0.0), LiteralValue::Number(0.0));
        assert_eq!(
            LiteralValue::String("x".into()),
            LiteralValue::String("x".into())
        );
    }

    #[test]
    fn is_equivalent_ignores_type_and_span() {
        let a = Expr::with_type(ExprKind::ReadVar { name: "v".into() }, Some(number_type()));
        let mut b = Expr::bare(ExprKind::ReadVar { name: "v".into() });
        b.meta.span = Some(ParseSourceSpan::new(0, 3));
        assert!(a.is_equivalent(&b));

        let c = Expr::bare(ExprKind::ReadVar { name: "w".into() });
        assert!(!a.is_equivalent(&c));
    }

    #[test]
    fn invoke_is_equivalent_compares_pure_not_optional() {
        let mk = |pure: bool, optional: bool| {
            Expr::bare(ExprKind::Invoke {
                callee: Box::new(variable("f", None)),
                args: vec![lit_num(1.0)],
                pure,
                optional,
            })
        };
        // Same pure, differing optional → equivalent (optional ignored).
        assert!(mk(true, false).is_equivalent(&mk(true, true)));
        // Differing pure → not equivalent.
        assert!(!mk(true, false).is_equivalent(&mk(false, false)));
    }

    #[test]
    fn localized_string_is_never_equivalent() {
        let ls = localized_string(I18nMeta::default(), vec![], vec![], vec![]);
        assert!(!ls.is_equivalent(&ls.clone()));
    }

    #[test]
    fn external_equivalence_uses_name_and_module() {
        let a = import_expr(
            ExternalReference::new(Some("m".into()), "n"),
            Some(vec![number_type()]),
        );
        let b = import_expr(ExternalReference::new(Some("m".into()), "n"), None);
        assert!(a.is_equivalent(&b)); // type_params ignored
        let c = import_expr(ExternalReference::new(Some("other".into()), "n"), None);
        assert!(!a.is_equivalent(&c));
    }

    #[test]
    fn is_constant_predicate() {
        assert!(lit_num(1.0).is_constant());
        assert!(literal_arr(vec![lit_num(1.0), lit_str("a")], None).is_constant());
        assert!(!literal_arr(vec![variable("x", None)], None).is_constant());
        assert!(!variable("x", None).is_constant());
        // Delegating wrapper.
        assert!(typeof_expr(lit_num(1.0)).is_constant());
        assert!(!typeof_expr(variable("x", None)).is_constant());
        // Map constant only if all entries constant.
        let const_map = literal_map(vec![("k".into(), false, lit_num(1.0))], None);
        assert!(const_map.is_constant());
        let nonconst_map = literal_map(vec![("k".into(), false, variable("x", None))], None);
        assert!(!nonconst_map.is_constant());
    }

    #[test]
    fn clone_is_deep_and_equivalent() {
        let e = Expr::bare(ExprKind::Binary {
            op: BinaryOperator::Plus,
            lhs: Box::new(lit_num(1.0)),
            rhs: Box::new(variable("x", None)),
        });
        let c = e.clone();
        assert!(e.is_equivalent(&c));
        assert_eq!(e, c);
    }

    #[test]
    fn arrow_body_equivalence_requires_same_shape() {
        let expr_body = arrow_fn(vec![], ArrowBody::Expr(Box::new(lit_num(1.0))), None);
        let block_body = arrow_fn(vec![], ArrowBody::Block(vec![]), None);
        assert!(!expr_body.is_equivalent(&block_body));
        assert!(expr_body.is_equivalent(&expr_body.clone()));
    }

    #[test]
    fn conditional_inherits_true_case_type() {
        let cond = variable("c", None).conditional(
            Expr::with_type(ExprKind::ReadVar { name: "t".into() }, Some(string_type())),
            Some(variable("f", None)),
        );
        assert_eq!(cond.meta.ty.as_deref(), Some(&string_type()));
    }

    #[test]
    fn not_forces_bool_type_and_unary_forces_number() {
        assert_eq!(not(variable("x", None)).meta.ty.as_deref(), Some(&bool_type()));
        assert_eq!(
            unary(UnaryOperator::Minus, lit_num(1.0), None)
                .meta
                .ty
                .as_deref(),
            Some(&number_type())
        );
    }

    #[test]
    fn stmt_modifier_flags() {
        let m = StmtModifier::EXPORTED | StmtModifier::STATIC;
        assert!(m.has_modifier(StmtModifier::EXPORTED));
        assert!(m.has_modifier(StmtModifier::STATIC));
        assert!(!m.has_modifier(StmtModifier::PRIVATE));
    }

    #[test]
    fn type_modifier_flags() {
        assert!(TypeModifier::CONST.has_modifier(TypeModifier::CONST));
        assert!(!TypeModifier::NONE.has_modifier(TypeModifier::CONST));
        let t = Type::Builtin {
            name: BuiltinTypeName::Bool,
            modifiers: TypeModifier::CONST,
        };
        assert!(t.has_modifier(TypeModifier::CONST));
    }

    #[test]
    fn is_null_helper() {
        assert!(is_null(&null_expr()));
        assert!(is_null(&typed_null_expr()));
        assert!(!is_null(&lit_num(0.0)));
    }

    #[test]
    fn serialize_tags_empty_and_single() {
        assert_eq!(serialize_tags(&[]), "");
        let single = vec![JsDocTag {
            tag_name: Some("desc".into()),
            text: None,
        }];
        assert_eq!(serialize_tags(&single), "* @desc ");
    }

    #[test]
    fn serialize_tags_multi() {
        let tags = vec![
            JsDocTag {
                tag_name: None,
                text: Some("Some description".into()),
            },
            JsDocTag {
                tag_name: Some("param".into()),
                text: Some("x the value".into()),
            },
        ];
        let out = serialize_tags(&tags);
        assert_eq!(out, "*\n * Some description\n * @param x the value\n ");
    }

    #[test]
    fn tag_to_string_rejects_comment_markers() {
        let bad = JsDocTag {
            tag_name: Some("desc".into()),
            text: Some("oops /* nested".into()),
        };
        assert!(tag_to_string(&bad).is_err());
    }

    #[test]
    fn cooked_raw_string_no_metablock() {
        let crs = create_cooked_raw_string("", ":leadingColon", None);
        assert_eq!(crs.cooked, ":leadingColon");
        // Leading ":" escaped, no metablock.
        assert_eq!(crs.raw, "\\:leadingColon");
    }

    #[test]
    fn cooked_raw_string_with_metablock() {
        let crs = create_cooked_raw_string("meaning|desc", "Hello", None);
        assert_eq!(crs.cooked, ":meaning|desc:Hello");
        // Only colons (and slashes/template chars) are escaped in the raw form, not "|".
        assert_eq!(crs.raw, ":meaning|desc:Hello");
    }

    #[test]
    fn template_literal_element_derives_raw_text() {
        let el = TemplateLiteralElement::new("a`b${c}", None);
        // backtick and ${ escaped, backslashes doubled (none here).
        assert_eq!(el.raw_text, "a\\`b$\\{c}");
    }

    #[test]
    fn stmt_is_equivalent_declare_var() {
        let a = Stmt::bare(StmtKind::DeclareVar {
            name: "x".into(),
            value: Some(lit_num(1.0)),
            ty: None,
        });
        let b = a.clone();
        assert!(a.is_equivalent(&b));
        let c = Stmt::bare(StmtKind::DeclareVar {
            name: "x".into(),
            value: None,
            ty: None,
        });
        assert!(!a.is_equivalent(&c));
    }

    #[test]
    fn visitor_counts_nested_exprs() {
        struct Counter {
            count: usize,
        }
        impl Visitor for Counter {
            fn visit_expr(&mut self, expr: &Expr) {
                self.count += 1;
                walk_expr(self, expr);
            }
        }
        // (a + b) * c → 3 leaf vars + 2 binary = 5 expressions visited.
        let tree = variable("a", None)
            .plus(variable("b", None))
            .multiply(variable("c", None));
        let mut counter = Counter { count: 0 };
        counter.visit_expr(&tree);
        assert_eq!(counter.count, 5);
    }
}
