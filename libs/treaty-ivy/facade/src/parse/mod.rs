//! Engine-neutral PARSE front-end seam for the facade's TypeScript-source modules.
//!
//! The facade's three source front-ends — [`crate::source_compile`] (the
//! `@Component`/`@Directive`/`@Pipe`/`@NgModule` AOT compiler), [`crate::linker`] (the
//! partial-declaration linker) and [`crate::partial_emit`] (the AOT→partial emitter) — every one
//! begins by parsing a TypeScript/JS source string into an AST and then walking it to extract Angular
//! metadata. Historically each of the nine parse call sites named `oxc_parser::Parser` /
//! `oxc_span::SourceType` directly. This module is the single CHOKEPOINT that owns the parser, so the
//! parse engine can be swapped behind a feature gate WITHOUT touching the front-end modules — mirroring
//! the EMIT chokepoint already established in `treaty_ivy_core::output::emitter` (oxc default /
//! `--features swc` neutral printer, gated 1:1 by `tools/backend-parity`). See
//! `migration/SWC-BACKEND-PLAN.md` (the parse half, Approach B).
//!
//! # Approach B — neutral data, engine-owned arena
//!
//! [`ParseBackend::parse_module`] takes the source + a [`SourceKind`] and a CALLBACK; it parses
//! internally (owning the parse arena for the callback's lifetime) and hands the callback a borrowed
//! [`ParseModule`]. The callback runs the existing metadata walk and returns an OWNED result (every
//! front-end produces a `String`-based artifact, so nothing borrows past the callback). Because the
//! backend owns the arena, the front-end never names an `oxc_` type to drive the parse.
//!
//! The engine-neutral data the parse exposes is pre-lowered into the structs below — chiefly
//! [`ObjLit`] / [`LitValue`] for object-literal metadata, walked in SOURCE order so emit ordering is
//! preserved byte-for-byte. The `oxc` backend ([`oxc::OxcParseBackend`]) is the ONLY facade file
//! permitted to `use oxc_`; it fills these structs and (for the parts of the walk that genuinely need
//! the live AST — arbitrary `Expression` conversion, ctor-dep extraction, span rewrites) exposes the
//! parsed [`ParseModule`] so the existing oxc walk runs unchanged and BYTE-IDENTICAL.
//!
//! # Spans
//!
//! [`TreatySpan`] is an engine-neutral `(start, end)` byte range. Recovering the TEXT a span covers is
//! engine-specific (oxc: absolute byte offsets into the original source; swc: `BytePos` relative to a
//! `SourceMap`), so callers go through [`ParseBackend::span_text`] rather than slicing the source
//! directly.

pub mod oxc;

/// The SWC parse backend — compiled only under `--features swc` (the heavy `swc_*` crates are off by
/// default). It fills the SAME neutral [`ParseOutput`] / [`ObjLit`] / [`LitValue`] the oxc backend
/// does, in source order, and is gated 1:1 against oxc by `tools/backend-parity` + the `parse_parity`
/// test below.
#[cfg(feature = "swc")]
pub mod swc;

/// What kind of source a parse call is handed, selecting the parser's `SourceType`/module flags.
///
/// The facade only ever parses three shapes, mirroring the historical `SourceType` choices at the
/// nine call sites:
///   - [`SourceKind::TypeScriptModule`] — the `@Component` source front-end + the linker's re-parse of
///     a declaration-object slice (`SourceType::default().with_typescript(true)`).
///   - [`SourceKind::TypeScriptEsModule`] — the AOT→partial emitter's input
///     (`…​.with_typescript(true).with_module(true)`).
///   - [`SourceKind::ByFilename`] — the linker's top-level entry, whose `SourceType` is chosen from the
///     `.mjs`/`.cjs`/`.js`/`.ts` extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind<'a> {
    /// `SourceType::default().with_typescript(true)`.
    TypeScriptModule,
    /// `SourceType::default().with_typescript(true).with_module(true)`.
    TypeScriptEsModule,
    /// `SourceType` derived from a filename's extension (`source_type_for` in the linker).
    ByFilename(&'a str),
}

/// The engine-neutral PARSE IR types — moved to `treaty_ivy_core::neutral` so the decorator →
/// definition layer (`treaty_ivy_decorators`) can name them through its public API
/// (`treaty_ivy_decorators::registry::ClassMeta`) without an oxc dependency. Re-exported from their
/// historical `crate::parse::*` paths here so every facade call site and the swc backend keep
/// resolving unchanged. The parse-channel types (`SourceKind`, `ImportInfo`, `NgDeclareCall`,
/// `ParseOutput`, `ParseBackend`) stay defined in this module — they describe the parse SEAM, not the
/// decorator surface.
pub use treaty_ivy_core::neutral::{
    ClassWithDecorators, DecoratorInfo, LitValue, MemberInfo, MemberKind, NArg, NArrayElement,
    NArrowBody, NAssignment, NCtorParam, NExpr, NObjectProp, NParam, NStmt, NTopStmt, NVarDeclarator,
    ObjLit, TreatySpan,
};

