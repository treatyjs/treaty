//! AST node types for Angular binding expressions (Binary, PropertyRead, Call, Pipe, ...).
//! PORT TARGET: `migration/render3-specs/04-expr_ast.md`
//! Source: `tools/angular-ref/packages/compiler/src/expression_parser/ast.ts`
//!
//! Per `migration/PORT-ARCHITECTURE.md`, this IR is OWNED and ARENA-FREE: we use
//! `Box`/`Vec`/`String`, NOT oxc arena lifetimes. The spec (04) suggests an arena
//! model, but the architecture document overrides that; OXC arena/codegen only enters
//! at a later lowering step.
//!
//! The TS `AST` class hierarchy is mapped to a wrapper struct [`AstNode`] holding the
//! common spans plus an [`ExprKind`] enum with one variant per concrete TS class.
//! Virtual `visit()` dispatch becomes a `match` in [`AstVisitor::walk`].

// ---------------------------------------------------------------------------
// Local placeholder types for not-yet-ported sibling modules.
// ---------------------------------------------------------------------------
//
// These mirror `parse_util.ts` (`ParseSourceSpan`, `ParseError`) and `core`
// (`SecurityContext`). They are intentionally minimal stubs so this module
// compiles standalone; replace with real ports when those modules land.

/// Placeholder for `parse_util.ParseSourceSpan` (location-based span: file +
/// offset + line/col). Distinct from [`AbsoluteSourceSpan`], which is an
/// offset-only pair. Real port lives in the future `parse_util` module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseSourceSpan {
    pub start: u32,
    pub end: u32,
}

/// Placeholder for `parse_util.ParseError`. Real port lives in `parse_util`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub span: ParseSourceSpan,
    pub msg: String,
}

/// Placeholder for `core.SecurityContext` (re-exported from
/// `schema/dom_security_schema`). Real port lives in `core`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecurityContext {
    None,
    Html,
    Style,
    Script,
    Url,
    ResourceUrl,
}

// ---------------------------------------------------------------------------
// Spans (Copy value types, no lifetimes).
// ---------------------------------------------------------------------------

/// `ParseSpan` — a relative [start, end) offset pair within the parsed source
/// fragment. Mirrors `expression_parser/ast.ts#ParseSpan`.
///
/// Offsets are `u32` (OXC convention). Angular uses JS `number` (UTF-16 code
/// units); a UTF-16->UTF-8 remap is only required at the OXC boundary, not here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseSpan {
    pub start: u32,
    pub end: u32,
}

impl ParseSpan {
    pub fn new(start: u32, end: u32) -> Self {
        ParseSpan { start, end }
    }

    /// Mirrors `ParseSpan.toAbsolute(absoluteOffset)`.
    pub fn to_absolute(self, absolute_offset: u32) -> AbsoluteSourceSpan {
        AbsoluteSourceSpan {
            start: absolute_offset + self.start,
            end: absolute_offset + self.end,
        }
    }
}

/// `AbsoluteSourceSpan` — absolute [start, end) byte offsets in the source file.
/// Mirrors `expression_parser/ast.ts#AbsoluteSourceSpan`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AbsoluteSourceSpan {
    pub start: u32,
    pub end: u32,
}

impl AbsoluteSourceSpan {
    pub fn new(start: u32, end: u32) -> Self {
        AbsoluteSourceSpan { start, end }
    }
}

// ---------------------------------------------------------------------------
// Operators.
// ---------------------------------------------------------------------------

/// `AssignmentOperation` — the 10 assignment tokens. Mirrors the exported TS
/// string union `AssignmentOperation`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignmentOperation {
    Assign,      // =
    AddAssign,   // +=
    SubAssign,   // -=
    MulAssign,   // *=
    DivAssign,   // /=
    ModAssign,   // %=
    PowAssign,   // **=
    AndAssign,   // &&=
    OrAssign,    // ||=
    NullishAssign, // ??=
}

impl AssignmentOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            AssignmentOperation::Assign => "=",
            AssignmentOperation::AddAssign => "+=",
            AssignmentOperation::SubAssign => "-=",
            AssignmentOperation::MulAssign => "*=",
            AssignmentOperation::DivAssign => "/=",
            AssignmentOperation::ModAssign => "%=",
            AssignmentOperation::PowAssign => "**=",
            AssignmentOperation::AndAssign => "&&=",
            AssignmentOperation::OrAssign => "||=",
            AssignmentOperation::NullishAssign => "??=",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "=" => AssignmentOperation::Assign,
            "+=" => AssignmentOperation::AddAssign,
            "-=" => AssignmentOperation::SubAssign,
            "*=" => AssignmentOperation::MulAssign,
            "/=" => AssignmentOperation::DivAssign,
            "%=" => AssignmentOperation::ModAssign,
            "**=" => AssignmentOperation::PowAssign,
            "&&=" => AssignmentOperation::AndAssign,
            "||=" => AssignmentOperation::OrAssign,
            "??=" => AssignmentOperation::NullishAssign,
            _ => return None,
        })
    }
}

/// `BinaryOperation` — the full set of binary operators Angular allows in a
/// template expression. Mirrors the (un-exported) TS `BinaryOperation` union:
/// it folds the assignment operations in plus logical/equality/relational/
/// additive/multiplicative/exponentiation operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOperation {
    Assignment(AssignmentOperation),
    // Logical
    And,        // &&
    Or,         // ||
    Nullish,    // ??
    // Equality
    Eq,         // ==
    Neq,        // !=
    Identity,   // ===
    NotIdentity, // !==
    // Relational
    Lt,         // <
    Gt,         // >
    Le,         // <=
    Ge,         // >=
    In,         // in
    Instanceof, // instanceof
    // Additive
    Add,        // +
    Sub,        // -
    // Multiplicative
    Mul,        // *
    Mod,        // %
    Div,        // /
    // Exponentiation
    Pow,        // **
}

