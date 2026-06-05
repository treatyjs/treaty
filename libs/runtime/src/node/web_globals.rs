//! WinterCG web globals backing module — the native primitives behind the `crypto` (Web Crypto),
//! `Blob`, `File`, `FormData`, `AbortController`/`AbortSignal`, `Event`/`EventTarget`, `performance`,
//! `btoa`/`atob` globals that Bun and Cloudflare Workers (`nodejs_compat`) expose.
//!
//! This is a **globals-only** leaf, the WinterCG analogue of `text_encoding`/`fetch`: it is wired by
//! [`crate::node::globals`] through the lazy self-replacing accessors and the hidden native-module
//! slot, NOT registered in [`crate::node::BUILTINS`] (these are not importable `node:` specifiers).
//! It exposes the uniform [`install`] seam every leaf shares so the globals bootstrap can pull its
//! native primitives the same way the URL/TextEncoder/fetch families do.
//!
//! ## Architecture
//!
//! Like `node:crypto`, the cryptographic / codec heart is a **pure-Rust core** that never touches
//! Nova ([`base64_encode`]/[`base64_decode`], [`fill_random`], [`random_uuid`], [`sha_digest`],
//! [`monotonic_millis`]). It is unit-tested in isolation. The JS-facing surface is a thin layer: a
//! handful of Rust-backed [`nova_vm::ecmascript::RegularFn`] natives marshal bytes in and out, and a
//! compile-time JS [`PRELUDE`] assembles the object the globals bootstrap reads off the hidden slot —
//! `getRandomValues`, `randomUUID`, the `subtle` namespace (`subtle.digest` resolved as a
//! `Promise<ArrayBuffer>` through the event loop), `btoa`, `atob`, and the monotonic `now`. The
//! `Blob`/`File`/`FormData`/`Event`/`EventTarget`/`AbortController`/`AbortSignal` value-level classes
//! are pure JS and live in the globals bootstrap over `btoa`/`atob`/`getRandomValues`/`now` (mirroring
//! how `node:assert`/`node:util` layer JS over native primitives).
//!
//! ## Why `getRandomValues` is a top-level native while `subtle.digest` is JS-over-native
//!
//! `getRandomValues(view)` must fill the caller's typed array **in place**, so it reads the view's
//! backing buffer directly in Rust (the same `as_mut_slice` path `node:crypto`'s `randomFillSync`
//! uses). `subtle.digest(alg, data)` must return a `Promise<ArrayBuffer>`; the pinned Nova rev exposes
//! no embedder-side `ArrayBuffer`-from-bytes constructor, so the native ([`native_subtle_digest_bytes`])
//! returns the raw digest as a plain byte `Array` and the JS prelude wraps it (`Uint8Array.from(...)
//! .buffer`, settled via `Promise.resolve`, which the event loop drains). This is the same "JS object
//! graph over Rust hot-path natives" split `node:crypto` and `node:buffer` use.
//!
//! ## Laziness
//!
//! Built at most once, on first touch of any backed global (tenet 2). Until then it costs only a lazy
//! accessor descriptor; the prelude is a `&'static str`, so an un-touched runtime pays nothing.

use std::sync::OnceLock;
use std::time::Instant;

use digest::Digest;
use sha1::Sha1;
use sha2::{Sha256, Sha384, Sha512};

use nova_vm::ecmascript::{
    Agent, Array, ArgumentsList, ArrayBuffer, ExceptionType, InternalMethods, JsError, JsResult,
    Object, OrdinaryObject, PropertyDescriptor, PropertyKey, String as JsString, TypedArray, Value,
    parse_script, script_evaluation,
};
use nova_vm::engine::{Bindable, GcScope, NoGcScope};

use crate::node::core::{InstallError, NodeCtx};
use crate::node::globals::define_fn;

// =================================================================================================
// Pure-Rust core (no Nova; unit-tested directly).
// =================================================================================================

/// A `SubtleCrypto.digest` algorithm. WHATWG names these with the canonical hyphenated forms
/// (`"SHA-256"`); Node/Bun/CF additionally tolerate the unhyphenated `"sha256"`. The set mirrors the
/// Web Crypto digest algorithms (SHA-1, SHA-256, SHA-384, SHA-512) — MD5 is intentionally absent from
/// Web Crypto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubtleAlg {
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

