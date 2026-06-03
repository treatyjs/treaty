//! `treaty_ivy` — Treaty's direct-to-Ivy Angular compiler.
//!
//! Ported from Angular 22.1's `packages/compiler` (vendored at `tools/angular-ref`).
//! Per-module port specs live in `migration/render3-specs/`; architecture in
//! `migration/PORT-ARCHITECTURE.md`.
//!
//! # Module groups
//!
//! The crate is organized into composable subtrees in dependency order
//! (`core <- template <- decorators <- facade`), joined by a decorator-compiler plugin registry
//! (see `migration/RENDER3-SPLIT-PLAN.md`):
//!
//!   - [`core`]         — backend-agnostic IR + emit + shared expression primitives (output AST,
//!                        emitter / source maps, runtime identifiers, factory, the binding-expression
//!                        lexer/parser/converter). Knows nothing about templates or decorators.
//!   - `treaty_ivy_template` — the HTML/template → instruction-IR layer (`ml_parser`, the template AST +
//!                        transform + control-flow / defer lowerings, the `t2` binder, the
//!                        template-definition builder + query generation, i18n). Depends on `core`.
//!   - [`decorators`]   — the decorator → definition layer (the `compile_*_from_metadata` instruction
//!                        emitter, `@Pipe`/`@NgModule` emit, and the
//!                        [`decorators::registry::DecoratorCompiler`] plugin registry — one plugin
//!                        per decorator kind, mirroring `apps/rust/authoring`'s `AuthoringPlugin`).
//!                        Depends on `core` + `template`.
//!   - `facade` (THIS crate) — the thin per-FILE "join": [`compile`] (template-only helper) and
//!                        [`source_compile`] (the TypeScript SOURCE front-end). Scans decorated
//!                        classes and dispatches each through the registry, then re-assembles the
//!                        complete module. Depends on all three lower layers.
//!
//! Every member is re-exported from its historical top-level path (`crate::output_ast`,
//! `crate::ml_parser`, `crate::view::compiler`, `crate::compile`, `crate::source_compile`, …) so
//! call sites inside and outside the crate (e.g. `apps/rust/authoring`) are unchanged; the split is
//! structural and emitted Ivy is byte-identical.

// The backend-agnostic foundation (output IR, emitter, runtime identifiers,
// factory, the binding-expression pipeline, util helpers, and the i18n digest
// primitives) is now the separate crate `treaty_ivy_core`. Re-export every core
// module from its historical top-level path so `crate::output_ast::…`,
// `crate::output::…`, `crate::factory::…`, `crate::util::…`, `crate::digest::…`,
// etc. keep resolving inside this crate exactly as before — the not-yet-carved
// template/decorator/facade subtrees reference these via `crate::…` and need no
// per-file edit until they are themselves carved. The split is structural;
// emitted Ivy is byte-identical.
pub use treaty_ivy_core::digest;
pub use treaty_ivy_core::expression;
pub use treaty_ivy_core::expression_converter;
pub use treaty_ivy_core::factory;
pub use treaty_ivy_core::identifiers;
pub use treaty_ivy_core::output;
pub use treaty_ivy_core::output_ast;
pub use treaty_ivy_core::util;

// ---------------------------------------------------------------------------
// `template` subtree. The HTML/template → instruction-IR layer — `ml_parser`,
// the template AST + transform + control-flow / defer lowerings, the `t2`
// binder, the template-definition builder + query generation, and i18n — is now
// the separate crate `treaty_ivy_template`. Each member is re-exported from its
// historical top-level path so call sites inside the not-yet-carved
// decorator/facade subtrees (which reference these via `crate::…`) need no
// per-file edit until they are themselves carved. The split is structural;
// emitted Ivy is byte-identical.
// ---------------------------------------------------------------------------

// Re-export every `template` module from its historical top-level path. These
// aliases keep `crate::template::r3_ast::…`, `crate::ml_parser::…`,
// `crate::binder::…`, and `crate::i18n::…` resolving exactly as before.
// (`crate::view::template` / `crate::view::queries` are re-exported from
// `crate::view` itself, alongside the still-resident `view::compiler`.)
pub use treaty_ivy_template::binder;
pub use treaty_ivy_template::i18n;
pub use treaty_ivy_template::ml_parser;
pub use treaty_ivy_template::template;

