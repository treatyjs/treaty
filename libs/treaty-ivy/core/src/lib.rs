//! `treaty_ivy_core` — Treaty's direct-to-Ivy compiler's backend-agnostic
//! foundation: the output IR, its emitter, and the shared primitives every higher
//! layer (template, decorators, facade) builds on. It has **no** knowledge of
//! templates, decorators, or metadata extraction.
//!
//! Ported from Angular 22.1's `packages/compiler` (vendored at `tools/angular-ref`).
//!
//! Contents:
//!   - [`neutral`]               — the engine-neutral PARSE IR (`ObjLit`/`LitValue`/`NExpr`/
//!                                 `ClassWithDecorators`/`DecoratorInfo`/…) the front-end + the
//!                                 `treaty_ivy_decorators` public API read without naming an oxc type.
//!   - [`output_ast`]            — the owned expression/statement IR.
//!   - [`output::emitter`]       — lowers the IR into `oxc_ast` and prints it.
//!   - [`output::source_map`]    — source-map building for the emitter.
//!   - [`identifiers`]           — `R3` runtime symbol references (`ɵɵ…`).
//!   - [`factory`]               — `ɵfac` factory-function construction.
//!   - [`expression`]            — the binding-expression pipeline (lexer/ast/parser).
//!   - [`expression_converter`]  — binding-expression AST → output IR.
//!   - [`util`]                  — small dependency-free render3 helper types/functions.
//!   - [`digest`]                — i18n message-id digest primitives (`computeMsgId`),
//!                                 moved here to break the core<-template cycle.
//!
//! The friendly facade crate (`treaty_ivy`) re-exports each of these from its
//! historical top-level path (e.g. `treaty_ivy::output_ast`) so external call
//! sites are unchanged; the split is structural — emitted code is byte-identical.

pub mod neutral;
pub mod output_ast;
pub mod identifiers;
pub mod factory;
pub mod expression;
pub mod output;
pub mod expression_converter;
pub mod util;
pub mod digest;
