//! Base Angular `.ts` front-end with `server { … }` block support.
//!
//! [`compile_angular_component`] is the server-aware entry point for a standard `@Component` `.ts`
//! source. It mirrors the `.treaty` pipeline in [`crate::sfc`]: lift any `server { … }` block out of
//! the source first, compile the cleaned client source through `render3`, then apply the active
//! backend [`plugin`](crate::plugin) to emit the server module and rewrite client call sites.

use render3::source_compile::compile_component_source;

use crate::plugin::{extract_server_block, rewrite_call_sites, PluginRegistry};
use crate::CompiledAuthoring;

/// Compile a base Angular `@Component` `.ts` source, handling a top-level `server { … }` block.
///
/// Steps:
///   1. [`extract_server_block`] removes any `server { … }` block and parses its functions.
///   2. `render3::source_compile::compile_component_source` compiles the cleaned client source to a
///      `defineComponent`.
///   3. When server functions were present, the active backend plugin — the
///      [`PluginRegistry`](crate::plugin::PluginRegistry) default (axum + typesafe resource HTTP
///      client) — emits a server module + per-fn client bindings, and the lowered `ctx.<fn>(` call
///      sites are rewritten to the plugin-provided binding for `<fn>`. The plugin is never
///      hardcoded; selecting a different backend (e.g. `elysia-eden`) is a registry-name lookup.
///
/// When no `server { … }` block is present the source compiles unchanged and `server_module` is
/// `None`.
pub fn compile_angular_component(source: &str) -> CompiledAuthoring {
    let registry = PluginRegistry::with_defaults();
    let plugin = registry
        .default_plugin()
        .expect("registry seeded with a default backend plugin");
    compile_angular_component_with(source, |fns| plugin.emit(fns))
}

/// Like [`compile_angular_component`], but emits server functions through `emit` (the caller's chosen
/// backend) rather than the registry default. Used to opt into a non-default backend such as
/// `elysia-eden` (`PluginRegistry::get("elysia-eden")`).
pub fn compile_angular_component_with(
    source: &str,
    emit: impl FnOnce(&[crate::plugin::ServerFn]) -> crate::plugin::BackendEmit,
) -> CompiledAuthoring {
    let extraction = extract_server_block(source);

    if extraction.server_fns.is_empty() {
        let compiled = compile_component_source(&extraction.client_source);
        return CompiledAuthoring {
            code: compiled.code,
            server_module: None,
            errors: compiled.errors,
        };
    }

    let emit = emit(&extraction.server_fns);
    let compiled = compile_component_source(&extraction.client_source);

    // render3 emits only the `defineComponent`, and lowers every template reference to a component
    // context member (`save(user)` -> `ctx.save(ctx.user)`). The generic [`rewrite_call_sites`]
    // (which targets *free* identifiers in client JS) handles free references in non-template code;
    // here we additionally swap the lowered `ctx.<fn>` callee to the plugin-provided binding for
    // `<fn>` so a server function invoked from a template/host handler routes through the active
    // backend. The binding text comes straight from the plugin's per-fn `client_bindings` map — no
    // backend path is hardcoded here.
    let mut code = rewrite_call_sites(&compiled.code, &emit.client_bindings);
    for f in &extraction.server_fns {
        if let Some(binding) = emit.client_bindings.get(&f.name) {
            code = code.replace(&format!("ctx.{}(", f.name), &format!("{binding}("));
        }
    }

    CompiledAuthoring {
        code,
        server_module: Some(emit.server_module),
        errors: compiled.errors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn angular_server_block_extracts_route_and_rewrites_call_through_default_axum() {
        // A standard @Component .ts with a server block declaring `save`, plus a usage of `save`
        // inside the component body. The DEFAULT backend (axum + typesafe resource HTTP client) is
        // applied via the PluginRegistry — not a hardcoded Elysia/eden path.
        let source = "import { Component } from '@angular/core';\n\
server {\n\
  async function save(user: User) { return db.insert(user); }\n\
}\n\
@Component({ template: '<button (click)=\"save(user)\">go</button>' })\n\
export class AppComponent {}\n";

        let out = compile_angular_component(source);

        // A server module was generated as a Rust/axum service with the POST route for `save`.
        let server_module = out.server_module.expect("expected a server module");
        assert!(
            server_module.contains("\"/__server/save\""),
            "no save route in axum server module; got: {server_module}"
        );
        assert!(
            server_module.contains("pub fn build_router() -> Router"),
            "no axum router builder in server module; got: {server_module}"
        );
        // The default axum backend is used, NOT Elysia.
        assert!(
            !server_module.contains("new Elysia()"),
            "default path should not emit an Elysia app; got: {server_module}"
        );

        // The compiled client routes the call through the axum typesafe resource client binding
        // (`edenHttpResource` POSTing to `/__server/save`), not the original fn and not an eden path.
        assert!(
            out.code.contains("edenHttpResource") && out.code.contains("'/__server/save'"),
            "call not rewritten to axum resource client; got: {}",
            out.code
        );
        assert!(
            !out.code.contains("client.__server.save.post"),
            "default path leaked the eden binding; got: {}",
            out.code
        );
        // The original server fn body never reaches the client bundle.
        assert!(
            !out.code.contains("db.insert"),
            "server body leaked into client; got: {}",
            out.code
        );
    }

    #[test]
    fn angular_server_block_opt_in_elysia_eden_binding() {
        // Opting into the `elysia-eden` backend by registry name yields the Eden client binding and
        // an Elysia server module instead of the default axum output.
        let source = "import { Component } from '@angular/core';\n\
server {\n\
  async function save(user: User) { return db.insert(user); }\n\
}\n\
@Component({ template: '<button (click)=\"save(user)\">go</button>' })\n\
export class AppComponent {}\n";

        let registry = PluginRegistry::with_defaults();
        let elysia = registry.get("elysia-eden").expect("elysia-eden registered");
        let out = compile_angular_component_with(source, |fns| elysia.emit(fns));

        let server_module = out.server_module.expect("expected a server module");
        assert!(
            server_module.contains("new Elysia()"),
            "no Elysia app in opt-in server module; got: {server_module}"
        );
        assert!(
            server_module.contains(".post('/__server/save'"),
            "no save route in Elysia server module; got: {server_module}"
        );
        // The compiled client routes the call through the Eden client binding.
        assert!(
            out.code.contains("client.__server.save.post"),
            "call not rewritten to eden client; got: {}",
            out.code
        );
        assert!(
            !out.code.contains("db.insert"),
            "server body leaked into client; got: {}",
            out.code
        );
    }

    #[test]
    fn angular_without_server_block_has_no_server_module() {
        let source = "import { Component } from '@angular/core';\n\
@Component({ template: '<div></div>' })\n\
export class AppComponent {}\n";
        let out = compile_angular_component(source);
        assert!(out.server_module.is_none(), "unexpected server module");
    }
}
