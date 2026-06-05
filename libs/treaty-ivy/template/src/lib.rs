//! `treaty_ivy_template` — the HTML/template → instruction-IR layer. It turns
//! parsed markup into render3's template AST, binds it (selectorless resolution +
//! auto-import), lowers it to the instruction-emitter inputs, and extracts i18n
//! metadata. It depends on [`treaty_ivy_core`] but knows nothing about decorators
//! or metadata extraction.
//!
//! Contents:
//!   - [`ml_parser`]            — HTML/markup lexer + parser.
//!   - [`template`]             — render3 template AST (`r3_ast`), the HTML→template
//!                                transform, and the control-flow / `@defer` lowerings.
//!   - [`binder`]               — `t2`-style binder: selectorless resolution + auto-import.
//!   - [`view::template`]       — the template-definition builder (slot allocation,
//!                                instruction generation).
//!   - [`view::queries`]        — content/view query generation + shared pool types.
//!   - [`i18n`]                 — i18n message/placeholder extraction.
//!
//! The backend-agnostic foundation (output IR, emitter, runtime identifiers, the
//! binding-expression pipeline, and the i18n message-id digest primitives) lives in
//! the lower crate [`treaty_ivy_core`]. Its modules are re-exported below from their
//! historical top-level paths so intra-crate references (`crate::output_ast::…`,
//! `crate::expression::…`, `crate::identifiers::…`, `crate::digest::…`, …) keep
//! resolving exactly as before — no per-file edit is needed across the move. The
//! split is structural — emitted code is byte-identical.

// Historical core aliases — every `crate::<core-module>` reference in this crate's
// template code resolves to the corresponding `treaty_ivy_core` module via these
// re-exports. (`i18n.rs` additionally re-exports `digest::{compute_msg_id,
// fingerprint}` to preserve the `crate::i18n::compute_msg_id` surface.)
pub use treaty_ivy_core::digest;
pub use treaty_ivy_core::expression;
pub use treaty_ivy_core::expression_converter;
pub use treaty_ivy_core::identifiers;
pub use treaty_ivy_core::output;
pub use treaty_ivy_core::output_ast;

pub mod ml_parser;
pub mod template;
pub mod binder;
pub mod i18n;
pub mod view;
