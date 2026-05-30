//! Lower the owned `output_ast::{Expr,Stmt}` IR into `oxc_ast` and emit JS/TS text
//! via `oxc_codegen`. This replaces Angular's hand-rolled `abstract_emitter.ts` /
//! `abstract_js_emitter.ts` text accumulator: instead of manually tracking lines,
//! indentation and source spans, we build a real `oxc_ast::Program` with
//! [`oxc_ast::AstBuilder`] and print it with [`oxc_codegen::Codegen`].
//!
//! PORT TARGET: `migration/render3-specs/02-abstract_emitter.md`
//! Sources (semantics only): `packages/compiler/src/output/abstract_emitter.ts`,
//! `packages/compiler/src/output/abstract_js_emitter.ts`.
//!
//! # Architecture
//! `oxc` owns the text + (eventually) source maps, so the entire
//! `EmitterVisitorContext` / `EmittedLine` / `toSourceMapGenerator` machinery is
//! dropped. Parenthesization is delegated to oxc_codegen's precedence logic — we do
//! NOT replicate Angular's aggressive always-parenthesize behaviour nor the
//! stateful `lastIfCondition` hack (see spec §7.1/§7.2; this is an intentional,
//! documented behavioural divergence from Angular's golden output).
//!
//! # Operator split
//! Angular conflates true-binary, logical (`&&`/`||`/`??`) and assignment operators
//! into one [`output_ast::BinaryOperator`] enum. oxc splits these into
//! `BinaryExpression` / `LogicalExpression` / `AssignmentExpression`, so
//! [`Lowerer::lower_binary`] dispatches to three different node builders.
//!
//! # Fallbacks (no `todo!()`)
//! A handful of `output_ast` node kinds are not yet lowered to a faithful oxc node
//! (i18n `LocalizedString`, `WrappedNode` foreign handles, regex literals, dynamic
//! import). Rather than panic, [`Lowerer`] emits a clearly-named placeholder
//! identifier (e.g. `__unsupported_LocalizedString`) so emission never aborts and
//! the gap is visible in output. These are enumerated in the module-level notes.

use std::cell::RefCell;

use oxc_allocator::{Allocator, Box as ArenaBox, Vec as ArenaVec};
use oxc_ast::AstBuilder;
use oxc_ast::ast::{
    Argument, ArrayExpressionElement, AssignmentOperator, AssignmentTarget, BinaryOperator as OxBin,
    BindingPattern, Declaration, Expression, FormalParameterKind, FunctionBody, FunctionType,
    ImportOrExportKind, LogicalOperator, NumberBase, ObjectPropertyKind, PropertyKey, PropertyKind,
    SimpleAssignmentTarget, Statement, UnaryOperator as OxUn, VariableDeclarationKind,
};
use oxc_codegen::Codegen;
use oxc_span::{SourceType, SPAN};

use crate::output_ast::{
    self as o, ArrowBody, BinaryOperator, ExprKind, FnParam, ImportUrl, LiteralMapEntry,
    LiteralValue, StmtKind, StmtModifier, UnaryOperator,
};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Lower a slice of `output_ast` statements to JS and return the printed source.
///
/// Allocates its own arena, lowers each statement, wraps them in a [`Program`],
/// and codegens to a [`String`].
pub fn emit_statements(stmts: &[o::Stmt]) -> String {
    let allocator = Allocator::default();
    let lowerer = Lowerer::new(&allocator);
    let mut lowered = lowerer.ast.vec_with_capacity(stmts.len());
    for stmt in stmts {
        lowered.push(lowerer.lower_stmt(stmt));
    }
    // Prepend `import * as iN from "module";` for every external module referenced
    // while lowering. This must run AFTER lowering so the import manager has seen
    // every `External` expression.
    let mut body = lowerer.ast.vec();
    for import in lowerer.namespace_import_stmts() {
        body.push(import);
    }
    for stmt in lowered {
        body.push(stmt);
    }
    lowerer.codegen(body)
}

/// Lower a single `output_ast` expression to JS and return the printed source.
///
/// The expression is wrapped in an expression statement so codegen has a complete
/// program to print (the trailing `;` codegen adds is left intact, matching
/// Angular's `visitExpressionStmt`).
pub fn emit_expression(expr: &o::Expr) -> String {
    let allocator = Allocator::default();
    let lowerer = Lowerer::new(&allocator);
    let oxc_expr = lowerer.lower_expr(expr);
    let stmt = lowerer.ast.statement_expression(SPAN, oxc_expr);
    // Like `emit_statements`, surface any external imports the expression referenced
    // as leading `import * as iN from "module";` lines so the snippet is runnable.
    let mut body = lowerer.ast.vec();
    for import in lowerer.namespace_import_stmts() {
        body.push(import);
    }
    body.push(stmt);
    lowerer.codegen(body)
}

