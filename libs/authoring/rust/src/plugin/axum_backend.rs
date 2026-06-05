//! Default [`BackendPlugin`] targeting an [axum](https://docs.rs/axum) server with a typesafe HTTP
//! resource client.
//!
//! This is the *default* backend. Its premise: a developer writes server functions in TypeScript and
//! receives a working Rust/axum service without ever touching Rust; a developer who *wants* Rust
//! writes `server:rust { … }` and their body passes through verbatim.
//!
//! For server functions `f`, [`AxumBackendPlugin::emit`] produces:
//!
//! * a **server module** — a generated axum Rust service as a source `String`. For each [`ServerFn`]
//!   it emits a serde `Deserialize` request struct from the params (types via
//!   [`super::ts_to_rust::ts_type_to_rust`]), a mapped response type, an `async` handler, and a
//!   `build_router()` that registers each fn as `POST /__server/<name>` using axum (`Router`,
//!   `routing::post`, `Json`). The handler DESERIALIZES the request struct, binds each param as a
//!   local, RUNS the (transpiled or rust-verbatim) body, and returns `Json<RESP>`. The body is
//!   dispatched by `fn.lang`: `rust` bodies are emitted verbatim (passthrough, the author's body
//!   producing the `Json`); `ts` (and any non-rust) bodies are lowered via
//!   [`super::ts_to_rust::transpile_handler_body`], which wraps every `return` into the handler's
//!   `Json<RESP>` shape. A `ts` body outside the supported subset degrades to a clearly-marked,
//!   COMPILING typed-default stub (original TS preserved as a comment) — never broken Rust. Verified
//!   end-to-end: the emitted module both parses as a `syn::File` and type-checks against real axum.
//! * **client bindings** — a typesafe client binding map (`name` -> TS expression string), each
//!   shaped to the fn's TRANSPORT and built ONLY on browser globals (`fetch` / `EventSource` /
//!   `WebSocket`) — never an invented `httpClient`/`edenStreamResource`/`edenWebSocket` shim. The Api
//!   binding is an imperative RPC call: it `POST`s to `/__server/<name>` via `fetch` and resolves the
//!   JSON response as a PLAIN PROMISE, so `await fn(x)` works inside an event handler (NOT an Angular
//!   injection context — wrapping it in `resource()`/`edenPromiseResource` there throws NG0203; a
//!   caller who wants reactive loading can wrap it themselves at field-init via `resource(() => fn(x))`).
//!   The Stream binding is a native async-iterable factory backed by an `EventSource` (consumed with
//!   `for await`), and the WebSocket binding opens a live `WebSocket` and returns a duplex control
//!   handle (see `stream_client_binding` / `ws_client_binding`). This is the typesafe client binding,
//!   distinct from the Eden binding emitted by [`super::ElysiaEdenPlugin`].
//!
//! The server module is a *library* (it ends at `pub fn build_router() -> Router`). To actually RUN
//! it in production, [`emit_production_host`] emits a companion `main.rs` that declares the server
//! module as a sibling, starts a `tokio` runtime, resolves the listen port from `$PORT`, and serves
//! `build_router()` with `axum::serve` — so a packaged app boots its server fns in production rather
//! than relying on the dev-only Vite middleware.

use std::collections::HashMap;

use super::ts_to_rust::{
    ends_in_unconditional_return, handler_response_type, transpile_handler_body,
    transpile_stream_body, transpile_ws_body, ts_type_to_rust,
};
use super::{BackendEmit, BackendPlugin, ServerFn, TransportKind};

/// The URL namespace every lifted server function is mounted under, e.g. `save` -> `/__server/save`.
const SERVER_ROUTE_PREFIX: &str = "/__server";

/// Stable registry name for this backend.
const PLUGIN_NAME: &str = "axum";

/// The language tag that means "the author wrote Rust; pass the body through verbatim".
const LANG_RUST: &str = "rust";

/// The default Rust module name a generated production host expects the server module to live under
/// (a sibling `<name>.rs` next to `main.rs`). The host does `mod <name>; use <name>::build_router;`.
pub const DEFAULT_SERVER_MODULE: &str = "treaty_server";

/// The default TCP port the generated production host binds when `PORT` is unset in the environment.
pub const DEFAULT_HOST_PORT: u16 = 3000;

/// Default axum backend. Each [`ServerFn`] becomes a `Deserialize` request struct, an `async`
/// handler, and a `POST /__server/<name>` route on a generated `build_router()`; the client binding
/// routes calls through a typed signal resource over HTTP.
pub struct AxumBackendPlugin;

impl BackendPlugin for AxumBackendPlugin {
    fn name(&self) -> &str {
        PLUGIN_NAME
    }

    fn emit(&self, fns: &[ServerFn]) -> BackendEmit {
        let mut server_module = String::new();

        // Use-statements the generated service relies on. The base axum/serde set is always present;
        // the bare `serde_json::Value` import is emitted only when the generated body actually uses
        // the bare `Value` type (an untyped Api param/return) — a fully-typed module references no
        // bare `Value`, so importing it would be an unused-import warning in the generated crate.
        // Streaming/websocket fns pull in their extra axum imports only when present.
        server_module.push_str("use axum::{Json, Router, routing::post};\n");
        server_module.push_str("use serde::Deserialize;\n");
        if uses_bare_value(fns) {
            server_module.push_str("use serde_json::Value;\n");
        }
        if fns.iter().any(|f| f.transport == TransportKind::Stream) {
            server_module.push_str("use axum::response::sse::{Event, Sse};\n");
            server_module.push_str("use axum::routing::get;\n");
            server_module.push_str("use futures::stream::Stream;\n");
            server_module.push_str("use std::convert::Infallible;\n");
        }
        if fns.iter().any(|f| f.transport == TransportKind::WebSocket) {
            server_module.push_str("use axum::extract::ws::{WebSocket, WebSocketUpgrade};\n");
            server_module.push_str("use axum::response::Response;\n");
            if !fns.iter().any(|f| f.transport == TransportKind::Stream) {
                server_module.push_str("use axum::routing::get;\n");
            }
        }
        server_module.push('\n');

        // For each fn: a request struct (Api only) and a per-kind handler.
        for f in fns {
            match f.transport {
                TransportKind::Api => {
                    server_module.push_str(&emit_request_struct(f));
                    server_module.push('\n');
                    server_module.push_str(&emit_handler(f));
                    server_module.push('\n');
                }
                TransportKind::Stream => {
                    server_module.push_str(&emit_stream_handler(f));
                    server_module.push('\n');
                }
                TransportKind::WebSocket => {
                    server_module.push_str(&emit_ws_handler(f));
                    server_module.push('\n');
                }
            }
        }

        // A `build_router()` that mounts one route per fn under the `/__server` namespace. Api fns use
        // `POST`; Stream and WebSocket fns use `GET` (SSE / ws upgrade are GET in axum).
        server_module.push_str("pub fn build_router() -> Router {\n");
        server_module.push_str("    Router::new()\n");
        for f in fns {
            let (verb, handler) = match f.transport {
                TransportKind::Api => ("post", handler_name(f)),
                TransportKind::Stream | TransportKind::WebSocket => ("get", handler_name(f)),
            };
            server_module.push_str(&format!(
                "        .route(\"{prefix}/{name}\", {verb}({handler}))\n",
                prefix = SERVER_ROUTE_PREFIX,
                name = f.name,
            ));
        }
        server_module.push_str("}\n");

        // Client bindings: a free call `save(arg)` becomes a typed signal-resource call that POSTs to
        // `/__server/save`. The rewrite is identifier-aware, so it swaps the callee and leaves the
        // argument list intact; the binding is a callable factory that takes the typed args. Stream
        // and WebSocket fns get their own client binding shapes (EventSource / WebSocket).
        let mut client_bindings = HashMap::new();
        for f in fns {
            let binding = match f.transport {
                TransportKind::Api => client_binding(f),
                TransportKind::Stream => stream_client_binding(f),
                TransportKind::WebSocket => ws_client_binding(f),
            };
            client_bindings.insert(f.name.clone(), binding);
        }

        BackendEmit { server_module, client_bindings }
    }
}

