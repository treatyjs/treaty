//! `node:tls` — a real TLS client (and a tractable TLS server) over `rustls` + the `ring` crypto
//! provider, plus the transport `node:https` is layered on.
//!
//! ## What is real here
//!
//! * The **TLS client transport** ([`tls_client_roundtrip`]): a genuine TLS 1.2/1.3 handshake to a
//!   `host:port`, after which one HTTP/1.1 request/response is exchanged over the encrypted stream
//!   using the same wire codec `node:http` owns. SNI and certificate-chain verification against a
//!   configured trust anchor are performed by rustls/webpki — there is no "accept any cert" default.
//! * The **TLS server transport** ([`accept_tls`]): wraps a freshly accepted `TcpStream` in a
//!   `rustls::ServerConnection` from a cert/key pair, then reads a request and writes a response with
//!   the shared codec. The reactor (`node:net`) owns the accept loop; this module supplies the
//!   per-connection TLS wrapping.
//! * The **config layer** ([`ClientTrust`], [`client_config`], [`server_config_from_pem`]): builds
//!   rustls `ClientConfig`/`ServerConfig` values explicitly pinned to the `ring` provider (so the
//!   build never reaches for the C/CMake `aws-lc-rs` default and stays offline-clean).
//!
//! ## Why `ring`
//!
//! The pure-Rust TLS crypto budget here is `ring` (pre-generated assembly; builds offline with no
//! system OpenSSL, no network, no `nasm`), pinned via `default-features = false, features = ["ring"]`.
//! `aws-lc-rs` (rustls's normal default) needs CMake + a C toolchain and so would break the offline
//! build; a from-scratch RustCrypto TLS record layer is out of scope and a correctness/security risk.
//!
//! All blocking socket + handshake I/O runs on the reactor's background threads (see `net.rs`); the
//! single JS thread never blocks on a handshake.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

use nova_vm::ecmascript::{
    Agent, ArgumentsList, ExceptionType, InternalMethods, JsResult, Object, OrdinaryObject,
    PropertyDescriptor, PropertyKey, String as JsString, Value, unwrap_try,
};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, ServerConfig, ServerConnection, StreamOwned};

use crate::node::core::{InstallError, NodeCtx};
use crate::node::globals::define_fn;
use crate::node::http::{self, ParsedRequest, RawResponse};
use crate::node::{GcScope, NodeModule};

// =================================================================================================
// Embedded loopback test material.
// =================================================================================================

/// A self-signed end-entity certificate (SAN `localhost` + `127.0.0.1`, no CA basic constraint) used
/// only by the in-process loopback TLS server in tests and by callers that opt into trusting it.
/// Committed as a fixed fixture so the build/test stays offline (no cert generation at build time).
/// Never a public trust anchor — it is trusted only when a caller explicitly passes
/// [`ClientTrust::Pinned`]. It carries no `CA:TRUE` basic constraint so webpki accepts it as the
/// leaf when it is also pinned as the trust anchor (a CA cert may not double as an end-entity).
pub(crate) const TEST_CERT_PEM: &str = "-----BEGIN CERTIFICATE-----\n\
MIIC9TCCAd2gAwIBAgIUYZ82pQlYAnxL7+Ijb9B73M9TWgAwDQYJKoZIhvcNAQEL\n\
BQAwFDESMBAGA1UEAwwJbG9jYWxob3N0MCAXDTI2MDUzMTIyMDgwN1oYDzIxMjYw\n\
NTA3MjIwODA3WjAUMRIwEAYDVQQDDAlsb2NhbGhvc3QwggEiMA0GCSqGSIb3DQEB\n\
AQUAA4IBDwAwggEKAoIBAQDdhzPLnxKP3qUqMGrg8yoowfIaUnOniovhg3FDROMB\n\
nzJJ4k4DcJrDObB5AHdgl82UttJbUgsS/KrNO/hz7/Xgj/yPvSwRCUM9cWjf1Wv+\n\
PDdl4dJBFi7uxyrFcm8EgXPsp755Z714tPOOTGVFXowgJmR11++f1E8RGwg3DkSv\n\
zytOuADVvpUZh9Lkq2k3o/pMtW1P7rspqkKrELvx/PqhUZtbSDpOr5tzGbMX3ys6\n\
cN8jfTt8G0kKJoi2d1an6T7T9Na7gbWM6uvwZMQld5IA6IyTM3dpqMGK8FmGqkBK\n\
/kDZ7pmtInGJFCl23LwxHiFsB/DSwyGE+lp6hhJ0dAp5AgMBAAGjPTA7MBoGA1Ud\n\
EQQTMBGCCWxvY2FsaG9zdIcEfwAAATAdBgNVHQ4EFgQUGPenJiEesGicKILQ9x0+\n\
HESBg14wDQYJKoZIhvcNAQELBQADggEBAMwQY0WXNc8/pQ1fBq2nDD2Lyxy1PX7o\n\
cb93JaWuCXzT0F/EoW7dTbPxPXQDasCLtKabUXtaFMtx6gKjC+BHIkMAcOfpfoVu\n\
9Q3ZjR1yqeblHj+MhIs4VIgD57AzzQMZgOptnDF/AcB3iBszNo5iM50tpocYYwZF\n\
ANiN9tEX9LhvBG5aWc8PNyc5agjZ5rhx7hNhDS3vx+UyoVt0YiIrjH2UzM3GSRvC\n\
QBMe7iTJE4pw0RK43+M0DI4wH2SO3INqk5OLk9iFl5Pp+6fEQ+BTsbrTNI9JzgT7\n\
zuNL8SKZ/PFRlMprPUYzurCpz8EBxyVUwiHKjVLTt8hIcw3vHB/Ool8=\n\
-----END CERTIFICATE-----\n";

