//! The `swc` PARSE backend — the engine-neutral mirror of [`super::oxc`], built on `swc_ecma_parser`.
//!
//! Compiled ONLY under `--features swc` (the heavy `swc_*` crates are off by default; default users
//! never build them). [`SwcParseBackend`] parses a TypeScript/JS source string with
//! `swc_ecma_parser::parse_file_as_program` — inside its OWN `swc_common::GLOBALS` scope and a
//! per-call `SourceMap` so the parse is self-contained and thread-safe (no shared thread-local state
//! leaks across a `rayon`/`compileMany` parallel compile) — and walks `swc_ecma_ast` to fill the
//! SAME engine-neutral [`super::ParseOutput`] / [`super::ObjLit`] / [`super::LitValue`] structs the
//! oxc backend produces, in SOURCE order.
//!
//! # Byte-identical neutral summary
//!
//! `tools/backend-parity` (and the in-crate `parse_parity` test in [`super`]) assert that the
//! neutral [`super::ParseOutput`] this backend yields is IDENTICAL to oxc's for the whole corpus.
//! The two parity-critical details:
//!
//!   * **Spans.** swc `BytePos` are `SourceMap`-relative; the FIRST (only) source file always starts
//!     at `BytePos(1)` (`SourceMap::next_start_pos` seeds the counter at 1 and adds `len + 1` per
//!     file). Subtracting that 1 turns every span into an ABSOLUTE byte offset into the original
//!     source — exactly oxc's span convention — so the neutral [`super::TreatySpan`] values match and
//!     [`SwcParseBackend::span_text`] can slice the original `source` directly (returning a borrowed
//!     `&'src str`, unlike `SourceMap::span_to_snippet`'s owned `String`).
//!   * **Source order + the literal subset.** The walk mirrors `oxc.rs` shape-for-shape: only
//!     static (identifier / string) object keys are captured; spreads / computed keys are dropped;
//!     numeric literals keep their parsed `f64`; a no-substitution template literal reads as its
//!     cooked string; `Foo.Bar` member access keeps the trailing property name; everything richer
//!     becomes [`super::LitValue::Other`] carrying its span.
//!
//! # Note on the parse-arena / `Module<'a>` escape hatch
//!
//! swc returns an OWNED `Program` (no `'a` arena lifetime). The facade's AOT/linker walks still reach
//! the LIVE oxc `Program` through [`super::oxc::OxcModule::program`]; there is no oxc `Program` to
//! hand back from an swc parse, so this backend's [`ParseBackend::Module`] exposes only the neutral
//! summary + the owned swc `Program`. Flipping the `crate::parse::ParsingBackend` alias to this
//! backend is therefore gated on first neutralizing those walks (SWC-BACKEND-PLAN.md §3.2 phase 3);
//! until then this backend is exercised through the parity gate, not the alias.

use swc_atoms::Wtf8Atom;
use swc_common::sync::Lrc;
use swc_common::{BytePos, FileName, Globals, SourceMap, Span, Spanned, GLOBALS};
use swc_ecma_ast::{
    ArrowExpr, BlockStmtOrExpr, Callee, ClassMember, Decl, DefaultDecl, Decorator, EsVersion, Expr,
    ExprOrSpread, Function, ImportSpecifier, Lit, MemberProp, MethodKind, ModuleDecl, ModuleItem,
    Param, ParamOrTsParamProp, Pat, Program, Prop, PropName, PropOrSpread, Stmt, VarDeclKind,
};
use swc_ecma_parser::{parse_file_as_program, Syntax, TsSyntax};

use super::{
    ClassWithDecorators, DecoratorInfo, ImportInfo, LitValue, MemberInfo, MemberKind, NArg,
    NArrayElement, NArrowBody, NAssignment, NCtorParam, NExpr, NObjectProp, NParam, NStmt, NTopStmt,
    NTypeRef, NVarDeclarator, NgDeclareCall, ObjLit, ParseBackend, ParseOutput, SourceKind,
    StructKind, TreatySpan,
};

/// The swc parse backend. Zero-sized; a fresh `GLOBALS` scope + `SourceMap` is created per
/// [`Self::parse_module`] call so each parse is independent (mirrors the oxc backend's per-call
/// `Allocator::default()`), with NO thread-local state shared across parses.
#[derive(Debug, Default, Clone, Copy)]
pub struct SwcParseBackend;

/// The borrowed parsed-module handle the [`ParseBackend::parse_module`] callback receives. Wraps the
/// owned swc `Program` and the pre-lowered engine-neutral [`ParseOutput`].
///
/// Unlike [`super::oxc::OxcModule`] there is no oxc `Program` to expose — the facade walk that still
/// needs the live AST (`compile_program_with_source`, and the live-oxc handle the AOT driver threads
/// opaquely through `treaty_ivy_decorators::ClassMeta::live`) is not yet neutralized
/// (SWC-BACKEND-PLAN.md §3.2 phase 3), so under this backend it is unreachable. Note the
/// `ClassMeta` PUBLIC surface is now engine-neutral; what stays oxc-typed is only the facade-private
/// live handle that rides through it. The neutral [`Self::summary`] is the contract the parity gate
/// checks.
pub struct SwcModule {
    /// The owned swc program — the escape hatch for a future neutral walk over swc nodes.
    pub program: Program,
    /// The engine-neutral pre-lowered summary (classes / `ɵɵngDeclare*` calls / errors).
    pub summary: ParseOutput,
}

impl SwcModule {
    /// The parsed program (owned swc AST).
    pub fn program(&self) -> &Program {
        &self.program
    }

    /// The engine-neutral pre-lowered summary.
    pub fn summary(&self) -> &ParseOutput {
        &self.summary
    }
}

impl ParseBackend for SwcParseBackend {
    type Module<'a> = SwcModule;

    fn parse_module<'src, R>(
        &self,
        source: &'src str,
        kind: SourceKind<'_>,
        f: impl FnOnce(&Self::Module<'_>) -> R,
    ) -> R {
        // Each parse establishes its OWN GLOBALS scope (swc interns SyntaxContext / atoms through a
        // thread-local) so concurrent compiles never share mutable state.
        let module = GLOBALS.set(&Globals::default(), || {
            let cm = SourceMap::default();
            // The first (only) source file in a fresh map starts at BytePos(1); record that base so
            // spans can be rebased to absolute offsets matching oxc.
            let fm = cm.new_source_file(Lrc::new(FileName::Anon), source.to_string());
            let base = fm.start_pos.0; // == 1 for the first file.

            let syntax = syntax_for(kind);
            let mut errors = Vec::new();
            let parsed = parse_file_as_program(&fm, syntax, EsVersion::EsNext, None, &mut errors);

            // A hard parse failure (`Err`) OR any recovered error means we cannot faithfully extract
            // metadata — surface the diagnostics and an empty summary, exactly like the oxc backend.
            match parsed {
                Ok(program) if errors.is_empty() => {
                    let summary = lower_program(&program, base);
                    SwcModule { program, summary }
                }
                Ok(program) => SwcModule {
                    program,
                    summary: ParseOutput {
                        classes: Vec::new(),
                        ng_declare_calls: Vec::new(),
                        imports: Vec::new(),
                        top_level: Vec::new(),
                        errors: errors.iter().map(|e| format!("{:?}", e.kind())).collect(),
                    },
                },
                Err(e) => SwcModule {
                    program: Program::Module(swc_ecma_ast::Module {
                        span: dummy_span(),
                        body: Vec::new(),
                        shebang: None,
                    }),
                    summary: ParseOutput {
                        classes: Vec::new(),
                        ng_declare_calls: Vec::new(),
                        imports: Vec::new(),
                        top_level: Vec::new(),
                        errors: vec![format!("{:?}", e.kind())],
                    },
                },
            }
        });
        f(&module)
    }

