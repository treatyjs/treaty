//! The **oxc** backend — REAL and the reference.
//!
//! This is the default backend that ships today (SWC-BACKEND-PLAN.md §0): it drives Treaty's actual
//! Ivy compiler over the oxc parse/codegen pipeline. It calls the facade crate `treaty_ivy`'s
//! component-compile entry point — `treaty_ivy::compile::compile_component(template, selector,
//! className) -> CompiledComponent { code, errors }` — for each fixture, returning the emitted
//! `ɵɵdefineComponent({...})` text.
//!
//! No invented API: `compile_component` is the public, unit-tested end-to-end pipeline glue in
//! `libs/treaty-ivy/facade/src/compile.rs` (re-exported as `treaty_ivy::compile`). It is the same
//! entry point the parity oracle (`parity.mjs`) exercises via the NAPI addon.

use treaty_ivy::compile::compile_component;

use super::Backend;
use crate::corpus::Fixture;

/// The default oxc-backed Treaty Ivy compiler backend.
pub struct OxcBackend;

impl Backend for OxcBackend {
    fn name(&self) -> &str {
        "oxc"
    }

    fn compile(&self, fixture: &Fixture) -> Result<String, String> {
        let compiled = compile_component(fixture.template, fixture.selector, fixture.class_name);
        // A template-parse / transform diagnostic is surfaced as an error for this fixture: the
        // backend could not faithfully compile it, so it cannot participate in a byte-equality
        // claim. (Today every corpus fixture compiles cleanly.)
        if !compiled.errors.is_empty() {
            return Err(format!(
                "treaty_ivy compile diagnostics: {}",
                compiled.errors.join("; ")
            ));
        }
        Ok(compiled.code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oxc_backend_compiles_interpolation_fixture() {
        let fixture = Fixture {
            id: "interpolation",
            template: "<div>{{name}}</div>",
            selector: "app-hello",
            class_name: "HelloComponent",
        };
        let code = OxcBackend.compile(&fixture).expect("oxc backend must compile the fixture");
        assert!(code.contains("\u{0275}\u{0275}defineComponent"), "got: {code}");
        assert!(code.contains("HelloComponent"), "got: {code}");
    }
}
