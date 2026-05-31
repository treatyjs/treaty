//! `node:http` — the HTTP/1.1 client and server (`createServer`, `request`, `get`, `Server`,
//! `ServerResponse`, `IncomingMessage`) over `std::net`, plus the wire codec the global `fetch`
//! reuses.
//!
//! ## Layering
//!
//! This module owns the **HTTP/1.1 wire format** ([`ParsedRequest`], [`RawResponse`],
//! [`read_request`], [`write_response`], [`client_roundtrip`], [`parse_response`]) — all pure, no
//! Nova — and the **JS object model** (`createServer().listen()`, `request`/`get`, the `req`/`res`
//! shapes) built as a small bootstrap on top of the native reactor in [`crate::node::net`]. The
//! global `fetch` (wired in `globals.rs`) performs a real `http://` request through the same native
//! client this module exposes.
//!
//! Plaintext `http://` only; `https://`/TLS is a documented follow-up (see `node:https`). All socket
//! I/O runs on background threads via the reactor (see `net.rs`), so the single JS thread never
//! blocks; the JS bootstrap pumps the reactor through the event loop while any request is in flight.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use nova_vm::ecmascript::{
    Agent, Array, ArgumentsList, ExceptionType, InternalMethods, JsResult, Object, OrdinaryObject,
    PropertyDescriptor, PropertyKey, String as JsString, Value, parse_script, script_evaluation,
    unwrap_try,
};
use nova_vm::engine::Bindable;

use crate::node::core::{InstallError, NodeCtx};
use crate::node::globals::define_fn;
use crate::node::net;
use crate::node::{GcScope, NodeModule};

// =================================================================================================
// Pure HTTP/1.1 wire codec (no Nova; unit-tested directly).
// =================================================================================================

/// A request as it crosses the wire / the reactor boundary: method, path, headers, body.
///
/// Header names are kept verbatim (HTTP names are case-insensitive; lookups go through
/// [`ParsedRequest::header`]). The body is raw bytes (decoded text for the common case is the
/// caller's concern).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedRequest {
    /// The request method, upper-cased on the wire (`GET`, `POST`, …).
    pub(crate) method: String,
    /// The request target (path + optional query), e.g. `/api?x=1`.
    pub(crate) path: String,
    /// Header name/value pairs in arrival order.
    pub(crate) headers: Vec<(String, String)>,
    /// The request body bytes (empty for a body-less request).
    pub(crate) body: Vec<u8>,
}

impl ParsedRequest {
    /// A body-less `GET path` request with no extra headers (the reactor adds `Host`/`Connection`).
    pub(crate) fn get(path: &str) -> Self {
        Self {
            method: "GET".to_owned(),
            path: path.to_owned(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// Case-insensitive header lookup.
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// A response as it crosses the wire / the reactor boundary: status, reason, headers, body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawResponse {
    /// The numeric status code (e.g. `200`).
    pub(crate) status: u16,
    /// The reason phrase (e.g. `OK`); may be empty.
    pub(crate) status_text: String,
    /// Response header name/value pairs in emission order.
    pub(crate) headers: Vec<(String, String)>,
    /// The response body bytes.
    pub(crate) body: Vec<u8>,
}

impl RawResponse {
    /// A `text/plain` response with the given status, reason, and UTF-8 body. The `Content-Type` and
    /// `Content-Length` headers are filled in by [`write_response`]/the encoder if absent.
    pub(crate) fn text(status: u16, status_text: &str, body: &str) -> Self {
        Self {
            status,
            status_text: status_text.to_owned(),
            headers: vec![("content-type".to_owned(), "text/plain;charset=utf-8".to_owned())],
            body: body.as_bytes().to_vec(),
        }
    }

    /// Case-insensitive header lookup.
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// The default reason phrase for a status code (a small, common subset; unknown codes get `""`).
fn default_reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "",
    }
}

/// Read and parse one HTTP/1.1 request from `stream` (server side).
///
/// Reads the request line and headers, then — honoring `Content-Length` — the body. `Transfer-
/// Encoding: chunked` request bodies are a documented follow-up (the test/loopback path and `fetch`
/// use `Content-Length`); a chunked request currently yields an empty body rather than erroring.
pub(crate) fn read_request(stream: &mut TcpStream) -> std::io::Result<ParsedRequest> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut reader = BufReader::new(stream.try_clone()?);

    let request_line = read_line(&mut reader)?;
    let mut parts = request_line.trim_end().splitn(3, ' ');
    let method = parts.next().unwrap_or("GET").to_owned();
    let path = parts.next().unwrap_or("/").to_owned();
    // The HTTP version (parts.next()) is accepted but not retained — we always speak HTTP/1.1 back.

    let headers = read_headers(&mut reader)?;
    let content_length = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        .unwrap_or(0);

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body)?;
    }

    Ok(ParsedRequest {
        method,
        path,
        headers,
        body,
    })
}

/// Serialize and write a response to `stream` (server side), HTTP/1.1, connection-close.
///
/// Fills a reason phrase from [`default_reason`] when none was set, always emits `Content-Length`
/// (computed from the body, overriding any caller-supplied value to keep the framing honest) and
/// `Connection: close`, and writes any other caller headers verbatim.
pub(crate) fn write_response(stream: &mut TcpStream, response: &RawResponse) -> std::io::Result<()> {
    let bytes = encode_response(response);
    stream.write_all(&bytes)
}

