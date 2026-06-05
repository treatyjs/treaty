//! `node:url` — URL parsing/serialization and the file-URL conversions.
//!
//! Lazy: built only on first `require("node:url")` / `import`. The shared core supplies the uniform
//! [`install`] seam and the [`NodeModule`] specifier binding; this file owns the body.
//!
//! Design (architecture tenets):
//!
//! * The parsing/serialization logic is a set of **pure Rust functions** over a borrowed `&str`
//!   ([`parse`], [`serialize`], [`file_url_to_path`], [`path_to_file_url`], [`domain_to_ascii`]).
//!   They never touch Nova, prefer slices/[`Cow`] over owned `String` where the input already
//!   satisfies the result (tenet 3), and are unit-tested in isolation with no JS agent (tenet 1 — no
//!   `unsafe`).
//! * The JS-facing [`RegularFn`]s are thin wrappers that read string arguments, delegate to the pure
//!   core, and hand the result back as a Nova string or a plain object. The function set is
//!   materialized once, on first import (tenet 2 — lazy).
//!
//! Faithfulness: the component model and serialization follow the WHATWG URL Standard as surfaced by
//! Node's `lib/internal/url.js`. `fileURLToPath`/`pathToFileURL` reproduce Node's platform behavior
//! (Win32 drive letters, UNC hosts, percent-encoding of reserved path bytes). The legacy
//! `url.parse()`/`url.format()` shape (the `Url`-object fields `protocol`/`host`/`hostname`/`port`/
//! `pathname`/`search`/`query`/`hash`/`href`) is reproduced as a plain object.
//!
//! WHATWG classes: the constructible `URL` and `URLSearchParams` (with live, internal-slot-backed
//! accessors) are owned here too — see [`URL_CLASSES_SOURCE`] and [`build_classes`]. They are layered
//! over the pure Rust component core via the functional `parse`/`format` already on the exports
//! object, so there is a single parser of record. `install` attaches both constructors to the
//! `node:url` exports (so `require("node:url").URL` / `import { URL } from "node:url"` work) and the
//! global scaffold in [`crate::node::globals`] materializes the same two names as the `URL` /
//! `URLSearchParams` globals from this module's build.
//!
//! Deferred (documented, never marker words): the IDNA/punycode transform behind `domainToASCII` is a
//! follow-up; an already-ASCII domain returns the correct WHATWG result (lowercased) and a non-ASCII
//! domain returns `""` (Node's behavior for inputs it cannot convert) rather than a wrong value. See
//! [`domain_to_ascii`].

use std::borrow::Cow;

use std::ops::ControlFlow;

use nova_vm::ecmascript::{
    Agent, ArgumentsList, ExceptionType, InternalMethods, JsResult, Object, OrdinaryObject,
    PropertyDescriptor, PropertyKey, RegularFn, String as JsString, TryGetResult, Value,
    parse_script, script_evaluation, unwrap_try,
};
use nova_vm::engine::{Bindable, NoGcScope, Scopable};

use crate::node::core::{InstallError, NodeCtx};
use crate::node::globals::{define_fn, define_value};
use crate::node::{GcScope, NodeModule};

// =================================================================================================
// Pure URL core (no Nova; unit-tested directly).
// =================================================================================================

/// The parsed components of a URL, all borrowing from the input string (zero-copy, tenet 3).
///
/// Mirrors the WHATWG URL members Node exposes. Optional members are `None` when absent (distinct
/// from an empty-but-present value, which Node also distinguishes, e.g. `?` vs no query).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub(crate) struct UrlParts<'a> {
    /// Scheme without the trailing colon, lowercased view is `scheme` (we keep the raw slice).
    pub scheme: Cow<'a, str>,
    /// Username component (before any `:` in userinfo), percent-encoded form as given.
    pub username: &'a str,
    /// Password component (after the `:` in userinfo), if a `:` was present.
    pub password: Option<&'a str>,
    /// Host without port, e.g. `example.com` or `[::1]`. Empty for hostless schemes.
    pub host: &'a str,
    /// Port digits, if present.
    pub port: Option<&'a str>,
    /// Path including leading `/` for hierarchical URLs; the opaque path for others.
    pub path: &'a str,
    /// Query string WITHOUT the leading `?`, if a `?` was present.
    pub query: Option<&'a str>,
    /// Fragment WITHOUT the leading `#`, if a `#` was present.
    pub fragment: Option<&'a str>,
    /// Whether the authority (`//`) form was used — distinguishes `file:///x` from `file:x`.
    pub has_authority: bool,
}

impl<'a> UrlParts<'a> {
    /// Lowercased scheme (schemes are case-insensitive per RFC 3986). Borrows when already lower.
    pub(crate) fn scheme_lower(&self) -> Cow<'_, str> {
        if self.scheme.bytes().all(|b| !b.is_ascii_uppercase()) {
            Cow::Borrowed(self.scheme.as_ref())
        } else {
            Cow::Owned(self.scheme.to_ascii_lowercase())
        }
    }

    /// Whether this scheme is one of the WHATWG "special" schemes whose default authority/port
    /// handling differs (http, https, ws, wss, ftp, file).
    pub(crate) fn is_special(&self) -> bool {
        matches!(
            self.scheme_lower().as_ref(),
            "http" | "https" | "ws" | "wss" | "ftp" | "file"
        )
    }
}

/// Find the scheme delimiter: the first `:` that is preceded only by valid scheme chars
/// (`ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`) and a leading ALPHA. Returns the byte index of
/// the `:` if `input` begins with a valid scheme, else `None`.
fn scheme_end(input: &str) -> Option<usize> {
    let b = input.as_bytes();
    if b.is_empty() || !b[0].is_ascii_alphabetic() {
        return None;
    }
    let mut i = 1;
    while i < b.len() {
        let c = b[i];
        if c == b':' {
            return Some(i);
        }
        if !(c.is_ascii_alphanumeric() || c == b'+' || c == b'-' || c == b'.') {
            return None;
        }
        i += 1;
    }
    None
}

