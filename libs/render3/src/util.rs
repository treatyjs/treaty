//! `render3/util.ts` — shared render3 helper types and functions.
//!
//! PORT TARGET: `tools/angular-ref/packages/compiler/src/render3/util.ts`
//!
//! These are the small, dependency-free building blocks shared across the render3 emitters
//! (`factory.rs`, `pipe_module_injector.rs`, `view/compiler.rs`, …): the [`R3Reference`] /
//! [`R3CompiledExpression`] data shapes returned by every `compile*` emitter, plus the
//! [`type_with_parameters`] and [`ts_ignore_comment`] helpers.
//!
//! Field-name note: the TS `R3Reference.type` / `R3CompiledExpression.type` field is named `ty`
//! here because `type` is a reserved word in Rust. The value is an `output_ast` expression
//! ([`R3Reference`]) or [`Type`] ([`R3CompiledExpression`]) respectively.

use crate::output_ast::{self as o, Expr, LeadingComment, Stmt, Type};

/// `util.ts` `R3Reference { value, type }` — a pair of expressions: the runtime value and the
/// `.d.ts` type expression for a referenced symbol. `type` → `ty` (Rust keyword).
#[derive(Debug, Clone, PartialEq)]
pub struct R3Reference {
    pub value: Expr,
    pub ty: Expr,
}

impl R3Reference {
    /// Convenience constructor (`{value, type}`).
    pub fn new(value: Expr, ty: Expr) -> R3Reference {
        R3Reference { value, ty }
    }
}

/// `util.ts` `R3CompiledExpression { expression, type, statements }` — the result of compiling a
/// render3 code unit (component, directive, pipe, etc.): the def RHS expression, its `.d.ts` type,
/// and any extra top-level statements. `type` → `ty` (Rust keyword).
#[derive(Debug, Clone, PartialEq)]
pub struct R3CompiledExpression {
    pub expression: Expr,
    pub ty: Type,
    pub statements: Vec<Stmt>,
}

/// `util.ts` `typeWithParameters(type, numParams)` — `ExpressionType(type)` with `numParams`
/// `DYNAMIC_TYPE` type-arguments (none when `numParams === 0`).
pub fn type_with_parameters(ty: Expr, num_params: u32) -> Type {
    if num_params == 0 {
        o::expression_type(ty, None, None)
    } else {
        let params = (0..num_params).map(|_| o::dynamic_type()).collect();
        o::expression_type(ty, None, Some(params))
    }
}

/// `util.ts` `tsIgnoreComment()` — a leading, multiline `@ts-ignore` comment with a trailing
/// newline. It must sit on a *statement* (the newline would break a `return` if placed on an
/// expression).
pub fn ts_ignore_comment() -> LeadingComment {
    o::leading_comment("@ts-ignore", true, true)
}
