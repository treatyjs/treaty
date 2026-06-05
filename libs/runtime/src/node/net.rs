//! `node:net` — the low-level TCP primitives plus the shared HTTP transport reactor that
//! `node:http` and the global `fetch` are layered on.
//!
//! ## What is real here
//!
//! This module owns two things:
//!
//! * The pure **IP-address classification** core ([`classify_ip`] backing `isIP`/`isIPv4`/`isIPv6`),
//!   which touches neither Nova nor the network and is unit-tested directly.
//! * The process-wide **transport reactor** ([`Reactor`]): the registry of listening HTTP servers and
//!   in-flight client requests that the single JS thread drives through the event loop. All blocking
//!   socket I/O happens on background OS threads (`std::net::TcpStream`/`TcpListener`); the JS thread
//!   never blocks. The reactor is the bridge that lets a JS request handler — which can only run on
//!   the (non-`Send`) JS thread — service a connection accepted on a background thread, and lets a
//!   `fetch()`/`http.get` promise settle once its background client thread finishes.
//!
//! Plaintext `http://` only (loopback in tests; no external network reached by the test suite). TLS /
//! `https://` is a documented follow-up (see `node:https`) because every offline-buildable pure-Rust
//! TLS stack is out of scope for this task's dependency budget.
//!
//! ## Why a reactor rather than a blocking call
//!
//! The runtime is single-threaded: Nova's `Agent` is not `Send`, so a JS server handler
//! `(req, res) => …` can only execute on the JS thread. A server therefore cannot both *accept* a
//! connection and *run its handler* without a second thread for the accept. The reactor splits the
//! two: a background thread does the OS-blocking `accept`/read/write, parks the parsed request in the
//! registry, and blocks on a one-shot channel for the response; the JS thread, pumped by
//! [`js::pump`], drains parked requests, runs their handlers, and pushes the responses back. Client
//! requests are symmetric — a background thread does the blocking round-trip and the JS thread polls
//! for completion. Because the pump re-arms (from JS, via a microtask) only while work is outstanding,
//! the event loop still drains to idle once every request has settled and every server is closed.

use std::collections::HashMap;
use std::io::Write;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;

use nova_vm::ecmascript::{
    Agent, ArgumentsList, JsResult, Object, OrdinaryObject, String as JsString, Value,
};

use crate::node::core::{InstallError, NodeCtx};
use crate::node::globals::define_fn;
use crate::node::http::{self, ParsedRequest, RawResponse};
use crate::node::{GcScope, NodeModule};

// =================================================================================================
// Pure IP-address classification (no Nova, no network; unit-tested directly).
// =================================================================================================

/// Node's `net.isIP(input)` result: `0` (not an IP), `4` (IPv4), or `6` (IPv6).
///
/// Pure ASCII parsing — no allocation, no DNS, no `std::net` parse (so the rules match Node's, which
/// rejects forms `std`'s parser accepts, e.g. a leading-zero octet). Borrows `input`.
pub(crate) fn classify_ip(input: &str) -> u8 {
    if is_ipv4(input) {
        4
    } else if is_ipv6(input) {
        6
    } else {
        0
    }
}

/// Whether `s` is a canonical dotted-quad IPv4 address: four decimal octets `0..=255`, each 1..=3
/// digits with no leading zero (so `01.2.3.4` is rejected, matching Node).
fn is_ipv4(s: &str) -> bool {
    let mut octets = 0u8;
    for part in s.split('.') {
        octets += 1;
        if octets > 4 {
            return false;
        }
        let bytes = part.as_bytes();
        if bytes.is_empty() || bytes.len() > 3 || !bytes.iter().all(u8::is_ascii_digit) {
            return false;
        }
        if bytes.len() > 1 && bytes[0] == b'0' {
            return false; // no leading zero
        }
        if part.parse::<u16>().map(|n| n > 255).unwrap_or(true) {
            return false;
        }
    }
    octets == 4
}