    fn span_text<'src>(&self, source: &'src str, span: TreatySpan) -> &'src str {
        // The neutral spans are already ABSOLUTE offsets into `source` (rebased by `-base` during
        // lowering), so a direct slice yields the borrowed text — matching oxc's `&source[a..b]` and
        // avoiding `SourceMap::span_to_snippet`'s owned `String` (which cannot satisfy `&'src str`).
        let start = (span.start as usize).min(source.len());
        let end = (span.end as usize).min(source.len());
        if start <= end {
            &source[start..end]
        } else {
            ""
        }
    }
}

/// Convert an swc `Wtf8Atom` (the type of a string-literal `value` / template-element `cooked` in
/// swc_ecma_ast 17) to an owned `String`. Angular metadata strings are always valid UTF-8 (selectors,
/// templates, style strings), so `as_str()` (on the `Wtf8` it derefs to) returns `Some` and this is
/// exact; a lone-surrogate value (never seen in real Angular metadata) degrades to the lossy form so
/// the walk never panics.
fn wtf8_to_string(atom: &Wtf8Atom) -> String {
    match atom.as_str() {
        Some(s) => s.to_string(),
        None => atom.to_string_lossy().into_owned(),
    }
}

/// A throwaway dummy span for the empty fallback program.
fn dummy_span() -> Span {
    Span {
        lo: BytePos(0),
        hi: BytePos(0),
    }
}

/// Map a [`SourceKind`] onto the swc `Syntax` the historical oxc call site used. Every facade parse
/// is TypeScript with decorators enabled (the Angular `@Component({...})` form); `.tsx` adds JSX.
fn syntax_for(kind: SourceKind<'_>) -> Syntax {
    let tsx = match kind {
        SourceKind::ByFilename(filename) => filename.to_ascii_lowercase().ends_with(".tsx"),
        _ => false,
    };
    Syntax::Typescript(TsSyntax {
        tsx,
        decorators: true,
        ..Default::default()
    })
}

// ---------------------------------------------------------------------------
// Span rebasing: swc SourceMap-relative BytePos -> absolute offset (oxc convention).
// ---------------------------------------------------------------------------

/// Rebase a swc `Span` to a [`TreatySpan`] of ABSOLUTE byte offsets into the original source by
/// subtracting the source file's `start_pos` (`base`, == 1 for the only file). This makes the neutral
/// span values byte-identical to the oxc backend's absolute offsets.
fn span_of(span: Span, base: u32) -> TreatySpan {
    TreatySpan::new(span.lo.0.saturating_sub(base), span.hi.0.saturating_sub(base))
}

// ---------------------------------------------------------------------------
// Pre-lowering: swc AST -> engine-neutral ParseOutput, in SOURCE order.
// (Shape-for-shape mirror of `oxc.rs::lower_program` and friends.)
// ---------------------------------------------------------------------------

/// Lower a parsed program into the neutral [`ParseOutput`] (decorated classes + `ɵɵngDeclare*` calls,
/// source order).
fn lower_program(program: &Program, base: u32) -> ParseOutput {
    let mut classes = Vec::new();
    let mut ng_declare_calls = Vec::new();
    let mut imports = Vec::new();
    let mut top_level = Vec::new();

    match program {
        Program::Module(module) => {
            for item in &module.body {
                lower_top_level_item(
                    item,
                    base,
                    &mut classes,
                    &mut ng_declare_calls,
                    &mut imports,
                    &mut top_level,
                );
            }
        }
        Program::Script(script) => {
            for stmt in &script.body {
                lower_top_level_stmt(stmt, base, &mut classes, &mut ng_declare_calls);
                top_level.push(lower_top_stmt(stmt, base));
            }
        }
    }

    ParseOutput {
        classes,
        ng_declare_calls,
        imports,
        top_level,
        errors: Vec::new(),
    }
}

/// Handle a module-level item: a statement, an `export`/`export default` wrapping a class, or an
/// `import` declaration (whose bindings feed the neutral import surface). The enclosing item's span
/// is threaded so a decorated class records its `stmt_span` (the whole `export class …` / `@Dec class
/// …` statement), matching the oxc backend's `span_of(stmt)`.
fn lower_top_level_item(
    item: &ModuleItem,
    base: u32,
    classes: &mut Vec<ClassWithDecorators>,
    ng_declares: &mut Vec<NgDeclareCall>,
    imports: &mut Vec<ImportInfo>,
    top_level: &mut Vec<NTopStmt>,
) {
    let item_span = span_of(item.span(), base);
    match item {
        ModuleItem::Stmt(stmt) => {
            lower_top_level_stmt_spanned(stmt, base, item_span, false, classes, ng_declares);
            top_level.push(lower_top_stmt(stmt, base));
        }
        ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => {
            lower_top_level_decl(&export.decl, base, item_span, true, classes, ng_declares);
            // An exported `function f(): RetType {…}` surfaces as a neutral `FnDecl` (the
            // module-with-providers return-type surface), exactly like the oxc backend's
            // `Statement::ExportNamedDeclaration` whose declaration is a function. Every OTHER exported
            // declaration is neither an assignment, a bare-call expression statement, nor a top-level
            // `var`/`let`/`const` scanned by the AOT emitter — record it as `Other` with the whole
            // `export …` statement span (matching the oxc backend, where `export class X {}` is a single
            // `ExportNamedDeclaration` statement that `lower_top_stmt` maps to `Other`).
            match &export.decl {
                Decl::Fn(fn_decl) => top_level.push(lower_fn_decl(fn_decl, item_span)),
                _ => top_level.push(NTopStmt::Other(item_span)),
            }
        }
        ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultDecl(export)) => {
            if let DefaultDecl::Class(class_expr) = &export.decl {
                push_class(
                    class_expr.ident.as_ref().map(|id| id.sym.to_string()),
                    class_expr
                        .ident
                        .as_ref()
                        .map(|id| span_of(id.span, base))
                        .unwrap_or_default(),
                    &class_expr.class,
                    base,
                    item_span,
                    true,
                    classes,
                    ng_declares,
                );
            }
            top_level.push(NTopStmt::Other(item_span));
        }
        ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultExpr(export)) => {
            // `export default ɵɵngDeclare*({...})` as a free expression.
            push_if_ng_declare(&export.expr, base, ng_declares);
            top_level.push(NTopStmt::Other(item_span));
        }
        ModuleItem::ModuleDecl(ModuleDecl::Import(import)) => {
            collect_import(import, imports);
            top_level.push(NTopStmt::Other(item_span));
        }
        _ => top_level.push(NTopStmt::Other(item_span)),
    }
}

