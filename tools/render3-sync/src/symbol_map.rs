//! The SYMBOL -> MODULE map: the maintained, static table mapping Angular
//! `packages/compiler` TypeScript source files (and their exported symbols) to the Rust
//! modules under `libs/treaty-ivy/*` that port them.
//!
//! This is pillar 1 of the harness (see `migration/RENDER3-SYNC-PLAN.md`): when Angular
//! drifts, a changed TS file/symbol is resolved through this table to the precise Rust file a
//! human must touch. It is intentionally a hand-maintained, deterministic constant — NO AI is
//! involved in producing or consulting it.
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
//! The attributions below were verified against the `//! Source:` / `PORT TARGET:` headers in
//! each ported Rust module.

/// Root, relative to the Treaty repo, of the vendored Angular compiler sources this harness
/// diffs against.
pub const ANGULAR_COMPILER_SRC_ROOT: &str = "tools/angular-ref/packages/compiler/src";

/// Root, relative to the Treaty repo, of the Rust port the harness keeps 1:1. The port now spans
/// four crates under `libs/treaty-ivy`, so each `rust_file` carries its crate's `src/` prefix.
pub const TREATY_IVY_SRC_ROOT: &str = "libs/treaty-ivy";

/// Back-compat alias for the pre-rename `libs/render3/src` root constant. Retained as a re-export
/// so any out-of-crate consumer keeps compiling; new code should use [`TREATY_IVY_SRC_ROOT`].
#[deprecated(note = "the render3 port was carved into libs/treaty-ivy/*; use TREATY_IVY_SRC_ROOT")]
pub const RENDER3_SRC_ROOT: &str = TREATY_IVY_SRC_ROOT;

/// Confidence that the Rust module is a *mechanical* (value/name table) port of the TS, and
/// therefore a candidate for pillar-3 oxc codegen, versus hand-ported logic that drift only
/// flags for manual re-porting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PortKind {
    /// Pure data table (consts / enums / numeric flags / name maps) — safe to auto-codegen and
    /// diff byte-for-byte against the committed Rust.
    Mechanical,
    /// Hand-ported logic (control flow, ownership) — drift is reported as a manual port task,
    /// never auto-transpiled.
    Logic,
}

/// One row of the symbol->module map: an Angular TS source file and the Rust module that ports
/// it, plus the kind of port and the (optional) per-module spec under `migration/render3-specs`.
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
    /// Per-module port spec under `migration/render3-specs/`, if one exists.
    pub spec: Option<&'static str>,
    /// Notable exported symbols that anchor this mapping (used to attribute a changed export to
    /// a Rust file). Not exhaustive — drift falls back to file-level mapping.
    pub anchor_symbols: &'static [&'static str],
}

