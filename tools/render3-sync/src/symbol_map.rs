//! The SYMBOL -> MODULE map: the maintained, static table mapping Angular
//! `packages/compiler` TypeScript source files (and their exported symbols) to the Rust
//! modules under `libs/treaty-ivy/*` that port them.
//!
//! This is pillar 1 of the harness (see `tools/render3-sync/README.md`): when Angular drifts, a
//! changed TS file/symbol is resolved through this table to the precise Rust file a human must
//! touch. It is intentionally a hand-maintained, deterministic constant — NO AI is involved in
//! producing or consulting it.
//!
//! The former single-crate `libs/render3` port was carved into four workspace crates under
//! `libs/treaty-ivy/{core,template,decorators,facade}`; each `rust_file` below is therefore a path
//! relative to [`TREATY_IVY_SRC_ROOT`] that includes its owning crate's `src/` prefix
//! (e.g. `core/src/identifiers.rs`).
//!
//! Paths are kept relative to their respective roots:
//!   * `ts_file`  is relative to `tools/angular-ref/packages/compiler/src/`
//!   * `rust_file` is relative to `libs/treaty-ivy/`
//!
//! Every TS file under the Angular compiler src root must be either MAPPED (a row in
//! [`MODULE_MAP`]) or explicitly OUT OF SCOPE ([`OUT_OF_SCOPE_FILES`], with a reason). `drift`
//! reports any file that is neither, so a new Angular file can never be silently missed.
//!
//! Symbol attribution: a row's `anchor_symbols` name the exports it owns. An export is attributed
//! to the rows of ITS OWN FILE whose anchors list it ([`owners_for_export`]); anchors are never
//! matched across files (two files may both export a `Type` or a `TokenType`). An export no anchor
//! names falls back to the file's un-anchored rows. A file with several rows must anchor every
//! export (enforced by a test), so no export fans out to unrelated owners.

/// Root, relative to the Treaty repo, of the vendored Angular compiler sources this harness
/// diffs against.
pub const ANGULAR_COMPILER_SRC_ROOT: &str = "tools/angular-ref/packages/compiler/src";

/// Root, relative to the Treaty repo, of the Rust port the harness keeps 1:1. The port now spans
/// four crates under `libs/treaty-ivy`, so each `rust_file` carries its crate's `src/` prefix.
pub const TREATY_IVY_SRC_ROOT: &str = "libs/treaty-ivy";

/// Confidence that the Rust module is a *mechanical* (value/name table) port of the TS, and
/// therefore a candidate for pillar-3 oxc codegen, versus hand-ported logic that drift only
/// flags for manual re-porting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PortKind {
    /// Pure data table (consts / enums / numeric flags / name maps) — safe to auto-codegen and
    /// diff against the committed Rust.
    Mechanical,
    /// Hand-ported logic (control flow, ownership) — drift is reported as a manual port task,
    /// never auto-transpiled.
    Logic,
}

/// One row of the symbol->module map: an Angular TS source file and the Rust module that ports
/// it, plus the kind of port and the (optional) per-module spec.
// `ModuleMapping` is a `'static` const table the harness only ever SERIALIZES (e.g. `map`
// subcommand). It is not deserialized — its borrowed `&'static [&'static str]` field has no
// owned `Deserialize` impl — so it derives `Serialize` only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct ModuleMapping {
    /// TS source path, relative to [`ANGULAR_COMPILER_SRC_ROOT`].
    pub ts_file: &'static str,
    /// Rust port path, relative to [`TREATY_IVY_SRC_ROOT`] (includes the owning crate's `src/`
    /// prefix, e.g. `core/src/identifiers.rs`).
    pub rust_file: &'static str,
    /// Whether this is a mechanical table (codegen candidate) or hand-ported logic.
    pub kind: PortKind,
    /// Per-module port spec, if one exists.
    pub spec: Option<&'static str>,
    /// Exports of `ts_file` owned by this row. Matching is per file (see module docs). Empty means
    /// the row is the file-level owner of every export the file's anchored rows do not claim.
    pub anchor_symbols: &'static [&'static str],
    /// `Some(reason)` when the Rust file is not yet in the state this row needs because another
    /// developer is still porting it (or it is planned and not yet created). `codegen-verify` then
    /// reports the row as PENDING instead of failing, and fails the build as soon as the row
    /// verifies in sync (the marker is stale and must be removed).
    pub pending: Option<&'static str>,
}

/// An Angular file (or every file under a directory, when `pattern` ends with `/`) that Treaty
/// deliberately does not port. `reason` is a one-line justification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct OutOfScopeFile {
    /// Path relative to [`ANGULAR_COMPILER_SRC_ROOT`]; a trailing `/` matches a whole directory.
    pub pattern: &'static str,
    /// Why Treaty does not port this file.
    pub reason: &'static str,
}

/// An individual export of a MAPPED file that Treaty deliberately does not port. Drift ignores
/// changes to it, so it never produces an `UpdateMap` task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct OutOfScopeSymbol {
    /// TS file, relative to [`ANGULAR_COMPILER_SRC_ROOT`].
    pub ts_file: &'static str,
    /// The exported name.
    pub symbol: &'static str,
    /// Why Treaty does not port this export.
    pub reason: &'static str,
}

/// Classification of one TS file under [`ANGULAR_COMPILER_SRC_ROOT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileClass {
    /// Has at least one [`MODULE_MAP`] row.
    Mapped,
    /// Matched an [`OUT_OF_SCOPE_FILES`] entry; carries its reason.
    OutOfScope(&'static str),
    /// Neither mapped nor out of scope: drift reports it until someone classifies it.
    Unclassified,
}