/// Collect the local binding names of an `import` declaration (skipping whole-declaration and inline
/// `type`-only specifiers), mirroring the oxc backend's `collect_imports_in_stmt`.
fn collect_import(import: &swc_ecma_ast::ImportDecl, out: &mut Vec<ImportInfo>) {
    let type_only_decl = import.type_only;
    for spec in &import.specifiers {
        let (local, inline_type) = match spec {
            ImportSpecifier::Named(s) => (s.local.sym.to_string(), s.is_type_only),
            ImportSpecifier::Default(s) => (s.local.sym.to_string(), false),
            ImportSpecifier::Namespace(s) => (s.local.sym.to_string(), false),
        };
        out.push(ImportInfo {
            local_name: local,
            type_only: type_only_decl || inline_type,
        });
    }
}

/// Handle a top-level statement (class decl / expr stmt / var decl carrying a `ɵɵngDeclare*`). The
/// statement's own span is used as the enclosing `stmt_span` for any class it declares.
fn lower_top_level_stmt(
    stmt: &Stmt,
    base: u32,
    classes: &mut Vec<ClassWithDecorators>,
    ng_declares: &mut Vec<NgDeclareCall>,
) {
    let stmt_span = span_of(stmt.span(), base);
    lower_top_level_stmt_spanned(stmt, base, stmt_span, false, classes, ng_declares);
}

/// As [`lower_top_level_stmt`] but with the enclosing statement span supplied (the module-item span
/// for a bare statement; equal to the statement's own span for a `Program::Script` body statement).
/// `exported` is `true` when the declaration is wrapped in an `export …` (so a decorated class anchors
/// its span at the `class` keyword, not the decorators — see [`lower_class`]).
fn lower_top_level_stmt_spanned(
    stmt: &Stmt,
    base: u32,
    stmt_span: TreatySpan,
    exported: bool,
    classes: &mut Vec<ClassWithDecorators>,
    ng_declares: &mut Vec<NgDeclareCall>,
) {
    match stmt {
        Stmt::Decl(decl) => {
            lower_top_level_decl(decl, base, stmt_span, exported, classes, ng_declares)
        }
        Stmt::Expr(es) => push_if_ng_declare(&es.expr, base, ng_declares),
        _ => {}
    }
}

/// Handle a declaration: a class (decorated -> a neutral class; always scanned for static
/// `ɵɵngDeclare*` members) or a `var`/`let`/`const` whose initializer is a `ɵɵngDeclare*` call.
fn lower_top_level_decl(
    decl: &Decl,
    base: u32,
    stmt_span: TreatySpan,
    exported: bool,
    classes: &mut Vec<ClassWithDecorators>,
    ng_declares: &mut Vec<NgDeclareCall>,
) {
    match decl {
        Decl::Class(class_decl) => {
            push_class(
                Some(class_decl.ident.sym.to_string()),
                span_of(class_decl.ident.span, base),
                &class_decl.class,
                base,
                stmt_span,
                exported,
                classes,
                ng_declares,
            );
        }
        Decl::Var(var) => {
            for d in &var.decls {
                if let Some(init) = &d.init {
                    push_if_ng_declare(init, base, ng_declares);
                }
            }
        }
        _ => {}
    }
}

/// Push a class: append its neutral form IFF it carries a decorator (matching the oxc backend), and
/// always scan its static members for `ɵɵngDeclare*` calls.
fn push_class(
    name: Option<String>,
    name_span: TreatySpan,
    class: &swc_ecma_ast::Class,
    base: u32,
    stmt_span: TreatySpan,
    exported: bool,
    classes: &mut Vec<ClassWithDecorators>,
    ng_declares: &mut Vec<NgDeclareCall>,
) {
    if !class.decorators.is_empty() {
        classes.push(lower_class(name, name_span, class, base, stmt_span, exported));
    }
    collect_ng_declares_in_class(class, base, ng_declares);
}

/// Lower a class to the neutral [`ClassWithDecorators`] (name + decorators + members, source order).
///
/// The class node's own `span` must match the oxc backend's `Class::span`, and the enclosing
/// `stmt_span` must match oxc's declaring-statement span. The two engines agree EXCEPT for a
/// NON-exported, decorated, ABSTRACT class: oxc anchors both the class span and the statement span at
/// the first decorator (oxc's start span "points at the start of all decorators and class keyword"),
/// while swc's raw `Class.span` / item span start at the `abstract` keyword. (For a non-exported
/// NON-abstract class swc already starts at the decorator; for any EXPORTED class oxc anchors the
/// class span at the `class` keyword — decorators ride on the enclosing `export …` statement — and
/// swc already matches.) So when the class is non-exported and decorated, pull both `span.start` and
/// `stmt_span.start` back to the earliest decorator start, which is a no-op for the already-matching
/// forms and the exact fix for the abstract case. Verified against oxc by the parity probe + the real
/// corpus (`useclass_forwardref.ts`).
fn lower_class(
    name: Option<String>,
    name_span: TreatySpan,
    class: &swc_ecma_ast::Class,
    base: u32,
    stmt_span: TreatySpan,
    exported: bool,
) -> ClassWithDecorators {
    let decorators: Vec<DecoratorInfo> = class
        .decorators
        .iter()
        .map(|d| lower_decorator(d, base))
        .collect();
    let members = class
        .body
        .iter()
        .filter_map(|m| lower_member(m, base))
        .collect::<Vec<_>>();
    let mut span = span_of(class.span, base);
    let mut stmt_span = stmt_span;
    if !exported {
        if let Some(first_dec) = decorators.iter().map(|d| d.span.start).min() {
            span.start = span.start.min(first_dec);
            stmt_span.start = stmt_span.start.min(first_dec);
        }
    }
    ClassWithDecorators {
        name,
        name_span,
        decorators,
        members,
        span,
        stmt_span,
        // The swc backend does not yet recognize `struct`/`shared struct` (the recognizer is M3 —
        // deferred); every class it lowers is a plain `class`. ADDITIVE: this matches the oxc backend
        // for all non-struct sources, so `ParseOutput` stays byte-identical at the parity gate.
        struct_kind: StructKind::None,
    }
}

