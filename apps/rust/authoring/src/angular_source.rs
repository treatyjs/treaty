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
use crate::sfc::to_multi_selector;
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
/// EVERY kind — `Component` / `Directive` / `Pipe` / `Injectable` / `NgModule` — is lowered to its
/// Ivy definition by the `treaty_ivy::source_compile` driver (a `@Component` → `ɵɵdefineComponent`,
/// `@Directive` → `ɵɵdefineDirective`, `@Pipe` → `ɵɵdefinePipe`, `@Injectable` → `ɵfac` +
/// `ɵɵdefineInjectable`, `@NgModule` → `ɵɵdefineNgModule`). Recognition here drives the routing
/// decision in [`compile_angular_source`]: a source carrying ANY of these is compiled to Ivy (so it
/// never falls to Angular's JIT at runtime), and a source carrying none is a plain `.ts` pass-through.
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

/// Whether `source` carries a top-level `@Component`-decorated class. The JSX front-end uses this to
/// route a `.tsx`/`.tjsx` that contains a base-Angular `@Component` class (rather than the bare
/// function/arrow JSX form) here instead of erroring with "no component found".
pub fn has_angular_component(source: &str) -> bool {
    detect_angular_decorators(source).contains(&AngularDecoratorKind::Component)
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
    // multi-selector (kebab/camel/Pascal) — the same Treaty convention the `.treaty`/`.tsx`/`.tjsx`
    // front-ends apply — so a bootstrapped selectorless `.ts` component renders a real host tag
    // instead of Angular's `ng-component` default, and a parent may reference it by any name spelling.
    // A component that DECLARES a selector keeps it (the facade only substitutes when the decorator
    // omits one), so explicit-selector `.ts` files are untouched.
    let default_selector = to_multi_selector(file_name);

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
    let mut server_bodies: Vec<String> =
        extraction.server_fns.iter().map(|f| f.source.clone()).collect();
    // Also redact the verbatim original declaration text (directive included), which is what the map's
    // `sourcesContent` embeds, so a `'use server'` marker fn beside the `@Component` is fully blanked.
    server_bodies.extend(extraction.server_fns.iter().map(|f| f.verbatim_source.clone()));
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

    // A lifted server fn that is a SIBLING module export (a top-level `$$` / `'use server'` fn declared
    // beside the `@Component`, not invoked from the component itself) was removed by the lift, so a
    // consumer that `import { loadUser$$ } from './app.component'` would now receive `undefined`.
    // Re-export each such fn as its client binding so the consumer transparently gets the RPC stub —
    // the SAME wiring the plain-`.ts` and JSX paths apply, via the shared
    // [`crate::plugin::export_server_fn_bindings`]. A fn already rewritten in-place (a free / `ctx.<fn>`
    // call) is skipped, so there is no double-binding.
    code = crate::plugin::export_server_fn_bindings(&code, &extraction.server_fns, &emit.client_bindings);

    // The rewritten call sites now reference the real `@treaty/httpclient` resource helper the
    // bindings wrap; prepend a real `import` of it so the module resolves the binding at boot rather
    // than throwing `<symbol> is not defined`. Keying off the emitted code keeps a non-axum backend's
    // distinct binding shape free of an unused import.
    let imports = crate::plugin::client_runtime_imports_for_code(&code);
    if !imports.is_empty() {
        code = format!("{imports}\n{code}");
    }

    CompiledAuthoring {
        code,
        server_module: Some(emit.server_module),
        errors: compiled.errors,
        map,
    }
}

