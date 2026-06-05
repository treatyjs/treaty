//! Reference [`BackendPlugin`] targeting an [Elysia](https://elysiajs.com) server with an
//! [Eden](https://elysiajs.com/eden/overview) treaty client.
//!
//! This is the *reference* backend: it shows how a [`BackendPlugin`] turns extracted server
//! functions into (a) a runnable server module and (b) per-fn client call bindings. The shape it
//! emits is faithful and well-formed; exact Elysia/Eden typing is approximate for v1.
//!
//! For server functions `f`, [`ElysiaEdenPlugin::emit`] produces:
//!
//! * a **server module** that declares each `f`'s body verbatim and mounts it as an Elysia `POST`
//!   route at `/__server/<f>` whose handler invokes `f` with the parsed request body, then exports
//!   the `App` type so Eden can derive the client type, and
//! * **client bindings** mapping each `f` -> `client.__server.<f>.post`, the typed Eden call
//!   expression that [`crate::plugin::rewrite_call_sites`] substitutes for free references to `f`.

use std::collections::HashMap;

use super::{BackendEmit, BackendPlugin, ServerFn, TransportKind};

/// The URL namespace every lifted server function is mounted under, e.g. `save` -> `/__server/save`.
const SERVER_ROUTE_PREFIX: &str = "/__server";

/// Reference Elysia/Eden backend. Each [`ServerFn`] becomes a `POST /__server/<name>` route on a
/// single exported Elysia `app`; the client binding routes calls through the generated Eden
/// `client`.
pub struct ElysiaEdenPlugin;

impl BackendPlugin for ElysiaEdenPlugin {
    fn name(&self) -> &str {
        "elysia-eden"
    }

    fn emit(&self, fns: &[ServerFn]) -> BackendEmit {
        let mut server_module = String::new();
        server_module.push_str("import { Elysia } from 'elysia';\n");
        server_module.push_str("import { treaty } from '@elysiajs/eden';\n\n");

        // 1. Declare each author handler verbatim, so the route handler can call it by name and the
        //    body the author wrote is preserved exactly.
        for f in fns {
            server_module.push_str(&f.source);
            server_module.push_str("\n\n");
        }

        // 2. Mount one POST route per fn under the `/__server` namespace. The handler destructures
        //    the request `body` and forwards it to the fn. We pass the named params positionally
        //    (`body.<param>`), falling back to the whole `body` when the fn takes a single unnamed
        //    arg, so the call shape matches the author's signature.
        server_module.push_str("export const app = new Elysia()\n");
        for f in fns {
            match f.transport {
                TransportKind::Api => {
                    let args = call_args(f);
                    server_module.push_str(&format!(
                        "  .post('{prefix}/{name}', ({{ body }}) => {name}({args}))\n",
                        prefix = SERVER_ROUTE_PREFIX,
                        name = f.name,
                    ));
                }
                TransportKind::Stream => {
                    // A streaming GET route whose handler returns the async generator directly; Elysia
                    // streams each yielded value to the client.
                    let args = call_args(f);
                    server_module.push_str(&format!(
                        "  .get('{prefix}/{name}', ({{ body }}) => {name}({args}))\n",
                        prefix = SERVER_ROUTE_PREFIX,
                        name = f.name,
                    ));
                }
                TransportKind::WebSocket => {
                    // A ws upgrade route; the author body runs per-message in the `message` handler.
                    let args = call_args(f);
                    server_module.push_str(&format!(
                        "  .ws('{prefix}/{name}', {{ message(ws, body) {{ ws.send({name}({args})); }} }})\n",
                        prefix = SERVER_ROUTE_PREFIX,
                        name = f.name,
                    ));
                }
            }
        }
        server_module.push_str("  ;\n\n");

        // 3. Export the app type so Eden derives a fully typed client from it.
        server_module.push_str("export type App = typeof app;\n");

        // Client bindings: a free call `save(arg)` becomes `client.__server.save.post(arg)` for Api;
        // Stream fns subscribe via `.get.subscribe`, WebSocket fns open the Eden `.subscribe` socket.
        // The rewrite is identifier-aware, so it swaps the callee and leaves the argument list intact.
        let mut client_bindings = HashMap::new();
        for f in fns {
            let binding = match f.transport {
                TransportKind::Api => format!("client.__server.{}.post", f.name),
                TransportKind::Stream => format!("client.__server.{}.get", f.name),
                TransportKind::WebSocket => format!("client.__server.{}.subscribe", f.name),
            };
            client_bindings.insert(f.name.clone(), binding);
        }

        BackendEmit { server_module, client_bindings }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::extract_server_block;

    #[test]
    fn stream_fn_emits_get_stream_route_and_binding() {
        let source = "server:ts {\n\
          async function* ticks() { yield 1; }\n\
        }\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns[0].transport, TransportKind::Stream);
        let emit = ElysiaEdenPlugin.emit(&extraction.server_fns);
        assert!(
            emit.server_module.contains(".get('/__server/ticks'"),
            "no stream GET route; got:\n{}",
            emit.server_module
        );
        assert_eq!(
            emit.client_bindings.get("ticks").map(String::as_str),
            Some("client.__server.ticks.get")
        );
    }

    #[test]
    fn websocket_fn_emits_ws_route_and_binding() {
        let source = "server:ts {\n\
          function chat(msg: string) { 'use websocket'; return msg; }\n\
        }\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns[0].transport, TransportKind::WebSocket);
        let emit = ElysiaEdenPlugin.emit(&extraction.server_fns);
        assert!(
            emit.server_module.contains(".ws('/__server/chat'"),
            "no ws route; got:\n{}",
            emit.server_module
        );
        assert_eq!(
            emit.client_bindings.get("chat").map(String::as_str),
            Some("client.__server.chat.subscribe")
        );
    }

    #[test]
    fn api_fn_emits_post_route_unchanged() {
        let source = "server:ts {\n\
          async function save(user: User) { return db.insert(user); }\n\
        }\n";
        let extraction = extract_server_block(source);
        assert_eq!(extraction.server_fns[0].transport, TransportKind::Api);
        let emit = ElysiaEdenPlugin.emit(&extraction.server_fns);
        assert!(emit.server_module.contains(".post('/__server/save', ({ body }) => save(body))"));
        assert_eq!(
            emit.client_bindings.get("save").map(String::as_str),
            Some("client.__server.save.post")
        );
    }
}

/// The argument expression a route handler passes to the lifted fn.
///
/// With named params we forward `body.<param>` in declaration order; with a single param we forward
/// the whole `body`; with no params we forward nothing.
fn call_args(f: &ServerFn) -> String {
    match f.params.len() {
        0 => String::new(),
        1 => "body".to_string(),
        _ => f
            .params
            .iter()
            .map(|p| format!("body.{}", p.name))
            .collect::<Vec<_>>()
            .join(", "),
    }
}