/// A top-level (or inline) IMPORT binding the foreign-import / imported-name collection reads: the
/// local binding name plus whether it is type-only (`import type` / inline `type`), mirroring
/// `source_compile::collect_imported_names`.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportInfo {
    /// The local binding name (`import { Foo as Bar }` → `Bar`; default / namespace → its local).
    pub local_name: String,
    /// Whether this binding is type-only (whole-declaration `import type` or inline `{ type Foo }`).
    pub type_only: bool,
}

/// A pre-lowered `ɵɵngDeclare*({…})` call discovered in a partial-declaration module: which kind of
/// declaration it is plus its pre-lowered object argument.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NgDeclareCall {
    /// The `ɵɵngDeclare*` callee suffix (`Component`, `Directive`, `Factory`, `Injectable`, …).
    pub kind: String,
    /// The declaration object argument, pre-lowered (its `nprops` carry the lossless full-`NExpr`
    /// values the partial-link walk lowers).
    pub object: ObjLit,
    /// The byte span of the WHOLE `ɵɵngDeclare*(...)` call expression — the bytes the partial-link
    /// surgical rewrite overwrites with the emitted `ɵɵdefine*(...)` text. (`object.span` covers only
    /// the `{...}` argument.) Filled identically by both backends.
    pub call_span: TreatySpan,
}

/// The engine-neutral SUMMARY of a parsed module — the pre-lowered, structurally-walkable surface.
///
/// This is the data shape the front-end can read WITHOUT naming an `oxc_` type. The parts of the walk
/// that still need the live AST reach it through the backend-specific [`ParseModule`] the callback is
/// handed, not through this summary.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParseOutput {
    /// Top-level (and `export`ed) classes carrying at least one decorator, in source order.
    pub classes: Vec<ClassWithDecorators>,
    /// `ɵɵngDeclare*({…})` calls found anywhere in the module, in source order.
    pub ng_declare_calls: Vec<NgDeclareCall>,
    /// Top-level `import` bindings (foreign-import / imported-name surface), in source order.
    pub imports: Vec<ImportInfo>,
    /// The TOP-LEVEL program statement surface, in source order — every top-level statement as an
    /// [`NTopStmt`] (definition-scaffold assignments, side-effect call statements, var declarations,
    /// everything else as `Other`). This is the engine-neutral surface the AOT→partial emitter
    /// (`partial_emit::collect_rewrites`) walks: the `X.ɵfac =`/`X.ɵprov = ɵɵdefineInjectable(…)`
    /// assignments it rewrites, the `ɵɵsetNgModuleScope(X, {…})` call it reads, and the factory-body
    /// decompile (all reachable through the assignments' `NExpr` values). Filled identically by both
    /// backends. (Additive this phase — the emitter still walks the live oxc AST.)
    pub top_level: Vec<NTopStmt>,
    /// Parse diagnostics; non-empty means the parse failed and `classes`/`ng_declare_calls` are empty.
    pub errors: Vec<String>,
}

/// The engine-neutral PARSE backend.
///
/// The single seam the nine facade parse call sites go through. The concrete backend owns the parse
/// arena for the lifetime of the [`ParseBackend::parse_module`] callback; the callback runs the
/// existing metadata walk against the borrowed module and returns an owned result.
pub trait ParseBackend {
    /// The engine-specific parsed-module handle handed to the callback. On the oxc backend this wraps
    /// the parsed `oxc_ast::Program` (so the existing walk runs unchanged); on a future swc backend it
    /// would wrap `swc_ecma_ast::Program` + the `SourceMap`.
    type Module<'a>
    where
        Self: 'a;

    /// Parse `source` as `kind`, then invoke `f` with a borrowed parsed module. The backend owns the
    /// parse arena for the duration of `f`; `f` returns an owned value (no borrow escapes).
    fn parse_module<'src, R>(
        &self,
        source: &'src str,
        kind: SourceKind<'_>,
        f: impl FnOnce(&Self::Module<'_>) -> R,
    ) -> R;

    /// Recover the source text a [`TreatySpan`] covers. On the oxc backend this is `&source[a..b]`; on
    /// the swc backend it is `SourceMap::span_to_snippet`. Callers MUST use this rather than slicing
    /// `source` directly so the span semantics stay backend-private.
    fn span_text<'src>(&self, source: &'src str, span: TreatySpan) -> &'src str;
}

// ---------------------------------------------------------------------------
// Feature-gated backend selection.
//
// Mirrors the EMIT chokepoint: `oxc` (default) is the reference backend. The SWC parse backend
// ([`swc::SwcParseBackend`]) is now IMPLEMENTED (compiled under `--features swc`) and fills the SAME
// neutral [`ParseOutput`] — proven byte-identical to oxc by `tools/backend-parity` + the
// `parse_parity` test below.
//
// The `ParsingBackend` alias the front-end imports stays on [`oxc::OxcParseBackend`] for now even
// under `--features swc`, because the facade's AOT/linker walks still reach the LIVE oxc `Program`
// through [`oxc::OxcModule::program`] — `compile_program_with_source` (in `crate::source_compile`)
// drives the walk against live oxc nodes. The `treaty_ivy_decorators::ClassMeta` PUBLIC API is now
// engine-NEUTRAL (it carries `ClassWithDecorators`/`DecoratorInfo`/`ObjLit`, re-exported from
// `treaty_ivy_core::neutral`); the live oxc nodes the AOT plugins still walk ride through that struct
// only as an OPAQUE, facade-private `ClassMeta::live` handle. Flipping the alias to the swc backend is
// gated on porting that walk to consume the neutral IR (SWC-BACKEND-PLAN.md §3.2 phase 3 — the wide
// port); until then the swc backend is exercised through the parity gate, not the alias, so BOTH the
// default and the `--features swc` builds compile against the same oxc-driven walk.
// ---------------------------------------------------------------------------

