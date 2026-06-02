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
//! # Template / i18n / regex / dynamic-import lowering
//! Regular-expression literals lower to a real `oxc_ast` [`RegExp`] literal,
//! template literals to a real [`TemplateLiteral`], tagged template literals to a
//! `TaggedTemplateExpression`, dynamic imports to a real `ImportExpression`
//! (`import(url)`), and i18n `LocalizedString`s to the faithful `$localize`
//! *tagged-template* form (`$localize\`:meta:head${expr}tail\``) Angular's
//! `AbstractEmitterVisitor.visitLocalizedString` produces — including the
//! `serializeI18nHead` / `serializeI18nTemplatePart` cooked/raw metadata blocks.
//! The only remaining placeholder is `WrappedNode` (an opaque foreign-AST handle
//! with no `output_ast`-level payload to lower); it emits a clearly-named
//! `__unsupported_WrappedNode` identifier so emission never aborts.

use std::cell::RefCell;

use oxc_allocator::{Allocator, Box as ArenaBox, Vec as ArenaVec};
use oxc_ast::AstBuilder;
use oxc_ast::ast::{
    Argument, ArrayExpressionElement, AssignmentOperator, AssignmentTarget, BinaryOperator as OxBin,
    BindingPattern, Declaration, Expression, FormalParameterKind, FunctionBody, FunctionType,
    ImportOrExportKind, LogicalOperator, NumberBase, ObjectPropertyKind, PropertyKey, PropertyKind,
    RegExp, RegExpFlags, RegExpPattern, SimpleAssignmentTarget, Statement, TemplateElement,
    TemplateElementValue, TemplateLiteral, UnaryOperator as OxUn, VariableDeclarationKind,
};
use oxc_codegen::Codegen;
use oxc_span::{SourceType, SPAN};

use crate::output::source_map::{byte_offset_to_line_col, utf16_columns, SourceMapBuilder};
use crate::output_ast::{
    self as o, ArrowBody, BinaryOperator, ExprKind, FnParam, ImportUrl, LiteralMapEntry,
    LiteralValue, ParseSourceSpan, StmtKind, StmtModifier, UnaryOperator,
};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Lower a slice of `output_ast` statements to JS and return the printed source.
///
/// Default (oxc) build: allocates its own arena, lowers each statement, wraps them in a
/// [`Program`], and codegens to a [`String`].
///
/// `--features swc` build: delegates to the engine-neutral printer in
/// [`crate::output::emitter_swc`], which walks the SAME `output_ast` and is tuned to emit the
/// exact bytes oxc_codegen produces (proven byte-for-byte by `tools/backend-parity`). See the
/// module header of `emitter_swc.rs` for why a neutral printer — not `swc_ecma_codegen` — is the
/// SWC-side emitter.
#[cfg(feature = "swc")]
pub fn emit_statements(stmts: &[o::Stmt]) -> String {
    crate::output::emitter_swc::emit_statements(stmts)
}

/// Lower a slice of `output_ast` statements to JS and return the printed source.
///
/// Allocates its own arena, lowers each statement, wraps them in a [`Program`],
/// and codegens to a [`String`].
#[cfg(not(feature = "swc"))]
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
/// `--features swc` build: delegates to the engine-neutral printer in
/// [`crate::output::emitter_swc`] (byte-identical to the oxc path, gated by `backend-parity`).
#[cfg(feature = "swc")]
pub fn emit_expression(expr: &o::Expr) -> String {
    crate::output::emitter_swc::emit_expression(expr)
}

/// Lower a single `output_ast` expression to JS and return the printed source.
///
/// The expression is wrapped in an expression statement so codegen has a complete
/// program to print (the trailing `;` codegen adds is left intact, matching
/// Angular's `visitExpressionStmt`).
#[cfg(not(feature = "swc"))]
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

/// An original-source anchor recorded while lowering a span-carrying node.
///
/// `token` is the literal text we know the node will print (e.g. a `ReadVar`'s name).
/// After codegen, [`build_segments`] locates each token in the final code — in
/// lowering order — and emits a `generated -> original` source-map segment using `span`.
#[derive(Debug, Clone)]
struct EmittedAnchor {
    /// Byte-offset span into the ORIGINAL authoring source (not the generated code).
    span: ParseSourceSpan,
    /// The exact token text the lowered node prints into the generated code.
    token: String,
}

/// The map-aware counterpart of [`emit_statements`] + [`emit_expression`]. Returns the
/// SAME code string as `emit_statements`/`emit_expression` together with the anchors
/// collected for span-carrying nodes, so a caller that owns the original source text can
/// build a source map without the emitter taking a text dependency.
///
/// [`emit_with_anchors_expr`] wraps the input as an expression statement (matching
/// `emit_expression`); [`emit_with_anchors_stmts`] lowers a statement list (matching
/// `emit_statements`). Both produce code byte-identical to their plain counterpart.
fn emit_with_anchors_expr(expr: &o::Expr) -> (String, Vec<EmittedAnchor>) {
    let allocator = Allocator::default();
    let lowerer = Lowerer::with_anchor_tracking(&allocator);
    let oxc_expr = lowerer.lower_expr(expr);
    let stmt = lowerer.ast.statement_expression(SPAN, oxc_expr);
    let mut body = lowerer.ast.vec();
    for import in lowerer.namespace_import_stmts() {
        body.push(import);
    }
    body.push(stmt);
    let code = lowerer.codegen(body);
    (code, lowerer.take_anchors())
}

fn emit_with_anchors_stmts(stmts: &[o::Stmt]) -> (String, Vec<EmittedAnchor>) {
    let allocator = Allocator::default();
    let lowerer = Lowerer::with_anchor_tracking(&allocator);
    let mut lowered = lowerer.ast.vec_with_capacity(stmts.len());
    for stmt in stmts {
        lowered.push(lowerer.lower_stmt(stmt));
    }
    let mut body = lowerer.ast.vec();
    for import in lowerer.namespace_import_stmts() {
        body.push(import);
    }
    for stmt in lowered {
        body.push(stmt);
    }
    let code = lowerer.codegen(body);
    (code, lowerer.take_anchors())
}