/// Whether `s` is an IPv6 address: hex groups separated by `:`, at most one `::` run, an optional
/// trailing embedded IPv4 (`::ffff:1.2.3.4`), 8 groups total (each absent group on a `::` counted).
fn is_ipv6(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    // Split on the single allowed "::" compressor.
    let double = s.matches("::").count();
    if double > 1 {
        return false;
    }
    let (head, tail, compressed) = match s.find("::") {
        Some(idx) => (&s[..idx], &s[idx + 2..], true),
        None => (s, "", false),
    };

    let mut groups = 0usize;
    let count_side = |side: &str, allow_trailing_v4: bool| -> Option<usize> {
        if side.is_empty() {
            return Some(0);
        }
        let parts: Vec<&str> = side.split(':').collect();
        let mut n = 0usize;
        for (i, part) in parts.iter().enumerate() {
            let last = i + 1 == parts.len();
            if last && allow_trailing_v4 && part.contains('.') {
                if !is_ipv4(part) {
                    return None;
                }
                n += 2; // an embedded IPv4 occupies two 16-bit groups
                continue;
            }
            if part.is_empty()
                || part.len() > 4
                || !part.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return None;
            }
            n += 1;
        }
        Some(n)
    };

    match count_side(head, true) {
        Some(n) => groups += n,
        None => return false,
    }
    match count_side(tail, true) {
        Some(n) => groups += n,
        None => return false,
    }

    if compressed {
        // "::" stands for one or more zero groups, so the explicit groups must leave room for it.
        groups < 8
    } else {
        groups == 8
    }
}

// =================================================================================================
// The transport reactor (process-wide; bridges background socket threads to the JS thread).
// =================================================================================================

/// A request accepted by a server's background thread, awaiting its JS handler.
///
/// `respond` is the one-shot the background thread is blocked on; the JS thread sends the
/// handler-produced [`RawResponse`] through it to release the connection.
pub(crate) struct PendingRequest {
    /// Stable id within the owning server, used by JS to address its `respond`.
    pub(crate) request_id: u64,
    /// The parsed request line + headers + body.
    pub(crate) request: ParsedRequest,
    /// The channel the accept thread is blocked on for this request's response.
    respond: Sender<RawResponse>,
}

/// A TLS request accepted by a TLS server's background thread, awaiting its JS handler.
///
/// Mirrors [`PendingRequest`] but additionally owns the live `rustls` server stream the accept thread
/// must write the response back on — a TLS stream cannot be `try_clone`d the way a `TcpStream` can, so
/// the accept thread keeps the stream and the JS thread only sends the [`RawResponse`] across the
/// one-shot. The accept thread is blocked on `respond` until that arrives.
pub(crate) struct PendingTlsRequest {
    /// Stable id within the owning server, used by JS to address its `respond`.
    pub(crate) request_id: u64,
    /// The parsed request line + headers + body.
    pub(crate) request: ParsedRequest,
    /// The channel the accept thread is blocked on for this request's response.
    respond: Sender<RawResponse>,
}

/// The per-server inbox of parked requests, parameterized by transport (plaintext vs TLS) because the
/// two carry different per-connection state across the reactor boundary.
enum ServerInbox {
    /// Plaintext HTTP: the accept thread parks the response one-shot and re-acquires the cloned
    /// `TcpStream` itself.
    Plain(Receiver<PendingRequest>),
    /// TLS: the accept thread owns the live `rustls` server stream and parks only the one-shot.
    Tls(Receiver<PendingTlsRequest>),
}