/// The out-of-scope list: Angular compiler files Treaty deliberately does not port. Every entry
/// is checked by a test to match at least one real file, so a removed or renamed Angular file
/// cannot leave a stale entry behind.
pub const OUT_OF_SCOPE_FILES: &[OutOfScopeFile] = &[
    // Language service and type-check code: Treaty ships neither an IDE service nor ngtsc diagnostics.
    OutOfScopeFile {
        pattern: "indexer/",
        reason: "language-service template indexer (IDE tooling); Treaty ships no language service",
    },
    OutOfScopeFile {
        pattern: "typecheck/",
        reason: "ngtsc type-check block generation (diagnostics only); Treaty does not type-check templates",
    },
    OutOfScopeFile {
        pattern: "resource_loader.ts",
        reason: "runtime/JIT resource loading abstraction; Treaty resolves templateUrl/styleUrls at build time",
    },
    OutOfScopeFile {
        pattern: "output/output_jit.ts",
        reason: "JIT evaluation of the emitted AST at runtime (new Function); Treaty is AOT-only",
    },
    OutOfScopeFile {
        pattern: "output/output_jit_trusted_types.ts",
        reason: "JIT trusted-types wrapper around the JIT evaluator; AOT-only",
    },
    OutOfScopeFile {
        pattern: "render3/r3_jit.ts",
        reason: "JIT reflector for runtime compilation; AOT-only",
    },
    OutOfScopeFile {
        pattern: "render3/r3_hmr_compiler.ts",
        reason: "HMR initializer for the dev server only; not part of the AOT compile",
    },
    OutOfScopeFile {
        pattern: "render3/r3_class_debug_info_compiler.ts",
        reason: "dev-mode ɵsetClassDebugInfo emission; Treaty emits no class debug info",
    },
    OutOfScopeFile {
        pattern: "legacy_optional_chaining_default.ts",
        reason: "3p-only compatibility flag (= false); no Treaty output depends on it",
    },
    OutOfScopeFile {
        pattern: "version.ts",
        reason: "package version placeholder; Treaty versions itself",
    },
    OutOfScopeFile {
        pattern: "util.ts",
        reason: "root string/regex helpers; the Rust port uses std equivalents, so there is no 1:1 module",
    },
    OutOfScopeFile {
        pattern: "i18n/index.ts",
        reason: "barrel re-export file with no code of its own",
    },
    OutOfScopeFile {
        pattern: "i18n/extractor_merger.ts",
        reason: "ng extract-i18n tooling (merges extracted messages); not on the compile path",
    },
    OutOfScopeFile {
        pattern: "i18n/message_bundle.ts",
        reason: "ng extract-i18n message bundle; tooling only",
    },
    OutOfScopeFile {
        pattern: "i18n/translation_bundle.ts",
        reason: "translation loading for ng build --localize; tooling only",
    },
    OutOfScopeFile {
        pattern: "i18n/serializers/serializer.ts",
        reason: "abstract serializer for the xliff/xmb/xtb file formats; tooling only",
    },
    OutOfScopeFile {
        pattern: "i18n/serializers/xliff.ts",
        reason: "xliff 1.2 translation file format; tooling only",
    },
    OutOfScopeFile {
        pattern: "i18n/serializers/xliff2.ts",
        reason: "xliff 2.0 translation file format; tooling only",
    },
    OutOfScopeFile {
        pattern: "i18n/serializers/xmb.ts",
        reason: "xmb translation file format; tooling only",
    },
    OutOfScopeFile {
        pattern: "i18n/serializers/xtb.ts",
        reason: "xtb translation file format; tooling only",
    },
    OutOfScopeFile {
        pattern: "i18n/serializers/xml_helper.ts",
        reason: "XML helper for the xliff/xmb/xtb formats; tooling only",
    },
    OutOfScopeFile {
        pattern: "ml_parser/xml_parser.ts",
        reason: "XML parser used only by the xliff/xtb translation formats; tooling only",
    },
    OutOfScopeFile {
        pattern: "ml_parser/xml_tags.ts",
        reason: "XML tag table used only by the xliff/xtb translation formats; tooling only",
    },
];

/// Individual exports of mapped files that Treaty deliberately does not port.
pub const OUT_OF_SCOPE_SYMBOLS: &[OutOfScopeSymbol] = &[
    OutOfScopeSymbol {
        ts_file: "core.ts",
        symbol: "Type",
        reason: "runtime Function-typed alias (`export const Type = Function`); the Rust port has no counterpart",
    },
    OutOfScopeSymbol {
        ts_file: "core.ts",
        symbol: "MissingTranslationStrategy",
        reason: "i18n missing-translation policy read by the runtime/JIT; Treaty's compile path never reads it",
    },
];