// ---------------------------------------------------------------------------
// `decorators` subtree (Phase 3). The decorator → definition layer — the
// instruction emitter (`compile_component_from_metadata` /
// `compile_directive_from_metadata`), the `@Pipe`/`@NgModule` emit
// (`pipe_module_injector`), and the `DecoratorCompiler` plugin registry that
// joins them — is now the separate crate `treaty_ivy_decorators`. It is
// re-exported here from its historical top-level path `crate::decorators` so
// call sites inside this (facade) crate (`crate::decorators::registry::…`) and
// outside it (`treaty_ivy::decorators::…`) keep resolving exactly as before;
// `compiler` is additionally re-exported via `crate::view::compiler` (from
// `view/mod.rs`) and `pipe_module_injector` from its historical top-level path
// below. The split is structural; emitted code is byte-identical.
// ---------------------------------------------------------------------------

/// Decorator → definition layer: the instruction emitter, `@Pipe`/`@NgModule` emit, and the
/// per-decorator [`treaty_ivy_decorators::registry::DecoratorCompiler`] plugin registry. Depends on
/// `treaty_ivy_core` and `treaty_ivy_template`.
pub use treaty_ivy_decorators as decorators;

// Re-export `pipe_module_injector` from its historical top-level path so `crate::pipe_module_injector::…`
// resolves exactly as before (the canonical path is now `treaty_ivy_decorators::pipe_module_injector`).
pub use treaty_ivy_decorators::pipe_module_injector;

// `view` is now a pure compatibility facade spanning two crates: `view::compiler` lives in
// `treaty_ivy_decorators`, while `view::template` / `view::queries` live in `treaty_ivy_template`.
// All three are re-exported here so `crate::view::…` (and the external
// `treaty_ivy::view::{compiler, template, queries}`) keep resolving exactly as before — this is the
// two-crate `view` facade called out as the trickiest re-export of the split.
pub mod view {
    pub use treaty_ivy_decorators::compiler;
    pub use treaty_ivy_template::view::queries;
    pub use treaty_ivy_template::view::template;
}

// ---------------------------------------------------------------------------
// `facade` subtree (Phase 4). This crate IS the facade — the thin per-FILE
// "join" at the top of the dependency DAG: the template-only `compile` helper
// and the TypeScript SOURCE front-end (`source_compile`). Both scan classes and
// dispatch through the `decorators` registry rather than carrying emit logic of
// their own. They are the facade crate's only own modules; their historical
// top-level path `treaty_ivy::compile` / `treaty_ivy::source_compile` is the
// module itself, so no extra `pub use` is needed. The split is structural;
// emitted code is byte-identical. See `migration/RENDER3-SPLIT-PLAN.md`.
// ---------------------------------------------------------------------------

/// The template-only end-to-end helper plus the shared `RealTemplateBuilder` glue and the
/// selectorless auto-import resolution the source front-end reuses. Joins the lower layers via the
/// [`decorators::registry::DecoratorRegistry`]. Depends on `core`, `template`, and `decorators`.
pub mod compile;

/// The `@Component`/`@Directive`/`@Pipe`/`@NgModule` TypeScript SOURCE front-end: oxc-parse the
/// module, register the per-decorator plugins, dispatch each class through the registry, and
/// re-assemble the augmented module. Depends on `core`, `template`, and `decorators`.
pub mod source_compile;

/// The Angular **partial-declaration linker**: rewrite a published library's `ɵɵngDeclare*({...})`
/// calls into the full AOT `ɵɵdefine*({...})` calls by driving the SAME emit fed from the
/// declaration object instead of a decorator. Depends on `core`, `template`, and `decorators`.
pub mod linker;

pub use linker::link_partial;

/// The Angular **partial-declaration emitter** — the INVERSE of [`linker`]: rewrite an AOT
/// `ɵɵdefine*({...})` module into the `ɵɵngDeclare*({...})` partial form a library publishes with
/// `compilationMode: "partial"`. Mode-gated: the AOT default path never calls it, so the default
/// emit is byte-unchanged. The DI/pipe family round-trips exactly back through [`linker`].
pub mod partial_emit;

pub use partial_emit::{emit_partial, PartialEmit};

/// The source-side **partial component / directive declaration emitter** — emits
/// `ɵɵngDeclareComponent` / `ɵɵngDeclareDirective` DIRECTLY from the source front-end's
/// [`view::compiler::R3DirectiveMetadata`] + the ORIGINAL template string, avoiding the AOT-to-HTML
/// decompiler that a span-rewrite [`partial_emit`] of a component would require. Mode-gated through
/// [`source_compile::CompileOptions::emit_partial_component`]; the default Full emit is untouched. The
/// output round-trips back to AOT through [`linker::link_partial`].
pub mod partial_component_emit;