/// A listening HTTP/HTTPS server registered in the [`Reactor`].
struct ServerEntry {
    /// The local port the listener bound to (resolved even when `listen(0)` was used).
    port: u16,
    /// Requests parsed by the background accept thread, not yet handed to JS (plaintext or TLS).
    inbox: ServerInbox,
    /// Set once `close()` is requested; the accept thread observes it (lock-free) and stops. Shared
    /// with the accept thread so it can be flipped without taking the registry lock on the hot path.
    closing: Arc<AtomicBool>,
    /// The background accept thread; joined on close so no thread leaks past a server's life.
    accept_thread: Option<JoinHandle<()>>,
    /// A connectable address kept open so the accept thread's blocking `accept()` can be unblocked
    /// by a self-connect at close time (the portable way to wake a blocked `accept`).
    local_addr: std::net::SocketAddr,
}

/// An in-flight client request whose background thread is doing the blocking round-trip.
struct ClientEntry {
    /// `None` while in flight; `Some(Ok)`/`Some(Err)` once the background thread finishes.
    result: Option<Result<RawResponse, String>>,
    /// The background worker; joined when its result is collected.
    thread: Option<JoinHandle<()>>,
    /// The channel the worker sends its single result through.
    rx: Receiver<Result<RawResponse, String>>,
}

/// The process-wide reactor: all listening servers and all in-flight clients, keyed by handle id.
///
/// One per process (a [`OnceLock`] `Mutex`). Handles are monotonic `u64`s minted here; JS holds the
/// integer and addresses its server/client through it. The mutex is held only for the brief registry
/// bookkeeping (insert/remove/poll), never across blocking socket I/O — that lives on the worker
/// threads — so it never serializes actual network work.
#[derive(Default)]
struct Reactor {
    next_id: u64,
    servers: HashMap<u64, ServerEntry>,
    clients: HashMap<u64, ClientEntry>,
}

/// The single global reactor instance.
fn reactor() -> &'static Mutex<Reactor> {
    static REACTOR: OnceLock<Mutex<Reactor>> = OnceLock::new();
    REACTOR.get_or_init(|| Mutex::new(Reactor::default()))
}

impl Reactor {
    /// Mint the next unique handle id.
    fn mint(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        id
    }
}

/// Start a listening HTTP server on `127.0.0.1:port` (port `0` selects a free port). Returns
/// `(handle, bound_port)`. A background thread accepts connections, parses each request, and parks it
/// in the server's inbox while blocking for the JS-produced response.
pub(crate) fn server_listen(port: u16) -> std::io::Result<(u64, u16)> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let local_addr = listener.local_addr()?;
    let bound_port = local_addr.port();

    let (tx, inbox) = channel::<PendingRequest>();
    let handle;
    {
        let mut r = reactor().lock().expect("reactor mutex poisoned");
        handle = r.mint();
    }

    let closing = Arc::new(AtomicBool::new(false));
    let accept_closing = Arc::clone(&closing);

    // The accept thread owns the listener and the request sender. It loops until it observes the
    // shared `closing` flag (checked after each accept, which the close path wakes with a
    // self-connect). Each accepted connection is parsed and parked with a fresh one-shot responder.
    let accept_thread = std::thread::spawn(move || {
        let mut next_request_id: u64 = 0;
        for stream in listener.incoming() {
            // Stop if the owner asked us to close (the close path connects once to wake this accept).
            if accept_closing.load(Ordering::SeqCst) {
                break;
            }
            let mut stream = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            let request = match http::read_request(&mut stream) {
                Ok(req) => req,
                Err(_) => continue,
            };
            let (resp_tx, resp_rx) = channel::<RawResponse>();
            let request_id = next_request_id;
            next_request_id = next_request_id.wrapping_add(1);
            if tx
                .send(PendingRequest {
                    request_id,
                    request,
                    respond: resp_tx,
                })
                .is_err()
            {
                break; // registry dropped the receiver: server is gone.
            }
            // Block until the JS thread runs the handler and sends the response back, then write it.
            match resp_rx.recv() {
                Ok(response) => {
                    let _ = http::write_response(&mut stream, &response);
                    let _ = stream.flush();
                    let _ = stream.shutdown(Shutdown::Both);
                }
                Err(_) => break, // server closed before responding.
            }
        }
    });

    {
        let mut r = reactor().lock().expect("reactor mutex poisoned");
        r.servers.insert(
            handle,
            ServerEntry {
                port: bound_port,
                inbox: ServerInbox::Plain(inbox),
                closing,
                accept_thread: Some(accept_thread),
                local_addr,
            },
        );
    }
    Ok((handle, bound_port))
}

