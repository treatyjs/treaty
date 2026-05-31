//! `core` — render3's backend-agnostic foundation: the output IR, its emitter, and
//! the shared primitives every higher layer (template, decorators) builds on. It has
//! **no** knowledge of templates, decorators, or metadata extraction.
//!
//! Contents:
//!   - [`output_ast`]            — the owned expression/statement IR.
//!   - [`output::emitter`]       — lowers the IR into `oxc_ast` and prints it.
//!   - [`output::source_map`]    — source-map building for the emitter.
//!   - [`identifiers`]           — `R3` runtime symbol references (`ɵɵ…`).
//!   - [`factory`]               — `ɵfac` factory-function construction.
//!   - [`expression`]            — the binding-expression pipeline (lexer/ast/parser).
//!   - [`expression_converter`]  — binding-expression AST → output IR.
//!
//! The crate root re-exports each of these from its historical top-level path (e.g.
//! `crate::output_ast`) so call sites outside `core` are unchanged; the canonical
//! path is `crate::core::output_ast`. The split is structural — emitted code is
//! byte-identical.

pub mod output_ast;
pub mod identifiers;
pub mod factory;
pub mod expression;
pub mod output;
pub mod expression_converter;
