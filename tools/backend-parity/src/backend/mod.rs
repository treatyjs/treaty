//! The pluggable **backend** seam.
//!
//! A [`Backend`] is a single parser+codegen engine for Treaty's Ivy compiler. The whole point of
//! `backend-parity` is to compile the SAME [`Fixture`](crate::corpus::Fixture) through every ENABLED
//! backend and assert their emitted Ivy is byte-identical (SWC-BACKEND-PLAN.md §3.4 / §4).
//!
//! Today exactly one backend is real:
//!   * [`OxcBackend`] — the default oxc engine; calls `treaty_ivy`'s component-compile entry point.
//!
//! A second is a feature-gated placeholder, ready to light up the moment the SWC port lands:
//!   * [`swc::SwcBackend`] — `#[cfg(feature = "swc")]`; returns a clear "not yet implemented" error.
//!     When the `swc` feature is OFF the backend is omitted entirely (see [`enabled_backends`]),
//!     with a logged notice, so the harness compiles + runs on oxc alone TODAY.
//!
//! ## Adding a future target (e.g. the React emitter)
//!
//! A future `ReactEmitter` target (the React-emit work called for in SWC-BACKEND-PLAN.md §3.3, which
//! "must be matched here too") plugs in the EXACT same way as `SwcBackend`: add a feature-gated
//! module with a struct implementing [`Backend`], push it into [`enabled_backends`] under its
//! feature cfg, and the pairwise byte-equality gate ([`crate::parity`]) compares it against every
//! other enabled backend automatically — no gate change needed. The trait is the only contract.

use crate::corpus::Fixture;

pub mod oxc;
pub mod swc;

/// A single compiler backend (one parser + codegen engine).
///
/// The contract is intentionally tiny — exactly what the parity gate needs: a stable [`name`](Self::name)
/// and a [`compile`](Self::compile) that turns one fixture into its emitted Ivy text (or an error
/// string). Every backend that can be enabled implements this; the gate is engine-agnostic.
pub trait Backend {
    /// Stable, human-readable backend name (used in reports + as the diff key, e.g. `"oxc"`).
    fn name(&self) -> &str;

    /// Compile one fixture to its emitted Ivy `ɵɵdefineComponent({...})` text.
    ///
    /// Returns `Ok(code)` with the emitted JS, or `Err(message)` describing why this backend could
    /// not produce output (e.g. a not-yet-implemented backend, or a compiler diagnostic). The gate
    /// treats an `Err` as a parity failure for that fixture.
    fn compile(&self, fixture: &Fixture) -> Result<String, String>;
}

/// The set of backends ENABLED for this build, in deterministic order (oxc first, the reference).
///
/// Feature-gated: `oxc` is always present (the default + reference). `swc` is appended only when the
/// `swc` feature is on; when it is off we log a one-line NOTICE to stderr (so a CI/dev run makes it
/// explicit that the swc side was skipped, not silently missing) and the gate runs oxc-only —
/// trivially "100% parity" against itself, which is the Phase-1 state in SWC-BACKEND-PLAN.md §5.
pub fn enabled_backends() -> Vec<Box<dyn Backend>> {
    let mut backends: Vec<Box<dyn Backend>> = Vec::new();

    #[cfg(feature = "oxc")]
    backends.push(Box::new(oxc::OxcBackend));

    #[cfg(feature = "swc")]
    backends.push(Box::new(swc::SwcBackend));

    #[cfg(not(feature = "swc"))]
    eprintln!(
        "backend-parity: NOTICE — `swc` feature OFF; SKIPPING the swc backend. \
         Running oxc-only (ready to diff swc once it lands — see migration/SWC-BACKEND-PLAN.md §5). \
         Enable with `--features swc` after the SWC emit/parse backends are implemented."
    );

    backends
}
