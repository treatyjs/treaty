//! The Angular **partial-declaration emitter** (Rust) — the inverse of [`crate::linker`].
//!
//! A published Angular library is, by default, *partial*-compiled: each class emits
//! `ɵɵngDeclare*({...})` calls (the partial format) rather than the full AOT `ɵɵdefine*` calls. The
//! application build's Angular **Linker** later rewrites every `ɵɵngDeclare*` back into the
//! corresponding `ɵɵdefine*`. [`crate::linker`] is that linker; THIS module is its inverse — it
//! turns an AOT `ɵɵdefine*` module into the partial `ɵɵngDeclare*` form a library publishes with
//! `compilationMode: "partial"`.
//!
//! # Scope (and why it is mode-gated)
//!
//! The DI + pipe family — `ɵfac` (factory), `ɵprov` (`ɵɵdefineInjectable`), `ɵpipe`
//! (`ɵɵdefinePipe`), `ɵmod` (`ɵɵdefineNgModule`), `ɵinj` (`ɵɵdefineInjector`) — have pure-DATA
//! definition objects, so inverting them to `ɵɵngDeclare*` is a faithful syntactic transform that
//! **round-trips exactly** back through [`crate::linker::link_partial`] to the original AOT
//! `ɵɵdefine*`. [`emit_partial`] performs that inversion as a surgical span rewrite — every byte
//! outside a rewritten definition (the class body, the `X.ɵfac = …` assignment scaffold, imports)
//! is preserved verbatim — exactly mirroring how the linker rewrites only the call spans.
//!
//! Component / directive (`ɵcmp` / `ɵdir`) partial declarations are intentionally **out of scope**:
//! `ɵɵngDeclareComponent` carries the component `template` as an HTML STRING (and the directive host
//! bindings / queries in declarative form), so producing it from an AOT `ɵɵdefineComponent` would
//! require decompiling the lowered instruction stream back to HTML — a template DECOMPILER, a major
//! feature in its own right. Those definitions are left as their AOT `ɵɵdefine*` form and reported,
//! so a partial build of a component library still produces a valid, loadable module (the AOT
//! component defs are themselves directly usable; they simply are not in partial format).
//!
//! Because this is a SEPARATE entry the caller invokes only for `compilationMode: "partial"`, the
//! default AOT compile path is byte-for-byte untouched.

// PARSE and WALK are now BOTH driven through the engine-neutral `crate::parse` surface: this module
// names ZERO live oxc nodes. `emit_partial` parses through `ParsingBackend`, then walks the
// pre-lowered neutral TOP-LEVEL surface (`ParseOutput::top_level`) — the `X.ɵfac =`/`X.ɵprov = …`
// definition-scaffold assignments ([`NAssignment`]), the `ɵɵsetNgModuleScope` side-effect call
// statements, and the factory-body decompile (`NExpr`/`NArg` trees). Byte-exact source recovery (the
// verbatim `providedIn`/`providers`/inject-token/RHS slices) goes through `ParseBackend::span_text`
// on the neutral [`TreatySpan`]s the surface carries, so the rewritten output stays byte-identical.
use crate::parse::{
    NArg, NArrowBody, NExpr, NObjectProp, NStmt, NTopStmt, ParseBackend, ParsingBackend, SourceKind,
    TreatySpan,
};

/// The Angular version a partial declaration is stamped with. A published library stamps its own
/// compiler version; this is the in-repo placeholder value the linker (and ngcc) treat as "newest
/// behaviour" — `read_is_standalone` / `is_placeholder_version` in [`crate::linker`] accept any
/// `0.0.0-…` prerelease, so the round-trip is version-stable.
const PARTIAL_VERSION: &str = "0.0.0-PLACEHOLDER";
/// The `minVersion` a partial declaration records (the earliest linker that understands the shape).
const MIN_VERSION: &str = "12.0.0";

/// The result of a partial emit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialEmit {
    /// The module source with every supported AOT `ɵɵdefine*` definition rewritten to its
    /// `ɵɵngDeclare*` partial form. Byte-identical to the input outside the rewritten spans.
    pub code: String,
    /// Diagnostics: each component/directive definition left as AOT (partial declaration of those
    /// kinds is not yet emitted — see the module note), one message per skipped definition.
    pub notes: Vec<String>,
}

/// Emit the partial-declaration form of an AOT-compiled Ivy module.
///
/// `aot_code` is the output of the AOT source front-end (`X.ɵfac = function …`, `X.ɵprov =
/// i0.ɵɵdefineInjectable({…})`, `X.ɵpipe = i0.ɵɵdefinePipe({…})`, …). Every supported DI/pipe-family
/// definition is rewritten to its `ɵɵngDeclare*` partial form; component/directive defs are left as
/// AOT and reported in `notes`. The transform is a surgical span rewrite, so the surrounding module
/// is preserved verbatim. On a parse failure the input is returned unchanged with a note.
pub fn emit_partial(aot_code: &str) -> PartialEmit {
    let backend = ParsingBackend::default();
    backend.parse_module(aot_code, SourceKind::TypeScriptEsModule, |module| {
        let summary = module.summary();
        if !summary.errors.is_empty() {
            return PartialEmit {
                code: aot_code.to_string(),
                notes: vec![format!(
                    "partial emit: input did not parse ({} error(s)); left as AOT",
                    summary.errors.len()
                )],
            };
        }

        let mut rewrites: Vec<Rewrite> = Vec::new();
        let mut notes: Vec<String> = Vec::new();
        collect_rewrites(&backend, &summary.top_level, aot_code, &mut rewrites, &mut notes);

        // Apply rewrites back-to-front so earlier byte offsets stay valid.
        rewrites.sort_by_key(|r| std::cmp::Reverse(r.start));
        let mut code = aot_code.to_string();
        for r in &rewrites {
            code.replace_range(r.start as usize..r.end as usize, &r.text);
        }

        PartialEmit { code, notes }
    })
}

/// Recover the trimmed source text a [`TreatySpan`] covers, through the parse backend's `span_text`
/// (the only sanctioned way to map a neutral span back to bytes — see [`ParseBackend::span_text`]).
fn span_src<'s>(backend: &ParsingBackend, source: &'s str, span: TreatySpan) -> &'s str {
    backend.span_text(source, span).trim()
}

/// One span replacement: the byte range to overwrite and the replacement text.
struct Rewrite {
    start: u32,
    end: u32,
    text: String,
}