impl SubtleAlg {
    /// Resolve a Web Crypto algorithm name (case-insensitive, with or without the `-` separator) to a
    /// [`SubtleAlg`], or `None` if unsupported. `crypto.subtle.digest` accepts either a string or an
    /// `{ name }` object; the JS prelude flattens the object form to its `name` before calling.
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        let normalized = name.trim().to_ascii_lowercase().replace("sha-", "sha");
        match normalized.as_str() {
            "sha1" => Some(SubtleAlg::Sha1),
            "sha256" => Some(SubtleAlg::Sha256),
            "sha384" => Some(SubtleAlg::Sha384),
            "sha512" => Some(SubtleAlg::Sha512),
            _ => None,
        }
    }
}

/// One-shot Web Crypto digest. Borrows the input bytes (tenet 3); the only allocation is the
/// fixed-size output `Vec`. Pure-Rust RustCrypto, no OpenSSL.
pub(crate) fn sha_digest(alg: SubtleAlg, data: &[u8]) -> Vec<u8> {
    match alg {
        SubtleAlg::Sha1 => Sha1::digest(data).to_vec(),
        SubtleAlg::Sha256 => Sha256::digest(data).to_vec(),
        SubtleAlg::Sha384 => Sha384::digest(data).to_vec(),
        SubtleAlg::Sha512 => Sha512::digest(data).to_vec(),
    }
}

/// Fill `out` with cryptographically secure random bytes from the OS CSPRNG (`getrandom`, the same
/// source `node:crypto` draws from). On the rare platform error returns `Err`; callers surface it as a
/// JS `Error` (Web Crypto's `getRandomValues` is allowed to throw an `OperationError`).
pub(crate) fn fill_random(out: &mut [u8]) -> Result<(), getrandom::Error> {
    getrandom::getrandom(out)
}

/// Generate an RFC 4122 version-4 (random) UUID string — what `crypto.randomUUID()` returns.
///
/// 16 CSPRNG bytes with the version nibble pinned to `4` and the variant bits to `10`, formatted
/// lowercase hyphenated `8-4-4-4-12`.
pub(crate) fn random_uuid() -> Result<String, getrandom::Error> {
    let mut b = [0u8; 16];
    fill_random(&mut b)?;
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // variant 10xx
    Ok(format_uuid(&b))
}

/// Format 16 bytes as a lowercase hyphenated RFC 4122 UUID string (`8-4-4-4-12`).
fn format_uuid(b: &[u8; 16]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(36);
    for (i, byte) in b.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            s.push('-');
        }
        s.push(HEX[(byte >> 4) as usize] as char);
        s.push(HEX[(byte & 0x0f) as usize] as char);
    }
    s
}

