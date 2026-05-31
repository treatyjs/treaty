//! `node:https` — the TLS-secured counterpart of `node:http` (`request`, `get`, `createServer`,
//! `Agent`, `globalAgent`), layered on the real TLS transport in [`crate::node::tls`].
//!
//! ## Layering
//!
//! `node:https` owns no transport of its own: it reuses the `node:http` request/response object model
//! (the `ClientRequest`/`IncomingMessage` shapes, `res.writeHead`/`res.end`, …) but routes every
//! request over the TLS client/server primitives `node:tls` publishes on the shared `__treaty_tls`
//! global. A `node:tls` import is forced first (so that global is present) and then a small bootstrap
//! builds the `https.*` surface on top. The handshake, certificate verification, and the encrypted
//! HTTP/1.1 exchange are all real (rustls + the `ring` provider); see `tls.rs`.
//!
//! All socket + handshake I/O runs on the reactor's background threads (see `net.rs`); the single JS
//! thread drives requests to completion through the shared event-loop pump.

use nova_vm::ecmascript::{Agent, Object, OrdinaryObject};

use crate::node::core::{InstallError, NodeCtx};
use crate::node::{GcScope, NodeModule};

/// Zero-sized marker for the `node:https` builtin.
pub(crate) struct HttpsModule;

impl NodeModule for HttpsModule {
    const SPECIFIER: &'static str = "https";

    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: GcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError> {
        install(agent, ctx, gc)
    }
}

/// Build a tiny natives object for the https bootstrap. The TLS transport itself is reached through
/// the `__treaty_tls` global that the eagerly-installed `node:tls` module publishes; the only native
/// `node:https` needs of its own is forcing that module to materialize.
fn build_natives<'gc>(agent: &mut Agent, gc: GcScope<'gc, '_>) -> OrdinaryObject<'gc> {
    let gc = gc.into_nogc();
    OrdinaryObject::create_empty_object(agent, gc)
}

/// Uniform per-module entry. Returns the `node:https` exports object.
///
/// First materializes `node:tls` (which publishes the `__treaty_tls` transport global), then evaluates
/// [`HTTPS_BOOTSTRAP`] to build the `request`/`get`/`createServer`/`Agent` surface over it.
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    ctx: &NodeCtx,
    mut gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    // Force node:tls so `globalThis.__treaty_tls` exists before the https bootstrap runs.
    let _tls = crate::node::tls::install(agent, ctx, gc.reborrow())?;
    super::run_module_bootstrap(
        agent,
        gc,
        NATIVES_KEY,
        build_natives,
        HTTPS_BOOTSTRAP,
        "node:https",
    )
}

/// The hidden global slot the bootstrap reads its (empty) natives off.
const NATIVES_KEY: &str = "__treaty_https_natives";

