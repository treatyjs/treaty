//! `treaty_ivy_ng_bridge`: Angular's Rust decorator analyser (ng-analyze) as an opt-in source of
//! decorator facts for Treaty's compiler.
//!
//! The fact types and span helpers live in the dependency-free crate `treaty_ivy_facts`, re-exported
//! here as [`facts`] and [`span`]. The analyser integration ([`analyze`], [`adapt`]) is compiled only
//! with the `analyze` feature, which is what pulls in ng-analyze (and through it oxc 0.144).
//! See `README.md` in this crate for the reuse boundary, the pinned rev and the parity numbers.

pub use treaty_ivy_facts as facts;
pub use treaty_ivy_facts::span;

#[cfg(feature = "analyze")]
pub mod analyze;
#[cfg(feature = "analyze")]
pub mod adapt;

#[cfg(feature = "analyze")]
pub use adapt::{extract, FileFacts};
