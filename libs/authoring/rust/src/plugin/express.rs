//! Opt-in [`BackendPlugin`] targeting a runnable [Express](https://expressjs.com) JS server.
//!
//! This backend exists so a developer can *run* a real JS server: [`ExpressBackendPlugin::emit`]
//! produces a complete, standalone Express application source string (every lifted [`ServerFn`]
//! plumbed in as a route) plus the matching typesafe client bindings. Unlike the
//! [`super::ElysiaEdenPlugin`] reference (which leans on Eden's derived typing) and the default
//! [`super::AxumBackendPlugin`] (which emits Rust), this emits plain Node/Express so `node server.js`
//! just works.
//!
//! For server functions `f`, the emitted **server module** is a single file that:
//!   * imports Express (CommonJS `require`) and `express.json()` body parsing,
//!   * declares each `f`'s body verbatim so the route handler can call it by name,
//!   * registers a handler per `f` keyed by its [`TransportKind`]:
//!       - [`TransportKind::Api`] — `app.post('/__server/<name>', …)`, JSON in / JSON out,
//!       - [`TransportKind::Stream`] — a Server-Sent-Events endpoint (`text/event-stream`) on a GET
//!         route that drives the author's (async-generator) body and writes each yielded value,
//!       - [`TransportKind::WebSocket`] — a WebSocket endpoint registered with the
//!         [`express-ws`](https://www.npmjs.com/package/express-ws) convention (`app.ws(…)`), which
//!         the emitted preamble wires up via `require('express-ws')(app)`; the author body runs
//!         per-message,
//!   * and finally `app.listen(PORT)` so the file is directly runnable.
//!
//! The emitted **client bindings** map each `f` to a typesafe call expression against
//! `/__server/<name>`, consistent with the resource-client style used by [`super::AxumBackendPlugin`]
//! (`edenHttpResource` / `edenStreamResource` / `edenWebSocket` factories).

use std::collections::HashMap;

use super::{BackendEmit, BackendPlugin, ServerFn, TransportKind};

/// The URL namespace every lifted server function is mounted under, e.g. `save` -> `/__server/save`.
const SERVER_ROUTE_PREFIX: &str = "/__server";

/// Stable registry name for this backend.
const PLUGIN_NAME: &str = "express";

/// Opt-in Express backend. Each [`ServerFn`] becomes a route on a single runnable Express `app`;
/// the client binding routes calls through a typed resource over HTTP / SSE / WebSocket.
pub struct ExpressBackendPlugin;

impl BackendPlugin for ExpressBackendPlugin {
    fn name(&self) -> &str {
        PLUGIN_NAME
    }

    fn emit(&self, fns: &[ServerFn]) -> BackendEmit {
        let has_ws = fns.iter().any(|f| f.transport == TransportKind::WebSocket);

        let mut server_module = String::new();

        // 1. Preamble: a runnable Express app. Body parsing is enabled so Api/Stream handlers can read
        //    `req.body`. When any fn is a WebSocket, wire up the `express-ws` convention so `app.ws`
        //    is available (a no-op for non-ws apps, so we only require it when needed).
        server_module.push_str("const express = require('express');\n");
        if has_ws {
            server_module.push_str("const expressWs = require('express-ws');\n");
        }
        server_module.push('\n');
        server_module.push_str("const app = express();\n");
        if has_ws {
            // `express-ws` augments `app` with `.ws(...)`. It must run before any `app.ws` call.
            server_module.push_str("expressWs(app);\n");
        }
        server_module.push_str("app.use(express.json());\n\n");

        // 2. Declare each author handler verbatim, so the route handler can call it by name and the
        //    body the author wrote is preserved exactly.
        for f in fns {
            server_module.push_str(&f.source);
            server_module.push_str("\n\n");
        }

        // 3. One handler per fn, keyed by transport.
        for f in fns {
            match f.transport {
                TransportKind::Api => server_module.push_str(&emit_api_route(f)),
                TransportKind::Stream => server_module.push_str(&emit_stream_route(f)),
                TransportKind::WebSocket => server_module.push_str(&emit_ws_route(f)),
            }
            server_module.push('\n');
        }

        // 4. Listen so the file is directly runnable (`node server.js`).
        server_module.push_str("const PORT = process.env.PORT || 3000;\n");
        server_module
            .push_str("app.listen(PORT, () => console.log(`server listening on ${PORT}`));\n");

        // Client bindings: a free call `save(arg)` becomes a typed resource call against
        // `/__server/save`. The rewrite is identifier-aware, so it swaps the callee and leaves the
        // argument list intact. Stream / WebSocket fns get their EventSource / WebSocket factories.
        let mut client_bindings = HashMap::new();
        for f in fns {
            let binding = match f.transport {
                TransportKind::Api => api_client_binding(f),
                TransportKind::Stream => stream_client_binding(f),
                TransportKind::WebSocket => ws_client_binding(f),
            };
            client_bindings.insert(f.name.clone(), binding);
        }

        BackendEmit { server_module, client_bindings }
    }
}