/// Encode a [`RawResponse`] to its HTTP/1.1 wire bytes (pure; the I/O-free half of
/// [`write_response`], unit-tested directly).
fn encode_response(response: &RawResponse) -> Vec<u8> {
    let reason = if response.status_text.is_empty() {
        default_reason(response.status)
    } else {
        response.status_text.as_str()
    };
    let mut head = format!("HTTP/1.1 {} {}\r\n", response.status, reason);
    for (name, value) in &response.headers {
        // Content-Length and Connection are framing-owned; skip caller copies and emit our own below.
        if name.eq_ignore_ascii_case("content-length") || name.eq_ignore_ascii_case("connection") {
            continue;
        }
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str(&format!("Content-Length: {}\r\n", response.body.len()));
    head.push_str("Connection: close\r\n\r\n");

    let mut out = head.into_bytes();
    out.extend_from_slice(&response.body);
    out
}

/// Perform one blocking client request to `host:port` and read the full response (client side).
///
/// Opens a `TcpStream`, writes the request (adding `Host`, `Connection: close`, and a
/// `Content-Length` when there is a body), and reads the response with [`parse_response`]. Runs on a
/// background thread (see [`crate::node::net::client_start`]), so the blocking is off the JS thread.
pub(crate) fn client_roundtrip(
    host: &str,
    port: u16,
    req: &ParsedRequest,
) -> Result<RawResponse, String> {
    let mut stream =
        TcpStream::connect((host, port)).map_err(|e| format!("connect {host}:{port}: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|e| e.to_string())?;

    let bytes = encode_request(host, port, req);
    stream.write_all(&bytes).map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())?;

    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    parse_response(&buf)
}

/// Encode a [`ParsedRequest`] to its HTTP/1.1 wire bytes for the client side (pure; unit-tested).
///
/// Adds `Host` (when the caller omitted it), `Connection: close`, and `Content-Length` for a
/// non-empty body, preserving any other caller headers. The port is included in `Host` only when it
/// is not the default `80`, matching common client behavior.
fn encode_request(host: &str, port: u16, req: &ParsedRequest) -> Vec<u8> {
    let mut head = format!("{} {} HTTP/1.1\r\n", req.method, req.path);

    let has_host = req.header("host").is_some();
    if !has_host {
        if port == 80 {
            head.push_str(&format!("Host: {host}\r\n"));
        } else {
            head.push_str(&format!("Host: {host}:{port}\r\n"));
        }
    }
    for (name, value) in &req.headers {
        if name.eq_ignore_ascii_case("connection") || name.eq_ignore_ascii_case("content-length") {
            continue;
        }
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    if !req.body.is_empty() {
        head.push_str(&format!("Content-Length: {}\r\n", req.body.len()));
    }
    head.push_str("Connection: close\r\n\r\n");

    let mut out = head.into_bytes();
    out.extend_from_slice(&req.body);
    out
}

/// Parse a full HTTP/1.1 response from raw bytes (pure; the I/O-free half of the client, unit-tested).
///
/// Splits the status line, headers, and body. The body is everything after the blank line; when a
/// `Content-Length` is present it bounds the body, otherwise the remainder (connection-close framing)
/// is used. `Transfer-Encoding: chunked` responses are a documented follow-up.
pub(crate) fn parse_response(buf: &[u8]) -> Result<RawResponse, String> {
    let split = find_header_end(buf).ok_or_else(|| "malformed response: no header terminator".to_owned())?;
    let head = std::str::from_utf8(&buf[..split])
        .map_err(|_| "malformed response: non-UTF-8 headers".to_owned())?;
    let body_start = split + 4; // past "\r\n\r\n"

    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let mut sp = status_line.splitn(3, ' ');
    let _version = sp.next().unwrap_or("");
    let status = sp
        .next()
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| format!("malformed status line: {status_line:?}"))?;
    let status_text = sp.next().unwrap_or("").to_owned();

    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_owned(), value.trim().to_owned()));
        }
    }

    let content_length = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok());
    let body = match content_length {
        Some(len) => {
            let end = (body_start + len).min(buf.len());
            buf[body_start..end].to_vec()
        }
        None => buf[body_start.min(buf.len())..].to_vec(),
    };

    Ok(RawResponse {
        status,
        status_text,
        headers,
        body,
    })
}

/// Find the byte index of the `\r\n\r\n` that terminates the header block, if present.
fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Read a single CRLF-terminated line (LF-tolerant) from `reader`, returning it without the line
/// terminator's `\r`.
fn read_line<R: BufRead>(reader: &mut R) -> std::io::Result<String> {
    let mut line = Vec::new();
    reader.read_until(b'\n', &mut line)?;
    while matches!(line.last(), Some(b'\n') | Some(b'\r')) {
        line.pop();
    }
    Ok(String::from_utf8_lossy(&line).into_owned())
}

/// Read header lines until the blank line, returning `(name, value)` pairs (names verbatim, values
/// trimmed).
fn read_headers<R: BufRead>(reader: &mut R) -> std::io::Result<Vec<(String, String)>> {
    let mut headers = Vec::new();
    loop {
        let line = read_line(reader)?;
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_owned(), value.trim().to_owned()));
        }
    }
    Ok(headers)
}

// =================================================================================================
// JS-facing wiring for `node:http`.
// =================================================================================================

/// Zero-sized marker for the `node:http` builtin.
pub(crate) struct HttpModule;

impl NodeModule for HttpModule {
    const SPECIFIER: &'static str = "http";

    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: GcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError> {
        install(agent, ctx, gc)
    }
}

/// The hidden global slot the bootstrap reads the native primitives off, parked just before the
/// bootstrap evaluates and deleted immediately after (so it never lingers as an observable global).
const NATIVES_KEY: &str = "__treaty_http_natives";

/// Build the native primitives object the `node:http` bootstrap is layered on.
///
/// These are the reactor seam exposed to JS: server lifecycle (`serverListen`/`serverPort`/
/// `serverTakePending`/`serverRespond`/`serverClose`), client lifecycle (`clientStart`/`clientPoll`),
/// and the URL split (`splitUrl`) the client and `fetch` share. Every blocking socket op lives behind
/// these on a background thread; the functions themselves never block.
pub(crate) fn build_natives<'gc>(agent: &mut Agent, gc: GcScope<'gc, '_>) -> OrdinaryObject<'gc> {
    let gc = gc.into_nogc();
    let natives = OrdinaryObject::create_empty_object(agent, gc);
    define_fn(agent, natives, "serverListen", js::server_listen, 1, gc);
    define_fn(agent, natives, "serverPort", js::server_port, 1, gc);
    define_fn(agent, natives, "serverTakePending", js::server_take_pending, 1, gc);
    define_fn(agent, natives, "serverRespond", js::server_respond, 3, gc);
    define_fn(agent, natives, "serverClose", js::server_close, 1, gc);
    define_fn(agent, natives, "clientStart", js::client_start, 4, gc);
    define_fn(agent, natives, "clientPoll", js::client_poll, 1, gc);
    define_fn(agent, natives, "splitUrl", js::split_url, 1, gc);
    natives
}