/// Emit a runnable production host `main.rs` for a generated server module.
///
/// The [`BackendPlugin::emit`] output is a *library* — it ends at `pub fn build_router() -> Router`,
/// with no `main`, no async runtime, and no listener — so a packaged app can compile the server
/// module but not actually *run* it. (Dev relies on Vite's Node middleware; production had no boot
/// path.) This closes that gap: it returns a complete, self-contained `main.rs` that
///
///   * declares the generated server module as a sibling Rust module (`mod <module>;`) and pulls in
///     its `build_router()`,
///   * starts a multi-threaded `tokio` runtime via `#[tokio::main]`,
///   * resolves the listen port from the `PORT` environment variable (falling back to `default_port`,
///     so a packaged app honors the platform's injected `$PORT` — Render/Fly/Heroku/etc. — yet runs
///     locally with no configuration), binds `0.0.0.0:<port>`, and
///   * serves `build_router()` with `axum::serve`.
///
/// The result is valid, standalone Rust: write it next to the server module (which the generator
/// writes as `<module>.rs`) and `cargo run` boots the production server. `module` is the server
/// module's Rust path name (default [`DEFAULT_SERVER_MODULE`]); `default_port` is the fallback bind
/// port (default [`DEFAULT_HOST_PORT`]).
///
/// Required `Cargo.toml` dependencies for the host crate (the server module already needs `axum` /
/// `serde` / `serde_json`, plus `futures` for stream fns):
///
/// ```toml
/// axum = { version = "0.7", features = ["ws"] }
/// tokio = { version = "1", features = ["rt-multi-thread", "macros", "net"] }
/// serde = { version = "1", features = ["derive"] }
/// serde_json = "1"
/// futures = "0.3"
/// ```
pub fn emit_production_host(module: &str, default_port: u16) -> String {
    // A heredoc-style template kept deliberately small: a sibling-module declaration, a tokio entry
    // point, env-driven port resolution, and an `axum::serve` over `build_router()`. Every brace is
    // balanced and the file parses as a complete Rust program (verified in tests via `syn::parse_file`).
    format!(
        "//! Production host for the generated Treaty server module.\n\
         //!\n\
         //! Generated by the Treaty axum backend. It boots the server functions lifted out of the\n\
         //! authoring source so a packaged app can run them in production (dev uses the Vite\n\
         //! middleware instead). The port is read from `$PORT`, defaulting to {default_port}.\n\
         \n\
         mod {module};\n\
         \n\
         use {module}::build_router;\n\
         \n\
         #[tokio::main]\n\
         async fn main() {{\n\
         \x20   let port: u16 = std::env::var(\"PORT\")\n\
         \x20       .ok()\n\
         \x20       .and_then(|p| p.parse().ok())\n\
         \x20       .unwrap_or({default_port});\n\
         \x20   let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));\n\
         \x20   let listener = tokio::net::TcpListener::bind(addr)\n\
         \x20       .await\n\
         \x20       .expect(\"failed to bind the configured port\");\n\
         \x20   println!(\"treaty server listening on http://{{addr}}\");\n\
         \x20   axum::serve(listener, build_router())\n\
         \x20       .await\n\
         \x20       .expect(\"server error\");\n\
         }}\n",
    )
}

/// The Rust handler function name for a fn, e.g. `save` -> `__server_save`.
fn handler_name(f: &ServerFn) -> String {
    format!("__server_{}", f.name)
}

/// The Rust request-struct name for a fn, e.g. `save` -> `SaveRequest`.
fn request_struct_name(f: &ServerFn) -> String {
    format!("{}Request", to_pascal_case(&f.name))
}

/// Convert a JS/TS identifier (snake_case, camelCase, or `$$`-suffixed) to PascalCase for a Rust type
/// name. Non-alphanumeric separators are dropped and the following char is uppercased.
fn to_pascal_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper_next = true;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            if upper_next {
                out.extend(ch.to_uppercase());
            } else {
                out.push(ch);
            }
            upper_next = false;
        } else {
            // Any separator (`_`, `$`, …) starts a new word.
            upper_next = true;
        }
    }
    if out.is_empty() {
        out.push_str("Server");
    }
    out
}

/// Does the generated module reference the bare `Value` type, requiring `use serde_json::Value;`?
///
/// The bare `Value` shorthand is emitted ONLY as the fallback for an untyped Api param (a request
/// struct field, see [`emit_request_struct`]) or an untyped `server:rust` Api return ([`response_type`]).
/// Every other "no useful Rust type" mapping (`object`/`unknown`/named interfaces, the ws/stream JSON
/// payloads) uses the fully-qualified `serde_json::Value`, which needs no import. So the import is
/// needed exactly when some Api fn has an untyped param, or some `rust`-lang Api fn has an untyped
/// return — otherwise it would be an unused import in the generated crate.
fn uses_bare_value(fns: &[ServerFn]) -> bool {
    fns.iter().filter(|f| f.transport == TransportKind::Api).any(|f| {
        f.params.iter().any(|p| p.ty.is_none())
            || (f.lang == LANG_RUST && f.return_type.is_none())
    })
}

/// Emit the serde `Deserialize` request struct for a fn's params. Each param becomes a typed field
/// (TS type via [`ts_type_to_rust`], untyped params fall back to the bare `Value`). A no-param fn
/// still gets a struct (an empty payload) so handlers have a uniform `Json<…Request>` signature.
fn emit_request_struct(f: &ServerFn) -> String {
    let mut out = String::new();
    out.push_str("#[derive(Debug, Deserialize)]\n");
    out.push_str(&format!("pub struct {} {{\n", request_struct_name(f)));
    for p in &f.params {
        let ty = p
            .ty
            .as_deref()
            .map(ts_type_to_rust)
            .unwrap_or_else(|| "Value".to_string());
        out.push_str(&format!("    pub {}: {},\n", p.name, ty));
    }
    out.push_str("}\n");
    out
}

/// Map a fn's return type to a Rust response type (via [`ts_type_to_rust`]); unannotated fns return
/// `serde_json::Value`.
fn response_type(f: &ServerFn) -> String {
    f.return_type
        .as_deref()
        .map(ts_type_to_rust)
        .unwrap_or_else(|| "Value".to_string())
}

/// Emit the async axum handler for a fn. The handler deserializes the typed request as
/// `Json<…Request>`, binds each param as a local from the deserialized payload, runs the (dispatched)
/// body, and returns `Json<RESP>` — the body itself produces that `Json(…)`.
///
/// Body dispatch by `fn.lang`:
///   * `rust` -> the author's body is emitted verbatim (passthrough); a trailing `Json(Default::default())`
///     fall-through keeps a body that does not end in an explicit `Json(…)` compiling.
///   * anything else (`ts`, …) -> lowered via [`transpile_handler_body`], which wraps every `return`
///     into the handler's `Json<RESP>` shape. When the TS body falls outside the supported subset the
///     handler becomes a clearly-marked, COMPILING stub (original TS as a comment + a typed `Json`
///     default) — never broken Rust.
///
/// The `-> Json<RESP>` response type is the fn's mapped return type ([`handler_response_type`] for the
/// transpiled path, [`response_type`] for the verbatim path); both produce the same mapping.
fn emit_handler(f: &ServerFn) -> String {
    let mut out = String::new();
    let resp = if f.lang == LANG_RUST {
        response_type(f)
    } else {
        // Use the transpiler's mapping so the `-> Json<RESP>` signature matches the wrapped body.
        handler_response_type(&f.source)
    };
    out.push_str(&format!(
        "pub async fn {handler}(Json(req): Json<{req}>) -> Json<{resp}> {{\n",
        handler = handler_name(f),
        req = request_struct_name(f),
    ));

    // Bind each param as a local from the deserialized request so both verbatim Rust bodies and
    // transpiled TS bodies can reference params by their original names.
    for p in &f.params {
        out.push_str(&format!("    let {name} = req.{name};\n", name = p.name));
    }

    let body = if f.lang == LANG_RUST {
        // Passthrough: emit the author's verbatim function body (it is responsible for producing the
        // `Json<RESP>`), with a trailing typed default so a body that omits a final `Json(…)` still
        // returns the promised type.
        rust_body_block(f)
    } else {
        // Lower the TS body into the handler's `Json<RESP>` shape; the result always compiles (a real
        // run for the covered shapes, a clearly-marked typed-default stub otherwise).
        let transpiled = transpile_handler_body(&f.source);
        transpiled.rust_body
    };
    out.push_str(&body);
    if !body.ends_with('\n') {
        out.push('\n');
    }

    out.push_str("}\n");
    out
}

/// For a `server:rust` fn, slice the author's body out of `ServerFn::source` and emit it verbatim,
/// indented one level, followed by a trailing `Json(Default::default())` so a body whose final
/// statement is not an explicit `Json(…)` still returns the handler's `Json<RESP>`. When no braces can
/// be located the whole source is preserved as a comment so the handler still compiles and nothing is
/// silently dropped.
fn rust_body_block(f: &ServerFn) -> String {
    match (f.source.find('{'), f.source.rfind('}')) {
        (Some(open), Some(close)) if close > open => {
            let inner = f.source[open + 1..close].trim_matches(['\n', '\r']);
            let mut out = indent_block(inner);
            if !out.ends_with('\n') {
                out.push('\n');
            }
            // Fall-through typed default so a verbatim body without a final `Json(…)` still compiles.
            out.push_str("    Json(Default::default())\n");
            out
        }
        _ => {
            // No discernible body; keep the source as a comment plus a typed default.
            let mut out = String::new();
            for line in f.source.lines() {
                out.push_str(&format!("    // {line}\n"));
            }
            out.push_str("    Json(Default::default())\n");
            out
        }
    }
}