/// The `node:https` JS object model, layered on the `__treaty_tls` transport global.
const HTTPS_BOOTSTRAP: &str = r#"
(function () {
  var TLS = globalThis.__treaty_tls;
  if (!TLS) throw new Error("node:https requires node:tls transport (not initialized)");

  if (!globalThis.__treaty_io) {
    var io = { drivers: [], armed: false };
    function tick() {
      io.armed = false;
      var live = [];
      for (var i = 0; i < io.drivers.length; i++) {
        var d = io.drivers[i], keep = true;
        try { keep = d(); } catch (e) { keep = false; }
        if (keep) live.push(d);
      }
      io.drivers = live;
      if (io.drivers.length > 0) arm();
    }
    function arm() { if (io.armed) return; io.armed = true; Promise.resolve().then(tick); }
    io.add = function (driver) { io.drivers.push(driver); arm(); };
    globalThis.__treaty_io = io;
  }
  var IO = globalThis.__treaty_io;

  function makeClientResponse(resp) {
    var inc = {
      statusCode: resp.status,
      statusMessage: resp.statusText,
      httpVersion: "1.1",
      headers: {},
      rawHeaders: [],
      _listeners: {},
      on: function (ev, fn) { (this._listeners[ev] = this._listeners[ev] || []).push(fn); return this; },
      once: function (ev, fn) { return this.on(ev, fn); },
      setEncoding: function () { return this; },
      _emit: function (ev, a) { var ls = this._listeners[ev]; if (ls) for (var i = 0; i < ls.length; i++) ls[i](a); },
    };
    for (var i = 0; i < resp.headers.length; i++) {
      inc.rawHeaders.push(resp.headers[i][0], resp.headers[i][1]);
      inc.headers[resp.headers[i][0].toLowerCase()] = resp.headers[i][1];
    }
    return inc;
  }

  function normalizeOptions(input, init) {
    var url, method = "GET", headerPairs = [], opts = {};
    if (typeof input === "string") {
      url = input;
      if (init && typeof init === "object") opts = init;
    } else if (input && typeof input === "object") {
      opts = input;
      var host = input.hostname || input.host || "127.0.0.1";
      var port = input.port != null ? input.port : 443;
      var path = input.path || "/";
      url = "https://" + host + ":" + port + path;
      if (input.method) method = String(input.method);
      if (input.headers && typeof input.headers === "object") {
        var keys = Object.keys(input.headers);
        for (var i = 0; i < keys.length; i++) headerPairs.push([keys[i], String(input.headers[keys[i]])]);
      }
    }
    if (init && typeof init === "object" && init.method) method = String(init.method);
    if (init && typeof init === "object") opts = Object.assign({}, opts, init);
    return { url: url, method: method, headerPairs: headerPairs, opts: opts };
  }

  function ClientRequest(o, cb) {
    this._url = o.url;
    this._method = o.method;
    this._headerPairs = o.headerPairs;
    this._opts = o.opts || {};
    this._listeners = {};
    this._body = [];
    if (cb) this.on("response", cb);
  }
  ClientRequest.prototype.on = function (ev, fn) {
    (this._listeners[ev] = this._listeners[ev] || []).push(fn);
    return this;
  };
  ClientRequest.prototype.once = ClientRequest.prototype.on;
  ClientRequest.prototype._emit = function (ev, a) {
    var ls = this._listeners[ev]; if (ls) for (var i = 0; i < ls.length; i++) ls[i](a);
  };
  ClientRequest.prototype.setHeader = function (n, v) { this._headerPairs.push([String(n), String(v)]); return this; };
  ClientRequest.prototype.write = function (c) { if (c != null) this._body.push(String(c)); return true; };
  ClientRequest.prototype.end = function (c) {
    if (c != null) this._body.push(String(c));
    var self = this;
    var trust = TLS.trustOf(this._opts);
    TLS.startClient(this._method, this._url, this._headerPairs, this._body.join(""), trust).then(
      function (resp) {
        var inc = makeClientResponse(resp);
        self._emit("response", inc);
        Promise.resolve().then(function () {
          inc._emit("data", resp.body);
          inc._emit("end");
        });
      },
      function (err) { self._emit("error", err); }
    );
    return this;
  };

  function request(input, init, cb) {
    if (typeof init === "function") { cb = init; init = null; }
    var o = normalizeOptions(input, init);
    return new ClientRequest(o, cb);
  }
  function get(input, init, cb) {
    var req = request(input, init, cb);
    req.end();
    return req;
  }

  // ---- Server (https.createServer): a TLS server with the http req/res handler shape ----------
  function ServerResponse(handle, requestId) {
    this._handle = handle;
    this._requestId = requestId;
    this.statusCode = 200;
    this.statusMessage = "";
    this._headers = [];
    this._chunks = [];
    this._ended = false;
    this.headersSent = false;
  }
  ServerResponse.prototype.setHeader = function (n, v) { this._headers.push([String(n), String(v)]); return this; };
  ServerResponse.prototype.getHeader = function (n) {
    var lc = String(n).toLowerCase();
    for (var i = this._headers.length - 1; i >= 0; i--) if (this._headers[i][0].toLowerCase() === lc) return this._headers[i][1];
    return undefined;
  };
  ServerResponse.prototype.writeHead = function (s, m, h) {
    this.statusCode = s | 0;
    if (typeof m === "string") this.statusMessage = m; else if (m && typeof m === "object") h = m;
    if (h && typeof h === "object") { var ks = Object.keys(h); for (var k = 0; k < ks.length; k++) this.setHeader(ks[k], h[ks[k]]); }
    this.headersSent = true;
    return this;
  };
  ServerResponse.prototype.write = function (c) { if (c != null) this._chunks.push(String(c)); return true; };
  ServerResponse.prototype.end = function (c) {
    if (c != null) this._chunks.push(String(c));
    this._ended = true;
    TLS.nativeServer.respond(this._handle, this._requestId, {
      status: this.statusCode, statusText: this.statusMessage,
      headers: this._headers, body: this._chunks.join(""),
    });
    return this;
  };

  function IncomingMessage(parsed) {
    this.method = parsed.method;
    this.url = parsed.url;
    this.httpVersion = "1.1";
    this.headers = {};
    this.rawHeaders = [];
    for (var i = 0; i < parsed.headers.length; i++) {
      this.rawHeaders.push(parsed.headers[i][0], parsed.headers[i][1]);
      this.headers[parsed.headers[i][0].toLowerCase()] = parsed.headers[i][1];
    }
    this.body = parsed.body;
  }

  function Server(opts, handler) {
    this._opts = opts || {};
    this._handler = handler || function () {};
    this._handle = -1;
    this._listening = false;
  }
  Server.prototype.listen = function () {
    var args = Array.prototype.slice.call(arguments);
    var port = 0, cb = null;
    for (var i = 0; i < args.length; i++) {
      if (typeof args[i] === "number") port = args[i] | 0;
      else if (typeof args[i] === "function") cb = args[i];
    }
    var tc = TLS.testCert();
    var cert = this._opts.cert != null ? String(this._opts.cert) : tc.cert;
    var key = this._opts.key != null ? String(this._opts.key) : tc.key;
    var res = TLS.nativeServer.listen(port, cert, key);
    this._handle = res.handle;
    this._port = res.port;
    this._listening = true;
    var self = this;
    IO.add(function () {
      if (!self._listening) return false;
      var pending = TLS.nativeServer.takePending(self._handle);
      for (var j = 0; j < pending.length; j++) {
        (function (p) {
          var req = new IncomingMessage(p.request);
          var resObj = new ServerResponse(self._handle, p.requestId);
          try { self._handler(req, resObj); }
          catch (e) { if (!resObj._ended) { resObj.statusCode = 500; resObj.end(""); } }
        })(pending[j]);
      }
      return self._listening;
    });
    if (cb) Promise.resolve().then(function () { cb(); });
    return this;
  };
  Server.prototype.address = function () {
    return this._listening ? { address: "127.0.0.1", family: "IPv4", port: this._port } : null;
  };
  Server.prototype.close = function (cb) {
    if (this._listening) { this._listening = false; TLS.nativeServer.close(this._handle); }
    if (cb) Promise.resolve().then(function () { cb(); });
    return this;
  };

  function createServer(opts, handler) {
    if (typeof opts === "function") { handler = opts; opts = {}; }
    return new Server(opts, handler);
  }

  function Agent(opts) { this.options = opts || {}; }

  return {
    request: request,
    get: get,
    createServer: createServer,
    Server: Server,
    ServerResponse: ServerResponse,
    IncomingMessage: IncomingMessage,
    ClientRequest: ClientRequest,
    Agent: Agent,
    globalAgent: new Agent({}),
  };
})()
"#;