/// The active parse backend the front-end imports. [`oxc::OxcParseBackend`] today on every build (see
/// the module note above): the front-end's metadata walk still names the live oxc `Program`, so the
/// alias cannot point at the swc backend until that walk is neutralized. The swc backend is wired and
/// gated in parallel via `tools/backend-parity` + [`swc::SwcParseBackend`].
pub type ParsingBackend = oxc::OxcParseBackend;

// ---------------------------------------------------------------------------
// Parse-parity gate: the swc backend's neutral ParseOutput == the oxc backend's, byte-for-byte.
//
// This is the PARSE half of the `tools/backend-parity` discipline (the emit half lives in
// `treaty_ivy_core::output::emitter_swc`). It runs only when BOTH backends are compiled
// (`--features oxc,swc`) and asserts the two engines produce the IDENTICAL engine-neutral summary —
// same classes, same decorators, same object-literal props in the same SOURCE order, same span
// values (absolute offsets), same `ɵɵngDeclare*` extraction — over a representative fixture corpus.
// `ParseOutput` derives `PartialEq`, so a single `assert_eq!` is the whole gate.
// ---------------------------------------------------------------------------
#[cfg(all(test, feature = "oxc", feature = "swc"))]
mod parse_parity {
    use super::oxc::OxcParseBackend;
    use super::swc::SwcParseBackend;
    use super::{
        LitValue, NAssignment, NExpr, NObjectProp, NTopStmt, ParseBackend, ParseOutput, SourceKind,
    };

    /// The neutral summary each backend extracts for `source` under `kind`.
    fn oxc_summary(source: &str, kind: SourceKind<'_>) -> ParseOutput {
        OxcParseBackend.parse_module(source, kind, |m| m.summary().clone())
    }
    fn swc_summary(source: &str, kind: SourceKind<'_>) -> ParseOutput {
        SwcParseBackend.parse_module(source, kind, |m| m.summary().clone())
    }

    /// Assert the two backends' neutral summaries are byte-identical for one source.
    fn assert_parity(source: &str, kind: SourceKind<'_>) {
        let o = oxc_summary(source, kind);
        let s = swc_summary(source, kind);
        assert_eq!(o, s, "parse-parity DIFF for source:\n{source}");
    }

    /// The representative TypeScript-source corpus: a decorated component with inline + member
    /// decorators, nested object/array metadata, identifier + member-access values, numbers,
    /// booleans, a template-literal string, an exported class, and a default-exported class.
    const SOURCE_CORPUS: &[&str] = &[
        // Minimal component.
        r#"@Component({ selector: "app-x", template: "<div></div>" }) class XComponent {}"#,
        // Member decorators + several value kinds + source-order-sensitive props.
        r#"
@Component({
  selector: "app-hello",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `<h1>{{ title }}</h1>`,
  styles: ["h1 { color: red; }", ".a { margin: 0; }"],
  host: { "[class.x]": "y", "(click)": "onClick($event)" },
})
export class HelloComponent {
  @Input() title = "hi";
  @Output() done = new EventEmitter();
  @HostBinding("class.active") active = false;
  count = 3;
  ratio = 0.5;
}
"#,
        // Directive + Pipe + NgModule + Injectable shapes.
        r#"@Directive({ selector: "[appHl]" }) export class HlDirective {}"#,
        r#"@Pipe({ name: "money", standalone: true }) export class MoneyPipe {}"#,
        r#"@Injectable({ providedIn: "root" }) export class DataService {}"#,
        r#"@NgModule({ declarations: [A, B], imports: [CommonModule], exports: [A] }) export class M {}"#,
        // Default-exported decorated class.
        r#"@Component({ selector: "app-d" }) export default class DComponent {}"#,
        // Nested object metadata + array of arrays + null + boolean.
        r#"@Component({ selector: "app-n", animations: [{ name: "x", value: null, on: true }], data: [["a"], ["b"]] }) class NComponent {}"#,
        // METHOD decorators (`@HostListener`) + accessor decorators — these live on the swc
        // `ClassMethod.function.decorators` / `AutoAccessor.decorators`, NOT a top-level field, so this
        // exercises the parity-sensitive member-decorator extraction.
        r#"
@Component({ selector: "app-m", template: "" })
class MComponent {
  @HostListener("click", ["$event"]) onClick(e) {}
  @HostListener("window:resize") onResize() {}
  @Input() set value(v) {}
  get value() { return 1; }
}
"#,
        // CONSTRUCTOR DEPENDENCIES + parameter decorators: TS parameter properties (`private a: A`)
        // and explicit `@Inject`/`@Optional` param decorators — the surface ctor-dep extraction reads.
        // swc models param props as `TsParamProp`, oxc as a `FormalParameter` w/ accessibility; both
        // must yield the same neutral `NCtorParam` (name + decorators).
        r#"
import { Component, Inject } from "@angular/core";
import type { Foo } from "./foo";
import Default, { Named as Aliased, type TypeOnly } from "./mixed";
import * as ns from "./ns";
@Component({ selector: "app-di", template: "" })
export class DiComponent {
  constructor(
    private a: A,
    @Inject(TOKEN) @Optional() public b: B,
    readonly c: C,
  ) {}
}
"#,
        // MEMBER INITIALIZERS exercising the full neutral expression surface: signal `input()` /
        // query `viewChild()` calls, `new`, member access, computed member, conditional,
        // binary + logical + unary, array (with spread), object (with spread + computed key), and an
        // arrow `@Input({transform})`. Drives signal/query detection off the initializer.
        r#"
@Component({ selector: "app-sig", template: "" })
export class SigComponent {
  count = input(0);
  name = input.required<string>();
  first = viewChild("ref");
  emitter = new EventEmitter<number>();
  ref = this.svc.thing;
  idx = arr[0];
  flag = (a && b) || !c;
  sum = x + y * 2 - 1;
  cond = ready ? 1 : 0;
  list = [1, ...rest, 2];
  cfg = { a: 1, ["b-c"]: 2, ...defaults };
  @Input({ transform: (v) => v == null ? 0 : numberAttribute(v) }) value = 0;
  @Input({ transform: function (v) { const n = Number(v); return n; } }) other = 0;
}
"#,
    ];