/// The static, maintained symbol->module map. One Rust module may be fed by several TS files
/// (e.g. `ml_parser.rs`, `i18n.rs`), and one TS file may feed several Rust modules (`core.ts`, whose
/// enums live in different crates, is split into one row per export).
pub const MODULE_MAP: &[ModuleMapping] = &[
    // ---- L0 foundation -----------------------------------------------------------------
    ModuleMapping {
        ts_file: "render3/r3_identifiers.ts",
        rust_file: "core/src/identifiers.rs",
        kind: PortKind::Mechanical,
        spec: Some("migration/render3-specs/14-identifiers.md"),
        anchor_symbols: &["Identifiers"],
        pending: None,
    },
    ModuleMapping {
        ts_file: "output/output_ast.ts",
        rust_file: "core/src/output_ast.rs",
        kind: PortKind::Logic,
        spec: Some("migration/render3-specs/01-output_ast.md"),
        anchor_symbols: &[
            "Expression",
            "Statement",
            "Type",
            "BuiltinType",
            "BinaryOperator",
        ],
        pending: None,
    },
    ModuleMapping {
        ts_file: "render3/util.ts",
        rust_file: "core/src/util.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["R3Reference"],
        pending: None,
    },
    // ---- expression parser -------------------------------------------------------------
    ModuleMapping {
        ts_file: "expression_parser/lexer.ts",
        rust_file: "core/src/expression/lexer.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["Lexer", "Token", "TokenType"],
        pending: None,
    },
    ModuleMapping {
        ts_file: "expression_parser/ast.ts",
        rust_file: "core/src/expression/ast.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["AST", "ASTWithSource", "Binary", "PropertyRead"],
        pending: None,
    },
    ModuleMapping {
        ts_file: "expression_parser/parser.ts",
        rust_file: "core/src/expression/parser.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["Parser"],
        pending: None,
    },
    // ---- output emitter ----------------------------------------------------------------
    // The Rust emitter replaces Angular's hand-rolled abstract emitters with oxc codegen, so it
    // is fed by both abstract_emitter.ts and abstract_js_emitter.ts.
    ModuleMapping {
        ts_file: "output/abstract_emitter.ts",
        rust_file: "core/src/output/emitter.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["AbstractEmitterVisitor", "EmitterVisitorContext"],
        pending: None,
    },
    ModuleMapping {
        ts_file: "output/abstract_js_emitter.ts",
        rust_file: "core/src/output/emitter.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["AbstractJsEmitterVisitor"],
        pending: None,
    },
    // ---- r3 template AST + transform ---------------------------------------------------
    ModuleMapping {
        ts_file: "render3/r3_ast.ts",
        rust_file: "template/src/template/r3_ast.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["Element", "Template", "BoundText", "Node"],
        pending: None,
    },
    ModuleMapping {
        ts_file: "render3/r3_template_transform.ts",
        rust_file: "template/src/template/template_transform.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["htmlAstToRender3Ast", "Render3ParseResult"],
        pending: None,
    },
    ModuleMapping {
        ts_file: "render3/r3_control_flow.ts",
        rust_file: "template/src/template/control_flow.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["createIfBlock", "createForLoop", "createSwitchBlock"],
        pending: None,
    },
    ModuleMapping {
        ts_file: "render3/r3_deferred_blocks.ts",
        rust_file: "template/src/template/deferred.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["createDeferredBlock"],
        pending: None,
    },
    ModuleMapping {
        ts_file: "render3/r3_deferred_triggers.ts",
        rust_file: "template/src/template/deferred.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["parseDeferredTime", "parseWhenTrigger", "parseOnTrigger"],
        pending: None,
    },
    // ---- view (compiler / template instructions / queries / binder / config) -----------
    ModuleMapping {
        ts_file: "render3/view/compiler.ts",
        rust_file: "decorators/src/compiler.rs",
        kind: PortKind::Logic,
        spec: Some("migration/render3-specs/11-view_compiler.md"),
        anchor_symbols: &[
            "compileComponentFromMetadata",
            "compileDirectiveFromMetadata",
        ],
        pending: None,
    },
    ModuleMapping {
        ts_file: "render3/view/template.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["parseTemplate", "ParsedTemplate"],
        pending: None,
    },
    ModuleMapping {
        ts_file: "render3/view/query_generation.ts",
        rust_file: "template/src/view/queries.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["createViewQueriesFunction", "createContentQueriesFunction"],
        pending: None,
    },
    ModuleMapping {
        ts_file: "render3/view/t2_binder.ts",
        rust_file: "template/src/binder.rs",
        kind: PortKind::Logic,
        spec: Some("migration/render3-specs/10-t2_binder.md"),
        anchor_symbols: &[
            "R3TargetBinder",
            "DirectiveMatcher",
            "findMatchingDirectivesAndPipes",
        ],
        pending: None,
    },
    ModuleMapping {
        ts_file: "render3/view/t2_api.ts",
        rust_file: "template/src/binder.rs",
        kind: PortKind::Logic,
        spec: Some("migration/render3-specs/10-t2_binder.md"),
        anchor_symbols: &["Target", "BoundTarget", "DirectiveMeta"],
        pending: None,
    },
    // ---- factory / pipe / module / injector codegen ------------------------------------
    ModuleMapping {
        ts_file: "render3/r3_factory.ts",
        rust_file: "core/src/factory.rs",
        kind: PortKind::Logic,
        spec: Some("migration/render3-specs/15-factory.md"),
        anchor_symbols: &["compileFactoryFunction", "R3FactoryMetadata"],
        pending: None,
    },
    ModuleMapping {
        ts_file: "render3/r3_pipe_compiler.ts",
        rust_file: "decorators/src/pipe_module_injector.rs",
        kind: PortKind::Logic,
        spec: Some("migration/render3-specs/16-pipe_module_injector.md"),
        anchor_symbols: &["compilePipeFromMetadata", "createPipeType"],
        pending: None,
    },
    ModuleMapping {
        ts_file: "render3/r3_module_compiler.ts",
        rust_file: "decorators/src/pipe_module_injector.rs",
        kind: PortKind::Logic,
        spec: Some("migration/render3-specs/16-pipe_module_injector.md"),
        anchor_symbols: &["compileNgModule", "createNgModuleType"],
        pending: None,
    },
    ModuleMapping {
        ts_file: "render3/r3_injector_compiler.ts",
        rust_file: "decorators/src/pipe_module_injector.rs",
        kind: PortKind::Logic,
        spec: Some("migration/render3-specs/16-pipe_module_injector.md"),
        anchor_symbols: &["compileInjector", "R3InjectorMetadata"],
        pending: None,
    },
    // ---- expression converter ----------------------------------------------------------
    // Angular v22 retired the historic `compiler_util/expression_converter.ts`; the
    // `e.AST -> o.Expression` lowering it used to own now lives in the template *pipeline*:
    // `convertAst` (the per-node lowering) in `template/pipeline/src/ingest.ts` and the binary
    // operator table `BINARY_OPERATORS` in `template/pipeline/src/conversion.ts`. The Rust
    // `expression_converter.rs` reproduces the classic `convertPropertyBinding` behaviour from
    // those two pipeline sources (see its `PORT TARGET:` header).
    ModuleMapping {
        ts_file: "template/pipeline/src/ingest.ts",
        rust_file: "core/src/expression_converter.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["ingestComponent", "ingestHostBinding"],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/conversion.ts",
        rust_file: "core/src/expression_converter.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["BINARY_OPERATORS", "literalOrArrayLiteral"],
        pending: None,
    },
    // ---- ml_parser (HTML front-end) ----------------------------------------------------
    // The Rust `ml_parser.rs` header lists lexer, parser, ast, html_parser, tags, html_tags and
    // tokens (the HTML token types are its own `TokenType`, not the expression lexer's).
    ModuleMapping {
        ts_file: "ml_parser/lexer.ts",
        rust_file: "template/src/ml_parser.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["tokenize", "TokenizeResult"],
        pending: None,
    },
    ModuleMapping {
        ts_file: "ml_parser/parser.ts",
        rust_file: "template/src/ml_parser.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["Parser", "ParseTreeResult"],
        pending: None,
    },
    ModuleMapping {
        ts_file: "ml_parser/ast.ts",
        rust_file: "template/src/ml_parser.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["Element", "Attribute", "Text", "Node"],
        pending: None,
    },
    ModuleMapping {
        ts_file: "ml_parser/html_parser.ts",
        rust_file: "template/src/ml_parser.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["HtmlParser"],
        pending: None,
    },
    ModuleMapping {
        ts_file: "ml_parser/tags.ts",
        rust_file: "template/src/ml_parser.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["TagContentType", "getNsPrefix"],
        pending: None,
    },
    // html_tags.ts is a class with instance methods and a function body (`getHtmlTagDefinition`
    // builds a cache), so it is hand-ported logic, not a data table.
    ModuleMapping {
        ts_file: "ml_parser/html_tags.ts",
        rust_file: "template/src/ml_parser.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["HtmlTagDefinition", "getHtmlTagDefinition"],
        pending: None,
    },
    // tokens.ts defines the HTML lexer's `TokenType` / `Token`; the Rust home is ml_parser.rs.
    ModuleMapping {
        ts_file: "ml_parser/tokens.ts",
        rust_file: "template/src/ml_parser.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["TokenType", "Token"],
        pending: None,
    },
    // ---- i18n core ---------------------------------------------------------------------
    ModuleMapping {
        ts_file: "i18n/digest.ts",
        rust_file: "core/src/digest.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["computeMsgId", "fingerprint", "decimalDigest"],
        pending: None,
    },
    ModuleMapping {
        ts_file: "i18n/i18n_ast.ts",
        rust_file: "template/src/i18n.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["Message", "Node", "Container", "Icu", "Placeholder"],
        pending: None,
    },
    // ---- end-to-end pipeline glue + source front-end -----------------------------------
    // compile.rs / source_compile.rs are Treaty-side glue (no single 1:1 Angular source); they
    // are driven by the view compiler + a oxc-based @Component/@Directive source front-end.
    ModuleMapping {
        ts_file: "render3/view/api.ts",
        rust_file: "facade/src/compile.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["R3ComponentMetadata", "R3DirectiveMetadata"],
        pending: None,
    },
    // config.ts is one flag, `ENABLE_TEMPLATE_SOURCE_LOCATIONS = false`, plus two setters. Treaty
    // emits no source-location instructions, so the flag has no Rust consumer yet. The row is a
    // pointer: a change to the file is a manual task against the view compiler (decorators).
    // It is Logic, not Mechanical, because the TS is functions, not a literal table.
    ModuleMapping {
        ts_file: "render3/view/config.ts",
        rust_file: "decorators/src/compiler.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["ENABLE_TEMPLATE_SOURCE_LOCATIONS"],
        pending: None,
    },
    ModuleMapping {
        ts_file: "jit_compiler_facade.ts",
        rust_file: "facade/src/source_compile.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["CompilerFacadeImpl"],
        pending: None,
    },
    // ---- core.ts: one row per export, each pointing at the Rust file that really owns it -------
    // core.ts is the canonical home of the runtime numeric flags and the schema/selector types. The
    // Rust port keeps each one where it is used, so the owner is per symbol (verified by grep of
    // the committed sources). Symbols whose Rust shape is a hand-written struct/newtype or a
    // `mod` of consts are Logic rows; a row is Mechanical only when the emitter's output shape
    // matches the committed Rust item.
    ModuleMapping {
        ts_file: "core.ts",
        rust_file: "decorators/src/compiler.rs",
        kind: PortKind::Mechanical,
        spec: None,
        anchor_symbols: &["ViewEncapsulation"],
        pending: Some(
            "PRE-EXISTING gap, not caused by 78d2a67 (core.ts is identical at the old and new ref): \
             decorators/src/compiler.rs ViewEncapsulation lacks the ExperimentalIsolatedShadowDom = 4 \
             member that core.ts declares. Owner: the decorators port. Remove this marker when it lands.",
        ),
    },
    ModuleMapping {
        ts_file: "core.ts",
        rust_file: "decorators/src/compiler.rs",
        kind: PortKind::Mechanical,
        spec: None,
        anchor_symbols: &["ChangeDetectionStrategy"],
        pending: Some(
            "PRE-EXISTING gap, not caused by 78d2a67 (core.ts is identical at the old and new ref): \
             decorators/src/compiler.rs ChangeDetectionStrategy lacks the Eager = 1 alias that core.ts \
             declares. Owner: the decorators port. Remove this marker when it lands.",
        ),
    },
    ModuleMapping {
        ts_file: "core.ts",
        rust_file: "template/src/view/queries.rs",
        kind: PortKind::Mechanical,
        spec: None,
        anchor_symbols: &["RenderFlags"],
        pending: None,
    },
    ModuleMapping {
        ts_file: "core.ts",
        rust_file: "decorators/src/compiler.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["InputFlags"],
        pending: None,
    },
    // AttributeMarker and SelectorFlags are PARTIAL ports: decorators/src/compiler.rs carries only
    // the Classes (1) / Styles (2) markers used by static host attributes, and the SelectorFlags
    // bits as a private consts module. The template crate keeps its own SelectorFlags consts.
    ModuleMapping {
        ts_file: "core.ts",
        rust_file: "decorators/src/compiler.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["AttributeMarker", "SelectorFlags"],
        pending: None,
    },
    ModuleMapping {
        ts_file: "core.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[
            "AttributeMarker",
            "SelectorFlags",
            "R3CssSelector",
            "R3CssSelectorList",
            "parseSelectorToR3Selector",
        ],
        pending: None,
    },
    ModuleMapping {
        ts_file: "core.ts",
        rust_file: "core/src/util.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["InjectFlags"],
        pending: None,
    },
    ModuleMapping {
        ts_file: "core.ts",
        rust_file: "core/src/neutral.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["Input", "Output", "HostBinding", "HostListener"],
        pending: None,
    },
    ModuleMapping {
        ts_file: "core.ts",
        rust_file: "facade/src/source_compile.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[
            "SchemaMetadata",
            "CUSTOM_ELEMENTS_SCHEMA",
            "NO_ERRORS_SCHEMA",
        ],
        pending: None,
    },
    ModuleMapping {
        ts_file: "core.ts",
        rust_file: "core/src/expression/ast.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["SecurityContext"],
        pending: None,
    },
    ModuleMapping {
        ts_file: "core.ts",
        rust_file: "facade/src/partial_component_emit.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["emitDistinctChangesOnlyDefaultValue"],
        pending: None,
    },
    // ---- classified by the mapping sweep (every other Angular compiler file) ----------------
    // Rows below were added so that every TS file under ANGULAR_COMPILER_SRC_ROOT is either mapped
    // here or listed in OUT_OF_SCOPE_FILES. Pipeline phases with no more specific home are planned
    // to be ported inside `template/src/view/template.rs`; `render3/r3_boundaries.ts` and
    // `render3/r3_content_blocks.ts` are planned for the new `template/src/template/boundary.rs`.
    ModuleMapping {
        ts_file: "parse_util.ts",
        rust_file: "core/src/expression/ast.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/generate_arrow_functions.ts",
        rust_file: "core/src/expression_converter.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/pipe_variadic.ts",
        rust_file: "core/src/expression_converter.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/pure_function_extraction.ts",
        rust_file: "core/src/expression_converter.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/pure_literal_structures.ts",
        rust_file: "core/src/expression_converter.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/reify.ts",
        rust_file: "core/src/expression_converter.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/var_counting.ts",
        rust_file: "core/src/expression_converter.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "injectable_compiler_2.ts",
        rust_file: "core/src/factory.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "service_compiler.ts",
        rust_file: "core/src/factory.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "output/source_map.ts",
        rust_file: "core/src/output/source_map.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "output/map_util.ts",
        rust_file: "core/src/output_ast.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "compiler_facade_interface.ts",
        rust_file: "core/src/util.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "constant_pool.ts",
        rust_file: "decorators/src/compiler.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "render3/view/util.ts",
        rust_file: "decorators/src/compiler.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["DefinitionMap"],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/naming.ts",
        rust_file: "decorators/src/compiler.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/parse_extracted_styles.ts",
        rust_file: "decorators/src/compiler.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "render3/r3_class_metadata_compiler.ts",
        rust_file: "decorators/src/pipe_module_injector.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "shadow_css.ts",
        rust_file: "decorators/src/shadow_css.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "compiler.ts",
        rust_file: "facade/src/compile.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "config.ts",
        rust_file: "facade/src/compile.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "render3/partial/class_metadata.ts",
        rust_file: "facade/src/partial_class_metadata.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "render3/partial/component.ts",
        rust_file: "facade/src/partial_component_emit.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "render3/partial/api.ts",
        rust_file: "facade/src/partial_emit.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "render3/partial/directive.ts",
        rust_file: "facade/src/partial_emit.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "render3/partial/factory.ts",
        rust_file: "facade/src/partial_emit.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "render3/partial/injectable.ts",
        rust_file: "facade/src/partial_emit.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "render3/partial/injector.ts",
        rust_file: "facade/src/partial_emit.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "render3/partial/ng_module.ts",
        rust_file: "facade/src/partial_emit.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "render3/partial/pipe.ts",
        rust_file: "facade/src/partial_emit.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "render3/partial/service.ts",
        rust_file: "facade/src/partial_emit.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "render3/partial/util.ts",
        rust_file: "facade/src/partial_emit.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "combined_visitor.ts",
        rust_file: "template/src/binder.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "property_mapping.ts",
        rust_file: "template/src/binder.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "schema/dom_element_schema_registry.ts",
        rust_file: "template/src/binder.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "schema/dom_security_schema.ts",
        rust_file: "template/src/binder.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "schema/element_schema_registry.ts",
        rust_file: "template/src/binder.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "schema/trusted_types_sinks.ts",
        rust_file: "template/src/binder.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "expression_parser/serializer.ts",
        rust_file: "template/src/i18n.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "i18n/i18n_html_parser.ts",
        rust_file: "template/src/i18n.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "i18n/i18n_parser.ts",
        rust_file: "template/src/i18n.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "i18n/serializers/placeholder.ts",
        rust_file: "template/src/i18n.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "render3/view/i18n/get_msg_utils.ts",
        rust_file: "template/src/i18n.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "render3/view/i18n/icu_serializer.ts",
        rust_file: "template/src/i18n.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "render3/view/i18n/localize_utils.ts",
        rust_file: "template/src/i18n.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "render3/view/i18n/meta.ts",
        rust_file: "template/src/i18n.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "render3/view/i18n/util.ts",
        rust_file: "template/src/i18n.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "chars.ts",
        rust_file: "template/src/ml_parser.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "ml_parser/entities.ts",
        rust_file: "template/src/ml_parser.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "render3/r3_boundaries.ts",
        rust_file: "template/src/template/boundary.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: Some("planned new file template/src/template/boundary.rs; not created yet"),
    },
    ModuleMapping {
        ts_file: "render3/r3_content_blocks.ts",
        rust_file: "template/src/template/boundary.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: Some("planned new file template/src/template/boundary.rs; not created yet"),
    },
    ModuleMapping {
        ts_file: "ml_parser/html_whitespaces.ts",
        rust_file: "template/src/template/template_transform.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "style_url_resolver.ts",
        rust_file: "template/src/template/template_transform.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template_parser/binding_parser.ts",
        rust_file: "template/src/template/template_transform.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template_parser/template_preparser.ts",
        rust_file: "template/src/template/template_transform.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "directive_matching.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/ir/index.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/ir/src/enums.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/ir/src/expression.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/ir/src/handle.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/ir/src/operations.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/ir/src/ops/create.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/ir/src/ops/host.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/ir/src/ops/shared.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/ir/src/ops/update.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/ir/src/traits.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/ir/src/variable.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/compilation.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/emit.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/instruction.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/namespaces.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/any_cast.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/apply_i18n_expressions.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/assign_i18n_slot_dependencies.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/attach_source_locations.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/attribute_extraction.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/binding_specialization.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/boundary_conditions.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/chaining.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/collapse_singleton_interpolations.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/conditionals.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/const_collection.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/control_directives.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/convert_animations.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/convert_i18n_bindings.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/create_i18n_contexts.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/deduplicate_text_bindings.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/defer_configs.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/defer_resolve_targets.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/empty_elements.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/expand_safe_reads.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/extract_i18n_messages.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/generate_advance.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/generate_local_let_references.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/generate_projection_def.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/generate_variables.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/has_const_expression_collection.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/host_style_property_parsing.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/i18n_const_collection.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/i18n_text_extraction.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/insert_incremental_hydration_runtime.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/local_refs.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/namespace.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/next_context_merging.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/ng_container.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/nonbindable.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/ordering.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/phase_remove_content_selectors.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/pipe_creation.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/propagate_i18n_blocks.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/regular_expression_optimization.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/remove_empty_bindings.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/remove_i18n_contexts.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/remove_illegal_let_references.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/remove_unused_i18n_attrs.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/resolve_boundaries.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/resolve_contexts.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/resolve_defer_deps_fns.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/resolve_dollar_event.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/resolve_foreign_content.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/resolve_i18n_attr_sanitizers.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/resolve_i18n_element_placeholders.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/resolve_i18n_expression_placeholders.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/resolve_names.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/resolve_sanitizers.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/safe_navigation_migration.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/save_restore_view.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/slot_allocation.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/store_let_optimization.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/strip_nonrequired_parentheses.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/style_binding_specialization.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/temporary_variables.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/track_fn_optimization.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/track_variables.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/transform_two_way_binding_set.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/variable_optimization.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/phases/wrap_icus.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/util/attributes.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/util/elements.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &[],
        pending: None,
    },
];

