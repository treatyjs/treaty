//! View compilation: instruction emitter (compileComponentFromMetadata), template builder, queries.
//!
//! The template-definition builder and query generation moved into the `template` subtree
//! ([`crate::template_mod::view`]); the decorator-facing instruction emitter moved into the
//! `decorators` subtree ([`crate::decorators::compiler`]) as part of the modular split. Both are
//! re-exported here so `crate::view::template`, `crate::view::queries`, and `crate::view::compiler`
//! keep resolving exactly as before. This module is now purely a compatibility facade — every
//! member physically lives in `template_mod` or `decorators`.
pub use crate::decorators::compiler;
pub use crate::template_mod::view::queries;
pub use crate::template_mod::view::template;