/// Walk every neutral top-level statement collecting the `ɵfac`/`ɵprov`/`ɵpipe`/`ɵmod`/`ɵinj`
/// definition assignments to rewrite, and noting each `ɵcmp`/`ɵdir` left as AOT.
fn collect_rewrites(
    backend: &ParsingBackend,
    top_level: &[NTopStmt],
    source: &str,
    rewrites: &mut Vec<Rewrite>,
    notes: &mut Vec<String>,
) {
    for stmt in top_level {
        let NTopStmt::Assignment(assign) = stmt else {
            continue;
        };
        // LHS must be `<Ident>.<member>`.
        let (Some(type_name), Some(member)) =
            (assign.target_object.as_deref(), assign.target_member.as_deref())
        else {
            continue;
        };
        // The RHS span is what every supported kind overwrites with its `ɵɵngDeclare*` text.
        let rhs_span = assign.value_span;

        match member {
            // `X.ɵfac = function X_Factory(t){ return new (t||X)(<inject calls>); }` →
            // ngDeclareFactory. The `deps` array is DECOMPILED from the AOT factory body's
            // `ɵɵinject`/`ɵɵdirectiveInject`/`ɵɵinjectAttribute`/`ɵɵinvalidFactoryDep` calls (see
            // `factory_deps_from_body`) so a constructor WITH DI publishes its real `deps:[{token,…}]`.
            "\u{0275}fac" => {
                // Determine this class's factory TARGET from a sibling definition member in the
                // module (ɵcmp→Component, ɵdir→Directive, ɵpipe→Pipe, ɵprov→Injectable, ɵmod→
                // NgModule). Default to Injectable when none is found (a bare `@Injectable`).
                let target = factory_target_for(top_level, type_name);
                let deps = factory_deps_from_body(backend, &assign.value, source);
                if let Some(text) = ng_declare_factory(type_name, target, &deps) {
                    rewrites.push(Rewrite { start: rhs_span.start, end: rhs_span.end, text });
                }
            }
            // `X.ɵprov = i0.ɵɵdefineInjectable({...})` → ngDeclareInjectable.
            "\u{0275}prov" => {
                if let Some(obj) = define_call_object(&assign.value, "\u{0275}\u{0275}defineInjectable") {
                    let text = ng_declare_injectable(backend, type_name, obj, source);
                    rewrites.push(Rewrite { start: rhs_span.start, end: rhs_span.end, text });
                }
            }
            // `X.ɵpipe = i0.ɵɵdefinePipe({...})` → ngDeclarePipe.
            "\u{0275}pipe" => {
                if let Some(obj) = define_call_object(&assign.value, "\u{0275}\u{0275}definePipe") {
                    let text = ng_declare_pipe(backend, type_name, obj, source);
                    rewrites.push(Rewrite { start: rhs_span.start, end: rhs_span.end, text });
                }
            }
            // `X.ɵinj = i0.ɵɵdefineInjector({...})` → ngDeclareInjector.
            "\u{0275}inj" => {
                if let Some(obj) = define_call_object(&assign.value, "\u{0275}\u{0275}defineInjector") {
                    let text = ng_declare_injector(backend, type_name, obj, source);
                    rewrites.push(Rewrite { start: rhs_span.start, end: rhs_span.end, text });
                }
            }
            // `X.ɵmod = i0.ɵɵdefineNgModule({...})` → ngDeclareNgModule.
            "\u{0275}mod" => {
                if let Some(obj) = define_call_object(&assign.value, "\u{0275}\u{0275}defineNgModule") {
                    let text = ng_declare_ng_module(backend, type_name, obj, source, top_level);
                    rewrites.push(Rewrite { start: rhs_span.start, end: rhs_span.end, text });
                }
            }
            // Component / directive. When the source front-end already emitted the partial
            // declaration (`compilationMode: "partial"` in `source_compile`), the RHS is a
            // `ɵɵngDeclareComponent`/`ɵɵngDeclareDirective` call — leave it untouched and add no note.
            // Otherwise it is an AOT `ɵɵdefine*` whose template is a lowered instruction stream;
            // inverting that needs a template decompiler (out of scope), so report it.
            "\u{0275}cmp" => {
                if !is_call_named(&assign.value, "\u{0275}\u{0275}ngDeclareComponent") {
                    notes.push(format!(
                        "{type_name}: ɵcmp (component) left as AOT — ɵɵngDeclareComponent requires template decompilation"
                    ));
                }
            }
            "\u{0275}dir" => {
                if !is_call_named(&assign.value, "\u{0275}\u{0275}ngDeclareDirective") {
                    notes.push(format!(
                        "{type_name}: ɵdir (directive) left as AOT — ɵɵngDeclareDirective partial emit not implemented"
                    ));
                }
            }
            _ => {}
        }
    }
}

/// The object-literal argument (as its lossless [`NObjectProp`] list) of `<ns?>.<callee>({...})`, or
/// `None` if `expr` is not that call. The callee is matched bare (`ɵɵdefineX`) or namespaced
/// (`i0.ɵɵdefineX`).
fn define_call_object<'a>(expr: &'a NExpr, callee: &str) -> Option<&'a [NObjectProp]> {
    let NExpr::Call { callee: call_callee, args } = expr else {
        return None;
    };
    if call_callee_name(call_callee) != Some(callee) {
        return None;
    }
    args.iter().find_map(|a| match a.expr() {
        NExpr::Object(props) => Some(props.as_slice()),
        _ => None,
    })
}

/// The (bare or `i0.`-namespaced) callee name of a callee expression — a bare `Identifier`
/// (`ɵɵdefineX`) or a `Member` (`i0.ɵɵdefineX`), returning the trailing name.
fn call_callee_name(callee: &NExpr) -> Option<&str> {
    match callee {
        NExpr::Identifier(name) => Some(name.as_str()),
        NExpr::Member { property, .. } => Some(property.as_str()),
        _ => None,
    }
}

/// Whether `expr` is a call `<ns?>.<callee>(…)` whose callee is `callee` (bare or `i0.<callee>`).
/// Used to detect a component/directive RHS that the source front-end already emitted in partial
/// (`ɵɵngDeclareComponent`/`ɵɵngDeclareDirective`) form.
fn is_call_named(expr: &NExpr, callee: &str) -> bool {
    matches!(expr, NExpr::Call { callee: c, .. } if call_callee_name(c) == Some(callee))
}

/// The factory target for `type_name`, inferred from which definition member the class also carries.
fn factory_target_for(top_level: &[NTopStmt], type_name: &str) -> &'static str {
    let mut found: Option<&'static str> = None;
    for stmt in top_level {
        let NTopStmt::Assignment(assign) = stmt else { continue };
        let (Some(name), Some(member)) =
            (assign.target_object.as_deref(), assign.target_member.as_deref())
        else {
            continue;
        };
        if name != type_name {
            continue;
        }
        let target = match member {
            "\u{0275}cmp" => Some("Component"),
            "\u{0275}dir" => Some("Directive"),
            "\u{0275}pipe" => Some("Pipe"),
            "\u{0275}mod" => Some("NgModule"),
            "\u{0275}prov" => Some("Injectable"),
            _ => None,
        };
        if let Some(t) = target {
            found = Some(t);
            // Component/Directive/Pipe/NgModule are more specific than Injectable; prefer them.
            if t != "Injectable" {
                break;
            }
        }
    }
    found.unwrap_or("Injectable")
}

/// Read a property's value verbatim from the source (so opaque expressions — `providedIn: SomeMod`,
/// `useFactory: () => …` — round-trip exactly). Returns the trimmed source slice, recovered through
/// the parse backend's `span_text` on the property's neutral `value_span`.
fn prop_source<'a>(
    backend: &ParsingBackend,
    props: &[NObjectProp],
    name: &str,
    source: &'a str,
) -> Option<&'a str> {
    for p in props {
        if let NObjectProp::KeyValue { key, value_span, .. } = p {
            if key == name {
                return Some(span_src(backend, source, *value_span));
            }
        }
    }
    None
}

/// The shared `version` / `ngImport` suffix every `ɵɵngDeclare*` object carries, with a per-kind
/// `minVersion` (the earliest linker that understands that declaration shape — `"12.0.0"` for the
/// factory/injectable/injector/ngmodule family, `"14.0.0"` for pipe/directive/component, matching
/// Angular's own partial emitters).
fn declare_prelude_min(min_version: &str) -> String {
    format!("minVersion: \"{min_version}\", version: \"{PARTIAL_VERSION}\", ngImport: i0")
}

/// The default `minVersion` prelude (`"12.0.0"` — the DI/ngmodule family).
fn declare_prelude() -> String {
    declare_prelude_min(MIN_VERSION)
}

/// The decompiled `deps` tri-state of an AOT factory body — the inverse of the factory codegen's
/// `FactoryDeps`, recovered from the emitted factory function (see [`factory_deps_from_body`]).
enum DecompiledDeps {
    /// `deps: [ <entry source>, … ]` — a constructor factory's resolved dependency list (may be the
    /// empty array for a parameterless constructor). Each entry is the rendered `{ token, …flags }`
    /// object-literal SOURCE.
    Deps(Vec<String>),
    /// `deps: null` — no own constructor; the factory inherits the base-class factory
    /// (`ɵɵgetInheritedFactory`). Round-trips to `FactoryDeps::Inherit`.
    Inherit,
    /// `deps: "invalid"` — at least one dep was unresolvable (`ɵɵinvalidFactory`/`ɵɵinvalidFactoryDep`).
    Invalid,
}

/// `ɵɵngDeclareFactory({...})` for `type_name` with the given factory `target` and the `deps`
/// recovered from the AOT factory body. The `deps` field round-trips through the linker's
/// `get_dependencies`:
///   * [`DecompiledDeps::Deps`] → `deps: [ … ]` (the resolved list; empty `[]` is the dominant
///     no-arg shape that re-emits `function X_Factory(t){ return new (t||X)(); }`);
///   * [`DecompiledDeps::Inherit`] → `deps: null` (inherit the base-class factory);
///   * [`DecompiledDeps::Invalid`] → `deps: "invalid"` (re-emit `ɵɵinvalidFactory()`).
fn ng_declare_factory(type_name: &str, target: &str, deps: &DecompiledDeps) -> Option<String> {
    let deps_field = match deps {
        DecompiledDeps::Deps(entries) => format!("[{}]", entries.join(", ")),
        DecompiledDeps::Inherit => "null".to_string(),
        DecompiledDeps::Invalid => "\"invalid\"".to_string(),
    };
    Some(format!(
        "i0.\u{0275}\u{0275}ngDeclareFactory({{ {}, type: {type_name}, deps: {deps_field}, target: i0.\u{0275}\u{0275}FactoryTarget.{target} }})",
        declare_prelude()
    ))
}