/// Start a listening TLS server on `127.0.0.1:port` (port `0` selects a free port), serving the
/// `config`'s certificate. Returns `(handle, bound_port)`. A background thread accepts connections,
/// performs the TLS handshake + reads one HTTP/1.1 request per connection, and parks each request
/// (with the live TLS stream held thread-side) while blocking for the JS-produced response.
///
/// Symmetric to [`server_listen`] but over TLS; shares the registry, `server_port`, and `server_close`
/// so a TLS server is closed and joined exactly like a plaintext one. The certificate verification on
/// the *client* side is the client's concern ([`crate::node::tls::ClientTrust`]); this server presents
/// its single cert and does not request client auth.
pub(crate) fn tls_server_listen(
    port: u16,
    config: Arc<rustls::ServerConfig>,
) -> std::io::Result<(u64, u16)> {
    use crate::node::tls;

    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let local_addr = listener.local_addr()?;
    let bound_port = local_addr.port();

    let (tx, inbox) = channel::<PendingTlsRequest>();
    let handle = {
        let mut r = reactor().lock().expect("reactor mutex poisoned");
        r.mint()
    };

    let closing = Arc::new(AtomicBool::new(false));
    let accept_closing = Arc::clone(&closing);

    let accept_thread = std::thread::spawn(move || {
        let mut next_request_id: u64 = 0;
        for stream in listener.incoming() {
            if accept_closing.load(Ordering::SeqCst) {
                break;
            }
            let tcp = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            // Perform the TLS handshake + read one request. A handshake failure (e.g. an untrusted
            // client that aborts, or a probe with no SNI) drops this connection without wedging the
            // accept loop.
            let (request, mut tls_stream) = match tls::accept_tls(tcp, Arc::clone(&config)) {
                Ok(pair) => pair,
                Err(_) => continue,
            };
            let (resp_tx, resp_rx) = channel::<RawResponse>();
            let request_id = next_request_id;
            next_request_id = next_request_id.wrapping_add(1);
            if tx
                .send(PendingTlsRequest {
                    request_id,
                    request,
                    respond: resp_tx,
                })
                .is_err()
            {
                break; // registry dropped the receiver: server is gone.
            }
            // Block until the JS thread runs the handler and sends the response back, then write it
            // over the (thread-owned) TLS stream and close the session.
            match resp_rx.recv() {
                Ok(response) => tls::respond_tls(&mut tls_stream, &response),
                Err(_) => break, // server closed before responding.
            }
        }
    });

    {
        let mut r = reactor().lock().expect("reactor mutex poisoned");
        r.servers.insert(
            handle,
            ServerEntry {
                port: bound_port,
                inbox: ServerInbox::Tls(inbox),
                closing,
                accept_thread: Some(accept_thread),
                local_addr,
            },
        );
    }
    Ok((handle, bound_port))
}

/// The bound local port of a listening server, or `None` if the handle is unknown.
pub(crate) fn server_port(handle: u64) -> Option<u16> {
    reactor()
        .lock()
        .ok()
        .and_then(|r| r.servers.get(&handle).map(|s| s.port))
}

/// Drain every request parked by `handle`'s accept thread that has not yet been handed to JS.
///
/// Each returned [`PendingRequest`] carries the one-shot the accept thread is blocked on; the caller
/// runs the JS handler and later calls [`server_respond`] with the matching `request_id`.
pub(crate) fn server_take_pending(handle: u64) -> Vec<PendingRequest> {
    let mut out = Vec::new();
    if let Ok(r) = reactor().lock() {
        if let Some(server) = r.servers.get(&handle) {
            if let ServerInbox::Plain(inbox) = &server.inbox {
                while let Ok(req) = inbox.try_recv() {
                    out.push(req);
                }
            }
        }
    }
    out
}

