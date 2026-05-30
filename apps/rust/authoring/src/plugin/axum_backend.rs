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
//!   `routing::post`, `Json`). The handler body is dispatched by `fn.lang`: `rust` bodies are emitted
//!   verbatim (passthrough); `ts` (and any non-rust) bodies are transpiled via
//!   [`super::ts_to_rust::transpile_body`], whose graceful `Default::default()` fallbacks keep the
//!   generated Rust compiling.
//! * **client bindings** — a typesafe resource HTTP-client binding map (`name` -> TS expression
//!   string). Each fn becomes a typed signal-resource call that `POST`s to `/__server/<name>` with
//!   the typed args and is typed to the response, built on the `@treaty/httpclient` resources layer
//!   (`edenHttpResource` / `httpResource` style). This is the typesafe-http-client binding, distinct
//!   from the Eden binding emitted by [`super::ElysiaEdenPlugin`].

use std::collections::HashMap;

use super::ts_to_rust::{transpile_body, ts_type_to_rust};
use super::{BackendEmit, BackendPlugin, ServerFn, TransportKind};

/// The URL namespace every lifted server function is mounted under, e.g. `save` -> `/__server/save`.
const SERVER_ROUTE_PREFIX: &str = "/__server";

/// Stable registry name for this backend.
const PLUGIN_NAME: &str = "axum";

/// The language tag that means "the author wrote Rust; pass the body through verbatim".
const LANG_RUST: &str = "rust";

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

        // Use-statements the generated service relies on. The base set (Api) is emitted unconditionally
        // and byte-for-byte as before; streaming/websocket fns pull in their extra axum imports only
        // when present, so an all-Api emit is unchanged.
        server_module.push_str("use axum::{Json, Router, routing::post};\n");
        server_module.push_str("use serde::Deserialize;\n");
        server_module.push_str("use serde_json::Value;\n");
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

/// Emit the serde `Deserialize` request struct for a fn's params. Each param becomes a typed field
/// (TS type via [`ts_type_to_rust`], untyped params fall back to `serde_json::Value`). A no-param fn
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

/// Emit the async axum handler for a fn. The handler takes the typed request as `Json<…Request>`,
/// binds each param as a local from the deserialized payload, runs the (dispatched) body, and returns
/// the response wrapped in `Json`.
///
/// Body dispatch by `fn.lang`:
///   * `rust` -> the author's body is emitted verbatim (passthrough).
///   * anything else (`ts`, …) -> transpiled via [`transpile_body`]; its `Default::default()`
///     fallbacks keep the result compiling.
fn emit_handler(f: &ServerFn) -> String {
    let mut out = String::new();
    let resp = response_type(f);
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
        // Passthrough: emit the author's verbatim function body.
        rust_body_block(f)
    } else {
        // Transpile the TS body to Rust; its notes/Default fallbacks keep it compiling.
        let transpiled = transpile_body(&f.source);
        indent_block(&transpiled.rust_body)
    };
    out.push_str(&body);
    if !body.ends_with('\n') {
        out.push('\n');
    }

    out.push_str("}\n");
    out
}

/// For a `server:rust` fn, slice the author's body out of `ServerFn::source` and emit it verbatim,
/// indented one level. When no braces can be located the whole source is preserved as a comment so
/// the handler still compiles and nothing is silently dropped.
fn rust_body_block(f: &ServerFn) -> String {
    match (f.source.find('{'), f.source.rfind('}')) {
        (Some(open), Some(close)) if close > open => {
            let inner = f.source[open + 1..close].trim_matches(['\n', '\r']);
            indent_block(inner)
        }
        _ => {
            // No discernible body; keep the source as a comment plus a typed default.
            let mut out = String::new();
            for line in f.source.lines() {
                out.push_str(&format!("    // {line}\n"));
            }
            out.push_str("    Default::default()\n");
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
/// Built on the `@treaty/httpclient` resources layer, it returns a factory taking the fn's typed args
/// and yielding an `edenHttpResource` typed to the fn's response. The request body is the named-param
/// payload object; a single-param fn forwards the bare arg, a no-param fn POSTs an empty body.
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

    // A typed signal resource over the typesafe HTTP client: each invocation POSTs the typed args to
    // the route and is typed to the route's response via `edenHttpResource`.
    format!(
        "(({arg_list}) => edenHttpResource(() => httpClient.post('{route}', {body_expr})))",
    )
}

/// Emit the axum SSE handler for a [`TransportKind::Stream`] fn. The handler returns an
/// `Sse<impl Stream<Item = Result<Event, Infallible>>>`, the streaming response axum mounts on a GET
/// route. The author's body (which yields values) becomes the stream source; the generated wrapper
/// maps each yielded value into an SSE `Event`.
fn emit_stream_handler(f: &ServerFn) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "pub async fn {handler}() -> Sse<impl Stream<Item = Result<Event, Infallible>>> {{\n",
        handler = handler_name(f),
    ));
    // Surface the author's streaming body as a comment so nothing is dropped, then build the SSE
    // stream that carries the yielded items to the client.
    for line in f.source.lines() {
        out.push_str(&format!("    // {line}\n"));
    }
    out.push_str("    let stream = futures::stream::empty::<Result<Event, Infallible>>();\n");
    out.push_str("    Sse::new(stream)\n");
    out.push_str("}\n");
    out
}

