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
use swc_common::{BytePos, FileName, Globals, SourceMap, Span, GLOBALS};
use swc_ecma_ast::{
    ClassMember, Decl, DefaultDecl, Decorator, EsVersion, Expr, Lit, MemberProp, ModuleDecl,
    ModuleItem, Program, Prop, PropName, PropOrSpread, Stmt,
};
use swc_ecma_parser::{parse_file_as_program, Syntax, TsSyntax};

use super::{
    ClassWithDecorators, DecoratorInfo, LitValue, MemberInfo, NgDeclareCall, ObjLit, ParseBackend,
    ParseOutput, SourceKind, TreatySpan,
};

/// The swc parse backend. Zero-sized; a fresh `GLOBALS` scope + `SourceMap` is created per
/// [`Self::parse_module`] call so each parse is independent (mirrors the oxc backend's per-call
/// `Allocator::default()`), with NO thread-local state shared across parses.
#[derive(Debug, Default, Clone, Copy)]
pub struct SwcParseBackend;

/// The borrowed parsed-module handle the [`ParseBackend::parse_module`] callback receives. Wraps the
/// owned swc `Program` and the pre-lowered engine-neutral [`ParseOutput`].
///
/// Unlike [`super::oxc::OxcModule`] there is no oxc `Program` to expose — the facade walks that still
/// need the live AST (the oxc-typed `compile_program_with_source` / `ClassMeta` path) are not yet
/// neutralized (SWC-BACKEND-PLAN.md §3.2 phase 3), so under this backend they are unreachable. The
/// neutral [`Self::summary`] is the contract the parity gate checks.
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

    match program {
        Program::Module(module) => {
            for item in &module.body {
                lower_top_level_item(item, base, &mut classes, &mut ng_declare_calls);
            }
        }
        Program::Script(script) => {
            for stmt in &script.body {
                lower_top_level_stmt(stmt, base, &mut classes, &mut ng_declare_calls);
            }
        }
    }

    ParseOutput {
        classes,
        ng_declare_calls,
        errors: Vec::new(),
    }
}

/// Handle a module-level item: a statement, or an `export`/`export default` wrapping a class.
fn lower_top_level_item(
    item: &ModuleItem,
    base: u32,
    classes: &mut Vec<ClassWithDecorators>,
    ng_declares: &mut Vec<NgDeclareCall>,
) {
    match item {
        ModuleItem::Stmt(stmt) => lower_top_level_stmt(stmt, base, classes, ng_declares),
        ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => {
            lower_top_level_decl(&export.decl, base, classes, ng_declares);
        }
        ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultDecl(export)) => {
            if let DefaultDecl::Class(class_expr) = &export.decl {
                push_class(
                    class_expr.ident.as_ref().map(|id| id.sym.to_string()),
                    &class_expr.class,
                    base,
                    classes,
                    ng_declares,
                );
            }
        }
        ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultExpr(export)) => {
            // `export default ɵɵngDeclare*({...})` as a free expression.
            push_if_ng_declare(&export.expr, base, ng_declares);
        }
        _ => {}
    }
}

/// Handle a top-level statement (class decl / expr stmt / var decl carrying a `ɵɵngDeclare*`).
fn lower_top_level_stmt(
    stmt: &Stmt,
    base: u32,
    classes: &mut Vec<ClassWithDecorators>,
    ng_declares: &mut Vec<NgDeclareCall>,
) {
    match stmt {
        Stmt::Decl(decl) => lower_top_level_decl(decl, base, classes, ng_declares),
        Stmt::Expr(es) => push_if_ng_declare(&es.expr, base, ng_declares),
        _ => {}
    }
}

/// Handle a declaration: a class (decorated -> a neutral class; always scanned for static
/// `ɵɵngDeclare*` members) or a `var`/`let`/`const` whose initializer is a `ɵɵngDeclare*` call.
fn lower_top_level_decl(
    decl: &Decl,
    base: u32,
    classes: &mut Vec<ClassWithDecorators>,
    ng_declares: &mut Vec<NgDeclareCall>,
) {
    match decl {
        Decl::Class(class_decl) => {
            push_class(
                Some(class_decl.ident.sym.to_string()),
                &class_decl.class,
                base,
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
    class: &swc_ecma_ast::Class,
    base: u32,
    classes: &mut Vec<ClassWithDecorators>,
    ng_declares: &mut Vec<NgDeclareCall>,
) {
    if !class.decorators.is_empty() {
        classes.push(lower_class(name, class, base));
    }
    collect_ng_declares_in_class(class, base, ng_declares);
}

/// Lower a class to the neutral [`ClassWithDecorators`] (name + decorators + members, source order).
fn lower_class(name: Option<String>, class: &swc_ecma_ast::Class, base: u32) -> ClassWithDecorators {
    let decorators = class
        .decorators
        .iter()
        .map(|d| lower_decorator(d, base))
        .collect();
    let members = class
        .body
        .iter()
        .filter_map(|m| lower_member(m, base))
        .collect::<Vec<_>>();
    ClassWithDecorators {
        name,
        decorators,
        members,
    }
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
    let (name, decorators): (Option<String>, &[Decorator]) = match member {
        ClassMember::ClassProp(p) => (prop_name_str(&p.key), &p.decorators),
        ClassMember::Method(m) => (prop_name_str(&m.key), &m.function.decorators),
        ClassMember::Constructor(c) => (prop_name_str(&c.key), &[]),
        ClassMember::AutoAccessor(a) => (key_str(&a.key), &a.decorators),
        // A stray `;` (`constructor() {};`) is parsed by swc as an `Empty` member but DISCARDED by oxc
        // — drop it so the neutral member list is element-for-element identical.
        ClassMember::Empty(_) => return None,
        // StaticBlock / TsIndexSignature / PrivateMethod / PrivateProp: oxc still emits a (nameless)
        // class element for these, so emit an un-named member to keep the counts aligned.
        _ => return Some(MemberInfo::default()),
    };
    Some(MemberInfo {
        name,
        decorators: decorators.iter().map(|d| lower_decorator(d, base)).collect(),
    })
}

/// Lower a decorator to the neutral [`DecoratorInfo`] (callee name + first object-literal argument).
fn lower_decorator(dec: &Decorator, base: u32) -> DecoratorInfo {
    let name = decorator_name(dec).unwrap_or_default().to_string();
    let object = decorator_object(dec).map(|obj| lower_object(obj, base));
    DecoratorInfo { name, object }
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
    }
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
    use swc_common::Spanned;
    expr.span()
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
    let Expr::Call(call) = expr else {
        return;
    };
    let Some(kind) = declare_callee_kind(&call.callee) else {
        return;
    };
    for arg in &call.args {
        if arg.spread.is_none() {
            if let Expr::Object(obj) = &*arg.expr {
                out.push(NgDeclareCall {
                    kind,
                    object: lower_object(obj, base),
                });
                return;
            }
        }
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
