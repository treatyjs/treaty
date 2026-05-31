//! `template` — the HTML/template → instruction-IR layer. It turns parsed markup
//! into render3's template AST, binds it (selectorless resolution + auto-import),
//! lowers it to the instruction-emitter inputs, and extracts i18n metadata. It
//! depends on [`treaty_ivy_core`] but knows nothing about decorators or metadata
//! extraction.
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
//! The crate root re-exports each of these from its historical top-level path (e.g.
//! `crate::ml_parser`, `crate::binder`, `crate::template::r3_ast`, `crate::view::template`)
//! so call sites outside `template` are unchanged; the canonical path is now
//! `crate::template_mod::…`. The split is structural — emitted code is byte-identical.

pub mod ml_parser;
pub mod template;
pub mod binder;
pub mod i18n;
pub mod view;