    /// The representative partial-declaration corpus: `ɵɵngDeclare*` calls in the shapes the linker
    /// reads — bare callee, namespaced callee, as a free statement, and as a `static` class member.
    const NG_DECLARE_CORPUS: &[&str] = &[
        r#"ɵɵngDeclareComponent({ minVersion: "12.0.0", version: "22.1.0", type: X, selector: "app-x" });"#,
        r#"i0.ɵɵngDeclareDirective({ version: "22.1.0", type: Y, selector: "[appY]" });"#,
        r#"const f = i0.ɵɵngDeclareFactory({ type: Z, deps: [], target: 0 });"#,
        r#"class W { static ɵcmp = i0.ɵɵngDeclareComponent({ type: W, selector: "app-w", template: "<p></p>" }); }"#,
    ];

    /// The representative AOT-module corpus the AOT→partial emitter (`partial_emit::collect_rewrites`)
    /// walks: the `X.ɵfac = function …` / `X.ɵprov = i0.ɵɵdefineInjectable({…})` definition-scaffold
    /// ASSIGNMENT statements (with a real constructor-DI factory body — `new`, `ɵɵinject(Token, flags)`,
    /// `ɵɵinjectAttribute`, the `ɵɵgetInheritedFactory`/`ɵɵinvalidFactory` shapes), a tree-shakeable
    /// `ɵɵsetNgModuleScope(X, {…})` side-effect CALL statement, and an opaque `providedIn`/`useFactory`
    /// value to round-trip. This exercises the neutral TOP-LEVEL surface (`ParseOutput::top_level`):
    /// the assignment LHS/RHS spans, the factory-body `NExpr` tree, and the `NObjectProp::value_span` /
    /// `NArg` span surface — all of which must be byte-identical across the two backends.
    const PARTIAL_AOT_CORPUS: &[&str] = &[
        // Injectable with a constructor-DI factory + a `ɵɵdefineInjectable` with an opaque `useFactory`.
        r#"
export class Svc {}
Svc.ɵfac = function Svc_Factory(t) { return new (t || Svc)(i0.ɵɵinject(Dep), i0.ɵɵinject(Other, 8), i0.ɵɵinjectAttribute("name")); };
Svc.ɵprov = i0.ɵɵdefineInjectable({ token: Svc, factory: Svc.ɵfac, providedIn: "root" });
"#,
        // Pipe + an inherited-factory shape (`ɵɵgetInheritedFactory`).
        r#"
export class P {}
P.ɵfac = (function () { let ɵP_BaseFactory; return function P_Factory(t) { return (ɵP_BaseFactory || (ɵP_BaseFactory = i0.ɵɵgetInheritedFactory(P)))(t || P); }; })();
P.ɵpipe = i0.ɵɵdefinePipe({ name: "p", type: P, pure: false });
"#,
        // NgModule with a `ɵɵsetNgModuleScope` side-effect call (declarations/imports/exports).
        r#"
export class M {}
M.ɵmod = i0.ɵɵdefineNgModule({ type: M });
M.ɵinj = i0.ɵɵdefineInjector({ imports: [CommonModule] });
(typeof ngJitMode === "undefined" || ngJitMode) && i0.ɵɵsetNgModuleScope(M, { declarations: [A, B], imports: [CommonModule], exports: [A] });
"#,
        // Invalid-factory shape (`ɵɵinvalidFactory`) + a `useFactory` arrow on the provider.
        r#"
export class Q {}
Q.ɵfac = function Q_Factory(t) { i0.ɵɵinvalidFactory(); };
Q.ɵprov = i0.ɵɵdefineInjectable({ token: Q, factory: () => new Q(), providedIn: SomeMod });
"#,
    ];

