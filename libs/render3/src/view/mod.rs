//! View compilation: instruction emitter (compileComponentFromMetadata), template builder, queries.
//!
//! The template-definition builder and query generation live in the `treaty_ivy_template` crate
//! (`treaty_ivy_template::view`); the decorator-facing instruction emitter lives in the
//! `decorators` subtree ([`crate::decorators::compiler`]) as part of the modular split. Both are
//! re-exported here so `crate::view::template`, `crate::view::queries`, and `crate::view::compiler`
//! keep resolving exactly as before. This module is now purely a compatibility facade — every
//! member physically lives in `treaty_ivy_template` or `decorators`.
pub use crate::decorators::compiler;
pub use treaty_ivy_template::view::queries;
pub use treaty_ivy_template::view::template;