// ---------------------------------------------------------------------------
// Lowerer
// ---------------------------------------------------------------------------

/// Owns the [`AstBuilder`] used to allocate every lowered node into the arena, plus
/// an [`ImportManager`] that assigns each distinct external module a stable namespace
/// alias (`i0`, `i1`, ...) so runtime references emit as `i0.ɵɵfoo` instead of an
/// invalid dotted module path (`@angular/core.ɵɵfoo`).
struct Lowerer<'a> {
    ast: AstBuilder<'a>,
    imports: RefCell<ImportManager>,
}

/// Maps module specifiers (`@angular/core`, ...) to stable namespace import aliases
/// in first-seen order. Mirrors the behaviour of Angular's ngtsc `ImportManager`,
/// which emits `import * as iN from "<module>"` and references symbols as `iN.symbol`.
#[derive(Default)]
struct ImportManager {
    /// `(module_specifier, alias)` pairs, in insertion order. A `Vec` keeps emission
    /// deterministic (alias index == position) and the set is tiny in practice.
    modules: Vec<(String, String)>,
}

impl ImportManager {
    /// Return the stable alias for `module`, allocating a fresh `iN` on first sight.
    fn alias_for(&mut self, module: &str) -> String {
        if let Some((_, alias)) = self.modules.iter().find(|(m, _)| m == module) {
            return alias.clone();
        }
        let alias = format!("i{}", self.modules.len());
        self.modules.push((module.to_string(), alias.clone()));
        alias
    }
}

impl<'a> Lowerer<'a> {
    fn new(allocator: &'a Allocator) -> Self {
        Lowerer {
            ast: AstBuilder::new(allocator),
            imports: RefCell::new(ImportManager::default()),
        }
    }