/// Standard (RFC 4648 §4) base64 alphabet with `=` padding — the codec `btoa` produces and `atob`
/// consumes.
const B64_STD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// `btoa` core: base64-encode a "binary string" given as its raw bytes (each input character is one
/// byte — the caller has already range-checked that every code point is `<= 0xFF`). Standard base64
/// with `=` padding.
pub(crate) fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64_STD[((n >> 18) & 0x3f) as usize] as char);
        out.push(B64_STD[((n >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            B64_STD[((n >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64_STD[(n & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// `atob` core: decode standard base64 to its raw bytes.
///
/// Mirrors the WHATWG "forgiving-base64 decode" used by `atob`: ASCII whitespace is ignored, trailing
/// `=` padding is optional, and any other non-alphabet character (or an invalid length) is an error.
/// Returns `Err` so the JS wrapper can throw the `InvalidCharacterError` `atob` is specified to throw.
pub(crate) fn base64_decode(input: &str) -> Result<Vec<u8>, Base64Error> {
    // Build the reverse lookup once: byte -> 6-bit value, 0xFF for "not in alphabet".
    let mut lut = [0xFFu8; 256];
    for (i, &c) in B64_STD.iter().enumerate() {
        lut[c as usize] = i as u8;
    }

    // Strip ASCII whitespace (the only characters WHATWG forgiving-base64 ignores).
    let mut sextets: Vec<u8> = Vec::with_capacity(input.len());
    let mut padding = 0usize;
    for ch in input.bytes() {
        match ch {
            b' ' | b'\t' | b'\n' | b'\x0c' | b'\r' => continue,
            b'=' => {
                padding += 1;
                continue;
            }
            _ => {
                // Any character after padding has begun, or any non-alphabet character, is invalid.
                if padding != 0 {
                    return Err(Base64Error::InvalidCharacter);
                }
                let v = lut[ch as usize];
                if v == 0xFF {
                    return Err(Base64Error::InvalidCharacter);
                }
                sextets.push(v);
            }
        }
    }

    // A remainder of exactly one base64 character can never form a byte — invalid input.
    if sextets.len() % 4 == 1 {
        return Err(Base64Error::InvalidCharacter);
    }

    let mut out = Vec::with_capacity(sextets.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for &s in &sextets {
        acc = (acc << 6) | s as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    Ok(out)
}

/// An `atob`/forgiving-base64 decode failure. Surfaced to JS as the `InvalidCharacterError` that
/// `atob` throws on malformed input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Base64Error {
    /// The input contains a character outside the base64 alphabet, or is of an impossible length.
    InvalidCharacter,
}

/// The process-relative monotonic clock origin, captured on first read of `performance.now()`.
static MONOTONIC_ORIGIN: OnceLock<Instant> = OnceLock::new();

/// Monotonic high-resolution time in fractional milliseconds since the first call.
///
/// Backs `performance.now()`. Uses [`std::time::Instant`], which is monotonic (never goes backwards),
/// so successive readings are non-decreasing — the invariant `performance.now()` guarantees.
pub(crate) fn monotonic_millis() -> f64 {
    let origin = MONOTONIC_ORIGIN.get_or_init(Instant::now);
    origin.elapsed().as_secs_f64() * 1000.0
}

// =================================================================================================
// JS-facing wiring.
// =================================================================================================

/// Uniform per-module entry. Returns the web-globals native primitives object the globals bootstrap
/// reads off the hidden slot: `{ getRandomValues, randomUUID, subtle: { digest }, btoa, atob, now }`.
///
/// Steps mirror `node:crypto`: (1) build the natives object with the Rust-backed CSPRNG / digest /
/// base64 / clock primitives, (2) stash it on the realm global under a private key, (3) evaluate the
/// JS [`PRELUDE`] (an IIFE that assembles the public-shaped object over those natives), (4) delete the
/// private key, (5) return the assembled object.
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    mut gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    // (1) Build the natives object.
    let natives = {
        let nogc = gc.nogc();
        let natives = OrdinaryObject::create_empty_object(agent, nogc);
        define_fn(agent, natives, "getRandomValues", native_get_random_values, 1, nogc);
        define_fn(agent, natives, "randomUuid", native_random_uuid, 0, nogc);
        define_fn(agent, natives, "subtleDigestBytes", native_subtle_digest_bytes, 2, nogc);
        define_fn(agent, natives, "btoa", native_btoa, 1, nogc);
        define_fn(agent, natives, "atob", native_atob, 1, nogc);
        define_fn(agent, natives, "now", native_now, 0, nogc);
        natives
    };

    // (2) Stash on the realm global under the private key.
    {
        let nogc = gc.nogc();
        let global = agent.current_realm(nogc).global_object(agent);
        let key = PropertyKey::from_static_str(agent, NATIVES_KEY, nogc);
        let defined = global.unbind().try_define_own_property(
            agent,
            key.unbind(),
            PropertyDescriptor::new_data_descriptor(natives),
            None,
            nogc,
        );
        if defined.is_break() {
            return Err(InstallError::Nova(
                "could not stash web-globals natives on the global".to_owned(),
            ));
        }
    }

    // (3) Evaluate the prelude; its completion value is the natives-facing object.
    let exports = run_prelude(agent, gc.reborrow())?.unbind();

    // (4) Delete the private key so it never leaks to user code.
    {
        let nogc = gc.nogc();
        let global = agent.current_realm(nogc).global_object(agent);
        let key = PropertyKey::from_static_str(agent, NATIVES_KEY, nogc);
        let _ = global.unbind().try_delete(agent, key.unbind(), nogc);
    }

    Ok(exports.bind(gc.into_nogc()))
}

/// The private global key under which the Rust-backed natives are handed to the prelude. Installed
/// just before the prelude runs and deleted immediately after, so it never leaks into a user-visible
/// global. Distinct from the bootstrap's own `__treaty_native_module` slot (this is internal to the
/// leaf's own install/prelude handshake).
const NATIVES_KEY: &str = "__treaty_web_globals_natives__";

/// Parse + evaluate [`PRELUDE`] in the current realm and return its completion value as an [`Object`].
fn run_prelude<'gc>(
    agent: &mut Agent,
    mut gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    let source = JsString::from_static_str(agent, PRELUDE, gc.nogc());
    let realm = agent.current_realm(gc.nogc());
    let script = parse_script(agent, source.unbind(), realm.unbind(), true, None, gc.nogc())
        .map_err(|diags| {
            let msg = diags
                .iter()
                .map(|d| d.to_string())
                .collect::<Vec<_>>()
                .join("; ");
            InstallError::Nova(format!("web-globals prelude parse error: {msg}"))
        })?;

    let value = script_evaluation(agent, script.unbind(), gc.reborrow())
        .unbind()
        .bind(gc.nogc());
    let value = match value {
        Ok(v) => v,
        Err(err) => {
            let msg = err
                .value()
                .unbind()
                .string_repr(agent, gc.reborrow())
                .to_string_lossy(agent)
                .into_owned();
            return Err(InstallError::Nova(format!("web-globals prelude threw: {msg}")));
        }
    };

    Object::try_from(value.unbind())
        .map(|o| o.unbind().bind(gc.into_nogc()))
        .map_err(|_| InstallError::Nova("web-globals prelude did not return an object".to_owned()))
}

// ---------------------------------------------------------------------------------------------
// Rust-backed natives.
// ---------------------------------------------------------------------------------------------

fn type_error<'a>(agent: &mut Agent, msg: &'static str, gc: NoGcScope<'a, '_>) -> JsError<'a> {
    agent.throw_exception_with_static_message(ExceptionType::TypeError, msg, gc)
}

fn js_str<'gc>(agent: &mut Agent, s: &str, gc: NoGcScope<'gc, '_>) -> Value<'gc> {
    JsString::from_string(agent, s.to_owned(), gc).into()
}

/// Inspect a typed-array argument and return its backing `(buffer, byte_offset, byte_len)`.
///
/// The returned [`ArrayBuffer`] handle is `unbind`ed (it is `Copy` and carries no borrow of `agent`),
/// so callers may immediately take an exclusive `as_mut_slice(agent)`. `None` when the value is not a
/// typed array or its backing buffer is detached.
fn ta_view(agent: &Agent, value: Value) -> Option<(ArrayBuffer<'static>, usize, usize)> {
    let ta = TypedArray::try_from(value).ok()?;
    let buf = ta.get_viewed_array_buffer(agent);
    if buf.is_detached(agent) {
        return None;
    }
    let offset = ta.byte_offset(agent);
    // The pinned Nova rev exposes only `array_length` publicly (not a byte length or element size).
    // For the byte-oriented integer views `getRandomValues`/`subtle.digest` operate on
    // (`Uint8Array`/`Int8Array`/`Uint8ClampedArray`, element size 1) this is the byte length — the
    // same assumption `node:crypto`'s `randomFillSync` and `node:buffer` make. An auto-length view
    // means "to the end of the buffer".
    let len = ta
        .array_length(agent)
        .unwrap_or_else(|| buf.byte_length(agent).saturating_sub(offset));
    Some((buf.unbind(), offset, len))
}

/// Read the bytes of a `BufferSource` argument (a TypedArray/DataView view, or a bare ArrayBuffer)
/// into an owned `Vec<u8>`.
///
/// Owned (not borrowed) so the digest computation does not hold a borrow of `agent` while it later
/// re-borrows `agent` mutably to build the result. `None` when the value is neither view nor buffer
/// (the JS prelude only ever passes a `Uint8Array`, but accept any `BufferSource` defensively).
fn arg_buffer_source(agent: &Agent, value: Value) -> Option<Vec<u8>> {
    if let Some((buf, offset, len)) = ta_view(agent, value) {
        let slice = buf.as_slice(agent);
        return slice
            .get(offset..offset.saturating_add(len))
            .map(<[u8]>::to_vec);
    }
    if let Ok(buf) = ArrayBuffer::try_from(value) {
        if buf.is_detached(agent) {
            return None;
        }
        return Some(buf.as_slice(agent).to_vec());
    }
    None
}

/// Read a JS string argument into an owned `String`.
fn arg_string(agent: &Agent, args: &ArgumentsList, index: usize) -> Option<String> {
    JsString::try_from(args.get(index))
        .ok()
        .map(|s| s.to_string_lossy(agent).into_owned())
}

/// Build a JS `Array` whose elements are the given bytes (each a small integer `0..=255`).
///
/// The pinned Nova rev exposes no embedder-side slice-to-`Uint8Array` constructor, so bytes cross into
/// JS as a plain byte `Array` (exactly as `node:crypto`/`node:text_encoding` do); the prelude wraps it
/// with `Uint8Array.from`. Each element is a tagged small integer — no per-byte heap allocation.
fn bytes_to_array<'gc>(agent: &mut Agent, bytes: &[u8], gc: NoGcScope<'gc, '_>) -> Array<'gc> {
    let values: Vec<Value> = bytes.iter().map(|&b| Value::from(b)).collect();
    Array::from_slice(agent, &values, gc)
}

/// `getRandomValues(view)` — fill the integer typed array `view` with CSPRNG bytes **in place** and
/// return it. Backs `crypto.getRandomValues`. Works on the view's backing buffer directly (the same
/// in-place fill `node:crypto`'s `randomFillSync` uses), so it honours the view's byte offset/length.
fn native_get_random_values<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let view = args.get(0);
    let Some((buf, offset, len)) = ta_view(agent, view) else {
        return Err(type_error(
            agent,
            "getRandomValues expects an integer TypedArray",
            gc.into_nogc(),
        ));
    };
    // Generate into a scratch buffer (the CSPRNG borrow must not overlap the agent borrow), then copy
    // into the view's byte range. `len` is bounded by the caller's buffer.
    let mut scratch = vec![0u8; len];
    if fill_random(&mut scratch).is_err() {
        return Err(agent.throw_exception_with_static_message(
            ExceptionType::Error,
            "the OS CSPRNG failed",
            gc.into_nogc(),
        ));
    }
    {
        let dst = buf.as_mut_slice(agent);
        if let Some(region) = dst.get_mut(offset..offset + len) {
            region.copy_from_slice(&scratch);
        }
    }
    // Per spec, `getRandomValues` returns the same array it was given.
    Ok(view.unbind().bind(gc.into_nogc()))
}

/// `randomUuid() -> string` — an RFC 4122 v4 UUID. Backs `crypto.randomUUID`.
fn native_random_uuid<'gc>(
    agent: &mut Agent,
    _this: Value,
    _args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let nogc = gc.into_nogc();
    match random_uuid() {
        Ok(s) => Ok(js_str(agent, &s, nogc)),
        Err(_) => Err(agent.throw_exception_with_static_message(
            ExceptionType::Error,
            "the OS CSPRNG failed",
            nogc,
        )),
    }
}

/// `subtleDigestBytes(alg: string, data: BufferSource) -> number[]` — the raw SHA digest of `data` as
/// a JS byte array. The prelude's `subtle.digest` wraps the result in a `Uint8Array(...).buffer` and a
/// resolved Promise (settled via the event loop), yielding the `Promise<ArrayBuffer>` Web Crypto
/// specifies.
fn native_subtle_digest_bytes<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let nogc = gc.into_nogc();
    let Some(alg_name) = arg_string(agent, &args, 0) else {
        return Err(type_error(agent, "subtle.digest expects an algorithm name", nogc));
    };
    let Some(alg) = SubtleAlg::from_name(&alg_name) else {
        return Err(agent.throw_exception_with_static_message(
            ExceptionType::Error,
            "Unrecognized algorithm name",
            nogc,
        ));
    };
    let Some(data) = arg_buffer_source(agent, args.get(1)) else {
        return Err(type_error(
            agent,
            "subtle.digest expects a BufferSource of input",
            nogc,
        ));
    };
    let bytes = sha_digest(alg, &data);
    Ok(bytes_to_array(agent, &bytes, nogc).into())
}

/// `btoa(binaryString) -> string` — base64-encode a binary string. Each input code unit must be a
/// byte (`<= 0xFF`); a code unit outside Latin-1 throws (matching `btoa`'s `InvalidCharacterError`).
fn native_btoa<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let nogc = gc.into_nogc();
    let Some(input) = arg_string(agent, &args, 0) else {
        return Err(type_error(agent, "btoa expects a string", nogc));
    };
    // Each UTF-16 code unit must fit in a byte. Iterate code units (not Rust chars): a char above
    // U+00FF is an out-of-range code unit and so is any UTF-16 surrogate.
    let mut bytes = Vec::with_capacity(input.len());
    for unit in input.encode_utf16() {
        if unit > 0xFF {
            return Err(agent.throw_exception_with_static_message(
                ExceptionType::Error,
                "The string to be encoded contains characters outside of the Latin1 range",
                nogc,
            ));
        }
        bytes.push(unit as u8);
    }
    let encoded = base64_encode(&bytes);
    Ok(js_str(agent, &encoded, nogc))
}

/// `atob(base64) -> string` — decode base64 to a binary string (each output byte becomes one U+00xx
/// code unit). Throws on malformed input, matching `atob`'s `InvalidCharacterError`.
fn native_atob<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let nogc = gc.into_nogc();
    let Some(input) = arg_string(agent, &args, 0) else {
        return Err(type_error(agent, "atob expects a string", nogc));
    };
    match base64_decode(&input) {
        // Each decoded byte maps to the matching U+00xx code point (a Latin-1 / "binary" string).
        Ok(bytes) => {
            let binary: String = bytes.iter().map(|&b| b as char).collect();
            Ok(js_str(agent, &binary, nogc))
        }
        Err(Base64Error::InvalidCharacter) => Err(agent.throw_exception_with_static_message(
            ExceptionType::Error,
            "The string to be decoded is not correctly encoded",
            nogc,
        )),
    }
}

/// `now() -> number` — monotonic high-resolution time in fractional milliseconds. Backs
/// `performance.now()` (and seeds `performance.timeOrigin`).
fn native_now<'gc>(
    _agent: &mut Agent,
    _this: Value,
    _args: ArgumentsList,
    _gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    Ok(Value::from_f64(_agent, monotonic_millis(), _gc.into_nogc()))
}

