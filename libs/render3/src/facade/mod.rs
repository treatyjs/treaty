//! `facade` — the thin "join" of the modular split (Phase 4).
//!
//! This is the top of the dependency DAG (`core <- template <- decorators <- facade`): the
//! per-FILE compile entry points that stitch the lower layers together. It owns no metadata
//! extraction or emit logic of its own — it scans a parsed module for decorated classes, dispatches
//! each class through the [`crate::decorators::registry::DecoratorRegistry`] (one
//! [`crate::decorators::registry::DecoratorCompiler`] plugin per kind), and re-assembles the
//! complete ES module around the kept (decorator-stripped) class declarations.
//!
//! Two members:
//!   - [`compile`]         — the template-only end-to-end helper
//!                           ([`compile::compile_component`]) plus the shared `RealTemplateBuilder`
//!                           glue and the selectorless auto-import resolution the source front-end
//!                           reuses.
//!   - [`source_compile`]  — the `@Component`/`@Directive`/`@Pipe`/`@NgModule` TypeScript SOURCE
//!                           front-end ([`source_compile::compile_component_source`] /
//!                           [`source_compile::compile_component_source_with_map`]): oxc-parse the
//!                           module, register the per-decorator plugins, dispatch each class through
//!                           the registry, and assemble the augmented module.
//!
//! Depends on [`treaty_ivy_core`], [`crate::template_mod`] and [`crate::decorators`]. The crate root
//! re-exports both members from their historical top-level paths (`crate::compile`,
//! `crate::source_compile`) so every call site — inside and outside the crate (e.g.
//! `apps/rust/authoring`) — is unchanged. The split is structural: emitted code is byte-identical.

pub mod compile;
pub mod source_compile;