#[cfg(test)]
mod tests {
    use crate::JsRuntime;
    use serde_json::{Value as JsonValue, json};

    /// Run a scenario that stashes its result on `globalThis.__out`, then read it back after the
    /// event loop drains (the reactor's background TLS threads do the blocking handshake + I/O).
    fn run_scenario(body: &str) -> JsonValue {
        let mut rt = JsRuntime::with_node_compat();
        rt.eval(&format!("globalThis.__out = null;\n{body}\n0"))
            .expect("scenario schedules");
        rt.eval("globalThis.__out").expect("result reads back")
    }

    #[test]
    fn https_get_against_a_js_tls_server_returns_the_body() {
        // A real https.createServer (the embedded self-signed loopback cert) on 127.0.0.1:0, hit by
        // https.get with that exact cert pinned via the `ca` option, exchanging a request over a
        // genuine TLS session and verifying the chain + SAN name `localhost`. No external network.
        let out = run_scenario(
            r#"
            const https = require('node:https');
            const tls = require('node:tls');
            const ca = tls.testCert().cert; // pin the embedded cert the default server presents
            const server = https.createServer(function (req, res) {
              res.writeHead(201, { 'content-type': 'text/plain' });
              res.end('secure:' + req.url);
            });
            server.listen(0, function () {
              const port = server.address().port;
              https.get({ host: 'localhost', port: port, path: '/abc', ca: ca, rejectUnauthorized: true }, function (res) {
                let buf = '';
                res.on('data', function (d) { buf += d; });
                res.on('end', function () {
                  globalThis.__out = { status: res.statusCode, ct: res.headers['content-type'], body: buf };
                  server.close();
                });
              }).on('error', function (e) { globalThis.__out = { err: String(e && e.message) }; server.close(); });
            });
            "#,
        );
        assert_eq!(
            out,
            json!({ "status": 201, "ct": "text/plain", "body": "secure:/abc" })
        );
    }

