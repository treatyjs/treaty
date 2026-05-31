//! View compilation: instruction emitter (compileComponentFromMetadata), template builder, queries.
//!
//! The template-definition builder and query generation moved into the `template`
//! subtree ([`crate::template_mod::view`]) as part of the modular split; they are
//! re-exported here so `crate::view::template` / `crate::view::queries` keep
//! resolving. Only [`compiler`] — the decorator-facing instruction emitter — still
//! physically lives here (it joins the `decorators` layer in a later phase).
pub use crate::template_mod::view::queries;
pub use crate::template_mod::view::template;
pub mod compiler;
