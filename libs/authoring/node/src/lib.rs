#[macro_use]
extern crate napi_derive;

use render3::compile::compile_component as r3_compile_component;

/// Result of compiling a component template to Ivy.
#[napi(object)]
pub struct CompiledComponent {
    /// The emitted JavaScript (the `ɵɵdefineComponent({...})` definition).
    pub code: String,
    /// Parse/transform diagnostics (empty on success).
    pub errors: Vec<String>,
}

/// Compile an Angular component template directly to Ivy via the Rust/OXC `render3` compiler.
///
/// `template` is the HTML template source, `selector` the component selector
/// (e.g. `"app-hello"`), and `class_name` the component class identifier.
#[napi]
pub fn compile_component(
    template: String,
    selector: String,
    class_name: String,
) -> CompiledComponent {
    let result = r3_compile_component(&template, &selector, &class_name);
    CompiledComponent {
        code: result.code,
        errors: result.errors,
    }
}
