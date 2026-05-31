//! `treaty_ivy_decorators` — the decorator → definition layer (Phase 3 of the
//! modular split).
//!
//! This is the layer that turns an Angular decorated class (`@Component` /
//! `@Directive` / `@Pipe` / `@NgModule` / `@Injectable`) into its emitted Ivy
//! definition. It owns:
//!   - [`compiler`]               — the decorator-facing instruction emitter
//!                                  (`compile_component_from_metadata` /
//!                                  `compile_directive_from_metadata`, host-binding builder,
//!                                  the `R3*Metadata` shapes).
//!   - [`pipe_module_injector`]   — `@Pipe` (`ɵɵdefinePipe`) and `@NgModule`
//!                                  (`ɵɵdefineNgModule` + `ɵɵsetNgModuleScope`) emit.
//!   - [`registry`]               — the [`registry::DecoratorCompiler`] plugin trait +
//!                                  [`registry::DecoratorRegistry`] join, mirroring
//!                                  `apps/rust/authoring`'s `AuthoringPlugin` / `AuthoringRegistry`.
//!                                  One plugin per decorator kind compiles an extracted
//!                                  [`registry::ClassMeta`] into a [`registry::CompiledDef`];
//!                                  the per-FILE driver's per-class loop dispatches through it so
//!                                  adding a kind is a registration, not a `match` arm.
//!
//! Depends on [`treaty_ivy_core`] (IR + emit) and [`treaty_ivy_template`]
//! (template lowering, the template-definition builder, query generation). It
//! knows nothing about the per-FILE driver — that thin "join" stays in the
//! facade crate `treaty_ivy` (`treaty_ivy::source_compile`).
//!
//! The lower crates are re-exported below from their historical top-level paths
//! (`crate::output_ast`, `crate::util`, `crate::binder`, `crate::view::template`,
//! …) so the module bodies need no per-line edit across the carve. The split is
//! structural — emitted code is byte-identical.

// Historical core aliases — every `crate::<core-module>` reference in this
// crate's decorator code resolves to the corresponding `treaty_ivy_core` module
// via these re-exports.
pub use treaty_ivy_core::expression;
pub use treaty_ivy_core::expression_converter;
pub use treaty_ivy_core::factory;
pub use treaty_ivy_core::identifiers;
pub use treaty_ivy_core::output;
pub use treaty_ivy_core::output_ast;
pub use treaty_ivy_core::util;

// Historical template aliases — `crate::template::r3_ast` and
// `crate::binder::…` resolve to the corresponding `treaty_ivy_template` modules.
pub use treaty_ivy_template::binder;
pub use treaty_ivy_template::template;

pub mod compiler;
pub mod pipe_module_injector;
pub mod registry;

// `view` is the historical compatibility facade. Within this crate the
// instruction emitter is the local [`compiler`] module, while the
// template-definition builder and query generation live in
// `treaty_ivy_template`. Re-export all three so `crate::view::compiler`,
// `crate::view::template`, and `crate::view::queries` keep resolving exactly as
// before. (The facade crate rebuilds the same `view` module for its consumers.)
pub mod view {
    pub use crate::compiler;
    pub use treaty_ivy_template::view::queries;
    pub use treaty_ivy_template::view::template;
}
