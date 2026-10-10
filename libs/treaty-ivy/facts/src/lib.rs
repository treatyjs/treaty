//! Plain-data decorator facts and UTF-16 <-> byte span helpers. See [`facts`] for the conventions.

pub mod facts;
pub mod span;

pub use facts::*;
pub use span::{Span, Utf16Index};