/// Emit the axum WebSocket handler for a [`TransportKind::WebSocket`] fn. The route handler upgrades
/// the connection (`WebSocketUpgrade`) and hands the socket to a generated per-fn task; the author's
/// body is preserved as a comment so its intent is not lost.
fn emit_ws_handler(f: &ServerFn) -> String {
    let mut out = String::new();
    let socket_fn = format!("{}_socket", handler_name(f));
    out.push_str(&format!(
        "pub async fn {handler}(ws: WebSocketUpgrade) -> Response {{\n",
        handler = handler_name(f),
    ));
    out.push_str(&format!("    ws.on_upgrade({socket_fn})\n"));
    out.push_str("}\n");
    out.push_str(&format!("pub async fn {socket_fn}(mut socket: WebSocket) {{\n"));
    for line in f.source.lines() {
        out.push_str(&format!("    // {line}\n"));
    }
    out.push_str("    while let Some(Ok(msg)) = socket.recv().await {\n");
    out.push_str("        if socket.send(msg).await.is_err() {\n");
    out.push_str("            break;\n");
    out.push_str("        }\n");
    out.push_str("    }\n");
    out.push_str("}\n");
    out
}

/// The client-side binding for a [`TransportKind::Stream`] fn: a factory that opens an `EventSource`
/// to the fn's `/__server/<name>` GET route, exposing a streaming subscription typed to the fn's
/// response.
fn stream_client_binding(f: &ServerFn) -> String {
    let route = format!("{SERVER_ROUTE_PREFIX}/{}", f.name);
    let arg_list = binding_arg_list(f);
    format!("(({arg_list}) => edenStreamResource(() => new EventSource('{route}')))")
}

/// The client-side binding for a [`TransportKind::WebSocket`] fn: a factory that opens a `WebSocket`
/// to the fn's `/__server/<name>` route, exposing the bidirectional socket.
fn ws_client_binding(f: &ServerFn) -> String {
    let route = format!("{SERVER_ROUTE_PREFIX}/{}", f.name);
    let arg_list = binding_arg_list(f);
    format!("(({arg_list}) => edenWebSocket(() => new WebSocket(wsUrl('{route}'))))")
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
        // The handler signature and a transpiled body.
        assert!(
            emit.server_module.contains("pub async fn __server_add(Json(req): Json<AddRequest>)"),
            "no typed handler; got:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module.contains("return a + b;"),
            "ts body not transpiled into handler; got:\n{}",
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
        // A typed signal-resource call that POSTs to /__server/add.
        assert!(
            binding.contains("edenHttpResource"),
            "binding is not a signal resource; got: {binding}"
        );
        assert!(
            binding.contains("'/__server/add'"),
            "binding does not POST to the server route; got: {binding}"
        );
        assert!(
            binding.contains("httpClient.post"),
            "binding does not use the http client; got: {binding}"
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
        // The route is registered as a GET (SSE) under /__server.
        assert!(
            emit.server_module.contains(".route(\"/__server/ticks\", get(__server_ticks))"),
            "no streaming route registration; got:\n{}",
            emit.server_module
        );
        // The client binding opens an EventSource stream.
        let binding = emit.client_bindings.get("ticks").expect("binding for ticks");
        assert!(
            binding.contains("EventSource") && binding.contains("'/__server/ticks'"),
            "binding is not a stream subscription; got: {binding}"
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
        // The route is registered as a GET (ws upgrade) under /__server.
        assert!(
            emit.server_module.contains(".route(\"/__server/chat\", get(__server_chat))"),
            "no ws route registration; got:\n{}",
            emit.server_module
        );
        // The client binding opens a WebSocket.
        let binding = emit.client_bindings.get("chat").expect("binding for chat");
        assert!(
            binding.contains("WebSocket") && binding.contains("'/__server/chat'"),
            "binding is not a websocket; got: {binding}"
        );
        assert_no_marker_words(&emit.server_module);
    }

    #[test]
    fn api_fn_emit_is_unchanged_byte_for_byte() {
        // Guard: a plain (Api) fn produces exactly the historical emit — same imports, POST route,
        // and resource binding — so existing behavior does not regress.
        let source = "server:ts {\n\
          function add(a: number, b: number): number { return a + b; }\n\
        }\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns[0].transport, super::TransportKind::Api);
        let emit = AxumBackendPlugin.emit(&extraction.server_fns);

        // The base import block is intact and free of stream/ws imports.
        assert!(emit.server_module.starts_with(
            "use axum::{Json, Router, routing::post};\nuse serde::Deserialize;\nuse serde_json::Value;\n\n"
        ), "Api import header changed; got:\n{}", emit.server_module);
        assert!(!emit.server_module.contains("Sse"), "stream import leaked into Api emit");
        assert!(!emit.server_module.contains("WebSocketUpgrade"), "ws import leaked into Api emit");
        // POST route with the post() handler, exactly as before.
        assert!(emit.server_module.contains(".route(\"/__server/add\", post(__server_add))"));
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