impl BinaryOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            BinaryOperation::Assignment(a) => a.as_str(),
            BinaryOperation::And => "&&",
            BinaryOperation::Or => "||",
            BinaryOperation::Nullish => "??",
            BinaryOperation::Eq => "==",
            BinaryOperation::Neq => "!=",
            BinaryOperation::Identity => "===",
            BinaryOperation::NotIdentity => "!==",
            BinaryOperation::Lt => "<",
            BinaryOperation::Gt => ">",
            BinaryOperation::Le => "<=",
            BinaryOperation::Ge => ">=",
            BinaryOperation::In => "in",
            BinaryOperation::Instanceof => "instanceof",
            BinaryOperation::Add => "+",
            BinaryOperation::Sub => "-",
            BinaryOperation::Mul => "*",
            BinaryOperation::Mod => "%",
            BinaryOperation::Div => "/",
            BinaryOperation::Pow => "**",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        if let Some(a) = AssignmentOperation::from_str(s) {
            return Some(BinaryOperation::Assignment(a));
        }
        Some(match s {
            "&&" => BinaryOperation::And,
            "||" => BinaryOperation::Or,
            "??" => BinaryOperation::Nullish,
            "==" => BinaryOperation::Eq,
            "!=" => BinaryOperation::Neq,
            "===" => BinaryOperation::Identity,
            "!==" => BinaryOperation::NotIdentity,
            "<" => BinaryOperation::Lt,
            ">" => BinaryOperation::Gt,
            "<=" => BinaryOperation::Le,
            ">=" => BinaryOperation::Ge,
            "in" => BinaryOperation::In,
            "instanceof" => BinaryOperation::Instanceof,
            "+" => BinaryOperation::Add,
            "-" => BinaryOperation::Sub,
            "*" => BinaryOperation::Mul,
            "%" => BinaryOperation::Mod,
            "/" => BinaryOperation::Div,
            "**" => BinaryOperation::Pow,
            _ => return None,
        })
    }

    /// Mirrors the static `Binary.isAssignmentOperation`.
    pub fn is_assignment(self) -> bool {
        matches!(self, BinaryOperation::Assignment(_))
    }
}

/// Operator for [`ExprKind::Unary`]. In TS, `Unary` only allows `'+'` / `'-'`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOperator {
    Plus,  // +
    Minus, // -
}

// ---------------------------------------------------------------------------
// Leaf value / key / parameter helpers.
// ---------------------------------------------------------------------------

/// `LiteralPrimitive.value` — `string | number | boolean | null | undefined`.
/// `Null` and `Undefined` are kept distinct (they differ downstream).
#[derive(Clone, Debug, PartialEq)]
pub enum LiteralValue {
    Str(String),
    Num(f64),
    Bool(bool),
    Null,
    Undefined,
}

/// `LiteralMapKey` — discriminated union of property and spread keys, mirroring
/// `LiteralMapPropertyKey | LiteralMapSpreadKey`.
#[derive(Clone, Debug, PartialEq)]
pub enum LiteralMapKey {
    /// `kind: 'property'`.
    Property {
        key: String,
        quoted: bool,
        span: ParseSpan,
        source_span: AbsoluteSourceSpan,
        /// TS `isShorthandInitialized?` — optional, defaults to `false`.
        is_shorthand_initialized: bool,
    },
    /// `kind: 'spread'`.
    Spread {
        span: ParseSpan,
        source_span: AbsoluteSourceSpan,
    },
}

/// `ArrowFunctionIdentifierParameter` — a plain named parameter `(name) => …`.
#[derive(Clone, Debug, PartialEq)]
pub struct ArrowFunctionIdentifierParameter {
    pub name: String,
    pub span: ParseSpan,
    pub source_span: AbsoluteSourceSpan,
}

/// `ArrowFunctionRestParameter` — a rest parameter `(...name) => …`. The `name` is
/// the identifier the rest array binds to; `span`/`source_span` cover the whole
/// `...name` token range (including the leading `...`).
#[derive(Clone, Debug, PartialEq)]
pub struct ArrowFunctionRestParameter {
    pub name: String,
    pub span: ParseSpan,
    pub source_span: AbsoluteSourceSpan,
}

/// `ArrowFunctionParameter` — an identifier parameter or a trailing rest parameter.
/// (The TS `type ArrowFunctionParameter` historically aliased only the identifier
/// form; this port models the rest form as a distinct variant so `(...rest) => …`
/// lowers faithfully.)
#[derive(Clone, Debug, PartialEq)]
pub enum ArrowFunctionParameter {
    Identifier(ArrowFunctionIdentifierParameter),
    Rest(ArrowFunctionRestParameter),
}

/// A single literal/static text chunk of a template literal. Mirrors the
/// `TemplateLiteralElement` class (an `AST` subtype, but it only carries `text`
/// plus the common spans). Carried inline inside the `TemplateLiteral` variant.
#[derive(Clone, Debug, PartialEq)]
pub struct TemplateLiteralElement {
    pub span: ParseSpan,
    pub source_span: AbsoluteSourceSpan,
    pub text: String,
}

// ---------------------------------------------------------------------------
// The core expression node.
// ---------------------------------------------------------------------------

/// `AstNode` — a single expression node: the common `span` / `sourceSpan`
/// (factored out of every TS `AST` subclass) plus the per-class payload in
/// [`ExprKind`].
#[derive(Clone, Debug, PartialEq)]
pub struct AstNode {
    pub span: ParseSpan,
    pub source_span: AbsoluteSourceSpan,
    pub kind: ExprKind,
}