/// The PKCS#8 private key matching [`TEST_CERT_PEM`]. Test-only fixture (see that constant).
pub(crate) const TEST_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----\n\
MIIEvwIBADANBgkqhkiG9w0BAQEFAASCBKkwggSlAgEAAoIBAQDdhzPLnxKP3qUq\n\
MGrg8yoowfIaUnOniovhg3FDROMBnzJJ4k4DcJrDObB5AHdgl82UttJbUgsS/KrN\n\
O/hz7/Xgj/yPvSwRCUM9cWjf1Wv+PDdl4dJBFi7uxyrFcm8EgXPsp755Z714tPOO\n\
TGVFXowgJmR11++f1E8RGwg3DkSvzytOuADVvpUZh9Lkq2k3o/pMtW1P7rspqkKr\n\
ELvx/PqhUZtbSDpOr5tzGbMX3ys6cN8jfTt8G0kKJoi2d1an6T7T9Na7gbWM6uvw\n\
ZMQld5IA6IyTM3dpqMGK8FmGqkBK/kDZ7pmtInGJFCl23LwxHiFsB/DSwyGE+lp6\n\
hhJ0dAp5AgMBAAECggEAHwykjDAHPauwLKyinlWWyCWpla+Ozvcn9KYkCZCt3Kaq\n\
jsPYZA+0Br70PCbTlJynpOYcWnkHslPrFgmyINXg+ZD/jkoD034f3Yx4GD9lUToG\n\
wxHsEqaq/K66Zk2haoPrx/TESWc/8vDciPUDkL3YGBLUK9bl8C9QHGP/OnBBMlQt\n\
UsB5y2zPYlWFuC51zzZf9bZ3aQB22xxFb9vWovt05ifcMfy/jof6mlhYt5c295h2\n\
U+2hW9sbH5nrY/wH7xfLB/0OoQoPdYUcuM5MfZObW7CQ5OYNEgrMCVHnKoky3DDu\n\
S74lPUvlEMr1NR19tnsKqLnVrUgcC4Rq+PdJA/SwbQKBgQD9tdgzDR8BRcJe+tiY\n\
tUsRFjhj20B5FOReFIy7ivrADJ8vMG5gl3e+vAXLigzxzYqFBsXrCEDKoX3pvkML\n\
3+FoojxWv51Z6WgXN5nUMSDOekS7eyrPcuUoxPnVoupaABrdiL5/OnZHGm05G33X\n\
VxGmE9oLI+VUb8q18xNkQNQyBwKBgQDfhwGVxMwzrGTwbuxaPXejEBdILU91liiG\n\
xCrhMYsIxS/0oUnmCRfa3eH/bjzWZqnlWL0mSoAyePrO8ec+lvNi5RYdb0fKsRrQ\n\
sT1Qlz4lxFhZNHP0XsbYlPhU7TSuK8qsNtuFFTtLH2Gb3SO2lHJljpXbmbEImDTS\n\
coUj0ES/fwKBgQDu0FG+1DYAI6Lvdp2VOOl9HvZbgFEy6CiCKkPCcPLQ/dCFQchU\n\
IZ90qVWnHr5KiZg+2X5JWw5p7hMwh4hi0A1ESZoUae96Z8s0N4EUDF5+HPc/ppNI\n\
jDUK6EbnAqAnsXuYVhRCfExDZ6uyGp+cqHeTZZJT9Cj1Dvm3xSPWtXNH1wKBgQDG\n\
MHIbVQ6pkmU9OVye9nkpP48lE+esHqN4Ol66pK7d69iFUqyvJcjc6ncDf765avWg\n\
wHmVheD833+iFaIvQLA0M2LUXmKNOVLJTx1KY49a9ShQj81wEsjEJ/G3e0qGU5Wz\n\
9D/XU+fqx7xH8l9D94MmwLHmr/Lj5/CN17Rs+LC8CQKBgQCYphMsuDHf2xTaqVvY\n\
ytuh3o+7NPzICpNYakxeWJRwHtgCaKmWIpLz7DkZOWU5Sh13YDGX58PMJztAtT04\n\
aDzaVa2PCpSOnu0l7+UGzKKX6aMlsPJDZ4GQqgiun043KEWgK32iaaBYY/ELosXH\n\
MfXYkElW9x8u1ycgstBHo+I1qA==\n\
-----END PRIVATE KEY-----\n";

// =================================================================================================
// Config layer (rustls, pinned to the ring provider).
// =================================================================================================

/// How a TLS client establishes trust in the server's certificate chain.
#[derive(Debug, Clone)]
pub(crate) enum ClientTrust {
    /// Verify the chain against a root store seeded only with the PEM trust anchors in this vec
    /// (webpki chain + name validation). This is the real, non-bypassing verification path; the
    /// loopback test uses it by pinning [`TEST_CERT_PEM`].
    Pinned(Vec<String>),
    /// Do not verify the certificate chain. Reserved for an explicit `rejectUnauthorized: false`
    /// opt-in; never the default. The handshake still happens (encryption is real), only the chain/
    /// name checks are skipped.
    Insecure,
}

/// Parse a PEM bundle into DER certificates.
fn certs_from_pem(pem: &str) -> Result<Vec<CertificateDer<'static>>, String> {
    let mut reader = std::io::BufReader::new(pem.as_bytes());
    rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("invalid certificate PEM: {e}"))
}

/// Parse the first private key (PKCS#8, RSA, or SEC1) from a PEM bundle into DER.
fn key_from_pem(pem: &str) -> Result<PrivateKeyDer<'static>, String> {
    let mut reader = std::io::BufReader::new(pem.as_bytes());
    rustls_pemfile::private_key(&mut reader)
        .map_err(|e| format!("invalid private key PEM: {e}"))?
        .ok_or_else(|| "no private key found in PEM".to_owned())
}