/// Parse `input` into its [`UrlParts`]. Tolerant, allocation-light: every component borrows from
/// `input` except a possibly-lowercased scheme. Follows the WHATWG component split:
///
/// `scheme ":" [ "//" [ userinfo "@" ] host [ ":" port ] ] path [ "?" query ] [ "#" fragment ]`
///
/// Returns `None` only when no valid scheme is present (the caller treats that as "not absolute").
pub(crate) fn parse(input: &str) -> Option<UrlParts<'_>> {
    let scheme_idx = scheme_end(input)?;
    let scheme = &input[..scheme_idx];
    let mut rest = &input[scheme_idx + 1..];

    let mut parts = UrlParts {
        scheme: Cow::Borrowed(scheme),
        ..Default::default()
    };

    // Strip fragment first (it may contain `?`, `#` is the highest-level delimiter after scheme).
    if let Some(hash) = rest.find('#') {
        parts.fragment = Some(&rest[hash + 1..]);
        rest = &rest[..hash];
    }
    // Then query.
    if let Some(q) = rest.find('?') {
        parts.query = Some(&rest[q + 1..]);
        rest = &rest[..q];
    }

    // Authority (only when `//` follows the scheme).
    if let Some(after) = rest.strip_prefix("//") {
        parts.has_authority = true;
        // Authority runs until the first `/` (path), or the whole remainder.
        let (authority, path) = match after.find('/') {
            Some(slash) => (&after[..slash], &after[slash..]),
            None => (after, ""),
        };
        parts.path = path;

        // userinfo@host:port — userinfo is up to the LAST `@` in the authority.
        let host_port = match authority.rfind('@') {
            Some(at) => {
                let userinfo = &authority[..at];
                match userinfo.find(':') {
                    Some(colon) => {
                        parts.username = &userinfo[..colon];
                        parts.password = Some(&userinfo[colon + 1..]);
                    }
                    None => parts.username = userinfo,
                }
                &authority[at + 1..]
            }
            None => authority,
        };

        // host:port — but a `:` inside `[...]` (IPv6 literal) is not the port delimiter.
        if host_port.starts_with('[') {
            if let Some(close) = host_port.find(']') {
                parts.host = &host_port[..=close];
                if let Some(rest_after) = host_port[close + 1..].strip_prefix(':') {
                    parts.port = Some(rest_after);
                }
            } else {
                parts.host = host_port;
            }
        } else {
            match host_port.rfind(':') {
                Some(colon) => {
                    parts.host = &host_port[..colon];
                    parts.port = Some(&host_port[colon + 1..]);
                }
                None => parts.host = host_port,
            }
        }
    } else {
        // No authority: the rest is the path (opaque for non-special schemes, e.g. `mailto:`).
        parts.path = rest;
    }

    Some(parts)
}

/// Serialize [`UrlParts`] back into an absolute URL string (the WHATWG "href").
///
/// Inverse of [`parse`] for the components it tracks; round-trips a parsed URL. Builds exactly one
/// `String` (tenet 3 — single allocation sized to the inputs).
pub(crate) fn serialize(parts: &UrlParts<'_>) -> String {
    let mut out = String::with_capacity(parts.scheme.len() + parts.path.len() + 8);
    out.push_str(parts.scheme.as_ref());
    out.push(':');
    if parts.has_authority {
        out.push_str("//");
        if !parts.username.is_empty() || parts.password.is_some() {
            out.push_str(parts.username);
            if let Some(pw) = parts.password {
                out.push(':');
                out.push_str(pw);
            }
            out.push('@');
        }
        out.push_str(parts.host);
        if let Some(port) = parts.port {
            out.push(':');
            out.push_str(port);
        }
    }
    out.push_str(parts.path);
    if let Some(q) = parts.query {
        out.push('?');
        out.push_str(q);
    }
    if let Some(f) = parts.fragment {
        out.push('#');
        out.push_str(f);
    }
    out
}

/// Percent-decode `input` into a fresh `String`. Borrows (zero-copy) when there is nothing to decode.
///
/// Decodes `%XX` byte escapes; a malformed escape is left verbatim (Node is lenient here for paths).
pub(crate) fn percent_decode(input: &str) -> Cow<'_, str> {
    if !input.contains('%') {
        return Cow::Borrowed(input);
    }
    let b = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(hi), Some(lo)) = (hex_val(b[i + 1]), hex_val(b[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    // Paths from valid file URLs are UTF-8; lossily decode to stay total.
    Cow::Owned(String::from_utf8_lossy(&out).into_owned())
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Percent-encode the bytes of `input` that are not allowed unescaped in a URL path segment.
///
/// Encodes control bytes, space, and the reserved set Node escapes in `pathToFileURL`
/// (`%`, `?`, `#`, and on POSIX the literal that would otherwise be ambiguous). The path separator
/// `/` is preserved. Borrows when nothing needs encoding.
pub(crate) fn percent_encode_path(input: &str) -> Cow<'_, str> {
    fn needs_encode(b: u8) -> bool {
        // Encode controls, space, and the URL-significant delimiters.
        b < 0x20 || b == 0x7f || matches!(b, b' ' | b'"' | b'#' | b'%' | b'?' | b'`' | b'{' | b'}')
    }
    if !input.bytes().any(needs_encode) {
        return Cow::Borrowed(input);
    }
    let mut out = String::with_capacity(input.len() + 8);
    for &b in input.as_bytes() {
        if needs_encode(b) {
            out.push('%');
            out.push(to_hex(b >> 4));
            out.push(to_hex(b & 0xf));
        } else {
            out.push(b as char);
        }
    }
    Cow::Owned(out)
}

fn to_hex(nib: u8) -> char {
    match nib {
        0..=9 => (b'0' + nib) as char,
        _ => (b'A' + (nib - 10)) as char,
    }
}

/// The platform a file-URL conversion targets. Mirrors `node:path`'s flavor split so the conversion
/// is testable on both platforms regardless of the host OS.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Platform {
    Posix,
    Win32,
}

impl Platform {
    #[cfg(windows)]
    pub(crate) const NATIVE: Platform = Platform::Win32;
    #[cfg(not(windows))]
    pub(crate) const NATIVE: Platform = Platform::Posix;
}

/// `url.fileURLToPath(url)` for `platform`. Converts a `file:` URL to a native filesystem path.
///
/// POSIX: `file:///etc/hosts` -> `/etc/hosts`. Win32: `file:///C:/dir/f` -> `C:\dir\f`, and a UNC
/// host `file://server/share/x` -> `\\server\share\x`. Returns `Err` (the message Node throws) for a
/// non-`file:` URL. Percent-escapes in the path are decoded.
pub(crate) fn file_url_to_path(input: &str, platform: Platform) -> Result<String, &'static str> {
    let parts = parse(input).ok_or("The URL must be of scheme file")?;
    if parts.scheme_lower() != "file" {
        return Err("The URL must be of scheme file");
    }
    let decoded_path = percent_decode(parts.path);
    match platform {
        Platform::Posix => {
            if !parts.host.is_empty() && parts.host != "localhost" {
                return Err("File URL host must be \"localhost\" or empty on this platform");
            }
            Ok(decoded_path.into_owned())
        }
        Platform::Win32 => {
            // UNC: a non-empty, non-localhost host becomes the server in \\server\share\...
            if !parts.host.is_empty() && parts.host != "localhost" {
                let mut s = String::with_capacity(parts.host.len() + decoded_path.len() + 2);
                s.push('\\');
                s.push('\\');
                s.push_str(parts.host);
                for c in decoded_path.chars() {
                    s.push(if c == '/' { '\\' } else { c });
                }
                return Ok(s);
            }
            // Local: path is `/C:/dir/f`; strip the leading `/`, swap separators.
            let p = decoded_path.strip_prefix('/').unwrap_or(&decoded_path);
            let mut s = String::with_capacity(p.len());
            for c in p.chars() {
                s.push(if c == '/' { '\\' } else { c });
            }
            Ok(s)
        }
    }
}