/// Decompile an AOT factory function's body back into its [`DecompiledDeps`] — the inverse of the
/// factory codegen (`treaty_ivy_core::factory`). The recognised shapes:
///
///   * `function X_Factory(t){ return new (t || X)(<inject calls>); }` — a constructor factory:
///     each constructor argument is an inject call we map back to a `{ token, …flags }` entry.
///     A zero-arg `new (t||X)()` yields the empty `deps: []`.
///   * a body that calls `ɵɵgetInheritedFactory(X)` (the base-factory IIFE/memoized form) — no own
///     constructor → [`DecompiledDeps::Inherit`] (`deps: null`).
///   * a body whose constructor expression is `ɵɵinvalidFactory()`, or any argument is
///     `ɵɵinvalidFactoryDep(i)` — [`DecompiledDeps::Invalid`] (`deps: "invalid"`).
///
/// Any other (unrecognised) shape conservatively yields the empty `deps: []` — the prior behaviour,
/// so nothing regresses; only the recognised constructor-DI shape now publishes real deps.
fn factory_deps_from_body(backend: &ParsingBackend, rhs: &NExpr, source: &str) -> DecompiledDeps {
    // The factory RHS is `function X_Factory(t){ … }`, possibly wrapped in a base-factory IIFE
    // `(() => { let ɵX_BaseFactory; return function X_Factory(t){ … }; })()`. Unwrap to the inner
    // function body and scan it.
    let Some(body_stmts) = factory_function_body(rhs) else {
        return DecompiledDeps::Deps(Vec::new());
    };

    // An inherited-factory body memoizes via `ɵɵgetInheritedFactory` — the whole module body (and
    // the wrapper IIFE) reference it. If any statement's expression tree contains that call, this
    // class inherits. (The source-slice contains-probe is replaced by a structural callee search over
    // the neutral `NExpr` tree, which is exact for these unique `ɵɵ`-prefixed runtime symbols.)
    if statements_reference_callee(body_stmts, "\u{0275}\u{0275}getInheritedFactory") {
        return DecompiledDeps::Inherit;
    }

    // Find the constructor `new (t || X)(<args>)` expression anywhere in the body (it is the
    // `return new …` or the conditional `r = new …`). The args are the inject calls.
    let Some(args) = find_ctor_call_args(body_stmts) else {
        // No `new` constructor expression: either an invalid factory (`ɵɵinvalidFactory()`) or an
        // unrecognised shape. Treat an explicit `ɵɵinvalidFactory` as Invalid; else empty deps.
        if statements_reference_callee(body_stmts, "\u{0275}\u{0275}invalidFactory") {
            return DecompiledDeps::Invalid;
        }
        return DecompiledDeps::Deps(Vec::new());
    };

    let mut entries: Vec<String> = Vec::with_capacity(args.len());
    for arg in args {
        match dep_entry_from_inject(backend, arg, source) {
            Some(entry) => entries.push(entry),
            // An `ɵɵinvalidFactoryDep(i)` (or an arg we cannot map) makes the whole factory invalid —
            // matching the codegen, where an unresolvable token poisons the dep list.
            None => return DecompiledDeps::Invalid,
        }
    }
    DecompiledDeps::Deps(entries)
}

/// Unwrap a neutral `NExpr::Parenthesized` chain to the innermost expression (the structural
/// equivalent of oxc's `get_inner_expression`, which also peels parens / TS casts; the neutral tree
/// has no TS-cast node, so only parens are peeled).
fn inner_expr(expr: &NExpr) -> &NExpr {
    match expr {
        NExpr::Parenthesized(inner) => inner_expr(inner),
        other => other,
    }
}