/// The static, maintained symbol->module map. One Rust module may be fed by several TS files
/// (e.g. `ml_parser.rs`, `i18n.rs`), so several rows can share a `rust_file`.
pub const MODULE_MAP: &[ModuleMapping] = &[
    // ---- L0 foundation -----------------------------------------------------------------
    ModuleMapping {
        ts_file: "render3/r3_identifiers.ts",
        rust_file: "core/src/identifiers.rs",
        kind: PortKind::Mechanical,
        spec: Some("migration/render3-specs/14-identifiers.md"),
        anchor_symbols: &["Identifiers"],
    },
    ModuleMapping {
        ts_file: "output/output_ast.ts",
        rust_file: "core/src/output_ast.rs",
        kind: PortKind::Logic,
        spec: Some("migration/render3-specs/01-output_ast.md"),
        anchor_symbols: &["Expression", "Statement", "Type", "BuiltinType", "BinaryOperator"],
    },
    ModuleMapping {
        ts_file: "render3/util.ts",
        rust_file: "core/src/util.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["R3Reference", "DefinitionMap"],
    },
    // ---- expression parser -------------------------------------------------------------
    ModuleMapping {
        ts_file: "expression_parser/lexer.ts",
        rust_file: "core/src/expression/lexer.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["Lexer", "Token", "TokenType"],
    },
    ModuleMapping {
        ts_file: "expression_parser/ast.ts",
        rust_file: "core/src/expression/ast.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["AST", "ASTWithSource", "Binary", "PropertyRead"],
    },
    ModuleMapping {
        ts_file: "expression_parser/parser.ts",
        rust_file: "core/src/expression/parser.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["Parser", "ParseSpan"],
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
    },
    ModuleMapping {
        ts_file: "output/abstract_js_emitter.ts",
        rust_file: "core/src/output/emitter.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["AbstractJsEmitterVisitor"],
    },
    // ---- r3 template AST + transform ---------------------------------------------------
    ModuleMapping {
        ts_file: "render3/r3_ast.ts",
        rust_file: "template/src/template/r3_ast.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["Element", "Template", "BoundText", "Node"],
    },
    ModuleMapping {
        ts_file: "render3/r3_template_transform.ts",
        rust_file: "template/src/template/template_transform.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["HtmlAstToIvyAst", "htmlAstToRender3Ast"],
    },
    ModuleMapping {
        ts_file: "render3/r3_control_flow.ts",
        rust_file: "template/src/template/control_flow.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["createIfBlock", "createForLoop", "createSwitchBlock"],
    },
    ModuleMapping {
        ts_file: "render3/r3_deferred_blocks.ts",
        rust_file: "template/src/template/deferred.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["createDeferredBlock"],
    },
    ModuleMapping {
        ts_file: "render3/r3_deferred_triggers.ts",
        rust_file: "template/src/template/deferred.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["parseDeferredTime", "parseWhenTrigger", "parseOnTrigger"],
    },
    // ---- view (compiler / template instructions / queries / binder / config) -----------
    ModuleMapping {
        ts_file: "render3/view/compiler.ts",
        rust_file: "decorators/src/compiler.rs",
        kind: PortKind::Logic,
        spec: Some("migration/render3-specs/11-view_compiler.md"),
        anchor_symbols: &["compileComponentFromMetadata", "compileDirectiveFromMetadata"],
    },
    ModuleMapping {
        ts_file: "render3/view/template.ts",
        rust_file: "template/src/view/template.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["TemplateDefinitionBuilder"],
    },
    ModuleMapping {
        ts_file: "render3/view/query_generation.ts",
        rust_file: "template/src/view/queries.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["createViewQueriesFunction", "createContentQueriesFunction"],
    },
    ModuleMapping {
        ts_file: "render3/view/t2_binder.ts",
        rust_file: "template/src/binder.rs",
        kind: PortKind::Logic,
        spec: Some("migration/render3-specs/10-t2_binder.md"),
        anchor_symbols: &["R3TargetBinder", "BoundTarget"],
    },
    ModuleMapping {
        ts_file: "render3/view/t2_api.ts",
        rust_file: "template/src/binder.rs",
        kind: PortKind::Logic,
        spec: Some("migration/render3-specs/10-t2_binder.md"),
        anchor_symbols: &["Target", "BoundTarget", "DirectiveMeta"],
    },
    // ---- factory / pipe / module / injector codegen ------------------------------------
    ModuleMapping {
        ts_file: "render3/r3_factory.ts",
        rust_file: "core/src/factory.rs",
        kind: PortKind::Logic,
        spec: Some("migration/render3-specs/15-factory.md"),
        anchor_symbols: &["compileFactoryFunction", "R3FactoryMetadata", "FactoryTarget"],
    },
    ModuleMapping {
        ts_file: "render3/r3_pipe_compiler.ts",
        rust_file: "decorators/src/pipe_module_injector.rs",
        kind: PortKind::Logic,
        spec: Some("migration/render3-specs/16-pipe_module_injector.md"),
        anchor_symbols: &["compilePipeFromMetadata", "createPipeType"],
    },
    ModuleMapping {
        ts_file: "render3/r3_module_compiler.ts",
        rust_file: "decorators/src/pipe_module_injector.rs",
        kind: PortKind::Logic,
        spec: Some("migration/render3-specs/16-pipe_module_injector.md"),
        anchor_symbols: &["compileNgModule", "createNgModuleType"],
    },
    ModuleMapping {
        ts_file: "render3/r3_injector_compiler.ts",
        rust_file: "decorators/src/pipe_module_injector.rs",
        kind: PortKind::Logic,
        spec: Some("migration/render3-specs/16-pipe_module_injector.md"),
        anchor_symbols: &["compileInjector", "R3InjectorMetadata"],
    },
    // ---- expression converter ----------------------------------------------------------
    // Angular v22 retired the historic `compiler_util/expression_converter.ts`; the
    // `e.AST -> o.Expression` lowering it used to own now lives in the template *pipeline*:
    // `convertAst` (the per-node lowering) in `template/pipeline/src/ingest.ts` and the binary
    // operator table `BINARY_OPERATORS` in `template/pipeline/src/conversion.ts`. The Rust
    // `expression_converter.rs` reproduces the classic `convertPropertyBinding` behaviour from
    // those two pipeline sources (see its `PORT TARGET:` header), so the map points at the
    // vendored pipeline files that actually exist — not the removed `compiler_util` path.
    ModuleMapping {
        ts_file: "template/pipeline/src/ingest.ts",
        rust_file: "core/src/expression_converter.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["ingestComponent", "ingestHostBinding"],
    },
    ModuleMapping {
        ts_file: "template/pipeline/src/conversion.ts",
        rust_file: "core/src/expression_converter.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["BINARY_OPERATORS", "literalOrArrayLiteral"],
    },
    // ---- ml_parser (HTML front-end) ----------------------------------------------------
    ModuleMapping {
        ts_file: "ml_parser/lexer.ts",
        rust_file: "template/src/ml_parser.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["Lexer", "tokenize"],
    },
    ModuleMapping {
        ts_file: "ml_parser/parser.ts",
        rust_file: "template/src/ml_parser.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["Parser", "ParseTreeResult"],
    },
    ModuleMapping {
        ts_file: "ml_parser/ast.ts",
        rust_file: "template/src/ml_parser.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["Element", "Attribute", "Text", "Node"],
    },
    ModuleMapping {
        ts_file: "ml_parser/html_parser.ts",
        rust_file: "template/src/ml_parser.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["HtmlParser"],
    },
    ModuleMapping {
        ts_file: "ml_parser/tags.ts",
        rust_file: "template/src/ml_parser.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["TagContentType", "getNsPrefix"],
    },
    ModuleMapping {
        ts_file: "ml_parser/html_tags.ts",
        rust_file: "template/src/ml_parser.rs",
        kind: PortKind::Mechanical,
        spec: None,
        anchor_symbols: &["HtmlTagDefinition", "getHtmlTagDefinition"],
    },
    ModuleMapping {
        ts_file: "ml_parser/tokens.ts",
        rust_file: "template/src/ml_parser.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["TokenType", "Token"],
    },
    // ---- i18n core ---------------------------------------------------------------------
    ModuleMapping {
        ts_file: "i18n/digest.ts",
        rust_file: "core/src/digest.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["computeMsgId", "fingerprint", "decimalDigest"],
    },
    ModuleMapping {
        ts_file: "i18n/i18n_ast.ts",
        rust_file: "template/src/i18n.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["Message", "Node", "Container", "Icu", "Placeholder"],
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
    },
    ModuleMapping {
        ts_file: "render3/view/config.ts",
        rust_file: "decorators/src/compiler.rs",
        kind: PortKind::Mechanical,
        spec: None,
        anchor_symbols: &["toOptimizableTemplate"],
    },
    ModuleMapping {
        ts_file: "jit_compiler_facade.ts",
        rust_file: "facade/src/source_compile.rs",
        kind: PortKind::Logic,
        spec: None,
        anchor_symbols: &["CompilerFacadeImpl"],
    },
    // ---- core enums / flag tables (the MECHANICAL codegen targets, pillar 3) ------------
    // `core.ts` is the canonical home of the numeric flag enums (AttributeMarker, SelectorFlags,
    // ChangeDetectionStrategy, ViewEncapsulation, RenderFlags, ...) whose values must match
    // ours byte-for-byte. They are consumed across output_ast.rs / view/* / template/* — drift
    // here is the highest-value, safest auto-codegen.
    ModuleMapping {
        ts_file: "core.ts",
        rust_file: "core/src/output_ast.rs",
        kind: PortKind::Mechanical,
        spec: None,
        anchor_symbols: &[
            "AttributeMarker",
            "SelectorFlags",
            "ChangeDetectionStrategy",
            "ViewEncapsulation",
            "RenderFlags",
            "BindingFlags",
            "InputFlags",
        ],
    },
];

