//! `rust_authoring` — Treaty's Rust/OXC Angular compiler.
//!
//! Wires the previously-orphaned `angular` (OXC-based Ivy codegen) and `html`
//! modules into the crate so they are actually type-checked and testable.
//! The legacy top-level `parser.rs` targets a stale `treaty` API and is left
//! out of the build until the render3 port replaces it.

pub mod angular;
pub mod html;
pub mod treaty;
