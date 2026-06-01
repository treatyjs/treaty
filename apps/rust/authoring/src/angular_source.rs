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
use treaty_ivy::source_compile::compile_component_source_with_map_and_selector;

use crate::plugin::{extract_server_block, rewrite_call_sites, PluginRegistry};
use crate::sfc::to_kebab_case;
use crate::source_map::redact_server_bodies_in_map;
use crate::CompiledAuthoring;

/// The original-source name embedded in the emitted map's `sources[0]`. The base-Angular path does
/// not yet thread a real file path through to here, so a stable placeholder is used; the generated
/// artifact name (`file`) is left to render3's default.
const SOURCE_NAME: &str = "component.ts";
const GENERATED_NAME: &str = "component.js";

/// Normalize render3's empty-string "no map" sentinel into `None`. render3 returns an empty `map`
/// when compilation failed (it never emits `{}`); anything non-empty is a real v3 JSON document.
fn map_or_none(map: String) -> Option<String> {
    if map.trim().is_empty() {
        None
    } else {
        Some(map)
    }
}

/// The recognized top-level Angular decorator kinds a `.ts` source may carry.
///
/// `Component` is the one kind the `render3` source front-end emits today. The remaining kinds
/// (`Directive` / `Pipe` / `Injectable` / `NgModule`) are recognized so the base-Angular path can
/// make an informed routing decision, but `render3` has no *source* extractor for them yet — their
/// metadata-driven emitters in [`treaty_ivy::pipe_module_injector`] require structured metadata the
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
///   2. `treaty_ivy::source_compile::compile_component_source` compiles the cleaned client source to a
///      `defineComponent`.
///   3. When server functions were present, the active backend plugin — the
///      [`PluginRegistry`](crate::plugin::PluginRegistry) default (axum + typesafe resource HTTP
///      client) — emits a server module + per-fn client bindings, and the lowered `ctx.<fn>(` call
///      sites are rewritten to the plugin-provided binding for `<fn>`. The plugin is never
///      hardcoded; selecting a different backend (e.g. `elysia-eden`) is a registry-name lookup.
///
/// When no `server { … }` block is present the source compiles unchanged and `server_module` is
/// `None`.
pub fn compile_angular_component(source: &str, file_name: &str) -> CompiledAuthoring {
    let registry = PluginRegistry::with_defaults();
    let plugin = registry
        .default_plugin()
        .expect("registry seeded with a default backend plugin");
    compile_angular_component_with(source, file_name, |fns| plugin.emit(fns))
}

/// Like [`compile_angular_component`], but emits server functions through `emit` (the caller's chosen
/// backend) rather than the registry default. Used to opt into a non-default backend such as
/// `elysia-eden` (`PluginRegistry::get("elysia-eden")`).
pub fn compile_angular_component_with(
    source: &str,
    file_name: &str,
    emit: impl FnOnce(&[crate::plugin::ServerFn]) -> crate::plugin::BackendEmit,
) -> CompiledAuthoring {
    let extraction = extract_server_block(source);

    // A SELECTORLESS `@Component` (no `selector` in its decorator) adopts the filename-derived
    // kebab selector — the same Treaty convention the `.treaty`/`.tsx`/`.tjsx` front-ends apply —
    // so a bootstrapped selectorless `.ts` component renders a real host tag instead of Angular's
    // `ng-component` default. A component that DECLARES a selector keeps it (the facade only
    // substitutes when the decorator omits one), so explicit-selector `.ts` files are untouched.
    let default_selector = to_kebab_case(file_name);

    if extraction.server_fns.is_empty() {
        // No server block: compile with the additive v3 map and pass it through UNCHANGED.
        let compiled = compile_component_source_with_map_and_selector(
            &extraction.client_source,
            GENERATED_NAME,
            SOURCE_NAME,
            Some(&default_selector),
        );
        return CompiledAuthoring {
            code: compiled.code,
            server_module: None,
            errors: compiled.errors,
            map: map_or_none(compiled.map),
        };
    }

    let emit = emit(&extraction.server_fns);
    let compiled = compile_component_source_with_map_and_selector(
        &extraction.client_source,
        GENERATED_NAME,
        SOURCE_NAME,
        Some(&default_selector),
    );

    // CLIENT PRIVACY: the map embeds the authoring source as `sourcesContent`. Even though the
    // `server { … }` block was already lifted out of `client_source` before compilation, redact each
    // lifted server-fn body from the map's `sourcesContent` as a defense-in-depth guarantee, blanking
    // the body bytes to spaces so the map's line/column positions stay valid. The server source text
    // never reaches the client map.
    let server_bodies: Vec<String> =
        extraction.server_fns.iter().map(|f| f.source.clone()).collect();
    let map = map_or_none(redact_server_bodies_in_map(&compiled.map, &server_bodies));

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
        map,
    }
}