/// Drain every TLS request parked by `handle`'s accept thread that has not yet been handed to JS.
///
/// The TLS counterpart of [`server_take_pending`]. Each returned [`PendingTlsRequest`] carries the
/// one-shot the accept thread is blocked on; the caller runs the JS handler and later calls
/// [`tls_request_respond`] with the matching `request_id`. A handle that is not a TLS server yields an
/// empty vec.
pub(crate) fn tls_server_take_pending(handle: u64) -> Vec<PendingTlsRequest> {
    let mut out = Vec::new();
    if let Ok(r) = reactor().lock() {
        if let Some(server) = r.servers.get(&handle) {
            if let ServerInbox::Tls(inbox) = &server.inbox {
                while let Ok(req) = inbox.try_recv() {
                    out.push(req);
                }
            }
        }
    }
    out
}

/// Whether `handle` still has an open accept thread that may yield more requests.
pub(crate) fn server_is_open(handle: u64) -> bool {
    reactor()
        .lock()
        .ok()
        .map(|r| r.servers.contains_key(&handle))
        .unwrap_or(false)
}

/// Close a listening server: flag the accept thread to stop, wake its blocked `accept()` with a
/// self-connect, and join it so no background thread outlives the server.
pub(crate) fn server_close(handle: u64) {
    let addr = {
        let r = reactor().lock().expect("reactor mutex poisoned");
        match r.servers.get(&handle) {
            Some(server) => {
                server.closing.store(true, Ordering::SeqCst);
                Some(server.local_addr)
            }
            None => None,
        }
    };
    // Wake the blocked accept() by connecting once; the thread then observes `closing` and returns.
    if let Some(addr) = addr {
        if let Ok(stream) = TcpStream::connect(addr) {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
    let thread = {
        let mut r = reactor().lock().expect("reactor mutex poisoned");
        r.servers.remove(&handle).and_then(|mut s| s.accept_thread.take())
    };
    if let Some(thread) = thread {
        let _ = thread.join();
    }
}

/// Hand a server's pending request its response (by `request_id`), unblocking the accept thread so it
/// writes the bytes to the client. Consumes the matching one-shot sender, taken from a freshly
/// drained pending request — so this is called with a [`PendingRequest`] in hand.
pub(crate) fn request_respond(pending: PendingRequest, response: RawResponse) {
    // The accept thread is blocked on `pending.respond`; sending releases it. A send error means the
    // connection already went away (client hung up / server closed), which is harmless to ignore.
    let _ = pending.respond.send(response);
}

/// Hand a TLS server's pending request its response (by `request_id`), unblocking the accept thread so
/// it writes the bytes over the live TLS stream. The TLS counterpart of [`request_respond`].
pub(crate) fn tls_request_respond(pending: PendingTlsRequest, response: RawResponse) {
    let _ = pending.respond.send(response);
}

/// Spawn a background client thread that performs one blocking HTTP/1.1 request and returns a handle
/// to poll for completion via [`client_poll`].
pub(crate) fn client_start(req: ParsedRequest, host: String, port: u16) -> u64 {
    let (tx, rx) = channel::<Result<RawResponse, String>>();
    let thread = std::thread::spawn(move || {
        let result = http::client_roundtrip(&host, port, &req);
        let _ = tx.send(result);
    });
    register_client(thread, rx)
}

/// Spawn a background client thread that performs one blocking TLS handshake + HTTP/1.1 request to
/// `host:port` (verifying the server cert per `trust`) and returns a handle to poll via [`client_poll`].
///
/// Shares the client registry and [`client_poll`]/[`ClientPoll`] with the plaintext [`client_start`]
/// — the only difference is the transport — so the JS reactor pump drives an https client exactly like
/// an http one.
pub(crate) fn tls_client_start(
    req: ParsedRequest,
    host: String,
    port: u16,
    trust: crate::node::tls::ClientTrust,
) -> u64 {
    let (tx, rx) = channel::<Result<RawResponse, String>>();
    let thread = std::thread::spawn(move || {
        let result = crate::node::tls::tls_client_roundtrip(&host, port, &req, &trust);
        let _ = tx.send(result);
    });
    register_client(thread, rx)
}

/// Register a started background client worker in the reactor and mint its poll handle. Shared by the
/// plaintext and TLS client starts.
fn register_client(thread: JoinHandle<()>, rx: Receiver<Result<RawResponse, String>>) -> u64 {
    let mut r = reactor().lock().expect("reactor mutex poisoned");
    let handle = r.mint();
    r.clients.insert(
        handle,
        ClientEntry {
            result: None,
            thread: Some(thread),
            rx,
        },
    );
    handle
}

/// The outcome of polling an in-flight client.
pub(crate) enum ClientPoll {
    /// The handle is unknown (already collected, or never existed).
    Unknown,
    /// The request is still running.
    Pending,
    /// The request finished; its result (response or error) is returned and the handle is removed.
    Done(Result<RawResponse, String>),
}

/// Poll a client started by [`client_start`]. On completion the background thread is joined and the
/// entry removed, so a settled client never lingers in the registry.
pub(crate) fn client_poll(handle: u64) -> ClientPoll {
    let mut r = reactor().lock().expect("reactor mutex poisoned");
    let Some(entry) = r.clients.get_mut(&handle) else {
        return ClientPoll::Unknown;
    };
    if entry.result.is_none() {
        match entry.rx.try_recv() {
            Ok(result) => entry.result = Some(result),
            Err(std::sync::mpsc::TryRecvError::Empty) => return ClientPoll::Pending,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                entry.result = Some(Err("client thread terminated unexpectedly".to_owned()));
            }
        }
    }
    // Settled: collect the result, join the worker, and drop the registry entry.
    let mut entry = r.clients.remove(&handle).expect("entry present");
    if let Some(thread) = entry.thread.take() {
        let _ = thread.join();
    }
    ClientPoll::Done(entry.result.expect("result set on the settled path"))
}

// =================================================================================================
// JS-facing wiring for `node:net`.
// =================================================================================================

/// Zero-sized marker for the `node:net` builtin.
pub(crate) struct NetModule;

impl NodeModule for NetModule {
    const SPECIFIER: &'static str = "net";

    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: GcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError> {
        install(agent, ctx, gc)
    }
}

/// Uniform per-module entry. Returns the `node:net` exports object.
///
/// `net` surfaces the pure `isIP`/`isIPv4`/`isIPv6` classifiers (Node's address validators) over the
/// native [`classify_ip`]. The connection/server object model lives in `node:http`, which is where
/// the reactor is consumed; a standalone raw-socket `net.Socket` is a documented follow-up — the
/// HTTP-shaped transport is what Bun/CF `nodejs_compat` callers reach for.
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    let gc = gc.into_nogc();
    let obj = OrdinaryObject::create_empty_object(agent, gc);
    define_fn(agent, obj, "isIP", js::is_ip, 1, gc);
    define_fn(agent, obj, "isIPv4", js::is_ipv4_fn, 1, gc);
    define_fn(agent, obj, "isIPv6", js::is_ipv6_fn, 1, gc);
    Ok(obj.into())
}