/// The argument expression a route handler passes to the lifted fn, sourced from `req.body`.
///
/// With named params we forward `req.body.<param>` in declaration order; with a single param we
/// forward the whole `req.body`; with no params we forward nothing. This mirrors the call-shape
/// convention used by the Elysia reference backend.
fn call_args(f: &ServerFn) -> String {
    match f.params.len() {
        0 => String::new(),
        1 => "req.body".to_string(),
        _ => f
            .params
            .iter()
            .map(|p| format!("req.body.{}", p.name))
            .collect::<Vec<_>>()
            .join(", "),
    }
}

/// Emit an `app.post('/__server/<name>', …)` handler. The handler calls the author fn with the
/// destructured request body, awaits the (possibly async) result, and returns it as JSON.
fn emit_api_route(f: &ServerFn) -> String {
    let args = call_args(f);
    format!(
        "app.post('{prefix}/{name}', async (req, res) => {{\n\
         \x20 const result = await {name}({args});\n\
         \x20 res.json(result);\n\
         }});\n",
        prefix = SERVER_ROUTE_PREFIX,
        name = f.name,
    )
}

/// Emit a Server-Sent-Events endpoint for a [`TransportKind::Stream`] fn. The GET route sets the
/// `text/event-stream` headers, drives the author's async-generator body, and writes each yielded
/// value as an SSE `data:` frame, closing the response when the generator completes.
fn emit_stream_route(f: &ServerFn) -> String {
    let args = call_args(f);
    format!(
        "app.get('{prefix}/{name}', async (req, res) => {{\n\
         \x20 res.setHeader('Content-Type', 'text/event-stream');\n\
         \x20 res.setHeader('Cache-Control', 'no-cache');\n\
         \x20 res.setHeader('Connection', 'keep-alive');\n\
         \x20 for await (const chunk of {name}({args})) {{\n\
         \x20   res.write(`data: ${{JSON.stringify(chunk)}}\\n\\n`);\n\
         \x20 }}\n\
         \x20 res.end();\n\
         }});\n",
        prefix = SERVER_ROUTE_PREFIX,
        name = f.name,
    )
}

/// Emit a WebSocket endpoint for a [`TransportKind::WebSocket`] fn using the `express-ws`
/// convention (`app.ws('/__server/<name>', …)`, enabled by the `require('express-ws')(app)` line in
/// the preamble). The author body runs per inbound message; its result is sent back over the socket.
fn emit_ws_route(f: &ServerFn) -> String {
    // For a ws handler the message payload arrives per-message (not on `req.body`): the parsed
    // message is `data`. A single-param fn receives the whole parsed message; a multi-param fn
    // receives the parsed object's named fields.
    let call_args_ws = match f.params.len() {
        0 => String::new(),
        1 => "data".to_string(),
        _ => f
            .params
            .iter()
            .map(|p| format!("data.{}", p.name))
            .collect::<Vec<_>>()
            .join(", "),
    };
    format!(
        "app.ws('{prefix}/{name}', (ws, req) => {{\n\
         \x20 ws.on('message', async (msg) => {{\n\
         \x20   const data = JSON.parse(msg);\n\
         \x20   const result = await {name}({call_args_ws});\n\
         \x20   ws.send(JSON.stringify(result));\n\
         \x20 }});\n\
         }});\n",
        prefix = SERVER_ROUTE_PREFIX,
        name = f.name,
    )
}

/// The typed arrow parameter list for a client binding factory.
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

/// The client-side binding for an [`TransportKind::Api`] fn: a factory taking the fn's typed args
/// and yielding an `edenHttpResource` that POSTs the named-param payload to `/__server/<name>`.
fn api_client_binding(f: &ServerFn) -> String {
    let route = format!("{SERVER_ROUTE_PREFIX}/{}", f.name);
    let arg_list = binding_arg_list(f);
    let body_expr = match f.params.len() {
        0 => "{}".to_string(),
        1 => {
            let name = &f.params[0].name;
            format!("{{ {name}: {name} }}")
        }
        _ => {
            let body = f
                .params
                .iter()
                .map(|p| p.name.clone())
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{ {body} }}")
        }
    };
    format!(
        "(({arg_list}) => edenHttpResource(() => httpClient.post('{route}', {body_expr})))",
    )
}