/// The base-Angular `.ts` front-end entry, broadened across decorator kinds.
///
/// Routing, by the top-level Angular decorator(s) the source carries:
///   * `@Component` — compiled to a `ɵɵdefineComponent` via [`compile_angular_component`]
///     (server-block aware: a `server { … }` block is lifted and routed through the default
///     backend, exactly as before).
///   * `@Directive` / `@Pipe` / `@Injectable` / `@NgModule` — `render3` has metadata-driven emitters
///     for pipes / modules / injectors ([`treaty_ivy::pipe_module_injector`]) but no *source* extractor
///     wiring them up yet, so rather than erroring we emit the source **unchanged** (a faithful
///     pass-through) with no diagnostics. When a source extractor for these lands, this is the one
///     place to route them through.
///   * no Angular decorator at all — a plain `.ts` module — passes through **unchanged**.
///
/// The `file_name` derives the fallback element selector a SELECTORLESS `@Component` adopts (its
/// kebab-case stem, the same Treaty convention the JSX / `.treaty` front-ends use); a `@Component`
/// that declares its own `selector` keeps it. The base-Angular path does not derive a class name
/// from `file_name` (the `@Component` class names itself).
pub fn compile_angular_source(source: &str, file_name: &str) -> CompiledAuthoring {
    // Detect decorators on the SERVER-STRIPPED source: a `server { … }` block is not valid TS, so a
    // `@Component` that colocates one would otherwise fail to parse here and be misrouted to the
    // pass-through path. Lifting the block first lets detection see the real `@Component` and route
    // it to the server-block-aware component path (which re-extracts the block itself).
    let stripped = extract_server_block(source).client_source;
    let kinds = detect_angular_decorators(&stripped);

    // A `@Component` is the only kind the source front-end emits; route it through the existing
    // server-block-aware component path. (If a file mixes `@Component` with other decorators, the
    // component still drives compilation — `compile_component_source` already rejects multi-class
    // files, so the component path will report that faithfully.)
    if kinds.contains(&AngularDecoratorKind::Component) {
        return compile_angular_component(source, file_name);
    }

    // Either no Angular decorator (plain `.ts`) or only decorator kinds without a source extractor
    // yet (`@Directive` / `@Pipe` / `@Injectable` / `@NgModule`): emit the source verbatim so the
    // bundler still gets a usable module, with no spurious diagnostics.
    CompiledAuthoring {
        code: source.to_string(),
        server_module: None,
        errors: Vec::new(),
        map: None,
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

        let out = compile_angular_component(source, "app.component.ts");

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
        let out = compile_angular_component_with(source, "app.component.ts", |fns| elysia.emit(fns));

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
    fn component_without_server_block_carries_a_v3_map() {
        // A plain `@Component` (no server block) compiles WITH the additive v3 map threaded out.
        let source = "import { Component } from '@angular/core';\n\
@Component({ selector: 'app-x', template: '<div>{{x}}</div>' })\n\
export class XComponent { x = 1; }\n";
        let out = compile_angular_component(source, "x.component.ts");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);

        let map = out.map.expect("expected a source map");
        let value: serde_json::Value =
            serde_json::from_str(&map).expect("map should be valid JSON");
        assert_eq!(value["version"], serde_json::json!(3), "not a v3 map: {map}");
        assert!(value.get("sourcesContent").is_some(), "no sourcesContent: {map}");
    }

    #[test]
    fn component_server_block_body_is_absent_from_client_map() {
        // CLIENT PRIVACY: a `@Component` with an inline server fn must compile to a v3 map whose
        // `sourcesContent` does NOT contain the server fn body text.
        let source = "import { Component } from '@angular/core';\n\
server {\n\
  async function save(user: User) { return db.insert(user); }\n\
}\n\
@Component({ template: '<button (click)=\"save(user)\">go</button>' })\n\
export class AppComponent {}\n";

        let out = compile_angular_component(source, "app.component.ts");
        let map = out.map.expect("expected a source map for a server-block component");
        let value: serde_json::Value =
            serde_json::from_str(&map).expect("map should be valid JSON");
        assert_eq!(value["version"], serde_json::json!(3), "not a v3 map: {map}");

        // The server body never appears anywhere in the map's sourcesContent.
        let contents = value["sourcesContent"]
            .as_array()
            .expect("sourcesContent array");
        for c in contents {
            let text = c.as_str().unwrap_or("");
            assert!(!text.contains("db.insert"), "server body leaked into map content: {text}");
            assert!(
                !text.contains("async function save"),
                "server signature leaked into map content: {text}"
            );
        }
    }

    #[test]
    fn angular_without_server_block_has_no_server_module() {
        let source = "import { Component } from '@angular/core';\n\
@Component({ template: '<div></div>' })\n\
export class AppComponent {}\n";
        let out = compile_angular_component(source, "app.component.ts");
        assert!(out.server_module.is_none(), "unexpected server module");
    }

    #[test]
    fn selectorless_component_adopts_filename_kebab_selector() {
        // The reported defect for a SELECTORLESS `@Component` `.ts`: with no `selector` in the
        // decorator, the emit used Angular's `ng-component` no-selector default, so a bootstrapped
        // component rendered a bare `<ng-component>` host. The filename now drives a kebab selector
        // (`log-viewer.component.ts` -> `log-viewer`), removing the `ng-component` host.
        let source = "import { Component } from '@angular/core';\n\
@Component({ template: '<div>{{x}}</div>' })\n\
export class LogViewer { x = 1; }\n";
        let out = compile_angular_source(source, "log-viewer.component.ts");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(
            out.code.contains("selectors: [[\"log-viewer\"]]"),
            "expected filename-derived `log-viewer` selector; got: {}",
            out.code
        );
        assert!(
            !out.code.contains("ng-component"),
            "ng-component default must not survive for a selectorless component; got: {}",
            out.code
        );
    }

    #[test]
    fn explicit_selector_component_is_not_overridden_by_file_name() {
        // A `@Component` that DECLARES its own selector keeps it verbatim — the filename fallback only
        // fills in a MISSING selector, so explicit-selector `.ts` files (the golden-corpus shape) are
        // never rewritten.
        let source = "import { Component } from '@angular/core';\n\
@Component({ selector: 'my-explicit-thing', template: '<div></div>' })\n\
export class Whatever {}\n";
        let out = compile_angular_source(source, "some-other-name.component.ts");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(
            out.code.contains("selectors: [[\"my-explicit-thing\"]]"),
            "explicit selector must be preserved; got: {}",
            out.code
        );
        assert!(
            !out.code.contains("some-other-name"),
            "filename must NOT override an explicit selector; got: {}",
            out.code
        );
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
    fn source_router_routes_component_with_server_block_and_redacts_map() {
        // A `@Component` that colocates a `server { … }` block must route through the component path
        // even via the unified `compile_angular_source` entry (the block is stripped before decorator
        // detection), produce a v3 map, and keep the server body out of that map's sourcesContent.
        let source = "import { Component } from '@angular/core';\n\
server {\n\
  async function save(user: User) { return db.insert(user); }\n\
}\n\
@Component({ template: '<button (click)=\"save(user)\">go</button>' })\n\
export class AppComponent {}\n";

        let out = compile_angular_source(source, "app.component.ts");
        assert!(out.server_module.is_some(), "server block not routed: no server module");
        let map = out.map.expect("expected a v3 map for the routed server-block component");
        let value: serde_json::Value = serde_json::from_str(&map).expect("valid JSON map");
        assert_eq!(value["version"], serde_json::json!(3));
        let joined: String = value["sourcesContent"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!joined.contains("db.insert"), "server body leaked into map: {joined}");
        assert!(!out.code.contains("db.insert"), "server body leaked into client code");
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