/// Resolve a changed TS file (path relative to [`ANGULAR_COMPILER_SRC_ROOT`]) to all Rust
/// modules that port it. Returns every matching row, since one TS file can map to one Rust file
/// and several TS files can fan into one Rust file.
pub fn rust_modules_for_ts(ts_file: &str) -> Vec<&'static ModuleMapping> {
    let norm = ts_file.replace('\\', "/");
    MODULE_MAP.iter().filter(|m| m.ts_file == norm).collect()
}

/// Resolve a changed exported symbol name to the Rust modules whose `anchor_symbols` list it.
/// Used to refine a file-level drift hit to a specific symbol owner.
pub fn rust_modules_for_symbol(symbol: &str) -> Vec<&'static ModuleMapping> {
    MODULE_MAP
        .iter()
        .filter(|m| m.anchor_symbols.contains(&symbol))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(count >= 7, "expected ml_parser.rs to be fed by >=7 TS files, got {count}");
    }

    #[test]
    fn symbol_resolves_to_module() {
        let hits = rust_modules_for_symbol("AttributeMarker");
        assert!(hits.iter().any(|m| m.rust_file == "core/src/output_ast.rs"));
    }

    /// Locate the Treaty repo root from this crate's directory, so the test resolves vendored
    /// `tools/angular-ref` paths regardless of the cargo invocation's CWD. `CARGO_MANIFEST_DIR`
    /// is `<repo>/tools/render3-sync`; the root is two levels up.
    fn repo_root() -> std::path::PathBuf {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        manifest
            .parent() // tools/
            .and_then(|p| p.parent()) // <repo>/
            .expect("crate is nested at <repo>/tools/render3-sync")
            .to_path_buf()
    }

    #[test]
    fn every_ts_file_resolves_to_an_existing_vendored_source() {
        // A future bad/stale row (e.g. pointing at a non-vendored or renamed Angular source) must
        // fail CI here, rather than silently letting drift read an empty source for that file.
        let root = repo_root();
        let mut missing: Vec<String> = Vec::new();
        for m in MODULE_MAP {
            let path = root.join(ANGULAR_COMPILER_SRC_ROOT).join(m.ts_file);
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
        // The render3 port was carved into libs/treaty-ivy/{core,template,decorators,facade}; a
        // stale row pointing at a moved/renamed Rust module must fail here rather than letting the
        // codegen-verify path silently read an absent committed file (and report false "Missing").
        let root = repo_root();
        let mut missing: Vec<String> = Vec::new();
        for m in MODULE_MAP {
            let path = root.join(TREATY_IVY_SRC_ROOT).join(m.rust_file);
            if !path.is_file() {
                missing.push(format!("{} (resolved: {})", m.rust_file, path.display()));
            }
        }
        assert!(
            missing.is_empty(),
            "every symbol_map rust_file must exist under {TREATY_IVY_SRC_ROOT}; missing:\n  {}",
            missing.join("\n  ")
        );
    }

    #[test]
    fn paths_are_normalized_relative() {
        for m in MODULE_MAP {
            assert!(!m.ts_file.starts_with('/'), "ts_file must be relative: {}", m.ts_file);
            assert!(!m.ts_file.contains('\\'), "ts_file must use '/': {}", m.ts_file);
            assert!(m.ts_file.ends_with(".ts"), "ts_file must be a .ts file: {}", m.ts_file);
            assert!(m.rust_file.ends_with(".rs"), "rust_file must be a .rs file: {}", m.rust_file);
            // Every rust_file is rooted at its owning treaty-ivy crate's src/ dir.
            assert!(
                m.rust_file.contains("/src/"),
                "rust_file must include its crate's src/ prefix: {}",
                m.rust_file
            );
        }
    }
}
