//! `treaty_ivy_core` — Treaty's direct-to-Ivy compiler's backend-agnostic
//! foundation: the output IR, its emitter, and the shared primitives every higher
//! layer (template, decorators, facade) builds on. It has **no** knowledge of
//! templates, decorators, or metadata extraction.
//!
//! Ported from Angular main at `78d2a67d` (`tools/render3-sync/ANGULAR_REF`)'s `packages/compiler` (vendored at `tools/angular-ref`).
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
//!   - [`parse_util`]            — `ParseLocation`/`ParseSourceSpan`/`ParseError`: the one shared
//!                                 source-location and diagnostic model (UTF-16 offsets).
//!
//! The friendly facade crate (`treaty_ivy`) re-exports each of these from its
//! historical top-level path (e.g. `treaty_ivy::output_ast`) so external call
//! sites are unchanged; the split is structural — emitted code is byte-identical.

/// The Angular commit this compiler is ported from: Angular `main` at `78d2a67d` (2026-10-10).
///
/// This is the one Rust-side copy of the pin. The single source of truth is
/// `tools/render3-sync/ANGULAR_REF`; the unit test `angular_ref_matches_the_render3_sync_pin` fails if
/// the two disagree. Doc comments across the crates name the same commit.
pub const ANGULAR_REF: &str = "78d2a67d922b2a2af29d50aed587bc78a99a5450";

pub mod neutral;
pub mod output_ast;
pub mod identifiers;
pub mod factory;
pub mod expression;
pub mod output;
pub mod expression_converter;
pub mod util;
pub mod digest;
pub mod parse_util;

#[cfg(test)]
mod tests {
    use super::ANGULAR_REF;

    #[test]
    fn angular_ref_matches_the_render3_sync_pin() {
        // `tools/render3-sync/ANGULAR_REF` is the single pinned SHA; skip only when the crate is
        // built outside the repository (for example from a packaged copy).
        let pin = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../../tools/render3-sync/ANGULAR_REF");
        if let Ok(text) = std::fs::read_to_string(pin) {
            assert_eq!(text.trim(), ANGULAR_REF);
        }
    }
}
