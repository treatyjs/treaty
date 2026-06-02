//! Output lowering & emission: lower the owned `output_ast` IR into `oxc_ast` via
//! `AstBuilder`, then print with `oxc_codegen`. Replaces Angular's hand-rolled
//! `abstract_emitter`/`abstract_js_emitter`. See `migration/render3-specs/02-abstract_emitter.md`.

pub mod emitter;
/// The engine-neutral printer used as the SWC-backend emitter. It walks the SAME `output_ast` as
/// the oxc emitter and is tuned to emit byte-identical output, proven by `tools/backend-parity`.
/// See its module header for why a neutral printer rather than `swc_ecma_codegen` (which cannot
/// match oxc_codegen's tab-indented, compact-array pretty form).
///
/// Always compiled (not feature-gated) so a single binary — e.g. `tools/backend-parity` — can
/// drive BOTH the oxc emit path (`emitter::emit_*`) and this neutral path side by side and diff
/// them in one process. Under `--features swc` the public `emitter::emit_*` functions delegate
/// here; under the default build they do not, and this module is simply unused by the hot path.
pub mod emitter_swc;
pub mod source_map;