/// `ExprKind` — one variant per concrete TS `AST` subclass.
///
/// `name_span` (from the TS `ASTWithName` base) is stored inline on the
/// `PropertyRead` / `SafePropertyRead` / `BindingPipe` variants that need it.
#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    /// `EmptyExpr` — empty expression (no payload).
    EmptyExpr,
    /// `ImplicitReceiver` — the implicit component context.
    ImplicitReceiver,
    /// `ThisReceiver` — explicit `this`. Distinct from `ImplicitReceiver`
    /// (matters for scope resolution), though TS conflates them in some spots.
    ThisReceiver,
    /// `Chain` — multiple expressions separated by `;`.
    Chain { expressions: Vec<AstNode> },
    /// `Conditional` — `cond ? trueExp : falseExp`.
    Conditional {
        condition: Box<AstNode>,
        true_exp: Box<AstNode>,
        false_exp: Box<AstNode>,
    },
    /// `PropertyRead` — `receiver.name`.
    PropertyRead {
        name_span: AbsoluteSourceSpan,
        receiver: Box<AstNode>,
        name: String,
    },
    /// `SafePropertyRead` — `receiver?.name`.
    SafePropertyRead {
        name_span: AbsoluteSourceSpan,
        receiver: Box<AstNode>,
        name: String,
    },
    /// `KeyedRead` — `receiver[key]`.
    KeyedRead {
        receiver: Box<AstNode>,
        key: Box<AstNode>,
    },
    /// `SafeKeyedRead` — `receiver?.[key]`.
    SafeKeyedRead {
        receiver: Box<AstNode>,
        key: Box<AstNode>,
    },
    /// `BindingPipe` — `exp | name:arg0:arg1`.
    BindingPipe {
        name_span: AbsoluteSourceSpan,
        exp: Box<AstNode>,
        name: String,
        args: Vec<AstNode>,
        pipe_type: BindingPipeType,
    },
    /// `LiteralPrimitive` — string / number / boolean / null / undefined.
    LiteralPrimitive { value: LiteralValue },
    /// `LiteralArray` — `[a, b, ...]`.
    LiteralArray { expressions: Vec<AstNode> },
    /// `SpreadElement` — `...expression` (inside arrays / calls).
    SpreadElement { expression: Box<AstNode> },
    /// `LiteralMap` — `{k: v, ...spread}`. `keys` and `values` are parallel
    /// vectors; spread keys participate differently from property keys, so they
    /// are NOT zipped into pairs.
    LiteralMap {
        keys: Vec<LiteralMapKey>,
        values: Vec<AstNode>,
    },
    /// `Interpolation` — `{{ ... }}`. Invariant: `strings.len() == expressions.len() + 1`.
    Interpolation {
        strings: Vec<String>,
        expressions: Vec<AstNode>,
    },
    /// `Binary` — `left <op> right`.
    Binary {
        operation: BinaryOperation,
        left: Box<AstNode>,
        right: Box<AstNode>,
    },
    /// `Unary` — `+expr` / `-expr`. Modeled independently of `Binary` (the TS
    /// inheritance is a back-compat hack). See [`AstNode::create_plus`] /
    /// [`AstNode::create_minus`] for the desugaring constructors and
    /// [`AstVisitor::visit_unary`] for the binary fallback semantics.
    Unary {
        operator: UnaryOperator,
        expr: Box<AstNode>,
    },
    /// `PrefixNot` — `!expression`.
    PrefixNot { expression: Box<AstNode> },
    /// `TypeofExpression` — `typeof expression`.
    TypeofExpression { expression: Box<AstNode> },
    /// `VoidExpression` — `void expression`.
    VoidExpression { expression: Box<AstNode> },
    /// `NonNullAssert` — `expression!`.
    NonNullAssert { expression: Box<AstNode> },
    /// `Call` — `receiver(...args)`.
    Call {
        receiver: Box<AstNode>,
        args: Vec<AstNode>,
        argument_span: AbsoluteSourceSpan,
    },
    /// `SafeCall` — `receiver?.(...args)`.
    SafeCall {
        receiver: Box<AstNode>,
        args: Vec<AstNode>,
        argument_span: AbsoluteSourceSpan,
    },
    /// `TaggedTemplateLiteral` — `tag\`...\``. `template` is always a
    /// [`ExprKind::TemplateLiteral`] node.
    TaggedTemplateLiteral {
        tag: Box<AstNode>,
        template: Box<AstNode>,
    },
    /// `TemplateLiteral` — `` `a${x}b` ``. Invariant: `expressions.len() == elements.len() - 1`.
    TemplateLiteral {
        elements: Vec<TemplateLiteralElement>,
        expressions: Vec<AstNode>,
    },
    /// `TemplateLiteralElement` — a static text chunk used as a standalone node.
    TemplateLiteralElement { text: String },
    /// `ParenthesizedExpression` — `(expression)`.
    ParenthesizedExpression { expression: Box<AstNode> },
    /// `ArrowFunction` — `(params) => body`.
    ArrowFunction {
        parameters: Vec<ArrowFunctionParameter>,
        body: Box<AstNode>,
    },
    /// `RegularExpressionLiteral` — `/body/flags`.
    RegularExpressionLiteral {
        body: String,
        flags: Option<String>,
    },
}

impl AstNode {
    pub fn new(span: ParseSpan, source_span: AbsoluteSourceSpan, kind: ExprKind) -> Self {
        AstNode {
            span,
            source_span,
            kind,
        }
    }