/// Lower one TOP-LEVEL statement to the neutral [`NTopStmt`] — the surface the AOT→partial emitter
/// (`partial_emit::collect_rewrites`) walks. Mirrors the oxc backend's `lower_top_stmt`: an
/// `X.member = rhs;` assignment expression statement becomes [`NTopStmt::Assignment`]; a bare
/// expression statement becomes [`NTopStmt::ExprStmt`]; a `var`/`let`/`const` becomes
/// [`NTopStmt::VarDecl`]; everything else carries its span as [`NTopStmt::Other`].
fn lower_top_stmt(stmt: &Stmt, base: u32) -> NTopStmt {
    match stmt {
        Stmt::Expr(es) => {
            if let Expr::Assign(assign) = &*es.expr {
                let (target_object, target_member) = assignment_member(assign);
                return NTopStmt::Assignment(NAssignment {
                    target_object,
                    target_member,
                    value: lower_expr(&assign.right, base),
                    value_span: span_of(assign.right.span(), base),
                    span: span_of(stmt.span(), base),
                });
            }
            NTopStmt::ExprStmt {
                expr: lower_expr(&es.expr, base),
                span: span_of(stmt.span(), base),
            }
        }
        Stmt::Decl(Decl::Var(decl)) => {
            let is_const = matches!(decl.kind, VarDeclKind::Const);
            let decls = decl
                .decls
                .iter()
                .map(|d| NVarDeclarator {
                    name: pat_binding_name(&d.name),
                    init: d.init.as_deref().map(|e| lower_expr(e, base)),
                })
                .collect();
            NTopStmt::VarDecl {
                is_const,
                decls,
                span: span_of(stmt.span(), base),
            }
        }
        // A bare `function f(): RetType {…}` declaration. The exported form (`export function …`) is a
        // `ModuleDecl::ExportDecl` (handled in `lower_top_level_item`); this arm covers the bare /
        // `Program::Script` body form, matching the oxc backend's `Statement::FunctionDeclaration`.
        Stmt::Decl(Decl::Fn(fn_decl)) => {
            lower_fn_decl(fn_decl, span_of(stmt.span(), base))
        }
        // A bare (non-exported) decorated class DECLARATION statement: oxc's statement span includes
        // the leading decorators (its class-declaration span starts at the first decorator), but swc's
        // `Stmt::Decl` span starts at the `class`/`abstract` keyword. Pull the `Other` span start back
        // to the earliest decorator so the top-level surface stays byte-identical (the same
        // reconciliation `lower_class` applies to the class span). No-op for an undecorated class.
        Stmt::Decl(Decl::Class(class_decl)) => {
            let mut span = span_of(stmt.span(), base);
            if let Some(first_dec) = class_decl
                .class
                .decorators
                .iter()
                .map(|d| span_of(d.span, base).start)
                .min()
            {
                span.start = span.start.min(first_dec);
            }
            NTopStmt::Other(span)
        }
        _ => NTopStmt::Other(span_of(stmt.span(), base)),
    }
}

/// Lower an swc `FnDecl` to the neutral [`NTopStmt::FnDecl`] (name + annotated return type). Shared by
/// the bare (`Stmt::Decl(Decl::Fn)`) and exported (`ModuleDecl::ExportDecl`) function-declaration
/// paths so both surface byte-identically, matching the oxc backend's single `statement_function`
/// extraction. `span` is the enclosing statement span (the whole `export function …` for the exported
/// form), exactly as the oxc backend uses `span_of(stmt)`.
fn lower_fn_decl(fn_decl: &swc_ecma_ast::FnDecl, span: TreatySpan) -> NTopStmt {
    NTopStmt::FnDecl {
        name: Some(fn_decl.ident.sym.to_string()),
        return_type: fn_decl
            .function
            .return_type
            .as_deref()
            .and_then(|ann| lower_type_ref(&ann.type_ann)),
        span,
    }
}

/// The `<Ident>.<member>` LHS parts of an swc assignment target (`X.ɵfac = …` → `(Some("X"),
/// Some("ɵfac"))`), or `(None, None)` when the target is not a static-member-on-identifier. Mirrors
/// the oxc backend's `assignment_member` + `partial_emit::assignment_member`.
fn assignment_member(assign: &swc_ecma_ast::AssignExpr) -> (Option<String>, Option<String>) {
    use swc_ecma_ast::{AssignTarget, SimpleAssignTarget};
    if let AssignTarget::Simple(SimpleAssignTarget::Member(member)) = &assign.left {
        if let Expr::Ident(obj) = &*member.obj {
            if let MemberProp::Ident(prop) = &member.prop {
                return (Some(obj.sym.to_string()), Some(prop.sym.to_string()));
            }
        }
    }
    (None, None)
}

/// Lower a class member (property / method / getter / setter / constructor / accessor) to the neutral
/// [`MemberInfo`], or [`None`] for a member kind oxc does NOT surface as a class element at all (a
/// stray `;`, i.e. `ClassMember::Empty`) so the member LIST matches oxc element-for-element.
///
/// swc stores a method's (and getter/setter's) decorators on `ClassMethod.function.decorators` — NOT
/// a member-level field like oxc's `MethodDefinition.decorators` — so the method arm reads through
/// `m.function`. This is the parity-critical detail for Angular METHOD decorators (`@HostListener`)
/// and accessor decorators (`@Input() set x`).
///
/// swc models the constructor as a dedicated `Constructor` member; oxc models it as a
/// `MethodDefinition` whose key is the `constructor` identifier. Reading its `PropName` key yields
/// `Some("constructor")` on BOTH backends (the DI / ctor-dep walk keys on it). The constructor carries
/// no member-level decorators (parameter decorators live on the params, which the neutral surface does
/// not model — oxc drops them here too).
///
/// Static blocks / TS index signatures / private members yield an UN-NAMED member (`Some(default)`),
/// matching oxc, which still produces a class element for them (its `key_name` just returns `None`).
fn lower_member(member: &ClassMember, base: u32) -> Option<MemberInfo> {
    match member {
        ClassMember::ClassProp(p) => Some(MemberInfo {
            name: prop_name_str(&p.key),
            decorators: p.decorators.iter().map(|d| lower_decorator(d, base)).collect(),
            kind: MemberKind::Property,
            is_static: p.is_static,
            params: Vec::new(),
            initializer: p.value.as_deref().map(|e| lower_expr(e, base)),
            has_body: false,
            span: span_of(p.span, base),
        }),
        ClassMember::Method(m) => Some(MemberInfo {
            name: prop_name_str(&m.key),
            decorators: m
                .function
                .decorators
                .iter()
                .map(|d| lower_decorator(d, base))
                .collect(),
            kind: match m.kind {
                MethodKind::Method => MemberKind::Method,
                MethodKind::Getter => MemberKind::Getter,
                MethodKind::Setter => MemberKind::Setter,
            },
            is_static: m.is_static,
            params: lower_fn_ctor_params(&m.function.params, base),
            initializer: None,
            // A bodiless TS method overload signature has `function.body == None`.
            has_body: m.function.body.is_some(),
            span: span_of(m.span, base),
        }),
        ClassMember::Constructor(c) => Some(MemberInfo {
            name: prop_name_str(&c.key),
            decorators: Vec::new(),
            kind: MemberKind::Constructor,
            is_static: false,
            params: lower_constructor_params(&c.params, base),
            initializer: None,
            // A bodiless constructor overload signature has `body == None`; the implementation has a
            // body. Constructor-dependency extraction prefers the body-bearing constructor.
            has_body: c.body.is_some(),
            span: span_of(c.span, base),
        }),
        ClassMember::AutoAccessor(a) => Some(MemberInfo {
            name: key_str(&a.key),
            decorators: a.decorators.iter().map(|d| lower_decorator(d, base)).collect(),
            kind: MemberKind::Accessor,
            is_static: a.is_static,
            params: Vec::new(),
            initializer: a.value.as_deref().map(|e| lower_expr(e, base)),
            has_body: false,
            span: span_of(a.span, base),
        }),
        // A stray `;` (`constructor() {};`) is parsed by swc as an `Empty` member but DISCARDED by oxc
        // — drop it so the neutral member list is element-for-element identical.
        ClassMember::Empty(_) => None,
        // StaticBlock / TsIndexSignature / PrivateMethod / PrivateProp: oxc still emits a (nameless)
        // class element for these, so emit an un-named member to keep the counts aligned.
        _ => Some(MemberInfo {
            kind: MemberKind::Other,
            span: span_of(member.span(), base),
            ..MemberInfo::default()
        }),
    }
}

