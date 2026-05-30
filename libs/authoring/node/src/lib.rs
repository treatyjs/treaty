#[macro_use]
extern crate napi_derive;

use render3::compile::compile_component as r3_compile_component;
use render3::source_compile::compile_component_source as r3_compile_component_source;

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

/// Compile an Angular `@Component`/`@Directive` class directly from its TypeScript SOURCE.
///
/// `source` is the full TS file (or snippet) containing exactly one decorated class. Returns the
/// emitted `ɵɵdefineComponent({...})` definition, or a `CompiledComponent` carrying a descriptive
/// error for shapes the source front-end does not yet support (providers, queries, host bindings,
/// `templateUrl`, multi-class files, etc.).
#[napi]
pub fn compile_component_source(source: String) -> CompiledComponent {
    let result = r3_compile_component_source(&source);
    CompiledComponent {
        code: result.code,
        errors: result.errors,
    }
}

/// Compile a `.treaty` single-file component directly from its source.
///
/// `source` is the full `.treaty` file contents and `file_name` its path/name (used for
/// diagnostics). Returns the emitted `ɵɵdefineComponent({...})` definition, or a
/// `CompiledComponent` carrying descriptive errors.
#[napi]
pub fn compile_treaty_file(source: String, file_name: String) -> CompiledComponent {
    let result = rust_authoring::sfc::compile_treaty_file(&source, &file_name);
    CompiledComponent {
        code: result.code,
        errors: result.errors,
    }
}