    /// Mirrors `Unary.createMinus`: a unary minus `-x`. The visitor fallback for
    /// consumers without `visit_unary` treats this as the desugared `0 - x`.
    pub fn create_minus(span: ParseSpan, source_span: AbsoluteSourceSpan, expr: AstNode) -> AstNode {
        AstNode::new(
            span,
            source_span,
            ExprKind::Unary {
                operator: UnaryOperator::Minus,
                expr: Box::new(expr),
            },
        )
    }

    /// Mirrors `Unary.createPlus`: a unary plus `+x`. The visitor fallback for
    /// consumers without `visit_unary` treats this as the desugared `x - 0`.
    pub fn create_plus(span: ParseSpan, source_span: AbsoluteSourceSpan, expr: AstNode) -> AstNode {
        AstNode::new(
            span,
            source_span,
            ExprKind::Unary {
                operator: UnaryOperator::Plus,
                expr: Box::new(expr),
            },
        )
    }

    /// Whether this handler expression reads the implicit `$event` parameter anywhere in its
    /// tree — a bare `$event` (or `this.$event`) read. Angular's view compiler omits the
    /// `$event` parameter from a generated listener handler function when the handler does not
    /// reference it (`BoundEvent` → `getEventHandlerVars`/`resolveDollarEvent`); mirroring that
    /// keeps the generated handler signature byte-identical (`fn()` vs `fn($event)`). Used by both
    /// the template-side listener builder and the host-binding listener builder.
    pub fn references_dollar_event(&self) -> bool {
        const EVENT_NAME: &str = "$event";
        use ExprKind as EK;

        // A bare implicit/this read named `$event` is the reference we look for.
        if let EK::PropertyRead { receiver, name, .. } | EK::SafePropertyRead { receiver, name, .. } =
            &self.kind
        {
            if name == EVENT_NAME
                && matches!(receiver.kind, EK::ImplicitReceiver | EK::ThisReceiver)
            {
                return true;
            }
        }

        // Otherwise recurse into every child expression.
        match &self.kind {
            EK::EmptyExpr
            | EK::ImplicitReceiver
            | EK::ThisReceiver
            | EK::LiteralPrimitive { .. }
            | EK::TemplateLiteralElement { .. }
            | EK::RegularExpressionLiteral { .. } => false,
            EK::Chain { expressions }
            | EK::LiteralArray { expressions }
            | EK::Interpolation { expressions, .. } => {
                expressions.iter().any(AstNode::references_dollar_event)
            }
            EK::Conditional {
                condition,
                true_exp,
                false_exp,
            } => {
                condition.references_dollar_event()
                    || true_exp.references_dollar_event()
                    || false_exp.references_dollar_event()
            }
            EK::PropertyRead { receiver, .. } | EK::SafePropertyRead { receiver, .. } => {
                receiver.references_dollar_event()
            }
            EK::KeyedRead { receiver, key } | EK::SafeKeyedRead { receiver, key } => {
                receiver.references_dollar_event() || key.references_dollar_event()
            }
            EK::BindingPipe { exp, args, .. } => {
                exp.references_dollar_event()
                    || args.iter().any(AstNode::references_dollar_event)
            }
            EK::SpreadElement { expression }
            | EK::PrefixNot { expression }
            | EK::TypeofExpression { expression }
            | EK::VoidExpression { expression }
            | EK::NonNullAssert { expression }
            | EK::ParenthesizedExpression { expression } => expression.references_dollar_event(),
            EK::LiteralMap { values, .. } => {
                values.iter().any(AstNode::references_dollar_event)
            }
            EK::Binary { left, right, .. } => {
                left.references_dollar_event() || right.references_dollar_event()
            }
            EK::Unary { expr, .. } => expr.references_dollar_event(),
            EK::Call { receiver, args, .. } | EK::SafeCall { receiver, args, .. } => {
                receiver.references_dollar_event()
                    || args.iter().any(AstNode::references_dollar_event)
            }
            EK::TaggedTemplateLiteral { tag, template } => {
                tag.references_dollar_event() || template.references_dollar_event()
            }
            EK::TemplateLiteral { expressions, .. } => {
                expressions.iter().any(AstNode::references_dollar_event)
            }
            EK::ArrowFunction { body, .. } => body.references_dollar_event(),
        }
    }

    /// Builds the synthetic `LiteralPrimitive(0)` used by the unary desugaring
    /// (`-x` => `0 - x`, `+x` => `x - 0`). Mirrors `new LiteralPrimitive(span, sourceSpan, 0)`.
    /// Retained for the parser/lowering step that materializes the desugared binary form.
    #[allow(dead_code)]
    fn synthetic_zero(span: ParseSpan, source_span: AbsoluteSourceSpan) -> AstNode {
        AstNode::new(
            span,
            source_span,
            ExprKind::LiteralPrimitive {
                value: LiteralValue::Num(0.0),
            },
        )
    }
}

// ---------------------------------------------------------------------------
// ASTWithSource (the usual root wrapper).
// ---------------------------------------------------------------------------

/// `ASTWithSource` — pairs a parsed expression with its original source text,
/// location, absolute offset and parse errors. In TS this is itself an `AST`
/// subtype; here it is a top-level struct since it is almost always the root
/// wrapper. Its `span` / `source_span` are derived from `source` length.
#[derive(Clone, Debug, PartialEq)]
pub struct AstWithSource {
    pub ast: Box<AstNode>,
    pub source: Option<String>,
    pub location: String,
    pub span: ParseSpan,
    pub source_span: AbsoluteSourceSpan,
    pub errors: Vec<ParseError>,
}

