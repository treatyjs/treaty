//! The template-side of view compilation: the template-definition builder and the
//! query-generation helpers. The decorator-facing instruction emitter
//! (`compileComponentFromMetadata`) lives in [`crate::view::compiler`] and is part
//! of the `decorators` layer, not here.
pub mod template;
pub mod queries;