    #[test]
    fn parse_parity_source_corpus() {
        for src in SOURCE_CORPUS {
            assert_parity(src, SourceKind::TypeScriptModule);
        }
    }

    #[test]
    fn parse_parity_ng_declare_corpus() {
        for src in NG_DECLARE_CORPUS {
            assert_parity(src, SourceKind::TypeScriptEsModule);
        }
    }

    /// The AOT-module TOP-LEVEL surface (`ParseOutput::top_level`) the partial emitter walks is
    /// byte-identical across the two backends for the whole partial-AOT corpus (the assignment LHS/RHS
    /// spans, the factory-body `NExpr` trees, the `NObjectProp::value_span` / `NArg` spans). The whole
    /// `ParseOutput` derives `PartialEq`, so `assert_parity` covers it.
    #[test]
    fn parse_parity_partial_aot_corpus() {
        for src in PARTIAL_AOT_CORPUS {
            assert_parity(src, SourceKind::TypeScriptEsModule);
        }
    }

    #[test]
    fn parse_parity_by_filename() {
        let src = r#"export class C { static ɵcmp = ɵɵngDeclareComponent({ type: C, selector: "c" }); }"#;
        assert_parity(src, SourceKind::ByFilename("foo.mjs"));
        assert_parity(src, SourceKind::ByFilename("foo.ts"));
    }

    /// Walk the REAL Angular compliance corpus (`tools/angular-ref/.../compliance/test_cases`) and
    /// assert the swc backend's neutral [`ParseOutput`] is byte-identical to oxc's for EVERY `.ts`
    /// input file. This is the broadest parse-parity measure — the actual sources the AOT front-end
    /// compiles — and reports the OK/DIFF counts. Skips cleanly when the corpus is not checked out
    /// (so a slim clone still passes), exactly like the `dump_corpus` harness.
    #[test]
    fn parse_parity_real_corpus() {
        use std::path::PathBuf;

        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let corpus = manifest
            .join("../../..")
            .join("tools/angular-ref/packages/compiler-cli/test/compliance/test_cases");
        let corpus = corpus.canonicalize().unwrap_or(corpus);
        if !corpus.is_dir() {
            eprintln!("compliance corpus not present at {corpus:?}; skipping real-corpus parity");
            return;
        }

        // Collect every `.ts` source under the corpus (the AOT inputs + their goldens are `.js`).
        let mut sources: Vec<PathBuf> = Vec::new();
        collect_ts(&corpus, &mut sources);
        sources.sort();

        let oxc = super::oxc::OxcParseBackend;
        let swc = super::swc::SwcParseBackend;
        let mut ok = 0usize;
        let mut diffs: Vec<String> = Vec::new();
        for path in &sources {
            let Ok(src) = std::fs::read_to_string(path) else {
                continue;
            };
            let o = oxc.parse_module(&src, SourceKind::TypeScriptModule, |m| m.summary().clone());
            let s = swc.parse_module(&src, SourceKind::TypeScriptModule, |m| m.summary().clone());
            // Only compare cases the oxc backend parsed cleanly (errors empty) — a parse error is a
            // backend-diagnostic concern, not a neutral-shape one, and the two engines word their
            // diagnostics differently.
            if !o.errors.is_empty() || !s.errors.is_empty() {
                continue;
            }
            if o == s {
                ok += 1;
            } else {
                diffs.push(path.to_string_lossy().into_owned());
            }
        }
        eprintln!(
            "parse-parity real corpus: {ok} OK / {} DIFF (of {} .ts files)",
            diffs.len(),
            sources.len()
        );
        assert!(
            diffs.is_empty(),
            "parse-parity DIFF on {} real-corpus file(s):\n{}",
            diffs.len(),
            diffs.join("\n")
        );
    }

    /// Recursively collect `.ts` files under `dir` (skipping the `.js` goldens + non-source files).
    fn collect_ts(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_ts(&path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("ts") {
                out.push(path);
            }
        }
    }

    /// A focused regression for the three member-shape parity details the real-corpus sweep surfaced:
    /// computed string-literal object keys (`{ ['class.x']: … }`) are captured (not dropped); a
    /// `constructor` is named `Some("constructor")`; and a stray `;` class member is discarded.
    #[test]
    fn parse_parity_member_shape_regressions() {
        // Computed string-literal host keys + a trailing-`;` constructor + a method decorator.
        assert_parity(
            r#"
@Component({ selector: 'c', template: '', host: { ['class.x']: 'false', ['style.w']: '0' } })
export class C {
  constructor(private a: A) {};
  @HostListener('click') onClick() {}
}
"#,
            SourceKind::TypeScriptModule,
        );
    }