/// Lower a method/function's `Vec<Param>` to neutral [`NCtorParam`]s (name + own decorators + rest
/// flag). A `...rest` is a `Pat::Rest` here (oxc stores it out of `items`); the [`NCtorParam::is_rest`]
/// flag + the inner binding name keep the neutral list aligned with the oxc backend.
fn lower_fn_ctor_params(params: &[Param], base: u32) -> Vec<NCtorParam> {
    params
        .iter()
        .map(|p| {
            let (name, is_rest) = pat_name_and_rest(&p.pat);
            NCtorParam {
                name,
                decorators: p.decorators.iter().map(|d| lower_decorator(d, base)).collect(),
                is_rest,
                // A `...rest` carries no DI token (and oxc surfaces no annotation on its rest element),
                // so leave it `None`; otherwise read the binding's type annotation.
                type_ref: if is_rest { None } else { pat_type_ref(&p.pat) },
            }
        })
        .collect()
}

/// Lower a constructor's `Vec<ParamOrTsParamProp>` to neutral [`NCtorParam`]s. A TS parameter property
/// (`constructor(private dep: Dep)`) is a `TsParamProp` in swc but a plain `FormalParameter` with an
/// accessibility modifier in oxc; BOTH expose the binding identifier + decorators, so the neutral form
/// matches.
fn lower_constructor_params(params: &[ParamOrTsParamProp], base: u32) -> Vec<NCtorParam> {
    params
        .iter()
        .map(|p| match p {
            ParamOrTsParamProp::Param(param) => {
                let (name, is_rest) = pat_name_and_rest(&param.pat);
                NCtorParam {
                    name,
                    decorators: param.decorators.iter().map(|d| lower_decorator(d, base)).collect(),
                    is_rest,
                    type_ref: if is_rest { None } else { pat_type_ref(&param.pat) },
                }
            }
            // A TS parameter property (`private dep: Dep`) is never a rest param. Its type annotation
            // lives on the inner binding identifier (`TsParamPropParam::Ident(BindingIdent).type_ann`),
            // matching oxc's `FormalParameter.type_annotation` for the same `private a: A` shape.
            ParamOrTsParamProp::TsParamProp(prop) => NCtorParam {
                name: match &prop.param {
                    swc_ecma_ast::TsParamPropParam::Ident(id) => Some(id.id.sym.to_string()),
                    swc_ecma_ast::TsParamPropParam::Assign(a) => pat_binding_name(&a.left),
                },
                decorators: prop.decorators.iter().map(|d| lower_decorator(d, base)).collect(),
                is_rest: false,
                type_ref: match &prop.param {
                    swc_ecma_ast::TsParamPropParam::Ident(id) => {
                        id.type_ann.as_deref().and_then(|ann| lower_type_ref(&ann.type_ann))
                    }
                    swc_ecma_ast::TsParamPropParam::Assign(a) => pat_type_ref(&a.left),
                },
            },
        })
        .collect()
}

/// The binding-identifier name of a `Pat`, when it is a plain identifier binding (the only shape the
/// param-name neutral surface models — matching oxc's `get_binding_identifier`).
fn pat_binding_name(pat: &Pat) -> Option<String> {
    match pat {
        Pat::Ident(id) => Some(id.id.sym.to_string()),
        _ => None,
    }
}

/// The binding name + rest flag of a parameter `Pat`. A `...rest` is `Pat::Rest` in swc (oxc stores it
/// out of `items`); unwrap to its inner binding identifier and flag it, so the neutral param list
/// matches the oxc backend's appended-rest representation.
fn pat_name_and_rest(pat: &Pat) -> (Option<String>, bool) {
    match pat {
        Pat::Rest(rest) => (pat_binding_name(&rest.arg), true),
        other => (pat_binding_name(other), false),
    }
}

/// The declared TYPE of a parameter `Pat`, when it is an identifier binding carrying a type
/// annotation (`a: Foo`). swc stores the annotation on `Pat::Ident(BindingIdent).type_ann`, exactly
/// where oxc keeps `FormalParameter.type_annotation`, so [`lower_type_ref`] yields the byte-identical
/// neutral [`NTypeRef`]. `None` for an un-annotated binding or a non-identifier pattern.
fn pat_type_ref(pat: &Pat) -> Option<NTypeRef> {
    match pat {
        Pat::Ident(id) => id.type_ann.as_deref().and_then(|ann| lower_type_ref(&ann.type_ann)),
        _ => None,
    }
}

/// Lower an swc `TsType` to the neutral [`NTypeRef`] — `Some` ONLY for a type REFERENCE
/// (`TsType::TsTypeRef`), `None` for every other type form. Mirrors the oxc backend's `lower_type_ref`
/// shape-for-shape (and `source_compile::type_token_expr` / `module_with_providers_type_arg`).
fn lower_type_ref(ty: &swc_ecma_ast::TsType) -> Option<NTypeRef> {
    let swc_ecma_ast::TsType::TsTypeRef(reference) = ty else {
        return None;
    };
    Some(NTypeRef {
        name_path: ts_entity_name_path(&reference.type_name),
        // Keep ONLY type-reference args (a non-reference arg is dropped — matching the oxc backend +
        // `module_with_providers_type_arg`).
        type_args: reference
            .type_params
            .as_deref()
            .map(|args| args.params.iter().filter_map(|p| lower_type_ref(p)).collect())
            .unwrap_or_default(),
    })
}