/// Build a [`SourceMapBuilder`] for a compiled component's `(code, map)` pair.
///
/// `code` is the FINAL emitted code (byte-identical to `emit_*`). `anchors` are the
/// span-carrying nodes recorded during lowering. `source_name` / `source_content` describe
/// the original authoring source the spans index into. Each anchor's `token` is located in
/// `code` (scanning forward from the previous match so order is respected) and a segment is
/// emitted from that generated position to the anchor's original position. Anchors whose
/// token cannot be found are skipped (honest coverage — no fabricated mapping).
fn build_source_map(
    file_name: &str,
    source_name: &str,
    source_content: &str,
    code: &str,
    anchors: &[EmittedAnchor],
) -> SourceMapBuilder {
    let mut builder = SourceMapBuilder::new(file_name.to_string());
    let src_index = builder.add_source(source_name, Some(source_content.to_string()));

    // Precompute generated line-start byte offsets so a byte position -> (line, col) is a
    // binary search rather than a rescan per anchor.
    let mut search_from = 0usize;
    for anchor in anchors {
        if anchor.token.is_empty() {
            continue;
        }
        let Some(rel) = code[search_from..].find(&anchor.token) else {
            continue;
        };
        let gen_byte = search_from + rel;
        // Advance the search cursor past this match so repeated tokens map in order.
        search_from = gen_byte + anchor.token.len();

        let generated = generated_byte_to_line_col(code, gen_byte);
        let original = byte_offset_to_line_col(source_content, anchor.span.start);
        builder.add_segment(generated, src_index, original);
    }
    builder
}

/// Convert a byte offset in the GENERATED code into a (line, UTF-16 column) position.
fn generated_byte_to_line_col(
    code: &str,
    byte_offset: usize,
) -> crate::output::source_map::LineCol {
    let offset = byte_offset.min(code.len());
    let mut line: u32 = 0;
    let mut line_start = 0usize;
    for (i, b) in code.as_bytes().iter().enumerate() {
        if i >= offset {
            break;
        }
        if *b == b'\n' {
            line += 1;
            line_start = i + 1;
        }
    }
    let column = utf16_columns(&code[line_start..offset]);
    crate::output::source_map::LineCol::new(line, column)
}

/// Emit `(code, source_map_json)` for a slice of statements, where `code` is BYTE-IDENTICAL
/// to [`emit_statements`]. The v3 source map maps span-carrying nodes back into
/// `source_content` (named `source_name`); the generated file is `file_name`.
#[cfg(not(feature = "swc"))]
pub fn emit_statements_with_map(
    stmts: &[o::Stmt],
    file_name: &str,
    source_name: &str,
    source_content: &str,
) -> (String, String) {
    let (code, anchors) = emit_with_anchors_stmts(stmts);
    let map = build_source_map(file_name, source_name, source_content, &code, &anchors);
    (code, map.to_json())
}

/// `--features swc` build of [`emit_statements_with_map`] — delegates to the neutral printer.
#[cfg(feature = "swc")]
pub fn emit_statements_with_map(
    stmts: &[o::Stmt],
    file_name: &str,
    source_name: &str,
    source_content: &str,
) -> (String, String) {
    crate::output::emitter_swc::emit_statements_with_map(stmts, file_name, source_name, source_content)
}

/// Build a v3 source-map JSON for a definition `expr` that has already been printed into
/// `full_code` starting at byte offset `expr_offset` (e.g. after a pool-statement prefix).
///
/// Re-lowers `expr` purely to recover its span-carrying anchors, then locates each anchor's
/// token in `full_code` from `expr_offset` onward — so the generated positions are correct
/// in the FINAL concatenated artifact, not in the isolated expression slice. The code is
/// never reprinted into `full_code` here; this is map-only and additive.
#[cfg(feature = "swc")]
pub fn build_definition_map(
    file_name: &str,
    source_name: &str,
    source_content: &str,
    full_code: &str,
    expr_offset: usize,
    expr: &o::Expr,
) -> String {
    crate::output::emitter_swc::build_definition_map(
        file_name,
        source_name,
        source_content,
        full_code,
        expr_offset,
        expr,
    )
}

#[cfg(not(feature = "swc"))]
pub fn build_definition_map(
    file_name: &str,
    source_name: &str,
    source_content: &str,
    full_code: &str,
    expr_offset: usize,
    expr: &o::Expr,
) -> String {
    // Re-lower with anchor tracking to recover the anchors (the printed code is discarded).
    let (_discarded, anchors) = emit_with_anchors_expr(expr);

    let mut builder = SourceMapBuilder::new(file_name.to_string());
    let src_index = builder.add_source(source_name, Some(source_content.to_string()));
    let mut search_from = expr_offset.min(full_code.len());
    for anchor in &anchors {
        if anchor.token.is_empty() {
            continue;
        }
        let Some(rel) = full_code[search_from..].find(&anchor.token) else {
            continue;
        };
        let gen_byte = search_from + rel;
        search_from = gen_byte + anchor.token.len();
        let generated = generated_byte_to_line_col(full_code, gen_byte);
        let original = byte_offset_to_line_col(source_content, anchor.span.start);
        builder.add_segment(generated, src_index, original);
    }
    builder.to_json()
}

/// Emit `(code, source_map_json)` for a single expression, where `code` is BYTE-IDENTICAL
/// to [`emit_expression`].
#[cfg(not(feature = "swc"))]
pub fn emit_expression_with_map(
    expr: &o::Expr,
    file_name: &str,
    source_name: &str,
    source_content: &str,
) -> (String, String) {
    let (code, anchors) = emit_with_anchors_expr(expr);
    let map = build_source_map(file_name, source_name, source_content, &code, &anchors);
    (code, map.to_json())
}

