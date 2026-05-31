//! `render3` — Treaty's direct-to-Ivy Angular compiler.
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
//!   - [`template_mod`] — the HTML/template → instruction-IR layer (`ml_parser`, the template AST +
//!                        transform + control-flow / defer lowerings, the `t2` binder, the
//!                        template-definition builder + query generation, i18n). Depends on `core`.
//!   - [`decorators`]   — the decorator → definition layer (the `compile_*_from_metadata` instruction
//!                        emitter, `@Pipe`/`@NgModule` emit, and the
//!                        [`decorators::registry::DecoratorCompiler`] plugin registry — one plugin
//!                        per decorator kind, mirroring `apps/rust/authoring`'s `AuthoringPlugin`).
//!                        Depends on `core` + `template`.
//!   - [`facade`]       — the thin per-FILE "join": [`compile`] (template-only helper) and
//!                        [`source_compile`] (the TypeScript SOURCE front-end). Scans decorated
//!                        classes and dispatches each through the registry, then re-assembles the
//!                        complete module. Depends on all three lower layers.
//!
//! Every member is re-exported from its historical top-level path (`crate::output_ast`,
//! `crate::ml_parser`, `crate::view::compiler`, `crate::compile`, `crate::source_compile`, …) so
//! call sites inside and outside the crate (e.g. `apps/rust/authoring`) are unchanged; the split is
//! structural and emitted Ivy is byte-identical.

/// Backend-agnostic foundation: output IR, emitter, runtime identifiers, factory,
/// and the binding-expression pipeline. Depends on nothing in `template`/`decorators`.
pub mod core;

// Re-export every `core` module from its historical top-level path. These aliases
// keep `crate::output_ast::…`, `crate::output::…`, `crate::factory::…`, etc.
// resolving exactly as before (the canonical paths are now `crate::core::…`).
pub use core::expression;
pub use core::expression_converter;
pub use core::factory;
pub use core::identifiers;
pub use core::output;
pub use core::output_ast;

pub mod util;

// ---------------------------------------------------------------------------
// `template` subtree (Phase 2). The HTML/template → instruction-IR layer —
// `ml_parser`, the template AST + transform + control-flow / defer lowerings,
// the `t2` binder, the template-definition builder + query generation, and i18n
// — now live under `template_mod/`. Each is re-exported from its historical
// top-level path so call sites outside `template` are unchanged (the canonical
// paths are now `crate::template_mod::…`). See `migration/RENDER3-SPLIT-PLAN.md`.
// ---------------------------------------------------------------------------

/// HTML/template → instruction-IR layer: parser, template AST/transform, binder,
/// template-definition builder, query generation, and i18n. Depends on `core`.
pub mod template_mod;

// Re-export every `template` module from its historical top-level path. These
// aliases keep `crate::template::r3_ast::…`, `crate::ml_parser::…`,
// `crate::binder::…`, and `crate::i18n::…` resolving exactly as before.
// (`crate::view::template` / `crate::view::queries` are re-exported from
// `crate::view` itself, alongside the still-resident `view::compiler`.)
pub use template_mod::binder;
pub use template_mod::i18n;
pub use template_mod::ml_parser;
pub use template_mod::template;

// ---------------------------------------------------------------------------
// `decorators` subtree (Phase 3). The decorator → definition layer — the
// instruction emitter (`compile_component_from_metadata` /
// `compile_directive_from_metadata`), the `@Pipe`/`@NgModule` emit
// (`pipe_module_injector`), and the `DecoratorCompiler` plugin registry that
// joins them — now lives under `decorators/`. `compiler` is re-exported via
// `crate::view::compiler` (from `view/mod.rs`) and `pipe_module_injector` is
// re-exported from its historical top-level path below, so call sites outside
// this subtree are unchanged. See `migration/RENDER3-SPLIT-PLAN.md`.
// ---------------------------------------------------------------------------

/// Decorator → definition layer: the instruction emitter, `@Pipe`/`@NgModule` emit, and the
/// per-decorator [`decorators::registry::DecoratorCompiler`] plugin registry. Depends on `core`
/// and `template`.
pub mod decorators;

// Re-export `pipe_module_injector` from its historical top-level path so `crate::pipe_module_injector::…`
// resolves exactly as before (the canonical path is now `crate::decorators::pipe_module_injector`).
pub use decorators::pipe_module_injector;

// `view` is now a pure compatibility facade: every member physically lives in `template_mod`
// (`view::template` / `view::queries`) or `decorators` (`view::compiler`) and is re-exported from
// here so `crate::view::…` keeps resolving exactly as before.
pub mod view;

// ---------------------------------------------------------------------------
// `facade` subtree (Phase 4). The thin per-FILE "join" at the top of the
// dependency DAG: the template-only `compile` helper and the TypeScript SOURCE
// front-end (`source_compile`). Both scan classes and dispatch through the
// `decorators` registry rather than carrying emit logic of their own. Each is
// re-exported from its historical top-level path (`crate::compile`,
// `crate::source_compile`) so call sites inside and outside the crate are
// unchanged. See `migration/RENDER3-SPLIT-PLAN.md`.
// ---------------------------------------------------------------------------

/// The thin compile facade: the template-only end-to-end helper and the `@Component`/`@Directive`/
/// `@Pipe`/`@NgModule` TypeScript SOURCE front-end, both joining the lower layers via the
/// [`decorators::registry::DecoratorRegistry`]. Depends on `core`, `template`, and `decorators`.
pub mod facade;

// Re-export the facade members from their historical top-level paths so `crate::compile::…` and
// `crate::source_compile::…` resolve exactly as before (the canonical paths are now
// `crate::facade::…`).
pub use facade::compile;
pub use facade::source_compile;
