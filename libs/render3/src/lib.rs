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
pub mod template;
pub mod pipe_module_injector;
pub mod binder;
pub mod ml_parser;
pub mod view;
pub mod compile;
pub mod source_compile;
pub mod i18n;