/// `--features swc` build of [`emit_expression_with_map`] — delegates to the neutral printer.
#[cfg(feature = "swc")]
pub fn emit_expression_with_map(
    expr: &o::Expr,
    file_name: &str,
    source_name: &str,
    source_content: &str,
) -> (String, String) {
    crate::output::emitter_swc::emit_expression_with_map(expr, file_name, source_name, source_content)
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
    /// `Some` when this lowering pass should record source-map anchors for span-carrying
    /// nodes; `None` for the plain `emit_*` path (zero overhead, no behavioural change).
    anchors: Option<RefCell<Vec<EmittedAnchor>>>,
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
            anchors: None,
        }
    }

    /// Like [`Lowerer::new`] but additionally records source-map anchors for every
    /// span-carrying node lowered. Used only by the `emit_*_with_map` paths.
    fn with_anchor_tracking(allocator: &'a Allocator) -> Self {
        Lowerer {
            ast: AstBuilder::new(allocator),
            imports: RefCell::new(ImportManager::default()),
            anchors: Some(RefCell::new(Vec::new())),
        }
    }

    /// Drain the recorded anchors (empty when anchor tracking is off).
    fn take_anchors(&self) -> Vec<EmittedAnchor> {
        self.anchors
            .as_ref()
            .map(|a| a.borrow().clone())
            .unwrap_or_default()
    }

    /// Record an anchor mapping `span` (original-source bytes) to the literal `token`
    /// the lowered node will print. No-op when anchor tracking is off, so the plain
    /// `emit_*` path stays allocation-free and byte-identical.
    fn record_anchor(&self, span: &Option<ParseSourceSpan>, token: &str) {
        if let (Some(anchors), Some(span)) = (self.anchors.as_ref(), span) {
            anchors.borrow_mut().push(EmittedAnchor {
                span: span.clone(),
                token: token.to_string(),
            });
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
        let code = Codegen::new().build(&program).code;
        // Two value-preserving text post-passes over the printed source, each of
        // which skips string / template / comment (/ regex) spans so only real code
        // tokens are touched:
        //  1. `normalize_numeric_literals` undoes oxc_codegen's scientific re-spelling
        //     of round numbers (`1000` -> `1e3`), restoring the plain decimal form
        //     Angular's TypeScript printer always emits.
        //  2. `drop_single_param_arrow_parens` strips the redundant parens oxc puts
        //     around a lone simple arrow parameter (`(x) =>` -> `x =>`).
        let code = normalize_numeric_literals(&code);
        let code = drop_single_param_arrow_parens(&code);
        //  3. `escape_replacement_chars` rewrites every raw U+FFFD code point to the six-character
        //     `�` escape. The i18n runtime placeholder magic strings (`�0�`, `�#1�`, …) carry
        //     U+FFFD; TypeScript's printer (which Angular's goldens are produced by) escapes it,
        //     whereas oxc_codegen emits the raw code point. U+FFFD never appears as legitimate raw
        //     output outside these string/template-literal placeholders, so the global rewrite is
        //     value-preserving.
        escape_replacement_chars(&code)
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
                // visitDeclareVarStmt; the JS emitter forces `var`, which this lowering
                // intentionally does not model).
                let kind = if stmt.meta.modifiers.has_modifier(StmtModifier::FINAL) {
                    VariableDeclarationKind::Const
                } else {
                    VariableDeclarationKind::Let
                };
                // The declared `name` is printed verbatim; anchor it when a span exists.
                self.record_anchor(&stmt.meta.span, name);
                self.lower_var_decl(kind, name, value.as_ref())
            }
            StmtKind::DeclareFunction {
                name,
                params,
                statements,
                ..
            } => {
                // The function declaration prints its `name` token; anchor it when the
                // front-end recorded a source span for this declaration.
                self.record_anchor(&stmt.meta.span, name);
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
            ExprKind::ReadVar { name } => {
                // A read of a named variable prints exactly `name`; if the front-end
                // recorded where this read came from in the original source, anchor it.
                self.record_anchor(&expr.meta.span, name);
                self.ident_expr(name)
            }

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
                    // A `Spread` entry becomes a real `...x` array element; everything
                    // else is a plain expression element.
                    if let ExprKind::Spread(inner) = &e.kind {
                        let el = self.lower_expr(inner);
                        elements.push(self.ast.array_expression_element_spread_element(SPAN, el));
                    } else {
                        let el = self.lower_expr(e);
                        elements.push(ArrayExpressionElement::from(el));
                    }
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

            ExprKind::TemplateLiteral {
                elements,
                expressions,
            } => self.lower_template_literal(elements, expressions),

            // A standalone template-literal element is only meaningful inside a
            // `TemplateLiteral`; emit it as a single-quasi template literal so the
            // text is preserved and well-formed.
            ExprKind::TemplateLiteralElement(el) => {
                self.lower_template_literal(std::slice::from_ref(el), &[])
            }

            ExprKind::TaggedTemplate { tag, template } => {
                let tag_expr = self.lower_expr(tag);
                // `template` is always a `TemplateLiteral` node; build the quasi.
                let quasi = match &template.kind {
                    ExprKind::TemplateLiteral {
                        elements,
                        expressions,
                    } => self.template_literal_node(elements, expressions),
                    // Defensive: a non-template payload degrades to an empty quasi.
                    _ => self.template_literal_node(&[], &[]),
                };
                self.ast
                    .expression_tagged_template(SPAN, tag_expr, oxc_ast::NONE, quasi)
            }

            ExprKind::RegExpLiteral { body, flags } => {
                let pattern_text = self.ast.str(body);
                let regexp = RegExp {
                    pattern: RegExpPattern {
                        text: pattern_text,
                        pattern: None,
                    },
                    flags: parse_regexp_flags(flags.as_deref().unwrap_or("")),
                };
                // `raw` lets codegen print `/body/flags` verbatim.
                let raw_text = match flags {
                    Some(f) => format!("/{body}/{f}"),
                    None => format!("/{body}/"),
                };
                let raw = self.ast.str(&raw_text);
                self.ast.expression_reg_exp_literal(SPAN, regexp, Some(raw))
            }

            // i18n `LocalizedString` -> the `$localize` tagged-template form (the
            // faithful, non-downlevelled output of Angular's
            // `AbstractEmitterVisitor.visitLocalizedString`):
            // `$localize\`:meta:part0${e0}part1...\``. The cooked/raw of each quasi
            // come from `serialize_i18n_head` / `serialize_i18n_template_part`.
            ExprKind::LocalizedString {
                meta,
                message_parts,
                placeholders,
                expressions,
            } => self.lower_localized_string(meta, message_parts, placeholders, expressions),

            // `WrappedNode` is an opaque foreign-AST handle with no output_ast-level
            // payload to lower to an oxc node; keep the visible placeholder.
            ExprKind::WrappedNode(_) => self.unsupported("WrappedNode"),

            ExprKind::DynamicImport { url, .. } => {
                // A real `import(<url>)` ImportExpression.
                let source = match url {
                    ImportUrl::Str(s) => {
                        let v = self.ast.str(s);
                        self.ast.expression_string_literal(SPAN, v, None)
                    }
                    ImportUrl::Expr(e) => self.lower_expr(e),
                };
                self.ast.expression_import(SPAN, source, None, None)
            }
        }
    }

    /// Build a real `oxc_ast` [`TemplateLiteral`] expression from `output_ast`
    /// elements + interpolated expressions (mirrors `visitTemplateLiteralExpr`).
    fn lower_template_literal(
        &self,
        elements: &[o::TemplateLiteralElement],
        expressions: &[o::Expr],
    ) -> Expression<'a> {
        let quasi = self.template_literal_node(elements, expressions);
        Expression::TemplateLiteral(self.ast.alloc(quasi))
    }

    /// Build the `TemplateLiteral` AST node (shared by template + tagged-template).
    /// The N quasis interleave with N-1 expressions; the final quasi is `tail=true`.
    fn template_literal_node(
        &self,
        elements: &[o::TemplateLiteralElement],
        expressions: &[o::Expr],
    ) -> TemplateLiteral<'a> {
        let mut quasis = self.ast.vec_with_capacity(elements.len().max(1));
        let mut exprs = self.ast.vec_with_capacity(expressions.len());

        if elements.is_empty() {
            // A template literal must always have at least one quasi.
            quasis.push(self.template_element("", "", true));
        } else {
            let last = elements.len() - 1;
            for (i, el) in elements.iter().enumerate() {
                quasis.push(self.template_element(&el.text, &el.raw_text, i == last));
                if let Some(e) = expressions.get(i) {
                    exprs.push(self.lower_expr(e));
                }
            }
        }
        self.ast.template_literal(SPAN, quasis, exprs)
    }

    /// Build a `TemplateElement` with explicit cooked + raw text (raw passed
    /// through verbatim; the caller is responsible for any escaping).
    fn template_element(&self, cooked: &str, raw: &str, tail: bool) -> TemplateElement<'a> {
        let value = TemplateElementValue {
            raw: self.ast.str(raw),
            cooked: Some(self.ast.str(cooked)),
        };
        self.ast.template_element(SPAN, value, tail, false)
    }

    /// Lower a `LocalizedString` to `$localize\`...\`` as a `TaggedTemplateExpression`
    /// whose tag is the `$localize` identifier. The quasi's first part carries the
    /// serialized meta block (`serialize_i18n_head`); each subsequent part carries
    /// the placeholder meta block (`serialize_i18n_template_part`).
    fn lower_localized_string(
        &self,
        meta: &o::I18nMeta,
        message_parts: &[o::LiteralPiece],
        placeholders: &[o::PlaceholderPiece],
        expressions: &[o::Expr],
    ) -> Expression<'a> {
        let tag = self.ident_expr("$localize");
        let n = message_parts.len();
        let mut quasis = self.ast.vec_with_capacity(n.max(1));
        let mut exprs = self.ast.vec_with_capacity(expressions.len());

        if message_parts.is_empty() {
            quasis.push(self.template_element("", "", true));
        } else {
            let head = serialize_i18n_head(meta, &message_parts[0].text);
            quasis.push(self.template_element(&head.0, &head.1, n == 1));
            for i in 1..n {
                if let Some(e) = expressions.get(i - 1) {
                    exprs.push(self.lower_expr(e));
                }
                let part = serialize_i18n_template_part(&placeholders[i - 1], &message_parts[i].text);
                quasis.push(self.template_element(&part.0, &part.1, i == n - 1));
            }
        }
        let quasi = self.ast.template_literal(SPAN, quasis, exprs);
        self.ast
            .expression_tagged_template(SPAN, tag, oxc_ast::NONE, quasi)
    }

    fn lower_literal(&self, value: &LiteralValue) -> Expression<'a> {
        match value {
            LiteralValue::String(s) => {
                let v = self.ast.str(s);
                self.ast.expression_string_literal(SPAN, v, None)
            }
            LiteralValue::Number(n) => {
                // Supply an explicit `raw` text so the OXC printer emits the
                // literal verbatim. Without it the printer re-derives a minimal
                // form and renders integers like 2000 as `2e3`, whereas Angular
                // (via the TypeScript printer) always emits the plain decimal.
                let raw = Self::format_number(*n);
                let raw = self.ast.str(&raw);
                self.ast
                    .expression_numeric_literal(SPAN, *n, Some(raw), NumberBase::Decimal)
            }
            LiteralValue::Bool(b) => self.ast.expression_boolean_literal(SPAN, *b),
            LiteralValue::Null => self.ast.expression_null_literal(SPAN),
            // `undefined` is an identifier in JS, not a literal.
            LiteralValue::Undefined => self.ident_expr("undefined"),
        }
    }

    /// Format an `f64` the way JavaScript's `Number.prototype.toString()`
    /// does, which is what the TypeScript printer Angular uses produces. This
    /// keeps plain decimals/integers verbatim (`2000`, `1000`, `123.456`)
    /// instead of letting OXC collapse them to scientific notation (`2e3`).
    ///
    /// JS only switches to exponential form when the decimal exponent is
    /// `>= 21` or `<= -7`; for everything in between it emits the fixed form.
    /// Realistic Angular literals fall in the fixed range, so Rust's default
    /// `{}` formatting matches JS exactly there. We special-case the extreme
    /// magnitudes to stay faithful for the rare large/small values.
    fn format_number(n: f64) -> String {
        if !n.is_finite() {
            // NaN / Infinity are not valid numeric literals; fall back to a
            // textual form that round-trips through the printer.
            if n.is_nan() {
                return "NaN".to_string();
            }
            return if n < 0.0 { "-Infinity".to_string() } else { "Infinity".to_string() };
        }
        if n == 0.0 {
            // Covers both +0.0 and -0.0 (JS prints both as "0").
            return "0".to_string();
        }

        let abs = n.abs();
        // Outside JS's fixed-notation window, defer to Rust's exponential
        // formatting (close enough for these vanishingly rare values).
        if abs >= 1e21 || abs < 1e-6 {
            return format!("{n:e}");
        }

        // Within the window Rust's default formatting matches JS's output.
        format!("{n}")
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
            LiteralMapEntry::Property { key, value, quoted } => {
                // A `quoted` key emits as a string-literal property name (`{"foo": …}`); otherwise
                // an identifier name (`{foo: …}`). Angular's emitter (`AbstractJsEmitterVisitor`'s
                // `visitLiteralMapExpr`) keys on the entry's `quoted` flag — i18n `goog.getMsg`
                // placeholder/`original_code` maps use quoted keys, plain object literals do not.
                let prop_key: PropertyKey = if *quoted {
                    let s = self.ast.str(key);
                    let lit = self.ast.expression_string_literal(SPAN, s, None);
                    PropertyKey::from(lit)
                } else {
                    self.ast.property_key_static_identifier(SPAN, self.ast.ident(key))
                };
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
// Numeric-literal de-minification.
//
// `oxc_codegen` (in its default, non-TypeScript context) IGNORES the `raw` text we
// attach to numeric literals and re-derives the shortest spelling via
// `print_non_negative_float` — so a round integer like `1000` is emitted as `1e3`,
// `12000` as `12e3`, and large/round values can even appear as hex (`0x...`).
// Angular's TypeScript printer always emits the plain `Number.prototype.toString()`
// form (`1000`, `123.456`). This post-pass walks the printed source byte-wise,
// skipping string / template-literal / comment / regex spans, finds each *token-
// bounded* numeric literal and reprints it via [`format_number`] (the same routine
// `lower_literal` used to build the now-ignored `raw`). The rewrite is value-
// preserving: every recognised token is parsed back to the identical `f64` and
// re-spelled in the canonical decimal form Angular emits. Tokens we cannot losslessly
// reparse (BigInt `…n`, separators) are left exactly as printed.
// ---------------------------------------------------------------------------

/// Format an `f64` the way JavaScript's `Number.prototype.toString()` does, which is
/// what the TypeScript printer Angular uses produces. This keeps plain
/// decimals/integers verbatim (`2000`, `1000`, `123.456`) instead of OXC's collapsed
/// scientific notation (`2e3`).
///
/// JS only switches to exponential form when the decimal exponent is `>= 21` or
/// `<= -7`; for everything in between it emits the fixed form. Realistic Angular
/// literals fall in the fixed range, so Rust's default `{}` formatting matches JS
/// exactly there. We special-case the extreme magnitudes to stay faithful for the
/// rare large/small values.
fn format_number(n: f64) -> String {
    if !n.is_finite() {
        // NaN / Infinity are not valid numeric literals; fall back to a
        // textual form that round-trips through the printer.
        if n.is_nan() {
            return "NaN".to_string();
        }
        return if n < 0.0 { "-Infinity".to_string() } else { "Infinity".to_string() };
    }
    if n == 0.0 {
        // Covers both +0.0 and -0.0 (JS prints both as "0").
        return "0".to_string();
    }

    let abs = n.abs();
    // Outside JS's fixed-notation window, defer to Rust's exponential
    // formatting (close enough for these vanishingly rare values).
    if abs >= 1e21 || abs < 1e-6 {
        return format!("{n:e}");
    }

    // Within the window Rust's default formatting matches JS's output.
    format!("{n}")
}

/// Is `b` a byte that can be the FIRST character of a numeric-literal token? Only a
/// leading decimal digit; the `.5` lead-dot form is handled by the caller (it must look
/// ahead one byte to confirm a following digit).
fn is_number_start(b: u8) -> bool {
    b.is_ascii_digit()
}

/// Is `b` a byte that may appear inside a numeric-literal token after the first? Covers
/// decimal digits, the decimal point, exponent marker (`e`/`E`), hex digits, the
/// `0x`/`0b`/`0o` radix letters, the `_` numeric separator and the BigInt `n` suffix.
fn is_number_continue(b: u8) -> bool {
    b.is_ascii_hexdigit()
        || matches!(b, b'.' | b'e' | b'E' | b'x' | b'X' | b'b' | b'B' | b'o' | b'O' | b'_' | b'n')
}

/// Parse a JS numeric-literal token (decimal, scientific, hex/oct/bin) into its `f64`
/// value. Returns `None` for BigInt (`…n`), separator-bearing, or otherwise non-trivially
/// reparseable tokens so the caller leaves them untouched.
fn parse_js_number(tok: &str) -> Option<f64> {
    if tok.is_empty() || tok.contains('_') || tok.ends_with('n') {
        return None;
    }
    // Radix-prefixed integers: 0x.. / 0b.. / 0o..
    if let Some(hex) = tok.strip_prefix("0x").or_else(|| tok.strip_prefix("0X")) {
        return u128::from_str_radix(hex, 16).ok().map(|v| v as f64);
    }
    if let Some(bin) = tok.strip_prefix("0b").or_else(|| tok.strip_prefix("0B")) {
        return u128::from_str_radix(bin, 2).ok().map(|v| v as f64);
    }
    if let Some(oct) = tok.strip_prefix("0o").or_else(|| tok.strip_prefix("0O")) {
        return u128::from_str_radix(oct, 8).ok().map(|v| v as f64);
    }
    // Decimal / scientific: Rust's f64 parser matches JS's grammar for these.
    tok.parse::<f64>().ok()
}

/// Rewrite each minified numeric literal in `code` to its plain-decimal Angular form.
/// Skips string, template, comment and regex spans so only real numeric tokens in code
/// positions are considered, and only rewrites a token when it is preceded by a non-
/// identifier, non-`.` byte (so member chains like `i0.x` and identifiers like `_r1`
/// are never touched) and reparses losslessly.
fn normalize_numeric_literals(code: &str) -> String {
    let bytes = code.as_bytes();
    let n = bytes.len();
    let mut out: Vec<u8> = Vec::with_capacity(n);
    let mut i = 0usize;
    // The last non-whitespace byte we emitted, used to decide whether a `/` opens a
    // regex (no preceding operand) or is a division operator, and whether a digit run
    // is a fresh numeric token vs. the tail of an identifier.
    let mut prev_significant: u8 = 0;
    while i < n {
        let c = bytes[i];
        match c {
            // String literals: copy verbatim until the matching unescaped quote.
            b'"' | b'\'' => {
                let quote = c;
                out.push(c);
                i += 1;
                while i < n {
                    let b = bytes[i];
                    out.push(b);
                    i += 1;
                    if b == b'\\' && i < n {
                        out.push(bytes[i]);
                        i += 1;
                    } else if b == quote {
                        break;
                    }
                }
                prev_significant = quote;
            }
            // Template literals: copy verbatim until the matching unescaped backtick.
            b'`' => {
                out.push(c);
                i += 1;
                while i < n {
                    let b = bytes[i];
                    out.push(b);
                    i += 1;
                    if b == b'\\' && i < n {
                        out.push(bytes[i]);
                        i += 1;
                    } else if b == b'`' {
                        break;
                    }
                }
                prev_significant = b'`';
            }
            // Line comments.
            b'/' if i + 1 < n && bytes[i + 1] == b'/' => {
                while i < n && bytes[i] != b'\n' {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            // Block comments.
            b'/' if i + 1 < n && bytes[i + 1] == b'*' => {
                out.push(bytes[i]);
                out.push(bytes[i + 1]);
                i += 2;
                while i < n {
                    if bytes[i] == b'*' && i + 1 < n && bytes[i + 1] == b'/' {
                        out.push(bytes[i]);
                        out.push(bytes[i + 1]);
                        i += 2;
                        break;
                    }
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            // Regex literal: a `/` is a regex iff the previous significant byte cannot
            // end an operand. Copy the body + flags verbatim so digits inside are left
            // alone.
            b'/' if regex_can_follow(prev_significant) => {
                out.push(c);
                i += 1;
                let mut in_class = false;
                while i < n {
                    let b = bytes[i];
                    out.push(b);
                    i += 1;
                    if b == b'\\' && i < n {
                        out.push(bytes[i]);
                        i += 1;
                    } else if b == b'[' {
                        in_class = true;
                    } else if b == b']' {
                        in_class = false;
                    } else if b == b'/' && !in_class {
                        break;
                    }
                }
                // Copy trailing flag letters.
                while i < n && bytes[i].is_ascii_alphabetic() {
                    out.push(bytes[i]);
                    i += 1;
                }
                prev_significant = b'/';
            }
            // A numeric-literal token: only when not glued to a preceding identifier /
            // member access (so `_r1`, `i0`, and `obj.field` are never mistaken for a
            // fresh number).
            _ if (is_number_start(c)
                || (c == b'.' && i + 1 < n && bytes[i + 1].is_ascii_digit()))
                && !is_ident_byte(prev_significant)
                && prev_significant != b'.' =>
            {
                let start = i;
                // Advance over the optional lead-in `.`.
                if c == b'.' {
                    i += 1;
                }
                while i < n {
                    let b = bytes[i];
                    if is_number_continue(b) {
                        // `e`/`E` may be followed by an explicit sign.
                        if (b == b'e' || b == b'E')
                            && i + 1 < n
                            && (bytes[i + 1] == b'+' || bytes[i + 1] == b'-')
                        {
                            i += 2;
                            continue;
                        }
                        i += 1;
                    } else {
                        break;
                    }
                }
                let tok = &code[start..i];
                match parse_js_number(tok) {
                    Some(v) => {
                        let canon = format_number(v);
                        out.extend_from_slice(canon.as_bytes());
                        prev_significant = *canon.as_bytes().last().unwrap_or(&b'0');
                    }
                    None => {
                        out.extend_from_slice(tok.as_bytes());
                        prev_significant = *tok.as_bytes().last().unwrap_or(&b'0');
                    }
                }
            }
            _ => {
                out.push(c);
                if !c.is_ascii_whitespace() {
                    prev_significant = c;
                }
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| code.to_string())
}

/// Can a `/` at this position begin a regex literal? True when the previous significant
/// byte cannot terminate an operand (so the `/` is not a division operator). Standard
/// lexer heuristic; conservative — a wrong guess only changes whether a span is scanned
/// as regex vs. code, and numbers inside either are handled correctly regardless.
fn regex_can_follow(prev: u8) -> bool {
    match prev {
        0 => true, // start of input
        b')' | b']' | b'}' | b'"' | b'\'' | b'`' => false,
        b => !is_ident_byte(b),
    }
}

// ---------------------------------------------------------------------------
// Single-parameter arrow parenthesization.
//
// `oxc_codegen` always parenthesizes an arrow's parameter list (it only drops the
// parens around a lone simple binding identifier when `minify` is enabled). Angular's
// own emitter prints `value => value + 1` — no parens around a single simple
// identifier parameter. This post-pass rewrites `(<ident>) =>` back to `<ident> =>`
// for exactly one bare identifier parameter (no default, no destructuring, no rest,
// no type annotation), matching Angular's golden output. String / template-literal
// and comment spans are skipped so literal text is never rewritten.
// ---------------------------------------------------------------------------

/// Is `b` a byte that may appear in a JS identifier? (ASCII fast-path plus any
/// non-ASCII byte, since identifiers may contain Unicode letters and our emitted
/// runtime names use the `ɵ` prefix.)
fn is_ident_byte(b: u8) -> bool {
    b == b'_' || b == b'$' || b.is_ascii_alphanumeric() || b >= 0x80
}

/// Rewrite `(<ident>) =>` to `<ident> =>` for a single bare-identifier arrow
/// parameter. Walks the source byte-wise, skipping string, template and comment
/// spans, so only real code is considered. Any param list that is not exactly one
/// simple identifier (commas, defaults, destructuring, rest, annotations) keeps its
/// parens because the inner scan would hit a non-identifier byte before the `)`.
/// Rewrite every raw U+FFFD replacement character to the six-character `�` escape, matching
/// TypeScript's printer for the i18n placeholder magic strings (`�0�`, `�#1�`,
/// …). oxc_codegen emits U+FFFD as the raw code point; Angular's goldens escape it. U+FFFD only
/// ever appears inside the i18n string/template-literal placeholders, so this whole-source rewrite
/// is value-preserving.
fn escape_replacement_chars(code: &str) -> String {
    if !code.contains('\u{FFFD}') {
        return code.to_string();
    }
    code.replace('\u{FFFD}', "\\uFFFD")
}

fn drop_single_param_arrow_parens(code: &str) -> String {
    let bytes = code.as_bytes();
    let n = bytes.len();
    let mut out: Vec<u8> = Vec::with_capacity(n);
    let mut i = 0usize;
    while i < n {
        let c = bytes[i];
        match c {
            // String literals: copy verbatim until the matching unescaped quote.
            b'"' | b'\'' => {
                let quote = c;
                out.push(c);
                i += 1;
                while i < n {
                    let b = bytes[i];
                    out.push(b);
                    i += 1;
                    if b == b'\\' && i < n {
                        out.push(bytes[i]);
                        i += 1;
                    } else if b == quote {
                        break;
                    }
                }
            }
            // Template literals: copy verbatim until the matching unescaped backtick.
            // (Interpolation contents are copied too; an arrow inside `${...}` is rare
            // in emitted Ivy and not worth a nested parser here.)
            b'`' => {
                out.push(c);
                i += 1;
                while i < n {
                    let b = bytes[i];
                    out.push(b);
                    i += 1;
                    if b == b'\\' && i < n {
                        out.push(bytes[i]);
                        i += 1;
                    } else if b == b'`' {
                        break;
                    }
                }
            }
            // Comments: copy verbatim to end-of-line / `*/`.
            b'/' if i + 1 < n && bytes[i + 1] == b'/' => {
                while i < n && bytes[i] != b'\n' {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b'/' if i + 1 < n && bytes[i + 1] == b'*' => {
                out.push(bytes[i]);
                out.push(bytes[i + 1]);
                i += 2;
                while i < n {
                    if bytes[i] == b'*' && i + 1 < n && bytes[i + 1] == b'/' {
                        out.push(bytes[i]);
                        out.push(bytes[i + 1]);
                        i += 2;
                        break;
                    }
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            // Candidate arrow-param list: `(` <ident> `)` optional-ws `=>`.
            b'(' => {
                if let Some(next) = try_single_param_arrow(bytes, i) {
                    let (ident_start, ident_end, after_paren) = next;
                    // Emit the identifier (dropping the surrounding parens), then resume
                    // scanning at the byte after `)` so the ` =>` is copied normally.
                    out.extend_from_slice(&bytes[ident_start..ident_end]);
                    i = after_paren;
                } else {
                    out.push(c);
                    i += 1;
                }
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    // `out` only ever contains bytes copied from valid UTF-8 `code` (whole `(`/ident/
    // string spans), so it is valid UTF-8.
    String::from_utf8(out).unwrap_or(code.to_string())
}

/// If `bytes[open]` is a `(` that begins a single bare-identifier arrow parameter
/// list — `(ident) =>` — return `(ident_start, ident_end, index_after_close_paren)`.
/// Returns `None` for anything else (empty parens, multiple params, defaults,
/// destructuring, rest, type annotations, or a non-arrow `(...)` group).
fn try_single_param_arrow(bytes: &[u8], open: usize) -> Option<(usize, usize, usize)> {
    let n = bytes.len();
    debug_assert_eq!(bytes[open], b'(');
    let ident_start = open + 1;
    // First identifier byte must be a non-digit identifier start. Reject `(0` etc. so
    // we never strip parens off a parenthesized expression like `(0, fn)(...)`.
    let mut j = ident_start;
    if j >= n {
        return None;
    }
    let first = bytes[j];
    if !(first == b'_' || first == b'$' || first.is_ascii_alphabetic() || first >= 0x80) {
        return None;
    }
    j += 1;
    while j < n && is_ident_byte(bytes[j]) {
        j += 1;
    }
    let ident_end = j;
    // The very next byte must be the closing `)` — no spaces, commas, `=`, `:`, etc.
    // oxc emits arrow params with no inner padding, so a tight `)` is the simple-param
    // signature; anything else means a more complex list we must leave parenthesized.
    if j >= n || bytes[j] != b')' {
        return None;
    }
    let after_paren = j + 1;
    // After `)` (skipping whitespace) must come `=>` to confirm this is an arrow head
    // and not a call/group. This rules out `foo(x)` (followed by `.`/`;`/etc.).
    let mut k = after_paren;
    while k < n && (bytes[k] == b' ' || bytes[k] == b'\t' || bytes[k] == b'\n' || bytes[k] == b'\r') {
        k += 1;
    }
    if k + 1 < n && bytes[k] == b'=' && bytes[k + 1] == b'>' {
        Some((ident_start, ident_end, after_paren))
    } else {
        None
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
// Regex flag parsing.
// ---------------------------------------------------------------------------

/// Parse a JS regex flag string (`"gi"`, `"sm"`, ...) into oxc [`RegExpFlags`].
/// Unknown chars are ignored (oxc only models the standard `gimsuydv` set).
fn parse_regexp_flags(flags: &str) -> RegExpFlags {
    let mut out = RegExpFlags::empty();
    for c in flags.chars() {
        out |= match c {
            'g' => RegExpFlags::G,
            'i' => RegExpFlags::I,
            'm' => RegExpFlags::M,
            's' => RegExpFlags::S,
            'u' => RegExpFlags::U,
            'y' => RegExpFlags::Y,
            'd' => RegExpFlags::D,
            'v' => RegExpFlags::V,
            _ => RegExpFlags::empty(),
        };
    }
    out
}

// ---------------------------------------------------------------------------
// `$localize` cooked/raw serialization (mirror output_ast.ts
// `serializeI18nHead` / `serializeI18nTemplatePart` / `createCookedRawString`).
// ---------------------------------------------------------------------------

const MEANING_SEPARATOR: &str = "|";
const ID_SEPARATOR: &str = "@@";
const LEGACY_ID_INDICATOR: &str = "\u{241f}";

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

/// `createCookedRawString(metaBlock, messagePart)` -> `(cooked, raw)`.
fn create_cooked_raw_string(meta_block: &str, message_part: &str) -> (String, String) {
    if meta_block.is_empty() {
        let cooked = message_part.to_string();
        let raw = escape_for_template_literal(&escape_starting_colon(&escape_slashes(message_part)));
        (cooked, raw)
    } else {
        let cooked = format!(":{meta_block}:{message_part}");
        let raw = escape_for_template_literal(&format!(
            ":{}:{}",
            escape_colons(&escape_slashes(meta_block)),
            escape_slashes(message_part)
        ));
        (cooked, raw)
    }
}

/// `LocalizedString.serializeI18nHead()` -> `(cooked, raw)` for message part 0.
/// The meta block is `meaning|description@@customId␟legacyId...` (each segment
/// present only when set), per `parseI18nMeta`'s format.
fn serialize_i18n_head(meta: &o::I18nMeta, first_part: &str) -> (String, String) {
    let mut meta_block = meta.description.clone().unwrap_or_default();
    if let Some(meaning) = meta.meaning.as_deref().filter(|m| !m.is_empty()) {
        meta_block = format!("{meaning}{MEANING_SEPARATOR}{meta_block}");
    }
    if let Some(id) = meta.custom_id.as_deref().filter(|i| !i.is_empty()) {
        meta_block = format!("{meta_block}{ID_SEPARATOR}{id}");
    }
    for legacy_id in &meta.legacy_ids {
        meta_block = format!("{meta_block}{LEGACY_ID_INDICATOR}{legacy_id}");
    }
    create_cooked_raw_string(&meta_block, first_part)
}

/// `LocalizedString.serializeI18nTemplatePart(i)` -> `(cooked, raw)`. The meta
/// block is `<placeholder-name>[@@<associated-id>]`, where the associated id is the
/// computed message id of the associated (ICU) message when it has no legacy ids.
fn serialize_i18n_template_part(
    placeholder: &o::PlaceholderPiece,
    message_part: &str,
) -> (String, String) {
    let mut meta_block = placeholder.text.clone();
    if let Some(assoc) = &placeholder.associated_message {
        if assoc.legacy_ids.is_empty() {
            let id = crate::digest::compute_msg_id(
                &assoc.message_string,
                assoc.meaning.as_deref().unwrap_or(""),
            );
            meta_block = format!("{meta_block}{ID_SEPARATOR}{id}");
        }
    }
    create_cooked_raw_string(&meta_block, message_part)
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
    fn string_literal_escapes_replacement_char() {
        // The i18n runtime placeholder magic strings carry U+FFFD; TypeScript's printer escapes it
        // as the six-character `�` sequence. The emitter rewrites the raw code point oxc_codegen
        // would otherwise print, so the literal text appears verbatim in the output.
        let out = emit_expression(&str_lit("\u{FFFD}0\u{FFFD}"));
        assert!(out.contains("\\uFFFD0\\uFFFD"), "expected escaped form, got: {out}");
        assert!(!out.contains('\u{FFFD}'), "raw U+FFFD must not survive, got: {out}");
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
        // A single simple-identifier parameter is printed WITHOUT parens, matching
        // Angular's emitter (`a => a + 1`), not oxc's default `(a) => a + 1`.
        assert!(out.contains("a =>"), "expected unparenthesized single param; got: {out}");
        assert!(!out.contains("(a) =>"), "single param should not be parenthesized; got: {out}");
    }

    #[test]
    fn multi_param_arrow_keeps_parens() {
        // Two params must stay parenthesized: `(a, b) => a + b`.
        let arrow = o::arrow_fn(
            vec![FnParam::new("a", None), FnParam::new("b", None)],
            ArrowBody::Expr(Box::new(variable("a", None).plus(variable("b", None)))),
            None,
        );
        let out = emit_expression(&arrow);
        assert!(out.contains("(a, b) =>"), "multi-param must keep parens; got: {out}");
    }

    #[test]
    fn zero_param_arrow_keeps_parens() {
        // No params must stay as `() => ...`.
        let arrow = o::arrow_fn(vec![], ArrowBody::Expr(Box::new(num(1.0))), None);
        let out = emit_expression(&arrow);
        assert!(out.contains("() =>"), "zero-param must keep parens; got: {out}");
    }

    #[test]
    fn single_param_arrow_inside_call_drops_parens() {
        // `sig.update(value => value + 1)` — the listener-handler shape from the
        // arrow-functions compliance cases. The `(value)` parens must be stripped even
        // though the arrow sits inside a call argument list.
        let arrow = o::arrow_fn(
            vec![FnParam::new("value", None)],
            ArrowBody::Expr(Box::new(variable("value", None).plus(num(1.0)))),
            None,
        );
        let call = variable("update", None).call_fn(vec![arrow], false);
        let out = emit_expression(&call);
        assert!(out.contains("value => value + 1"), "got: {out}");
        assert!(!out.contains("(value) =>"), "got: {out}");
    }

    #[test]
    fn arrow_param_parens_not_stripped_inside_string_literal() {
        // A string literal that happens to contain `(x) =>` must be left untouched.
        let s = str_lit("(x) => y");
        let out = emit_expression(&s);
        assert!(out.contains("(x) => y"), "string content must be preserved; got: {out}");
    }

    #[test]
    fn regexp_literal_emits_real_regex() {
        let rx = o::Expr::bare(ExprKind::RegExpLiteral {
            body: "abc".to_string(),
            flags: Some("gi".to_string()),
        });
        let out = emit_expression(&rx);
        assert!(out.contains("/abc/gi"), "got: {out}");
    }

    #[test]
    fn wrapped_node_still_placeholder_not_panic() {
        use crate::output_ast::WrappedNodeHandle;
        let wn = o::Expr::bare(ExprKind::WrappedNode(WrappedNodeHandle(0)));
        let out = emit_expression(&wn);
        assert!(out.contains("__unsupported_WrappedNode"), "got: {out}");
    }

    #[test]
    fn template_literal_emits_backticks_and_interpolation() {
        use crate::output_ast::TemplateLiteralElement;
        let tl = o::Expr::bare(ExprKind::TemplateLiteral {
            elements: vec![
                TemplateLiteralElement::new("a", None),
                TemplateLiteralElement::new("b", None),
            ],
            expressions: vec![variable("x", None)],
        });
        let out = emit_expression(&tl);
        assert!(out.contains('`'), "got: {out}");
        assert!(out.contains("${"), "got: {out}");
        assert!(out.contains('x'), "got: {out}");
    }

    #[test]
    fn dynamic_import_emits_real_import_expression() {
        use crate::output_ast::ImportUrl;
        let di = o::Expr::bare(ExprKind::DynamicImport {
            url: ImportUrl::Str("./chunk".to_string()),
            url_comment: None,
        });
        let out = emit_expression(&di);
        assert!(out.contains("import(") && out.contains("./chunk"), "got: {out}");
    }

    #[test]
    fn with_map_expression_code_is_byte_identical() {
        // The additive map path must reproduce `emit_expression` byte-for-byte.
        let element = import_expr(R3::Element.reference(), None);
        let call = element.call_fn(vec![num(0.0), str_lit("div")], false);
        let plain = emit_expression(&call);
        let (mapped_code, _map) =
            emit_expression_with_map(&call, "out.js", "src.ts", "const x = 1;");
        assert_eq!(plain, mapped_code, "map path changed expression bytes");
    }

    #[test]
    fn with_map_statements_code_is_byte_identical() {
        let stmt = Stmt::with_modifiers(
            StmtKind::DeclareVar {
                name: "cmp".to_string(),
                value: Some(num(1.0)),
                ty: None,
            },
            StmtModifier::FINAL,
        );
        let plain = emit_statements(std::slice::from_ref(&stmt));
        let (mapped_code, _map) = emit_statements_with_map(
            std::slice::from_ref(&stmt),
            "out.js",
            "src.ts",
            "const cmp = 1;",
        );
        assert_eq!(plain, mapped_code, "map path changed statement bytes");
    }

    #[test]
    fn with_map_anchors_span_carrying_var_decl() {
        use crate::output::source_map::{byte_offset_to_line_col, decode_mappings};
        // A DeclareVar carrying an original-source span anchors its emitted `name` token.
        let source = "let myVar = 1;";
        let var_byte = source.find("myVar").unwrap();
        let mut stmt = Stmt::with_modifiers(
            StmtKind::DeclareVar {
                name: "myVar".to_string(),
                value: Some(num(1.0)),
                ty: None,
            },
            StmtModifier::FINAL,
        );
        stmt.meta.span = Some(ParseSourceSpan::new(var_byte, var_byte + 5));

        let (_code, map) = emit_statements_with_map(
            std::slice::from_ref(&stmt),
            "out.js",
            "src.ts",
            source,
        );
        // The map is valid v3 with content.
        assert!(map.contains("\"version\":3"), "{map}");
        assert!(map.contains("\"sourcesContent\":[\"let myVar = 1;\"]"), "{map}");

        let key = "\"mappings\":\"";
        let s = map.find(key).unwrap() + key.len();
        let rest = &map[s..];
        let mappings = &rest[..rest.find('"').unwrap()];
        let decoded = decode_mappings(mappings).expect("decode");
        let expected = byte_offset_to_line_col(source, var_byte);
        assert!(
            decoded.iter().any(|&(_, _, _, sl, sc)| sl == expected.line && sc == expected.column),
            "no segment for myVar; decoded {decoded:?}; map {map}"
        );
    }

    #[test]
    fn localized_string_emits_dollar_localize_tagged_template() {
        use crate::output_ast::{I18nMeta, LiteralPiece, ParseSourceSpan, PlaceholderPiece};
        let sp = ParseSourceSpan::new(0, 0);
        let ls = o::Expr::bare(ExprKind::LocalizedString {
            meta: I18nMeta {
                description: Some("greeting".to_string()),
                meaning: Some("salute".to_string()),
                custom_id: Some("xyz".to_string()),
                legacy_ids: vec![],
            },
            message_parts: vec![
                LiteralPiece { text: "Hello ".to_string(), source_span: sp.clone() },
                LiteralPiece { text: "!".to_string(), source_span: sp.clone() },
            ],
            placeholders: vec![PlaceholderPiece {
                text: "PH".to_string(),
                source_span: sp.clone(),
                associated_message: None,
            }],
            expressions: vec![variable("name", None)],
        });
        let out = emit_expression(&ls);
        assert!(out.contains("$localize"), "got: {out}");
        assert!(out.contains('`'), "got: {out}");
        // Head meta block: meaning|description@@customId.
        assert!(out.contains(":salute|greeting@@xyz:Hello "), "got: {out}");
        // Placeholder meta block on the second part.
        assert!(out.contains(":PH:!"), "got: {out}");
        assert!(out.contains("${"), "got: {out}");
    }
}