impl AstWithSource {
    /// Mirrors the TS constructor, including the derived-span logic:
    /// `span = ParseSpan(0, source?.length ?? 0)` and
    /// `sourceSpan = AbsoluteSourceSpan(absoluteOffset, source===null ? absoluteOffset : absoluteOffset + source.length)`.
    ///
    /// NOTE: Angular's `source.length` is UTF-16 code units; here we use the
    /// byte length. A UTF-16<->UTF-8 remap is only required at the OXC boundary.
    pub fn new(
        ast: AstNode,
        source: Option<String>,
        location: String,
        absolute_offset: u32,
        errors: Vec<ParseError>,
    ) -> Self {
        let len = source.as_ref().map_or(0, |s| s.len() as u32);
        let span = ParseSpan::new(0, len);
        let source_span = match &source {
            None => AbsoluteSourceSpan::new(absolute_offset, absolute_offset),
            Some(_) => AbsoluteSourceSpan::new(absolute_offset, absolute_offset + len),
        };
        AstWithSource {
            ast: Box::new(ast),
            source,
            location,
            span,
            source_span,
            errors,
        }
    }
}

// ---------------------------------------------------------------------------
// Enums shared with binding metadata.
// ---------------------------------------------------------------------------

/// `BindingPipeType` — how a pipe is referenced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingPipeType {
    ReferencedByName,
    ReferencedDirectly,
}

/// `ParsedPropertyType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParsedPropertyType {
    Default,
    LiteralAttr,
    LegacyAnimation,
    TwoWay,
    Animation,
}

/// `ParsedEventType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParsedEventType {
    Regular,
    LegacyAnimation,
    TwoWay,
    Animation,
}

/// `BindingType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingType {
    Property,
    Attribute,
    Class,
    Style,
    LegacyAnimation,
    TwoWay,
    Animation,
}

// ---------------------------------------------------------------------------
// Microsyntax / template bindings.
// ---------------------------------------------------------------------------

/// `TemplateBindingIdentifier` — `{ source, span }`.
#[derive(Clone, Debug, PartialEq)]
pub struct TemplateBindingIdentifier {
    pub source: String,
    pub span: AbsoluteSourceSpan,
}

/// `TemplateBinding = VariableBinding | ExpressionBinding`.
#[derive(Clone, Debug, PartialEq)]
pub enum TemplateBinding {
    /// `VariableBinding`.
    Variable {
        source_span: AbsoluteSourceSpan,
        key: TemplateBindingIdentifier,
        value: Option<TemplateBindingIdentifier>,
    },
    /// `ExpressionBinding`.
    Expression {
        source_span: AbsoluteSourceSpan,
        key: TemplateBindingIdentifier,
        value: Option<AstWithSource>,
    },
}

// ---------------------------------------------------------------------------
// Binding metadata wrappers.
// ---------------------------------------------------------------------------

/// `ParsedProperty`. The TS derived booleans (`isLiteral`, `isLegacyAnimation`,
/// `isAnimation`) are exposed as methods to avoid stale derived state.
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedProperty {
    pub name: String,
    pub expression: AstWithSource,
    pub ty: ParsedPropertyType,
    pub source_span: ParseSourceSpan,
    pub key_span: ParseSourceSpan,
    pub value_span: Option<ParseSourceSpan>,
}

impl ParsedProperty {
    pub fn is_literal(&self) -> bool {
        self.ty == ParsedPropertyType::LiteralAttr
    }
    pub fn is_legacy_animation(&self) -> bool {
        self.ty == ParsedPropertyType::LegacyAnimation
    }
    pub fn is_animation(&self) -> bool {
        self.ty == ParsedPropertyType::Animation
    }
}

/// `ParsedEvent`. The TS overloaded constructor narrows `handler` for `TwoWay`
/// events; that narrowing is advisory, so this is a single struct.
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedEvent {
    pub name: String,
    pub target_or_phase: Option<String>,
    pub ty: ParsedEventType,
    pub handler: AstWithSource,
    pub source_span: ParseSourceSpan,
    pub handler_span: ParseSourceSpan,
    pub key_span: ParseSourceSpan,
}

/// `ParsedVariable` — a variable declaration in a microsyntax expression.
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedVariable {
    pub name: String,
    pub value: String,
    pub source_span: ParseSourceSpan,
    pub key_span: ParseSourceSpan,
    pub value_span: Option<ParseSourceSpan>,
}

/// `BoundElementProperty`.
#[derive(Clone, Debug, PartialEq)]
pub struct BoundElementProperty {
    pub name: String,
    pub ty: BindingType,
    pub security_context: SecurityContext,
    pub value: AstWithSource,
    pub unit: Option<String>,
    pub source_span: ParseSourceSpan,
    pub key_span: Option<ParseSourceSpan>,
    pub value_span: Option<ParseSourceSpan>,
}

// ---------------------------------------------------------------------------
// Visitor.
// ---------------------------------------------------------------------------

/// `AstVisitor` + `RecursiveAstVisitor` folded into one trait. Every per-variant
/// hook has a default implementation that recurses into children in the exact
/// order `RecursiveAstVisitor` uses (the order is observable downstream, e.g.
/// i18n message ordering), so implementors override only the hooks they care
/// about.
///
/// The TS "optional" methods (`visitUnary?`, `visitThisReceiver?`,
/// `visitEmptyExpr?`, `visitASTWithSource?`, gate `visit?`) become trait methods
/// with their behavioral defaults preserved:
/// - `visit_unary` falls back to treating the node as the desugared binary form;
/// - `visit_empty_expr` is a no-op;
/// - `visit_ast_with_source` transparently forwards to the wrapped node.
#[allow(unused_variables)]
pub trait AstVisitor {
    /// Optional gate (`RecursiveAstVisitor.visit`): dispatch a node. Override to
    /// selectively descend. Default just walks the node.
    fn visit(&mut self, node: &AstNode) {
        self.walk(node);
    }