/// The dotted NAME segments of an swc `TsEntityName` (`Foo` → `["Foo"]`; `ns.Foo` →
/// `["ns", "Foo"]`; `a.b.C` → `["a", "b", "C"]`). swc has no `this`-type entity name (a `this`-type is
/// a separate `TsType::TsThisType`, which never reaches here — `lower_type_ref` only descends a
/// `TsTypeRef`), so this always yields a non-empty path, matching the oxc backend's `type_name_path`.
fn ts_entity_name_path(name: &swc_ecma_ast::TsEntityName) -> Vec<String> {
    match name {
        swc_ecma_ast::TsEntityName::Ident(id) => vec![id.sym.to_string()],
        swc_ecma_ast::TsEntityName::TsQualifiedName(q) => {
            let mut path = ts_entity_name_path(&q.left);
            path.push(q.right.sym.to_string());
            path
        }
    }
}

/// Lower a decorator to the neutral [`DecoratorInfo`] (callee name + first object-literal argument +
/// full neutral argument list).
fn lower_decorator(dec: &Decorator, base: u32) -> DecoratorInfo {
    let name = decorator_name(dec).unwrap_or_default().to_string();
    let object = decorator_object(dec).map(|obj| lower_object(obj, base));
    let arguments = decorator_arguments(dec, base);
    DecoratorInfo {
        name,
        object,
        arguments,
        span: span_of(dec.span, base),
    }
}

/// The full neutral argument list of a decorator call `@Foo(a, b, …)` (empty for a bare `@Foo`).
fn decorator_arguments(dec: &Decorator, base: u32) -> Vec<NArg> {
    if let Expr::Call(call) = &*dec.expr {
        lower_args(&call.args, base)
    } else {
        Vec::new()
    }
}

/// Returns the callee identifier name of a decorator's expression — bare `@Foo` or call `@Foo({…})`.
fn decorator_name(dec: &Decorator) -> Option<&str> {
    match &*dec.expr {
        Expr::Call(call) => match &call.callee {
            swc_ecma_ast::Callee::Expr(callee) => match &**callee {
                Expr::Ident(id) => Some(&id.sym),
                _ => None,
            },
            _ => None,
        },
        Expr::Ident(id) => Some(&id.sym),
        _ => None,
    }
}

/// The first object-literal argument of a decorator call `@Foo({…})`.
fn decorator_object(dec: &Decorator) -> Option<&swc_ecma_ast::ObjectLit> {
    if let Expr::Call(call) = &*dec.expr {
        for arg in &call.args {
            if arg.spread.is_none() {
                if let Expr::Object(obj) = &*arg.expr {
                    return Some(obj);
                }
            }
        }
    }
    None
}

/// A `PropName`'s static name. Matches the oxc `key_name`, which captures an identifier
/// (`StaticIdentifier`) or a string literal (`StringLiteral`) — INCLUDING a computed key whose
/// expression is a string literal (`{ ['class.x']: … }`), because oxc lowers that to a
/// `PropertyKey::StringLiteral` regardless of the `computed` flag. A computed key with a non-string
/// expression (`{ [x]: … }`), and numeric / bigint keys, are dropped on BOTH backends.
fn prop_name_str(key: &PropName) -> Option<String> {
    match key {
        PropName::Ident(id) => Some(id.sym.to_string()),
        PropName::Str(s) => Some(wtf8_to_string(&s.value)),
        // `{ ['literal']: v }` — oxc keeps this as a static string key; mirror it.
        PropName::Computed(c) => match &*c.expr {
            Expr::Lit(Lit::Str(s)) => Some(wtf8_to_string(&s.value)),
            _ => None,
        },
        _ => None,
    }
}

/// An `AutoAccessor` `Key`'s static name (public identifier / string only).
fn key_str(key: &swc_ecma_ast::Key) -> Option<String> {
    match key {
        swc_ecma_ast::Key::Public(name) => prop_name_str(name),
        swc_ecma_ast::Key::Private(_) => None,
    }
}

/// Lower an swc `ObjectLit` to the neutral [`ObjLit`] — static-keyed `key: value` properties in
/// SOURCE order. Shorthand / spread / method / getter / setter / computed-key entries are dropped
/// (matching the oxc backend, which only captures `ObjectProperty` with a static key).
fn lower_object(obj: &swc_ecma_ast::ObjectLit, base: u32) -> ObjLit {
    let mut props = Vec::with_capacity(obj.props.len());
    for p in &obj.props {
        if let PropOrSpread::Prop(prop) = p {
            if let Prop::KeyValue(kv) = &**prop {
                if let Some(key) = prop_name_str(&kv.key) {
                    props.push((key, lower_value(&kv.value, base)));
                }
            }
        }
    }
    ObjLit {
        props,
        span: span_of(obj.span, base),
        // The lossless full-expression view of the SAME literal (every property, full `NExpr` values).
        nprops: lower_object_props(obj, base),
    }
}

/// Lower an swc `ObjectLit`'s properties to the LOSSLESS [`NObjectProp`] list (every property in
/// source order, key/value as full `NExpr`, spreads + computed keys preserved). Shared by
/// [`lower_object`] (the `ObjLit::nprops` channel) and the `NExpr::Object` arm of [`lower_expr`]. A
/// shorthand `{ child }` is mirrored as the oxc backend does: a `KeyValue` with the identifier as both
/// key and value (oxc models shorthand that way), keeping the two backends byte-identical.
fn lower_object_props(obj: &swc_ecma_ast::ObjectLit, base: u32) -> Vec<NObjectProp> {
    obj.props
        .iter()
        .map(|p| match p {
            PropOrSpread::Spread(sp) => NObjectProp::Spread(lower_expr(&sp.expr, base)),
            PropOrSpread::Prop(prop) => match &**prop {
                Prop::KeyValue(kv) => match prop_name_str(&kv.key) {
                    Some(key) => NObjectProp::KeyValue {
                        key: key.clone(),
                        value: lower_expr(&kv.value, base),
                        quoted: !is_safe_object_key(&key),
                        computed: matches!(&kv.key, PropName::Computed(_)),
                        value_span: span_of(kv.value.span(), base),
                    },
                    None => NObjectProp::Other(span_of(prop_span(prop), base)),
                },
                Prop::Shorthand(id) => NObjectProp::KeyValue {
                    key: id.sym.to_string(),
                    value: NExpr::Identifier(id.sym.to_string()),
                    quoted: !is_safe_object_key(&id.sym),
                    computed: false,
                    value_span: span_of(id.span, base),
                },
                _ => NObjectProp::Other(span_of(prop_span(prop), base)),
            },
        })
        .collect()
}