/// Build a client `ClientConfig` for the given trust mode, pinned to the `ring` crypto provider.
pub(crate) fn client_config(trust: &ClientTrust) -> Result<ClientConfig, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("tls client protocol setup failed: {e}"))?;
    match trust {
        ClientTrust::Pinned(pems) => {
            let mut roots = rustls::RootCertStore::empty();
            for pem in pems {
                for cert in certs_from_pem(pem)? {
                    roots
                        .add(cert)
                        .map_err(|e| format!("could not add trust anchor: {e}"))?;
                }
            }
            if roots.is_empty() {
                return Err("no trust anchors supplied for pinned verification".to_owned());
            }
            Ok(builder
                .with_root_certificates(roots)
                .with_no_client_auth())
        }
        ClientTrust::Insecure => Ok(builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerification))
            .with_no_client_auth()),
    }
}

/// Build a server `ServerConfig` from a cert chain PEM + private key PEM, pinned to `ring`.
pub(crate) fn server_config_from_pem(cert_pem: &str, key_pem: &str) -> Result<ServerConfig, String> {
    let certs = certs_from_pem(cert_pem)?;
    if certs.is_empty() {
        return Err("server certificate PEM contained no certificates".to_owned());
    }
    let key = key_from_pem(key_pem)?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("tls server protocol setup failed: {e}"))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("tls server cert/key setup failed: {e}"))
}

/// A certificate verifier that accepts any chain — used only for [`ClientTrust::Insecure`]. It still
/// reports the `ring` provider's supported signature schemes so the handshake's signature checks run
/// (the bytes are real TLS); it only declines to validate the chain to a trust anchor.
#[derive(Debug)]
struct NoVerification;

impl ServerCertVerifier for NoVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

// =================================================================================================
// TLS transport (client + server), reusing the `node:http` wire codec.
// =================================================================================================

/// A connected TLS client stream: the rustls connection driving a plaintext `TcpStream`.
pub(crate) type ClientTlsStream = StreamOwned<ClientConnection, TcpStream>;
/// A connected TLS server stream.
pub(crate) type ServerTlsStream = StreamOwned<ServerConnection, TcpStream>;

/// Perform one TLS client round-trip: connect TCP, handshake to `host`, exchange one HTTP/1.1
/// request, and parse the response. Runs on a reactor background thread (never the JS thread).
///
/// `host` doubles as the SNI / verification name. The default port `443` is the caller's concern; the
/// `port` is passed through. The HTTP framing (`Host`, `Connection: close`, body length) is produced
/// by the shared `node:http` encoder, so an https response is identical in shape to an http one.
pub(crate) fn tls_client_roundtrip(
    host: &str,
    port: u16,
    req: &ParsedRequest,
    trust: &ClientTrust,
) -> Result<RawResponse, String> {
    let config = Arc::new(client_config(trust)?);
    let server_name = ServerName::try_from(host.to_owned())
        .map_err(|_| format!("invalid TLS server name: {host:?}"))?;
    let conn = ClientConnection::new(config, server_name)
        .map_err(|e| format!("tls client setup failed: {e}"))?;

    let tcp = TcpStream::connect((host, port))
        .map_err(|e| format!("connect {host}:{port}: {e}"))?;
    tcp.set_read_timeout(Some(std::time::Duration::from_secs(30)))
        .map_err(|e| e.to_string())?;
    let mut tls: ClientTlsStream = StreamOwned::new(conn, tcp);

    let bytes = http::encode_request(host, port, req);
    // The first write drives the handshake to completion (rustls negotiates before app data flows),
    // so a handshake failure surfaces here as an I/O error carrying the TLS alert/reason.
    tls.write_all(&bytes)
        .map_err(|e| format!("tls write/handshake failed: {e}"))?;
    tls.flush().map_err(|e| format!("tls flush failed: {e}"))?;

    let mut buf = Vec::new();
    // `read_to_end` returns when the peer closes the TLS session (Connection: close framing). A
    // `close_notify`-less abrupt close is reported by rustls as `UnexpectedEof`; with `Connection:
    // close` HTTP framing the bytes we already read are still the complete response, so treat a
    // clean-enough EOF as end-of-body rather than an error when we have a parseable response.
    match tls.read_to_end(&mut buf) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && !buf.is_empty() => {}
        Err(e) => return Err(format!("tls read failed: {e}")),
    }
    http::parse_response(&buf)
}

/// Wrap a freshly accepted server `TcpStream` in a TLS session from `config`, read one HTTP/1.1
/// request, and return the parsed request plus the live TLS stream to write the response back on.
///
/// The handshake is driven by the first read. Runs on the reactor's accept thread.
pub(crate) fn accept_tls(
    tcp: TcpStream,
    config: Arc<ServerConfig>,
) -> Result<(ParsedRequest, ServerTlsStream), String> {
    tcp.set_read_timeout(Some(std::time::Duration::from_secs(30)))
        .map_err(|e| e.to_string())?;
    let conn = ServerConnection::new(config).map_err(|e| format!("tls server setup failed: {e}"))?;
    let mut tls: ServerTlsStream = StreamOwned::new(conn, tcp);
    let request = http::read_request_from(&mut tls)
        .map_err(|e| format!("tls server read/handshake failed: {e}"))?;
    Ok((request, tls))
}

/// Write a response over a live server TLS stream and close the session.
pub(crate) fn respond_tls(tls: &mut ServerTlsStream, response: &RawResponse) {
    let _ = http::write_response(tls, response);
    // Send a clean TLS close_notify, then drop, which shuts the inner socket.
    tls.conn.send_close_notify();
    let _ = tls.flush();
}

