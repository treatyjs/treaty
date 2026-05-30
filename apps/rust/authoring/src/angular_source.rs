//! Base Angular `.ts` front-end with `server { … }` block support.
//!
//! [`compile_angular_component`] is the server-aware entry point for a standard `@Component` `.ts`
//! source. It mirrors the `.treaty` pipeline in [`crate::sfc`]: lift any `server { … }` block out of
//! the source first, compile the cleaned client source through `render3`, then apply the active
//! backend [`plugin`](crate::plugin) to emit the server module and rewrite client call sites.

use oxc_allocator::Allocator;
use oxc_ast::ast::{Class, Decorator, Expression, Statement};
use oxc_parser::Parser;
use oxc_span::SourceType;
use render3::source_compile::compile_component_source;

use crate::plugin::{extract_server_block, rewrite_call_sites, PluginRegistry};
use crate::CompiledAuthoring;

/// The recognized top-level Angular decorator kinds a `.ts` source may carry.
///
/// `Component` is the one kind the `render3` source front-end emits today. The remaining kinds
/// (`Directive` / `Pipe` / `Injectable` / `NgModule`) are recognized so the base-Angular path can
/// make an informed routing decision, but `render3` has no *source* extractor for them yet — their
/// metadata-driven emitters in [`render3::pipe_module_injector`] require structured metadata the
/// source front-end does not build. Until that lands, a source carrying only these decorators is a
/// faithful pass-through (see [`compile_angular_source`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AngularDecoratorKind {
    Component,
    Directive,
    Pipe,
    Injectable,
    NgModule,
}

impl AngularDecoratorKind {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "Component" => Some(Self::Component),
            "Directive" => Some(Self::Directive),
            "Pipe" => Some(Self::Pipe),
            "Injectable" => Some(Self::Injectable),
            "NgModule" => Some(Self::NgModule),
            _ => None,
        }
    }
}

/// The callee identifier of a decorator, whether `@Foo` (identifier) or `@Foo({...})` (call).
fn decorator_name<'a>(dec: &'a Decorator<'a>) -> Option<&'a str> {
    match &dec.expression {
        Expression::CallExpression(call) => match &call.callee {
            Expression::Identifier(id) => Some(id.name.as_str()),
            _ => None,
        },
        Expression::Identifier(id) => Some(id.name.as_str()),
        _ => None,
    }
}