/// Indent a (possibly multi-line) block of Rust source one level (four spaces), preserving relative
/// indentation. Blank lines are left empty rather than padded.
fn indent_block(body: &str) -> String {
    let mut out = String::new();
    for line in body.lines() {
        if line.trim().is_empty() {
            out.push('\n');
        } else {
            out.push_str("    ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// The client-side binding for a fn: a typed signal-resource call that POSTs to `/__server/<name>`.
///
/// Built ONLY on symbols the real `@treaty/httpclient` resources layer exports (`edenPromiseResource`)
/// plus the `fetch` browser global — no invented `httpClient` shim. It returns a factory taking the
/// fn's typed args and yielding an `edenPromiseResource` typed to the fn's response: each invocation
/// POSTs the named-param payload to the route via `fetch` and resolves the JSON response. A
/// single-param fn forwards the bare arg, a no-param fn POSTs an empty body. The
/// [`client_runtime_imports_for_code`](super::client_runtime_imports_for_code) emitter prepends the
/// real `import { edenPromiseResource } from '@treaty/httpclient/resources'` so the binding resolves
/// at boot.
fn client_binding(f: &ServerFn) -> String {
    let route = format!("{SERVER_ROUTE_PREFIX}/{}", f.name);

    // The arrow parameter list and the body object posted to the route.
    let (arg_list, body_expr) = match f.params.len() {
        0 => (String::new(), "{}".to_string()),
        1 => {
            let p = &f.params[0];
            let ty = p.ty.as_deref().map(|t| format!(": {t}")).unwrap_or_default();
            (format!("{}{}", p.name, ty), format!("{{ {name}: {name} }}", name = p.name))
        }
        _ => {
            let params = f
                .params
                .iter()
                .map(|p| {
                    let ty = p.ty.as_deref().map(|t| format!(": {t}")).unwrap_or_default();
                    format!("{}{}", p.name, ty)
                })
                .collect::<Vec<_>>()
                .join(", ");
            let body = f
                .params
                .iter()
                .map(|p| p.name.clone())
                .collect::<Vec<_>>()
                .join(", ");
            (params, format!("{{ {body} }}"))
        }
    };

    // A typed imperative RPC call: each invocation POSTs the typed args to the route via the `fetch`
    // browser global and resolves the JSON response as a PLAIN PROMISE. A server fn is most often
    // called imperatively (`await save(x)` inside an event handler), which is NOT an Angular injection
    // context — so the binding must NOT wrap the call in `resource()`/`edenPromiseResource` (that
    // throws NG0203 outside a constructor/field initializer). A caller who wants reactive,
    // signal-based loading can still wrap it themselves at field-init: `resource(() => save(x))`.
    format!(
        "(({arg_list}) => \
         fetch('{route}', {{ method: 'POST', headers: {{ 'content-type': 'application/json' }}, \
         body: JSON.stringify({body_expr}) }}).then((res) => res.json()))",
    )
}

/// Emit the axum SSE handler for a [`TransportKind::Stream`] fn. The handler returns an
/// `Sse<impl Stream<Item = Result<Event, Infallible>>>`, the streaming response axum mounts on a GET
/// route. The author's async-generator body becomes the stream source: it is transpiled with
/// [`transpile_stream_body`], which lowers every `yield X` into a push of `serde_json::json!(X)` onto
/// an `__items` accumulator. The generated wrapper then turns that accumulator into the SSE stream,
/// mapping each value to `Event::default().json_data(value)`.
///
/// The streaming transport is defined by JavaScript constructs (`function*` / `yield`), so the body
/// is ALWAYS lowered through the yield-aware [`transpile_stream_body`] — even for a `server:rust` block
/// (whose bare-`server { … }` default is `rust`), because a generator body is JS, not Rust. The
/// transpiler's graceful degradation guarantees the result still compiles.
///
/// GRACEFUL DEGRADATION: when the body cannot be fully transpiled (a `while`/`switch`/value-position
/// `yield`, etc.), the generated handler still COMPILES — it wires the real transport and yields a
/// typed PLACEHOLDER stream (`futures::stream::empty()`) while preserving the author's body verbatim
/// as a `//` comment, so nothing is silently dropped and the route is never broken Rust.
fn emit_stream_handler(f: &ServerFn) -> String {
    let mut out = String::new();

    // A stream fn's args travel in the EventSource URL query string as a JSON `args` array (see
    // `stream_client_binding`). A param-less fn keeps the historical no-arg signature; a fn WITH params
    // takes `Query<HashMap<String, String>>`, parses the `args` entry as a JSON array, and binds each
    // param by index with a typed default so the bind always compiles for any param type.
    if f.params.is_empty() {
        out.push_str(&format!(
            "pub async fn {handler}() -> Sse<impl Stream<Item = Result<Event, Infallible>>> {{\n",
            handler = handler_name(f),
        ));
    } else {
        out.push_str(&format!(
            "pub async fn {handler}(axum::extract::Query(__query): axum::extract::Query<std::collections::HashMap<String, String>>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {{\n",
            handler = handler_name(f),
        ));
    }

    // Lower the generator body through the yield-aware stream transpiler.
    let transpiled = transpile_stream_body(&f.source);
    let body = indent_block(&transpiled.rust_body);
    let covered = transpiled.covered;

    if covered {
        // Bind each param from the query `args` JSON array before the body references it.
        out.push_str(&stream_param_bindings(f));
        // Drive the generator's yields: accumulate each yielded value into `__items`, then turn the
        // accumulator into the SSE stream, mapping each value to a JSON SSE frame. `json_data` only
        // fails if serialization fails (it cannot here — the items are already `serde_json::Value`),
        // so the `unwrap_or_else` fallback keeps the item type `Result<Event, Infallible>`.
        out.push_str("    let mut __items: Vec<serde_json::Value> = Vec::new();\n");
        out.push_str(&body);
        if !body.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("    let stream = futures::stream::iter(__items.into_iter().map(|__value| {\n");
        out.push_str("        Ok::<Event, Infallible>(\n");
        out.push_str("            Event::default()\n");
        out.push_str("                .json_data(&__value)\n");
        out.push_str("                .unwrap_or_else(|_| Event::default().data(\"null\")),\n");
        out.push_str("        )\n");
        out.push_str("    }));\n");
        out.push_str("    Sse::new(stream)\n");
    } else {
        // Could not fully lower the body: wire the transport with a typed placeholder stream and keep
        // the author's body verbatim as a comment so its intent survives and the handler still
        // compiles.
        for line in f.source.lines() {
            out.push_str(&format!("    // {line}\n"));
        }
        out.push_str("    let stream = futures::stream::empty::<Result<Event, Infallible>>();\n");
        out.push_str("    Sse::new(stream)\n");
    }
    out.push_str("}\n");
    out
}

/// Bind each parameter of a stream fn from the EventSource URL's `args` query entry (a JSON array),
/// deserializing each by index into its mapped Rust type with a typed default fallback so the bind
/// always compiles for any param type. Emits nothing for a param-less fn (its handler takes no query).
fn stream_param_bindings(f: &ServerFn) -> String {
    if f.params.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    out.push_str(
        "    let __args: serde_json::Value = __query.get(\"args\")\n        .and_then(|__s| serde_json::from_str(__s).ok()).unwrap_or(serde_json::Value::Null);\n",
    );
    for (i, p) in f.params.iter().enumerate() {
        let ty = p
            .ty
            .as_deref()
            .map(ts_type_to_rust)
            .unwrap_or_else(|| "serde_json::Value".to_string());
        out.push_str(&format!(
            "    let {name}: {ty} = __args.get({i}).cloned()\n        .and_then(|__v| serde_json::from_value(__v).ok()).unwrap_or_default();\n",
            name = p.name,
        ));
    }
    out
}

/// Emit the axum WebSocket handler for a [`TransportKind::WebSocket`] fn. The route handler upgrades
/// the connection (`WebSocketUpgrade`) and hands the socket to a generated per-fn task that runs the
/// real duplex body: a `recv` loop that, for each incoming Text frame, binds the fn's DATA params
/// from the frame's `args` array, runs the (transpiled) body to compute a response, and `send`s that
/// response back as a JSON Text frame — the duplex echo-of-logic contract a WebSocket server fn
/// presents to its peer. A `Close` frame ends the loop.
///
/// The body is ALWAYS lowered through [`transpile_ws_body`] — even for a `server:rust` block (whose
/// bare-`server { … }` default is `rust`), because a duplex server fn is authored in JS (the
/// `'use websocket'` directive, the event-callback param). The transpiler wraps every return in
/// `serde_json::json!`, so the per-message result is a uniform `serde_json::Value` regardless of the
/// source return type and serializes the same way over the socket. A function-typed param (its type
/// text contains `=>`, e.g. an event callback like `onEvent: (e) => void`) is the client-side message
/// sink and carries no server value, so it is EXCLUDED from the server-side data params.
///
/// GRACEFUL DEGRADATION: when the body cannot be fully transpiled, the generated handler still
/// COMPILES — it wires the real transport (the upgrade + the `recv` loop) and uses a typed PLACEHOLDER
/// per-message handler (an echo of the inbound frame) while preserving the author's body verbatim as a
/// `//` comment, so the route is never broken Rust.
fn emit_ws_handler(f: &ServerFn) -> String {
    let mut out = String::new();
    let socket_fn = format!("{}_socket", handler_name(f));
    let handle_fn = format!("{}_handle", handler_name(f));

    // The DATA params (everything but the function-typed callback param the client wires to its own
    // message sink).
    let data_params: Vec<&super::ServerParam> = f
        .params
        .iter()
        .filter(|p| !p.ty.as_deref().is_some_and(|t| t.contains("=>")))
        .collect();

    // Lower the duplex body through the JSON-value transpiler so the per-message handler returns a
    // uniform `serde_json::Value`.
    let transpiled = transpile_ws_body(&f.source);
    let body = indent_block(&transpiled.rust_body);
    let covered = transpiled.covered;

    // The upgrade handler.
    out.push_str(&format!(
        "pub async fn {handler}(ws: WebSocketUpgrade) -> Response {{\n",
        handler = handler_name(f),
    ));
    out.push_str(&format!("    ws.on_upgrade({socket_fn})\n"));
    out.push_str("}\n");

    // The per-connection socket task: a recv loop that binds data params from each frame, runs the
    // body, and sends the response back.
    out.push_str(&format!("pub async fn {socket_fn}(mut socket: WebSocket) {{\n"));
    out.push_str("    use axum::extract::ws::Message;\n");
    out.push_str("    while let Some(Ok(__msg)) = socket.recv().await {\n");
    out.push_str("        match __msg {\n");
    out.push_str("            Message::Text(__text) => {\n");
    if covered {
        // Decode the frame and bind each data param from its `args` array by index, deserializing
        // into the mapped Rust type with a typed default fallback so the bind always compiles for any
        // param type. (The placeholder echo path needs none of this, so it is emitted only here.)
        out.push_str(
            "                let __frame: serde_json::Value =\n                    serde_json::from_str(&__text).unwrap_or(serde_json::Value::Null);\n",
        );
        out.push_str(
            "                let __args = __frame.get(\"args\").cloned().unwrap_or(serde_json::Value::Null);\n",
        );
        let mut call_args = Vec::with_capacity(data_params.len());
        for (i, p) in data_params.iter().enumerate() {
            let ty = p
                .ty
                .as_deref()
                .map(ts_type_to_rust)
                .unwrap_or_else(|| "serde_json::Value".to_string());
            out.push_str(&format!(
                "                let {name}: {ty} = __args.get({i}).cloned()\n                    .and_then(|__v| serde_json::from_value(__v).ok()).unwrap_or_default();\n",
                name = p.name,
            ));
            call_args.push(p.name.clone());
        }
        // Run the body via the generated per-message handler and send its JSON result back.
        out.push_str(&format!(
            "                let __response = {handle_fn}({args}).await;\n",
            args = call_args.join(", "),
        ));
        out.push_str(
            "                let __payload = serde_json::to_string(&__response).unwrap_or_else(|_| String::from(\"null\"));\n",
        );
        out.push_str("                if socket.send(Message::Text(__payload.into())).await.is_err() {\n");
        out.push_str("                    break;\n");
        out.push_str("                }\n");
    } else {
        // Placeholder per-message handler: echo the inbound text back so the transport is live and the
        // route compiles. The author's body is preserved as a comment below.
        out.push_str("                if socket.send(Message::Text(__text)).await.is_err() {\n");
        out.push_str("                    break;\n");
        out.push_str("                }\n");
    }
    out.push_str("            }\n");
    out.push_str("            Message::Close(_) => break,\n");
    out.push_str("            _ => {}\n");
    out.push_str("        }\n");
    out.push_str("    }\n");
    out.push_str("}\n");

    // The per-message body handler (covered bodies only). Data params in, a uniform
    // `serde_json::Value` out.
    if covered {
        let sig_params = data_params
            .iter()
            .map(|p| {
                let ty = p
                    .ty
                    .as_deref()
                    .map(ts_type_to_rust)
                    .unwrap_or_else(|| "serde_json::Value".to_string());
                format!("{}: {}", p.name, ty)
            })
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!(
            "pub async fn {handle_fn}({sig_params}) -> serde_json::Value {{\n"
        ));
        out.push_str(&body);
        if !body.ends_with('\n') {
            out.push('\n');
        }
        // A body that can fall through without an explicit return still type-checks to the declared
        // `serde_json::Value` return. Skip the tail when the body always returns, so the generated
        // handler has no unreachable-code warning. The check runs on the canonical one-level-indented
        // `transpiled.rust_body` (the form `ends_in_unconditional_return` expects), not the deeper
        // `body` re-indented for emission.
        if !ends_in_unconditional_return(&transpiled.rust_body) {
            out.push_str("    serde_json::Value::Null\n");
        }
        out.push_str("}\n");
    } else {
        // The author's body, preserved verbatim as a comment, so a placeholder-echo connection does
        // not silently drop the authored intent.
        for line in f.source.lines() {
            out.push_str(&format!("// {line}\n"));
        }
    }
    out
}

/// The client-side binding for a [`TransportKind::Stream`] fn: a factory that opens an `EventSource`
/// (a browser global) to the fn's `/__server/<name>` GET route and returns a genuine multi-value
/// **async iterable** over the streamed messages — the exact contract a stream-transport server fn
/// presents to its caller (`for await (const x of streamFn(...))`).
///
/// The server fn is authored as an async generator; its over-the-wire shape is therefore an async
/// iterable, not a one-shot resource. The binding is a native async generator: each SSE `message`
/// event is buffered and yielded in turn; the iterator settles (returns) when the server closes the
/// stream by sending the sentinel `event: end`, and rejects on transport error. This needs only the
/// `EventSource` browser global and built-in async iteration — no runtime helper and no stub — so it
/// boots clean AND satisfies the consumer's `for await` use without resolving merely the first value.
///
/// ENVIRONMENT GUARD: `EventSource` is a browser-only global (absent under SSR / Node / jsdom). When it
/// is not present the binding yields an empty async iterable (the `for await` completes immediately)
/// rather than throwing `EventSource is not defined` — so a component that opens a stream in its
/// constructor still boots in a non-browser host; the live stream attaches only where `EventSource`
/// exists.
///
/// ARGS: a stream fn's args travel in the URL query string as a JSON `args` array
/// (`?args=<encoded JSON>`); the generated SSE handler decodes them back into its typed params (see
/// `stream_param_bindings`). A param-less fn opens the bare route with no query.
fn stream_client_binding(f: &ServerFn) -> String {
    let route = format!("{SERVER_ROUTE_PREFIX}/{}", f.name);
    let arg_list = binding_arg_list(f);
    // The args JSON array, by param name; param-less fns open the bare route.
    let arg_names = f.params.iter().map(|p| p.name.clone()).collect::<Vec<_>>().join(", ");
    let url_expr = if f.params.is_empty() {
        format!("'{route}'")
    } else {
        format!("'{route}?args=' + encodeURIComponent(JSON.stringify([{arg_names}]))")
    };
    // A self-contained async generator: an EventSource feeds a queue of pending values and a queue of
    // waiting consumers; `next()` resolves from whichever is ready. `event: end` ends iteration and
    // closes the socket; an error rejects the in-flight pull and closes the socket.
    format!(
        "(async function* ({arg_list}) {{ \
         if (typeof EventSource === 'undefined') return; \
         const source = new EventSource({url_expr}); \
         const values = []; const waiters = []; let done = false; let failure = null; \
         const settle = () => {{ while (waiters.length) {{ const w = waiters.shift(); \
         if (failure) w.reject(failure); else if (values.length) w.resolve({{ value: values.shift(), done: false }}); \
         else if (done) w.resolve({{ value: undefined, done: true }}); else {{ waiters.unshift(w); break; }} }} }}; \
         source.addEventListener('end', () => {{ done = true; source.close(); settle(); }}); \
         source.onmessage = (event) => {{ values.push(JSON.parse(event.data)); settle(); }}; \
         source.onerror = (event) => {{ failure = event; source.close(); settle(); }}; \
         try {{ \
         while (true) {{ \
         if (values.length) {{ yield values.shift(); continue; }} \
         if (failure) throw failure; \
         if (done) return; \
         const next = await new Promise((resolve, reject) => waiters.push({{ resolve, reject }})); \
         if (next.done) return; yield next.value; \
         }} \
         }} finally {{ source.close(); }} \
         }})"
    )
}

/// The client-side binding for a [`TransportKind::WebSocket`] fn: a factory that opens a live
/// `WebSocket` (a browser global) to the fn's `/__server/<name>` route and returns a duplex socket
/// HANDLE — the real contract of a duplex server fn, not a one-shot resource.
///
/// A WebSocket server fn takes its non-callback args plus an event CALLBACK (a function-typed param,
/// e.g. `onEvent: (e) => void`) and returns a control handle whose methods push messages to the peer.
/// The binding mirrors that: the callback param (detected as the function-typed param — its type text
/// contains `=>`) is wired to `socket.onmessage` (each parsed message is delivered to it); the returned
/// handle is a `Proxy` that forwards EVERY method call (`announce(...)`, `close()`, …) to the server as
/// a `{ method, args }` frame over the socket, with `close()` also closing the connection. The leading
/// args are sent as an `init` frame once the socket opens. This needs only the `WebSocket` browser
/// global — no runtime helper and no stub — so it boots clean and satisfies the consumer's
/// `socket.announce(...)` / `socket.close()` usage instead of yielding a one-shot value.
///
/// ENVIRONMENT GUARD: `WebSocket` is a browser-only global (absent under SSR / Node / jsdom). When it
/// is not present the binding returns an inert handle (a `Proxy` whose every method is a no-op) instead
/// of throwing `WebSocket is not defined` — so a component that opens a channel in its constructor
/// still boots in a non-browser host; the live duplex channel attaches only where `WebSocket` exists.
fn ws_client_binding(f: &ServerFn) -> String {
    let route = format!("{SERVER_ROUTE_PREFIX}/{}", f.name);
    let arg_list = binding_arg_list(f);
    // The callback param (function-typed) and the leading data args, by name.
    let callback = f
        .params
        .iter()
        .find(|p| p.ty.as_deref().is_some_and(|t| t.contains("=>")))
        .map(|p| p.name.clone());
    let init_args = f
        .params
        .iter()
        .filter(|p| Some(&p.name) != callback.as_ref())
        .map(|p| p.name.clone())
        .collect::<Vec<_>>()
        .join(", ");
    // Deliver each parsed message to the callback if one was provided; otherwise just buffer-drop.
    let on_message = match &callback {
        Some(cb) => format!(
            "socket.onmessage = (event) => {{ try {{ {cb}(JSON.parse(event.data)); }} catch (_e) {{ /* non-JSON frame */ }} }};",
            cb = cb
        ),
        None => String::new(),
    };
    format!(
        "(({arg_list}) => {{ \
         if (typeof WebSocket === 'undefined') return new Proxy({{}}, {{ get: () => () => {{}} }}); \
         const scheme = (typeof location !== 'undefined' && location.protocol === 'https:') ? 'wss://' : 'ws://'; \
         const host = (typeof location !== 'undefined') ? location.host : ''; \
         const socket = new WebSocket(scheme + host + '{route}'); \
         const queue = []; let open = false; \
         const flush = () => {{ while (open && queue.length) socket.send(queue.shift()); }}; \
         const sendFrame = (frame) => {{ queue.push(JSON.stringify(frame)); flush(); }}; \
         socket.onopen = () => {{ open = true; sendFrame({{ kind: 'init', args: [{init_args}] }}); }}; \
         {on_message} \
         return new Proxy({{}}, {{ get: (_t, method) => (...args) => {{ \
         sendFrame({{ kind: 'call', method: String(method), args }}); \
         if (method === 'close') {{ try {{ socket.close(); }} catch (_e) {{}} }} \
         }} }}); \
         }})"
    )
}

/// The typed arrow parameter list for a binding factory, shared by the stream/ws bindings.
fn binding_arg_list(f: &ServerFn) -> String {
    f.params
        .iter()
        .map(|p| {
            let ty = p.ty.as_deref().map(|t| format!(": {t}")).unwrap_or_default();
            format!("{}{}", p.name, ty)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{extract_server_block, PluginRegistry};

    #[test]
    fn registry_default_is_axum() {
        let registry = PluginRegistry::with_defaults();
        assert_eq!(registry.default_plugin().map(|p| p.name()), Some("axum"));
        // Elysia stays registered but is now opt-in by name.
        assert!(registry.get("axum").is_some());
        assert!(registry.get("elysia-eden").is_some());
    }

    #[test]
    fn rust_fn_body_passes_through_verbatim_with_route() {
        // A `server:rust` block: the signature is authored in the TS-shaped declaration form (so the
        // extractor lifts name/params), while the body is Rust to be passed through verbatim.
        let source = "server:rust {\n\
          function save(user: User) {\n\
            db.insert(user)\n\
          }\n\
        }\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns.len(), 1);
        assert_eq!(extraction.server_fns[0].lang, "rust");

        let emit = AxumBackendPlugin.emit(&extraction.server_fns);

        // The verbatim author body is present.
        assert!(
            emit.server_module.contains("db.insert(user)"),
            "rust body not passed through verbatim; got:\n{}",
            emit.server_module
        );
        // The POST route is registered under the `/__server` namespace.
        assert!(
            emit.server_module.contains("post(__server_save)"),
            "no handler registration; got:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module.contains("\"/__server/save\""),
            "no /__server/save route; got:\n{}",
            emit.server_module
        );
        // The router builder is emitted.
        assert!(emit.server_module.contains("pub fn build_router() -> Router"));
        assert_no_marker_words(&emit.server_module);
    }

    #[test]
    fn ts_fn_emits_request_struct_and_transpiled_handler() {
        // A default (`ts`) server fn must yield a Deserialize request struct and a transpiled handler.
        let source = "server:ts {\n\
          function add(a: number, b: number): number { return a + b; }\n\
        }\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns.len(), 1);
        assert_eq!(extraction.server_fns[0].lang, "ts");

        let emit = AxumBackendPlugin.emit(&extraction.server_fns);

        // A serde Deserialize request struct with typed fields.
        assert!(
            emit.server_module.contains("#[derive(Debug, Deserialize)]"),
            "no derive; got:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module.contains("pub struct AddRequest {"),
            "no request struct; got:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module.contains("pub a: f64,") && emit.server_module.contains("pub b: f64,"),
            "params not typed via ts_to_rust; got:\n{}",
            emit.server_module
        );
        // The handler signature and a transpiled body whose return is wrapped into the handler's
        // `Json<RESP>` shape (it deserializes the request, runs the body, and returns `Json(…)`).
        assert!(
            emit.server_module.contains("pub async fn __server_add(Json(req): Json<AddRequest>) -> Json<f64>"),
            "no typed handler; got:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module.contains("let a = req.a;") && emit.server_module.contains("let b = req.b;"),
            "handler does not bind params from the deserialized request; got:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module.contains("return Json(a + b);"),
            "ts body not transpiled + wrapped as Json into handler; got:\n{}",
            emit.server_module
        );
        // The use-statements the service relies on.
        assert!(emit.server_module.contains("use axum::{Json, Router, routing::post};"));
        assert!(emit.server_module.contains("use serde::Deserialize;"));
        assert_no_marker_words(&emit.server_module);
    }

    #[test]
    fn client_bindings_post_to_server_route_as_resource() {
        let source = "server:ts {\n\
          function add(a: number, b: number): number { return a + b; }\n\
        }\n";
        let extraction = extract_server_block(source);
        let emit = AxumBackendPlugin.emit(&extraction.server_fns);

        let binding = emit.client_bindings.get("add").expect("binding for add");
        // An imperative fetch POST — NOT wrapped in `resource()`/`edenPromiseResource` (calling a
        // server fn imperatively inside an event handler outside an injection context threw NG0203).
        assert!(
            binding.contains("fetch('/__server/") && !binding.contains("edenPromiseResource"),
            "binding is not a plain imperative fetch POST; got: {binding}"
        );
        assert!(
            binding.contains("'/__server/add'"),
            "binding does not POST to the server route; got: {binding}"
        );
        // POSTs via the `fetch` browser global — no invented `httpClient` shim.
        assert!(
            binding.contains("fetch('/__server/add'") && binding.contains("method: 'POST'"),
            "binding does not POST via fetch; got: {binding}"
        );
        assert!(
            !binding.contains("httpClient"),
            "binding references an invented httpClient shim; got: {binding}"
        );
    }

    #[test]
    fn stream_fn_emits_streaming_route_and_binding() {
        let source = "server {\n\
          async function* ticks() { yield 1; yield 2; }\n\
        }\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns.len(), 1);
        assert_eq!(extraction.server_fns[0].transport, super::TransportKind::Stream);

        let emit = AxumBackendPlugin.emit(&extraction.server_fns);

        // A streaming (SSE) handler and the SSE imports.
        assert!(
            emit.server_module.contains("use axum::response::sse::{Event, Sse};"),
            "no SSE import; got:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module.contains("-> Sse<impl Stream<Item = Result<Event, Infallible>>>"),
            "no streaming handler signature; got:\n{}",
            emit.server_module
        );
        // The async-generator body must actually DRIVE the stream: each `yield` becomes a push onto
        // the `__items` accumulator, and the accumulator is mapped into JSON SSE frames. This must
        // NOT be the empty-stream placeholder for a body the transpiler fully covers.
        assert!(
            emit.server_module.contains("let mut __items: Vec<serde_json::Value> = Vec::new();"),
            "stream handler does not declare the yield accumulator; got:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module.contains("__items.push(serde_json::json!(1.0));")
                && emit.server_module.contains("__items.push(serde_json::json!(2.0));"),
            "yielded values not lowered into accumulator pushes; got:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module.contains(".json_data(&__value)"),
            "accumulated values not mapped into SSE events; got:\n{}",
            emit.server_module
        );
        assert!(
            !emit.server_module.contains("futures::stream::empty"),
            "a fully-covered generator body must not fall back to the empty-stream placeholder; got:\n{}",
            emit.server_module
        );
        // The generated stream handler is well-formed Rust (balanced delimiters).
        assert_balanced_delimiters(&emit.server_module);
        // The route is registered as a GET (SSE) under /__server.
        assert!(
            emit.server_module.contains(".route(\"/__server/ticks\", get(__server_ticks))"),
            "no streaming route registration; got:\n{}",
            emit.server_module
        );
        // The client binding is a native async-iterable factory backed by `EventSource` — the real
        // contract of a stream-transport (async-generator) server fn, consumed with `for await`. It
        // needs no runtime helper, so it must NOT wrap a resource (no `edenPromiseResource`) and must
        // NOT invent an `edenStreamResource` shim.
        let binding = emit.client_bindings.get("ticks").expect("binding for ticks");
        assert!(
            binding.contains("EventSource") && binding.contains("'/__server/ticks'"),
            "binding is not a stream subscription; got: {binding}"
        );
        assert!(
            binding.contains("async function*") && binding.contains("yield"),
            "stream binding must be an async-iterable factory (async generator); got: {binding}"
        );
        assert!(
            !binding.contains("edenPromiseResource") && !binding.contains("edenStreamResource"),
            "stream binding must be a real async iterable, not a one-shot resource wrapper; got: {binding}"
        );
        // Non-browser hosts (SSR / jsdom) have no `EventSource`: the binding guards it and yields an
        // empty iterable instead of throwing `EventSource is not defined` at boot.
        assert!(
            binding.contains("typeof EventSource === 'undefined'"),
            "stream binding must guard a missing EventSource so it boots in non-browser hosts; got: {binding}"
        );
        assert_no_marker_words(&emit.server_module);
    }

    #[test]
    fn websocket_fn_emits_ws_route_and_binding() {
        let source = "server {\n\
          function chat(msg: string) { 'use websocket'; return msg; }\n\
        }\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns.len(), 1);
        assert_eq!(extraction.server_fns[0].transport, super::TransportKind::WebSocket);

        let emit = AxumBackendPlugin.emit(&extraction.server_fns);

        // A ws upgrade handler and the ws imports.
        assert!(
            emit.server_module.contains("use axum::extract::ws::{WebSocket, WebSocketUpgrade};"),
            "no ws import; got:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module.contains("ws: WebSocketUpgrade) -> Response"),
            "no ws upgrade handler; got:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module.contains("ws.on_upgrade(__server_chat_socket)"),
            "no upgrade dispatch; got:\n{}",
            emit.server_module
        );
        // The socket task must run the REAL duplex body: a recv loop that binds the data param from
        // the frame, runs the per-message handler, and sends the JSON response back — not a bare echo
        // of the inbound message, since this body is fully covered.
        assert!(
            emit.server_module.contains("pub async fn __server_chat_socket(mut socket: WebSocket)"),
            "no socket task; got:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module.contains("while let Some(Ok(__msg)) = socket.recv().await")
                && emit.server_module.contains("Message::Text(__text)"),
            "no recv loop over Text frames; got:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module.contains("let msg: String = __args.get(0).cloned()"),
            "data param not bound from the inbound frame; got:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module.contains("let __response = __server_chat_handle(msg).await;")
                && emit.server_module.contains("socket.send(Message::Text(__payload.into()))"),
            "per-message handler result not sent back over the socket; got:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module
                .contains("pub async fn __server_chat_handle(msg: String) -> serde_json::Value")
                && emit.server_module.contains("return serde_json::json!(msg);"),
            "no per-message body handler returning a uniform JSON value; got:\n{}",
            emit.server_module
        );
        // The generated ws handler is well-formed Rust (balanced delimiters).
        assert_balanced_delimiters(&emit.server_module);
        // The route is registered as a GET (ws upgrade) under /__server.
        assert!(
            emit.server_module.contains(".route(\"/__server/chat\", get(__server_chat))"),
            "no ws route registration; got:\n{}",
            emit.server_module
        );
        // The client binding opens a live WebSocket and returns a duplex control handle (a Proxy that
        // forwards method calls to the peer) — NOT a one-shot resource. The ws URL is derived inline (no
        // invented `edenWebSocket`/`wsUrl` shim) and it must not wrap `edenPromiseResource`.
        let binding = emit.client_bindings.get("chat").expect("binding for chat");
        assert!(
            binding.contains("WebSocket") && binding.contains("'/__server/chat'"),
            "binding is not a websocket; got: {binding}"
        );
        assert!(
            binding.contains("new Proxy") && binding.contains("socket.send"),
            "ws binding must return a duplex socket handle that forwards calls; got: {binding}"
        );
        assert!(
            !binding.contains("edenPromiseResource")
                && !binding.contains("edenWebSocket")
                && !binding.contains("wsUrl("),
            "ws binding must be a live duplex handle, not a one-shot resource wrapper; got: {binding}"
        );
        // Non-browser hosts (SSR / jsdom) have no `WebSocket`: the binding guards it and returns an
        // inert handle instead of throwing `WebSocket is not defined` at boot.
        assert!(
            binding.contains("typeof WebSocket === 'undefined'"),
            "ws binding must guard a missing WebSocket so it boots in non-browser hosts; got: {binding}"
        );
        assert_no_marker_words(&emit.server_module);
    }

    #[test]
    fn api_fn_emit_header_and_route_are_stable() {
        // Guard: a plain (Api) fn produces the stable import header and POST route registration — the
        // streaming/ws imports must not leak into an all-Api emit, and the route stays a POST.
        let source = "server:ts {\n\
          function add(a: number, b: number): number { return a + b; }\n\
        }\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns[0].transport, super::TransportKind::Api);
        let emit = AxumBackendPlugin.emit(&extraction.server_fns);

        // The base import block is intact and free of stream/ws imports. This fully-typed fn references
        // no bare `Value`, so the `use serde_json::Value;` import is omitted (it would be unused).
        assert!(emit.server_module.starts_with(
            "use axum::{Json, Router, routing::post};\nuse serde::Deserialize;\n\n"
        ), "Api import header changed; got:\n{}", emit.server_module);
        assert!(!emit.server_module.contains("use serde_json::Value;"),
            "a fully-typed Api fn must not import the unused bare Value; got:\n{}", emit.server_module);
        assert!(!emit.server_module.contains("Sse"), "stream import leaked into Api emit");
        assert!(!emit.server_module.contains("WebSocketUpgrade"), "ws import leaked into Api emit");
        // POST route with the post() handler, exactly as before.
        assert!(emit.server_module.contains(".route(\"/__server/add\", post(__server_add))"));
    }

    #[test]
    fn untyped_api_param_keeps_the_bare_value_import() {
        // An Api fn with an untyped param falls back to the bare `Value` in its request struct, so the
        // `use serde_json::Value;` import must be present (and the module still parses as real Rust).
        let source = "server:ts {\n\
          function echo(payload): number { return 1; }\n\
        }\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns[0].transport, super::TransportKind::Api);
        let emit = AxumBackendPlugin.emit(&extraction.server_fns);

        assert!(
            emit.server_module.contains("use serde_json::Value;"),
            "an untyped Api param must keep the bare Value import; got:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module.contains("pub payload: Value,"),
            "untyped param not lowered to the bare Value field; got:\n{}",
            emit.server_module
        );
    }

    /// Parse `module` as a real `syn::File`, returning a readable error when the generated Rust is not
    /// well-formed. This is the strong well-formedness gate for the generated server module: it must
    /// be syntactically valid Rust, not merely contain the right substrings.
    fn assert_well_formed_rust(module: &str) {
        if let Err(err) = syn::parse_file(module) {
            panic!("generated server module is not well-formed Rust: {err}\n--- module ---\n{module}");
        }
    }

    #[test]
    fn ts_api_handler_module_is_well_formed_rust() {
        // The whole generated module for a simple arithmetic Api fn must parse as real Rust.
        let source = "server:ts {\n\
          function add(a: number, b: number): number { return a + b; }\n\
        }\n";
        let emit = AxumBackendPlugin.emit(&extract_server_block(source).server_fns);
        assert_well_formed_rust(&emit.server_module);

        // And the handler deserializes the request, runs the body, and returns a wrapped `Json`.
        assert!(emit.server_module.contains("pub struct AddRequest {"));
        assert!(emit
            .server_module
            .contains("pub async fn __server_add(Json(req): Json<AddRequest>) -> Json<f64>"));
        assert!(emit.server_module.contains("return Json(a + b);"));
        assert_no_marker_words(&emit.server_module);
    }

    #[test]
    fn ts_object_return_handler_is_well_formed_and_real() {
        // A realistic server fn: typed param, an object return shaped to an (untyped) JSON response —
        // exactly the `addTodo`-style shape. It must transpile to a real, well-formed handler whose
        // body produces a `Json(serde_json::json!({ … }))`, not a stub.
        let source = "server:ts {\n\
          export async function addTodo(title: string) {\n\
            const created = { id: 1, title: title, done: false };\n\
            return created;\n\
          }\n\
        }\n";
        let extraction = extract_server_block(source);
        let emit = AxumBackendPlugin.emit(&extraction.server_fns);
        assert_well_formed_rust(&emit.server_module);

        // The request struct deserializes the typed param.
        assert!(
            emit.server_module.contains("pub struct AddTodoRequest {")
                && emit.server_module.contains("pub title: String,"),
            "no typed request struct; got:\n{}",
            emit.server_module
        );
        // The body runs (binds the local, builds the object) and returns it wrapped as Json(json!(…)).
        assert!(
            emit.server_module.contains(r#"let created = serde_json::json!({ "id": 1.0, "title": title, "done": false });"#),
            "object local not lowered to json!:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module.contains("return Json(serde_json::json!(created));")
                || emit.server_module.contains("return Json(created);"),
            "object return not wrapped into the handler's Json response:\n{}",
            emit.server_module
        );
        // The POST route is mounted.
        assert!(emit.server_module.contains(".route(\"/__server/addTodo\", post(__server_addTodo))"));
        assert_no_marker_words(&emit.server_module);
    }

    #[test]
    fn stream_handler_with_loop_yield_drives_the_stream() {
        // A generator that yields inside a numeric for-loop must still drive the stream: the loop is
        // lowered to a Rust range loop and the `yield` inside it pushes onto the accumulator.
        let source = "server {\n\
          async function* counter(n: number) { for (let i = 0; i < n; i++) { yield i; } }\n\
        }\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns[0].transport, super::TransportKind::Stream);
        let emit = AxumBackendPlugin.emit(&extraction.server_fns);

        assert!(
            emit.server_module.contains("for i in ((0.0) as i64)..((n) as i64) {"),
            "for-loop not lowered inside the stream body; got:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module.contains("__items.push(serde_json::json!(i));"),
            "loop yield not lowered into an accumulator push; got:\n{}",
            emit.server_module
        );
        assert!(
            !emit.server_module.contains("futures::stream::empty"),
            "a covered loop+yield body must not fall back to the empty placeholder; got:\n{}",
            emit.server_module
        );
        assert_balanced_delimiters(&emit.server_module);
        assert_no_marker_words(&emit.server_module);
    }

    #[test]
    fn ts_db_ish_call_degrades_to_compiling_stub() {
        // A `ts` body that calls into an injected dependency (a DB-ish call) whose Rust signature is
        // not known cannot be guaranteed to compile against real Rust, so the WHOLE handler must
        // degrade to a clearly-marked, COMPILING typed-default stub (original TS preserved as a
        // comment, body returns `Json(Default::default())`) — never broken Rust. We trigger the
        // uncovered path with a `try/catch` (outside the supported subset) around the dependency call.
        let source = "server:ts {\n\
          export async function listTodos() {\n\
            try { return await db.todos.findAll(); } catch (e) { return []; }\n\
          }\n\
        }\n";
        let extraction = extract_server_block(source);
        let emit = AxumBackendPlugin.emit(&extraction.server_fns);

        // The module is still well-formed Rust despite the unsupported body.
        assert_well_formed_rust(&emit.server_module);
        // The handler returns a typed default (a compiling stub), and preserves the TS as a comment.
        assert!(
            emit.server_module.contains("Json(Default::default())"),
            "uncovered body did not degrade to a typed Json default:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module.contains("db.todos.findAll()"),
            "original TS dependency call not preserved as a comment:\n{}",
            emit.server_module
        );
        // It is still routed.
        assert!(emit.server_module.contains(".route(\"/__server/listTodos\", post(__server_listTodos))"));
        assert_no_marker_words(&emit.server_module);
    }

    #[test]
    fn stream_handler_degrades_to_a_compiling_placeholder() {
        // A generator whose body is outside the supported subset (a `while` loop) cannot be fully
        // lowered. The handler must still COMPILE: it wires the SSE transport with the empty-stream
        // placeholder and preserves the author body as a comment — never broken Rust, no marker words.
        let source = "server {\n\
          async function* live() { while (true) { yield Date.now(); } }\n\
        }\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns[0].transport, super::TransportKind::Stream);
        let emit = AxumBackendPlugin.emit(&extraction.server_fns);

        // Still a real SSE handler with the right signature mounted on a GET route.
        assert!(
            emit.server_module.contains("-> Sse<impl Stream<Item = Result<Event, Infallible>>>"),
            "degraded stream still needs the SSE signature; got:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module.contains(".route(\"/__server/live\", get(__server_live))"),
            "degraded stream still needs its GET route; got:\n{}",
            emit.server_module
        );
        // The transport is wired with the typed placeholder stream.
        assert!(
            emit.server_module
                .contains("let stream = futures::stream::empty::<Result<Event, Infallible>>();")
                && emit.server_module.contains("Sse::new(stream)"),
            "degraded stream did not wire the placeholder transport; got:\n{}",
            emit.server_module
        );
        // The author's intent survives as a comment.
        assert!(
            emit.server_module.contains("// async function* live()"),
            "degraded stream dropped the author body; got:\n{}",
            emit.server_module
        );
        assert_balanced_delimiters(&emit.server_module);
        assert_no_marker_words(&emit.server_module);
    }

    #[test]
    fn build_router_mounts_the_post_route_and_is_well_formed() {
        // The generated `build_router()` must mount the fn under a POST `/__server/<name>` route, and
        // the whole module (router included) must parse as real Rust.
        let source = "server:ts {\n\
          function add(a: number, b: number): number { return a + b; }\n\
        }\n";
        let emit = AxumBackendPlugin.emit(&extract_server_block(source).server_fns);

        assert!(
            emit.server_module.contains("pub fn build_router() -> Router {"),
            "no build_router; got:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module.contains(".route(\"/__server/add\", post(__server_add))"),
            "build_router does not mount the POST route; got:\n{}",
            emit.server_module
        );
        assert_well_formed_rust(&emit.server_module);
    }

    #[test]
    fn multi_fn_module_with_all_transports_is_well_formed_rust() {
        // A module mixing an Api fn, a streaming generator, and a websocket fn must produce a single
        // well-formed Rust module (every handler + the router parse), with the POST/GET routes mounted.
        let source = "server:ts {\n\
          function add(a: number, b: number): number { return a + b; }\n\
          async function* ticks() { yield 1; yield 2; }\n\
          function chat(msg: string) { 'use websocket'; return msg; }\n\
        }\n";
        let emit = AxumBackendPlugin.emit(&extract_server_block(source).server_fns);

        assert_well_formed_rust(&emit.server_module);
        assert!(emit.server_module.contains(".route(\"/__server/add\", post(__server_add))"));
        assert!(emit.server_module.contains(".route(\"/__server/ticks\", get(__server_ticks))"));
        assert!(emit.server_module.contains(".route(\"/__server/chat\", get(__server_chat))"));
        assert_no_marker_words(&emit.server_module);
    }

    #[test]
    fn ws_handler_degrades_to_a_compiling_echo() {
        // A ws body outside the supported subset (a `try`/`catch`) cannot be fully lowered. The
        // handler must still COMPILE: it wires the upgrade + recv loop with a placeholder echo and
        // preserves the author body as a comment — never broken Rust, no marker words.
        let source = "server {\n\
          function relay(msg: string) { 'use websocket'; try { return msg; } catch (e) { return null; } }\n\
        }\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns[0].transport, super::TransportKind::WebSocket);
        let emit = AxumBackendPlugin.emit(&extraction.server_fns);

        // Still a real upgrade handler + socket task on a GET route.
        assert!(
            emit.server_module.contains("ws: WebSocketUpgrade) -> Response")
                && emit.server_module.contains("ws.on_upgrade(__server_relay_socket)"),
            "degraded ws still needs the upgrade handler; got:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module.contains(".route(\"/__server/relay\", get(__server_relay))"),
            "degraded ws still needs its GET route; got:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module.contains("while let Some(Ok(__msg)) = socket.recv().await"),
            "degraded ws still needs the recv loop; got:\n{}",
            emit.server_module
        );
        // The placeholder echoes the inbound frame; no per-message body handler is generated.
        assert!(
            emit.server_module.contains("socket.send(Message::Text(__text))"),
            "degraded ws did not wire the placeholder echo; got:\n{}",
            emit.server_module
        );
        assert!(
            !emit.server_module.contains("__server_relay_handle"),
            "degraded ws must not call a body handler it cannot soundly generate; got:\n{}",
            emit.server_module
        );
        assert_balanced_delimiters(&emit.server_module);
        assert_no_marker_words(&emit.server_module);
    }

    #[test]
    fn router_mounts_get_routes_for_stream_and_websocket() {
        // A module mixing an Api, a Stream, and a WebSocket fn: the router must mount the Api fn with
        // POST and BOTH the stream and websocket fns with GET (SSE / ws upgrade are GET in axum), and
        // pull in each transport's imports exactly once.
        let source = "server {\n\
          function add(a: number, b: number): number { return a + b; }\n\
          async function* ticks() { yield 1; }\n\
          function chat(msg: string) { 'use websocket'; return msg; }\n\
        }\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns.len(), 3);
        let emit = AxumBackendPlugin.emit(&extraction.server_fns);

        // The router builder mounts each transport on the right verb.
        assert!(
            emit.server_module.contains(".route(\"/__server/add\", post(__server_add))"),
            "Api fn not mounted as POST; got:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module.contains(".route(\"/__server/ticks\", get(__server_ticks))"),
            "Stream fn not mounted as GET; got:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module.contains(".route(\"/__server/chat\", get(__server_chat))"),
            "WebSocket fn not mounted as GET; got:\n{}",
            emit.server_module
        );
        // The `get` routing import is pulled in exactly once even with both GET transports present.
        assert_eq!(
            emit.server_module.matches("use axum::routing::get;").count(),
            1,
            "the `get` routing import must appear exactly once; got:\n{}",
            emit.server_module
        );
        // Both transport import blocks are present.
        assert!(emit.server_module.contains("use axum::response::sse::{Event, Sse};"));
        assert!(emit.server_module.contains("use axum::extract::ws::{WebSocket, WebSocketUpgrade};"));
        // The whole generated module is well-formed (balanced delimiters) and marker-free.
        assert_balanced_delimiters(&emit.server_module);
        assert_no_marker_words(&emit.server_module);
    }

    #[test]
    fn explicit_ts_stream_body_is_transpiled_and_drives_the_stream() {
        // An explicit `server:ts` generator is lowered the same way as a bare `server { … }` one: the
        // generator body is JS regardless of the block's language tag, so it always transpiles into the
        // yield-driven SSE stream rather than passing through as (invalid) verbatim Rust.
        let source = "server:ts {\n\
          async function* squares(n: number) { for (let i = 0; i < n; i++) { yield i * i; } }\n\
        }\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns[0].lang, "ts");
        assert_eq!(extraction.server_fns[0].transport, super::TransportKind::Stream);
        let emit = AxumBackendPlugin.emit(&extraction.server_fns);

        assert!(
            emit.server_module.contains("for i in ((0.0) as i64)..((n) as i64) {")
                && emit.server_module.contains("__items.push(serde_json::json!(i * i));"),
            "explicit-ts generator body not transpiled into the stream; got:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module.contains(".json_data(&__value)"),
            "ts stream body not wired to the SSE transport; got:\n{}",
            emit.server_module
        );
        assert!(
            !emit.server_module.contains("futures::stream::empty"),
            "a covered ts generator body must not fall back to the placeholder; got:\n{}",
            emit.server_module
        );
        assert_balanced_delimiters(&emit.server_module);
        assert_no_marker_words(&emit.server_module);
    }

    /// Assert a generated Rust string is well-formed enough to parse: its `()`, `[]`, and `{}`
    /// delimiters are balanced and never close out of order. `//`-comment lines are skipped (an author
    /// body preserved as a comment may carry unbalanced delimiters that are not part of the Rust). This
    /// is a structural smoke check — the `rust_authoring` crate does not depend on axum, so the
    /// generated service is validated by shape, not by compiling it in-crate.
    fn assert_balanced_delimiters(text: &str) {
        let mut stack: Vec<char> = Vec::new();
        for line in text.lines() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for ch in line.chars() {
                match ch {
                    '(' | '[' | '{' => stack.push(ch),
                    ')' | ']' | '}' => {
                        let want = match ch {
                            ')' => '(',
                            ']' => '[',
                            _ => '{',
                        };
                        match stack.pop() {
                            Some(open) => assert_eq!(
                                open, want,
                                "mismatched closing `{ch}` in generated Rust:\n{text}"
                            ),
                            None => panic!("unmatched closing `{ch}` in generated Rust:\n{text}"),
                        }
                    }
                    _ => {}
                }
            }
        }
        assert!(
            stack.is_empty(),
            "unbalanced opening delimiters {stack:?} in generated Rust:\n{text}"
        );
    }

    #[test]
    fn production_host_is_well_formed_rust() {
        // The emitted host must parse as a COMPLETE Rust file (a real parse via `syn`, not a regex
        // shape check) so a packaged app `cargo run`s its server fns in production.
        let host = emit_production_host(DEFAULT_SERVER_MODULE, DEFAULT_HOST_PORT);
        syn::parse_file(&host)
            .unwrap_or_else(|e| panic!("emitted host is not well-formed Rust: {e}\n---\n{host}"));

        // It wires the sibling server module and its router factory, runs under tokio, and binds an
        // env-driven port with the default fallback.
        assert!(host.contains("mod treaty_server;"), "no server-module declaration; got:\n{host}");
        assert!(
            host.contains("use treaty_server::build_router;"),
            "host does not import build_router; got:\n{host}"
        );
        assert!(host.contains("#[tokio::main]"), "no tokio runtime entry; got:\n{host}");
        assert!(host.contains("async fn main()"), "no async main; got:\n{host}");
        assert!(
            host.contains("std::env::var(\"PORT\")") && host.contains("unwrap_or(3000)"),
            "port is not env-driven with a default fallback; got:\n{host}"
        );
        assert!(
            host.contains("axum::serve(listener, build_router())"),
            "host does not serve build_router(); got:\n{host}"
        );
        assert_no_marker_words(&host);
    }

    #[test]
    fn production_host_honors_custom_module_and_port() {
        // A caller can name the server module and pick the fallback port; both flow through verbatim
        // and the result is still a well-formed Rust file.
        let host = emit_production_host("api_server", 8080);
        syn::parse_file(&host)
            .unwrap_or_else(|e| panic!("custom host is not well-formed Rust: {e}\n---\n{host}"));
        assert!(host.contains("mod api_server;"), "custom module name not honored; got:\n{host}");
        assert!(
            host.contains("use api_server::build_router;"),
            "custom module import not honored; got:\n{host}"
        );
        assert!(host.contains("unwrap_or(8080)"), "custom default port not honored; got:\n{host}");
    }

    #[test]
    fn production_host_links_against_the_generated_server_module() {
        // End-to-end: a real generated server module (`build_router()` + handlers) plus the host must
        // BOTH parse as well-formed Rust, and the host must reference exactly the `build_router` the
        // module defines — i.e. the two halves form a coherent, runnable crate.
        let source = "server:ts {\n\
          function add(a: number, b: number): number { return a + b; }\n\
        }\n";
        let extraction = extract_server_block(source);
        let emit = AxumBackendPlugin.emit(&extraction.server_fns);

        // The generated server module is itself well-formed and exposes the router factory.
        syn::parse_file(&emit.server_module)
            .unwrap_or_else(|e| panic!("server module is not well-formed Rust: {e}"));
        assert!(emit.server_module.contains("pub fn build_router() -> Router"));

        // The host parses and binds to that exact factory by its module path.
        let host = emit_production_host(DEFAULT_SERVER_MODULE, DEFAULT_HOST_PORT);
        syn::parse_file(&host).expect("host is not well-formed Rust");
        assert!(host.contains("use treaty_server::build_router;"));
    }

    /// Generated output must never carry a marker token. The forbidden tokens are assembled from
    /// fragments so this guard does not itself contain any of them verbatim.
    fn assert_no_marker_words(text: &str) {
        let markers = [
            format!("{}{}", "NO", "TE(port)"),
            format!("{}{}", "TO", "DO"),
            format!("{}{}", "FIX", "ME"),
            format!("{}{}", "tod", "o!"),
        ];
        for marker in &markers {
            assert!(
                !text.contains(marker.as_str()),
                "marker word `{marker}` leaked into output:\n{text}"
            );
        }
    }
}