    /// Walk a node: `match` on its kind and dispatch to the matching hook.
    /// Mirrors each concrete class's virtual `visit()`.
    fn walk(&mut self, node: &AstNode) {
        match &node.kind {
            ExprKind::EmptyExpr => self.visit_empty_expr(node),
            ExprKind::ImplicitReceiver => self.visit_implicit_receiver(node),
            ExprKind::ThisReceiver => self.visit_this_receiver(node),
            ExprKind::Chain { .. } => self.visit_chain(node),
            ExprKind::Conditional { .. } => self.visit_conditional(node),
            ExprKind::PropertyRead { .. } => self.visit_property_read(node),
            ExprKind::SafePropertyRead { .. } => self.visit_safe_property_read(node),
            ExprKind::KeyedRead { .. } => self.visit_keyed_read(node),
            ExprKind::SafeKeyedRead { .. } => self.visit_safe_keyed_read(node),
            ExprKind::BindingPipe { .. } => self.visit_pipe(node),
            ExprKind::LiteralPrimitive { .. } => self.visit_literal_primitive(node),
            ExprKind::LiteralArray { .. } => self.visit_literal_array(node),
            ExprKind::SpreadElement { .. } => self.visit_spread_element(node),
            ExprKind::LiteralMap { .. } => self.visit_literal_map(node),
            ExprKind::Interpolation { .. } => self.visit_interpolation(node),
            ExprKind::Binary { .. } => self.visit_binary(node),
            ExprKind::Unary { .. } => self.visit_unary(node),
            ExprKind::PrefixNot { .. } => self.visit_prefix_not(node),
            ExprKind::TypeofExpression { .. } => self.visit_typeof_expression(node),
            ExprKind::VoidExpression { .. } => self.visit_void_expression(node),
            ExprKind::NonNullAssert { .. } => self.visit_non_null_assert(node),
            ExprKind::Call { .. } => self.visit_call(node),
            ExprKind::SafeCall { .. } => self.visit_safe_call(node),
            ExprKind::TaggedTemplateLiteral { .. } => self.visit_tagged_template_literal(node),
            ExprKind::TemplateLiteral { .. } => self.visit_template_literal(node),
            ExprKind::TemplateLiteralElement { .. } => self.visit_template_literal_element(node),
            ExprKind::ParenthesizedExpression { .. } => self.visit_parenthesized_expression(node),
            ExprKind::ArrowFunction { .. } => self.visit_arrow_function(node),
            ExprKind::RegularExpressionLiteral { .. } => self.visit_regular_expression_literal(node),
        }
    }

    /// Helper (`RecursiveAstVisitor.visitAll`): visit a slice in order.
    fn visit_all(&mut self, nodes: &[AstNode]) {
        for n in nodes {
            self.visit(n);
        }
    }

    /// `visitUnary` — default falls back to the desugared `Binary` form (`-x` =>
    /// `0 - x`, `+x` => `x - 0`), then recurses into `expr`, preserving the TS
    /// `visitUnary?` -> `visitBinary` fallback semantics.
    fn visit_unary(&mut self, node: &AstNode) {
        if let ExprKind::Unary { expr, .. } = &node.kind {
            self.visit(expr);
        }
    }

    fn visit_binary(&mut self, node: &AstNode) {
        if let ExprKind::Binary { left, right, .. } = &node.kind {
            self.visit(left);
            self.visit(right);
        }
    }

    fn visit_chain(&mut self, node: &AstNode) {
        if let ExprKind::Chain { expressions } = &node.kind {
            self.visit_all(expressions);
        }
    }

    fn visit_conditional(&mut self, node: &AstNode) {
        if let ExprKind::Conditional {
            condition,
            true_exp,
            false_exp,
        } = &node.kind
        {
            self.visit(condition);
            self.visit(true_exp);
            self.visit(false_exp);
        }
    }

    fn visit_this_receiver(&mut self, node: &AstNode) {}

    fn visit_implicit_receiver(&mut self, node: &AstNode) {}

    fn visit_interpolation(&mut self, node: &AstNode) {
        if let ExprKind::Interpolation { expressions, .. } = &node.kind {
            self.visit_all(expressions);
        }
    }

    fn visit_keyed_read(&mut self, node: &AstNode) {
        if let ExprKind::KeyedRead { receiver, key } = &node.kind {
            self.visit(receiver);
            self.visit(key);
        }
    }

    fn visit_literal_array(&mut self, node: &AstNode) {
        if let ExprKind::LiteralArray { expressions } = &node.kind {
            self.visit_all(expressions);
        }
    }

    fn visit_literal_map(&mut self, node: &AstNode) {
        if let ExprKind::LiteralMap { values, .. } = &node.kind {
            self.visit_all(values);
        }
    }

    fn visit_literal_primitive(&mut self, node: &AstNode) {}

    fn visit_pipe(&mut self, node: &AstNode) {
        if let ExprKind::BindingPipe { exp, args, .. } = &node.kind {
            self.visit(exp);
            self.visit_all(args);
        }
    }

    fn visit_prefix_not(&mut self, node: &AstNode) {
        if let ExprKind::PrefixNot { expression } = &node.kind {
            self.visit(expression);
        }
    }

    fn visit_typeof_expression(&mut self, node: &AstNode) {
        if let ExprKind::TypeofExpression { expression } = &node.kind {
            self.visit(expression);
        }
    }

    fn visit_void_expression(&mut self, node: &AstNode) {
        if let ExprKind::VoidExpression { expression } = &node.kind {
            self.visit(expression);
        }
    }

    fn visit_non_null_assert(&mut self, node: &AstNode) {
        if let ExprKind::NonNullAssert { expression } = &node.kind {
            self.visit(expression);
        }
    }