/// Pull the class out of a top-level statement (plain / exported / default-exported declaration).
fn statement_class<'a>(stmt: &'a Statement<'a>) -> Option<&'a Class<'a>> {
    match stmt {
        Statement::ClassDeclaration(c) => Some(c.as_ref()),
        Statement::ExportNamedDeclaration(export) => match &export.declaration {
            Some(oxc_ast::ast::Declaration::ClassDeclaration(c)) => Some(c.as_ref()),
            _ => None,
        },
        Statement::ExportDefaultDeclaration(export) => {
            if let oxc_ast::ast::ExportDefaultDeclarationKind::ClassDeclaration(c) =
                &export.declaration
            {
                Some(c.as_ref())
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Scan a `.ts` source for the Angular decorator kinds carried by its top-level classes.
///
/// Parses with `oxc` (TypeScript) and collects every recognized `@Component` / `@Directive` /
/// `@Pipe` / `@Injectable` / `@NgModule` decorator across all top-level (plain / exported /
/// default-exported) classes. A parse failure or a source with no Angular decorator yields an empty
/// set, which the router treats as "nothing to compile — pass through unchanged".
fn detect_angular_decorators(source: &str) -> Vec<AngularDecoratorKind> {
    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true);
    let ret = Parser::new(&allocator, source, source_type).parse();
    // A source we cannot parse is not Angular we can compile; let it pass through unchanged rather
    // than surfacing a parse error the caller never asked for.
    if !ret.errors.is_empty() {
        return Vec::new();
    }

    let mut kinds = Vec::new();
    for stmt in &ret.program.body {
        let Some(class) = statement_class(stmt) else {
            continue;
        };
        for dec in &class.decorators {
            if let Some(kind) = decorator_name(dec).and_then(AngularDecoratorKind::from_name) {
                kinds.push(kind);
            }
        }
    }
    kinds
}

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

/// The base-Angular `.ts` front-end entry, broadened across decorator kinds.
///
/// Routing, by the top-level Angular decorator(s) the source carries:
///   * `@Component` — compiled to a `ɵɵdefineComponent` via [`compile_angular_component`]
///     (server-block aware: a `server { … }` block is lifted and routed through the default
///     backend, exactly as before).
///   * `@Directive` / `@Pipe` / `@Injectable` / `@NgModule` — `render3` has metadata-driven emitters
///     for pipes / modules / injectors ([`render3::pipe_module_injector`]) but no *source* extractor
///     wiring them up yet, so rather than erroring we emit the source **unchanged** (a faithful
///     pass-through) with no diagnostics. When a source extractor for these lands, this is the one
///     place to route them through.
///   * no Angular decorator at all — a plain `.ts` module — passes through **unchanged**.
///
/// The `file_name` is accepted for parity with the other authoring plugins (and future diagnostics);
/// the base-Angular path does not derive a class name from it (the `@Component` class names itself).
pub fn compile_angular_source(source: &str, file_name: &str) -> CompiledAuthoring {
    let _ = file_name;
    let kinds = detect_angular_decorators(source);

    // A `@Component` is the only kind the source front-end emits; route it through the existing
    // server-block-aware component path. (If a file mixes `@Component` with other decorators, the
    // component still drives compilation — `compile_component_source` already rejects multi-class
    // files, so the component path will report that faithfully.)
    if kinds.contains(&AngularDecoratorKind::Component) {
        return compile_angular_component(source);
    }

    // Either no Angular decorator (plain `.ts`) or only decorator kinds without a source extractor
    // yet (`@Directive` / `@Pipe` / `@Injectable` / `@NgModule`): emit the source verbatim so the
    // bundler still gets a usable module, with no spurious diagnostics.
    CompiledAuthoring {
        code: source.to_string(),
        server_module: None,
        errors: Vec::new(),
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

    const DEFINE: &str = "\u{0275}\u{0275}defineComponent";

    #[test]
    fn source_router_compiles_component_to_define_component() {
        // A `@Component` `.ts` routes through the component path and emits a `defineComponent`.
        let source = "import { Component } from '@angular/core';\n\
@Component({ selector: 'app-x', template: '<div>{{x}}</div>' })\n\
export class XComponent { x = 1; }\n";
        let out = compile_angular_source(source, "x.component.ts");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(out.code.contains(DEFINE), "no defineComponent; got: {}", out.code);
    }

    #[test]
    fn source_router_passes_plain_ts_through_unchanged() {
        // A plain `.ts` with no Angular decorator must pass through verbatim — no compile, no errors.
        let source = "export const add = (a: number, b: number): number => a + b;\n";
        let out = compile_angular_source(source, "math.ts");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(out.server_module.is_none(), "unexpected server module");
        assert_eq!(out.code, source, "plain .ts was not passed through unchanged");
    }

    #[test]
    fn source_router_passes_pipe_directive_injectable_module_through_unchanged() {
        // Decorator kinds without a source extractor yet are faithful pass-throughs (no error).
        for source in [
            "import { Pipe } from '@angular/core';\n\
@Pipe({ name: 'cap' })\nexport class CapPipe { transform(v: string) { return v; } }\n",
            "import { Directive } from '@angular/core';\n\
@Directive({ selector: '[foo]' })\nexport class FooDirective {}\n",
            "import { Injectable } from '@angular/core';\n\
@Injectable({ providedIn: 'root' })\nexport class DataService {}\n",
            "import { NgModule } from '@angular/core';\n\
@NgModule({ declarations: [] })\nexport class AppModule {}\n",
        ] {
            let out = compile_angular_source(source, "x.ts");
            assert!(out.errors.is_empty(), "unexpected errors for {source:?}: {:?}", out.errors);
            assert_eq!(out.code, source, "source was not passed through unchanged: {source:?}");
        }
    }
}