/// The statement body of an AOT factory function, unwrapping the base-factory IIFE wrapper
/// `(() => { … return function …(){ … }; })()` to the INNER `function …_Factory(t){ … }` body.
fn factory_function_body(rhs: &NExpr) -> Option<&[NStmt]> {
    match inner_expr(rhs) {
        NExpr::Function { body, .. } => Some(body.as_slice()),
        // `(() => { let ɵX_BaseFactory; return function X_Factory(t){…}; })()` — the wrapper IIFE.
        NExpr::Call { callee, .. } => {
            let arrow_body = match inner_expr(callee) {
                NExpr::Arrow { body, .. } => match body.as_ref() {
                    NArrowBody::Block(stmts) => stmts.as_slice(),
                    // An expression-bodied arrow `() => expr` is never the factory-wrapper shape.
                    NArrowBody::Expr(_) => return None,
                },
                NExpr::Function { body, .. } => body.as_slice(),
                _ => return None,
            };
            for stmt in arrow_body {
                if let NStmt::Return(Some(arg)) = stmt {
                    if let NExpr::Function { body, .. } = inner_expr(arg) {
                        return Some(body.as_slice());
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// Whether any statement's expression tree contains a call to `callee` — a structural search over the
/// neutral `NExpr` tree (the small factory body). Exact for the unique `ɵɵ`-prefixed runtime symbols
/// it probes (`ɵɵgetInheritedFactory`, `ɵɵinvalidFactory`).
fn statements_reference_callee(stmts: &[NStmt], callee: &str) -> bool {
    stmts.iter().any(|s| stmt_references_callee(s, callee))
}

/// Whether one statement's expression tree contains a call to `callee`.
fn stmt_references_callee(stmt: &NStmt, callee: &str) -> bool {
    match stmt {
        NStmt::Expr(e) | NStmt::Return(Some(e)) => expr_references_callee(e, callee),
        NStmt::Return(None) => false,
        NStmt::VarDecl { decls, .. } => decls
            .iter()
            .any(|d| d.init.as_ref().is_some_and(|e| expr_references_callee(e, callee))),
        NStmt::If { test, consequent, alternate } => {
            expr_references_callee(test, callee)
                || statements_reference_callee(consequent, callee)
                || statements_reference_callee(alternate, callee)
        }
        NStmt::Block(body) => statements_reference_callee(body, callee),
        NStmt::Other(_) => false,
    }
}

/// Whether an expression tree contains a `Call`/`New` whose callee resolves to `callee` (bare or
/// `i0.`-namespaced), recursively.
fn expr_references_callee(expr: &NExpr, callee: &str) -> bool {
    match expr {
        NExpr::Call { callee: c, args } | NExpr::New { callee: c, args } => {
            if call_callee_name(c) == Some(callee) {
                return true;
            }
            expr_references_callee(c, callee) || args.iter().any(|a| expr_references_callee(a.expr(), callee))
        }
        NExpr::Member { object, .. } => expr_references_callee(object, callee),
        NExpr::ComputedMember { object, index } => {
            expr_references_callee(object, callee) || expr_references_callee(index, callee)
        }
        NExpr::Parenthesized(inner) => expr_references_callee(inner, callee),
        NExpr::Conditional { test, consequent, alternate } => {
            expr_references_callee(test, callee)
                || expr_references_callee(consequent, callee)
                || expr_references_callee(alternate, callee)
        }
        NExpr::Binary { left, right, .. } => {
            expr_references_callee(left, callee) || expr_references_callee(right, callee)
        }
        NExpr::Unary { argument, .. } => expr_references_callee(argument, callee),
        NExpr::Array(elems) => elems.iter().any(|el| match el {
            crate::parse::NArrayElement::Expr(e) | crate::parse::NArrayElement::Spread(e) => {
                expr_references_callee(e, callee)
            }
            crate::parse::NArrayElement::Hole => false,
        }),
        NExpr::Object(props) => props.iter().any(|p| match p {
            NObjectProp::KeyValue { value, .. } | NObjectProp::Spread(value) => {
                expr_references_callee(value, callee)
            }
            NObjectProp::Other(_) => false,
        }),
        NExpr::Arrow { body, .. } => match body.as_ref() {
            NArrowBody::Expr(e) => expr_references_callee(e, callee),
            NArrowBody::Block(stmts) => statements_reference_callee(stmts, callee),
        },
        NExpr::Function { body, .. } => statements_reference_callee(body, callee),
        _ => false,
    }
}

/// Find the constructor `new (t || X)(<args>)` (or `new t(<args>)` in a delegated factory) call's
/// arguments within a factory body. Scans the `return`/assignment statements for a `New` expression.
fn find_ctor_call_args(stmts: &[NStmt]) -> Option<&[NArg]> {
    for stmt in stmts {
        if let Some(args) = ctor_args_in_statement(stmt) {
            return Some(args);
        }
    }
    None
}

/// Locate a `new …(<args>)` expression inside one factory-body statement (a `return new …`, or a
/// conditional `r = new …`), returning its arguments.
fn ctor_args_in_statement(stmt: &NStmt) -> Option<&[NArg]> {
    match stmt {
        NStmt::Return(Some(e)) => new_expr_args(e),
        NStmt::Return(None) => None,
        NStmt::Expr(e) => new_expr_args(e),
        NStmt::If { consequent, alternate, .. } => {
            // The conditional factory's then/else branches assign `r = new …` / `r = <nonCtor>`.
            find_ctor_call_args(consequent).or_else(|| find_ctor_call_args(alternate))
        }
        NStmt::Block(body) => find_ctor_call_args(body),
        _ => None,
    }
}

/// The arguments of a `new …(<args>)` expression. The neutral tree has no assignment-expression node
/// (oxc surfaced `r = new …` as one); the codegen's conditional-factory `r = new …` is modelled by the
/// emitter's `If` branches as bare expression statements whose expression is the `new`, so unwrapping
/// the parenthesized chain to a `New` is sufficient.
fn new_expr_args(expr: &NExpr) -> Option<&[NArg]> {
    match inner_expr(expr) {
        NExpr::New { args, .. } => Some(args.as_slice()),
        _ => None,
    }
}

/// Map ONE constructor-argument inject call back to its `{ token, …flags }` declaration entry SOURCE:
///   * `i0.ɵɵinject(Token[, flags])` / `i0.ɵɵdirectiveInject(Token[, flags])` →
///     `{ token: <Token src>[, host: true][, optional: true][, self: true][, skipSelf: true] }`
///     (Angular `compileDependency` field order: token, attribute, host, optional, self, skipSelf)
///     (the flags are decoded from the numeric `InjectFlags` 2nd arg: HOST=1, SELF=2, SKIP_SELF=4,
///     OPTIONAL=8; the FOR_PIPE=16 bit is a codegen marker, dropped from the declaration);
///   * `i0.ɵɵinjectAttribute("name")` → `{ token: "name", attribute: true }`;
///   * `i0.ɵɵinvalidFactoryDep(i)` (or anything else) → `None` (poisons the factory to `"invalid"`).
fn dep_entry_from_inject(backend: &ParsingBackend, arg: &NArg, source: &str) -> Option<String> {
    let NExpr::Call { callee, args } = inner_expr(arg.expr()) else {
        return None;
    };
    let callee = call_callee_name(callee)?;
    match callee {
        "\u{0275}\u{0275}inject" | "\u{0275}\u{0275}directiveInject" => {
            let token = arg_source(backend, args.first()?, source)?;
            let mut entry = format!("{{ token: {token}");
            if let Some(flags_arg) = args.get(1) {
                if let Some(flags) = numeric_literal_value(flags_arg.expr()) {
                    let bits = flags as u8;
                    // Field emit order follows Angular's `compileDependency` (compiler.mjs):
                    // token, attribute, host, optional, self, skipSelf — NOT the InjectFlags bit
                    // order. (`attribute` rides the separate `ɵɵinjectAttribute` path, never combined
                    // with these flags.) Match it for byte parity with ng-packagr's GOLDEN_PARTIAL.
                    if bits & 0b0_0001 != 0 {
                        entry.push_str(", host: true");
                    }
                    if bits & 0b0_1000 != 0 {
                        entry.push_str(", optional: true");
                    }
                    if bits & 0b0_0010 != 0 {
                        entry.push_str(", self: true");
                    }
                    if bits & 0b0_0100 != 0 {
                        entry.push_str(", skipSelf: true");
                    }
                }
            }
            entry.push_str(" }");
            Some(entry)
        }
        "\u{0275}\u{0275}injectAttribute" => {
            let token = arg_source(backend, args.first()?, source)?;
            Some(format!("{{ token: {token}, attribute: true }}"))
        }
        // `ɵɵinvalidFactoryDep(i)` or any unknown call — the dep is unresolvable.
        _ => None,
    }
}

/// The trimmed source slice of a call argument expression (the injection TOKEN), preserving an
/// opaque token expression (`MyService`, `'name'`, `dynamicAttrName()`, `i0.ChangeDetectorRef`)
/// verbatim so it round-trips. Recovered through the parse backend's `span_text` on the argument's
/// neutral span (the span of the argument EXPRESSION, as the live `arg.as_expression().span()` was).
fn arg_source<'a>(backend: &ParsingBackend, arg: &NArg, source: &'a str) -> Option<&'a str> {
    Some(span_src(backend, source, arg.span()))
}

/// The numeric value of a number literal — the `InjectFlags` 2nd inject arg.
fn numeric_literal_value(expr: &NExpr) -> Option<f64> {
    match expr {
        NExpr::Number(n) => Some(*n),
        _ => None,
    }
}

/// `ɵɵngDeclareInjectable({...})` from an `ɵɵdefineInjectable({ token, factory, providedIn })`.
/// The `token`/`factory` fields are the linker's responsibility to regenerate, so only `type` +
/// `providedIn` (the declarative inputs) are carried.
fn ng_declare_injectable(
    backend: &ParsingBackend,
    type_name: &str,
    props: &[NObjectProp],
    source: &str,
) -> String {
    let mut fields = format!("{}, type: {type_name}", declare_prelude());
    if let Some(provided_in) = prop_source(backend, props, "providedIn", source) {
        fields.push_str(&format!(", providedIn: {provided_in}"));
    }
    format!("i0.\u{0275}\u{0275}ngDeclareInjectable({{ {fields} }})")
}

/// `ɵɵngDeclarePipe({...})` from an `ɵɵdefinePipe({ name, type, pure })`. Mirrors Angular's own
/// partial pipe emitter: `minVersion: "14.0.0"`, an explicit `isStandalone: true` for the
/// standalone-default case, the pipe `name`, and an explicit `pure: false` only when impure.
fn ng_declare_pipe(
    backend: &ParsingBackend,
    type_name: &str,
    props: &[NObjectProp],
    source: &str,
) -> String {
    // The pipe declaration's minVersion is `"14.0.0"` (the earliest linker that understands the
    // pipe declaration shape), matching ng-packagr's published output.
    let mut fields = format!("{}, type: {type_name}", declare_prelude_min("14.0.0"));
    // The pipe DEF carries no `standalone` field (it is the v19+ default `true`); a published
    // declaration states it explicitly, exactly as ng-packagr emits `isStandalone: true`.
    fields.push_str(", isStandalone: true");
    if let Some(name) = prop_source(backend, props, "name", source) {
        fields.push_str(&format!(", name: {name}"));
    }
    // `pure` defaults to true in both the def and the declaration; carry an explicit `false` only.
    if prop_source(backend, props, "pure", source) == Some("false") {
        fields.push_str(", pure: false");
    }
    format!("i0.\u{0275}\u{0275}ngDeclarePipe({{ {fields} }})")
}

/// `ɵɵngDeclareInjector({...})` from an `ɵɵdefineInjector({ providers?, imports? })`. Both are opaque
/// arrays carried verbatim.
fn ng_declare_injector(
    backend: &ParsingBackend,
    type_name: &str,
    props: &[NObjectProp],
    source: &str,
) -> String {
    let mut fields = format!("{}, type: {type_name}", declare_prelude());
    if let Some(providers) = prop_source(backend, props, "providers", source) {
        fields.push_str(&format!(", providers: {providers}"));
    }
    if let Some(imports) = prop_source(backend, props, "imports", source) {
        fields.push_str(&format!(", imports: {imports}"));
    }
    format!("i0.\u{0275}\u{0275}ngDeclareInjector({{ {fields} }})")
}

/// `ɵɵngDeclareNgModule({...})` from an `ɵɵdefineNgModule({ type, ... })`.
///
/// The AOT NgModule def emits its declarations/imports/exports as a tree-shakeable
/// `ɵɵsetNgModuleScope(X, { declarations, imports, exports })` SIDE-EFFECT statement (not on the def
/// object), so the declarative arrays are read from that sibling call when present and folded back
/// onto the declaration object (matching the published partial shape).
fn ng_declare_ng_module(
    backend: &ParsingBackend,
    type_name: &str,
    props: &[NObjectProp],
    source: &str,
    top_level: &[NTopStmt],
) -> String {
    let mut fields = format!("{}, type: {type_name}", declare_prelude());
    // `bootstrap`/`id` may sit on the def object directly.
    for key in ["bootstrap", "id"] {
        if let Some(v) = prop_source(backend, props, key, source) {
            fields.push_str(&format!(", {key}: {v}"));
        }
    }
    // declarations/imports/exports come from a sibling `ɵɵsetNgModuleScope(X, {...})` call.
    if let Some(scope) = find_set_scope_object(backend, top_level, type_name, source) {
        for key in ["declarations", "imports", "exports"] {
            if let Some(v) = scope.get(key) {
                fields.push_str(&format!(", {key}: {v}"));
            }
        }
    }
    format!("i0.\u{0275}\u{0275}ngDeclareNgModule({{ {fields} }})")
}

/// Locate a `i0.ɵɵsetNgModuleScope(<type_name>, { ... })` call and return its scope object's
/// `declarations`/`imports`/`exports` field source slices keyed by name. Walks the neutral top-level
/// expression-statement surface (the scope call sits as a bare `ExprStmt`, possibly guarded).
fn find_set_scope_object(
    backend: &ParsingBackend,
    top_level: &[NTopStmt],
    type_name: &str,
    source: &str,
) -> Option<std::collections::HashMap<&'static str, String>> {
    for stmt in top_level {
        // The scope call may be wrapped in a `(typeof ngJitMode … && i0.ɵɵsetNgModuleScope(...))`
        // guard expression-statement; scan expression statements for the call.
        let NTopStmt::ExprStmt { expr, .. } = stmt else { continue };
        if let Some(map) = scope_from_expression(backend, expr, type_name, source) {
            return Some(map);
        }
    }
    None
}

/// Recursively search a neutral expression for a `ɵɵsetNgModuleScope(<type_name>, {…})` call.
fn scope_from_expression(
    backend: &ParsingBackend,
    expr: &NExpr,
    type_name: &str,
    source: &str,
) -> Option<std::collections::HashMap<&'static str, String>> {
    match expr {
        NExpr::Call { callee, args } => {
            let is_set_scope = call_callee_name(callee) == Some("\u{0275}\u{0275}setNgModuleScope");
            if is_set_scope {
                // First arg is the module type; second is the scope object.
                let matches_type =
                    matches!(args.first().map(NArg::expr), Some(NExpr::Identifier(id)) if id == type_name);
                if matches_type {
                    if let Some(NExpr::Object(scope_props)) = args.get(1).map(NArg::expr) {
                        let mut map = std::collections::HashMap::new();
                        for key in ["declarations", "imports", "exports"] {
                            if let Some(v) = prop_source(backend, scope_props, key, source) {
                                map.insert(key, v.to_string());
                            }
                        }
                        return Some(map);
                    }
                }
            }
            // Recurse into arguments (guarded forms wrap the call).
            args.iter()
                .find_map(|a| scope_from_expression(backend, a.expr(), type_name, source))
        }
        // `(typeof ngJitMode … || ngJitMode) && i0.ɵɵsetNgModuleScope(...)` — oxc's `LogicalExpression`
        // is unified into `NExpr::Binary` (operator `&&`/`||`); scan both sides.
        NExpr::Binary { left, right, .. } => {
            scope_from_expression(backend, left, type_name, source)
                .or_else(|| scope_from_expression(backend, right, type_name, source))
        }
        NExpr::Parenthesized(inner) => scope_from_expression(backend, inner, type_name, source),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link_partial;
    use crate::source_compile::compile_component_source;

    /// Canonicalize a module to compare a partial→linked round-trip against the original AOT,
    /// ignoring cosmetic differences that are semantically inert: ALL whitespace is removed, the
    /// linker's `(function …);` factory-wrapper parens are stripped, the dropped dev-only
    /// `ɵɵngDeclareClassMetadata` (which the linker replaces with a `void 0;` expression statement)
    /// is removed, and redundant `;` runs are collapsed. What remains is the exact token stream of
    /// the definitions.
    fn norm(code: &str) -> String {
        // Drop all ASCII whitespace.
        let mut s: String = code.chars().filter(|c| !c.is_whitespace()).collect();
        // The linker wraps a regenerated constructor factory as `ɵfac=(function …(){ … });` — strip
        // the wrapping `(` after `ɵfac=`, then drop the single matching wrapper-closing `)` that
        // precedes the factory's terminating `;`. The factory body always ends `…);}` (the `new …(…)`
        // expression's `)`, then the return `;`, then the function `}`); the wrapper adds one more
        // `)`, yielding `…);})`. Strip THAT trailing wrapper `)` so it matches the AOT `…);}`.
        s = s.replace("ɵfac=(function", "ɵfac=function");
        // No-arg ctor: body ends `();}` → wrapped `();})`. With ctor args it ends `…));}` → `…));})`.
        // Either way the wrapper close is the `)` immediately after the function's closing `}`,
        // i.e. the substring `;})`. Collapse it to `;}` (the AOT bare form).
        s = s.replace(";})", ";}");
        // The dropped `ɵɵngDeclareClassMetadata` links to a bare `void 0;` expression statement (a
        // dev-only reflection aid Treaty's classic AOT emit never produced). It is semantically inert
        // and absent from the direct AOT, so remove it for the round-trip token comparison.
        s = s.replace("void0;", "");
        // Collapse redundant `;` runs the linker leaves between rewritten statements.
        while s.contains(";;") {
            s = s.replace(";;", ";");
        }
        s
    }

    #[test]
    fn pipe_partial_emits_ng_declare() {
        let aot = compile_component_source(
            "import { Pipe } from '@angular/core';\n@Pipe({ name: 'shout', standalone: true })\nexport class ShoutPipe { transform(v){return v;} }",
        );
        let partial = emit_partial(&aot.code);
        assert!(
            partial.code.contains("\u{0275}\u{0275}ngDeclarePipe"),
            "no ngDeclarePipe; got:\n{}",
            partial.code
        );
        assert!(partial.code.contains("\u{0275}\u{0275}ngDeclareFactory"), "no ngDeclareFactory");
        assert!(partial.code.contains("name: \"shout\""), "pipe name lost");
        // The partial form must NOT carry the AOT define call.
        assert!(!partial.code.contains("\u{0275}\u{0275}definePipe"), "AOT definePipe survived");
    }

    #[test]
    fn pipe_round_trips_through_linker() {
        let aot = compile_component_source(
            "import { Pipe } from '@angular/core';\n@Pipe({ name: 'shout', standalone: true })\nexport class ShoutPipe { transform(v){return v;} }",
        );
        let partial = emit_partial(&aot.code);
        let relinked = link_partial(&partial.code, "x.mjs");
        assert!(relinked.errors.is_empty(), "relink errors: {:?}", relinked.errors);
        // The partial→linked output must match the original AOT (modulo cosmetic whitespace).
        assert_eq!(
            norm(&relinked.code),
            norm(&aot.code),
            "pipe round-trip diverged\n--- AOT ---\n{}\n--- RELINKED ---\n{}",
            aot.code,
            relinked.code
        );
    }

    #[test]
    fn injectable_round_trips_through_linker() {
        let aot = compile_component_source(
            "import { Injectable } from '@angular/core';\n@Injectable({ providedIn: 'root' })\nexport class Svc { x = 1; }",
        );
        let partial = emit_partial(&aot.code);
        assert!(partial.code.contains("\u{0275}\u{0275}ngDeclareInjectable"), "no ngDeclareInjectable");
        let relinked = link_partial(&partial.code, "x.mjs");
        assert!(relinked.errors.is_empty(), "relink errors: {:?}", relinked.errors);
        assert_eq!(
            norm(&relinked.code),
            norm(&aot.code),
            "injectable round-trip diverged\n--- AOT ---\n{}\n--- RELINKED ---\n{}",
            aot.code,
            relinked.code
        );
    }

    #[test]
    fn component_left_as_aot_with_note() {
        let aot = compile_component_source(
            "import { Component } from '@angular/core';\n@Component({ selector: 'app-x', template: '<div>{{x}}</div>' })\nexport class X { x = 1; }",
        );
        let partial = emit_partial(&aot.code);
        // The component def is left as AOT (no partial component emit) and reported.
        assert!(partial.code.contains("\u{0275}\u{0275}defineComponent"), "component def should remain");
        assert!(
            partial.notes.iter().any(|n| n.contains("component")),
            "expected a note about the component left as AOT; got: {:?}",
            partial.notes
        );
    }

    // ----------------------------------------------------------------------
    // PARTIAL component / directive declaration emit (source-side) + round-trip.
    // ----------------------------------------------------------------------

    use crate::source_compile::{compile_component_source_with_options, CompileOptions};

    /// Compile in partial mode and run the `emit_partial` pass over the result (the packagr flow):
    /// the source front-end emits `ɵɵngDeclareComponent`/`ɵɵngDeclareDirective`; `emit_partial` then
    /// inverts the DI-family `ɵfac` to `ɵɵngDeclareFactory` and leaves the already-partial def alone.
    fn partial_pipeline(src: &str) -> PartialEmit {
        let opts = CompileOptions {
            emit_partial_component: true,
            ..CompileOptions::default()
        };
        let compiled = compile_component_source_with_options(src, opts);
        assert!(compiled.errors.is_empty(), "compile errors: {:?}", compiled.errors);
        emit_partial(&compiled.code)
    }

    #[test]
    fn component_partial_emits_ng_declare_and_round_trips() {
        let src = "import { Component } from '@angular/core';\n@Component({ selector: 'app-x', template: '<div>{{x}}</div>' })\nexport class X { x = 1; }";
        let partial = partial_pipeline(src);
        // Partial form: ɵɵngDeclareComponent (NOT the AOT ɵɵdefineComponent), plus the ɵfac inverted
        // to ɵɵngDeclareFactory; no "left as AOT" note.
        assert!(
            partial.code.contains("\u{0275}\u{0275}ngDeclareComponent"),
            "no ngDeclareComponent; got:\n{}",
            partial.code
        );
        assert!(
            !partial.code.contains("\u{0275}\u{0275}defineComponent"),
            "AOT defineComponent survived; got:\n{}",
            partial.code
        );
        assert!(
            partial.code.contains("\u{0275}\u{0275}ngDeclareFactory"),
            "component ɵfac not inverted to ngDeclareFactory; got:\n{}",
            partial.code
        );
        assert!(
            partial.code.contains("template: \"<div>{{x}}</div>\""),
            "inline template string not carried; got:\n{}",
            partial.code
        );
        assert!(
            !partial.notes.iter().any(|n| n.contains("component")),
            "component should NOT be reported as left-as-AOT; got: {:?}",
            partial.notes
        );

        // ROUND-TRIP: the partial declaration links back to a valid AOT ɵɵdefineComponent.
        let relinked = link_partial(&partial.code, "x.mjs");
        assert!(relinked.errors.is_empty(), "relink errors: {:?}", relinked.errors);
        assert!(
            relinked.code.contains("\u{0275}\u{0275}defineComponent"),
            "linker did not restore ɵɵdefineComponent; got:\n{}",
            relinked.code
        );
        // The real template instruction function + bound expression are regenerated.
        assert!(relinked.code.contains("X_Template"), "no template fn; got:\n{}", relinked.code);
        assert!(relinked.code.contains("ctx.x"), "template binding lost; got:\n{}", relinked.code);
        assert_no_residual_declare(&relinked.code);
    }

    #[test]
    fn directive_partial_emits_ng_declare_and_round_trips() {
        let src = "import { Directive, Input } from '@angular/core';\n@Directive({ selector: '[appHi]', exportAs: 'hi' })\nexport class HiDir { @Input() value = ''; }";
        let partial = partial_pipeline(src);
        assert!(
            partial.code.contains("\u{0275}\u{0275}ngDeclareDirective"),
            "no ngDeclareDirective; got:\n{}",
            partial.code
        );
        assert!(
            !partial.code.contains("\u{0275}\u{0275}defineDirective"),
            "AOT defineDirective survived; got:\n{}",
            partial.code
        );
        // The declarative directive base is carried (selector, exportAs array, inputs map).
        assert!(partial.code.contains("selector: \"[appHi]\""), "selector lost; got:\n{}", partial.code);
        assert!(partial.code.contains("exportAs: [\"hi\"]"), "exportAs lost; got:\n{}", partial.code);
        assert!(partial.code.contains("inputs:"), "inputs lost; got:\n{}", partial.code);

        let relinked = link_partial(&partial.code, "x.mjs");
        assert!(relinked.errors.is_empty(), "relink errors: {:?}", relinked.errors);
        assert!(
            relinked.code.contains("\u{0275}\u{0275}defineDirective"),
            "linker did not restore ɵɵdefineDirective; got:\n{}",
            relinked.code
        );
        assert_no_residual_declare(&relinked.code);
    }

    #[test]
    fn component_partial_round_trips_byte_equal_modulo_cosmetic_to_direct_aot() {
        // The partial→linked output should match the DIRECT AOT emit (modulo the cosmetic factory
        // paren-wrap `norm` already canonicalizes), proving the parallel partial emit carries the
        // same metadata the AOT path does.
        let src = "import { Component, Input } from '@angular/core';\n@Component({ selector: 'app-y', template: '<span>{{label}}</span>' })\nexport class Y { @Input() label = ''; }";
        let direct_aot = compile_component_source(src);
        assert!(direct_aot.errors.is_empty(), "direct AOT errors: {:?}", direct_aot.errors);

        let partial = partial_pipeline(src);
        let relinked = link_partial(&partial.code, "y.mjs");
        assert!(relinked.errors.is_empty(), "relink errors: {:?}", relinked.errors);

        assert_eq!(
            norm(&relinked.code),
            norm(&direct_aot.code),
            "component partial round-trip diverged from direct AOT\n--- DIRECT AOT ---\n{}\n--- RELINKED ---\n{}",
            direct_aot.code,
            relinked.code
        );
    }

    #[test]
    fn directive_partial_field_shape_matches_real_ng_packagr_golden() {
        // GATE #4: compare the emitted `ɵɵngDeclareDirective` fields to a REAL Angular partial build
        // of the SAME `model()`-input directive (corpus `model_inputs/model_directive_definition.ts`,
        // golden `model_inputs/GOLDEN_PARTIAL.js`). The reference golden's declaration is:
        //   ɵɵngDeclareDirective({ minVersion: "17.1.0", version: "0.0.0-PLACEHOLDER", type: TestDir,
        //     isStandalone: true,
        //     inputs: { counter: { classPropertyName: "counter", publicName: "counter",
        //       isSignal: true, isRequired: false, transformFunction: null }, name: { … } },
        //     outputs: { counter: "counterChange", name: "nameChange" }, ngImport: i0 });
        //
        // We match the field SHAPE byte-for-byte for the `inputs`/`outputs` maps (rich object form,
        // UNQUOTED identifier keys) and the `type`/`isStandalone`/`ngImport` placement. Two honest,
        // documented differences remain (see notes), neither a round-trip break:
        //   * `minVersion`: Angular bumps it to "17.1.0" for model() inputs; we stamp the family-wide
        //     "14.0.0" (the linker accepts both — `is_placeholder_version` keys on the version).
        //   * top-level `isSignal: true`: this compiler derives `base.is_signal` from signal inputs,
        //     so it emits `isSignal` (and the AOT define emits `signals: true`) where real Angular —
        //     which sets the directive-level signal flag only for signal-based components — omits it.
        //     It is round-trip-consistent (relinked AOT == direct AOT).
        let src = "import { Directive, model } from '@angular/core';\n\
                   @Directive({})\n\
                   export class TestDir { counter = model(0); name = model.required<string>(); }";
        let partial = partial_pipeline(src);
        // Collapse the emitter's pretty-print whitespace to single spaces so the comparison is over
        // the field STRUCTURE (the real golden is single-line; our emit is multi-line — same tokens).
        let esm = partial.code.split_whitespace().collect::<Vec<_>>().join(" ");

        // The exact rich-input shape the real golden carries (UNQUOTED keys, insertion order).
        assert!(
            esm.contains(
                "inputs: { counter: { classPropertyName: \"counter\", publicName: \"counter\", isSignal: true, isRequired: false, transformFunction: null }, \
                 name: { classPropertyName: \"name\", publicName: \"name\", isSignal: true, isRequired: true, transformFunction: null } }"
            ),
            "inputs map field shape diverges from the real ng-packagr golden; got:\n{}",
            esm
        );
        // The `model()` two-way output (`<name>Change`) shape, exactly as Angular emits it.
        assert!(
            esm.contains("outputs: { counter: \"counterChange\", name: \"nameChange\" }"),
            "outputs map field shape diverges from the real golden; got:\n{}",
            esm
        );
        // `type`/`isStandalone` lead the base, `ngImport` is the trailing field — the reference order.
        assert!(esm.contains("type: TestDir, isStandalone: true, inputs:"), "base field order diverged; got:\n{}", esm);
        assert!(esm.contains(", ngImport: i0 }"), "ngImport must be the trailing directive field; got:\n{}", esm);
        // And it still round-trips to a valid AOT directive.
        let relinked = link_partial(&esm, "x.ts");
        assert!(relinked.errors.is_empty(), "relink errors: {:?}", relinked.errors);
        assert!(relinked.code.contains("\u{0275}\u{0275}defineDirective"), "no defineDirective; got:\n{}", relinked.code);
    }

    #[test]
    fn component_partial_with_host_query_and_dependency_round_trips() {
        // A richer single-file component: a host listener (non-identifier key → quoted), a view query,
        // an output, and a same-file directive dependency matched by SELECTOR. The partial declaration
        // must carry all of it and round-trip to the SAME AOT define as the direct Full emit.
        let src = "import { Component, Directive, Output, EventEmitter, ViewChild, ElementRef, HostListener } from '@angular/core';\n\
                   @Directive({ selector: '[hl]' })\n\
                   export class HlDir {}\n\
                   @Component({ selector: 'app-z', template: '<div hl #r></div>' })\n\
                   export class Z {\n\
                     @Output() done = new EventEmitter<void>();\n\
                     @ViewChild('r') r!: ElementRef;\n\
                     @HostListener('click') onClick() {}\n\
                   }";
        let partial = partial_pipeline(src);
        let esm = &partial.code;
        assert!(esm.contains("\u{0275}\u{0275}ngDeclareComponent"), "no ngDeclareComponent; got:\n{}", esm);
        assert!(esm.contains("\u{0275}\u{0275}ngDeclareDirective"), "no ngDeclareDirective; got:\n{}", esm);
        // The host listener key is a valid identifier here (`click`) so it stays unquoted; the
        // dependency carries kind/type/selector.
        let flat = esm.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(flat.contains("listeners: { click:"), "host listener lost; got:\n{}", flat);
        assert!(
            flat.contains("dependencies: [{ kind: \"directive\", type: HlDir, selector: \"[hl]\" }]"),
            "dependency entry shape diverged; got:\n{}",
            flat
        );

        // Round-trip the WHOLE module: both declarations must restore to AOT defines, no residual.
        let relinked = link_partial(esm, "z.ts");
        assert!(relinked.errors.is_empty(), "relink errors: {:?}", relinked.errors);
        assert!(relinked.code.contains("\u{0275}\u{0275}defineComponent"), "no defineComponent; got:\n{}", relinked.code);
        assert!(relinked.code.contains("\u{0275}\u{0275}defineDirective"), "no defineDirective; got:\n{}", relinked.code);
        assert!(relinked.code.contains("\u{0275}\u{0275}listener"), "host listener instruction lost; got:\n{}", relinked.code);
        assert!(relinked.code.contains("\u{0275}\u{0275}viewQuery"), "view query instruction lost; got:\n{}", relinked.code);
        assert_no_residual_declare(&relinked.code);
    }

    /// No residual `ɵɵngDeclare*` partial call survives a link.
    fn assert_no_residual_declare(code: &str) {
        assert!(
            !code.contains("\u{0275}\u{0275}ngDeclare"),
            "residual ngDeclare* after link; got:\n{}",
            code
        );
    }

    // ----------------------------------------------------------------------
    // GAP 2 — ɵɵngDeclareFactory NON-EMPTY deps (decompiled from the AOT factory).
    // ----------------------------------------------------------------------

    /// Flatten a code string to single spaces for field-shape comparison.
    fn flat(code: &str) -> String {
        code.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn factory_deps_decompiled_for_constructor_di() {
        // A `@Injectable` with two ctor deps (one `@Optional`) must publish its REAL `deps:[{token},
        // {token, optional:true}]` (the prior emit dropped them to `deps:[]`). Matches the
        // `r3_view_compiler_di` golden's `ctor_overload` factory shape.
        let aot = compile_component_source(
            "import { Injectable, Optional } from '@angular/core';\n\
             class Dep {}\n class OptDep {}\n\
             @Injectable()\n\
             export class Svc { constructor(d: Dep, @Optional() o: OptDep) {} }",
        );
        let partial = emit_partial(&aot.code);
        let f = flat(&partial.code);
        assert!(
            f.contains("deps: [{ token: Dep }, { token: OptDep, optional: true }]"),
            "ctor DI deps not decompiled into the factory declaration; got:\n{}",
            f
        );
        // Round-trips back to the SAME AOT factory.
        let relinked = link_partial(&partial.code, "svc.ts");
        assert!(relinked.errors.is_empty(), "relink errors: {:?}", relinked.errors);
        assert_eq!(
            norm(&relinked.code),
            norm(&aot.code),
            "ctor-DI factory round-trip diverged\n--- AOT ---\n{}\n--- RELINKED ---\n{}",
            aot.code,
            relinked.code
        );
        assert_no_residual_declare(&relinked.code);
    }

    #[test]
    fn factory_deps_decode_all_inject_flags_and_attribute() {
        // Every `@Inject`/`@Host`/`@Self`/`@SkipSelf`/`@Optional`/`@Attribute` qualifier must decode
        // from the AOT inject-flag 2nd arg back into the declaration `{token, …flags}` — matching the
        // `r3_view_compiler_di` `component_factory` golden's deps array.
        let aot = compile_component_source(
            "import { Component, Inject, Host, Self, SkipSelf, Optional, Attribute } from '@angular/core';\n\
             class S {}\n\
             @Component({ selector: 'c', template: '' })\n\
             export class C { constructor(\
               @Attribute('name') a: string, s: S, @Host() h: S, @Self() se: S, \
               @SkipSelf() sk: S, @Optional() o: S) {} }",
        );
        let partial = emit_partial(&aot.code);
        let f = flat(&partial.code);
        // The AOT factory's `ɵɵinjectAttribute` token literal is the codegen-normalized double-quoted
        // form; the decompiler carries it verbatim (matching the golden's `attribute: true` entry).
        assert!(f.contains("{ token: \"name\", attribute: true }"), "attribute dep lost; got:\n{}", f);
        assert!(f.contains("{ token: S }"), "plain token dep lost; got:\n{}", f);
        assert!(f.contains("{ token: S, host: true }"), "host flag lost; got:\n{}", f);
        assert!(f.contains("{ token: S, self: true }"), "self flag lost; got:\n{}", f);
        assert!(f.contains("{ token: S, skipSelf: true }"), "skipSelf flag lost; got:\n{}", f);
        assert!(f.contains("{ token: S, optional: true }"), "optional flag lost; got:\n{}", f);
        let relinked = link_partial(&partial.code, "c.ts");
        assert!(relinked.errors.is_empty(), "relink errors: {:?}", relinked.errors);
        assert_eq!(norm(&relinked.code), norm(&aot.code), "flagged-deps round-trip diverged");
        assert_no_residual_declare(&relinked.code);
    }

    #[test]
    fn factory_dep_multi_flag_field_order_matches_compile_dependency() {
        // A dep carrying 2+ inject flags must emit its fields in Angular `compileDependency` order
        // (token, attribute, host, optional, self, skipSelf) — NOT InjectFlags bit order. The
        // `r3_view_compiler_di` GOLDEN_PARTIAL `component_factory` case proves the byte form for an
        // `@Optional() @Self()` dep: `{ token: MyService, optional: true, self: true }`.
        let aot = compile_component_source(
            "import { Component, Optional, Self } from '@angular/core';\n\
             class MyService {}\n\
             @Component({ selector: 'c', template: '' })\n\
             export class C { constructor(@Optional() @Self() s: MyService) {} }",
        );
        let partial = emit_partial(&aot.code);
        let f = flat(&partial.code);
        assert!(
            f.contains("{ token: MyService, optional: true, self: true }"),
            "multi-flag dep field order wrong (want optional before self); got:\n{}",
            f
        );
        let relinked = link_partial(&partial.code, "c.ts");
        assert!(relinked.errors.is_empty(), "relink errors: {:?}", relinked.errors);
        assert_eq!(norm(&relinked.code), norm(&aot.code), "multi-flag dep round-trip diverged");
        assert_no_residual_declare(&relinked.code);
    }

    #[test]
    fn factory_deps_use_constructor_implementation_signature() {
        // TS constructor overloads: only the IMPLEMENTATION (the one WITH a body) is authoritative.
        // ngtsc reads its full parameter list — the bodiless overload signatures may declare fewer.
        // The factory deps must reflect ALL implementation params, not the first overload's.
        let aot = compile_component_source(
            "import { Injectable } from '@angular/core';\n\
             class A {}\n class B {}\n\
             @Injectable()\n\
             export class S {\n\
               constructor(a: A);\n\
               constructor(a: A, b: B) {}\n\
             }",
        );
        let partial = emit_partial(&aot.code);
        let f = flat(&partial.code);
        assert!(
            f.contains("deps: [{ token: A }, { token: B }]"),
            "ctor-overload deps did not use the implementation signature (expected A and B); got:\n{}",
            f
        );
        let relinked = link_partial(&partial.code, "s.ts");
        assert!(relinked.errors.is_empty(), "relink errors: {:?}", relinked.errors);
        assert_eq!(norm(&relinked.code), norm(&aot.code), "ctor-overload round-trip diverged");
        assert_no_residual_declare(&relinked.code);
    }

    #[test]
    fn factory_deps_empty_for_no_arg_constructor() {
        // The dominant no-arg shape still publishes the empty `deps: []` (NOT `null`), so the linker
        // re-emits `function X_Factory(t){ return new (t||X)(); }` rather than inheriting a base.
        let aot = compile_component_source(
            "import { Injectable } from '@angular/core';\n@Injectable()\nexport class Svc {}",
        );
        let partial = emit_partial(&aot.code);
        assert!(flat(&partial.code).contains("deps: []"), "no-arg ctor must emit deps:[]; got:\n{}", partial.code);
        let relinked = link_partial(&partial.code, "svc.ts");
        assert!(relinked.errors.is_empty(), "relink errors: {:?}", relinked.errors);
        assert_eq!(norm(&relinked.code), norm(&aot.code), "empty-deps round-trip diverged");
    }

    // ----------------------------------------------------------------------
    // GAP 3 — ɵɵngDeclareClassMetadata companion statement.
    // ----------------------------------------------------------------------

    #[test]
    fn class_metadata_emitted_in_partial_mode() {
        // A partial-mode component publishes the dev-only `ɵɵngDeclareClassMetadata` carrying the
        // ORIGINAL `@Component` decorator (with verbatim args) and ctor params — like ng-packagr.
        let src = "import { Component, Optional } from '@angular/core';\n\
                   class Dep {}\n\
                   @Component({ selector: 'app-x', template: '<div>{{x}}</div>' })\n\
                   export class X { x = 1; constructor(d: Dep, @Optional() o: Dep) {} }";
        let partial = partial_pipeline(src);
        let f = flat(&partial.code);
        assert!(f.contains("\u{0275}\u{0275}ngDeclareClassMetadata("), "no class metadata; got:\n{}", f);
        // Decorator reproduced with verbatim args.
        assert!(
            f.contains("decorators: [{ type: Component, args: [{ selector: 'app-x', template: '<div>{{x}}</div>' }] }]"),
            "class-metadata decorator shape diverged; got:\n{}",
            f
        );
        // ctorParameters reproduce the param types + @Optional decorator.
        assert!(
            f.contains("ctorParameters: () => [{ type: Dep }, { type: Dep, decorators: [{ type: Optional }] }]"),
            "ctorParameters shape diverged; got:\n{}",
            f
        );
        // It round-trips: the linker DROPS class metadata to `void 0`, no residual.
        let relinked = link_partial(&partial.code, "x.ts");
        assert!(relinked.errors.is_empty(), "relink errors: {:?}", relinked.errors);
        assert!(relinked.code.contains("\u{0275}\u{0275}defineComponent"), "no defineComponent after link");
        assert_no_residual_declare(&relinked.code);
    }

    #[test]
    fn class_metadata_prop_decorators() {
        // A directive with `@Input`/`@Output`/`@HostListener` members publishes a `propDecorators` map.
        let src = "import { Directive, Input, Output, EventEmitter } from '@angular/core';\n\
                   @Directive({ selector: '[d]' })\n\
                   export class D { @Input() foo = ''; @Output() bar = new EventEmitter(); }";
        let partial = partial_pipeline(src);
        let f = flat(&partial.code);
        assert!(
            f.contains("propDecorators: { foo: [{ type: Input }], bar: [{ type: Output }] }"),
            "propDecorators shape diverged; got:\n{}",
            f
        );
        let relinked = link_partial(&partial.code, "d.ts");
        assert!(relinked.errors.is_empty(), "relink errors: {:?}", relinked.errors);
        assert_no_residual_declare(&relinked.code);
    }

    // ----------------------------------------------------------------------
    // GAP 1 — external `templateUrl` partial (resolved content channel).
    // ----------------------------------------------------------------------

    #[test]
    fn external_template_partial_omits_is_inline() {
        use crate::source_compile::{
            compile_component_source_with_options_and_resolved, CompileOptions,
            ResolvedComponentContent, ResolvedContentMap,
        };
        // A `templateUrl` + `styleUrls` component, compiled in partial mode WITH host-resolved
        // content, must inline the resolved template (NO `isInline`) and the resolved styles.
        let src = "import { Component } from '@angular/core';\n\
                   @Component({ selector: 'app-ext', templateUrl: './x.html', styleUrls: ['./x.css'] })\n\
                   export class Ext {}";
        let mut resolved: ResolvedContentMap = ResolvedContentMap::new();
        resolved.insert(
            "Ext".to_string(),
            ResolvedComponentContent {
                template: Some("<span>resolved</span>".to_string()),
                styles: vec![".a{color:red}".to_string()],
            },
        );
        let opts = CompileOptions { emit_partial_component: true, ..Default::default() };
        let compiled = compile_component_source_with_options_and_resolved(src, opts, &resolved);
        assert!(compiled.errors.is_empty(), "compile errors: {:?}", compiled.errors);
        let partial = emit_partial(&compiled.code);
        let f = flat(&partial.code);
        assert!(f.contains("\u{0275}\u{0275}ngDeclareComponent"), "no ngDeclareComponent; got:\n{}", f);
        // Resolved template inlined as the `template` string.
        assert!(f.contains("template: \"<span>resolved</span>\""), "resolved template not inlined; got:\n{}", f);
        // External template → NO `isInline` field (matching ng-packagr).
        assert!(!f.contains("isInline"), "external template must omit isInline; got:\n{}", f);
        // Resolved style inlined.
        assert!(f.contains("styles: [\".a{color:red}\"]"), "resolved style not inlined; got:\n{}", f);
        // Round-trips to a valid AOT component def.
        let relinked = link_partial(&partial.code, "ext.ts");
        assert!(relinked.errors.is_empty(), "relink errors: {:?}", relinked.errors);
        assert!(relinked.code.contains("\u{0275}\u{0275}defineComponent"), "no defineComponent after link");
        assert_no_residual_declare(&relinked.code);
    }

    #[test]
    fn fields_match_real_ng_packagr_di_golden() {
        // GATE: compile the REAL vendored compliance corpus input
        // `r3_view_compiler_di/di/injectable_factory.ts` through the partial pipeline and diff the
        // emitted `ɵɵngDeclareFactory` deps + `ɵɵngDeclareClassMetadata` ctorParameters against the
        // SAME case's real ng-packagr partial GOLDEN (`di/GOLDEN_PARTIAL.js`, the `injectable_factory`
        // section), field-for-field. The golden's MyService is:
        //   ɵfac = ɵɵngDeclareFactory({ …, type: MyService, deps: [{ token: MyDependency }],
        //                               target: i0.ɵɵFactoryTarget.Injectable });
        //   ɵprov = ɵɵngDeclareInjectable({ …, type: MyService });
        //   ɵɵngDeclareClassMetadata({ …, type: MyService, decorators: [{ type: Injectable }],
        //                              ctorParameters: () => [{ type: MyDependency }] });
        let src = "import {Injectable} from '@angular/core';\n\
                   class MyDependency {}\n\
                   @Injectable()\n\
                   export class MyService { constructor(dep: MyDependency) {} }";
        let partial = partial_pipeline(src);
        let f = flat(&partial.code);
        // GAP 2 — factory deps decompiled to the golden's exact `[{ token: MyDependency }]`.
        assert!(
            f.contains("deps: [{ token: MyDependency }], target: i0.\u{0275}\u{0275}FactoryTarget.Injectable"),
            "factory deps diverge from ng-packagr di golden; got:\n{}",
            f
        );
        // GAP 3 — class metadata decorators + ctorParameters match the golden field-for-field.
        assert!(
            f.contains("type: MyService, decorators: [{ type: Injectable }], ctorParameters: () => [{ type: MyDependency }] }"),
            "class metadata diverges from ng-packagr di golden; got:\n{}",
            f
        );
        // And the whole thing round-trips back to AOT with no residual partial calls.
        let relinked = link_partial(&partial.code, "injectable_factory.ts");
        assert!(relinked.errors.is_empty(), "relink errors: {:?}", relinked.errors);
        assert!(relinked.code.contains("\u{0275}\u{0275}defineInjectable"), "no defineInjectable after link");
        assert_no_residual_declare(&relinked.code);
    }

    #[test]
    fn inline_template_still_carries_is_inline() {
        // The inline-template path is unchanged: it still emits `isInline: true`.
        let src = "import { Component } from '@angular/core';\n\
                   @Component({ selector: 'app-x', template: '<div></div>' })\n\
                   export class X {}";
        let partial = partial_pipeline(src);
        assert!(flat(&partial.code).contains("isInline: true"), "inline template lost isInline; got:\n{}", partial.code);
    }
}