/// Resolve a TS file (path relative to [`ANGULAR_COMPILER_SRC_ROOT`]) to every row that ports it.
/// Returns every matching row, since one TS file can map to several Rust files and several TS
/// files can fan into one Rust file.
pub fn rust_modules_for_ts(ts_file: &str) -> Vec<&'static ModuleMapping> {
    let norm = normalize(ts_file);
    MODULE_MAP.iter().filter(|m| m.ts_file == norm).collect()
}

/// The rows of `ts_file` whose `anchor_symbols` name `symbol`. Scoped to the file: an anchor in
/// one TS file never claims an export of another (`TokenType` in `expression_parser/lexer.ts` and
/// in `ml_parser/tokens.ts` are different types with different Rust homes).
pub fn rust_modules_for_anchor(ts_file: &str, symbol: &str) -> Vec<&'static ModuleMapping> {
    let norm = normalize(ts_file);
    MODULE_MAP
        .iter()
        .filter(|m| m.ts_file == norm && m.anchor_symbols.contains(&symbol))
        .collect()
}

/// The Rust owner(s) of an export of `ts_file`, in order of precedence:
///   1. the rows of the file whose `anchor_symbols` name the export;
///   2. otherwise the file's un-anchored rows (`anchor_symbols` empty), its file-level owners;
///   3. otherwise, if the file has exactly one row, that row (it owns every export it does not
///      anchor away from itself).
///
/// Empty when nothing claims the export, e.g. an export of a multi-row file that no anchor names:
/// it is reported as unmapped rather than fanned out to every owner of the file.
pub fn owners_for_export(ts_file: &str, symbol: &str) -> Vec<&'static ModuleMapping> {
    let anchored = rust_modules_for_anchor(ts_file, symbol);
    if !anchored.is_empty() {
        return anchored;
    }
    let rows = rust_modules_for_ts(ts_file);
    let unanchored: Vec<&'static ModuleMapping> = rows
        .iter()
        .copied()
        .filter(|m| m.anchor_symbols.is_empty())
        .collect();
    if !unanchored.is_empty() {
        return unanchored;
    }
    if rows.len() == 1 {
        return rows;
    }
    Vec::new()
}

