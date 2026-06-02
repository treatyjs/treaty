//! The **swc** backend — the engine-neutral (SWC-side) printer over Treaty's `output_ast`.
//!
//! This module exists only under `#[cfg(feature = "swc")]`. When the `swc` feature is OFF the
//! backend is not compiled and [`enabled_backends`](super::enabled_backends) omits it (with a
//! logged notice) — the harness then runs oxc-only and STILL compiles + runs, the Phase-1 state in
//! SWC-BACKEND-PLAN.md §5.
//!
//! # How it works
//!
//! Both backends link the SAME `treaty_ivy` crate (default features → oxc emit). The two emit
//! paths coexist in one binary because the neutral printer
//! (`treaty_ivy_core::output::emitter_swc`, surfaced as `treaty_ivy::compile::compile_component_swc`)
//! is always compiled, NOT feature-gated. So:
//!   * [`OxcBackend`](super::oxc::OxcBackend) calls `compile_component` (oxc emit), and
//!   * [`SwcBackend`] calls `compile_component_swc` (the neutral SWC-side printer),
//! and the gate diffs the two printers in a single process — no rebuild-under-a-different-feature
//! dance (SWC-BACKEND-PLAN.md §4.2 is satisfied structurally instead).
//!
//! Both consume the IDENTICAL assembled `output_ast`; only the final printer differs, so any byte
//! diff is a genuine printer-divergence bug — exactly what the gate is for.

#[cfg(feature = "swc")]
mod imp {
    use treaty_ivy::compile::compile_component_swc;

    use super::super::Backend;
    use crate::corpus::Fixture;

    /// The SWC-backed (engine-neutral printer) Treaty Ivy compiler backend.
    pub struct SwcBackend;

    impl Backend for SwcBackend {
        fn name(&self) -> &str {
            "swc"
        }

        fn compile(&self, fixture: &Fixture) -> Result<String, String> {
            let compiled =
                compile_component_swc(fixture.template, fixture.selector, fixture.class_name);
            if !compiled.errors.is_empty() {
                return Err(format!(
                    "treaty_ivy (swc emit) compile diagnostics: {}",
                    compiled.errors.join("; ")
                ));
            }
            Ok(compiled.code)
        }
    }
}

#[cfg(feature = "swc")]
pub use imp::SwcBackend;