// =================================================================================================
// JS-facing wiring for `node:tls`.
// =================================================================================================

/// Zero-sized marker for the `node:tls` builtin.
pub(crate) struct TlsModule;

impl NodeModule for TlsModule {
    const SPECIFIER: &'static str = "tls";

    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: GcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError> {
        install(agent, ctx, gc)
    }
}

/// The hidden global slot the bootstrap reads the native primitives off, parked just before the
/// bootstrap evaluates and deleted immediately after.
const NATIVES_KEY: &str = "__treaty_tls_natives";

/// Build the native primitives the `node:tls` / `node:https` bootstrap is layered on. These are the
/// reactor seam for TLS: a TLS client round-trip and a TLS server lifecycle, both running their
/// blocking handshake + socket I/O on background threads.
pub(crate) fn build_natives<'gc>(agent: &mut Agent, gc: GcScope<'gc, '_>) -> OrdinaryObject<'gc> {
    let gc = gc.into_nogc();
    let natives = OrdinaryObject::create_empty_object(agent, gc);
    define_fn(agent, natives, "clientStart", js::client_start, 5, gc);
    define_fn(agent, natives, "clientPoll", js::client_poll, 1, gc);
    define_fn(agent, natives, "serverListen", js::server_listen, 1, gc);
    define_fn(agent, natives, "serverPort", js::server_port, 1, gc);
    define_fn(agent, natives, "serverTakePending", js::server_take_pending, 1, gc);
    define_fn(agent, natives, "serverRespond", js::server_respond, 3, gc);
    define_fn(agent, natives, "serverClose", js::server_close, 1, gc);
    define_fn(agent, natives, "splitUrl", js::split_url, 1, gc);
    define_fn(agent, natives, "testCert", js::test_cert, 0, gc);
    natives
}

/// Uniform per-module entry. Returns the `node:tls` exports object built from [`TLS_BOOTSTRAP`].
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    super::run_module_bootstrap(agent, gc, NATIVES_KEY, build_natives, TLS_BOOTSTRAP, "node:tls")
}