    /// The NEW full-surface neutral nodes (constructor params + member initializers + decorator
    /// arguments + the import list + the `NExpr`/`NStmt` trees) are populated BYTE-IDENTICALLY by both
    /// backends. The whole `ParseOutput` (which now embeds all of them) derives `PartialEq`, so the
    /// shared `assert_parity` already covers every case in `SOURCE_CORPUS`; this test additionally
    /// asserts the richer fields are actually FILLED (not silently empty on one side) so the parity is
    /// meaningful, and pins the cross-engine shapes the later walk-switch depends on.
    #[test]
    fn parse_parity_full_surface_nodes() {
        use super::{MemberKind, NArg, NArrowBody, NExpr, NObjectProp, NStmt};

        // Constructor dependencies + parameter decorators + import bindings.
        let di = r#"
import { Component, Inject } from "@angular/core";
import type { Foo } from "./foo";
import Default, { Named as Aliased, type TypeOnly } from "./mixed";
@Component({ selector: "app-di", template: "" })
export class DiComponent {
  constructor(private a: A, @Inject(TOKEN) @Optional() public b: B) {}
}
"#;
        let o = oxc_summary(di, SourceKind::TypeScriptModule);
        let s = swc_summary(di, SourceKind::TypeScriptModule);
        assert_eq!(o, s, "full-surface DI parity");

        // Imports: 5 bindings, with the `import type` whole-decl + inline `type` flagged.
        let names: Vec<(&str, bool)> = o
            .imports
            .iter()
            .map(|i| (i.local_name.as_str(), i.type_only))
            .collect();
        assert_eq!(
            names,
            vec![
                ("Component", false),
                ("Inject", false),
                ("Foo", true),       // whole `import type`
                ("Default", false),
                ("Aliased", false),
                ("TypeOnly", true),  // inline `type`
            ],
            "import bindings + type-only flags"
        );

        // The constructor member carries both params with their decorators.
        let class = &o.classes[0];
        let ctor = class
            .members
            .iter()
            .find(|m| m.kind == MemberKind::Constructor)
            .expect("constructor member present");
        assert_eq!(ctor.params.len(), 2, "two ctor params");
        assert_eq!(ctor.params[0].name.as_deref(), Some("a"));
        assert!(ctor.params[0].decorators.is_empty());
        assert_eq!(ctor.params[1].name.as_deref(), Some("b"));
        let dec_names: Vec<&str> = ctor.params[1]
            .decorators
            .iter()
            .map(|d| d.name.as_str())
            .collect();
        assert_eq!(dec_names, vec!["Inject", "Optional"], "param decorators");
        // `@Inject(TOKEN)` argument captured as an identifier expression.
        match &ctor.params[1].decorators[0].arguments[..] {
            [NArg::Expr(NExpr::Identifier(tok), _)] => assert_eq!(tok, "TOKEN"),
            other => panic!("unexpected @Inject args: {other:?}"),
        }

        // Member initializers exercising the full NExpr surface.
        let sig = r#"
@Component({ selector: "app-sig", template: "" })
export class SigComponent {
  count = input(0);
  flag = a && b;
  cond = ready ? 1 : 0;
  list = [1, ...rest];
  cfg = { a: 1, ["b-c"]: 2, ...defaults };
  @Input({ transform: (v) => v == null ? 0 : f(v) }) value = 0;
  @Input({ transform: function (v) { const n = g(v); return n; } }) other = 0;
}
"#;
        let o = oxc_summary(sig, SourceKind::TypeScriptModule);
        let s = swc_summary(sig, SourceKind::TypeScriptModule);
        assert_eq!(o, s, "full-surface initializer parity");
        let m = &o.classes[0].members;
        let init = |name: &str| {
            m.iter()
                .find(|x| x.name.as_deref() == Some(name))
                .and_then(|x| x.initializer.as_ref())
                .unwrap_or_else(|| panic!("initializer for {name}"))
        };
        // `input(0)` — a call expression with one numeric arg.
        match init("count") {
            NExpr::Call { callee, args } => {
                assert!(matches!(&**callee, NExpr::Identifier(n) if n == "input"));
                assert!(matches!(args.as_slice(), [NArg::Expr(NExpr::Number(_), _)]));
            }
            other => panic!("count init: {other:?}"),
        }
        // `a && b` — logical operator folds into the unified Binary node with the `&&` spelling.
        match init("flag") {
            NExpr::Binary { op, .. } => assert_eq!(op, "&&"),
            other => panic!("flag init: {other:?}"),
        }
        // `ready ? 1 : 0` — conditional.
        assert!(matches!(init("cond"), NExpr::Conditional { .. }));
        // `[1, ...rest]` — array with a spread element.
        match init("list") {
            NExpr::Array(elems) => {
                assert!(matches!(elems[0], super::NArrayElement::Expr(NExpr::Number(_))));
                assert!(matches!(elems[1], super::NArrayElement::Spread(_)));
            }
            other => panic!("list init: {other:?}"),
        }
        // `{ a: 1, ['b-c']: 2, ...defaults }` — object with a computed string key + spread.
        match init("cfg") {
            NExpr::Object(props) => {
                assert!(matches!(&props[0], NObjectProp::KeyValue { key, computed, .. } if key == "a" && !*computed));
                assert!(matches!(&props[1], NObjectProp::KeyValue { key, quoted, computed, .. } if key == "b-c" && *quoted && *computed));
                assert!(matches!(&props[2], NObjectProp::Spread(_)));
            }
            other => panic!("cfg init: {other:?}"),
        }
        // The `transform` arrow `(v) => v == null ? 0 : f(v)` carried through the decorator's object.
        let value_dec = m
            .iter()
            .find(|x| x.name.as_deref() == Some("value"))
            .unwrap()
            .decorators[0]
            .object
            .as_ref()
            .unwrap();
        // The arrow is a richer-than-literal value; the structural `ObjLit` records it as `Other`, but
        // the decorator's full `arguments` carry the real `NExpr::Arrow`.
        let _ = value_dec;
        let value_args = &m
            .iter()
            .find(|x| x.name.as_deref() == Some("value"))
            .unwrap()
            .decorators[0]
            .arguments;
        match &value_args[..] {
            [NArg::Expr(NExpr::Object(props), _)] => {
                let transform = props.iter().find_map(|p| match p {
                    NObjectProp::KeyValue { key, value, .. } if key == "transform" => Some(value),
                    _ => None,
                });
                assert!(matches!(transform, Some(NExpr::Arrow { body, .. }) if matches!(&**body, NArrowBody::Expr(_))));
            }
            other => panic!("@Input value args: {other:?}"),
        }
        // The `function (v) { const n = g(v); return n; }` transform — a function body with a
        // var-decl statement + a return statement.
        let other_args = &m
            .iter()
            .find(|x| x.name.as_deref() == Some("other"))
            .unwrap()
            .decorators[0]
            .arguments;
        match &other_args[..] {
            [NArg::Expr(NExpr::Object(props), _)] => {
                let transform = props.iter().find_map(|p| match p {
                    NObjectProp::KeyValue { key, value, .. } if key == "transform" => Some(value),
                    _ => None,
                });
                match transform {
                    Some(NExpr::Function { body, .. }) => {
                        assert!(matches!(body[0], NStmt::VarDecl { is_const: true, .. }));
                        assert!(matches!(body[1], NStmt::Return(Some(_))));
                    }
                    other => panic!("other transform: {other:?}"),
                }
            }
            other => panic!("@Input other args: {other:?}"),
        }
    }

