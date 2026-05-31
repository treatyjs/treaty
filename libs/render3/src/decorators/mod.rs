//! `decorators` — the decorator → definition layer (Phase 3 of the modular split).
//!
//! This is the layer that turns an Angular decorated class (`@Component` / `@Directive` /
//! `@Pipe` / `@NgModule` / `@Injectable`) into its emitted Ivy definition. It owns:
//!   - [`compiler`]               — the decorator-facing instruction emitter
//!                                  (`compile_component_from_metadata` / `compile_directive_from_metadata`,
//!                                  host-binding builder, the `R3*Metadata` shapes).
//!   - [`pipe_module_injector`]   — `@Pipe` (`ɵɵdefinePipe`) and `@NgModule`
//!                                  (`ɵɵdefineNgModule` + `ɵɵsetNgModuleScope`) emit.
//!   - [`registry`]               — the [`registry::DecoratorCompiler`] plugin trait +
//!                                  [`registry::DecoratorRegistry`] join, mirroring
//!                                  `apps/rust/authoring`'s `AuthoringPlugin` / `AuthoringRegistry`.
//!                                  One plugin per decorator kind compiles an extracted
//!                                  [`registry::ClassMeta`] into a [`registry::CompiledDef`];
//!                                  `source_compile`'s per-class loop dispatches through it so
//!                                  adding a kind is a registration, not a `match` arm.
//!
//! Depends on [`crate::core`] (IR + emit) and [`crate::template_mod`] (template lowering, the
//! template-definition builder, query generation). It knows nothing about the per-FILE driver —
//! that thin "join" stays in [`crate::source_compile`].
//!
//! The crate root re-exports [`compiler`] and [`pipe_module_injector`] from their historical
//! top-level paths (`crate::view::compiler`, `crate::pipe_module_injector`) so call sites outside
//! this subtree are unchanged; the canonical path is now `crate::decorators::…`. The split is
//! structural — emitted code is byte-identical.

pub mod compiler;
pub mod pipe_module_injector;
pub mod registry;