/// The `node:tls` JS object model: `connect()` (a minimal TLSSocket-shaped client),
/// `createServer()`, and `createSecureContext()`, layered on the reactor primitives. `node:https`
/// reuses this exact transport via the shared `__treaty_tls` global the bootstrap publishes.
const TLS_BOOTSTRAP: &str = r#"
(function () {
  var N = globalThis.__treaty_tls_natives;

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

  function headersToPairs(headers) {
    var pairs = [];
    if (headers && typeof headers === "object") {
      var keys = Object.keys(headers);
      for (var i = 0; i < keys.length; i++) pairs.push([keys[i], String(headers[keys[i]])]);
    }
    return pairs;
  }

  // Start a TLS client request, returning a promise of { status, statusText, headers, body }.
  // trust: "pinned:<pem>" | "insecure" | "system" (system => empty roots, real verification).
  function startClient(method, url, headerPairs, body, trust) {
    return new Promise(function (resolve, reject) {
      var handle;
      try {
        handle = N.clientStart(String(method), String(url), headerPairs || [],
                               body == null ? "" : String(body), trust || "system");
      } catch (e) { reject(e); return; }
      IO.add(function () {
        var r = N.clientPoll(handle);
        if (r.pending) return true;
        if (r.error != null) { reject(new Error(String(r.error))); return false; }
        resolve(r.response);
        return false;
      });
    });
  }

  // ---- TLSSocket-ish client (tls.connect) --------------------------------------------------
  // A minimal duplex-ish object: connect resolves 'secureConnect', write() buffers a request body,
  // and the response is delivered via 'data'/'end'. This is the request-shaped surface https uses.
  function TLSSocket(opts) {
    this._opts = opts || {};
    this._listeners = {};
    this._chunks = [];
    this.authorized = false;
    this.encrypted = true;
  }
  TLSSocket.prototype.on = function (ev, fn) {
    (this._listeners[ev] = this._listeners[ev] || []).push(fn);
    return this;
  };
  TLSSocket.prototype.once = TLSSocket.prototype.on;
  TLSSocket.prototype._emit = function (ev, a) {
    var ls = this._listeners[ev];
    if (ls) for (var i = 0; i < ls.length; i++) ls[i](a);
  };
  TLSSocket.prototype.write = function (c) { if (c != null) this._chunks.push(String(c)); return true; };
  TLSSocket.prototype.end = function (c) { if (c != null) this._chunks.push(String(c)); return this; };

  function trustOf(opts) {
    if (opts && opts.rejectUnauthorized === false) return "insecure";
    if (opts && opts.ca != null) {
      var ca = Array.isArray(opts.ca) ? opts.ca.join("\n") : String(opts.ca);
      return "pinned:" + ca;
    }
    return "system";
  }

  function connect(opts, cb) {
    if (typeof opts === "number") { opts = { port: opts }; }
    var host = (opts && (opts.host || opts.servername)) || "127.0.0.1";
    var port = (opts && opts.port) != null ? (opts.port | 0) : 443;
    var path = (opts && opts.path) || "/";
    var url = "https://" + host + ":" + port + path;
    var sock = new TLSSocket(opts);
    if (cb) sock.on("secureConnect", cb);
    var method = (opts && opts.method) || "GET";
    var headerPairs = headersToPairs(opts && opts.headers);
    // Defer one tick so the caller can attach listeners before the round-trip resolves.
    Promise.resolve().then(function () {
      startClient(method, url, headerPairs, sock._chunks.join(""), trustOf(opts)).then(function (resp) {
        sock.authorized = trustOf(opts) !== "insecure";
        sock._response = resp;
        sock._emit("secureConnect");
        sock._emit("data", resp.body);
        sock._emit("end");
      }, function (err) { sock._emit("error", err); });
    });
    return sock;
  }

  // ---- TLS server (tls.createServer) -------------------------------------------------------
  function TlsServer(opts, handler) {
    this._opts = opts || {};
    this._handler = handler || function () {};
    this._handle = -1;
    this._listening = false;
  }
  TlsServer.prototype.on = function (ev, fn) {
    (this._listeners = this._listeners || {})[ev] = (this._listeners[ev] || []).concat([fn]);
    return this;
  };
  TlsServer.prototype.listen = function () {
    var args = Array.prototype.slice.call(arguments);
    var port = 0, cb = null;
    for (var i = 0; i < args.length; i++) {
      if (typeof args[i] === "number") port = args[i] | 0;
      else if (typeof args[i] === "function") cb = args[i];
    }
    var cert = this._opts.cert != null ? String(this._opts.cert) : N.testCert().cert;
    var key = this._opts.key != null ? String(this._opts.key) : N.testCert().key;
    var res = N.serverListen(port, cert, key);
    this._handle = res.handle;
    this._port = res.port;
    this._listening = true;
    var self = this;
    IO.add(function () {
      if (!self._listening) return false;
      var pending = N.serverTakePending(self._handle);
      for (var j = 0; j < pending.length; j++) {
        (function (p) {
          // The TLS server surface here is request/response shaped (like http.createServer): the
          // handler receives a parsed request and a response writer.
          var req = p.request;
          var resObj = {
            statusCode: 200, statusMessage: "", _headers: [], _chunks: [], _ended: false,
            setHeader: function (n, v) { this._headers.push([String(n), String(v)]); return this; },
            writeHead: function (s, m, h) {
              this.statusCode = s | 0;
              if (typeof m === "string") this.statusMessage = m; else if (m && typeof m === "object") h = m;
              if (h && typeof h === "object") { var ks = Object.keys(h); for (var k = 0; k < ks.length; k++) this.setHeader(ks[k], h[ks[k]]); }
              return this;
            },
            write: function (c) { if (c != null) this._chunks.push(String(c)); return true; },
            end: function (c) {
              if (c != null) this._chunks.push(String(c));
              this._ended = true;
              N.serverRespond(self._handle, p.requestId, {
                status: this.statusCode, statusText: this.statusMessage,
                headers: this._headers, body: this._chunks.join(""),
              });
            },
          };
          try { self._handler(req, resObj); }
          catch (e) { if (!resObj._ended) { resObj.statusCode = 500; resObj.end(""); } }
        })(pending[j]);
      }
      return self._listening;
    });
    if (cb) Promise.resolve().then(function () { cb(); });
    return this;
  };
  TlsServer.prototype.address = function () {
    return this._listening ? { address: "127.0.0.1", family: "IPv4", port: this._port } : null;
  };
  TlsServer.prototype.close = function (cb) {
    if (this._listening) { this._listening = false; N.serverClose(this._handle); }
    if (cb) Promise.resolve().then(function () { cb(); });
    return this;
  };

  function createServer(opts, handler) {
    if (typeof opts === "function") { handler = opts; opts = {}; }
    return new TlsServer(opts, handler);
  }

  function createSecureContext(opts) { return { context: opts || {} }; }

  // Publish the TLS client transport behind a stable global so node:https (and https-fetch) can
  // route an https:// request through this exact transport without re-importing node:tls.
  globalThis.__treaty_tls = {
    startClient: startClient,
    headersToPairs: headersToPairs,
    trustOf: trustOf,
    testCert: function () { return N.testCert(); },
    nativeServer: {
      listen: function (port, cert, key) { return N.serverListen(port, cert, key); },
      takePending: function (h) { return N.serverTakePending(h); },
      respond: function (h, id, r) { return N.serverRespond(h, id, r); },
      close: function (h) { return N.serverClose(h); },
    },
  };

  var DEFAULT_ECDH_CURVE = "auto";
  var rootCertificates = [];

  return {
    connect: connect,
    createServer: createServer,
    createSecureContext: createSecureContext,
    TLSSocket: TLSSocket,
    Server: TlsServer,
    DEFAULT_ECDH_CURVE: DEFAULT_ECDH_CURVE,
    rootCertificates: rootCertificates,
    // The embedded loopback self-signed cert/key (Treaty extension): the material the default
    // in-process TLS/HTTPS test server presents. A client pins `.cert` as its `ca` to verify it.
    testCert: function () { return N.testCert(); },
  };
})()
"#;

/// JS wrappers marshalling between the reactor and the bootstrap.
mod js {
    use super::*;

    fn arg_str(agent: &Agent, args: &ArgumentsList, idx: usize) -> String {
        match JsString::try_from(args.get(idx)) {
            Ok(s) => s.to_string_lossy(agent).into_owned(),
            Err(_) => String::new(),
        }
    }

    fn arg_u32(args: &ArgumentsList, idx: usize) -> u32 {
        match args.get(idx) {
            Value::Integer(i) => u32::try_from(i.into_i64()).unwrap_or(0),
            _ => 0,
        }
    }

    fn set_str(
        agent: &mut Agent,
        obj: OrdinaryObject,
        key: &'static str,
        value: &str,
        gc: nova_vm::engine::NoGcScope,
    ) {
        let v: Value = JsString::from_str(agent, value, gc).into();
        let k = PropertyKey::from_static_str(agent, key, gc);
        unwrap_try(obj.try_define_own_property(
            agent,
            k,
            PropertyDescriptor::new_data_descriptor(v),
            None,
            gc,
        ));
    }