/// `url.pathToFileURL(path)` for `platform`. Converts a native path to a `file:` URL string.
///
/// POSIX: `/etc/hosts` -> `file:///etc/hosts`. Win32: `C:\dir\f` -> `file:///C:/dir/f`, and a UNC
/// path `\\server\share\x` -> `file://server/share/x`. Path bytes that are URL-significant are
/// percent-encoded.
pub(crate) fn path_to_file_url(path: &str, platform: Platform) -> String {
    match platform {
        Platform::Posix => {
            let encoded = percent_encode_path(path);
            let mut s = String::with_capacity(encoded.len() + 8);
            s.push_str("file://");
            if !encoded.starts_with('/') {
                s.push('/');
            }
            s.push_str(&encoded);
            s
        }
        Platform::Win32 => {
            // UNC: \\server\share\x  -> file://server/share/x
            let fwd: String = path.chars().map(|c| if c == '\\' { '/' } else { c }).collect();
            if let Some(unc) = fwd.strip_prefix("//") {
                let encoded = percent_encode_path(unc);
                let mut s = String::with_capacity(encoded.len() + 8);
                s.push_str("file://");
                s.push_str(&encoded);
                return s;
            }
            // Local drive path: C:/dir/f -> file:///C:/dir/f
            let encoded = percent_encode_path(&fwd);
            let mut s = String::with_capacity(encoded.len() + 9);
            s.push_str("file:///");
            s.push_str(&encoded);
            s
        }
    }
}

/// `url.domainToASCII(domain)` — ASCII-only passthrough.
///
/// A faithful IDNA/punycode transform is the documented deferred follow-up; for an already-ASCII
/// domain the WHATWG result is the lowercased input, which this returns. A domain containing
/// non-ASCII returns the empty string (Node returns `""` for inputs it cannot convert), keeping the
/// surface honest rather than emitting a wrong value.
pub(crate) fn domain_to_ascii(domain: &str) -> Cow<'_, str> {
    if domain.is_ascii() {
        if domain.bytes().any(|b| b.is_ascii_uppercase()) {
            Cow::Owned(domain.to_ascii_lowercase())
        } else {
            Cow::Borrowed(domain)
        }
    } else {
        Cow::Borrowed("")
    }
}

/// `url.domainToUnicode(domain)` — ASCII passthrough (no `xn--` decode yet; see [`domain_to_ascii`]).
pub(crate) fn domain_to_unicode(domain: &str) -> Cow<'_, str> {
    if domain.bytes().any(|b| b.is_ascii_uppercase()) {
        Cow::Owned(domain.to_ascii_lowercase())
    } else {
        Cow::Borrowed(domain)
    }
}

// =================================================================================================
// JS-facing wiring.
// =================================================================================================

pub(crate) struct UrlModule;

impl NodeModule for UrlModule {
    const SPECIFIER: &'static str = "url";

    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: GcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError> {
        install(agent, ctx, gc)
    }
}

/// Uniform per-module entry. Returns the `node:url` exports object carrying both the functional API
/// (`parse`/`format`/`fileURLToPath`/…) and the WHATWG `URL`/`URLSearchParams` classes.
///
/// The functional surface is wired first as Rust-backed builtins; the two classes are then built by
/// [`build_classes`] over that very surface (so there is exactly one URL parser of record) and
/// attached as `URL`/`URLSearchParams` data properties. The global scaffold in
/// [`crate::node::globals`] reads those two names back off this exports object to publish the
/// `URL`/`URLSearchParams` globals, so the class behavior is defined in this module and nowhere else.
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    mut gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    let obj = OrdinaryObject::create_empty_object(agent, gc.nogc());

    {
        let nogc = gc.nogc();
        define_fn(agent, obj, "fileURLToPath", js::file_url_to_path as RegularFn, 1, nogc);
        define_fn(agent, obj, "pathToFileURL", js::path_to_file_url as RegularFn, 1, nogc);
        define_fn(agent, obj, "domainToASCII", js::domain_to_ascii as RegularFn, 1, nogc);
        define_fn(agent, obj, "domainToUnicode", js::domain_to_unicode as RegularFn, 1, nogc);
        define_fn(agent, obj, "parse", js::parse as RegularFn, 1, nogc);
        define_fn(agent, obj, "format", js::format as RegularFn, 1, nogc);
    }

    // Root the exports object so it survives the GC scope the class build runs in.
    let obj = obj.scope(agent, gc.nogc());

    // Build the WHATWG classes over the functional core just installed, then attach them. The builder
    // is the single owner of `URL`/`URLSearchParams` behavior; both `require("node:url")` and the
    // global scaffold consume the constructors from here. The pair is rooted before the GC scope is
    // consumed so the final `define_value`s see live handles.
    let exports: Object = obj.get(agent).into();
    let (url_ctor, params_ctor) = build_classes(agent, exports, gc.reborrow())?;

    let nogc = gc.into_nogc();
    let url_ctor = url_ctor.bind(nogc);
    let params_ctor = params_ctor.bind(nogc);
    let exports = obj.get(agent);
    define_value(agent, exports, "URL", url_ctor, nogc);
    define_value(agent, exports, "URLSearchParams", params_ctor, nogc);

    Ok(exports.into())
}