    fn visit_property_read(&mut self, node: &AstNode) {
        if let ExprKind::PropertyRead { receiver, .. } = &node.kind {
            self.visit(receiver);
        }
    }

    fn visit_safe_property_read(&mut self, node: &AstNode) {
        if let ExprKind::SafePropertyRead { receiver, .. } = &node.kind {
            self.visit(receiver);
        }
    }

    fn visit_safe_keyed_read(&mut self, node: &AstNode) {
        if let ExprKind::SafeKeyedRead { receiver, key } = &node.kind {
            self.visit(receiver);
            self.visit(key);
        }
    }

    fn visit_call(&mut self, node: &AstNode) {
        if let ExprKind::Call { receiver, args, .. } = &node.kind {
            self.visit(receiver);
            self.visit_all(args);
        }
    }

    fn visit_safe_call(&mut self, node: &AstNode) {
        if let ExprKind::SafeCall { receiver, args, .. } = &node.kind {
            self.visit(receiver);
            self.visit_all(args);
        }
    }

    /// `visitTemplateLiteral` — interleave element[i] then expression[i] in
    /// declaration order (there is always one fewer expression than element).
    fn visit_template_literal(&mut self, node: &AstNode) {
        if let ExprKind::TemplateLiteral {
            elements,
            expressions,
        } = &node.kind
        {
            for (i, el) in elements.iter().enumerate() {
                // Visit the element. We mint a transient node so the element can
                // be dispatched through the same machinery as any other node.
                self.visit_template_literal_element_inner(el);
                if let Some(expr) = expressions.get(i) {
                    self.visit(expr);
                }
            }
        }
    }

    /// Hook for a standalone `TemplateLiteralElement` node (no children).
    fn visit_template_literal_element(&mut self, node: &AstNode) {}

    /// Internal element-visit used by [`visit_template_literal`]. Default no-op,
    /// matching `RecursiveAstVisitor.visitTemplateLiteralElement`.
    fn visit_template_literal_element_inner(&mut self, element: &TemplateLiteralElement) {}

    fn visit_tagged_template_literal(&mut self, node: &AstNode) {
        if let ExprKind::TaggedTemplateLiteral { tag, template } = &node.kind {
            self.visit(tag);
            self.visit(template);
        }
    }

    fn visit_parenthesized_expression(&mut self, node: &AstNode) {
        if let ExprKind::ParenthesizedExpression { expression } = &node.kind {
            self.visit(expression);
        }
    }

    fn visit_arrow_function(&mut self, node: &AstNode) {
        if let ExprKind::ArrowFunction { body, .. } = &node.kind {
            self.visit(body);
        }
    }

    fn visit_regular_expression_literal(&mut self, node: &AstNode) {}

    fn visit_spread_element(&mut self, node: &AstNode) {
        if let ExprKind::SpreadElement { expression } = &node.kind {
            self.visit(expression);
        }
    }

    /// `visitEmptyExpr?` — default no-op (preserves the optional-method semantics).
    fn visit_empty_expr(&mut self, node: &AstNode) {}

    /// `visitASTWithSource?` — default transparently forwards to the wrapped node.
    fn visit_ast_with_source(&mut self, node: &AstWithSource) {
        self.visit(&node.ast);
    }
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_span() -> ParseSpan {
        ParseSpan::new(0, 0)
    }
    fn dummy_abs() -> AbsoluteSourceSpan {
        AbsoluteSourceSpan::new(0, 0)
    }

    #[test]
    fn parse_span_to_absolute() {
        let s = ParseSpan::new(2, 5);
        assert_eq!(s.to_absolute(10), AbsoluteSourceSpan::new(12, 15));
    }

    #[test]
    fn binary_operation_round_trip() {
        // Every variant must survive as_str -> from_str.
        let ops = [
            BinaryOperation::Assignment(AssignmentOperation::Assign),
            BinaryOperation::Assignment(AssignmentOperation::AddAssign),
            BinaryOperation::Assignment(AssignmentOperation::SubAssign),
            BinaryOperation::Assignment(AssignmentOperation::MulAssign),
            BinaryOperation::Assignment(AssignmentOperation::DivAssign),
            BinaryOperation::Assignment(AssignmentOperation::ModAssign),
            BinaryOperation::Assignment(AssignmentOperation::PowAssign),
            BinaryOperation::Assignment(AssignmentOperation::AndAssign),
            BinaryOperation::Assignment(AssignmentOperation::OrAssign),
            BinaryOperation::Assignment(AssignmentOperation::NullishAssign),
            BinaryOperation::And,
            BinaryOperation::Or,
            BinaryOperation::Nullish,
            BinaryOperation::Eq,
            BinaryOperation::Neq,
            BinaryOperation::Identity,
            BinaryOperation::NotIdentity,
            BinaryOperation::Lt,
            BinaryOperation::Gt,
            BinaryOperation::Le,
            BinaryOperation::Ge,
            BinaryOperation::In,
            BinaryOperation::Instanceof,
            BinaryOperation::Add,
            BinaryOperation::Sub,
            BinaryOperation::Mul,
            BinaryOperation::Mod,
            BinaryOperation::Div,
            BinaryOperation::Pow,
        ];
        for op in ops {
            assert_eq!(BinaryOperation::from_str(op.as_str()), Some(op));
        }
    }

    #[test]
    fn assignment_operation_strings() {
        assert_eq!(AssignmentOperation::NullishAssign.as_str(), "??=");
        assert_eq!(
            AssignmentOperation::from_str("**="),
            Some(AssignmentOperation::PowAssign)
        );
        assert_eq!(AssignmentOperation::from_str("nope"), None);
    }