/// The JS prelude. A `&'static str` (so an un-touched runtime pays nothing); an IIFE that reads the
/// Rust natives off the private key and assembles the object the globals bootstrap consumes:
/// `getRandomValues`, `randomUUID`, the `subtle` namespace, `btoa`, `atob`, and the monotonic `now`.
///
/// `subtle.digest` is the one method assembled here rather than handed across as a bare native,
/// because it must return a `Promise<ArrayBuffer>`: it normalizes the algorithm (string or `{ name }`)
/// and the `BufferSource`, calls the native byte-digest, and wraps the bytes as
/// `Uint8Array.from(...).buffer` resolved through `Promise.resolve` (settled by the event loop).
const PRELUDE: &str = r#"
(function () {
  var N = globalThis["__treaty_web_globals_natives__"];

  function algName(algorithm) {
    if (algorithm && typeof algorithm === "object" && algorithm.name !== undefined) {
      return String(algorithm.name);
    }
    return String(algorithm);
  }

  var subtle = {
    digest: function (algorithm, data) {
      // Normalize the input to a Uint8Array of bytes (BufferSource: ArrayBuffer or any view).
      var view;
      if (data instanceof Uint8Array) {
        view = data;
      } else if (ArrayBuffer.isView(data)) {
        view = new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
      } else if (data instanceof ArrayBuffer) {
        view = new Uint8Array(data);
      } else {
        return Promise.reject(new TypeError("subtle.digest expects a BufferSource"));
      }
      // Resolve through the microtask queue (the event loop) so the digest is delivered async, as
      // Web Crypto specifies. Any algorithm/validation error surfaces as a rejected promise.
      return Promise.resolve().then(function () {
        var bytes = N.subtleDigestBytes(algName(algorithm), view);
        return Uint8Array.from(bytes).buffer;
      });
    }
  };

  return {
    getRandomValues: N.getRandomValues,
    randomUUID: N.randomUuid,
    subtle: subtle,
    btoa: N.btoa,
    atob: N.atob,
    now: N.now
  };
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::JsRuntime;
    use serde_json::{json, Value as JsonValue};

    // ----- pure-core unit tests (no JS agent) -------------------------------------------------

    #[test]
    fn base64_encode_known_vectors() {
        // RFC 4648 examples.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(b"Man"), "TWFu");
    }

    #[test]
    fn base64_decode_known_vectors() {
        assert_eq!(base64_decode("").unwrap(), b"");
        assert_eq!(base64_decode("Zg==").unwrap(), b"f");
        assert_eq!(base64_decode("Zm8=").unwrap(), b"fo");
        assert_eq!(base64_decode("Zm9v").unwrap(), b"foo");
        assert_eq!(base64_decode("Zm9vYmFy").unwrap(), b"foobar");
        // Padding is optional in forgiving-base64.
        assert_eq!(base64_decode("Zg").unwrap(), b"f");
        // ASCII whitespace is ignored.
        assert_eq!(base64_decode("Zm9v YmFy").unwrap(), b"foobar");
        assert_eq!(base64_decode("Zm9v\nYmFy").unwrap(), b"foobar");
    }

    #[test]
    fn base64_round_trips_all_byte_values() {
        let all: Vec<u8> = (0..=255u8).collect();
        let encoded = base64_encode(&all);
        assert_eq!(base64_decode(&encoded).unwrap(), all);
    }

    #[test]
    fn base64_decode_rejects_malformed_input() {
        // A lone base64 char can never decode to a byte.
        assert_eq!(base64_decode("A"), Err(Base64Error::InvalidCharacter));
        // Characters outside the alphabet.
        assert_eq!(base64_decode("****"), Err(Base64Error::InvalidCharacter));
        // Content after padding has started.
        assert_eq!(base64_decode("Zg=A"), Err(Base64Error::InvalidCharacter));
    }

    #[test]
    fn sha256_of_abc_matches_known_hex() {
        let d = sha_digest(SubtleAlg::Sha256, b"abc");
        assert_eq!(
            d.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sha_family_digest_lengths() {
        assert_eq!(sha_digest(SubtleAlg::Sha1, b"x").len(), 20);
        assert_eq!(sha_digest(SubtleAlg::Sha256, b"x").len(), 32);
        assert_eq!(sha_digest(SubtleAlg::Sha384, b"x").len(), 48);
        assert_eq!(sha_digest(SubtleAlg::Sha512, b"x").len(), 64);
    }

    #[test]
    fn subtle_alg_name_parsing_is_lenient() {
        assert_eq!(SubtleAlg::from_name("SHA-256"), Some(SubtleAlg::Sha256));
        assert_eq!(SubtleAlg::from_name("sha256"), Some(SubtleAlg::Sha256));
        assert_eq!(SubtleAlg::from_name(" SHA-1 "), Some(SubtleAlg::Sha1));
        assert_eq!(SubtleAlg::from_name("md5"), None);
        assert_eq!(SubtleAlg::from_name("sha3-256"), None);
    }

    #[test]
    fn random_uuid_has_v4_shape() {
        let u = random_uuid().unwrap();
        assert_eq!(u.len(), 36);
        let bytes = u.as_bytes();
        for &i in &[8usize, 13, 18, 23] {
            assert_eq!(bytes[i], b'-', "expected hyphen at {i} in {u}");
        }
        assert_eq!(bytes[14], b'4', "version digit must be 4 in {u}");
        assert!(matches!(bytes[19], b'8' | b'9' | b'a' | b'b'), "variant digit in {u}");
    }

    #[test]
    fn fill_random_fills_and_uuids_are_distinct() {
        let mut buf = [0u8; 64];
        fill_random(&mut buf).unwrap();
        assert!(buf.iter().any(|&x| x != 0), "CSPRNG must not be a no-op");
        assert_ne!(random_uuid().unwrap(), random_uuid().unwrap());
    }

    #[test]
    fn monotonic_millis_is_non_decreasing() {
        let a = monotonic_millis();
        let b = monotonic_millis();
        assert!(b >= a, "monotonic clock went backwards: {a} -> {b}");
        assert!(a >= 0.0);
    }

    // ----- JS-surface integration tests (live runtime; the bootstrap shells over these natives) -

    /// Evaluate `src` in a Node-compat runtime and return its JSON value. The web-globals family is
    /// installed lazily on first touch of any member, so reading e.g. `crypto`/`Blob` here triggers
    /// the bootstrap over the primitives this module installs.
    fn run(src: &str) -> JsonValue {
        let mut rt = JsRuntime::with_node_compat();
        rt.eval(src).expect("eval should succeed")
    }

    #[test]
    fn subtle_digest_sha256_known_hex() {
        // crypto.subtle.digest returns a Promise<ArrayBuffer>; the resolved bytes are observed on a
        // global after the event-loop drain `eval` performs.
        let mut rt = JsRuntime::with_node_compat();
        let synchronous = rt
            .eval(
                "globalThis.__hash = null;\
                 crypto.subtle.digest('SHA-256', new TextEncoder().encode('abc')).then(function (buf) {\
                   var v = new Uint8Array(buf); var h = '';\
                   for (var i = 0; i < v.length; i++) h += (v[i] + 0x100).toString(16).slice(1);\
                   globalThis.__hash = h;\
                 });\
                 globalThis.__hash",
            )
            .unwrap();
        // Synchronously still null (the digest settles on a microtask).
        assert_eq!(synchronous, json!(null));
        // After the drain, the SHA-256 of "abc" (FIPS 180-2 vector).
        assert_eq!(
            rt.eval("globalThis.__hash").unwrap(),
            json!("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
    }

    #[test]
    fn get_random_values_fills_in_place_and_returns_same_array() {
        let v = run(
            "(() => { const a = new Uint8Array(16); const r = crypto.getRandomValues(a);\
               let nonzero = false; for (const x of a) if (x !== 0) nonzero = true;\
               return [r === a, a.length, nonzero]; })()",
        );
        // (Astronomically unlikely that 16 CSPRNG bytes are all zero.)
        assert_eq!(v, json!([true, 16, true]));
    }

    #[test]
    fn random_uuid_global_shape() {
        let v = run(
            "(() => { const u = crypto.randomUUID();\
               return [u.length, /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(u)]; })()",
        );
        assert_eq!(v, json!([36, true]));
    }

    #[test]
    fn btoa_and_atob_round_trip() {
        assert_eq!(run("btoa('hello')"), json!("aGVsbG8="));
        assert_eq!(run("atob('aGVsbG8=')"), json!("hello"));
        assert_eq!(run("atob(btoa('the quick brown fox'))"), json!("the quick brown fox"));
        // btoa throws on a code unit outside Latin-1.
        assert_eq!(
            run("(() => { try { btoa('\\u{1F600}'); return 'no-throw'; } catch (e) { return 'threw'; } })()"),
            json!("threw")
        );
    }

    #[test]
    fn abort_controller_fires_abort_event() {
        // AbortController.abort() must set the signal's state and dispatch an "abort" Event to a
        // registered listener.
        let v = run(
            "(() => { const c = new AbortController(); let fired = 0; let evType = null;\
               c.signal.addEventListener('abort', (e) => { fired++; evType = e.type; });\
               const before = c.signal.aborted;\
               c.abort('stop');\
               return [before, c.signal.aborted, fired, evType, c.signal.reason]; })()",
        );
        assert_eq!(v, json!([false, true, 1, "abort", "stop"]));
    }

    #[test]
    fn event_target_dispatch_and_remove() {
        let v = run(
            "(() => { const t = new EventTarget(); let n = 0; const h = () => { n++; };\
               t.addEventListener('x', h);\
               t.dispatchEvent(new Event('x'));\
               t.removeEventListener('x', h);\
               t.dispatchEvent(new Event('x'));\
               return n; })()",
        );
        assert_eq!(v, json!(1));
    }

    #[test]
    fn blob_text_round_trip_and_size_and_type() {
        // Blob.text() resolves with the concatenated parts; the result lands on a global after drain.
        let mut rt = JsRuntime::with_node_compat();
        let meta = rt
            .eval(
                "globalThis.__blobText = null;\
                 const b = new Blob(['Hello, ', 'world!'], { type: 'text/plain' });\
                 b.text().then((t) => { globalThis.__blobText = t; });\
                 [b.size, b.type]",
            )
            .unwrap();
        assert_eq!(meta, json!([13, "text/plain"]));
        assert_eq!(rt.eval("globalThis.__blobText").unwrap(), json!("Hello, world!"));
    }

    #[test]
    fn file_extends_blob_with_name_and_last_modified() {
        let v = run(
            "(() => { const f = new File(['abc'], 'a.txt', { type: 'text/plain', lastModified: 123 });\
               return [f instanceof Blob, f.name, f.size, f.type, f.lastModified]; })()",
        );
        assert_eq!(v, json!([true, "a.txt", 3, "text/plain", 123]));
    }

    #[test]
    fn form_data_get_get_all_and_delete() {
        let v = run(
            "(() => { const fd = new FormData();\
               fd.append('a', '1'); fd.append('a', '2'); fd.set('b', 'x');\
               const all = fd.getAll('a');\
               fd.delete('b');\
               return [fd.get('a'), all, fd.has('b'), fd.has('a')]; })()",
        );
        assert_eq!(v, json!(["1", ["1", "2"], false, true]));
    }

    #[test]
    fn performance_now_is_monotonic_and_has_time_origin() {
        let v = run(
            "(() => { const a = performance.now(); const b = performance.now();\
               return [typeof a === 'number', b >= a, typeof performance.timeOrigin === 'number']; })()",
        );
        assert_eq!(v, json!([true, true, true]));
    }
}