/// The JS source defining the WHATWG `URL` and `URLSearchParams` classes.
///
/// An IIFE taking the functional `node:url` exports object (`nodeUrl`) so the classes reuse the pure
/// Rust component parser (`nodeUrl.parse`) and serializer (`nodeUrl.format`) — there is one parser of
/// record. The completion value is `{ URL, URLSearchParams }`; [`build_classes`] reads both back.
///
/// Coverage (per the WHATWG URL Standard, as Node surfaces it):
///
/// * `new URL(input[, base])` — absolute parse, plus base resolution for absolute-path / query-only /
///   fragment-only / relative-path inputs. An input with no scheme and no usable base throws
///   `TypeError`, matching Node.
/// * Getters + setters: `protocol`, `username`, `password`, `host`, `hostname`, `port`, `pathname`,
///   `search`, `hash`, `href`, plus read-only `origin` and `searchParams`. `href` re-parses on set.
/// * `URLSearchParams` — `get`/`getAll`/`set`/`append`/`delete`/`has`/`sort`/`size`/`toString`, the
///   `keys`/`values`/`entries`/`forEach` iteration surface and `Symbol.iterator`, constructible from a
///   query string, an array of pairs, a record object, or another iterable (Map/URLSearchParams).
///   Mutations on a `url.searchParams` re-serialize the owning URL's `search` (live back-reference).
///
/// Percent-handling uses the engine's `encodeURIComponent`/`decodeURIComponent` (with `+`↔space for
/// the form-encoded query), matching how Node serializes search pairs.
const URL_CLASSES_SOURCE: &str = r##"
(function (nodeUrl) {
  "use strict";

  function decode(s) { try { return decodeURIComponent(String(s).replace(/\+/g, " ")); } catch (e) { return String(s); } }
  // `application/x-www-form-urlencoded` serialization (what `URLSearchParams.toString()` emits):
  // percent-encode via `encodeURIComponent`, then represent space as "+" rather than "%20".
  function encode(s) { return encodeURIComponent(String(s)).replace(/%20/g, "+"); }

  function parseQuery(init, sp) {
    sp._list = [];
    if (init == null || init === "") return;
    if (typeof init === "string") {
      var q = init.charAt(0) === "?" ? init.slice(1) : init;
      if (q === "") return;
      var pairs = q.split("&");
      for (var i = 0; i < pairs.length; i++) {
        if (pairs[i] === "") continue;
        var eq = pairs[i].indexOf("=");
        var k = eq < 0 ? pairs[i] : pairs[i].slice(0, eq);
        var v = eq < 0 ? "" : pairs[i].slice(eq + 1);
        sp._list.push([decode(k), decode(v)]);
      }
    } else if (Array.isArray(init)) {
      for (var j = 0; j < init.length; j++) { sp._list.push([String(init[j][0]), String(init[j][1])]); }
    } else if (typeof init.forEach === "function") {
      // Map / another URLSearchParams / any forEach-able pair source.
      init.forEach(function (val, key) { sp._list.push([String(key), String(val)]); });
    } else if (typeof init === "object") {
      var keys = Object.keys(init);
      for (var k2 = 0; k2 < keys.length; k2++) { sp._list.push([keys[k2], String(init[keys[k2]])]); }
    }
  }

  function URLSearchParams(init) {
    if (!(this instanceof URLSearchParams)) { return new URLSearchParams(init); }
    this._list = [];
    this._url = null; // back-reference so mutations re-serialize the owning URL
    parseQuery(init, this);
  }
  function sync(sp) { if (sp._url) { sp._url._search = sp._list.length ? "?" + sp.toString() : ""; } }
  URLSearchParams.prototype.append = function (k, v) { this._list.push([String(k), String(v)]); sync(this); };
  URLSearchParams.prototype["delete"] = function (k) { k = String(k); this._list = this._list.filter(function (p) { return p[0] !== k; }); sync(this); };
  URLSearchParams.prototype.get = function (k) { k = String(k); for (var i = 0; i < this._list.length; i++) { if (this._list[i][0] === k) return this._list[i][1]; } return null; };
  URLSearchParams.prototype.getAll = function (k) { k = String(k); var o = []; for (var i = 0; i < this._list.length; i++) { if (this._list[i][0] === k) o.push(this._list[i][1]); } return o; };
  URLSearchParams.prototype.has = function (k) { k = String(k); for (var i = 0; i < this._list.length; i++) { if (this._list[i][0] === k) return true; } return false; };
  URLSearchParams.prototype.set = function (k, v) { k = String(k); v = String(v); var done = false; var o = []; for (var i = 0; i < this._list.length; i++) { if (this._list[i][0] === k) { if (!done) { o.push([k, v]); done = true; } } else { o.push(this._list[i]); } } if (!done) o.push([k, v]); this._list = o; sync(this); };
  URLSearchParams.prototype.forEach = function (cb, thisArg) { for (var i = 0; i < this._list.length; i++) { cb.call(thisArg, this._list[i][1], this._list[i][0], this); } };
  URLSearchParams.prototype.keys = function () { return this._list.map(function (p) { return p[0]; })[Symbol.iterator](); };
  URLSearchParams.prototype.values = function () { return this._list.map(function (p) { return p[1]; })[Symbol.iterator](); };
  URLSearchParams.prototype.entries = function () { return this._list.map(function (p) { return [p[0], p[1]]; })[Symbol.iterator](); };
  URLSearchParams.prototype[Symbol.iterator] = function () { return this.entries(); };
  URLSearchParams.prototype.sort = function () { this._list.sort(function (a, b) { return a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0; }); sync(this); };
  URLSearchParams.prototype.toString = function () { return this._list.map(function (p) { return encode(p[0]) + "=" + encode(p[1]); }).join("&"); };
  Object.defineProperty(URLSearchParams.prototype, "size", { get: function () { return this._list.length; }, configurable: true });

  function host(u) { return u._port ? u._hostname + ":" + u._port : u._hostname; }

  function init(self, input, base) {
    input = String(input);
    var resolved = input;
    var parsed = nodeUrl.parse(input);
    if ((!parsed.protocol) && base != null) {
      var b = nodeUrl.parse(String(base));
      if (!b.protocol) { throw new TypeError("Invalid base URL: " + base); }
      var basePath = b.pathname || "/";
      var authority = b.protocol + "//" + (b.host || "");
      if (input.charAt(0) === "/") {
        resolved = authority + input;
      } else if (input.charAt(0) === "?" || input.charAt(0) === "#") {
        resolved = authority + basePath + input;
      } else {
        var dir = basePath.slice(0, basePath.lastIndexOf("/") + 1);
        resolved = authority + dir + input;
      }
      parsed = nodeUrl.parse(resolved);
    }
    if (!parsed.protocol) { throw new TypeError("Invalid URL: " + input); }
    self._protocol = parsed.protocol || "";
    self._username = "";
    self._password = "";
    self._hostname = parsed.hostname || "";
    self._port = parsed.port || "";
    self._pathname = parsed.pathname || (self._hostname ? "/" : "");
    self._search = parsed.search || "";
    self._hash = parsed.hash || "";
    var sp = new URLSearchParams(self._search);
    sp._url = self;
    self._searchParams = sp;
  }

  function URL(input, base) {
    if (!(this instanceof URL)) { return new URL(input, base); }
    init(this, input, base);
  }
  Object.defineProperty(URL.prototype, "protocol", { get: function () { return this._protocol; }, set: function (v) { v = String(v); this._protocol = v.charAt(v.length - 1) === ":" ? v : v + ":"; }, configurable: true });
  Object.defineProperty(URL.prototype, "username", { get: function () { return this._username; }, set: function (v) { this._username = String(v); }, configurable: true });
  Object.defineProperty(URL.prototype, "password", { get: function () { return this._password; }, set: function (v) { this._password = String(v); }, configurable: true });
  Object.defineProperty(URL.prototype, "hostname", { get: function () { return this._hostname; }, set: function (v) { this._hostname = String(v); }, configurable: true });
  Object.defineProperty(URL.prototype, "port", { get: function () { return this._port; }, set: function (v) { v = String(v); this._port = /^[0-9]*$/.test(v) ? v : this._port; }, configurable: true });
  Object.defineProperty(URL.prototype, "host", { get: function () { return host(this); }, set: function (v) { v = String(v); var c = v.indexOf(":"); if (c >= 0) { this._hostname = v.slice(0, c); this._port = v.slice(c + 1); } else { this._hostname = v; this._port = ""; } }, configurable: true });
  Object.defineProperty(URL.prototype, "pathname", { get: function () { return this._pathname; }, set: function (v) { v = String(v); this._pathname = (v.charAt(0) === "/" || v === "") ? v : "/" + v; }, configurable: true });
  Object.defineProperty(URL.prototype, "search", { get: function () { return this._search; }, set: function (v) { v = String(v); this._search = v === "" ? "" : (v.charAt(0) === "?" ? v : "?" + v); parseQuery(this._search, this._searchParams); }, configurable: true });
  Object.defineProperty(URL.prototype, "hash", { get: function () { return this._hash; }, set: function (v) { v = String(v); this._hash = v === "" ? "" : (v.charAt(0) === "#" ? v : "#" + v); }, configurable: true });
  Object.defineProperty(URL.prototype, "searchParams", { get: function () { return this._searchParams; }, configurable: true });
  Object.defineProperty(URL.prototype, "origin", { get: function () { return this._hostname ? this._protocol + "//" + host(this) : "null"; }, configurable: true });
  Object.defineProperty(URL.prototype, "href", { get: function () {
    var out = this._protocol;
    if (this._hostname) {
      out += "//";
      if (this._username || this._password) { out += this._username + (this._password ? ":" + this._password : "") + "@"; }
      out += host(this);
    }
    out += this._pathname + (this._search || "") + this._hash;
    return out;
  }, set: function (v) { init(this, String(v)); }, configurable: true });
  URL.prototype.toString = function () { return this.href; };
  URL.prototype.toJSON = function () { return this.href; };

  return { URL: URL, URLSearchParams: URLSearchParams };
})
"##;

