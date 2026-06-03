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
    ExportDefaultDeclarationKind, ObjectPropertyKind, Program, PropertyKey, Statement,
};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType};

use super::{
    ClassWithDecorators, DecoratorInfo, LitValue, MemberInfo, NgDeclareCall, ObjLit, ParseBackend,
    ParseOutput, SourceKind, TreatySpan,
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

    for stmt in &program.body {
        if let Some(class) = statement_class(stmt) {
            if !class.decorators.is_empty() {
                classes.push(lower_class(class));
            }
            collect_ng_declares_in_class(class, &mut ng_declare_calls);
        }
        // `ɵɵngDeclare*` can also sit as a free top-level statement (e.g. a re-parsed slice or a
        // non-class declaration shape); cover the expression-statement form too.
        collect_ng_declares_in_stmt(stmt, &mut ng_declare_calls);
    }

    let _ = source; // span_text recovers source text on demand; not needed during lowering.
    ParseOutput {
        classes,
        ng_declare_calls,
        errors: Vec::new(),
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
fn lower_class(class: &Class) -> ClassWithDecorators {
    let name = class.id.as_ref().map(|id| id.name.to_string());
    let decorators = class.decorators.iter().map(lower_decorator).collect();
    let members = class
        .body
        .body
        .iter()
        .map(lower_member)
        .collect::<Vec<_>>();
    ClassWithDecorators {
        name,
        decorators,
        members,
    }
}

/// Lower a class member (property / accessor / method) to the neutral [`MemberInfo`].
fn lower_member(element: &ClassElement) -> MemberInfo {
    let (name, decorators): (Option<String>, &oxc_allocator::Vec<Decorator>) = match element {
        ClassElement::PropertyDefinition(p) => (key_name(&p.key).map(str::to_string), &p.decorators),
        ClassElement::MethodDefinition(m) => (key_name(&m.key).map(str::to_string), &m.decorators),
        ClassElement::AccessorProperty(a) => (key_name(&a.key).map(str::to_string), &a.decorators),
        _ => return MemberInfo::default(),
    };
    MemberInfo {
        name,
        decorators: decorators.iter().map(lower_decorator).collect(),
    }
}

/// Lower a decorator to the neutral [`DecoratorInfo`] (callee name + first object-literal argument).
fn lower_decorator(dec: &Decorator) -> DecoratorInfo {
    let name = decorator_name(dec).unwrap_or_default().to_string();
    let object = decorator_object(dec).map(lower_object);
    DecoratorInfo { name, object }
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
/// order.
fn lower_object(obj: &oxc_ast::ast::ObjectExpression) -> ObjLit {
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
    }
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

/// If `expr` is a `ɵɵngDeclare*({…})` call with an object-literal argument, push its neutral form.
fn push_if_ng_declare(expr: &Expression, out: &mut Vec<NgDeclareCall>) {
    let Expression::CallExpression(call) = expr else {
        return;
    };
    let Some(kind) = declare_callee_kind(&call.callee) else {
        return;
    };
    for arg in &call.arguments {
        if let Argument::ObjectExpression(obj) = arg {
            out.push(NgDeclareCall {
                kind,
                object: lower_object(obj),
            });
            return;
        }
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
