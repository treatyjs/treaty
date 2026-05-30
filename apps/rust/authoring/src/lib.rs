//! `rust_authoring` — Treaty's Rust/OXC Angular compiler.
//!
//! Wires the previously-orphaned `angular` (OXC-based Ivy codegen) and `html`
//! modules into the crate so they are actually type-checked and testable.
//! The legacy top-level `parser.rs` targets a stale `treaty` API and is left
//! out of the build until the render3 port replaces it.

pub mod angular;
pub mod angular_source;
pub mod authoring;
pub mod html;
pub mod jsx;
pub mod plugin;
pub mod sfc;
pub mod treaty;

/// The result of compiling an authoring source (a `.treaty` SFC or a base Angular `.ts`) once
/// server-only `server { … }` logic has been lifted out via the [`plugin`] system.
///
/// `code` is the compiled client module (the `ɵɵdefineComponent` output with server calls rewritten
/// to their backend client bindings). `server_module` is the generated backend code from the active
/// [`plugin::BackendPlugin`] — `None` when the source declared no `server { … }` block. `errors`
/// carries any diagnostics from the underlying component compile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledAuthoring {
    pub code: String,
    pub server_module: Option<String>,
    pub errors: Vec<String>,
}