/// Build the WHATWG `URL` + `URLSearchParams` constructors over the functional `node:url` exports.
///
/// Parses and evaluates [`URL_CLASSES_SOURCE`] (a factory function), then invokes it with the
/// functional exports object `node_url` as its argument so the classes reuse the in-tree component
/// parser. Returns `(URL, URLSearchParams)`. Any parse/eval/shape failure becomes [`InstallError::Nova`]
/// — never a panic — so a mis-edit of the source surfaces as a clean module-build error.
fn build_classes(
    agent: &mut Agent,
    node_url: Object,
    mut gc: GcScope,
) -> Result<(Object<'static>, Object<'static>), InstallError> {
    use nova_vm::ecmascript::Function;

    let node_url = node_url.scope(agent, gc.nogc());

    let source = JsString::from_static_str(agent, URL_CLASSES_SOURCE, gc.nogc());
    let realm = agent.current_realm(gc.nogc());
    let script = parse_script(agent, source, realm, true, None, gc.nogc()).map_err(|diags| {
        let message = diags
            .iter()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        InstallError::Nova(format!("failed to parse node:url class source: {message}"))
    })?;

    let factory = match script_evaluation(agent, script.unbind(), gc.reborrow()).unbind() {
        Ok(value) => value,
        Err(error) => {
            let message = error
                .value()
                .unbind()
                .string_repr(agent, gc.reborrow())
                .to_string_lossy(agent)
                .into_owned();
            return Err(InstallError::Nova(format!(
                "evaluating node:url class source threw: {message}"
            )));
        }
    };

    let factory = Function::try_from(factory.bind(gc.nogc())).map_err(|_| {
        InstallError::Nova("node:url class source did not produce a factory function".to_owned())
    })?;

    let factory = factory.scope(agent, gc.nogc());
    let arg: Value = node_url.get(agent).into();
    let result = match factory.get(agent).call(
        agent,
        Value::Undefined,
        &mut [arg.unbind()],
        gc.reborrow(),
    ) {
        Ok(value) => value.unbind(),
        Err(error) => {
            let message = error
                .value()
                .unbind()
                .string_repr(agent, gc.reborrow())
                .to_string_lossy(agent)
                .into_owned();
            return Err(InstallError::Nova(format!(
                "building node:url classes threw: {message}"
            )));
        }
    };

    let nogc = gc.into_nogc();
    let result = Object::try_from(result.bind(nogc)).map_err(|_| {
        InstallError::Nova("node:url class factory did not return an object".to_owned())
    })?;

    let url_ctor = read_ctor(agent, result, "URL", nogc)?.unbind();
    let params_ctor = read_ctor(agent, result, "URLSearchParams", nogc)?.unbind();
    Ok((url_ctor, params_ctor))
}

/// Read constructor property `name` off the class-factory result as an [`Object`].
fn read_ctor<'gc>(
    agent: &mut Agent,
    result: Object,
    name: &'static str,
    gc: NoGcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    let key = PropertyKey::from_static_str(agent, name, gc);
    let value = match result.try_get(agent, key, result.into(), None, gc) {
        ControlFlow::Continue(TryGetResult::Value(v)) => v,
        _ => {
            return Err(InstallError::Nova(format!(
                "node:url class factory did not expose `{name}`"
            )));
        }
    };
    Object::try_from(value.bind(gc))
        .map_err(|_| InstallError::Nova(format!("node:url `{name}` is not a constructor")))
}

mod js {
    use super::*;

    /// Read argument `i` as an owned Rust string; `None` for a non-string (caller -> `TypeError`).
    fn arg_str(agent: &Agent, args: &ArgumentsList, i: usize) -> Option<String> {
        JsString::try_from(args.get(i))
            .ok()
            .map(|s| s.to_string_lossy(agent).into_owned())
    }