/// The out-of-scope entry covering `ts_file`, if any (exact file or directory prefix).
pub fn out_of_scope_file(ts_file: &str) -> Option<&'static OutOfScopeFile> {
    let norm = normalize(ts_file);
    OUT_OF_SCOPE_FILES.iter().find(|e| {
        if e.pattern.ends_with('/') {
            norm.starts_with(e.pattern)
        } else {
            norm == e.pattern
        }
    })
}

/// Classify a TS file under [`ANGULAR_COMPILER_SRC_ROOT`]: mapped, out of scope, or unclassified.
/// Mapped wins over out-of-scope; the two lists are checked disjoint by a test.
pub fn classify_ts_file(ts_file: &str) -> FileClass {
    if !rust_modules_for_ts(ts_file).is_empty() {
        return FileClass::Mapped;
    }
    match out_of_scope_file(ts_file) {
        Some(entry) => FileClass::OutOfScope(entry.reason),
        None => FileClass::Unclassified,
    }
}

/// Human-readable home of one export of `ts_file`, for the `exports` listing: its out-of-scope
/// reason, else the Rust owner(s), else `<unmapped>`.
pub fn describe_export_home(ts_file: &str, symbol: &str) -> String {
    if let Some(oos) = out_of_scope_symbol(ts_file, symbol) {
        return format!("<out of scope: {}>", oos.reason);
    }
    let owners = owners_for_export(ts_file, symbol);
    if owners.is_empty() {
        "<unmapped>".to_string()
    } else {
        owners
            .iter()
            .map(|m| m.rust_file)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// The out-of-scope entry for one export of a mapped file, if Treaty deliberately skips it.
pub fn out_of_scope_symbol(ts_file: &str, symbol: &str) -> Option<&'static OutOfScopeSymbol> {
    let norm = normalize(ts_file);
    OUT_OF_SCOPE_SYMBOLS
        .iter()
        .find(|s| s.ts_file == norm && s.symbol == symbol)
}

fn normalize(ts_file: &str) -> String {
    ts_file
        .replace('\\', "/")
        .trim_start_matches("./")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ts;
    use std::collections::BTreeSet;

    fn vendored_src() -> std::path::PathBuf {
        crate::repo_root_for_tests().join(ANGULAR_COMPILER_SRC_ROOT)
    }

    fn vendored_exports(ts_file: &str) -> Vec<String> {
        let path = vendored_src().join(ts_file);
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read vendored {}: {e}", path.display()));
        ts::exported_symbols(&src)
            .into_iter()
            .map(|s| s.name)
            .collect()
    }

    #[test]
    fn maps_identifiers_table() {
        let hits = rust_modules_for_ts("render3/r3_identifiers.ts");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].rust_file, "core/src/identifiers.rs");
        assert_eq!(hits[0].kind, PortKind::Mechanical);
    }

    #[test]
    fn maps_view_template() {
        let hits = rust_modules_for_ts("render3/view/template.ts");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].rust_file, "template/src/view/template.rs");
    }

    #[test]
    fn output_ast_maps_to_output_ast_rs() {
        let hits = rust_modules_for_ts("output/output_ast.ts");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].rust_file, "core/src/output_ast.rs");
    }

    #[test]
    fn ml_parser_fans_in() {
        // Several TS files all port into ml_parser.rs.
        let count = MODULE_MAP
            .iter()
            .filter(|m| m.rust_file == "template/src/ml_parser.rs")
            .count();
        assert!(
            count >= 7,
            "expected ml_parser.rs to be fed by >=7 TS files, got {count}"
        );
    }

    #[test]
    fn anchor_resolves_within_its_file() {
        let owners = owners_for_export("core.ts", "AttributeMarker");
        assert!(
            owners
                .iter()
                .any(|m| m.rust_file == "decorators/src/compiler.rs")
        );
        assert!(
            owners
                .iter()
                .any(|m| m.rust_file == "template/src/view/template.rs")
        );
        assert!(owners.iter().all(|m| m.ts_file == "core.ts"));
    }

    #[test]
    fn core_ts_symbols_resolve_to_their_real_owners() {
        let rust_for = |sym: &str| -> Vec<&'static str> {
            owners_for_export("core.ts", sym)
                .iter()
                .map(|m| m.rust_file)
                .collect()
        };
        assert_eq!(
            rust_for("ViewEncapsulation"),
            vec!["decorators/src/compiler.rs"]
        );
        assert_eq!(
            rust_for("ChangeDetectionStrategy"),
            vec!["decorators/src/compiler.rs"]
        );
        assert_eq!(
            rust_for("RenderFlags"),
            vec!["template/src/view/queries.rs"]
        );
        assert_eq!(rust_for("InjectFlags"), vec!["core/src/util.rs"]);
        assert_eq!(
            rust_for("SecurityContext"),
            vec!["core/src/expression/ast.rs"]
        );
        assert_eq!(rust_for("Input"), vec!["core/src/neutral.rs"]);
        // Must never be the identifiers table or the output AST, which the old row claimed.
        assert!(
            rust_for("ViewEncapsulation")
                .iter()
                .all(|f| *f != "core/src/output_ast.rs")
        );
    }

    #[test]
    fn single_row_file_owns_its_unclaimed_exports() {
        // `unwrapWriteTarget` is not named by the expression-AST row's anchors, but that file has a
        // single row, which owns every export of the file.
        let owners = owners_for_export("expression_parser/ast.ts", "unwrapWriteTarget");
        assert_eq!(owners.len(), 1, "{owners:?}");
        assert_eq!(owners[0].rust_file, "core/src/expression/ast.rs");
    }

    #[test]
    fn unanchored_row_owns_the_exports_nothing_anchors() {
        // A pipeline phase is one row with no anchors: it is the file-level owner of its exports.
        let owners = owners_for_export(
            "template/pipeline/src/phases/resolve_boundaries.ts",
            "resolveBoundaries",
        );
        assert_eq!(owners.len(), 1, "{owners:?}");
        assert_eq!(owners[0].rust_file, "template/src/view/template.rs");
    }

    #[test]
    fn multi_row_file_never_fans_out_an_unclaimed_export() {
        // core.ts has several rows and none is un-anchored, so an export that no row names is
        // unmapped. It must not be attributed to every owner of core.ts.
        assert!(owners_for_export("core.ts", "noSuchCoreExport").is_empty());
        // A named export still resolves to exactly its own owner.
        assert_eq!(
            owners_for_export("core.ts", "InjectFlags")
                .iter()
                .map(|m| m.rust_file)
                .collect::<Vec<_>>(),
            vec!["core/src/util.rs"]
        );
    }

    #[test]
    fn export_home_label_names_out_of_scope_owner_or_unmapped() {
        assert_eq!(describe_export_home("core.ts", "InjectFlags"), "core/src/util.rs");
        assert!(describe_export_home("core.ts", "Type").starts_with("<out of scope: "));
        assert_eq!(describe_export_home("core.ts", "noSuchCoreExport"), "<unmapped>");
    }

    #[test]
    fn tokens_anchor_resolves_to_html_lexer_not_expression_lexer() {
        // The defect: a global `TokenType` anchor attributed ml_parser/tokens.ts to the
        // expression lexer. Anchors are file-scoped, so each file keeps its own home.
        let html: Vec<&str> = owners_for_export("ml_parser/tokens.ts", "TokenType")
            .iter()
            .map(|m| m.rust_file)
            .collect();
        assert_eq!(html, vec!["template/src/ml_parser.rs"]);
        let expr: Vec<&str> = owners_for_export("expression_parser/lexer.ts", "TokenType")
            .iter()
            .map(|m| m.rust_file)
            .collect();
        assert_eq!(expr, vec!["core/src/expression/lexer.rs"]);
    }

    #[test]
    fn classify_covers_brief_files_and_scopes() {
        assert_eq!(
            classify_ts_file("render3/r3_boundaries.ts"),
            FileClass::Mapped
        );
        assert_eq!(
            classify_ts_file("template/pipeline/src/phases/resolve_boundaries.ts"),
            FileClass::Mapped
        );
        assert!(matches!(
            classify_ts_file("indexer/api.ts"),
            FileClass::OutOfScope(_)
        ));
        assert!(matches!(
            classify_ts_file("typecheck/ops/boundary.ts"),
            FileClass::OutOfScope(_)
        ));
        assert_eq!(
            classify_ts_file("definitely/new_angular_file.ts"),
            FileClass::Unclassified
        );
    }

    #[test]
    fn out_of_scope_symbol_is_only_for_mapped_files() {
        assert!(out_of_scope_symbol("core.ts", "Type").is_some());
        assert!(out_of_scope_symbol("core.ts", "ViewEncapsulation").is_none());
    }

    #[test]
    fn every_ts_file_resolves_to_an_existing_vendored_source() {
        // A future bad/stale row (e.g. pointing at a non-vendored or renamed Angular source) must
        // fail here, rather than silently letting drift read an empty source for that file.
        let mut missing: Vec<String> = Vec::new();
        for m in MODULE_MAP {
            let path = vendored_src().join(m.ts_file);
            if !path.is_file() {
                missing.push(format!("{} (resolved: {})", m.ts_file, path.display()));
            }
        }
        assert!(
            missing.is_empty(),
            "every symbol_map ts_file must exist under {ANGULAR_COMPILER_SRC_ROOT}; missing:\n  {}",
            missing.join("\n  ")
        );
    }

    #[test]
    fn every_rust_file_resolves_to_an_existing_port_module() {
        // A stale row pointing at a moved/renamed Rust module must fail here. Rows marked `pending`
        // are planned files (or ported by another developer) and are exempt until they land.
        let root = crate::repo_root_for_tests();
        let mut missing: Vec<String> = Vec::new();
        for m in MODULE_MAP.iter().filter(|m| m.pending.is_none()) {
            let path = root.join(TREATY_IVY_SRC_ROOT).join(m.rust_file);
            if !path.is_file() {
                missing.push(format!("{} (resolved: {})", m.rust_file, path.display()));
            }
        }
        assert!(
            missing.is_empty(),
            "every non-pending symbol_map rust_file must exist under {TREATY_IVY_SRC_ROOT}; missing:\n  {}",
            missing.join("\n  ")
        );
    }

    #[test]
    fn every_pending_row_has_a_reason() {
        for m in MODULE_MAP {
            if let Some(reason) = m.pending {
                assert!(
                    !reason.trim().is_empty(),
                    "pending row for {} needs a reason",
                    m.ts_file
                );
            }
        }
    }

    #[test]
    fn anchors_name_real_exports_of_their_file() {
        // A stale anchor (a name the file no longer exports) silently attributes nothing.
        let mut stale: Vec<String> = Vec::new();
        for m in MODULE_MAP {
            let exports = vendored_exports(m.ts_file);
            for anchor in m.anchor_symbols {
                if !exports.iter().any(|e| e == anchor) {
                    stale.push(format!(
                        "`{anchor}` is not an export of {} (-> {})",
                        m.ts_file, m.rust_file
                    ));
                }
            }
        }
        assert!(stale.is_empty(), "stale anchors:\n  {}", stale.join("\n  "));
    }

    #[test]
    fn multi_row_files_anchor_every_export() {
        // A file with several rows must claim each export with an anchor, otherwise an export
        // fans out to every owner of the file.
        let mut files: BTreeSet<&str> = BTreeSet::new();
        for m in MODULE_MAP {
            if rust_modules_for_ts(m.ts_file).len() > 1 {
                files.insert(m.ts_file);
            }
        }
        assert!(
            files.contains("core.ts"),
            "core.ts is expected to have several rows"
        );
        for file in files {
            let anchored: BTreeSet<&str> = MODULE_MAP
                .iter()
                .filter(|m| m.ts_file == file)
                .flat_map(|m| m.anchor_symbols.iter().copied())
                .collect();
            for export in vendored_exports(file) {
                let scoped_out = out_of_scope_symbol(file, &export).is_some();
                assert!(
                    anchored.contains(export.as_str()) || scoped_out,
                    "{file} has several rows, so export `{export}` must be anchored by one of them \
                     (or listed in OUT_OF_SCOPE_SYMBOLS)"
                );
            }
        }
    }

    #[test]
    fn out_of_scope_symbols_are_real_exports_of_mapped_files() {
        for s in OUT_OF_SCOPE_SYMBOLS {
            assert!(
                !rust_modules_for_ts(s.ts_file).is_empty(),
                "{} is not mapped",
                s.ts_file
            );
            assert!(
                vendored_exports(s.ts_file).iter().any(|e| e == s.symbol),
                "{} is not an export of {}",
                s.symbol,
                s.ts_file
            );
            assert!(!s.reason.trim().is_empty());
        }
    }

    #[test]
    fn out_of_scope_entries_match_real_files_and_are_disjoint_from_the_map() {
        let all =
            crate::sync::list_ts_files(&vendored_src()).expect("list vendored compiler sources");
        for entry in OUT_OF_SCOPE_FILES {
            assert!(
                !entry.reason.trim().is_empty(),
                "{} needs a reason",
                entry.pattern
            );
            let hits = all
                .iter()
                .filter(|f| {
                    if entry.pattern.ends_with('/') {
                        f.starts_with(entry.pattern)
                    } else {
                        f.as_str() == entry.pattern
                    }
                })
                .count();
            assert!(
                hits > 0,
                "out-of-scope entry `{}` matches no vendored file (stale?)",
                entry.pattern
            );
        }
        for m in MODULE_MAP {
            assert!(
                out_of_scope_file(m.ts_file).is_none(),
                "{} is both mapped and out of scope",
                m.ts_file
            );
        }
    }

    #[test]
    fn every_compiler_ts_file_is_mapped_or_out_of_scope() {
        // The check `drift` runs, as a test against the vendored tree: nothing may be silently missed.
        let all =
            crate::sync::list_ts_files(&vendored_src()).expect("list vendored compiler sources");
        let unclassified: Vec<&String> = all
            .iter()
            .filter(|f| classify_ts_file(f) == FileClass::Unclassified)
            .collect();
        assert!(
            unclassified.is_empty(),
            "unmapped and not out of scope (map it or add an OUT_OF_SCOPE_FILES entry):\n  {}",
            unclassified
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n  ")
        );
    }

    #[test]
    fn paths_are_normalized_relative() {
        for m in MODULE_MAP {
            assert!(
                !m.ts_file.starts_with('/'),
                "ts_file must be relative: {}",
                m.ts_file
            );
            assert!(
                !m.ts_file.contains('\\'),
                "ts_file must use '/': {}",
                m.ts_file
            );
            assert!(
                m.ts_file.ends_with(".ts"),
                "ts_file must be a .ts file: {}",
                m.ts_file
            );
            assert!(
                m.rust_file.ends_with(".rs"),
                "rust_file must be a .rs file: {}",
                m.rust_file
            );
            // Every rust_file is rooted at its owning treaty-ivy crate's src/ dir.
            assert!(
                m.rust_file.contains("/src/"),
                "rust_file must include its crate's src/ prefix: {}",
                m.rust_file
            );
        }
    }
}
