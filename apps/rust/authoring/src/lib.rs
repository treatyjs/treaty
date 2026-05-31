//! `rust_authoring` — Treaty's Rust/OXC Angular compiler.
//!
//! Routes each authoring format to its front-end (see [`authoring::compile_file`]):
//! `.treaty` via [`sfc`], `.tsx`/`.tjsx` via [`jsx`], and base Angular `.ts` via
//! [`angular_source`] — all lowering to Ivy through the `render3` crate. The
//! [`plugin`] system lifts `server { … }` blocks to a backend module and emits
//! the matching client bindings.

pub mod angular_source;
pub mod authoring;
pub mod jsx;
pub mod plugin;
pub mod sfc;
pub mod source_map;
pub mod treaty;

/// The result of compiling an authoring source (a `.treaty` SFC or a base Angular `.ts`) once
/// server-only `server { … }` logic has been lifted out via the [`plugin`] system.
///
/// `code` is the compiled client module (the `ɵɵdefineComponent` output with server calls rewritten
/// to their backend client bindings). `server_module` is the generated backend code from the active
/// [`plugin::BackendPlugin`] — `None` when the source declared no `server { … }` block. `errors`
/// carries any diagnostics from the underlying component compile.
///
/// `map` is the additive Source Map v3 JSON (render3's `SourceMap::to_json`) mapping `code` back to
/// the original authoring source, or `None` when the underlying front-end produced no map. CLIENT
/// PRIVACY: when the source declared a `server { … }` block, every lifted server-fn body has been
/// redacted out of the map's `sourcesContent` before it reaches this field (see
/// [`source_map::redact_server_bodies_in_map`]), so the server source never appears in the client map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledAuthoring {
    pub code: String,
    pub server_module: Option<String>,
    pub errors: Vec<String>,
    pub map: Option<String>,
}