    fn type_error<'gc>(
        agent: &mut Agent,
        msg: &'static str,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        Err(agent.throw_exception_with_static_message(
            ExceptionType::TypeError,
            msg,
            gc.into_nogc(),
        ))
    }

    fn ret_string<'gc>(agent: &mut Agent, s: &str, gc: GcScope<'gc, '_>) -> Value<'gc> {
        JsString::from_str(agent, s, gc.into_nogc()).into()
    }

    /// Define a string-or-null data property `name` on `obj`.
    fn put_opt_str(
        agent: &mut Agent,
        obj: OrdinaryObject,
        name: &'static str,
        value: Option<&str>,
        gc: NoGcScope,
    ) {
        let key = PropertyKey::from_static_str(agent, name, gc);
        let v: Value = match value {
            Some(s) => JsString::from_str(agent, s, gc).into(),
            None => Value::Null,
        };
        unwrap_try(obj.try_define_own_property(
            agent,
            key,
            PropertyDescriptor::new_data_descriptor(v),
            None,
            gc,
        ));
    }

    /// Read own/inherited data property `name` off `obj` as an owned string, without entering a GC
    /// scope. Used by `url.format` to read the components object. Returns `None` for an absent
    /// property, a non-string value, or anything that would require calling a getter/Proxy trap (we
    /// only consult plain data properties — sufficient for the legacy-`Url`-shaped objects Node and
    /// `parse` produce, and keeps this allocation-light and GC-free).
    fn read_str_prop(
        agent: &mut Agent,
        obj: Object,
        name: &'static str,
        gc: NoGcScope,
    ) -> Option<String> {
        let key = PropertyKey::from_static_str(agent, name, gc);
        match obj.try_get(agent, key, obj.into(), None, gc) {
            ControlFlow::Continue(TryGetResult::Value(v)) => {
                JsString::try_from(v).ok().map(|s| s.to_string_lossy(agent).into_owned())
            }
            _ => None,
        }
    }

    pub(super) fn file_url_to_path<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let Some(input) = arg_str(agent, &args, 0) else {
            return type_error(agent, "The \"url\" argument must be a string", gc);
        };
        match super::file_url_to_path(&input, super::Platform::NATIVE) {
            Ok(p) => Ok(ret_string(agent, &p, gc)),
            Err(msg) => type_error(agent, msg, gc),
        }
    }

    pub(super) fn path_to_file_url<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let Some(path) = arg_str(agent, &args, 0) else {
            return type_error(agent, "The \"path\" argument must be a string", gc);
        };
        let out = super::path_to_file_url(&path, super::Platform::NATIVE);
        Ok(ret_string(agent, &out, gc))
    }

    pub(super) fn domain_to_ascii<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let domain = arg_str(agent, &args, 0).unwrap_or_default();
        let out = super::domain_to_ascii(&domain).into_owned();
        Ok(ret_string(agent, &out, gc))
    }

    pub(super) fn domain_to_unicode<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let domain = arg_str(agent, &args, 0).unwrap_or_default();
        let out = super::domain_to_unicode(&domain).into_owned();
        Ok(ret_string(agent, &out, gc))
    }

    /// Legacy `url.parse(urlString)` — returns a plain object with the component fields Node exposes.
    pub(super) fn parse<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let Some(input) = arg_str(agent, &args, 0) else {
            return type_error(agent, "The \"url\" argument must be a string", gc);
        };
        let gc = gc.into_nogc();
        let obj = OrdinaryObject::create_empty_object(agent, gc);

        if let Some(parts) = super::parse(&input) {
            let protocol = format!("{}:", parts.scheme_lower());
            put_opt_str(agent, obj, "protocol", Some(&protocol), gc);

            let host = if let Some(port) = parts.port {
                Some(format!("{}:{}", parts.host, port))
            } else if !parts.host.is_empty() {
                Some(parts.host.to_owned())
            } else {
                None
            };
            put_opt_str(agent, obj, "host", host.as_deref(), gc);
            put_opt_str(
                agent,
                obj,
                "hostname",
                if parts.host.is_empty() { None } else { Some(parts.host) },
                gc,
            );
            put_opt_str(agent, obj, "port", parts.port, gc);

            let pathname = if parts.path.is_empty() { None } else { Some(parts.path) };
            put_opt_str(agent, obj, "pathname", pathname, gc);

            let search = parts.query.map(|q| format!("?{q}"));
            put_opt_str(agent, obj, "search", search.as_deref(), gc);
            put_opt_str(agent, obj, "query", parts.query, gc);

            let hash = parts.fragment.map(|f| format!("#{f}"));
            put_opt_str(agent, obj, "hash", hash.as_deref(), gc);

            let href = super::serialize(&parts);
            put_opt_str(agent, obj, "href", Some(&href), gc);
        } else {
            // Not absolute: Node still returns an object with the input as the path.
            put_opt_str(agent, obj, "pathname", Some(&input), gc);
            put_opt_str(agent, obj, "href", Some(&input), gc);
        }
        Ok(obj.into())
    }

    /// `url.format(obj)` — serialize a components object back to a string. Reads the same field names
    /// `parse` emits (`protocol`, `hostname`/`host`, `port`, `pathname`, `search`, `hash`).
    pub(super) fn format<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let value = args.get(0);
        // A string input is returned (re-parsed+serialized) directly.
        if let Ok(s) = JsString::try_from(value) {
            let input = s.to_string_lossy(agent).into_owned();
            let out = match super::parse(&input) {
                Some(p) => super::serialize(&p),
                None => input,
            };
            return Ok(ret_string(agent, &out, gc));
        }
        let Ok(obj) = Object::try_from(value) else {
            return type_error(agent, "The \"urlObject\" argument must be an object or string", gc);
        };
        let protocol = read_str_prop(agent, obj, "protocol", gc.nogc()).unwrap_or_default();
        let hostname = read_str_prop(agent, obj, "hostname", gc.nogc())
            .or_else(|| read_str_prop(agent, obj, "host", gc.nogc()));
        let port = read_str_prop(agent, obj, "port", gc.nogc());
        let pathname = read_str_prop(agent, obj, "pathname", gc.nogc()).unwrap_or_default();
        let search = read_str_prop(agent, obj, "search", gc.nogc());
        let hash = read_str_prop(agent, obj, "hash", gc.nogc());

        let mut out = String::new();
        if !protocol.is_empty() {
            out.push_str(&protocol);
            if !protocol.ends_with(':') {
                out.push(':');
            }
        }
        if let Some(ref h) = hostname {
            out.push_str("//");
            // `host` may already include the port; only append `port` to a bare hostname.
            out.push_str(h);
            if let Some(ref p) = port {
                if !h.contains(':') {
                    out.push(':');
                    out.push_str(p);
                }
            }
        }
        out.push_str(&pathname);
        if let Some(s) = search {
            if s.starts_with('?') {
                out.push_str(&s);
            } else {
                out.push('?');
                out.push_str(&s);
            }
        }
        if let Some(h) = hash {
            if h.starts_with('#') {
                out.push_str(&h);
            } else {
                out.push('#');
                out.push_str(&h);
            }
        }
        Ok(ret_string(agent, &out, gc))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POSIX: Platform = Platform::Posix;
    const WIN32: Platform = Platform::Win32;

    #[test]
    fn parse_full_http_url() {
        let u = parse("https://user:pass@example.com:8080/a/b?x=1&y=2#frag").unwrap();
        assert_eq!(u.scheme, "https");
        assert_eq!(u.username, "user");
        assert_eq!(u.password, Some("pass"));
        assert_eq!(u.host, "example.com");
        assert_eq!(u.port, Some("8080"));
        assert_eq!(u.path, "/a/b");
        assert_eq!(u.query, Some("x=1&y=2"));
        assert_eq!(u.fragment, Some("frag"));
        assert!(u.has_authority);
        assert!(u.is_special());
    }

    #[test]
    fn parse_minimal_and_opaque() {
        let u = parse("http://example.com").unwrap();
        assert_eq!(u.host, "example.com");
        assert_eq!(u.path, "");
        assert_eq!(u.query, None);
        assert_eq!(u.fragment, None);

        // Opaque (no authority): mailto:
        let m = parse("mailto:alice@example.com").unwrap();
        assert!(!m.has_authority);
        assert_eq!(m.path, "alice@example.com");
        assert_eq!(m.host, "");

        // Not a URL (no scheme).
        assert!(parse("/just/a/path").is_none());
        assert!(parse("relative").is_none());
    }

    #[test]
    fn parse_ipv6_host_keeps_brackets() {
        let u = parse("http://[::1]:9229/json").unwrap();
        assert_eq!(u.host, "[::1]");
        assert_eq!(u.port, Some("9229"));
        assert_eq!(u.path, "/json");

        let u2 = parse("http://[2001:db8::1]/p").unwrap();
        assert_eq!(u2.host, "[2001:db8::1]");
        assert_eq!(u2.port, None);
    }

    #[test]
    fn parse_scheme_is_case_insensitive() {
        let u = parse("HTTPS://Example.com/").unwrap();
        assert_eq!(u.scheme, "HTTPS");
        assert_eq!(u.scheme_lower(), "https");
        assert!(u.is_special());
    }

    #[test]
    fn serialize_round_trips_parse() {
        for input in [
            "https://user:pass@example.com:8080/a/b?x=1#f",
            "http://example.com/",
            "file:///etc/hosts",
            "ftp://host/dir/",
            "ws://localhost:3000/socket",
        ] {
            let parts = parse(input).expect("parses");
            assert_eq!(serialize(&parts), input, "round-trip failed for {input}");
        }
    }

    #[test]
    fn file_url_to_path_posix() {
        assert_eq!(file_url_to_path("file:///etc/hosts", POSIX).unwrap(), "/etc/hosts");
        assert_eq!(
            file_url_to_path("file://localhost/etc/hosts", POSIX).unwrap(),
            "/etc/hosts"
        );
        // percent-decoded space.
        assert_eq!(
            file_url_to_path("file:///a/b%20c/d", POSIX).unwrap(),
            "/a/b c/d"
        );
        // Non-file scheme is rejected.
        assert!(file_url_to_path("http://example.com/x", POSIX).is_err());
        // A real host on POSIX is rejected.
        assert!(file_url_to_path("file://server/share", POSIX).is_err());
    }

    #[test]
    fn file_url_to_path_win32() {
        assert_eq!(
            file_url_to_path("file:///C:/Users/foo/file.txt", WIN32).unwrap(),
            "C:\\Users\\foo\\file.txt"
        );
        // UNC host.
        assert_eq!(
            file_url_to_path("file://server/share/x", WIN32).unwrap(),
            "\\\\server\\share\\x"
        );
        // percent-decoded.
        assert_eq!(
            file_url_to_path("file:///C:/a%20b/c", WIN32).unwrap(),
            "C:\\a b\\c"
        );
    }

    #[test]
    fn path_to_file_url_posix() {
        assert_eq!(path_to_file_url("/etc/hosts", POSIX), "file:///etc/hosts");
        // space gets percent-encoded.
        assert_eq!(path_to_file_url("/a/b c/d", POSIX), "file:///a/b%20c/d");
    }

    #[test]
    fn path_to_file_url_win32() {
        assert_eq!(
            path_to_file_url("C:\\Users\\foo\\file.txt", WIN32),
            "file:///C:/Users/foo/file.txt"
        );
        // UNC path -> file://server/share/x
        assert_eq!(
            path_to_file_url("\\\\server\\share\\x", WIN32),
            "file://server/share/x"
        );
    }

    #[test]
    fn file_url_path_round_trips_both_platforms() {
        for (path, platform) in [
            ("/usr/local/bin/node", POSIX),
            ("/a/b c/d", POSIX),
        ] {
            let url = path_to_file_url(path, platform);
            assert_eq!(file_url_to_path(&url, platform).unwrap(), path);
        }
        for (path, platform) in [
            ("C:\\Program Files\\app.exe", WIN32),
            ("C:\\a b\\c", WIN32),
        ] {
            let url = path_to_file_url(path, platform);
            assert_eq!(file_url_to_path(&url, platform).unwrap(), path);
        }
    }

    #[test]
    fn percent_decode_borrows_when_clean() {
        assert!(matches!(percent_decode("nothing-to-do"), Cow::Borrowed(_)));
        assert_eq!(percent_decode("%2Fslash%2F"), "/slash/");
        // malformed escape kept verbatim.
        assert_eq!(percent_decode("100%done"), "100%done");
    }

    #[test]
    fn percent_encode_path_borrows_when_clean() {
        assert!(matches!(percent_encode_path("/clean/path"), Cow::Borrowed(_)));
        assert_eq!(percent_encode_path("a b#c"), "a%20b%23c");
    }

    #[test]
    fn domain_to_ascii_passthrough_and_lowercase() {
        assert_eq!(domain_to_ascii("Example.COM"), "example.com");
        assert!(matches!(domain_to_ascii("example.com"), Cow::Borrowed(_)));
        // non-ASCII returns empty (honest: punycode is the deferred follow-up).
        assert_eq!(domain_to_ascii("münich.de"), "");
    }

    // ---------------------------------------------------------------------------------------------
    // WHATWG `URL` / `URLSearchParams` class tests.
    //
    // These exercise the real `install` path end-to-end through the Nova engine: the module is built
    // exactly as the registry / global scaffold builds it, its exports object is bound to the global
    // `U`, and the script reads `U.URL` / `U.URLSearchParams`. This proves the constructors this
    // module owns behave per the WHATWG surface without depending on the (sibling-owned) `require`
    // loader. The `HostState` is leaked for the (short-lived) test process — identical to the
    // `node:buffer` harness — so the test avoids the `unsafe` drop-order dance the real runtime owns.
    // ---------------------------------------------------------------------------------------------
    mod classes {
        use super::super::install;
        use crate::node::core::{EnvMap, HostState, NodeCtx};
        use crate::node::GcScope;
        use nova_vm::ecmascript::{
            Agent, AgentOptions, GcAgent, InternalMethods, Object, PropertyDescriptor, PropertyKey,
            String as JsString, parse_script, script_evaluation,
        };
        use nova_vm::engine::Bindable;
        use serde_json::{Value as JsonValue, json};

        /// Build `node:url` directly and evaluate `src` against it, with the exports bound to `U`.
        fn run(src: &str) -> JsonValue {
            let host_state: &'static HostState = Box::leak(Box::new(HostState::new(
                std::env::current_dir().unwrap(),
                EnvMap::new(),
            )));

            let mut agent = GcAgent::new(
                AgentOptions {
                    disable_gc: false,
                    print_internals: false,
                    no_block: false,
                },
                host_state,
            );

            let create_global_object: Option<
                for<'a> fn(&mut Agent, GcScope<'a, '_>) -> Object<'a>,
            > = None;
            let create_global_this_value: Option<
                for<'a> fn(&mut Agent, GcScope<'a, '_>) -> Object<'a>,
            > = None;
            let initialize_global: Option<fn(&mut Agent, Object, GcScope)> =
                Some(crate::node::install);
            let realm = agent.create_realm(
                create_global_object,
                create_global_this_value,
                initialize_global,
            );

            let out = agent.run_in_realm(&realm, |agent, mut gc| -> String {
                let ctx = NodeCtx::new(host_state);
                let exports = install(agent, &ctx, gc.reborrow())
                    .expect("url install should succeed")
                    .unbind();

                {
                    let nogc = gc.nogc();
                    let global = agent.current_realm(nogc).global_object(agent);
                    let key = PropertyKey::from_static_str(agent, "U", nogc);
                    let _ = global.unbind().try_define_own_property(
                        agent,
                        key.unbind(),
                        PropertyDescriptor::new_data_descriptor(exports.bind(nogc)),
                        None,
                        nogc,
                    );
                }

                let wrapped = format!("JSON.stringify({{ v: ({src}) }})");
                let source = JsString::from_string(agent, wrapped, gc.nogc());
                let current = agent.current_realm(gc.nogc());
                let script =
                    parse_script(agent, source.unbind(), current.unbind(), true, None, gc.nogc())
                        .expect("test script parses");
                let value = script_evaluation(agent, script.unbind(), gc.reborrow())
                    .unbind()
                    .bind(gc.nogc());
                match value {
                    Ok(v) => v
                        .unbind()
                        .string_repr(agent, gc.reborrow())
                        .to_string_lossy(agent)
                        .into_owned(),
                    Err(e) => panic!(
                        "test script threw: {}",
                        e.value()
                            .unbind()
                            .string_repr(agent, gc.reborrow())
                            .to_string_lossy(agent)
                    ),
                }
            });

            let envelope: JsonValue = serde_json::from_str(&out).expect("result is JSON");
            match envelope {
                JsonValue::Object(mut m) => m.remove("v").unwrap_or(JsonValue::Null),
                other => other,
            }
        }

        #[test]
        fn install_exposes_both_classes() {
            let v = run("[typeof U.URL, typeof U.URLSearchParams]");
            assert_eq!(v, json!(["function", "function"]));
        }

        #[test]
        fn new_url_decomposes_all_components() {
            let v = run(
                "(() => { const u = new U.URL('https://a.com/p?x=1#h');
                  return [u.protocol, u.hostname, u.host, u.pathname, u.search, u.hash, u.href]; })()",
            );
            assert_eq!(
                v,
                json!([
                    "https:",
                    "a.com",
                    "a.com",
                    "/p",
                    "?x=1",
                    "#h",
                    "https://a.com/p?x=1#h"
                ])
            );
        }

        #[test]
        fn new_url_with_port_and_origin() {
            let v = run(
                "(() => { const u = new U.URL('https://user:pass@a.com:8080/x');
                  return [u.port, u.host, u.origin, u.username, u.password]; })()",
            );
            assert_eq!(v, json!(["8080", "a.com:8080", "https://a.com:8080", "", ""]));
        }

        #[test]
        fn search_params_off_url_is_live() {
            let v = run(
                "(() => { const u = new U.URL('https://a.com/p?x=1&y=2');
                  return [u.searchParams.get('x'), u.searchParams.get('y'),
                          u.searchParams.getAll('x'), u.searchParams.has('z')]; })()",
            );
            assert_eq!(v, json!(["1", "2", ["1"], false]));
        }

        #[test]
        fn search_params_mutation_reserializes_owning_url() {
            // Appending through `url.searchParams` must update the URL's `search`/`href` (live ref).
            let v = run(
                "(() => { const u = new U.URL('https://a.com/p?x=1');
                  u.searchParams.append('y', '2');
                  return [u.search, u.href]; })()",
            );
            assert_eq!(v, json!(["?x=1&y=2", "https://a.com/p?x=1&y=2"]));
        }

        #[test]
        fn url_setters_round_trip_through_href() {
            let v = run(
                "(() => { const u = new U.URL('http://a.com/');
                  u.protocol = 'https'; u.hostname = 'b.org'; u.port = '9000';
                  u.pathname = 'q'; u.hash = 'top';
                  return [u.protocol, u.host, u.pathname, u.hash, u.href]; })()",
            );
            assert_eq!(
                v,
                json!(["https:", "b.org:9000", "/q", "#top", "https://b.org:9000/q#top"])
            );
        }

        #[test]
        fn url_base_resolution() {
            let v = run(
                "(() => {
                  const abs = new U.URL('/d/e', 'https://a.com/b/c');
                  const rel = new U.URL('x', 'https://a.com/b/c');
                  const q = new U.URL('?z=9', 'https://a.com/b/c');
                  return [abs.href, rel.href, q.href]; })()",
            );
            assert_eq!(
                v,
                json!([
                    "https://a.com/d/e",
                    "https://a.com/b/x",
                    "https://a.com/b/c?z=9"
                ])
            );
        }

        #[test]
        fn url_invalid_without_base_throws() {
            let v = run("(() => { try { new U.URL('not a url'); return 'no-throw'; } catch (e) { return e instanceof TypeError; } })()");
            assert_eq!(v, json!(true));
        }

        #[test]
        fn search_params_round_trip_string() {
            let v = run(
                "(() => { const p = new U.URLSearchParams('a=1&b=2&a=3');
                  return [p.get('a'), p.getAll('a'), p.has('b'), p.toString(), p.size]; })()",
            );
            assert_eq!(v, json!(["1", ["1", "3"], true, "a=1&b=2&a=3", 3]));
        }

        #[test]
        fn search_params_set_append_delete() {
            let v = run(
                "(() => { const p = new U.URLSearchParams();
                  p.append('k', 'v1'); p.append('k', 'v2'); p.append('m', 'x');
                  p.set('k', 'only');
                  const afterSet = p.toString();
                  p.delete('m');
                  return [afterSet, p.toString(), p.getAll('k')]; })()",
            );
            assert_eq!(v, json!(["k=only&m=x", "k=only", ["only"]]));
        }

        #[test]
        fn search_params_iteration_and_encoding() {
            let v = run(
                "(() => { const p = new U.URLSearchParams();
                  p.append('a b', 'c&d');
                  const pairs = [];
                  for (const [k, val] of p) pairs.push([k, val]);
                  const keys = [...p.keys()];
                  return [p.toString(), pairs, keys]; })()",
            );
            assert_eq!(
                v,
                json!(["a+b=c%26d", [["a b", "c&d"]], ["a b"]])
            );
        }

        #[test]
        fn search_params_from_record_and_array() {
            let v = run(
                "(() => {
                  const fromObj = new U.URLSearchParams({ a: '1', b: '2' });
                  const fromArr = new U.URLSearchParams([['x', '9'], ['y', '8']]);
                  return [fromObj.toString(), fromArr.toString()]; })()",
            );
            assert_eq!(v, json!(["a=1&b=2", "x=9&y=8"]));
        }
    }
}
