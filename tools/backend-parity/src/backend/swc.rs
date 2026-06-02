//! The **swc** backend — a feature-gated PLACEHOLDER, ready to light up.
//!
//! This module exists only under `#[cfg(feature = "swc")]`. The whole struct is gated, so when the
//! `swc` feature is OFF the backend is not compiled at all and [`enabled_backends`] omits it (with a
//! logged notice) — the harness then runs oxc-only and STILL compiles + runs today, exactly the
//! Phase-1 state in SWC-BACKEND-PLAN.md §5.
//!
//! When the `swc` feature is ON, [`SwcBackend::compile`] currently returns a clear, actionable
//! "not yet implemented" error rather than silently passing. That keeps the gate honest: as soon as
//! the real SWC emit backend (`emitter_swc.rs`, plan §3.3 / phase 2) and parse backend (plan §3.2 /
//! phase 3) land, this method is swapped to call the same `treaty_ivy` entry point built under
//! `--features swc`, and the byte-equality gate ([`crate::parity`]) immediately does real work
//! (oxc-emitted vs swc-emitted Ivy).
//!
//! [`enabled_backends`]: super::enabled_backends

#[cfg(feature = "swc")]
mod imp {
    use super::super::Backend;
    use crate::corpus::Fixture;

    /// The SWC-backed Treaty Ivy compiler backend (placeholder until the SWC port lands).
    pub struct SwcBackend;

    impl Backend for SwcBackend {
        fn name(&self) -> &str {
            "swc"
        }

        fn compile(&self, _fixture: &Fixture) -> Result<String, String> {
            Err(
                "swc backend not yet implemented (see migration/SWC-BACKEND-PLAN.md §3.3 emit \
                 + §3.2 parse / phases 2-3). Once the SWC emit/parse backends land, swap this to \
                 call treaty_ivy built under `--features swc`."
                    .to_string(),
            )
        }
    }
}

#[cfg(feature = "swc")]
pub use imp::SwcBackend;
