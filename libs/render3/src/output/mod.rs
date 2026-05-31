//! Output lowering & emission: lower the owned `output_ast` IR into `oxc_ast` via
//! `AstBuilder`, then print with `oxc_codegen`. Replaces Angular's hand-rolled
//! `abstract_emitter`/`abstract_js_emitter`. See `migration/render3-specs/02-abstract_emitter.md`.

pub mod emitter;
pub mod source_map;