    /// Build a `import * as iN from "module";` statement for every external module
    /// referenced during lowering, in first-seen order. Returns an empty `Vec` when
    /// nothing external was referenced.
    fn namespace_import_stmts(&self) -> Vec<Statement<'a>> {
        self.imports
            .borrow()
            .modules
            .iter()
            .map(|(module, alias)| self.namespace_import_stmt(module, alias))
            .collect()
    }

    /// Build a single `import * as <alias> from "<module>";` statement.
    fn namespace_import_stmt(&self, module: &str, alias: &str) -> Statement<'a> {
        let local = self.ast.binding_identifier(SPAN, self.ast.ident(alias));
        let namespace = self
            .ast
            .import_declaration_specifier_import_namespace_specifier(SPAN, local);
        let mut specifiers = self.ast.vec_with_capacity(1);
        specifiers.push(namespace);
        let source = self.ast.string_literal(SPAN, self.ast.str(module), None);
        let module_decl = self.ast.module_declaration_import_declaration(
            SPAN,
            Some(specifiers),
            source,
            None, // phase: Option<ImportPhase>
            oxc_ast::NONE, // with_clause
            ImportOrExportKind::Value,
        );
        Statement::from(module_decl)
    }

    /// Wrap lowered statements in a [`Program`] and print via [`Codegen`].
    fn codegen(&self, body: ArenaVec<'a, Statement<'a>>) -> String {
        let program = self.ast.program(
            SPAN,
            SourceType::default(),
            "", // source_text: arena buffer is empty; codegen does not need it.
            self.ast.vec(),  // comments
            None,            // hashbang
            self.ast.vec(),  // directives
            body,
        );
        Codegen::new().build(&program).code
    }

    // -- helpers ----------------------------------------------------------

    /// Allocate a runtime string into the arena and build an identifier expression.
    fn ident_expr(&self, name: &str) -> Expression<'a> {
        let id = self.ast.ident(name);
        self.ast.expression_identifier(SPAN, id)
    }

    /// Placeholder for a not-yet-lowered node kind. Emits `__unsupported_<what>`
    /// rather than panicking (see module docs).
    fn unsupported(&self, what: &str) -> Expression<'a> {
        let mut name = String::from("__unsupported_");
        name.push_str(what);
        self.ident_expr(&name)
    }

    fn arg(&self, expr: Expression<'a>) -> Argument<'a> {
        Argument::from(expr)
    }

    // -- statements -------------------------------------------------------

    fn lower_stmt(&self, stmt: &o::Stmt) -> Statement<'a> {
        match &stmt.kind {
            StmtKind::Expression(expr) => {
                let e = self.lower_expr(expr);
                self.ast.statement_expression(SPAN, e)
            }
            StmtKind::Return(expr) => {
                let e = self.lower_expr(expr);
                self.ast.statement_return(SPAN, Some(e))
            }
            StmtKind::DeclareVar { name, value, .. } => {
                // `const` if Final modifier set, else `let` (matches Angular's
                // visitDeclareVarStmt; the JS emitter forces `var` — not modelled
                // here, see notes).
                let kind = if stmt.meta.modifiers.has_modifier(StmtModifier::FINAL) {
                    VariableDeclarationKind::Const
                } else {
                    VariableDeclarationKind::Let
                };
                self.lower_var_decl(kind, name, value.as_ref())
            }
            StmtKind::DeclareFunction {
                name,
                params,
                statements,
                ..
            } => {
                let id = Some(self.ast.binding_identifier(SPAN, self.ast.ident(name)));
                let oxc_params = self.lower_params(params);
                let body = self.lower_fn_body(statements);
                let decl: Declaration = self.ast.declaration_function(
                    SPAN,
                    FunctionType::FunctionDeclaration,
                    id,
                    false, // generator
                    false, // async
                    false, // declare
                    oxc_ast::NONE,
                    oxc_ast::NONE,
                    oxc_params,
                    oxc_ast::NONE,
                    Some(body),
                );
                Statement::from(decl)
            }
            StmtKind::If {
                condition,
                true_case,
                false_case,
            } => {
                let test = self.lower_expr(condition);
                let consequent = self.block(true_case);
                let alternate = if false_case.is_empty() {
                    None
                } else {
                    Some(self.block(false_case))
                };
                self.ast.statement_if(SPAN, test, consequent, alternate)
            }
        }
    }

    fn lower_var_decl(
        &self,
        kind: VariableDeclarationKind,
        name: &str,
        value: Option<&o::Expr>,
    ) -> Statement<'a> {
        let binding = self.ast.binding_pattern_binding_identifier(SPAN, self.ast.ident(name));
        let init = value.map(|v| self.lower_expr(v));
        let declarator = self.ast.variable_declarator(
            SPAN,
            kind,
            binding,
            oxc_ast::NONE,
            init,
            false, // definite
        );
        let mut decls = self.ast.vec_with_capacity(1);
        decls.push(declarator);
        let decl = self.ast.declaration_variable(SPAN, kind, decls, false);
        Statement::from(decl)
    }

    /// Build a `{ ... }` block statement from a slice of `output_ast` statements.
    fn block(&self, stmts: &[o::Stmt]) -> Statement<'a> {
        let mut body = self.ast.vec_with_capacity(stmts.len());
        for s in stmts {
            body.push(self.lower_stmt(s));
        }
        self.ast.statement_block(SPAN, body)
    }

    /// Build a [`FunctionBody`] from a slice of statements.
    fn lower_fn_body(&self, stmts: &[o::Stmt]) -> ArenaBox<'a, FunctionBody<'a>> {
        let mut body = self.ast.vec_with_capacity(stmts.len());
        for s in stmts {
            body.push(self.lower_stmt(s));
        }
        self.ast.alloc_function_body(SPAN, self.ast.vec(), body)
    }

    fn lower_params(&self, params: &[FnParam]) -> ArenaBox<'a, oxc_ast::ast::FormalParameters<'a>> {
        let mut items = self.ast.vec_with_capacity(params.len());
        for p in params {
            let pattern: BindingPattern =
                self.ast.binding_pattern_binding_identifier(SPAN, self.ast.ident(&p.name));
            let fp = self.ast.plain_formal_parameter(SPAN, pattern);
            items.push(fp);
        }
        self.ast.alloc_formal_parameters(
            SPAN,
            FormalParameterKind::FormalParameter,
            items,
            oxc_ast::NONE,
        )
    }

    // -- expressions ------------------------------------------------------

    fn lower_expr(&self, expr: &o::Expr) -> Expression<'a> {
        match &expr.kind {
            ExprKind::ReadVar { name } => self.ident_expr(name),

            ExprKind::Literal(value) => self.lower_literal(value),

            ExprKind::External { value, .. } => {
                // ExternalExpr — Angular's concrete emitter maps these to either a
                // namespaced member (`i0.foo`) or a bare identifier. When a
                // `module_name` is present we route it through the import manager,
                // which assigns the module a stable alias (`i0`, `i1`, ...) and records
                // it so `emit_statements`/`emit_expression` can prepend the matching
                // `import * as iN from "module";`. The reference itself becomes
                // `alias.name` (a static member on the alias identifier) — valid JS,
                // unlike the previous `@angular/core.name` dotted module path.
                match &value.module_name {
                    Some(module) if !module.is_empty() => {
                        let alias = self.imports.borrow_mut().alias_for(module);
                        let obj = self.ident_expr(&alias);
                        let prop = self.ast.identifier_name(SPAN, self.ast.ident(&value.name));
                        let member = self.ast.alloc_static_member_expression(SPAN, obj, prop, false);
                        Expression::StaticMemberExpression(member)
                    }
                    // No module: emit a bare identifier (the symbol is assumed already
                    // in scope / imported elsewhere).
                    _ => self.ident_expr(&value.name),
                }
            }

            ExprKind::Invoke {
                callee,
                args,
                optional,
                ..
            } => {
                let callee_expr = self.lower_expr(callee);
                let arguments = self.lower_args(args);
                self.ast.expression_call(
                    SPAN,
                    callee_expr,
                    oxc_ast::NONE,
                    arguments,
                    *optional,
                )
            }

            ExprKind::New { class_expr, args } => {
                let callee = self.lower_expr(class_expr);
                let arguments = self.lower_args(args);
                self.ast.expression_new(
                    SPAN,
                    callee,
                    oxc_ast::NONE,
                    arguments,
                )
            }

            ExprKind::ReadProp {
                receiver,
                name,
                optional,
            } => {
                let obj = self.lower_expr(receiver);
                let prop = self.ast.identifier_name(SPAN, self.ast.ident(name));
                let member = self.ast.alloc_static_member_expression(SPAN, obj, prop, *optional);
                Expression::StaticMemberExpression(member)
            }

            ExprKind::ReadKey {
                receiver,
                index,
                optional,
            } => {
                let obj = self.lower_expr(receiver);
                let idx = self.lower_expr(index);
                let member =
                    self.ast.alloc_computed_member_expression(SPAN, obj, idx, *optional);
                Expression::ComputedMemberExpression(member)
            }

            ExprKind::Conditional {
                condition,
                true_case,
                false_case,
            } => {
                let test = self.lower_expr(condition);
                let consequent = self.lower_expr(true_case);
                // Angular's falseCase is optional; oxc requires an alternate, so a
                // missing one degrades to `undefined` (matches JS semantics of a
                // dangling ternary, which Angular itself non-null-asserts).
                let alternate = match false_case {
                    Some(f) => self.lower_expr(f),
                    None => self.ident_expr("undefined"),
                };
                self.ast.expression_conditional(SPAN, test, consequent, alternate)
            }

            ExprKind::Not(inner) => {
                let arg = self.lower_expr(inner);
                self.ast.expression_unary(SPAN, OxUn::LogicalNot, arg)
            }

            ExprKind::Unary { op, expr, .. } => {
                let arg = self.lower_expr(expr);
                let oxc_op = match op {
                    UnaryOperator::Plus => OxUn::UnaryPlus,
                    UnaryOperator::Minus => OxUn::UnaryNegation,
                };
                self.ast.expression_unary(SPAN, oxc_op, arg)
            }

            ExprKind::Typeof(inner) => {
                let arg = self.lower_expr(inner);
                self.ast.expression_unary(SPAN, OxUn::Typeof, arg)
            }

            ExprKind::Void(inner) => {
                let arg = self.lower_expr(inner);
                self.ast.expression_unary(SPAN, OxUn::Void, arg)
            }

            ExprKind::Binary { op, lhs, rhs } => self.lower_binary(*op, lhs, rhs),

            ExprKind::LiteralArray(entries) => {
                let mut elements = self.ast.vec_with_capacity(entries.len());
                for e in entries {
                    let el = self.lower_expr(e);
                    elements.push(ArrayExpressionElement::from(el));
                }
                self.ast.expression_array(SPAN, elements)
            }

            ExprKind::LiteralMap { entries, .. } => {
                let mut props = self.ast.vec_with_capacity(entries.len());
                for entry in entries {
                    props.push(self.lower_map_entry(entry));
                }
                self.ast.expression_object(SPAN, props)
            }

            ExprKind::Comma(parts) => {
                let mut exprs = self.ast.vec_with_capacity(parts.len());
                for p in parts {
                    exprs.push(self.lower_expr(p));
                }
                self.ast.expression_sequence(SPAN, exprs)
            }

            ExprKind::Parenthesized(inner) => {
                // oxc_codegen reinserts parentheses by precedence; we still emit an
                // explicit parenthesized node to preserve intent where it survives.
                let e = self.lower_expr(inner);
                self.ast.expression_parenthesized(SPAN, e)
            }

            ExprKind::Spread(inner) => {
                // A bare spread is only valid inside call args / arrays; emitting it
                // standalone wraps it so output is still well-formed-ish. Callers
                // that need real spread semantics go through `lower_args`.
                let e = self.lower_expr(inner);
                self.ast.expression_parenthesized(SPAN, e)
            }

            ExprKind::Function {
                params,
                statements,
                name,
            } => {
                let id = name
                    .as_ref()
                    .map(|n| self.ast.binding_identifier(SPAN, self.ast.ident(n)));
                let oxc_params = self.lower_params(params);
                let body = self.lower_fn_body(statements);
                self.ast.expression_function(
                    SPAN,
                    FunctionType::FunctionExpression,
                    id,
                    false,
                    false,
                    false,
                    oxc_ast::NONE,
                    oxc_ast::NONE,
                    oxc_params,
                    oxc_ast::NONE,
                    Some(body),
                )
            }

            ExprKind::Arrow { params, body } => self.lower_arrow(params, body),

            // -- not-yet-lowered node kinds: emit a visible placeholder ------
            ExprKind::TaggedTemplate { .. } => self.unsupported("TaggedTemplate"),
            ExprKind::TemplateLiteral { .. } => self.unsupported("TemplateLiteral"),
            ExprKind::TemplateLiteralElement(_) => self.unsupported("TemplateLiteralElement"),
            ExprKind::LocalizedString { .. } => self.unsupported("LocalizedString"),
            ExprKind::RegExpLiteral { .. } => self.unsupported("RegExpLiteral"),
            ExprKind::WrappedNode(_) => self.unsupported("WrappedNode"),
            ExprKind::DynamicImport { url, .. } => {
                // `import(<url>)` — model as a call to the `import` keyword-ident so
                // output is recognizable even though it is not a true ImportExpression.
                let callee = self.ident_expr("import");
                let mut arguments = self.ast.vec_with_capacity(1);
                let url_expr = match url {
                    ImportUrl::Str(s) => {
                        let v = self.ast.str(s);
                        self.ast.expression_string_literal(SPAN, v, None)
                    }
                    ImportUrl::Expr(e) => self.lower_expr(e),
                };
                arguments.push(self.arg(url_expr));
                self.ast.expression_call(
                    SPAN,
                    callee,
                    oxc_ast::NONE,
                    arguments,
                    false,
                )
            }
        }
    }

    fn lower_literal(&self, value: &LiteralValue) -> Expression<'a> {
        match value {
            LiteralValue::String(s) => {
                let v = self.ast.str(s);
                self.ast.expression_string_literal(SPAN, v, None)
            }
            LiteralValue::Number(n) => {
                self.ast
                    .expression_numeric_literal(SPAN, *n, None, NumberBase::Decimal)
            }
            LiteralValue::Bool(b) => self.ast.expression_boolean_literal(SPAN, *b),
            LiteralValue::Null => self.ast.expression_null_literal(SPAN),
            // `undefined` is an identifier in JS, not a literal.
            LiteralValue::Undefined => self.ident_expr("undefined"),
        }
    }

    fn lower_args(&self, args: &[o::Expr]) -> ArenaVec<'a, Argument<'a>> {
        let mut out = self.ast.vec_with_capacity(args.len());
        for a in args {
            // A `Spread` argument becomes a real `...x` spread element.
            if let ExprKind::Spread(inner) = &a.kind {
                let e = self.lower_expr(inner);
                out.push(self.ast.argument_spread_element(SPAN, e));
            } else {
                let e = self.lower_expr(a);
                out.push(self.arg(e));
            }
        }
        out
    }

    fn lower_map_entry(&self, entry: &LiteralMapEntry) -> ObjectPropertyKind<'a> {
        match entry {
            LiteralMapEntry::Property { key, value, .. } => {
                let prop_key: PropertyKey =
                    self.ast.property_key_static_identifier(SPAN, self.ast.ident(key));
                let val = self.lower_expr(value);
                self.ast.object_property_kind_object_property(
                    SPAN,
                    PropertyKind::Init,
                    prop_key,
                    val,
                    false, // method
                    false, // shorthand
                    false, // computed
                )
            }
            LiteralMapEntry::Spread { expression } => {
                let e = self.lower_expr(expression);
                self.ast.object_property_kind_spread_property(SPAN, e)
            }
        }
    }

    /// Dispatch Angular's single `BinaryOperator` enum into oxc's three node kinds:
    /// `LogicalExpression` (`&&`/`||`/`??`), `AssignmentExpression` (`=` + compounds),
    /// and `BinaryExpression` (everything else).
    fn lower_binary(&self, op: BinaryOperator, lhs: &o::Expr, rhs: &o::Expr) -> Expression<'a> {
        // Logical operators.
        if let Some(logop) = logical_op(op) {
            let l = self.lower_expr(lhs);
            let r = self.lower_expr(rhs);
            return self.ast.expression_logical(SPAN, l, logop, r);
        }

        // Assignment operators (including compound).
        if op.is_assignment() {
            let assign_op = assignment_op(op);
            let r = self.lower_expr(rhs);
            // The LHS must be an assignment target. Support the common simple cases
            // (identifier, member access); otherwise fall back to an identifier
            // target so output stays well-formed.
            let target = self.lower_assignment_target(lhs);
            return self.ast.expression_assignment(SPAN, assign_op, target, r);
        }

        // Plain binary operators.
        let oxc_op = binary_op(op);
        let l = self.lower_expr(lhs);
        let r = self.lower_expr(rhs);
        self.ast.expression_binary(SPAN, l, oxc_op, r)
    }

    fn lower_assignment_target(&self, lhs: &o::Expr) -> AssignmentTarget<'a> {
        match &lhs.kind {
            ExprKind::ReadVar { name } => {
                let simple: SimpleAssignmentTarget = self
                    .ast
                    .simple_assignment_target_assignment_target_identifier(
                        SPAN,
                        self.ast.ident(name),
                    );
                AssignmentTarget::from(simple)
            }
            ExprKind::ReadProp {
                receiver,
                name,
                optional,
            } => {
                let obj = self.lower_expr(receiver);
                let prop = self.ast.identifier_name(SPAN, self.ast.ident(name));
                let member = self.ast.alloc_static_member_expression(SPAN, obj, prop, *optional);
                let simple = SimpleAssignmentTarget::StaticMemberExpression(member);
                AssignmentTarget::from(simple)
            }
            ExprKind::ReadKey {
                receiver,
                index,
                optional,
            } => {
                let obj = self.lower_expr(receiver);
                let idx = self.lower_expr(index);
                let member =
                    self.ast.alloc_computed_member_expression(SPAN, obj, idx, *optional);
                let simple = SimpleAssignmentTarget::ComputedMemberExpression(member);
                AssignmentTarget::from(simple)
            }
            _ => {
                // Unsupported LHS — fall back to a placeholder identifier target.
                let simple: SimpleAssignmentTarget = self
                    .ast
                    .simple_assignment_target_assignment_target_identifier(
                        SPAN,
                        self.ast.ident("__unsupported_assign_target"),
                    );
                AssignmentTarget::from(simple)
            }
        }
    }

    fn lower_arrow(&self, params: &[FnParam], body: &ArrowBody) -> Expression<'a> {
        let oxc_params = self.lower_params(params);
        match body {
            ArrowBody::Block(stmts) => {
                let fn_body = self.lower_fn_body(stmts);
                self.ast.expression_arrow_function(
                    SPAN,
                    false, // expression
                    false, // async
                    oxc_ast::NONE,
                    oxc_params,
                    oxc_ast::NONE,
                    fn_body,
                )
            }
            ArrowBody::Expr(e) => {
                // Expression-bodied arrow: oxc models this as a FunctionBody whose
                // single statement is an ExpressionStatement, with `expression=true`.
                let inner = self.lower_expr(e);
                let stmt = self.ast.statement_expression(SPAN, inner);
                let mut stmts = self.ast.vec_with_capacity(1);
                stmts.push(stmt);
                let fn_body = self.ast.alloc_function_body(SPAN, self.ast.vec(), stmts);
                self.ast.expression_arrow_function(
                    SPAN,
                    true, // expression
                    false,
                    oxc_ast::NONE,
                    oxc_params,
                    oxc_ast::NONE,
                    fn_body,
                )
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Operator mapping tables (mirror BINARY_OPERATORS in abstract_emitter.ts §3.3).
// ---------------------------------------------------------------------------

/// Logical operators (`&&`, `||`, `??`) → oxc `LogicalOperator`, or `None` if not logical.
fn logical_op(op: BinaryOperator) -> Option<LogicalOperator> {
    match op {
        BinaryOperator::And => Some(LogicalOperator::And),
        BinaryOperator::Or => Some(LogicalOperator::Or),
        BinaryOperator::NullishCoalesce => Some(LogicalOperator::Coalesce),
        _ => None,
    }
}

/// Assignment operators (`=` + compounds) → oxc `AssignmentOperator`.
/// Only called when [`BinaryOperator::is_assignment`] is true.
fn assignment_op(op: BinaryOperator) -> AssignmentOperator {
    match op {
        BinaryOperator::Assign => AssignmentOperator::Assign,
        BinaryOperator::AdditionAssignment => AssignmentOperator::Addition,
        BinaryOperator::SubtractionAssignment => AssignmentOperator::Subtraction,
        BinaryOperator::MultiplicationAssignment => AssignmentOperator::Multiplication,
        BinaryOperator::DivisionAssignment => AssignmentOperator::Division,
        BinaryOperator::RemainderAssignment => AssignmentOperator::Remainder,
        BinaryOperator::ExponentiationAssignment => AssignmentOperator::Exponential,
        BinaryOperator::AndAssignment => AssignmentOperator::LogicalAnd,
        BinaryOperator::OrAssignment => AssignmentOperator::LogicalOr,
        BinaryOperator::NullishCoalesceAssignment => AssignmentOperator::LogicalNullish,
        // Unreachable: guarded by is_assignment(). Default keeps the fn total.
        _ => AssignmentOperator::Assign,
    }
}

/// Plain binary operators → oxc `BinaryOperator`. Only called for non-logical,
/// non-assignment ops.
fn binary_op(op: BinaryOperator) -> OxBin {
    match op {
        BinaryOperator::Equals => OxBin::Equality,
        BinaryOperator::NotEquals => OxBin::Inequality,
        BinaryOperator::Identical => OxBin::StrictEquality,
        BinaryOperator::NotIdentical => OxBin::StrictInequality,
        BinaryOperator::Minus => OxBin::Subtraction,
        BinaryOperator::Plus => OxBin::Addition,
        BinaryOperator::Divide => OxBin::Division,
        BinaryOperator::Multiply => OxBin::Multiplication,
        BinaryOperator::Modulo => OxBin::Remainder,
        BinaryOperator::Exponentiation => OxBin::Exponential,
        BinaryOperator::BitwiseOr => OxBin::BitwiseOR,
        BinaryOperator::BitwiseAnd => OxBin::BitwiseAnd,
        BinaryOperator::Lower => OxBin::LessThan,
        BinaryOperator::LowerEquals => OxBin::LessEqualThan,
        BinaryOperator::Bigger => OxBin::GreaterThan,
        BinaryOperator::BiggerEquals => OxBin::GreaterEqualThan,
        BinaryOperator::In => OxBin::In,
        BinaryOperator::InstanceOf => OxBin::Instanceof,
        // Logical & assignment ops are routed elsewhere; default keeps the fn total.
        _ => OxBin::Equality,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identifiers::R3;
    use crate::output_ast::{
        import_expr, literal, variable, ExprKind, LiteralValue, Stmt, StmtKind, StmtModifier,
        UnaryOperator,
    };

    fn num(n: f64) -> o::Expr {
        literal(LiteralValue::Number(n), None)
    }
    fn str_lit(s: &str) -> o::Expr {
        literal(LiteralValue::String(s.to_string()), None)
    }

    #[test]
    fn emits_var_decl_and_element_call() {
        // const cmp = ɵɵelement(0, "div");
        let element = import_expr(R3::Element.reference(), None);
        let call = element.call_fn(vec![num(0.0), str_lit("div")], false);
        let stmt = Stmt::with_modifiers(
            StmtKind::DeclareVar {
                name: "cmp".to_string(),
                value: Some(call),
                ty: None,
            },
            StmtModifier::FINAL,
        );

        let out = emit_statements(&[stmt]);
        assert!(out.contains("const cmp"), "got: {out}");
        // External `ɵɵelement` is now namespaced under the `i0` alias, with a
        // prepended `import * as i0 from "@angular/core"` line (runnable Ivy).
        assert!(
            out.contains("import * as i0 from \"@angular/core\""),
            "got: {out}"
        );
        assert!(out.contains("i0.\u{0275}\u{0275}element"), "got: {out}");
        assert!(out.contains("\"div\""), "got: {out}");
        assert!(out.contains("0"), "got: {out}");
    }

    #[test]
    fn external_call_emits_namespace_import_and_alias() {
        // ɵɵelement(0) — an External callee with module_name @angular/core must emit a
        // top-of-file `import * as i0 from "@angular/core"` and reference the symbol as
        // `i0.ɵɵelement`, NOT the invalid dotted module path `@angular/core.ɵɵelement`.
        let element = import_expr(R3::Element.reference(), None);
        let call = element.call_fn(vec![num(0.0)], false);
        let out = emit_expression(&call);
        assert!(
            out.contains("import * as i0 from \"@angular/core\""),
            "missing namespace import; got: {out}"
        );
        assert!(out.contains("i0.\u{0275}\u{0275}element"), "got: {out}");
        assert!(
            !out.contains("@angular/core.\u{0275}\u{0275}element"),
            "still emitting invalid dotted module path; got: {out}"
        );
    }

    #[test]
    fn distinct_modules_get_distinct_aliases() {
        use crate::output_ast::ExternalReference;
        // Two different modules => i0 / i1, each with its own import line; a repeat of
        // the first module reuses i0 (no duplicate import).
        let a = import_expr(ExternalReference::new(Some("@angular/core".into()), "ɵɵa"), None);
        let b = import_expr(ExternalReference::new(Some("@angular/common".into()), "ɵɵb"), None);
        let a2 = import_expr(ExternalReference::new(Some("@angular/core".into()), "ɵɵc"), None);
        let stmts = vec![
            Stmt::bare(StmtKind::Expression(a)),
            Stmt::bare(StmtKind::Expression(b)),
            Stmt::bare(StmtKind::Expression(a2)),
        ];
        let out = emit_statements(&stmts);
        assert!(out.contains("import * as i0 from \"@angular/core\""), "got: {out}");
        assert!(out.contains("import * as i1 from \"@angular/common\""), "got: {out}");
        assert!(out.contains("i0.\u{0275}\u{0275}a"), "got: {out}");
        assert!(out.contains("i1.\u{0275}\u{0275}b"), "got: {out}");
        // Re-used module keeps the i0 alias.
        assert!(out.contains("i0.\u{0275}\u{0275}c"), "got: {out}");
        // Exactly one import for @angular/core.
        assert_eq!(out.matches("from \"@angular/core\"").count(), 1, "got: {out}");
    }

    #[test]
    fn bare_external_without_module_emits_plain_identifier() {
        use crate::output_ast::ExternalReference;
        // module_name None => bare identifier, no import line.
        let e = import_expr(ExternalReference::new(None, "someGlobal"), None);
        let out = emit_expression(&e);
        assert!(out.contains("someGlobal"), "got: {out}");
        assert!(!out.contains("import "), "should not emit an import; got: {out}");
    }

    #[test]
    fn emits_let_when_not_final() {
        let stmt = Stmt::bare(StmtKind::DeclareVar {
            name: "x".to_string(),
            value: Some(num(1.0)),
            ty: None,
        });
        let out = emit_statements(&[stmt]);
        assert!(out.contains("let x"), "got: {out}");
    }

    #[test]
    fn emits_binary_logical_assignment_split() {
        // a + b
        let add = variable("a", None).plus(variable("b", None));
        assert!(emit_expression(&add).contains("a + b"), "{}", emit_expression(&add));

        // a && b  -> logical
        let and = variable("a", None).and(variable("b", None));
        assert!(emit_expression(&and).contains("a && b"), "{}", emit_expression(&and));

        // a ?? b -> nullish
        let nc = variable("a", None).nullish_coalesce(variable("b", None));
        assert!(emit_expression(&nc).contains("a ?? b"), "{}", emit_expression(&nc));

        // a = b -> assignment
        let assign = variable("a", None).set(variable("b", None));
        assert!(emit_expression(&assign).contains("a = b"), "{}", emit_expression(&assign));
    }

    #[test]
    fn emits_member_and_index_reads() {
        let prop = variable("obj", None).prop("field");
        assert!(emit_expression(&prop).contains("obj.field"));

        let key = variable("arr", None).key(num(2.0));
        assert!(emit_expression(&key).contains("arr[2]"));
    }

    #[test]
    fn emits_conditional() {
        let cond = variable("c", None).conditional(num(1.0), Some(num(2.0)));
        let out = emit_expression(&cond);
        assert!(out.contains("?") && out.contains(":"), "got: {out}");
    }

    #[test]
    fn emits_not_and_unary() {
        let not_expr = o::not(variable("x", None));
        assert!(emit_expression(&not_expr).contains("!x"), "{}", emit_expression(&not_expr));

        let neg = o::unary(UnaryOperator::Minus, num(5.0), None);
        assert!(emit_expression(&neg).contains("-5"), "{}", emit_expression(&neg));
    }

    #[test]
    fn emits_array_and_map() {
        let arr = o::literal_arr(vec![num(1.0), num(2.0)], None);
        let out = emit_expression(&arr);
        assert!(out.contains("[") && out.contains("1") && out.contains("2"), "got: {out}");

        let map = o::literal_map(
            vec![("k".to_string(), false, num(3.0))],
            None,
        );
        let out = emit_expression(&map);
        assert!(out.contains("k") && out.contains("3"), "got: {out}");
    }

    #[test]
    fn emits_new_expression() {
        let inst = variable("Foo", None).instantiate(vec![num(1.0)]);
        let out = emit_expression(&inst);
        assert!(out.contains("new Foo"), "got: {out}");
    }

    #[test]
    fn emits_return_and_if() {
        let ret = Stmt::bare(StmtKind::Return(num(7.0)));
        assert!(emit_statements(&[ret]).contains("return 7"));

        let if_stmt = o::if_stmt(
            variable("c", None),
            vec![Stmt::bare(StmtKind::Return(num(1.0)))],
            Some(vec![Stmt::bare(StmtKind::Return(num(2.0)))]),
        );
        let out = emit_statements(&[if_stmt]);
        assert!(out.contains("if") && out.contains("else"), "got: {out}");
    }

    #[test]
    fn emits_arrow_function() {
        let arrow = o::arrow_fn(
            vec![FnParam::new("a", None)],
            ArrowBody::Expr(Box::new(variable("a", None).plus(num(1.0)))),
            None,
        );
        let out = emit_expression(&arrow);
        assert!(out.contains("=>"), "got: {out}");
        assert!(out.contains("a"), "got: {out}");
    }

    #[test]
    fn unsupported_node_emits_placeholder_not_panic() {
        let rx = o::Expr::bare(ExprKind::RegExpLiteral {
            body: "abc".to_string(),
            flags: None,
        });
        let out = emit_expression(&rx);
        assert!(out.contains("__unsupported_RegExpLiteral"), "got: {out}");
    }
}
