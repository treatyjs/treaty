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

pub mod output_ast;
pub mod identifiers;
pub mod util;
pub mod expression;
pub mod output;
pub mod template;
pub mod factory;
pub mod pipe_module_injector;
pub mod binder;
pub mod ml_parser;
pub mod view;
pub mod expression_converter;
pub mod compile;
pub mod source_compile;
pub mod i18n;
