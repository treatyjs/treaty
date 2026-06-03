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

/// An engine-neutral byte range `[start, end)` into the original source. Absolute byte offsets on the
/// oxc backend; recover the covered text via [`ParseBackend::span_text`] rather than slicing directly
/// (the swc backend's `BytePos` are `SourceMap`-relative).
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

/// A pre-lowered Angular DECORATOR on a class: its callee name and (when called with an object
/// literal) the pre-lowered object argument.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DecoratorInfo {
    /// The callee identifier — `Component`, `Directive`, `Pipe`, `NgModule`, `Injectable`, … — for
    /// both the bare `@Foo` and the call `@Foo({…})` forms.
    pub name: String,
    /// The first object-literal argument of `@Foo({…})`, pre-lowered. `None` for a bare `@Foo`.
    pub object: Option<ObjLit>,
}

/// A pre-lowered class MEMBER (property or accessor) carrying enough shape for the parts of the walk
/// that read members structurally (member name + its own decorators). The body/initializer detail the
/// signal/host/query extraction needs is read against the live AST in the `oxc` backend.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemberInfo {
    /// The member's name, when it is a statically-known identifier/string key.
    pub name: Option<String>,
    /// The member's own decorators (`@Input()`, `@Output()`, `@HostBinding(...)`, …), pre-lowered.
    pub decorators: Vec<DecoratorInfo>,
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

/// A pre-lowered `ɵɵngDeclare*({…})` call discovered in a partial-declaration module: which kind of
/// declaration it is plus its pre-lowered object argument.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NgDeclareCall {
    /// The `ɵɵngDeclare*` callee suffix (`Component`, `Directive`, `Factory`, `Injectable`, …).
    pub kind: String,
    /// The declaration object argument, pre-lowered.
    pub object: ObjLit,
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
// and the `treaty_ivy_decorators::ClassMeta<'a>` API are oxc-typed. Flipping the alias to the swc
// backend is gated on first neutralizing those walks (SWC-BACKEND-PLAN.md §3.2 phase 3 — the wide
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
    use super::{ParseBackend, ParseOutput, SourceKind};

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
    ];

    /// The representative partial-declaration corpus: `ɵɵngDeclare*` calls in the shapes the linker
    /// reads — bare callee, namespaced callee, as a free statement, and as a `static` class member.
    const NG_DECLARE_CORPUS: &[&str] = &[
        r#"ɵɵngDeclareComponent({ minVersion: "12.0.0", version: "22.1.0", type: X, selector: "app-x" });"#,
        r#"i0.ɵɵngDeclareDirective({ version: "22.1.0", type: Y, selector: "[appY]" });"#,
        r#"const f = i0.ɵɵngDeclareFactory({ type: Z, deps: [], target: 0 });"#,
        r#"class W { static ɵcmp = i0.ɵɵngDeclareComponent({ type: W, selector: "app-w", template: "<p></p>" }); }"#,
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