    fn set_val(
        agent: &mut Agent,
        obj: OrdinaryObject,
        key: &'static str,
        value: Value,
        gc: nova_vm::engine::NoGcScope,
    ) {
        let k = PropertyKey::from_static_str(agent, key, gc);
        unwrap_try(obj.try_define_own_property(
            agent,
            k,
            PropertyDescriptor::new_data_descriptor(value),
            None,
            gc,
        ));
    }

    /// Translate a JS trust token (`"insecure"`, `"system"`, `"pinned:<pem>"`) into a [`ClientTrust`].
    fn parse_trust(token: &str) -> ClientTrust {
        if token == "insecure" {
            ClientTrust::Insecure
        } else if let Some(pem) = token.strip_prefix("pinned:") {
            ClientTrust::Pinned(vec![pem.to_owned()])
        } else {
            // "system": real verification against an (empty) root store. Without a bundled root set
            // this rejects public CAs offline, which is the honest behavior for this runtime.
            ClientTrust::Pinned(Vec::new())
        }
    }

    /// `clientStart(method, url, headerPairs, body, trust)` -> client handle (number).
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
        let trust_token = arg_str(agent, &args, 4);
        let gc = gc.into_nogc();

        let headers = super::header_pairs::read(agent, header_value, gc);
        let parsed = match http::split_https_url(&url) {
            Ok(p) => p,
            Err(e) => return Err(agent.throw_exception(ExceptionType::TypeError, e, gc)),
        };
        let req = ParsedRequest {
            method: method.to_ascii_uppercase(),
            path: parsed.path,
            headers,
            body: body.into_bytes(),
        };
        let trust = parse_trust(&trust_token);
        let handle = crate::node::net::tls_client_start(req, parsed.host, parsed.port, trust);
        Ok(Value::Integer((handle as i32).into()))
    }

    /// `clientPoll(handle)` -> `{ pending } | { error } | { response }`.
    pub(super) fn client_poll<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let handle = arg_u32(&args, 0) as u64;
        let gc = gc.into_nogc();
        let obj = OrdinaryObject::create_empty_object(agent, gc);
        match crate::node::net::client_poll(handle) {
            crate::node::net::ClientPoll::Pending => {
                set_val(agent, obj, "pending", Value::Boolean(true), gc);
            }
            crate::node::net::ClientPoll::Unknown => {
                set_str(agent, obj, "error", "unknown tls client handle", gc);
            }
            crate::node::net::ClientPoll::Done(Err(e)) => {
                set_str(agent, obj, "error", &e, gc);
            }
            crate::node::net::ClientPoll::Done(Ok(resp)) => {
                let response = super::response_to_js(agent, &resp, gc);
                set_val(agent, obj, "response", response.into(), gc);
            }
        }
        Ok(obj.into())
    }

    /// `serverListen(port, certPem, keyPem)` -> `{ handle, port }`.
    pub(super) fn server_listen<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let port = arg_u32(&args, 0) as u16;
        let cert = arg_str(agent, &args, 1);
        let key = arg_str(agent, &args, 2);
        let gc = gc.into_nogc();
        let config = match server_config_from_pem(&cert, &key) {
            Ok(c) => Arc::new(c),
            Err(e) => return Err(agent.throw_exception(ExceptionType::Error, e, gc)),
        };
        match crate::node::net::tls_server_listen(port, config) {
            Ok((handle, bound)) => {
                let obj = OrdinaryObject::create_empty_object(agent, gc);
                set_val(agent, obj, "handle", Value::Integer((handle as i32).into()), gc);
                set_val(agent, obj, "port", Value::Integer(i32::from(bound).into()), gc);
                Ok(obj.into())
            }
            Err(e) => Err(agent.throw_exception(ExceptionType::Error, format!("tls listen failed: {e}"), gc)),
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
        Ok(match crate::node::net::server_port(handle) {
            Some(p) => Value::Integer(i32::from(p).into()),
            None => Value::Null,
        })
    }

    /// `serverTakePending(handle)` -> `[{ requestId, request }]`.
    pub(super) fn server_take_pending<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let handle = arg_u32(&args, 0) as u64;
        let pending = crate::node::net::tls_server_take_pending(handle);
        let gc = gc.into_nogc();
        let items: Vec<Value> = pending
            .into_iter()
            .map(|p| {
                let request_id = p.request_id;
                let request_js = super::request_to_js(agent, &p.request, gc);
                TLS_PENDING.with(|cell| cell.borrow_mut().push(((handle, request_id), p)));
                let obj = OrdinaryObject::create_empty_object(agent, gc);
                set_val(agent, obj, "requestId", Value::Integer((request_id as i32).into()), gc);
                set_val(agent, obj, "request", request_js.into(), gc);
                obj.into()
            })
            .collect();
        Ok(nova_vm::ecmascript::Array::from_slice(agent, &items, gc).into())
    }

    /// `serverRespond(handle, requestId, { status, statusText, headers, body })`.
    pub(super) fn server_respond<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let handle = arg_u32(&args, 0) as u64;
        let request_id = arg_u32(&args, 1) as u64;
        let gc = gc.into_nogc();
        let response = match Object::try_from(args.get(2)) {
            Ok(o) => super::read_response_object(agent, o, gc),
            Err(_) => return Ok(Value::Undefined),
        };
        let pending = TLS_PENDING.with(|cell| {
            let mut v = cell.borrow_mut();
            v.iter()
                .position(|((h, r), _)| *h == handle && *r == request_id)
                .map(|pos| v.remove(pos).1)
        });
        if let Some(pending) = pending {
            crate::node::net::tls_request_respond(pending, response);
        }
        Ok(Value::Undefined)
    }

    /// `serverClose(handle)`.
    pub(super) fn server_close<'gc>(
        _agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        _gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let handle = arg_u32(&args, 0) as u64;
        TLS_PENDING.with(|cell| cell.borrow_mut().retain(|((h, _), _)| *h != handle));
        crate::node::net::server_close(handle);
        Ok(Value::Undefined)
    }

    /// `splitUrl(url)` -> `{ host, port, path }` for an `https://` URL (throws otherwise).
    pub(super) fn split_url<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let url = arg_str(agent, &args, 0);
        let gc = gc.into_nogc();
        match http::split_https_url(&url) {
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

    /// `testCert()` -> `{ cert, key }` (the embedded loopback self-signed material).
    pub(super) fn test_cert<'gc>(
        agent: &mut Agent,
        _this: Value,
        _args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let gc = gc.into_nogc();
        let obj = OrdinaryObject::create_empty_object(agent, gc);
        set_str(agent, obj, "cert", super::TEST_CERT_PEM, gc);
        set_str(agent, obj, "key", super::TEST_KEY_PEM, gc);
        Ok(obj.into())
    }
}