/// Uniform per-module entry. Returns the `node:http` exports object.
///
/// Built by parking [`build_natives`] on a hidden global slot and evaluating [`HTTP_BOOTSTRAP`],
/// which builds the `createServer`/`request`/`get` surface and the `req`/`res`/`ClientRequest` shapes
/// on top of those primitives plus the realm's `EventEmitter`-free promise/timer machinery. The slot
/// is removed before returning so it never leaks to user code.
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    mut gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    // (1) Build + stash the natives on the hidden slot.
    let natives = build_natives(agent, gc.reborrow()).unbind();
    {
        let nogc = gc.nogc();
        let global = agent.current_realm(nogc).global_object(agent);
        let key = PropertyKey::from_static_str(agent, NATIVES_KEY, nogc);
        let defined = global.unbind().try_define_own_property(
            agent,
            key.unbind(),
            PropertyDescriptor::new_data_descriptor(natives.bind(nogc)),
            None,
            nogc,
        );
        if defined.is_break() {
            return Err(InstallError::Nova(
                "could not stash http natives on the global".to_owned(),
            ));
        }
    }

    // (2) Evaluate the bootstrap; its completion value is the exports object.
    let exports = run_bootstrap(agent, gc.reborrow())?.unbind();

    // (3) Delete the hidden slot.
    {
        let nogc = gc.nogc();
        let global = agent.current_realm(nogc).global_object(agent);
        let key = PropertyKey::from_static_str(agent, NATIVES_KEY, nogc);
        let _ = global.unbind().try_delete(agent, key.unbind(), nogc);
    }

    Ok(exports.bind(gc.into_nogc()))
}

/// Parse + evaluate [`HTTP_BOOTSTRAP`] in the current realm, returning its exports object.
fn run_bootstrap<'gc>(
    agent: &mut Agent,
    mut gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    let source = JsString::from_static_str(agent, HTTP_BOOTSTRAP, gc.nogc());
    let realm = agent.current_realm(gc.nogc());
    let script = parse_script(agent, source.unbind(), realm.unbind(), true, None, gc.nogc())
        .map_err(|diags| {
            let msg = diags
                .iter()
                .map(|d| d.to_string())
                .collect::<Vec<_>>()
                .join("; ");
            InstallError::Nova(format!("node:http bootstrap parse error: {msg}"))
        })?;
    let value = script_evaluation(agent, script.unbind(), gc.reborrow())
        .unbind()
        .bind(gc.nogc());
    let value = match value {
        Ok(v) => v.unbind(),
        Err(err) => {
            let msg = err
                .value()
                .unbind()
                .string_repr(agent, gc.reborrow())
                .to_string_lossy(agent)
                .into_owned();
            return Err(InstallError::Nova(format!("node:http bootstrap error: {msg}")));
        }
    };
    let gc = gc.into_nogc();
    Object::try_from(value.bind(gc))
        .map_err(|_| InstallError::Nova("node:http bootstrap did not return an object".to_owned()))
}