/// Lower an swc `Expr` to the neutral [`LitValue`] — the same literal subset the oxc backend
/// captures; anything richer becomes [`LitValue::Other`] carrying its (rebased) span.
fn lower_value(expr: &Expr, base: u32) -> LitValue {
    match expr {
        Expr::Lit(Lit::Str(s)) => LitValue::String(wtf8_to_string(&s.value)),
        Expr::Lit(Lit::Num(n)) => LitValue::Number(n.value),
        Expr::Lit(Lit::Bool(b)) => LitValue::Boolean(b.value),
        Expr::Lit(Lit::Null(_)) => LitValue::Null,
        Expr::Tpl(t) if t.exprs.is_empty() && t.quasis.len() == 1 => {
            match t.quasis[0].cooked.as_ref() {
                Some(c) => LitValue::String(wtf8_to_string(c)),
                None => LitValue::Other(span_of(t.span, base)),
            }
        }
        Expr::Ident(id) => LitValue::Identifier(id.sym.to_string()),
        // `Foo.Bar` member access keeps the trailing property name (matches the oxc backend's
        // `StaticMemberExpression` read). A computed member (`Foo[x]`) is NOT a static name -> Other.
        Expr::Member(m) => match &m.prop {
            MemberProp::Ident(id) => LitValue::Identifier(id.sym.to_string()),
            _ => LitValue::Other(span_of(m.span, base)),
        },
        Expr::Array(arr) => {
            let mut out = Vec::with_capacity(arr.elems.len());
            for el in &arr.elems {
                match el {
                    // An elision (hole) or a spread element is not a plain value.
                    None => out.push(LitValue::Other(span_of(arr.span, base))),
                    Some(e) if e.spread.is_some() => out.push(LitValue::Other(span_of(arr.span, base))),
                    Some(e) => out.push(lower_value(&e.expr, base)),
                }
            }
            LitValue::Array(out)
        }
        Expr::Object(obj) => LitValue::Object(lower_object(obj, base)),
        Expr::Paren(p) => lower_value(&p.expr, base),
        _ => LitValue::Other(span_of(expr_span(expr), base)),
    }
}

/// The span of an arbitrary expression (for the [`LitValue::Other`] fallback). swc has no blanket
/// `GetSpan`-style trait reachable here without `Spanned`, so we read the span of the shapes we
/// actually reach; anything else degrades to an empty span (the value is `Other` regardless).
fn expr_span(expr: &Expr) -> Span {
    expr.span()
}

// ---------------------------------------------------------------------------
// Neutral EXPRESSION / STATEMENT / PARAM lowering (the full walk surface).
//
// Shape-for-shape mirror of `oxc.rs::lower_expr` / `lower_stmt`. swc's `Expr::Bin` already unifies the
// binary + logical operators (oxc splits `LogicalExpression` out), so both collapse to the same
// neutral `NExpr::Binary { op }` keyed by the operator's source spelling — engine-identical.
// ---------------------------------------------------------------------------

/// Lower an swc `Expr` to the neutral [`NExpr`], mirroring the `convert_expr` surface.
fn lower_expr(expr: &Expr, base: u32) -> NExpr {
    match expr {
        Expr::Lit(Lit::Str(s)) => NExpr::String(wtf8_to_string(&s.value)),
        Expr::Tpl(t) if t.exprs.is_empty() && t.quasis.len() == 1 => {
            match t.quasis[0].cooked.as_ref() {
                Some(c) => NExpr::String(wtf8_to_string(c)),
                None => NExpr::Other(span_of(t.span, base)),
            }
        }
        Expr::Lit(Lit::Num(n)) => NExpr::Number(n.value),
        Expr::Lit(Lit::Bool(b)) => NExpr::Boolean(b.value),
        Expr::Lit(Lit::Null(_)) => NExpr::Null,
        Expr::Ident(id) => NExpr::Identifier(id.sym.to_string()),
        Expr::Member(m) => match &m.prop {
            MemberProp::Ident(id) => NExpr::Member {
                object: Box::new(lower_expr(&m.obj, base)),
                property: id.sym.to_string(),
            },
            MemberProp::Computed(c) => NExpr::ComputedMember {
                object: Box::new(lower_expr(&m.obj, base)),
                index: Box::new(lower_expr(&c.expr, base)),
            },
            // `obj.#priv` — not part of any converted surface; carry its span.
            MemberProp::PrivateName(_) => NExpr::Other(span_of(m.span, base)),
        },
        Expr::Call(call) => match &call.callee {
            Callee::Expr(callee) => NExpr::Call {
                callee: Box::new(lower_expr(callee, base)),
                args: lower_args(&call.args, base),
            },
            // `super(...)` / `import(...)` — not a plain callee; carry the call's span.
            _ => NExpr::Other(span_of(call.span, base)),
        },
        Expr::New(new_expr) => NExpr::New {
            callee: Box::new(lower_expr(&new_expr.callee, base)),
            args: new_expr
                .args
                .as_ref()
                .map(|a| lower_args(a, base))
                .unwrap_or_default(),
        },
        Expr::Paren(p) => NExpr::Parenthesized(Box::new(lower_expr(&p.expr, base))),
        Expr::Cond(c) => NExpr::Conditional {
            test: Box::new(lower_expr(&c.test, base)),
            consequent: Box::new(lower_expr(&c.cons, base)),
            alternate: Box::new(lower_expr(&c.alt, base)),
        },
        // swc's `Expr::Bin` covers BOTH oxc's `BinaryExpression` and `LogicalExpression` (its
        // `BinaryOp` includes `&&`/`||`/`??`). The operator's source spelling unifies them.
        Expr::Bin(b) => NExpr::Binary {
            op: b.op.as_str().to_string(),
            left: Box::new(lower_expr(&b.left, base)),
            right: Box::new(lower_expr(&b.right, base)),
        },
        Expr::Unary(u) => NExpr::Unary {
            op: u.op.as_str().to_string(),
            argument: Box::new(lower_expr(&u.arg, base)),
        },
        Expr::Array(arr) => {
            let elems = arr
                .elems
                .iter()
                .map(|el| match el {
                    None => NArrayElement::Hole,
                    Some(e) if e.spread.is_some() => NArrayElement::Spread(lower_expr(&e.expr, base)),
                    Some(e) => NArrayElement::Expr(lower_expr(&e.expr, base)),
                })
                .collect();
            NExpr::Array(elems)
        }
        Expr::Object(obj) => NExpr::Object(lower_object_props(obj, base)),
        Expr::Arrow(arrow) => NExpr::Arrow {
            params: lower_arrow_params(&arrow.params),
            body: Box::new(lower_arrow_body(arrow, base)),
        },
        Expr::Fn(func) => NExpr::Function {
            params: lower_fn_params(&func.function),
            body: func
                .function
                .body
                .as_ref()
                .map(|b| lower_stmts(&b.stmts, base))
                .unwrap_or_default(),
        },
        _ => NExpr::Other(span_of(expr_span(expr), base)),
    }
}