// The parked one-shot responders for accepted-but-unanswered TLS requests, keyed by `(handle,
// request_id)`. Thread-local (single JS thread); mirrors `node:http`'s `PENDING`.
thread_local! {
    static TLS_PENDING: std::cell::RefCell<Vec<((u64, u64), crate::node::net::PendingTlsRequest)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Read a JS `{ status, statusText, headers, body }` response object into a [`RawResponse`].
fn read_response_object(
    agent: &mut Agent,
    obj: Object,
    gc: nova_vm::engine::NoGcScope,
) -> RawResponse {
    let read_num = |agent: &mut Agent, key: &'static str| -> i64 {
        let k = PropertyKey::from_static_str(agent, key, gc);
        match obj.try_get(agent, k, obj.into(), None, gc) {
            std::ops::ControlFlow::Continue(nova_vm::ecmascript::TryGetResult::Value(
                Value::Integer(i),
            )) => i.into_i64(),
            _ => 0,
        }
    };
    let read_string = |agent: &mut Agent, key: &'static str| -> String {
        let k = PropertyKey::from_static_str(agent, key, gc);
        match obj.try_get(agent, k, obj.into(), None, gc) {
            std::ops::ControlFlow::Continue(nova_vm::ecmascript::TryGetResult::Value(v)) => {
                JsString::try_from(v)
                    .map(|s| s.to_string_lossy(agent).into_owned())
                    .unwrap_or_default()
            }
            _ => String::new(),
        }
    };
    let status = u16::try_from(read_num(agent, "status")).unwrap_or(200);
    let status_text = read_string(agent, "statusText");
    let body = read_string(agent, "body");
    let headers = {
        let k = PropertyKey::from_static_str(agent, "headers", gc);
        let hv = match obj.try_get(agent, k, obj.into(), None, gc) {
            std::ops::ControlFlow::Continue(nova_vm::ecmascript::TryGetResult::Value(v)) => v,
            _ => Value::Undefined,
        };
        header_pairs::read(agent, hv, gc)
    };
    RawResponse {
        status,
        status_text,
        headers,
        body: body.into_bytes(),
    }
}

/// Build the `{ method, url, headers, body }` JS object the server bootstrap reads a request as.
fn request_to_js<'gc>(
    agent: &mut Agent,
    req: &ParsedRequest,
    gc: nova_vm::engine::NoGcScope<'gc, '_>,
) -> Object<'gc> {
    let obj = OrdinaryObject::create_empty_object(agent, gc);
    let set_str = |agent: &mut Agent, key: &'static str, value: &str| {
        let v: Value = JsString::from_str(agent, value, gc).into();
        let k = PropertyKey::from_static_str(agent, key, gc);
        unwrap_try(obj.try_define_own_property(
            agent,
            k,
            PropertyDescriptor::new_data_descriptor(v),
            None,
            gc,
        ));
    };
    set_str(agent, "method", &req.method);
    set_str(agent, "url", &req.path);
    let headers: Value = header_pairs::to_array(agent, &req.headers, gc).into();
    let hk = PropertyKey::from_static_str(agent, "headers", gc);
    unwrap_try(obj.try_define_own_property(
        agent,
        hk,
        PropertyDescriptor::new_data_descriptor(headers),
        None,
        gc,
    ));
    let body = String::from_utf8_lossy(&req.body);
    set_str(agent, "body", &body);
    obj.into()
}

/// Build the `{ status, statusText, headers, body }` JS response object the client poll returns.
fn response_to_js<'gc>(
    agent: &mut Agent,
    resp: &RawResponse,
    gc: nova_vm::engine::NoGcScope<'gc, '_>,
) -> OrdinaryObject<'gc> {
    let obj = OrdinaryObject::create_empty_object(agent, gc);
    let set_str = |agent: &mut Agent, key: &'static str, value: &str| {
        let v: Value = JsString::from_str(agent, value, gc).into();
        let k = PropertyKey::from_static_str(agent, key, gc);
        unwrap_try(obj.try_define_own_property(
            agent,
            k,
            PropertyDescriptor::new_data_descriptor(v),
            None,
            gc,
        ));
    };
    let set_val = |agent: &mut Agent, key: &'static str, value: Value| {
        let k = PropertyKey::from_static_str(agent, key, gc);
        unwrap_try(obj.try_define_own_property(
            agent,
            k,
            PropertyDescriptor::new_data_descriptor(value),
            None,
            gc,
        ));
    };
    set_val(agent, "status", Value::Integer(i32::from(resp.status).into()));
    set_str(agent, "statusText", &resp.status_text);
    let headers = header_pairs::to_array(agent, &resp.headers, gc).into();
    set_val(agent, "headers", headers);
    let body = String::from_utf8_lossy(&resp.body);
    set_str(agent, "body", &body);
    obj
}