    #[test]
    fn https_get_rejects_an_unpinned_self_signed_cert() {
        // The same server, but the client does NOT pin the cert and keeps verification on
        // (rejectUnauthorized defaults to true => empty system roots). The handshake/verification must
        // fail and the request must emit a catchable `error`, never silently trust the cert.
        let out = run_scenario(
            r#"
            const https = require('node:https');
            const server = https.createServer(function (req, res) { res.end('nope'); });
            server.listen(0, function () {
              const port = server.address().port;
              https.get({ host: 'localhost', port: port, path: '/x' }, function (res) {
                globalThis.__out = { unexpectedlyResolved: res.statusCode };
                server.close();
              }).on('error', function (e) {
                globalThis.__out = { errored: true };
                server.close();
              });
            });
            "#,
        );
        assert_eq!(out, json!({ "errored": true }));
    }

    #[test]
    fn https_request_post_sends_a_body_over_tls() {
        // POST a body over TLS to the loopback server, which echoes it back, proving request-body
        // framing survives the encrypted exchange.
        let out = run_scenario(
            r#"
            const https = require('node:https');
            const tls = require('node:tls');
            const ca = tls.testCert().cert;
            const server = https.createServer(function (req, res) {
              res.writeHead(200);
              res.end('echo:' + req.body);
            });
            server.listen(0, function () {
              const port = server.address().port;
              const req = https.request({
                host: 'localhost', port: port, path: '/p', method: 'POST',
                ca: ca, rejectUnauthorized: true,
                headers: { 'content-type': 'text/plain' },
              }, function (res) {
                let buf = '';
                res.on('data', function (d) { buf += d; });
                res.on('end', function () { globalThis.__out = { status: res.statusCode, body: buf }; server.close(); });
              });
              req.on('error', function (e) { globalThis.__out = { err: String(e && e.message) }; server.close(); });
              req.write('payload-123');
              req.end();
            });
            "#,
        );
        assert_eq!(out, json!({ "status": 200, "body": "echo:payload-123" }));
    }
}