    /// The THREE new surfaces this phase adds are FILLED (not silently empty on one side) and
    /// byte-identical across the two backends, with the spans recovering the exact source slices:
    ///   * GAP1 SPANS — `ClassWithDecorators::{span, stmt_span}` / `DecoratorInfo::span` /
    ///     `MemberInfo::span` carry the decorator-strip + forward-ref anchor ranges;
    ///   * GAP2 TOP-LEVEL — `ParseOutput::top_level` carries the `X.ɵfac = …` / `X.ɵprov =
    ///     ɵɵdefineInjectable(…)` assignments + the `ɵɵsetNgModuleScope` call + the factory-body
    ///     `NExpr` tree (with `NArg` spans);
    ///   * GAP3 NGDECLARE VALUES — `ObjLit::nprops` carries each property's FULL `NExpr` value (the
    ///     lossless channel the linker walk needs) where `props` degrades a rich value to `LitValue::
    ///     Other`.
    #[test]
    fn parse_parity_phase_surfaces_filled() {
        // ---- GAP1: spans on the decorated class + its decorator + its members. ----
        let src = r#"@Component({ selector: "a", template: "" }) export class S {
  @Input() title = "hi";
  constructor(private a: A) {}
}"#;
        let o = oxc_summary(src, SourceKind::TypeScriptModule);
        let s = swc_summary(src, SourceKind::TypeScriptModule);
        assert_eq!(o, s, "GAP1 span parity");
        let class = &o.classes[0];
        // The decorator span recovers the exact `@Component({...})` decorator text.
        let dec_text = OxcParseBackend.span_text(src, class.decorators[0].span);
        assert_eq!(SwcParseBackend.span_text(src, class.decorators[0].span), dec_text);
        assert!(dec_text.starts_with("@Component(") && dec_text.ends_with(')'), "dec: {dec_text}");
        // The class span (exported → starts at `class`) and the enclosing statement span (`export …`)
        // are distinct + both non-empty.
        assert!(class.span.start > class.stmt_span.start, "exported class span starts after `export`");
        assert!(OxcParseBackend.span_text(src, class.span).starts_with("class S"));
        assert!(OxcParseBackend.span_text(src, class.stmt_span).starts_with("export class S"));
        // The member span recovers the `@Input() title = "hi";` member slice (decorator included).
        let title = class.members.iter().find(|m| m.name.as_deref() == Some("title")).unwrap();
        assert!(OxcParseBackend.span_text(src, title.span).starts_with("@Input() title"));
        assert_eq!(SwcParseBackend.span_text(src, title.span), OxcParseBackend.span_text(src, title.span));