/// JS wrappers for the `net` address classifiers.
mod js {
    use super::*;

    /// Read argument `idx` as an owned string, or `""` when it is not a JS string.
    fn arg_str(agent: &Agent, args: &ArgumentsList, idx: usize) -> String {
        match JsString::try_from(args.get(idx)) {
            Ok(s) => s.to_string_lossy(agent).into_owned(),
            Err(_) => String::new(),
        }
    }

    /// `net.isIP(input)` -> `0 | 4 | 6`.
    pub(super) fn is_ip<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let s = arg_str(agent, &args, 0);
        let _ = gc;
        Ok(Value::Integer(i32::from(classify_ip(&s)).into()))
    }

    /// `net.isIPv4(input)` -> boolean.
    pub(super) fn is_ipv4_fn<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        _gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let s = arg_str(agent, &args, 0);
        Ok(Value::Boolean(classify_ip(&s) == 4))
    }

    /// `net.isIPv6(input)` -> boolean.
    pub(super) fn is_ipv6_fn<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        _gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let s = arg_str(agent, &args, 0);
        Ok(Value::Boolean(classify_ip(&s) == 6))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv4_accepts_canonical_and_rejects_malformed() {
        assert_eq!(classify_ip("127.0.0.1"), 4);
        assert_eq!(classify_ip("0.0.0.0"), 4);
        assert_eq!(classify_ip("255.255.255.255"), 4);
        // Out of range, leading zero, wrong arity, non-digit.
        assert_eq!(classify_ip("256.0.0.1"), 0);
        assert_eq!(classify_ip("01.2.3.4"), 0);
        assert_eq!(classify_ip("1.2.3"), 0);
        assert_eq!(classify_ip("1.2.3.4.5"), 0);
        assert_eq!(classify_ip("1.2.3.x"), 0);
        assert_eq!(classify_ip(""), 0);
    }

    #[test]
    fn ipv6_accepts_canonical_compressed_and_embedded_v4() {
        assert_eq!(classify_ip("::1"), 6);
        assert_eq!(classify_ip("::"), 6);
        assert_eq!(classify_ip("2001:db8::1"), 6);
        assert_eq!(classify_ip("fe80::1"), 6);
        assert_eq!(
            classify_ip("2001:0db8:0000:0000:0000:0000:0000:0001"),
            6
        );
        assert_eq!(classify_ip("::ffff:127.0.0.1"), 6);
    }

    #[test]
    fn ipv6_rejects_malformed() {
        // Two compressors, oversize group, non-hex, too few groups uncompressed.
        assert_eq!(classify_ip("1::2::3"), 0);
        assert_eq!(classify_ip("12345::1"), 0);
        assert_eq!(classify_ip("xyz::1"), 0);
        assert_eq!(classify_ip("1:2:3:4:5:6:7"), 0);
        assert_eq!(classify_ip("1:2:3:4:5:6:7:8:9"), 0);
    }

    #[test]
    fn server_round_trip_through_the_reactor() {
        // Stand up a server, fire a background client at it, drive one pending request to a response
        // entirely at the reactor level (no JS), and confirm the client sees the bytes back.
        let (handle, port) = server_listen(0).expect("listener binds");
        assert!(port > 0);
        assert_eq!(server_port(handle), Some(port));

        let req = ParsedRequest::get("/hello");
        let client = client_start(req, "127.0.0.1".to_owned(), port);

        // Poll the server for the parked request, respond, then poll the client to completion. The
        // production drive loop is the JS reactor pump (which re-arms until done); here we emulate it
        // with a bounded spin that sleeps a beat between turns so the background socket threads make
        // progress (a tight yield-only loop could exhaust its bound before the round-trip completes).
        let mut response_body = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            for pending in server_take_pending(handle) {
                assert_eq!(pending.request.path, "/hello");
                let resp = RawResponse::text(200, "OK", "pong");
                request_respond(pending, resp);
            }
            match client_poll(client) {
                ClientPoll::Done(Ok(resp)) => {
                    response_body = Some(String::from_utf8_lossy(&resp.body).into_owned());
                    break;
                }
                ClientPoll::Done(Err(e)) => panic!("client errored: {e}"),
                _ => {}
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        server_close(handle);
        assert_eq!(response_body.as_deref(), Some("pong"));
        // The handle is gone once closed.
        assert!(!server_is_open(handle));
    }
}