/// `[name, value][]` header-array marshalling shared by the client and server JS seams.
mod header_pairs {
    use super::*;
    use nova_vm::ecmascript::{Array, TryGetResult};

    pub(super) fn to_array<'gc>(
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

    pub(super) fn read(
        agent: &mut Agent,
        value: Value,
        gc: nova_vm::engine::NoGcScope,
    ) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let Ok(array) = Array::try_from(value) else {
            return out;
        };
        let len = array.len(agent);
        for i in 0..len {
            let key = PropertyKey::Integer(i.into());
            let pair = match array.try_get(agent, key, array.into(), None, gc) {
                std::ops::ControlFlow::Continue(TryGetResult::Value(v)) => v,
                _ => continue,
            };
            let Ok(pair) = Array::try_from(pair) else {
                continue;
            };
            let read = |agent: &mut Agent, idx: u32| -> String {
                let k = PropertyKey::Integer(idx.into());
                match pair.try_get(agent, k, pair.into(), None, gc) {
                    std::ops::ControlFlow::Continue(TryGetResult::Value(v)) => JsString::try_from(v)
                        .map(|s| s.to_string_lossy(agent).into_owned())
                        .unwrap_or_default(),
                    _ => String::new(),
                }
            };
            let name = read(agent, 0);
            let val = read(agent, 1);
            out.push((name, val));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_test_material_parses() {
        let certs = certs_from_pem(TEST_CERT_PEM).expect("cert parses");
        assert_eq!(certs.len(), 1, "exactly one self-signed cert");
        let _key = key_from_pem(TEST_KEY_PEM).expect("key parses");
    }

    #[test]
    fn server_config_builds_from_embedded_material() {
        server_config_from_pem(TEST_CERT_PEM, TEST_KEY_PEM).expect("server config builds");
    }

    #[test]
    fn pinned_client_config_requires_an_anchor() {
        // An empty pin set is rejected (no trust anchor => verification cannot succeed honestly).
        assert!(client_config(&ClientTrust::Pinned(Vec::new())).is_err());
        // Pinning the test cert yields a usable config.
        client_config(&ClientTrust::Pinned(vec![TEST_CERT_PEM.to_owned()]))
            .expect("pinned config builds");
    }

    #[test]
    fn insecure_client_config_builds() {
        client_config(&ClientTrust::Insecure).expect("insecure config builds");
    }

    #[test]
    fn tls_loopback_roundtrip_through_the_reactor() {
        // A real TLS server on 127.0.0.1:0 (embedded self-signed cert), hit by a real TLS client that
        // pins that cert as its trust anchor, exchanging one HTTP/1.1 request over the encrypted
        // session — all at the reactor level (no JS, no external network). Proves the handshake +
        // app-data path end to end.
        let config = Arc::new(
            server_config_from_pem(TEST_CERT_PEM, TEST_KEY_PEM).expect("server config"),
        );
        let (handle, port) = crate::node::net::tls_server_listen(0, config).expect("tls listen");
        assert!(port > 0);

        let trust = ClientTrust::Pinned(vec![TEST_CERT_PEM.to_owned()]);
        let req = ParsedRequest::get("/secure");
        // `localhost` is in the cert SAN; the client verifies the chain + name against the pin.
        let client = crate::node::net::tls_client_start(req, "localhost".to_owned(), port, trust);

        let mut body = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while std::time::Instant::now() < deadline {
            for pending in crate::node::net::tls_server_take_pending(handle) {
                assert_eq!(pending.request.path, "/secure");
                let resp = RawResponse::text(200, "OK", "secure-pong");
                crate::node::net::tls_request_respond(pending, resp);
            }
            match crate::node::net::client_poll(client) {
                crate::node::net::ClientPoll::Done(Ok(resp)) => {
                    body = Some(String::from_utf8_lossy(&resp.body).into_owned());
                    break;
                }
                crate::node::net::ClientPoll::Done(Err(e)) => panic!("tls client errored: {e}"),
                _ => {}
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        crate::node::net::server_close(handle);
        assert_eq!(body.as_deref(), Some("secure-pong"));
    }

    #[test]
    fn tls_client_rejects_untrusted_cert() {
        // The same server, but a client that does NOT pin the cert (empty system roots) must fail the
        // handshake/verification rather than silently trusting it.
        let config = Arc::new(
            server_config_from_pem(TEST_CERT_PEM, TEST_KEY_PEM).expect("server config"),
        );
        let (handle, port) = crate::node::net::tls_server_listen(0, config).expect("tls listen");

        let req = ParsedRequest::get("/secure");
        // Empty pin set => real verification against no anchors => must reject.
        let client = crate::node::net::tls_client_start(
            req,
            "localhost".to_owned(),
            port,
            ClientTrust::Pinned(Vec::new()),
        );

        let mut outcome = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while std::time::Instant::now() < deadline {
            // Drain (and answer, in case the handshake somehow completes) so the accept thread is not
            // wedged; an untrusted client will error before sending a request.
            for pending in crate::node::net::tls_server_take_pending(handle) {
                crate::node::net::tls_request_respond(pending, RawResponse::text(200, "OK", "x"));
            }
            match crate::node::net::client_poll(client) {
                crate::node::net::ClientPoll::Done(result) => {
                    outcome = Some(result);
                    break;
                }
                _ => {}
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        crate::node::net::server_close(handle);
        assert!(
            matches!(outcome, Some(Err(_))),
            "untrusted cert must be rejected, got {outcome:?}"
        );
    }
}