/// The span of an object property (for the [`NObjectProp::Other`] fallback).
fn prop_span(prop: &Prop) -> Span {
    prop.span()
}

/// Whether an object key is a valid bare JS identifier (so it can be emitted unquoted). Faithful to
/// the `is_safe_object_key` helper the `convert_expr` walks use (and the oxc backend's mirror).
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
fn lower_args(args: &[ExprOrSpread], base: u32) -> Vec<NArg> {
    args.iter()
        .map(|a| {
            let span = span_of(a.expr.span(), base);
            if a.spread.is_some() {
                NArg::Spread(lower_expr(&a.expr, base), span)
            } else {
                NArg::Expr(lower_expr(&a.expr, base), span)
            }
        })
        .collect()
}

/// Lower a function's `Vec<Param>` to the simple-binding neutral [`NParam`]s.
fn lower_fn_params(func: &Function) -> Vec<NParam> {
    func.params
        .iter()
        .map(|p| {
            let (name, is_rest) = pat_name_and_rest(&p.pat);
            NParam { name, is_rest }
        })
        .collect()
}

/// Lower an arrow's `Vec<Pat>` params (arrows carry bare `Pat`, not the `Param` wrapper) to the
/// simple-binding neutral [`NParam`]s.
fn lower_arrow_params(params: &[Pat]) -> Vec<NParam> {
    params
        .iter()
        .map(|p| {
            let (name, is_rest) = pat_name_and_rest(p);
            NParam { name, is_rest }
        })
        .collect()
}

/// Lower an arrow body to a neutral [`NArrowBody`]: an expression body (`x => expr`) or a block body
/// (`x => { … }`). swc models the two directly via `BlockStmtOrExpr`, matching oxc's `expression`
/// flag + single-leading-expression-statement read.
fn lower_arrow_body(arrow: &ArrowExpr, base: u32) -> NArrowBody {
    match &*arrow.body {
        BlockStmtOrExpr::Expr(e) => NArrowBody::Expr(Box::new(lower_expr(e, base))),
        BlockStmtOrExpr::BlockStmt(block) => NArrowBody::Block(lower_stmts(&block.stmts, base)),
    }
}

/// Lower a list of statements to neutral [`NStmt`]s, in source order.
fn lower_stmts(stmts: &[Stmt], base: u32) -> Vec<NStmt> {
    stmts.iter().map(|s| lower_stmt(s, base)).collect()
}

/// Lower one statement to the neutral [`NStmt`], mirroring the `convert_statement` subset. A
/// `var`/`let`/`const` is `Stmt::Decl(Decl::Var)` in swc (vs oxc's `Statement::VariableDeclaration`).
fn lower_stmt(stmt: &Stmt, base: u32) -> NStmt {
    match stmt {
        Stmt::Decl(Decl::Var(decl)) => {
            let is_const = matches!(decl.kind, VarDeclKind::Const);
            let decls = decl
                .decls
                .iter()
                .map(|d| NVarDeclarator {
                    name: pat_binding_name(&d.name),
                    init: d.init.as_deref().map(|e| lower_expr(e, base)),
                })
                .collect();
            NStmt::VarDecl { is_const, decls }
        }
        Stmt::Expr(es) => NStmt::Expr(lower_expr(&es.expr, base)),
        Stmt::Return(ret) => NStmt::Return(ret.arg.as_deref().map(|e| lower_expr(e, base))),
        Stmt::If(if_stmt) => NStmt::If {
            test: lower_expr(&if_stmt.test, base),
            consequent: lower_branch(&if_stmt.cons, base),
            alternate: if_stmt
                .alt
                .as_deref()
                .map(|s| lower_branch(s, base))
                .unwrap_or_default(),
        },
        Stmt::Block(block) => NStmt::Block(lower_stmts(&block.stmts, base)),
        _ => NStmt::Other(span_of(stmt.span(), base)),
    }
}

/// Lower an `if`/`else` branch — a `{ … }` block's statements, or a one-element list for a bare
/// branch statement (mirrors the oxc backend's `lower_branch`).
fn lower_branch(stmt: &Stmt, base: u32) -> Vec<NStmt> {
    match stmt {
        Stmt::Block(block) => lower_stmts(&block.stmts, base),
        other => vec![lower_stmt(other, base)],
    }
}

// ---------------------------------------------------------------------------
// ɵɵngDeclare* collection (neutral surface for the linker).
// ---------------------------------------------------------------------------

/// Collect every `ɵɵngDeclare*({…})` call that sits as a `static X = …` member of a class.
fn collect_ng_declares_in_class(
    class: &swc_ecma_ast::Class,
    base: u32,
    out: &mut Vec<NgDeclareCall>,
) {
    for member in &class.body {
        if let ClassMember::ClassProp(prop) = member {
            if let Some(init) = &prop.value {
                push_if_ng_declare(init, base, out);
            }
        }
    }
}

/// If `expr` is a `ɵɵngDeclare*({…})` call with an object-literal argument, push its neutral form.
fn push_if_ng_declare(expr: &Expr, base: u32, out: &mut Vec<NgDeclareCall>) {
    match expr {
        Expr::Call(call) => {
            let Some(kind) = declare_callee_kind(&call.callee) else {
                return;
            };
            for arg in &call.args {
                if arg.spread.is_none() {
                    if let Expr::Object(obj) = &*arg.expr {
                        out.push(NgDeclareCall {
                            kind,
                            object: lower_object(obj, base),
                            call_span: span_of(call.span(), base),
                        });
                        return;
                    }
                }
            }
        }
        // Recurse through the wrappers a declaration call inhabits — the RHS of an assignment
        // (`X.ɵprov = <call>`), a parenthesized group, and a comma sequence — mirroring the linker's
        // `collect_in_expression` (and the oxc backend) so the assignment / member form is captured
        // identically across engines.
        Expr::Assign(assign) => push_if_ng_declare(&assign.right, base, out),
        Expr::Paren(p) => push_if_ng_declare(&p.expr, base, out),
        Expr::Seq(seq) => {
            for part in &seq.exprs {
                push_if_ng_declare(part, base, out);
            }
        }
        _ => {}
    }
}

/// The `ɵɵngDeclare*` callee suffix (`Component`, `Factory`, …) for the bare-identifier
/// (`ɵɵngDeclareX(...)`) and namespaced (`i0.ɵɵngDeclareX(...)`) call forms.
fn declare_callee_kind(callee: &swc_ecma_ast::Callee) -> Option<String> {
    let swc_ecma_ast::Callee::Expr(expr) = callee else {
        return None;
    };
    let name = match &**expr {
        Expr::Ident(id) => id.sym.as_str(),
        Expr::Member(m) => match &m.prop {
            MemberProp::Ident(id) => id.sym.as_str(),
            _ => return None,
        },
        _ => return None,
    };
    name.strip_prefix("\u{0275}\u{0275}ngDeclare")
        .filter(|suffix| !suffix.is_empty())
        .map(str::to_string)
}