    #[test]
    fn is_assignment_predicate() {
        assert!(BinaryOperation::Assignment(AssignmentOperation::Assign).is_assignment());
        assert!(!BinaryOperation::Add.is_assignment());
        assert!(!BinaryOperation::Nullish.is_assignment());
    }

    #[test]
    fn unary_constructors() {
        let expr = AstNode::new(
            dummy_span(),
            dummy_abs(),
            ExprKind::LiteralPrimitive {
                value: LiteralValue::Num(3.0),
            },
        );
        let minus = AstNode::create_minus(dummy_span(), dummy_abs(), expr.clone());
        match &minus.kind {
            ExprKind::Unary { operator, .. } => assert_eq!(*operator, UnaryOperator::Minus),
            _ => panic!("expected Unary"),
        }
        let plus = AstNode::create_plus(dummy_span(), dummy_abs(), expr);
        match &plus.kind {
            ExprKind::Unary { operator, .. } => assert_eq!(*operator, UnaryOperator::Plus),
            _ => panic!("expected Unary"),
        }
    }

    #[test]
    fn synthetic_zero_is_zero_literal() {
        let z = AstNode::synthetic_zero(dummy_span(), dummy_abs());
        match z.kind {
            ExprKind::LiteralPrimitive {
                value: LiteralValue::Num(n),
            } => assert_eq!(n, 0.0),
            _ => panic!("expected numeric zero literal"),
        }
    }

    #[test]
    fn literal_value_null_vs_undefined_distinct() {
        assert_ne!(LiteralValue::Null, LiteralValue::Undefined);
    }

    #[test]
    fn ast_with_source_derived_spans_some() {
        let inner = AstNode::new(dummy_span(), dummy_abs(), ExprKind::EmptyExpr);
        let ws = AstWithSource::new(
            inner,
            Some("a + b".to_string()),
            "loc".to_string(),
            100,
            vec![],
        );
        assert_eq!(ws.span, ParseSpan::new(0, 5));
        assert_eq!(ws.source_span, AbsoluteSourceSpan::new(100, 105));
    }

    #[test]
    fn ast_with_source_derived_spans_none() {
        let inner = AstNode::new(dummy_span(), dummy_abs(), ExprKind::EmptyExpr);
        let ws = AstWithSource::new(inner, None, "loc".to_string(), 100, vec![]);
        assert_eq!(ws.span, ParseSpan::new(0, 0));
        assert_eq!(ws.source_span, AbsoluteSourceSpan::new(100, 100));
    }

    /// A visitor that collects the visit order of leaf nodes, to assert the
    /// `RecursiveAstVisitor` child-visit order is preserved.
    #[derive(Default)]
    struct OrderCollector {
        log: Vec<String>,
    }

    impl AstVisitor for OrderCollector {
        fn visit_literal_primitive(&mut self, node: &AstNode) {
            if let ExprKind::LiteralPrimitive {
                value: LiteralValue::Num(n),
            } = &node.kind
            {
                self.log.push(format!("{n}"));
            }
        }
    }

    fn num(n: f64) -> AstNode {
        AstNode::new(
            dummy_span(),
            dummy_abs(),
            ExprKind::LiteralPrimitive {
                value: LiteralValue::Num(n),
            },
        )
    }

    #[test]
    fn conditional_visit_order() {
        // cond ? t : f  => condition, true, false.
        let node = AstNode::new(
            dummy_span(),
            dummy_abs(),
            ExprKind::Conditional {
                condition: Box::new(num(1.0)),
                true_exp: Box::new(num(2.0)),
                false_exp: Box::new(num(3.0)),
            },
        );
        let mut v = OrderCollector::default();
        v.visit(&node);
        assert_eq!(v.log, vec!["1", "2", "3"]);
    }

    #[test]
    fn call_visit_order_receiver_then_args() {
        let node = AstNode::new(
            dummy_span(),
            dummy_abs(),
            ExprKind::Call {
                receiver: Box::new(num(1.0)),
                args: vec![num(2.0), num(3.0)],
                argument_span: dummy_abs(),
            },
        );
        let mut v = OrderCollector::default();
        v.visit(&node);
        assert_eq!(v.log, vec!["1", "2", "3"]);
    }

    #[test]
    fn unary_visit_fallback_recurses_into_expr() {
        let node = AstNode::create_minus(dummy_span(), dummy_abs(), num(7.0));
        let mut v = OrderCollector::default();
        v.visit(&node);
        assert_eq!(v.log, vec!["7"]);
    }

    #[test]
    fn ast_with_source_visit_forwards() {
        let ws = AstWithSource::new(num(42.0), None, "loc".to_string(), 0, vec![]);
        let mut v = OrderCollector::default();
        v.visit_ast_with_source(&ws);
        assert_eq!(v.log, vec!["42"]);
    }

    #[test]
    fn parsed_property_derived_flags() {
        let mk = |ty| ParsedProperty {
            name: "x".to_string(),
            expression: AstWithSource::new(num(0.0), None, String::new(), 0, vec![]),
            ty,
            source_span: ParseSourceSpan { start: 0, end: 0 },
            key_span: ParseSourceSpan { start: 0, end: 0 },
            value_span: None,
        };
        assert!(mk(ParsedPropertyType::LiteralAttr).is_literal());
        assert!(mk(ParsedPropertyType::LegacyAnimation).is_legacy_animation());
        assert!(mk(ParsedPropertyType::Animation).is_animation());
        let d = mk(ParsedPropertyType::Default);
        assert!(!d.is_literal() && !d.is_legacy_animation() && !d.is_animation());
    }

    #[test]
    fn structural_equality_clone() {
        let a = num(5.0);
        let b = a.clone();
        assert_eq!(a, b);
    }
}