        // ---- GAP2: the top-level assignment + setNgModuleScope surface. ----
        let aot = r#"
export class M {}
M.ɵfac = function M_Factory(t) { return new (t || M)(i0.ɵɵinject(Dep)); };
M.ɵprov = i0.ɵɵdefineInjectable({ token: M, factory: M.ɵfac, providedIn: "root" });
(typeof ngJitMode === "undefined" || ngJitMode) && i0.ɵɵsetNgModuleScope(M, { declarations: [A] });
"#;
        let o = oxc_summary(aot, SourceKind::TypeScriptEsModule);
        let s = swc_summary(aot, SourceKind::TypeScriptEsModule);
        assert_eq!(o, s, "GAP2 top-level parity");
        // The two `X.member = …` definition scaffolds are surfaced as assignments with their parts.
        let assigns: Vec<&NAssignment> = o
            .top_level
            .iter()
            .filter_map(|t| match t {
                NTopStmt::Assignment(a) => Some(a),
                _ => None,
            })
            .collect();
        assert_eq!(assigns.len(), 2, "two definition-scaffold assignments");
        assert_eq!(assigns[0].target_object.as_deref(), Some("M"));
        assert_eq!(assigns[0].target_member.as_deref(), Some("\u{0275}fac"));
        // The factory RHS is a function whose body new-expr arg is the `ɵɵinject(Dep)` call.
        match &assigns[0].value {
            NExpr::Function { body, .. } => {
                let has_inject = format!("{body:?}").contains("\u{0275}\u{0275}inject");
                assert!(has_inject, "factory body carries the ɵɵinject call");
            }
            other => panic!("ɵfac value: {other:?}"),
        }
        // The `ɵprov` RHS span recovers the verbatim `i0.ɵɵdefineInjectable({...})` source.
        let prov_text = OxcParseBackend.span_text(aot, assigns[1].value_span);
        assert_eq!(SwcParseBackend.span_text(aot, assigns[1].value_span), prov_text);
        assert!(prov_text.starts_with("i0.\u{0275}\u{0275}defineInjectable("), "prov: {prov_text}");
        // The `ɵɵsetNgModuleScope` side-effect call is surfaced as a top-level expression statement.
        let has_set_scope = o.top_level.iter().any(|t| {
            matches!(t, NTopStmt::ExprStmt { expr, .. }
                if format!("{expr:?}").contains("setNgModuleScope"))
        });
        assert!(has_set_scope, "ɵɵsetNgModuleScope surfaced as a top-level ExprStmt");

        // ---- GAP3: ObjLit::nprops carries a rich (arrow) value losslessly. ----
        let decl = r#"i0.ɵɵngDeclareInjectable({ type: X, providedIn: "root", useFactory: () => new X(dep), deps: [{ token: Dep }] });"#;
        let o = oxc_summary(decl, SourceKind::TypeScriptEsModule);
        let s = swc_summary(decl, SourceKind::TypeScriptEsModule);
        assert_eq!(o, s, "GAP3 ngDeclare value parity");
        let obj = &o.ng_declare_calls[0].object;
        // The lossy `props` channel degrades the `useFactory` arrow to `LitValue::Other`...
        let lossy = obj.props.iter().find(|(k, _)| k == "useFactory").map(|(_, v)| v);
        assert!(matches!(lossy, Some(LitValue::Other(_))), "props loses the arrow");
        // ...while the lossless `nprops` channel carries the real `NExpr::Arrow`.
        let rich = obj.nprops.iter().find_map(|p| match p {
            NObjectProp::KeyValue { key, value, value_span, .. } if key == "useFactory" => {
                Some((value, *value_span))
            }
            _ => None,
        });
        let (rich_value, value_span) = rich.expect("nprops carries useFactory");
        assert!(matches!(rich_value, NExpr::Arrow { .. }), "nprops keeps the arrow: {rich_value:?}");
        // The value span recovers the verbatim arrow source on both backends.
        let arrow_text = OxcParseBackend.span_text(decl, value_span);
        assert_eq!(SwcParseBackend.span_text(decl, value_span), arrow_text);
        assert_eq!(arrow_text, "() => new X(dep)");
    }

    /// `span_text` recovers the IDENTICAL source slice on both backends for a captured object span.
    #[test]
    fn parse_parity_span_text() {
        let src = r#"@Component({ selector: "app-span" }) class S {}"#;
        let o_span = oxc_summary(src, SourceKind::TypeScriptModule).classes[0].decorators[0]
            .object
            .as_ref()
            .unwrap()
            .span;
        let s_span = swc_summary(src, SourceKind::TypeScriptModule).classes[0].decorators[0]
            .object
            .as_ref()
            .unwrap()
            .span;
        assert_eq!(o_span, s_span, "object spans must match");
        let o_text = OxcParseBackend.span_text(src, o_span);
        let s_text = SwcParseBackend.span_text(src, s_span);
        assert_eq!(o_text, s_text, "span_text must recover identical slices");
        assert!(o_text.starts_with('{') && o_text.ends_with('}'), "got: {o_text}");
    }
}