/// The `node:http` JS object model, layered on the native reactor primitives.
///
/// Builds:
/// * `createServer(handler)` → a `Server` whose `listen(port[, cb])` opens a native listener and
///   starts pumping the reactor (via the shared `__treaty_io_pump`), running `handler(req, res)` for
///   each parked request; `res.writeHead`/`res.setHeader`/`res.write`/`res.end` accumulate a
///   response that is handed back to the native side; `address()` reports the bound port; `close()`
///   stops the server.
/// * `request(opts[, cb])` / `get(...)` → a `ClientRequest` that starts a native background request
///   on `end()` and, once it completes, emits a minimal `IncomingMessage`-shaped response object to
///   the callback with `statusCode`/`headers` and a settled-text body via `on('data')`/`on('end')`.
/// * `fetchHttp(method, url, headerPairs, body)` → the promise the global `fetch` uses for an
///   `http://` URL: starts a native client and resolves with `{ status, statusText, headers, body }`.
///
/// The reactor pump (`__treaty_io_pump`) is installed on `globalThis` so `fetch` (in `globals.rs`)
/// shares the exact same drive loop: a microtask that services every listening server and resolves
/// every settled client, re-arming itself only while work is outstanding so the event loop still
/// drains to idle.
const HTTP_BOOTSTRAP: &str = r#"
(function () {
  var N = globalThis.__treaty_http_natives;

  // ---- Shared reactor pump (installed once on globalThis; fetch reuses it) -------------------
  //
  // A set of "drivers": each is a function returning true while it still has outstanding work.
  // The pump runs every driver on each microtask tick and re-arms via Promise.resolve().then while
  // ANY driver still reports work, so the event loop keeps turning until all requests settle, then
  // goes idle (no self-perpetuation once work is done).
  if (!globalThis.__treaty_io) {
    var io = { drivers: [], armed: false };
    function tick() {
      io.armed = false;
      var live = [];
      for (var i = 0; i < io.drivers.length; i++) {
        var d = io.drivers[i];
        var keep = true;
        try { keep = d(); } catch (e) { keep = false; }
        if (keep) live.push(d);
      }
      io.drivers = live;
      if (io.drivers.length > 0) arm();
    }
    function arm() {
      if (io.armed) return;
      io.armed = true;
      Promise.resolve().then(tick);
    }
    io.add = function (driver) { io.drivers.push(driver); arm(); };
    globalThis.__treaty_io = io;
  }
  var IO = globalThis.__treaty_io;

  // ---- Server -------------------------------------------------------------------------------

  function ServerResponse() {
    this.statusCode = 200;
    this.statusMessage = "";
    this._headers = [];
    this._chunks = [];
    this._ended = false;
    this.headersSent = false;
  }
  ServerResponse.prototype.setHeader = function (name, value) {
    this._headers.push([String(name), String(value)]);
    return this;
  };
  ServerResponse.prototype.getHeader = function (name) {
    var lc = String(name).toLowerCase();
    for (var i = this._headers.length - 1; i >= 0; i--) {
      if (this._headers[i][0].toLowerCase() === lc) return this._headers[i][1];
    }
    return undefined;
  };
  ServerResponse.prototype.writeHead = function (status, statusMessage, headers) {
    this.statusCode = status | 0;
    if (typeof statusMessage === "string") { this.statusMessage = statusMessage; }
    else if (statusMessage && typeof statusMessage === "object") { headers = statusMessage; }
    if (headers && typeof headers === "object") {
      var keys = Object.keys(headers);
      for (var i = 0; i < keys.length; i++) this.setHeader(keys[i], headers[keys[i]]);
    }
    this.headersSent = true;
    return this;
  };
  ServerResponse.prototype.write = function (chunk) {
    if (chunk != null) this._chunks.push(String(chunk));
    return true;
  };
  ServerResponse.prototype.end = function (chunk) {
    if (chunk != null) this._chunks.push(String(chunk));
    this._ended = true;
    if (typeof this._flush === "function") this._flush();
    return this;
  };

  function IncomingMessage(parsed) {
    this.method = parsed.method;
    this.url = parsed.url;
    this.httpVersion = "1.1";
    this.headers = {};
    this.rawHeaders = [];
    for (var i = 0; i < parsed.headers.length; i++) {
      var n = parsed.headers[i][0], v = parsed.headers[i][1];
      this.rawHeaders.push(n, v);
      this.headers[n.toLowerCase()] = v;
    }
    this.body = parsed.body;
  }

  function Server(handler) {
    this._handler = handler;
    this._handle = -1;
    this._listening = false;
    this._closeCbs = [];
  }
  Server.prototype.listen = function () {
    var args = Array.prototype.slice.call(arguments);
    var port = 0, cb = null;
    for (var i = 0; i < args.length; i++) {
      if (typeof args[i] === "number") port = args[i] | 0;
      else if (typeof args[i] === "function") cb = args[i];
    }
    var res = N.serverListen(port);
    this._handle = res.handle;
    this._port = res.port;
    this._listening = true;
    var self = this;
    // Drive: service every parked request through the handler, return true while still listening.
    IO.add(function () {
      if (!self._listening) return false;
      var pending = N.serverTakePending(self._handle);
      for (var j = 0; j < pending.length; j++) {
        (function (p) {
          var req = new IncomingMessage(p.request);
          var resObj = new ServerResponse();
          resObj._flush = function () {
            var body = resObj._chunks.join("");
            N.serverRespond(self._handle, p.requestId, {
              status: resObj.statusCode,
              statusText: resObj.statusMessage,
              headers: resObj._headers,
              body: body,
            });
          };
          try { self._handler(req, resObj); }
          catch (e) {
            if (!resObj._ended) { resObj.statusCode = 500; resObj.end(""); }
          }
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
    if (this._listening) { this._listening = false; N.serverClose(this._handle); }
    if (cb) Promise.resolve().then(function () { cb(); });
    return this;
  };

  function createServer(opt, handler) {
    if (typeof opt === "function") { handler = opt; }
    return new Server(handler || function () {});
  }

  // ---- Client -------------------------------------------------------------------------------

  // Start a native client and return a promise of { status, statusText, headers, body }.
  function startClient(method, url, headerPairs, body) {
    return new Promise(function (resolve, reject) {
      var handle;
      try { handle = N.clientStart(String(method), String(url), headerPairs || [], body == null ? "" : String(body)); }
      catch (e) { reject(e); return; }
      IO.add(function () {
        var r = N.clientPoll(handle);
        if (r.pending) return true;
        if (r.error != null) { reject(new Error(String(r.error))); return false; }
        resolve(r.response);
        return false;
      });
    });
  }

  // The promise the global fetch uses for http:// URLs.
  function fetchHttp(method, url, headerPairs, body) {
    return startClient(method, url, headerPairs, body);
  }

  // ClientRequest: a minimal EventEmitter-ish object with on()/end() supporting http.get/request.
  function ClientRequest(method, url, headerPairs, cb) {
    this._method = method;
    this._url = url;
    this._headerPairs = headerPairs;
    this._listeners = {};
    this._body = [];
    if (cb) this.on("response", cb);
  }
  ClientRequest.prototype.on = function (event, fn) {
    (this._listeners[event] = this._listeners[event] || []).push(fn);
    return this;
  };
  ClientRequest.prototype._emit = function (event, arg) {
    var ls = this._listeners[event];
    if (ls) for (var i = 0; i < ls.length; i++) ls[i](arg);
  };
  ClientRequest.prototype.setHeader = function (n, v) { this._headerPairs.push([String(n), String(v)]); return this; };
  ClientRequest.prototype.write = function (chunk) { if (chunk != null) this._body.push(String(chunk)); return true; };
  ClientRequest.prototype.end = function (chunk) {
    if (chunk != null) this._body.push(String(chunk));
    var self = this;
    startClient(this._method, this._url, this._headerPairs, this._body.join("")).then(function (resp) {
      var inc = makeClientResponse(resp);
      self._emit("response", inc);
      // Deliver the body on the next microtask so a handler can attach data/end listeners first.
      Promise.resolve().then(function () {
        inc._emit("data", resp.body);
        inc._emit("end");
      });
    }, function (err) { self._emit("error", err); });
    return this;
  };

  function makeClientResponse(resp) {
    var inc = {
      statusCode: resp.status,
      statusMessage: resp.statusText,
      headers: {},
      _listeners: {},
      on: function (event, fn) { (this._listeners[event] = this._listeners[event] || []).push(fn); return this; },
      setEncoding: function () { return this; },
      _emit: function (event, arg) { var ls = this._listeners[event]; if (ls) for (var i = 0; i < ls.length; i++) ls[i](arg); },
    };
    for (var i = 0; i < resp.headers.length; i++) inc.headers[resp.headers[i][0].toLowerCase()] = resp.headers[i][1];
    return inc;
  }

  function normalizeOptions(input, init) {
    // Accept a URL string, or an options object ({ host/hostname, port, path, method, headers }).
    var url, method = "GET", headerPairs = [];
    if (typeof input === "string") {
      url = input;
    } else if (input && typeof input === "object") {
      var host = input.hostname || input.host || "127.0.0.1";
      var port = input.port != null ? input.port : 80;
      var path = input.path || "/";
      url = "http://" + host + ":" + port + path;
      if (input.method) method = String(input.method);
      if (input.headers && typeof input.headers === "object") {
        var keys = Object.keys(input.headers);
        for (var i = 0; i < keys.length; i++) headerPairs.push([keys[i], String(input.headers[keys[i]])]);
      }
    }
    if (init && typeof init === "object" && init.method) method = String(init.method);
    return { url: url, method: method, headerPairs: headerPairs };
  }

  function request(input, init, cb) {
    if (typeof init === "function") { cb = init; init = null; }
    var o = normalizeOptions(input, init);
    return new ClientRequest(o.method, o.url, o.headerPairs, cb);
  }
  function get(input, init, cb) {
    var req = request(input, init, cb);
    req.end();
    return req;
  }

  var STATUS_CODES = {
    "200": "OK", "201": "Created", "204": "No Content", "301": "Moved Permanently",
    "302": "Found", "304": "Not Modified", "400": "Bad Request", "401": "Unauthorized",
    "403": "Forbidden", "404": "Not Found", "405": "Method Not Allowed",
    "500": "Internal Server Error", "502": "Bad Gateway", "503": "Service Unavailable"
  };
  var METHODS = ["DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT"];

  // Publish the http client behind a stable global slot so the fetch global (globals.rs) can route
  // an http:// request through this exact transport without re-importing node:http.
  globalThis.__treaty_fetch_http = fetchHttp;

  return {
    createServer: createServer,
    Server: Server,
    ServerResponse: ServerResponse,
    IncomingMessage: IncomingMessage,
    ClientRequest: ClientRequest,
    request: request,
    get: get,
    STATUS_CODES: STATUS_CODES,
    METHODS: METHODS,
    globalAgent: {},
  };
})()
"#;

/// JS wrappers marshalling between the reactor ([`crate::node::net`]) and the bootstrap.
mod js {
    use super::*;

    /// Read argument `idx` as an owned string, or `""` when it is not a JS string.
    fn arg_str(agent: &Agent, args: &ArgumentsList, idx: usize) -> String {
        match JsString::try_from(args.get(idx)) {
            Ok(s) => s.to_string_lossy(agent).into_owned(),
            Err(_) => String::new(),
        }
    }

    /// Read argument `idx` as a `u32`, or `0` when it is not an integer value.
    fn arg_u32(args: &ArgumentsList, idx: usize) -> u32 {
        match args.get(idx) {
            Value::Integer(i) => u32::try_from(i.into_i64()).unwrap_or(0),
            _ => 0,
        }
    }

    /// Define a string property on an object.
    fn set_str(agent: &mut Agent, obj: OrdinaryObject, key: &'static str, value: &str, gc: nova_vm::engine::NoGcScope) {
        let v: Value = JsString::from_str(agent, value, gc).into();
        let k = PropertyKey::from_static_str(agent, key, gc);
        unwrap_try(obj.try_define_own_property(agent, k, PropertyDescriptor::new_data_descriptor(v), None, gc));
    }

    /// Define an arbitrary `Value` property on an object.
    fn set_val(agent: &mut Agent, obj: OrdinaryObject, key: &'static str, value: Value, gc: nova_vm::engine::NoGcScope) {
        let k = PropertyKey::from_static_str(agent, key, gc);
        unwrap_try(obj.try_define_own_property(agent, k, PropertyDescriptor::new_data_descriptor(value), None, gc));
    }

    /// Build a `[name, value][]` JS array from header tuples.
    fn header_pairs_to_array<'gc>(
        agent: &mut Agent,
        headers: &[(String, String)],
        gc: nova_vm::engine::NoGcScope<'gc, '_>,
    ) -> Array<'gc> {
        let pairs: Vec<Value> = headers
            .iter()
            .map(|(n, v)| {
                let name: Value = JsString::from_str(agent, n, gc).into();
                let value: Value = JsString::from_str(agent, v, gc).into();
                Array::from_slice(agent, &[name, value], gc).into()
            })
            .collect();
        Array::from_slice(agent, &pairs, gc)
    }

    /// Read a JS `[name, value][]` header array into Rust tuples (defensive: skips non-pair items).
    fn read_header_pairs(agent: &mut Agent, value: Value, gc: nova_vm::engine::NoGcScope) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let Ok(array) = Array::try_from(value) else {
            return out;
        };
        let len = array.len(agent);
        for i in 0..len {
            let key = PropertyKey::Integer(i.into());
            let pair = match array.try_get(agent, key, array.into(), None, gc) {
                std::ops::ControlFlow::Continue(nova_vm::ecmascript::TryGetResult::Value(v)) => v,
                _ => continue,
            };
            let Ok(pair) = Array::try_from(pair) else { continue };
            let read = |agent: &mut Agent, idx: u32| -> String {
                let k = PropertyKey::Integer(idx.into());
                match pair.try_get(agent, k, pair.into(), None, gc) {
                    std::ops::ControlFlow::Continue(nova_vm::ecmascript::TryGetResult::Value(v)) => {
                        JsString::try_from(v).map(|s| s.to_string_lossy(agent).into_owned()).unwrap_or_default()
                    }
                    _ => String::new(),
                }
            };
            let name = read(agent, 0);
            let val = read(agent, 1);
            out.push((name, val));
        }
        out
    }

    /// Build the `{ method, path, headers, body }` JS object the server bootstrap reads a request as.
    fn parsed_request_to_js<'gc>(
        agent: &mut Agent,
        req: &ParsedRequest,
        gc: nova_vm::engine::NoGcScope<'gc, '_>,
    ) -> Object<'gc> {
        let obj = OrdinaryObject::create_empty_object(agent, gc);
        set_str(agent, obj, "method", &req.method, gc);
        set_str(agent, obj, "url", &req.path, gc);
        let headers = header_pairs_to_array(agent, &req.headers, gc).into();
        set_val(agent, obj, "headers", headers, gc);
        let body = String::from_utf8_lossy(&req.body);
        set_str(agent, obj, "body", &body, gc);
        obj.into()
    }

    /// `serverListen(port)` -> `{ handle, port }`. Opens a native listener on `127.0.0.1:port`.
    pub(super) fn server_listen<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let port = arg_u32(&args, 0) as u16;
        let gc = gc.into_nogc();
        match net::server_listen(port) {
            Ok((handle, bound)) => {
                let obj = OrdinaryObject::create_empty_object(agent, gc);
                set_val(agent, obj, "handle", Value::Integer((handle as i32).into()), gc);
                set_val(agent, obj, "port", Value::Integer((i32::from(bound)).into()), gc);
                Ok(obj.into())
            }
            Err(e) => Err(agent.throw_exception(
                ExceptionType::Error,
                format!("listen failed: {e}"),
                gc,
            )),
        }
    }

    /// `serverPort(handle)` -> number | null.
    pub(super) fn server_port<'gc>(
        _agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        _gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let handle = arg_u32(&args, 0) as u64;
        Ok(match net::server_port(handle) {
            Some(p) => Value::Integer(i32::from(p).into()),
            None => Value::Null,
        })
    }

    /// `serverTakePending(handle)` -> `[{ requestId, request }]`. Drains parked requests; each is
    /// later answered via [`server_respond`] with its `requestId`. The reactor's one-shot senders are
    /// held in a thread-local keyed by `(handle, requestId)` so `serverRespond` can recover them
    /// without crossing the JS boundary with a non-`Value` payload.
    pub(super) fn server_take_pending<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let handle = arg_u32(&args, 0) as u64;
        let pending = net::server_take_pending(handle);
        let gc = gc.into_nogc();
        let items: Vec<Value> = pending
            .into_iter()
            .map(|p| {
                let request_id = p.request_id;
                let request_js = parsed_request_to_js(agent, &p.request, gc);
                // Park the one-shot responder so `serverRespond(handle, requestId, …)` can find it.
                PENDING.with(|cell| cell.borrow_mut().push(((handle, request_id), p)));
                let obj = OrdinaryObject::create_empty_object(agent, gc);
                set_val(agent, obj, "requestId", Value::Integer((request_id as i32).into()), gc);
                set_val(agent, obj, "request", request_js.into(), gc);
                obj.into()
            })
            .collect();
        Ok(Array::from_slice(agent, &items, gc).into())
    }

    /// `serverRespond(handle, requestId, { status, statusText, headers, body })` — answer a parked
    /// request, unblocking its connection.
    pub(super) fn server_respond<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let handle = arg_u32(&args, 0) as u64;
        let request_id = arg_u32(&args, 1) as u64;
        let gc = gc.into_nogc();

        // Read the response object fields.
        let resp_obj = match Object::try_from(args.get(2)) {
            Ok(o) => o,
            Err(_) => return Ok(Value::Undefined),
        };
        let read_num = |agent: &mut Agent, key: &'static str| -> i64 {
            let k = PropertyKey::from_static_str(agent, key, gc);
            match resp_obj.try_get(agent, k, resp_obj.into(), None, gc) {
                std::ops::ControlFlow::Continue(nova_vm::ecmascript::TryGetResult::Value(Value::Integer(i))) => i.into_i64(),
                _ => 0,
            }
        };
        let read_string = |agent: &mut Agent, key: &'static str| -> String {
            let k = PropertyKey::from_static_str(agent, key, gc);
            match resp_obj.try_get(agent, k, resp_obj.into(), None, gc) {
                std::ops::ControlFlow::Continue(nova_vm::ecmascript::TryGetResult::Value(v)) => {
                    JsString::try_from(v).map(|s| s.to_string_lossy(agent).into_owned()).unwrap_or_default()
                }
                _ => String::new(),
            }
        };
        let status = u16::try_from(read_num(agent, "status")).unwrap_or(200);
        let status_text = read_string(agent, "statusText");
        let body = read_string(agent, "body");
        let headers = {
            let k = PropertyKey::from_static_str(agent, "headers", gc);
            let hv = match resp_obj.try_get(agent, k, resp_obj.into(), None, gc) {
                std::ops::ControlFlow::Continue(nova_vm::ecmascript::TryGetResult::Value(v)) => v,
                _ => Value::Undefined,
            };
            read_header_pairs(agent, hv, gc)
        };

        let response = RawResponse {
            status,
            status_text,
            headers,
            body: body.into_bytes(),
        };

        // Recover the parked one-shot for this (handle, requestId) and answer it.
        let pending = PENDING.with(|cell| {
            let mut v = cell.borrow_mut();
            if let Some(pos) = v.iter().position(|((h, r), _)| *h == handle && *r == request_id) {
                Some(v.remove(pos).1)
            } else {
                None
            }
        });
        if let Some(pending) = pending {
            net::request_respond(pending, response);
        }
        Ok(Value::Undefined)
    }

    /// `serverClose(handle)` — stop a listening server and join its accept thread.
    pub(super) fn server_close<'gc>(
        _agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        _gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let handle = arg_u32(&args, 0) as u64;
        // Drop any still-parked one-shots for this handle (their connections will be released as the
        // accept thread tears down), then close the server.
        PENDING.with(|cell| cell.borrow_mut().retain(|((h, _), _)| *h != handle));
        net::server_close(handle);
        Ok(Value::Undefined)
    }

    /// `clientStart(method, url, headerPairs, body)` -> client handle (number). Splits the URL into
    /// host/port/path and spawns a background request.
    pub(super) fn client_start<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let method = arg_str(agent, &args, 0);
        let url = arg_str(agent, &args, 1);
        let header_value = args.get(2);
        let body = arg_str(agent, &args, 3);
        let gc = gc.into_nogc();

        let headers = read_header_pairs(agent, header_value, gc);
        let parsed_url = match split_http_url(&url) {
            Ok(p) => p,
            Err(e) => {
                return Err(agent.throw_exception(ExceptionType::TypeError, e, gc));
            }
        };

        let req = ParsedRequest {
            method: method.to_ascii_uppercase(),
            path: parsed_url.path,
            headers,
            body: body.into_bytes(),
        };
        let handle = net::client_start(req, parsed_url.host, parsed_url.port);
        Ok(Value::Integer((handle as i32).into()))
    }

    /// `clientPoll(handle)` -> `{ pending } | { error } | { response: { status, statusText, headers, body } }`.
    pub(super) fn client_poll<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let handle = arg_u32(&args, 0) as u64;
        let gc = gc.into_nogc();
        let obj = OrdinaryObject::create_empty_object(agent, gc);
        match net::client_poll(handle) {
            net::ClientPoll::Pending => {
                set_val(agent, obj, "pending", Value::Boolean(true), gc);
            }
            net::ClientPoll::Unknown => {
                set_str(agent, obj, "error", "unknown client handle", gc);
            }
            net::ClientPoll::Done(Err(e)) => {
                set_str(agent, obj, "error", &e, gc);
            }
            net::ClientPoll::Done(Ok(resp)) => {
                let response = OrdinaryObject::create_empty_object(agent, gc);
                set_val(agent, response, "status", Value::Integer(i32::from(resp.status).into()), gc);
                let reason = if resp.status_text.is_empty() {
                    default_reason(resp.status).to_owned()
                } else {
                    resp.status_text.clone()
                };
                set_str(agent, response, "statusText", &reason, gc);
                let headers = header_pairs_to_array(agent, &resp.headers, gc).into();
                set_val(agent, response, "headers", headers, gc);
                let body = String::from_utf8_lossy(&resp.body);
                set_str(agent, response, "body", &body, gc);
                set_val(agent, obj, "response", response.into(), gc);
            }
        }
        Ok(obj.into())
    }

    /// `splitUrl(url)` -> `{ host, port, path }` for an `http://` URL (throws otherwise).
    pub(super) fn split_url<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let url = arg_str(agent, &args, 0);
        let gc = gc.into_nogc();
        match split_http_url(&url) {
            Ok(p) => {
                let obj = OrdinaryObject::create_empty_object(agent, gc);
                set_str(agent, obj, "host", &p.host, gc);
                set_val(agent, obj, "port", Value::Integer(i32::from(p.port).into()), gc);
                set_str(agent, obj, "path", &p.path, gc);
                Ok(obj.into())
            }
            Err(e) => Err(agent.throw_exception(ExceptionType::TypeError, e, gc)),
        }
    }
}