/// The client-side binding for a [`TransportKind::Stream`] fn: a factory that opens an `EventSource`
/// to the fn's `/__server/<name>` GET route, exposing a streaming subscription typed to the response.
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

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{extract_server_block, PluginRegistry};

    #[test]
    fn registry_resolves_express_opt_in() {
        let registry = PluginRegistry::with_defaults();
        // axum stays the default; express is registered after axum + elysia and selected by name.
        assert_eq!(registry.default_plugin().map(|p| p.name()), Some("axum"));
        assert!(registry.get("express").is_some());
        assert_eq!(registry.get("express").map(|p| p.name()), Some("express"));
    }

    #[test]
    fn api_fn_emits_runnable_express_app_and_post_route() {
        let source = "server:ts {\n\
          async function save(user: User) { return db.insert(user); }\n\
        }\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns[0].transport, TransportKind::Api);

        let emit = ExpressBackendPlugin.emit(&extraction.server_fns);

        // A runnable Express app: require, app(), json() body parsing, and listen.
        assert!(
            emit.server_module.contains("const express = require('express');"),
            "no express require; got:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module.contains("const app = express();"),
            "no app; got:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module.contains("app.use(express.json());"),
            "no json body parsing; got:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module.contains("app.listen(PORT"),
            "no listen; got:\n{}",
            emit.server_module
        );
        // The POST route under /__server.
        assert!(
            emit.server_module.contains("app.post('/__server/save'"),
            "no post route; got:\n{}",
            emit.server_module
        );
        // The author body passes through verbatim.
        assert!(
            emit.server_module.contains("db.insert(user)"),
            "author body not preserved; got:\n{}",
            emit.server_module
        );
        // express-ws is not pulled in for an all-Api app.
        assert!(
            !emit.server_module.contains("express-ws"),
            "express-ws leaked into a non-ws emit; got:\n{}",
            emit.server_module
        );

        // A client binding to /__server/save.
        let binding = emit.client_bindings.get("save").expect("binding for save");
        assert!(
            binding.contains("'/__server/save'"),
            "binding does not target the server route; got: {binding}"
        );
        assert!(
            binding.contains("httpClient.post"),
            "binding does not POST via the http client; got: {binding}"
        );

        assert_no_marker_words(&emit.server_module);
    }

    #[test]
    fn stream_fn_emits_sse_endpoint_and_binding() {
        let source = "server:ts {\n\
          async function* ticks() { yield 1; yield 2; }\n\
        }\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns[0].transport, TransportKind::Stream);

        let emit = ExpressBackendPlugin.emit(&extraction.server_fns);

        // A streaming GET route with SSE headers driving the generator.
        assert!(
            emit.server_module.contains("app.get('/__server/ticks'"),
            "no stream GET route; got:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module.contains("text/event-stream"),
            "no SSE content type; got:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module.contains("for await (const chunk of ticks()"),
            "generator body not driven; got:\n{}",
            emit.server_module
        );
        // Still a runnable app.
        assert!(emit.server_module.contains("app.listen(PORT"));

        // The client binding opens an EventSource stream.
        let binding = emit.client_bindings.get("ticks").expect("binding for ticks");
        assert!(
            binding.contains("EventSource") && binding.contains("'/__server/ticks'"),
            "binding is not a stream subscription; got: {binding}"
        );

        assert_no_marker_words(&emit.server_module);
    }

    #[test]
    fn websocket_fn_emits_ws_endpoint_and_binding() {
        let source = "server:ts {\n\
          function chat(msg: string) { 'use websocket'; return msg; }\n\
        }\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns[0].transport, TransportKind::WebSocket);

        let emit = ExpressBackendPlugin.emit(&extraction.server_fns);

        // express-ws is wired up and the ws route is registered.
        assert!(
            emit.server_module.contains("require('express-ws')")
                || emit.server_module.contains("expressWs(app);"),
            "express-ws not wired up; got:\n{}",
            emit.server_module
        );
        assert!(
            emit.server_module.contains("app.ws('/__server/chat'"),
            "no ws route; got:\n{}",
            emit.server_module
        );
        // The author body runs per-message.
        assert!(
            emit.server_module.contains("ws.on('message'"),
            "no per-message handler; got:\n{}",
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
    fn multi_param_api_fn_forwards_named_body_fields() {
        let source = "server:ts {\n\
          function add(a: number, b: number): number { return a + b; }\n\
        }\n";
        let extraction = extract_server_block(source);
        let emit = ExpressBackendPlugin.emit(&extraction.server_fns);

        // Named params are forwarded as req.body.<param>.
        assert!(
            emit.server_module.contains("add(req.body.a, req.body.b)"),
            "named params not forwarded; got:\n{}",
            emit.server_module
        );
        // The binding posts the named-param payload object.
        let binding = emit.client_bindings.get("add").expect("binding for add");
        assert!(
            binding.contains("{ a, b }"),
            "binding does not post named payload; got: {binding}"
        );
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
