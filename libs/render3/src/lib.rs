//! `render3` — Treaty's direct-to-Ivy Angular compiler.
//!
//! Ported from Angular 22.1's `packages/compiler` (vendored at `tools/angular-ref`).
//! Per-module port specs live in `migration/render3-specs/`; architecture in
//! `migration/PORT-ARCHITECTURE.md`.
//!
//! Layering (each must compile before the next):
//!   L0 foundation: [`output_ast`], [`expression::lexer`], [`expression::ast`], [`identifiers`]
//!   L1: [`expression::parser`], output emitter, r3 template AST
//!   L2+: template transform, binder (selectorless/auto-import), instruction emitter

// ---------------------------------------------------------------------------
// Module groups. The crate is being reorganized into composable subtrees
// (`core` / `template` / `decorators`) joined by a decorator-compiler registry
// (see `migration/RENDER3-SPLIT-PLAN.md`). Phase 1 lands `core`; the IR + emit +
// shared expression primitives now live under `core/` and are re-exported from
// their historical top-level paths so nothing outside `core` had to change.
// ---------------------------------------------------------------------------

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

pub mod view;
pub mod compile;
pub mod source_compile;
