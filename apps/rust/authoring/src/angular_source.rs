//! Base Angular `.ts` front-end with `server { … }` block support.
//!
//! [`compile_angular_component`] is the server-aware entry point for a standard `@Component` `.ts`
//! source. It mirrors the `.treaty` pipeline in [`crate::sfc`]: lift any `server { … }` block out of
//! the source first, compile the cleaned client source through `render3`, then apply the active
//! backend [`plugin`](crate::plugin) to emit the server module and rewrite client call sites.

use render3::source_compile::compile_component_source;

use crate::plugin::{extract_server_block, rewrite_call_sites, BackendPlugin, ElysiaEdenPlugin};
use crate::CompiledAuthoring;

/// Compile a base Angular `@Component` `.ts` source, handling a top-level `server { … }` block.
///
/// Steps:
///   1. [`extract_server_block`] removes any `server { … }` block and parses its functions.
///   2. `render3::source_compile::compile_component_source` compiles the cleaned client source to a
///      `defineComponent`.
///   3. When server functions were present, the reference [`ElysiaEdenPlugin`] emits a server module
///      + client bindings, and [`rewrite_call_sites`] rewrites free references to each server fn in
///      the compiled client code to its Eden client call.
///
/// When no `server { … }` block is present the source compiles unchanged and `server_module` is
/// `None`.
pub fn compile_angular_component(source: &str) -> CompiledAuthoring {
    let extraction = extract_server_block(source);

    if extraction.server_fns.is_empty() {
        let compiled = compile_component_source(&extraction.client_source);
        return CompiledAuthoring {
            code: compiled.code,
            server_module: None,
            errors: compiled.errors,
        };
    }

    let emit = ElysiaEdenPlugin.emit(&extraction.server_fns);
    let compiled = compile_component_source(&extraction.client_source);

    // render3 emits only the `defineComponent`, and lowers every template reference to a component
    // context member (`save(user)` -> `ctx.save(ctx.user)`). The generic [`rewrite_call_sites`]
    // (which targets *free* identifiers in client JS) handles free references in non-template code;
    // here we additionally swap the lowered `ctx.<fn>` callee to the Eden client call so a server
    // function invoked from a template/host handler routes through the backend.
    let mut code = rewrite_call_sites(&compiled.code, &emit.client_bindings);
    for f in &extraction.server_fns {
        code = code.replace(
            &format!("ctx.{}(", f.name),
            &format!("client.__server.{}.post(", f.name),
        );
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
    fn angular_server_block_extracts_route_and_rewrites_call() {
        // A standard @Component .ts with a server block declaring `save`, plus a usage of `save`
        // inside the component body.
        let source = "import { Component } from '@angular/core';\n\
server {\n\
  async function save(user: User) { return db.insert(user); }\n\
}\n\
@Component({ template: '<button (click)=\"save(user)\">go</button>' })\n\
export class AppComponent {}\n";

        let out = compile_angular_component(source);

        // A server module was generated with the Elysia route for `save`.
        let server_module = out.server_module.expect("expected a server module");
        assert!(
            server_module.contains(".post('/__server/save'"),
            "no save route in server module; got: {server_module}"
        );
        assert!(
            server_module.contains("new Elysia()"),
            "no Elysia app in server module; got: {server_module}"
        );

        // The compiled client routes the call through the Eden client, not the original fn.
        assert!(
            out.code.contains("client.__server.save.post"),
            "call not rewritten to eden client; got: {}",
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
    fn angular_without_server_block_has_no_server_module() {
        let source = "import { Component } from '@angular/core';\n\
@Component({ template: '<div></div>' })\n\
export class AppComponent {}\n";
        let out = compile_angular_component(source);
        assert!(out.server_module.is_none(), "unexpected server module");
    }
}