/// The base-Angular `.ts` front-end entry, lowering EVERY Angular decorator kind to Ivy.
///
/// Routing, by the top-level Angular decorator(s) the source carries:
///   * `@Component` — compiled to a `ɵɵdefineComponent` via [`compile_angular_component`]
///     (server-block aware: a `server { … }` block is lifted and routed through the default
///     backend, exactly as before).
///   * `@Directive` / `@Pipe` / `@Injectable` / `@NgModule` (and multi-class files mixing kinds) —
///     compiled to their Ivy definitions (`ɵɵdefineDirective` / `ɵɵdefinePipe` / `ɵfac` +
///     `ɵɵdefineInjectable` / `ɵɵdefineNgModule`) through the SAME `treaty_ivy::source_compile`
///     driver the component path uses. The driver lowers every decorated class in source order, so
///     the emitted module carries a real Ivy definition for each class and the raw `@Directive` /
///     `@Pipe` / `@Injectable` / `@NgModule` decorator is stripped — there is no surviving decorator
///     that would push Angular to JIT (and crash for want of `@angular/compiler`) at runtime.
///   * no Angular decorator at all — a plain `.ts` module — passes through **unchanged**.
///
/// The `file_name` derives the fallback element selector a SELECTORLESS `@Component` adopts (its
/// kebab-case stem, the same Treaty convention the JSX / `.treaty` front-ends use); a `@Component`
/// that declares its own `selector` keeps it. A `@Directive` is never given a fallback selector
/// (directives are legitimately class-only), so passing the kebab stem through is harmless for the
/// non-component kinds. The base-Angular path does not derive a class name from `file_name`.
pub fn compile_angular_source(source: &str, file_name: &str) -> CompiledAuthoring {
    // Detect decorators on the SERVER-STRIPPED source: a `server { … }` block is not valid TS, so a
    // `@Component` that colocates one would otherwise fail to parse here and be misrouted to the
    // pass-through path. Lifting the block first lets detection see the real `@Component` and route
    // it to the server-block-aware component path (which re-extracts the block itself).
    let stripped = extract_server_block(source).client_source;
    let kinds = detect_angular_decorators(&stripped);

    // A `@Component` may colocate a `server { … }` block, so it keeps the dedicated server-block-aware
    // path (which lifts the block, applies the active backend plugin, and rewrites call sites). The
    // `treaty_ivy` driver behind that path already lowers every OTHER decorated class in the same
    // file too, so a file mixing `@Component` with `@Directive`/`@Pipe`/`@Injectable`/`@NgModule`
    // emits an Ivy definition for each.
    if kinds.contains(&AngularDecoratorKind::Component) {
        return compile_angular_component(source, file_name);
    }

    // No `@Component`, but at least one `@Directive`/`@Pipe`/`@Injectable`/`@NgModule`: route through
    // the SAME `treaty_ivy::source_compile` driver so each decorated class lowers to its Ivy
    // definition (`ɵɵdefineDirective`/`ɵɵdefinePipe`/`ɵfac`+`ɵɵdefineInjectable`/`ɵɵdefineNgModule`).
    // These kinds never carry a `server { … }` block, so the cleaned client source IS the source.
    if !kinds.is_empty() {
        let default_selector = to_multi_selector(file_name);
        let compiled = compile_component_source_with_map_and_selector(
            source,
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

    // No Angular decorator at all (a plain `.ts` module). It may still be a SERVER module: a file-level
    // `'use server'` directive, an inline `server { … }` block, a `'use server'` body directive, or a
    // `$$`-suffixed export. Extract every such server fn through the active backend plugin so the
    // client keeps ONLY the typed RPC bindings and the bodies are lifted to the backend artifact.
    let extraction = extract_server_block(source);
    if !extraction.server_fns.is_empty() {
        return compile_plain_ts_server_module(source, &extraction, file_name);
    }

    // A genuinely plain `.ts` module: emit the source verbatim so the bundler still gets a usable
    // module, with no spurious diagnostics.
    CompiledAuthoring {
        code: source.to_string(),
        server_module: None,
        errors: Vec::new(),
        map: None,
    }
}

/// Emit a server module + a client stub for a plain (non-Angular) `.ts` module whose top-level
/// declarations were lifted as server fns (a file-level `'use server'` module, a `$$`-suffixed
/// export, or an inline `server { … }` block in a plain module).
///
/// The backend [`PluginRegistry`] default (axum) generates the server artifact and the per-fn client
/// bindings. The client module is the source with every server-fn DECLARATION removed
/// (`extraction.client_source`), then each lifted fn name re-exported as its typed client binding —
/// so a consumer that `import { streamLogs } from './logs.stream'` receives the RPC stub (an
/// `EventSource`/`fetch` resource), never the original body. The server-fn body statements are
/// therefore ABSENT from the emitted client code.
fn compile_plain_ts_server_module(
    original_source: &str,
    extraction: &crate::plugin::ServerExtraction,
    _file_name: &str,
) -> CompiledAuthoring {
    let registry = PluginRegistry::with_defaults();
    let plugin = registry
        .default_plugin()
        .expect("registry seeded with a default backend plugin");
    let emit = plugin.emit(&extraction.server_fns);

    // Start from the client source with every server-fn declaration already removed, then rewrite any
    // remaining FREE references to a lifted fn to its client binding (so an in-module caller routes
    // through the backend rather than dangling on the now-absent declaration).
    let code = rewrite_call_sites(&extraction.client_source, &emit.client_bindings);

    // Re-export each lifted server fn as its typed client binding so module consumers keep importing
    // the same name and transparently get the RPC stub. The binding is the plugin-provided client
    // expression (a fetch/EventSource resource factory) — the body never appears here. Shared with
    // every other front-end via [`crate::plugin::export_server_fn_bindings`].
    let mut code = crate::plugin::export_server_fn_bindings(&code, &extraction.server_fns, &emit.client_bindings);

    // The emitted bindings reference the real `@treaty/httpclient` resource helper
    // (`edenPromiseResource`). Prepend a real `import` of it so the client module resolves the binding
    // the moment it is imported (closing the `<symbol> is not defined` boot crash). The import is empty
    // when no binding symbol is referenced.
    let imports = crate::plugin::client_runtime_imports_for_code(&code);
    if !imports.is_empty() {
        code = format!("{imports}\n{code}");
    }

    // CLIENT PRIVACY: emit a v3 client map whose embedded `sourcesContent` is the ORIGINAL authoring
    // source with every lifted server-fn body blanked to position-preserving whitespace — the same
    // defense-in-depth guarantee the inline `server { … }` path applies via
    // `redact_server_bodies_in_map`. Without this, a bundler that re-embeds the authoring `.ts` as the
    // map's `sourcesContent` would leak the server bodies (and any secret in them) through the map even
    // though the client CODE no longer contains them.
    let mut server_bodies: Vec<String> =
        extraction.server_fns.iter().map(|f| f.source.clone()).collect();
    // Also redact the VERBATIM original declaration text (directive included) — what the map's
    // `sourcesContent` actually embeds — so a `'use server'` marker fn (whose stripped `source` would
    // not match the original) is still fully blanked.
    server_bodies.extend(extraction.server_fns.iter().map(|f| f.verbatim_source.clone()));
    // Also redact every stripped NON-fn server-only top-level statement (e.g. a `const DB_API_KEY = …`
    // beside the fns in a file-level `'use server'` module). These never reach the client CODE, but the
    // map's embedded `sourcesContent` is the ORIGINAL source, so without redacting them too the secret
    // would survive in the map even though the code is clean.
    server_bodies.extend(extraction.server_only_sources.iter().cloned());
    let map = crate::source_map::client_map_with_redacted_source(
        original_source,
        SOURCE_NAME,
        GENERATED_NAME,
        &server_bodies,
    );

    CompiledAuthoring {
        code,
        server_module: Some(emit.server_module),
        errors: Vec::new(),
        map: map_or_none(map),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse `code` (TypeScript) and assert it has no parse errors — proving the emitted client module
    /// is syntactically valid by building the AST, never a regex.
    fn assert_unified_client_parses(code: &str) {
        let allocator = Allocator::default();
        let source_type = SourceType::default().with_typescript(true);
        let ret = Parser::new(&allocator, code, source_type).parse();
        assert!(
            ret.errors.is_empty(),
            "emitted client did not parse: {:?}\n--- code ---\n{code}",
            ret.errors.iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn unified_component_with_dollar_suffix_fn_extracts_binds_and_imports() {
        // MATRIX (@Component + $$): a SIBLING `$$`-suffixed server fn declared beside a `@Component`
        // must be extracted to the server module, its body ABSENT from the client, and a client binding
        // re-exported + the resource helper imported — the SAME unified wiring every front-end applies.
        let source = "import { Component } from '@angular/core';\n\
export async function loadUser$$(id: number) { return db.users.find(id); }\n\
@Component({ template: '<div>{{ x }}</div>' })\n\
export class AppComponent { x = 1; }\n";
        let out = compile_angular_component(source, "app.component.ts");

        let server_module = out.server_module.expect("$$ fn must yield a server module");
        assert!(server_module.contains("db.users.find"), "body not in server module; got:\n{server_module}");

        assert_unified_client_parses(&out.code);
        assert!(!out.code.contains("db.users.find"), "SECURITY: body leaked into client; got:\n{}", out.code);
        assert!(
            out.code.contains("export const loadUser$$ ="),
            "no re-exported client binding for the lifted $$ fn; got:\n{}",
            out.code
        );
        assert!(
            out.code.contains("import { edenPromiseResource } from '@treaty/httpclient/resources'"),
            "no real resource-client import; got:\n{}",
            out.code
        );
        assert_imported_at_module_scope(&out.code, "edenPromiseResource");
    }

    #[test]
    fn unified_component_with_use_server_fn_extracts_binds_and_imports() {
        // MATRIX (@Component + 'use server'): a SIBLING fn carrying a `'use server'` body directive
        // declared beside a `@Component` must be extracted, body ABSENT from the client, a binding
        // re-exported, and the helper imported.
        let source = "import { Component } from '@angular/core';\n\
export async function loadUser(id: number) { 'use server'; return db.users.find(id); }\n\
@Component({ template: '<div>{{ x }}</div>' })\n\
export class AppComponent { x = 1; }\n";
        let out = compile_angular_component(source, "app.component.ts");

        let server_module = out.server_module.expect("use-server fn must yield a server module");
        assert!(server_module.contains("db.users.find"), "body not in server module; got:\n{server_module}");

        assert_unified_client_parses(&out.code);
        assert!(!out.code.contains("db.users.find"), "SECURITY: body leaked into client; got:\n{}", out.code);
        assert!(
            out.code.contains("export const loadUser ="),
            "no re-exported client binding for the lifted use-server fn; got:\n{}",
            out.code
        );
        assert_imported_at_module_scope(&out.code, "edenPromiseResource");
    }

    #[test]
    fn unified_component_with_server_block_binds_and_imports() {
        // MATRIX (.ts + server{}): a `@Component` with an inline `server { … }` block whose fn IS called
        // from the template keeps the in-place call-site rewrite (and so is NOT double-re-exported), and
        // imports the helper.
        let source = "import { Component } from '@angular/core';\n\
server {\n\
  async function save(user: User) { return db.insert(user); }\n\
}\n\
@Component({ template: '<button (click)=\"save(user)\">go</button>' })\n\
export class AppComponent {}\n";
        let out = compile_angular_component(source, "app.component.ts");
        assert!(out.server_module.is_some(), "server block must yield a server module");
        assert_unified_client_parses(&out.code);
        assert!(!out.code.contains("db.insert"), "SECURITY: body leaked; got:\n{}", out.code);
        // The fn is invoked in the template, so its call site was rewritten in place (it does not also
        // get a re-export — that would double-bind).
        assert!(
            !out.code.contains("export const save ="),
            "an in-place-rewritten fn was wrongly also re-exported; got:\n{}",
            out.code
        );
        assert_imported_at_module_scope(&out.code, "edenPromiseResource");
    }

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
        // (`edenPromiseResource` POSTing to `/__server/save`), not the original fn and not an eden path.
        assert!(
            out.code.contains("edenPromiseResource") && out.code.contains("'/__server/save'"),
            "call not rewritten to axum resource client; got: {}",
            out.code
        );
        // The resource helper is imported from the REAL `@treaty/httpclient` runtime (not a stub def).
        assert!(
            out.code.contains("import { edenPromiseResource } from '@treaty/httpclient/resources'"),
            "no real resource-client import; got: {}",
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
        // Multi-form selector (kebab/camel/Pascal) so a parent may reference the selectorless
        // component as `<log-viewer>`, `<logViewer>` OR `<LogViewer>`.
        assert!(
            out.code.contains("\"log-viewer\"")
                && out.code.contains("\"logViewer\"")
                && out.code.contains("\"LogViewer\""),
            "expected filename-derived multi-form selector (log-viewer/logViewer/LogViewer); got: {}",
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

    /// Parse `code` with oxc (TypeScript) and assert it has no parse errors — proving the emitted
    /// module is syntactically valid, NOT by regex but by actually building the AST. Returns the
    /// joined error messages on failure so the assertion message is actionable.
    fn assert_parses(code: &str) {
        let allocator = Allocator::default();
        let source_type = SourceType::default().with_typescript(true);
        let ret = Parser::new(&allocator, code, source_type).parse();
        assert!(
            ret.errors.is_empty(),
            "emitted code did not parse: {:?}\n--- code ---\n{code}",
            ret.errors.iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }

    /// True when the parsed `code` has NO top-level class carrying a decorator named `name`. Walks the
    /// AST (not a regex) so a surviving `@Directive`/`@Pipe`/`@Injectable` decorator is caught for what
    /// it is — a decorator node on a class — rather than a textual coincidence.
    fn no_surviving_decorator(code: &str, name: &str) -> bool {
        let allocator = Allocator::default();
        let source_type = SourceType::default().with_typescript(true);
        let ret = Parser::new(&allocator, code, source_type).parse();
        assert!(ret.errors.is_empty(), "code under decorator check did not parse: {code}");
        for stmt in &ret.program.body {
            let Some(class) = statement_class(stmt) else { continue };
            for dec in &class.decorators {
                if decorator_name(dec) == Some(name) {
                    return false;
                }
            }
        }
        true
    }

    const DEFINE_DIRECTIVE: &str = "\u{0275}\u{0275}defineDirective";
    const DEFINE_PIPE: &str = "\u{0275}\u{0275}definePipe";
    const DEFINE_INJECTABLE: &str = "\u{0275}\u{0275}defineInjectable";
    const DEFINE_NG_MODULE: &str = "\u{0275}\u{0275}defineNgModule";
    const FAC: &str = "\u{0275}fac";

    #[test]
    fn directive_source_lowers_to_define_directive_no_raw_decorator() {
        // The live failing file: a SELECTORLESS `@Directive` `.ts`. It must lower to a real
        // `ɵɵdefineDirective` (+ `ɵfac`) so Angular never falls to JIT at runtime, and the raw
        // `@Directive` decorator must NOT survive on the emitted class (verified by AST walk).
        let source = "import { Directive, ElementRef, computed, effect, inject, input } from '@angular/core'\n\
@Directive({ host: { '[style.color]': 'tint()' } })\n\
export class HighlightDelta {\n\
  readonly delta = input(0)\n\
  private readonly host = inject(ElementRef)\n\
  readonly tint = computed(() => this.delta() > 0 ? '#059669' : 'inherit')\n\
}\n";
        let out = compile_angular_source(source, "highlight-delta.directive.ts");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(out.code.contains(DEFINE_DIRECTIVE), "no ɵɵdefineDirective; got: {}", out.code);
        assert!(out.code.contains(FAC), "no ɵfac for the directive; got: {}", out.code);
        assert!(
            no_surviving_decorator(&out.code, "Directive"),
            "a raw @Directive decorator survived the lowering; got: {}",
            out.code
        );
        assert_parses(&out.code);
    }

    #[test]
    fn pipe_source_lowers_to_define_pipe_no_raw_decorator() {
        // The live failing file: a `@Pipe` `.ts`. It must lower to `ɵɵdefinePipe` (+ `ɵfac`), with no
        // raw `@Pipe` decorator surviving.
        let source = "import { Pipe, type PipeTransform } from '@angular/core'\n\
@Pipe({ name: 'percent01' })\n\
export class Percent01Pipe implements PipeTransform {\n\
  transform(ratio: number, fractionDigits = 0): string {\n\
    return `${(ratio * 100).toFixed(fractionDigits)}%`\n\
  }\n\
}\n";
        let out = compile_angular_source(source, "percent.pipe.ts");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(out.code.contains(DEFINE_PIPE), "no ɵɵdefinePipe; got: {}", out.code);
        assert!(out.code.contains(FAC), "no ɵfac for the pipe; got: {}", out.code);
        assert!(
            no_surviving_decorator(&out.code, "Pipe"),
            "a raw @Pipe decorator survived the lowering; got: {}",
            out.code
        );
        assert_parses(&out.code);
    }

    #[test]
    fn injectable_source_lowers_to_define_injectable_no_raw_decorator() {
        // A `@Injectable` `.ts` must lower to `ɵfac` + `ɵɵdefineInjectable` (its `ɵprov`), with no raw
        // `@Injectable` decorator surviving — otherwise Angular's JIT injector resolution crashes for
        // want of `@angular/compiler`.
        let source = "import { Injectable } from '@angular/core'\n\
@Injectable({ providedIn: 'root' })\n\
export class DataService { value = 1 }\n";
        let out = compile_angular_source(source, "data.service.ts");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(out.code.contains(DEFINE_INJECTABLE), "no ɵɵdefineInjectable; got: {}", out.code);
        assert!(out.code.contains(FAC), "no ɵfac for the injectable; got: {}", out.code);
        assert!(
            no_surviving_decorator(&out.code, "Injectable"),
            "a raw @Injectable decorator survived the lowering; got: {}",
            out.code
        );
        assert_parses(&out.code);
    }

    #[test]
    fn ng_module_source_lowers_to_define_ng_module_no_raw_decorator() {
        // A `@NgModule` `.ts` must lower to `ɵɵdefineNgModule` (+ `ɵinj`), with no raw decorator left.
        let source = "import { NgModule } from '@angular/core'\n\
@NgModule({ declarations: [] })\n\
export class AppModule {}\n";
        let out = compile_angular_source(source, "app.module.ts");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(out.code.contains(DEFINE_NG_MODULE), "no ɵɵdefineNgModule; got: {}", out.code);
        assert!(
            no_surviving_decorator(&out.code, "NgModule"),
            "a raw @NgModule decorator survived the lowering; got: {}",
            out.code
        );
        assert_parses(&out.code);
    }

    /// Collect every top-level (and exported) function/arrow-const NAME declared in `code` by walking
    /// the parsed AST (not a regex). Used to assert that a lifted server fn's DECLARATION is absent
    /// from the client module — a surviving declaration is a real AST node, not a textual coincidence.
    fn declared_fn_names(code: &str) -> Vec<String> {
        let allocator = Allocator::default();
        let source_type = SourceType::default().with_typescript(true);
        let ret = Parser::new(&allocator, code, source_type).parse();
        assert!(ret.errors.is_empty(), "client code did not parse: {code}");
        let mut names = Vec::new();
        for stmt in &ret.program.body {
            match stmt {
                Statement::FunctionDeclaration(f) => {
                    if let Some(id) = &f.id {
                        names.push(id.name.to_string());
                    }
                }
                Statement::VariableDeclaration(d) => collect_arrow_const_names(d, &mut names),
                Statement::ExportNamedDeclaration(e) => match &e.declaration {
                    Some(oxc_ast::ast::Declaration::FunctionDeclaration(f)) => {
                        if let Some(id) = &f.id {
                            names.push(id.name.to_string());
                        }
                    }
                    Some(oxc_ast::ast::Declaration::VariableDeclaration(v)) => {
                        collect_arrow_const_names(v, &mut names)
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        names
    }

    /// Push the NAME of each `const NAME = (…) => …` arrow-const declarator in `decl` into `names`.
    fn collect_arrow_const_names(decl: &oxc_ast::ast::VariableDeclaration, names: &mut Vec<String>) {
        for d in &decl.declarations {
            if let (Some(name), Some(Expression::ArrowFunctionExpression(_))) =
                (d.id.get_identifier_name(), &d.init)
            {
                names.push(name.to_string());
            }
        }
    }

    /// Parse `code` and assert it has no parse errors (the emitted client module is valid TS).
    fn assert_client_parses(code: &str) {
        let allocator = Allocator::default();
        let source_type = SourceType::default().with_typescript(true);
        let ret = Parser::new(&allocator, code, source_type).parse();
        assert!(
            ret.errors.is_empty(),
            "emitted client code did not parse: {:?}\n--- code ---\n{code}",
            ret.errors.iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }

    /// Parse `code` and assert `name` is bound at MODULE SCOPE by a real top-level `import` (an
    /// `ImportSpecifier`/`ImportDefaultSpecifier`/`ImportNamespaceSpecifier` local), reading the bound
    /// name off the PARSED AST — never a regex. A reference to a symbol that is imported is NOT a free
    /// (undefined) reference, so the module resolves it at boot. This is how PHASE 2 proves the boot
    /// `ReferenceError` is closed: the binding's runtime symbol comes from a real import, not a stub.
    fn assert_imported_at_module_scope(code: &str, name: &str) {
        use oxc_ast::ast::ImportDeclarationSpecifier;
        let allocator = Allocator::default();
        let source_type = SourceType::default().with_typescript(true);
        let ret = Parser::new(&allocator, code, source_type).parse();
        assert!(ret.errors.is_empty(), "client code did not parse: {code}");
        let imported = ret.program.body.iter().any(|stmt| {
            let Statement::ImportDeclaration(import) = stmt else { return false };
            let Some(specs) = &import.specifiers else { return false };
            specs.iter().any(|spec| {
                let local = match spec {
                    ImportDeclarationSpecifier::ImportSpecifier(s) => &s.local.name,
                    ImportDeclarationSpecifier::ImportDefaultSpecifier(s) => &s.local.name,
                    ImportDeclarationSpecifier::ImportNamespaceSpecifier(s) => &s.local.name,
                };
                local.as_str() == name
            })
        });
        assert!(
            imported,
            "`{name}` is referenced by an emitted binding but is NOT imported at module scope \
             (it would be a free/undefined reference -> `{name} is not defined` at boot); got:\n{code}"
        );
    }

    #[test]
    fn stream_consumer_client_emit_is_async_iterable_no_free_reference() {
        // PHASE 2/3 (the log-viewer boot + `for await` consumer): a `logs.stream.ts`-shaped FILE-LEVEL
        // `'use server'` module with an `async function*` is lowered so the client keeps only the typed
        // stream binding. A stream-transport fn is consumed with `for await`, so its binding is a native
        // async-iterable factory backed by `EventSource` — it needs NO runtime helper, so the module has
        // no free/undefined reference at all (the original `... is not defined` boot crash) and no
        // self-defined stub. PARSE the emitted client to verify.
        let source = "'use server'\n\
\n\
export interface LogLine { readonly seq: number }\n\
export async function* streamLogs(count: number): AsyncGenerator<LogLine> {\n\
  const levels = ['info', 'warn', 'error'];\n\
  for (let seq = 1; seq <= count; seq++) {\n\
    await Promise.resolve();\n\
    yield { seq, level: levels[seq % levels.length] };\n\
  }\n\
}\n";
        let out = compile_angular_source(source, "logs.stream.ts");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);

        // The emitted client parses and the binding is an async-iterable factory over an EventSource.
        assert_client_parses(&out.code);
        assert!(
            out.code.contains("export const streamLogs =")
                && out.code.contains("async function*")
                && out.code.contains("EventSource")
                && out.code.contains("'/__server/streamLogs'"),
            "no async-iterable stream client binding backed by EventSource; got:\n{}",
            out.code
        );
        // The stream binding references NO runtime resource helper, so none is imported and there is no
        // free reference — and no INVENTED stub of any resource symbol survives.
        assert!(
            !out.code.contains("edenPromiseResource")
                && !out.code.contains("edenStreamResource")
                && !out.code.contains("edenWebSocket")
                && !out.code.contains("httpClient"),
            "a resource-client symbol (helper or invented stub) leaked into a stream-only client; got:\n{}",
            out.code
        );
        // The stream body and its data never reach the client.
        assert!(
            !out.code.contains("await Promise.resolve()") && !out.code.contains("['info', 'warn', 'error']"),
            "SECURITY: stream body leaked into the client; got:\n{}",
            out.code
        );
    }

    #[test]
    fn file_level_use_server_module_extracts_streaming_and_req_resp_fns() {
        // The headline security fix, on a `logs.stream.ts`-shaped module: a FILE-LEVEL `'use server'`
        // module exporting an `async function*` (stream) AND a plain `async function` (req/resp). The
        // client output must contain a typed RPC binding for each fn and NONE of the bodies, while the
        // server module carries the real bodies.
        let source = "'use server'\n\
\n\
export interface LogLine { readonly seq: number }\n\
\n\
export async function* streamLogs(count: number): AsyncGenerator<LogLine> {\n\
  for (let seq = 1; seq <= count; seq++) {\n\
    await Promise.resolve();\n\
    yield { seq };\n\
  }\n\
}\n\
\n\
export async function loadUser(id: number) {\n\
  return db.users.findSecretById(id);\n\
}\n";

        let out = compile_angular_source(source, "logs.stream.ts");

        // A server module is populated and contains BOTH bodies (the whole point: bodies live on the
        // server, not the client).
        let server_module = out.server_module.expect("file-level 'use server' must yield a server module");
        assert!(
            server_module.contains("yield { seq }") || server_module.contains("yield {seq}")
                || server_module.contains("for (let seq"),
            "stream body not carried into server module; got:\n{server_module}"
        );
        assert!(
            server_module.contains("db.users.findSecretById"),
            "req/resp body not carried into server module; got:\n{server_module}"
        );
        // The streaming fn is mounted as an SSE GET route; the req/resp fn as a POST route.
        assert!(
            server_module.contains(".route(\"/__server/streamLogs\", get(__server_streamLogs))"),
            "no streaming route; got:\n{server_module}"
        );
        assert!(
            server_module.contains(".route(\"/__server/loadUser\", post(__server_loadUser))"),
            "no req/resp route; got:\n{server_module}"
        );

        // VERIFY EMITTED CLIENT CODE BY PARSING: the client must parse, and the server-fn body
        // statements must be ABSENT from it.
        assert_client_parses(&out.code);
        assert!(
            !out.code.contains("db.users.findSecretById"),
            "SECURITY: req/resp body leaked into client; got:\n{}",
            out.code
        );
        assert!(
            !out.code.contains("await Promise.resolve()"),
            "SECURITY: stream body leaked into client; got:\n{}",
            out.code
        );
        // The original generator/function declarations are gone from the client AST (a real binding
        // const may carry the same NAME, but never as a function/arrow declaration with the body).
        assert!(
            !out.code.contains("async function* streamLogs")
                && !out.code.contains("async function streamLogs"),
            "SECURITY: generator declaration leaked into client; got:\n{}",
            out.code
        );
        assert!(
            !out.code.contains("async function loadUser"),
            "SECURITY: req/resp declaration leaked into client; got:\n{}",
            out.code
        );

        // A typed client binding for each fn is present in the client module, exported under the same
        // name a consumer imports. The stream binding opens an EventSource; the req/resp binding POSTs.
        let names = declared_fn_names(&out.code);
        assert!(
            !names.contains(&"streamLogs".to_string()),
            "streamLogs survived as a fn/arrow declaration (body present); got: {names:?}"
        );
        assert!(
            out.code.contains("export const streamLogs ="),
            "no exported client binding for streamLogs; got:\n{}",
            out.code
        );
        assert!(
            out.code.contains("export const loadUser ="),
            "no exported client binding for loadUser; got:\n{}",
            out.code
        );
        assert!(
            out.code.contains("EventSource") && out.code.contains("'/__server/streamLogs'"),
            "stream binding is not an EventSource subscription; got:\n{}",
            out.code
        );
        assert!(
            out.code.contains("fetch('/__server/loadUser'") && out.code.contains("method: 'POST'"),
            "req/resp binding does not POST to the route via fetch; got:\n{}",
            out.code
        );
        // Every binding wraps the REAL resource helper, imported from the published runtime.
        assert!(
            out.code.contains("import { edenPromiseResource } from '@treaty/httpclient/resources'"),
            "no real resource-client import for the lifted server module; got:\n{}",
            out.code
        );
        // No invented client shim survives.
        assert!(
            !out.code.contains("httpClient")
                && !out.code.contains("edenStreamResource")
                && !out.code.contains("edenWebSocket"),
            "an invented resource-client shim leaked into the client; got:\n{}",
            out.code
        );
    }

    #[test]
    fn file_level_use_server_secret_is_absent_from_client_code_and_client_map() {
        // PHASE 2 (source-map redaction): a file-level `'use server'` module whose server fn embeds a
        // recognizable secret (a fake DB connection URL) must compile such that the secret token is
        // ABSENT from BOTH the client code AND the client map's `sourcesContent`. The map JSON is
        // PARSED and its `sourcesContent` scanned via the deserialized value — never a regex over the
        // raw text — so a leak is caught structurally.
        const SECRET: &str = "postgres://admin:hunter2@db.internal:5432/treaty_prod";
        let source = format!(
            "'use server'\n\
\n\
export async function loadSecret(id: number) {{\n\
  const conn = '{SECRET}';\n\
  return db.connect(conn).query(id);\n\
}}\n"
        );

        let out = compile_angular_source(&source, "secret.store.ts");

        // The server module carries the secret (it runs server-side) — that is correct.
        let server_module = out
            .server_module
            .expect("file-level 'use server' must yield a server module");
        assert!(
            server_module.contains(SECRET),
            "secret should live in the server module; got:\n{server_module}"
        );

        // SECURITY: the secret must NOT appear anywhere in the client code.
        assert_client_parses(&out.code);
        assert!(
            !out.code.contains(SECRET),
            "SECURITY: secret leaked into client code; got:\n{}",
            out.code
        );
        assert!(
            !out.code.contains("db.connect"),
            "SECURITY: server body leaked into client code; got:\n{}",
            out.code
        );

        // SECURITY: the secret must NOT appear in the client MAP's sourcesContent. Parse the map JSON
        // and scan the deserialized `sourcesContent` strings — not a regex over raw text.
        let map = out.map.expect("plain-ts server module must carry a redacted client map");
        let value: serde_json::Value =
            serde_json::from_str(&map).expect("client map should be valid JSON");
        assert_eq!(value["version"], serde_json::json!(3), "not a v3 map: {map}");
        let contents = value["sourcesContent"]
            .as_array()
            .expect("sourcesContent array present");
        assert!(!contents.is_empty(), "sourcesContent must embed the source");
        for c in contents {
            let text = c.as_str().unwrap_or("");
            assert!(
                !text.contains(SECRET),
                "SECURITY: secret leaked into client map sourcesContent: {text}"
            );
            assert!(
                !text.contains("db.connect"),
                "SECURITY: server body leaked into client map sourcesContent: {text}"
            );
        }

        // The map stays valid: same byte length as the original source (redaction is
        // position-preserving) so any client mapping still resolves.
        let embedded = contents[0].as_str().unwrap();
        assert_eq!(
            embedded.len(),
            source.len(),
            "redaction must preserve source length so the map stays valid"
        );
    }

    #[test]
    fn plain_ts_with_dollar_suffix_export_extracts_server_fn() {
        // A plain `.ts` (no Angular decorator, no file-level directive) with a single `$$`-suffixed
        // exported fn still extracts that one fn and leaves the rest of the module intact.
        let source = "export const PAGE_SIZE = 20;\n\
export async function loadPage$$(page: number) {\n\
  return db.rows.page(page, PAGE_SIZE);\n\
}\n";
        let out = compile_angular_source(source, "data.ts");
        let server_module = out.server_module.expect("$$ export must yield a server module");
        assert!(
            server_module.contains("db.rows.page"),
            "body not in server module; got:\n{server_module}"
        );
        assert_client_parses(&out.code);
        assert!(
            !out.code.contains("db.rows.page"),
            "SECURITY: body leaked into client; got:\n{}",
            out.code
        );
        // The non-server export is preserved.
        assert!(
            out.code.contains("export const PAGE_SIZE = 20;"),
            "non-server export lost; got:\n{}",
            out.code
        );
        // A binding is exported for the lifted fn.
        assert!(
            out.code.contains("export const loadPage$$ ="),
            "no client binding for loadPage$$; got:\n{}",
            out.code
        );
    }

    #[test]
    fn file_level_use_server_strips_top_level_secret_const_from_client() {
        // PHASE 3 regression: a file-level `'use server'` module is server-only IN FULL, so a TOP-LEVEL
        // secret declared BESIDE the server fns (not inside a fn body) must ALSO be stripped from the
        // client. Previously only the fn bodies were lifted, so a `const SECRET = …` survived into the
        // client bundle. The whole-module strip removes every top-level runtime statement, keeping only
        // imports + type declarations + the emitted client bindings.
        const SECRET: &str = "sk_live_TREATY_SERVER_ONLY_9f3a1c";
        let source = format!(
            "'use server'\n\
import {{ z }} from 'zod'\n\
export interface Todo {{ readonly id: number }}\n\
const DB_API_KEY = '{SECRET}'\n\
const store: Todo[] = [{{ id: 1 }}]\n\
export async function listTodos(): Promise<Todo[]> {{\n\
  if (DB_API_KEY.length === 0) throw new Error('no key');\n\
  return store.slice();\n\
}}\n"
        );

        let out = compile_angular_source(&source, "todos.server.ts");

        // The server module retains the secret + store (it runs server-side).
        let server_module = out.server_module.expect("file-level server must yield a server module");
        assert!(server_module.contains("listTodos"), "fn missing from server module");

        // SECURITY: the secret + the server-only data must be ABSENT from the client code.
        assert_client_parses(&out.code);
        assert!(
            !out.code.contains(SECRET),
            "SECURITY: top-level secret leaked into client code; got:\n{}",
            out.code
        );
        assert!(
            !out.code.contains("store.slice") && !out.code.contains("store: Todo[]"),
            "SECURITY: server-only store leaked into client code; got:\n{}",
            out.code
        );
        // The import and the exported type survive (no runtime value), and the binding is emitted.
        assert!(out.code.contains("import { z }"), "import lost; got:\n{}", out.code);
        assert!(out.code.contains("export interface Todo"), "type lost; got:\n{}", out.code);
        assert!(out.code.contains("export const listTodos ="), "binding missing; got:\n{}", out.code);

        // SECURITY: the secret must not appear in the client map's sourcesContent either.
        let map = out.map.expect("server module must carry a redacted client map");
        let value: serde_json::Value = serde_json::from_str(&map).expect("client map is valid JSON");
        for c in value["sourcesContent"].as_array().expect("sourcesContent present") {
            assert!(
                !c.as_str().unwrap_or("").contains(SECRET),
                "SECURITY: secret leaked into client map sourcesContent"
            );
        }
    }

    #[test]
    fn file_level_use_websocket_module_extracts_ws_fns_with_defined_binding() {
        // PHASE 1: a MODULE-LEVEL `'use websocket'` directive (the duplex analogue of file-level
        // `'use server'`) must lift every exported fn as a WebSocket-transport server fn through the
        // axum default backend: the body goes to the server module, the client keeps only the typed
        // binding (whose runtime symbol is defined so it resolves at boot), and the body is ABSENT
        // from the client — verified by PARSING the emitted client module, not a regex.
        let source = "'use websocket'\n\
\n\
export interface PresenceEvent { readonly userId: string }\n\
\n\
export function wsPresence(userId: string, onEvent: (e: PresenceEvent) => void) {\n\
  const broadcast = (status) => { onEvent({ userId, status, at: Date.now() }); };\n\
  broadcast('online');\n\
  return { close: () => broadcast('offline') };\n\
}\n";
        let out = compile_angular_source(source, "presence.ws.ts");

        // A server module is produced and carries the body + a ws upgrade route/handler.
        let server_module = out
            .server_module
            .expect("file-level 'use websocket' must yield a server module");
        assert!(
            server_module.contains("WebSocketUpgrade") && server_module.contains("__server_wsPresence"),
            "no ws upgrade handler for the lifted fn; got:\n{server_module}"
        );
        assert!(
            server_module.contains("onEvent({ userId") || server_module.contains("broadcast"),
            "ws body not carried into the server module; got:\n{server_module}"
        );

        // VERIFY EMITTED CLIENT CODE BY PARSING: the client must parse and the body must be absent.
        assert_client_parses(&out.code);
        assert!(
            !out.code.contains("onEvent({ userId") && !out.code.contains("broadcast("),
            "SECURITY: ws body leaked into the client; got:\n{}",
            out.code
        );
        // The fn is no longer a function/arrow declaration in the client AST (only a binding const).
        let names = declared_fn_names(&out.code);
        assert!(
            !names.contains(&"wsPresence".to_string()),
            "wsPresence survived as a fn/arrow declaration (body present); got: {names:?}"
        );
        // A typed WebSocket client binding is exported. It opens a live `WebSocket` (browser global) and
        // returns a duplex control handle (a Proxy forwarding method calls to the peer) — the real
        // duplex contract, NOT a one-shot resource. It needs no runtime helper.
        assert!(
            out.code.contains("export const wsPresence =")
                && out.code.contains("WebSocket")
                && out.code.contains("new Proxy"),
            "no duplex ws client binding for wsPresence; got:\n{}",
            out.code
        );
        // A ws-only module references no resource helper, so none is imported and no invented ws shim
        // survives — there is no dangling reference at boot.
        assert!(
            !out.code.contains("edenPromiseResource")
                && !out.code.contains("edenWebSocket")
                && !out.code.contains("wsUrl("),
            "a resource-client symbol (helper or invented ws shim) leaked into a ws-only client; got:\n{}",
            out.code
        );
        // The exported interface (a pure type) survives for consumers.
        assert!(
            out.code.contains("export interface PresenceEvent"),
            "exported type lost; got:\n{}",
            out.code
        );
    }

    #[test]
    fn plain_non_angular_ts_is_still_passed_through_unchanged() {
        // A plain `.ts` with no Angular decorator must STILL pass through verbatim after broadening the
        // router across decorator kinds — no compile, no errors, byte-for-byte identical.
        let source = "export const add = (a: number, b: number): number => a + b;\n";
        let out = compile_angular_source(source, "math.ts");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(out.server_module.is_none(), "unexpected server module");
        assert_eq!(out.code, source, "plain .ts was not passed through unchanged");
        assert!(out.map.is_none(), "plain .ts should carry no map");
    }
}