// The parked one-shot responders for accepted-but-unanswered requests, keyed by `(handle,
// request_id)`. Thread-local because all access is on the single JS thread, and a `Vec` because the
// pending set is tiny (a handful of in-flight requests at most). Lets `serverRespond` recover the
// reactor sender that `serverTakePending` handed out without smuggling a non-`Value` across JS.
thread_local! {
    static PENDING: std::cell::RefCell<Vec<((u64, u64), net::PendingRequest)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// A split `http://` URL: host, port (defaulting to 80), and path (defaulting to `/`).
#[derive(Debug)]
pub(crate) struct SplitUrl {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) path: String,
}

/// Split an `http://host[:port][/path][?query]` URL into host/port/path (pure; unit-tested).
///
/// Rejects a non-`http://` scheme (notably `https://`, which has no TLS transport here) and an empty
/// host. The default port is `80`; the default path is `/`. Query and fragment stay attached to the
/// path (the server receives the full request target).
pub(crate) fn split_http_url(url: &str) -> Result<SplitUrl, String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| {
            if url.starts_with("https://") {
                "https:// is not supported in this runtime (no TLS transport)".to_owned()
            } else {
                format!("unsupported URL scheme: {url:?}")
            }
        })?;

    // Authority is up to the first '/' , '?' or '#'.
    let authority_end = rest
        .find(['/', '?', '#'])
        .unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let path = if authority_end < rest.len() {
        let tail = &rest[authority_end..];
        if tail.starts_with('/') {
            tail.to_owned()
        } else {
            format!("/{tail}")
        }
    } else {
        "/".to_owned()
    };

    if authority.is_empty() {
        return Err(format!("URL has no host: {url:?}"));
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => {
            let port = p
                .parse::<u16>()
                .map_err(|_| format!("invalid port in URL: {url:?}"))?;
            (h.to_owned(), port)
        }
        None => (authority.to_owned(), 80),
    };
    if host.is_empty() {
        return Err(format!("URL has no host: {url:?}"));
    }
    Ok(SplitUrl { host, port, path })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_http_url_parses_host_port_path() {
        let u = split_http_url("http://127.0.0.1:8080/api?x=1").unwrap();
        assert_eq!(u.host, "127.0.0.1");
        assert_eq!(u.port, 8080);
        assert_eq!(u.path, "/api?x=1");

        let d = split_http_url("http://example.com/").unwrap();
        assert_eq!(d.host, "example.com");
        assert_eq!(d.port, 80);
        assert_eq!(d.path, "/");

        // No path -> "/".
        let n = split_http_url("http://localhost:3000").unwrap();
        assert_eq!(n.path, "/");
        assert_eq!(n.port, 3000);
    }

    #[test]
    fn split_http_url_rejects_https_and_bad_scheme() {
        assert!(split_http_url("https://x/").unwrap_err().contains("TLS"));
        assert!(split_http_url("ftp://x/").is_err());
        assert!(split_http_url("http:///nohost").is_err());
        assert!(split_http_url("http://host:notaport/").is_err());
    }

    #[test]
    fn encode_request_adds_host_and_framing() {
        let req = ParsedRequest::get("/p");
        let bytes = encode_request("127.0.0.1", 8080, &req);
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with("GET /p HTTP/1.1\r\n"));
        assert!(text.contains("Host: 127.0.0.1:8080\r\n"));
        assert!(text.contains("Connection: close\r\n"));
        // GET with no body carries no Content-Length.
        assert!(!text.contains("Content-Length"));
    }

    #[test]
    fn encode_request_includes_body_length() {
        let req = ParsedRequest {
            method: "POST".to_owned(),
            path: "/submit".to_owned(),
            headers: vec![("Content-Type".to_owned(), "application/json".to_owned())],
            body: b"{\"a\":1}".to_vec(),
        };
        let text = String::from_utf8(encode_request("h", 80, &req)).unwrap();
        assert!(text.starts_with("POST /submit HTTP/1.1\r\n"));
        assert!(text.contains("Host: h\r\n")); // default port omitted
        assert!(text.contains("Content-Type: application/json\r\n"));
        assert!(text.contains("Content-Length: 7\r\n"));
        assert!(text.ends_with("{\"a\":1}"));
    }

    #[test]
    fn encode_response_sets_reason_length_and_close() {
        let resp = RawResponse {
            status: 200,
            status_text: String::new(),
            headers: vec![("X-Test".to_owned(), "1".to_owned())],
            body: b"hello".to_vec(),
        };
        let text = String::from_utf8(encode_response(&resp)).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n")); // reason filled from default
        assert!(text.contains("X-Test: 1\r\n"));
        assert!(text.contains("Content-Length: 5\r\n"));
        assert!(text.contains("Connection: close\r\n"));
        assert!(text.ends_with("\r\n\r\nhello"));
    }

    #[test]
    fn parse_response_reads_status_headers_and_body() {
        let raw = b"HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: 7\r\n\r\n{\"n\":7}extra";
        let resp = parse_response(raw).unwrap();
        assert_eq!(resp.status, 201);
        assert_eq!(resp.status_text, "Created");
        assert_eq!(resp.header("content-type"), Some("application/json"));
        // Content-Length bounds the body, so trailing bytes past it are dropped.
        assert_eq!(resp.body, b"{\"n\":7}");
    }

    #[test]
    fn parse_response_without_length_uses_remainder() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nbody bytes here";
        let resp = parse_response(raw).unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, b"body bytes here");
    }

    #[test]
    fn parse_response_rejects_garbage() {
        assert!(parse_response(b"not http").is_err());
        assert!(parse_response(b"HTTP/1.1 \r\n\r\n").is_err());
    }

    // =============================================================================================
    // End-to-end loopback through `JsRuntime` (no external network: a server on 127.0.0.1:0 served
    // by a JS handler, hit by the JS `http.get` client and by the global `fetch`, all driven through
    // the shared event-loop reactor pump).
    // =============================================================================================

    use crate::JsRuntime;
    use serde_json::{Value as JsonValue, json};

    /// Run a scenario that stashes its result on `globalThis.__out`, then read it back. The first
    /// `eval` schedules the server + client; the post-eval event-loop drain runs the reactor pump to
    /// completion (the background socket threads do the blocking I/O), so by the time the second
    /// `eval` reads `__out` the request has settled.
    fn run_scenario(body: &str) -> JsonValue {
        let mut rt = JsRuntime::with_node_compat();
        rt.eval(&format!("globalThis.__out = null;\n{body}\n0"))
            .expect("scenario schedules");
        rt.eval("globalThis.__out").expect("result reads back")
    }

    #[test]
    fn http_get_against_a_js_createserver_returns_the_body() {
        // (a) An `http.createServer((req,res)=>…)` on 127.0.0.1:0, hit by `http.get`, delivers the
        // handler-produced body and status to the client through the reactor.
        let out = run_scenario(
            r#"
            const http = require('node:http');
            const server = http.createServer(function (req, res) {
              res.writeHead(200, { 'content-type': 'text/plain' });
              res.end('hello ' + req.url);
            });
            server.listen(0, function () {
              const port = server.address().port;
              http.get('http://127.0.0.1:' + port + '/world', function (res) {
                let body = '';
                res.on('data', function (d) { body += d; });
                res.on('end', function () {
                  globalThis.__out = { status: res.statusCode, body: body };
                  server.close();
                });
              });
            });
            "#,
        );
        assert_eq!(out, json!({ "status": 200, "body": "hello /world" }));
    }

    #[test]
    fn fetch_against_a_js_createserver_resolves_a_real_response() {
        // (b) The global `fetch` performs a real http:// request to the JS server; the resolved
        // `Response` reports the server's status and its `text()` is the handler's body.
        let out = run_scenario(
            r#"
            const http = require('node:http');
            const server = http.createServer(function (req, res) {
              res.writeHead(201, { 'content-type': 'text/plain' });
              res.end('fetched:' + req.url);
            });
            server.listen(0, function () {
              const port = server.address().port;
              fetch('http://127.0.0.1:' + port + '/abc')
                .then(function (r) { return r.text().then(function (t) { return { r: r, t: t }; }); })
                .then(function (o) {
                  globalThis.__out = { ok: o.r.ok, status: o.r.status, text: o.t };
                  server.close();
                })
                .catch(function (e) { globalThis.__out = 'rejected: ' + (e && e.message); server.close(); });
            });
            "#,
        );
        assert_eq!(
            out,
            json!({ "ok": true, "status": 201, "text": "fetched:/abc" })
        );
    }

    #[test]
    fn fetch_json_against_a_js_createserver_parses_the_body() {
        // `fetch().then(r => r.json())` parses a JSON body served by the JS handler — exercising the
        // status, content-type, and body all the way through the reactor and the Response model.
        let out = run_scenario(
            r#"
            const http = require('node:http');
            const server = http.createServer(function (req, res) {
              res.writeHead(200, { 'content-type': 'application/json' });
              res.end(JSON.stringify({ path: req.url, n: 42 }));
            });
            server.listen(0, function () {
              const port = server.address().port;
              fetch('http://127.0.0.1:' + port + '/data')
                .then(function (r) { return r.json().then(function (j) { return { ct: r.headers.get('content-type'), j: j }; }); })
                .then(function (o) {
                  globalThis.__out = { ct: o.ct, path: o.j.path, n: o.j.n };
                  server.close();
                })
                .catch(function (e) { globalThis.__out = 'rejected: ' + (e && e.message); server.close(); });
            });
            "#,
        );
        assert_eq!(
            out,
            json!({ "ct": "application/json", "path": "/data", "n": 42 })
        );
    }

    #[test]
    fn fetch_rejects_https_with_a_catchable_typeerror() {
        // No TLS transport: an https:// fetch rejects (catchable), rather than hanging or pretending.
        let out = run_scenario(
            r#"
            fetch('https://example.com/')
              .then(function () { globalThis.__out = 'resolved'; })
              .catch(function (e) { globalThis.__out = { name: e.name, tls: /TLS/.test(e.message) }; });
            "#,
        );
        assert_eq!(out, json!({ "name": "TypeError", "tls": true }));
    }

    #[test]
    fn net_is_ip_family_is_exposed() {
        // `node:net`'s address classifiers are importable and correct end-to-end.
        let out = run_scenario(
            r#"
            const net = require('node:net');
            globalThis.__out = {
              v4: net.isIP('127.0.0.1'),
              v6: net.isIP('::1'),
              no: net.isIP('nope'),
              is4: net.isIPv4('1.2.3.4'),
              is6: net.isIPv6('fe80::1'),
            };
            "#,
        );
        assert_eq!(
            out,
            json!({ "v4": 4, "v6": 6, "no": 0, "is4": true, "is6": true })
        );
    }
}
