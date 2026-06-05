//! `node:buffer` — Node's `Buffer` (a `Uint8Array` subclass) and its encodings.
//!
//! ## Architecture
//!
//! Constructing a real `Uint8Array` from Rust is not possible on this Nova rev: every typed-array
//! allocation helper (`typed_array_create`, `allocate_typed_array`, …) is `pub(crate)`. So the
//! object graph (`Buffer extends Uint8Array`, its prototype methods, the `Blob`/`atob`/`btoa`
//! surface) is built once by a small **JS prelude** that uses the JS-visible `Uint8Array`
//! constructor — exactly how Node layers `Buffer` over `Uint8Array`.
//!
//! The *hot paths* — base64/hex/utf8/latin1 transcoding and byte comparison — are **Rust-backed**
//! ([`RegularFn`]s installed by [`install`]). They read and write the buffer's bytes **zero-copy**
//! through Nova's public [`ArrayBuffer::as_slice`] / [`ArrayBuffer::as_mut_slice`] over the typed
//! array's `[[ViewedArrayBuffer]]` + `[[ByteOffset]]` + `[[ArrayLength]]` (tenet 3: no intermediate
//! `Vec` for the bytes; the only allocation is the destination `String`, which is unavoidable when
//! crossing back into JS). There is no `unsafe` here — the single FFI `unsafe` lives in `core.rs`.
//!
//! ## Laziness
//!
//! This module is built at most once, on the first `require("node:buffer")` / `import` (tenet 2).
//! Until then it costs one `&'static str` table entry. The prelude string is a compile-time
//! `&'static str`, so an un-imported runtime pays nothing for it.
//!
//! ## What is implemented vs. deferred
//!
//! Implemented faithfully: `Buffer.alloc/allocUnsafe/from(array|string|buffer|arraybuffer)`,
//! `Buffer.concat`, `Buffer.byteLength`, `Buffer.isBuffer`, `Buffer.compare`, `.toString(enc)`,
//! `.write(str, enc)`, `.equals`, `.compare`, `.slice`/`.subarray`, `.fill`, `.copy`, `.indexOf`,
//! `.includes`, fixed-width int read/write (`readUInt8`/`readUInt16LE/BE`/`readUInt32LE/BE`,
//! `readInt*`, and the matching writers) via a `DataView`, plus the module-level `kMaxLength`,
//! `constants`, and `SlowBuffer`. Encodings: `utf8`/`utf-8`, `hex`, `base64`, `base64url`,
//! `latin1`/`binary`, `ascii`, `ucs2`/`utf16le`. `atob`/`btoa` are also exported.
//!
//! Deferred (documented, not stubbed with markers): BigInt64 read/write helpers, `Buffer.transcode`
//! (needs ICU), the legacy `SlowBuffer` pool semantics (we alias `Buffer.alloc`), and `Blob`/`File`
//! (those belong to a future `node:buffer` Web-interop pass). These throw a clear `TypeError` only
//! if called, so the common Buffer surface is fully usable.

use nova_vm::ecmascript::{
    Agent, ArgumentsList, ArrayBuffer, ExceptionType, InternalMethods, JsError, JsResult, Object,
    OrdinaryObject, PropertyDescriptor, PropertyKey, String as JsString, TypedArray, Value,
    parse_script, script_evaluation,
};
use nova_vm::engine::{Bindable, GcScope, NoGcScope};

use crate::node::core::{InstallError, NodeCtx};
use crate::node::globals::define_fn;
use crate::node::NodeModule;

/// Zero-sized marker for the `node:buffer` builtin.
pub(crate) struct BufferModule;

impl NodeModule for BufferModule {
    const SPECIFIER: &'static str = "buffer";

    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: GcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError> {
        install(agent, ctx, gc)
    }
}

/// The private global key under which the Rust-backed codec natives are handed to the prelude.
///
/// Installed just before the prelude runs and deleted immediately after, so it never leaks into a
/// user-visible global. Chosen to be collision-proof with any real Node/user global.
const NATIVES_KEY: &str = "__treaty_buffer_natives__";

/// Uniform per-module entry. Returns the `node:buffer` exports object.
///
/// Steps: (1) build the natives object with the Rust-backed codecs, (2) stash it on the realm
/// global under a private key, (3) evaluate the JS prelude (an IIFE that builds and returns the
/// exports object using the JS `Uint8Array` constructor and the stashed natives), (4) delete the
/// private key, (5) return the exports object.
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    mut gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    // (1) Build the natives object. `define_fn` interns each name without a heap String (tenet 3).
    let natives = {
        let nogc = gc.nogc();
        let natives = OrdinaryObject::create_empty_object(agent, nogc);
        define_fn(agent, natives, "hexEncode", native_hex_encode, 1, nogc);
        define_fn(agent, natives, "hexDecode", native_hex_decode, 2, nogc);
        define_fn(agent, natives, "b64Encode", native_b64_encode, 2, nogc);
        define_fn(agent, natives, "b64Decode", native_b64_decode, 3, nogc);
        define_fn(agent, natives, "utf8Decode", native_utf8_decode, 1, nogc);
        define_fn(agent, natives, "utf8Write", native_utf8_write, 4, nogc);
        define_fn(agent, natives, "utf8ByteLen", native_utf8_byte_len, 1, nogc);
        define_fn(agent, natives, "latin1Decode", native_latin1_decode, 1, nogc);
        define_fn(agent, natives, "latin1Write", native_latin1_write, 4, nogc);
        define_fn(agent, natives, "cmp", native_cmp, 2, nogc);
        natives
    };

    // (2) Stash on the realm global under the private key.
    let global = agent.current_realm(gc.nogc()).global_object(agent);
    {
        let nogc = gc.nogc();
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
                "could not stash buffer natives on the global".to_owned(),
            ));
        }
    }

    // (3) Evaluate the prelude. It is an IIFE whose completion value is the exports object. Unbind
    // it immediately so the cleanup borrow of `gc` below is independent (the handle stays valid: it
    // is a rooted heap object until GC, and nothing here can collect it).
    let exports = run_prelude(agent, gc.reborrow())?.unbind();

    // (4) Delete the private key so it never leaks to user code. Best-effort: a failure here does
    // not invalidate the exports object, and the key is non-enumerable-noise at worst.
    {
        let nogc = gc.nogc();
        let global = agent.current_realm(nogc).global_object(agent);
        let key = PropertyKey::from_static_str(agent, NATIVES_KEY, nogc);
        let _ = global.unbind().try_delete(agent, key.unbind(), nogc);
    }

    Ok(exports.bind(gc.into_nogc()))
}

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
            InstallError::Nova(format!("buffer prelude parse error: {msg}"))
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
            return Err(InstallError::Nova(format!("buffer prelude threw: {msg}")));
        }
    };

    Object::try_from(value.unbind())
        .map(|o| o.unbind().bind(gc.into_nogc()))
        .map_err(|_| InstallError::Nova("buffer prelude did not return an object".to_owned()))
}

// ---------------------------------------------------------------------------------------------
// Rust-backed codec natives. Each is a `RegularFn`. They read/write typed-array bytes zero-copy.
// ---------------------------------------------------------------------------------------------

/// Inspect a typed array argument and return its backing `(buffer, byte_offset, len)`.
///
/// The returned [`ArrayBuffer`] handle is an `unbind`ed heap index (it is `Copy` and carries no
/// borrow of `agent`), so callers may immediately take an exclusive `as_mut_slice(agent)` afterward.
/// `None` when the value is not a typed array or its backing buffer is detached.
fn ta_view(agent: &Agent, value: Value) -> Option<(ArrayBuffer<'static>, usize, usize)> {
    let ta = TypedArray::try_from(value).ok()?;
    let buf = ta.get_viewed_array_buffer(agent);
    if buf.is_detached(agent) {
        return None;
    }
    let offset = ta.byte_offset(agent);
    // Uint8Array element size is 1, so array_length == byte length. `None` => auto-length view,
    // which for our JS callers (always concrete-length Uint8Arrays) means "to end of buffer".
    let len = ta
        .array_length(agent)
        .unwrap_or_else(|| buf.byte_length(agent).saturating_sub(offset));
    Some((buf.unbind(), offset, len))
}

/// Read the bytes of typed-array argument `index` as a zero-copy `&[u8]`.
fn arg_bytes<'a>(agent: &'a Agent, args: &ArgumentsList, index: usize) -> Option<&'a [u8]> {
    let (buf, offset, len) = ta_view(agent, args.get(index))?;
    let slice = buf.as_slice(agent);
    slice.get(offset..offset.saturating_add(len))
}

fn type_error<'a>(agent: &mut Agent, msg: &'static str, gc: NoGcScope<'a, '_>) -> JsError<'a> {
    agent.throw_exception_with_static_message(ExceptionType::TypeError, msg, gc)
}

fn js_str<'gc>(agent: &mut Agent, s: &str, gc: NoGcScope<'gc, '_>) -> Value<'gc> {
    JsString::from_string(agent, s.to_owned(), gc).into()
}

fn js_usize<'gc>(n: usize) -> Value<'gc> {
    // Byte counts comfortably fit the SmallInteger safe-integer range for any real buffer.
    Value::try_from(n as i64).unwrap_or(Value::from(0u8))
}

/// `hexEncode(buf: Uint8Array) -> string`
fn native_hex_encode<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let nogc = gc.into_nogc();
    let Some(bytes) = arg_bytes(agent, &args, 0) else {
        return Err(type_error(agent, "hexEncode expects a Uint8Array", nogc));
    };
    let mut out = String::with_capacity(bytes.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    Ok(js_str(agent, &out, nogc))
}

/// `hexDecode(str, out: Uint8Array) -> number` — bytes written. Stops at the first non-hex pair,
/// matching Node's lenient hex decoder.
fn native_hex_decode<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let nogc = gc.into_nogc();
    let Ok(s) = JsString::try_from(args.get(0)) else {
        return Err(type_error(agent, "hexDecode expects a string", nogc));
    };
    let Some(text) = s.as_str(agent).map(str::to_owned) else {
        return Err(type_error(agent, "hexDecode expects UTF-8 text", nogc));
    };
    let Some((buf, offset, cap)) = ta_view(agent, args.get(1)) else {
        return Err(type_error(agent, "hexDecode expects an output Uint8Array", nogc));
    };
    let dst = buf.as_mut_slice(agent);
    let dst = &mut dst[offset..offset + cap];
    let written = decode_hex_into(text.as_bytes(), dst);
    Ok(js_usize(written))
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn decode_hex_into(src: &[u8], dst: &mut [u8]) -> usize {
    let mut written = 0;
    let mut i = 0;
    while i + 1 < src.len() + 1 && written < dst.len() {
        if i + 1 >= src.len() {
            break;
        }
        let (Some(hi), Some(lo)) = (hex_val(src[i]), hex_val(src[i + 1])) else {
            break;
        };
        dst[written] = (hi << 4) | lo;
        written += 1;
        i += 2;
    }
    written
}

const STD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// `b64Encode(buf: Uint8Array, urlSafe: bool) -> string`
fn native_b64_encode<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let nogc = gc.into_nogc();
    let url_safe = to_bool(args.get(1));
    let Some(bytes) = arg_bytes(agent, &args, 0) else {
        return Err(type_error(agent, "b64Encode expects a Uint8Array", nogc));
    };
    let table = if url_safe { URL } else { STD };
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(table[((n >> 18) & 0x3f) as usize] as char);
        out.push(table[((n >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(table[((n >> 6) & 0x3f) as usize] as char);
        } else if !url_safe {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(table[(n & 0x3f) as usize] as char);
        } else if !url_safe {
            out.push('=');
        }
    }
    Ok(js_str(agent, &out, nogc))
}

/// `b64Decode(str, out: Uint8Array, urlSafe: bool) -> number` — bytes written.
fn native_b64_decode<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let nogc = gc.into_nogc();
    let Ok(s) = JsString::try_from(args.get(0)) else {
        return Err(type_error(agent, "b64Decode expects a string", nogc));
    };
    let Some(text) = s.as_str(agent).map(str::to_owned) else {
        return Err(type_error(agent, "b64Decode expects UTF-8 text", nogc));
    };
    let Some((buf, offset, cap)) = ta_view(agent, args.get(1)) else {
        return Err(type_error(agent, "b64Decode expects an output Uint8Array", nogc));
    };
    let dst = buf.as_mut_slice(agent);
    let dst = &mut dst[offset..offset + cap];
    let written = decode_b64_into(text.as_bytes(), dst);
    Ok(js_usize(written))
}

fn b64_val(b: u8) -> Option<u8> {
    match b {
        b'A'..=b'Z' => Some(b - b'A'),
        b'a'..=b'z' => Some(b - b'a' + 26),
        b'0'..=b'9' => Some(b - b'0' + 52),
        b'+' | b'-' => Some(62),
        b'/' | b'_' => Some(63),
        _ => None,
    }
}

/// Decode base64 / base64url leniently into `dst`, ignoring whitespace and `=` padding (Node's
/// decoder accepts both alphabets and stops at the first invalid character).
fn decode_b64_into(src: &[u8], dst: &mut [u8]) -> usize {
    let mut acc: u32 = 0;
    let mut bits = 0u8;
    let mut written = 0;
    for &b in src {
        if b == b'=' {
            break;
        }
        if b.is_ascii_whitespace() {
            continue;
        }
        let Some(v) = b64_val(b) else { break };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            if written >= dst.len() {
                break;
            }
            dst[written] = (acc >> bits) as u8;
            written += 1;
        }
    }
    written
}

/// `utf8Decode(buf: Uint8Array) -> string` — lossy UTF-8 (invalid sequences -> U+FFFD), matching
/// Node's `toString('utf8')`.
fn native_utf8_decode<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let nogc = gc.into_nogc();
    let Some(bytes) = arg_bytes(agent, &args, 0) else {
        return Err(type_error(agent, "utf8Decode expects a Uint8Array", nogc));
    };
    let text = String::from_utf8_lossy(bytes).into_owned();
    Ok(js_str(agent, &text, nogc))
}

/// `utf8Write(str, out: Uint8Array, offset, maxLen) -> number` — bytes written, never splitting a
/// multi-byte sequence past `maxLen` (matching Node).
fn native_utf8_write<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let nogc = gc.into_nogc();
    let Ok(s) = JsString::try_from(args.get(0)) else {
        return Err(type_error(agent, "utf8Write expects a string", nogc));
    };
    let Some(text) = s.as_str(agent).map(str::to_owned) else {
        return Err(type_error(agent, "utf8Write expects UTF-8 text", nogc));
    };
    let offset = to_usize(args.get(2));
    let max_len = to_usize(args.get(3));
    let Some((buf, base, cap)) = ta_view(agent, args.get(1)) else {
        return Err(type_error(agent, "utf8Write expects an output Uint8Array", nogc));
    };
    let limit = max_len.min(cap.saturating_sub(offset));
    let src = text.as_bytes();
    // Truncate at a char boundary so we never write a partial code point.
    let mut take = src.len().min(limit);
    while take > 0 && (src[take - 1] & 0xc0) == 0x80 {
        // back up over UTF-8 continuation bytes
        let start = utf8_seq_start(src, take - 1);
        let seq_len = utf8_seq_len(src[start]);
        if start + seq_len <= take {
            break;
        }
        take = start;
    }
    let dst = buf.as_mut_slice(agent);
    let region = &mut dst[base + offset..base + offset + cap.saturating_sub(offset)];
    region[..take].copy_from_slice(&src[..take]);
    Ok(js_usize(take))
}

fn utf8_seq_start(src: &[u8], mut i: usize) -> usize {
    while i > 0 && (src[i] & 0xc0) == 0x80 {
        i -= 1;
    }
    i
}

fn utf8_seq_len(lead: u8) -> usize {
    if lead < 0x80 {
        1
    } else if lead >> 5 == 0b110 {
        2
    } else if lead >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

/// `utf8ByteLen(str) -> number`
fn native_utf8_byte_len<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let nogc = gc.into_nogc();
    let Ok(s) = JsString::try_from(args.get(0)) else {
        return Err(type_error(agent, "utf8ByteLen expects a string", nogc));
    };
    // `len(agent)` is the UTF-8 byte length of the WTF-8 backing store — exactly what we want and
    // it touches no allocation.
    Ok(js_usize(s.len(agent)))
}

/// `latin1Decode(buf: Uint8Array) -> string` — each byte maps to the matching U+00xx code point.
fn native_latin1_decode<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let nogc = gc.into_nogc();
    let Some(bytes) = arg_bytes(agent, &args, 0) else {
        return Err(type_error(agent, "latin1Decode expects a Uint8Array", nogc));
    };
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        out.push(b as char);
    }
    Ok(js_str(agent, &out, nogc))
}

/// `latin1Write(str, out: Uint8Array, offset, maxLen) -> number` — low byte of each code unit.
fn native_latin1_write<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let nogc = gc.into_nogc();
    let Ok(s) = JsString::try_from(args.get(0)) else {
        return Err(type_error(agent, "latin1Write expects a string", nogc));
    };
    let Some(text) = s.as_str(agent).map(str::to_owned) else {
        return Err(type_error(agent, "latin1Write expects UTF-8 text", nogc));
    };
    let offset = to_usize(args.get(2));
    let max_len = to_usize(args.get(3));
    let Some((buf, base, cap)) = ta_view(agent, args.get(1)) else {
        return Err(type_error(agent, "latin1Write expects an output Uint8Array", nogc));
    };
    let limit = max_len.min(cap.saturating_sub(offset));
    let dst = buf.as_mut_slice(agent);
    let region = &mut dst[base + offset..base + cap];
    let mut written = 0;
    for ch in text.chars() {
        if written >= limit {
            break;
        }
        region[written] = ch as u32 as u8;
        written += 1;
    }
    Ok(js_usize(written))
}

/// `cmp(a: Uint8Array, b: Uint8Array) -> number` — `-1 | 0 | 1`, lexicographic byte comparison.
fn native_cmp<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let nogc = gc.into_nogc();
    // Snapshot `a` to an owned buffer so the second zero-copy borrow does not alias the agent. `a`
    // is typically small (keys, hashes); this is the only copy in the codec layer and only on the
    // compare path.
    let Some(a) = arg_bytes(agent, &args, 0).map(<[u8]>::to_vec) else {
        return Err(type_error(agent, "cmp expects two Uint8Arrays", nogc));
    };
    let Some(b) = arg_bytes(agent, &args, 1) else {
        return Err(type_error(agent, "cmp expects two Uint8Arrays", nogc));
    };
    let ord = a.as_slice().cmp(b) as i64;
    Ok(Value::try_from(ord.signum()).unwrap_or(Value::from(0u8)))
}

// --- tiny arg coercions (the JS glue always passes well-typed args, so these stay minimal) -----

fn to_bool(v: Value) -> bool {
    matches!(v, Value::Boolean(true))
}

fn to_usize(v: Value) -> usize {
    match v {
        Value::Integer(i) => {
            let n: i64 = i.into();
            n.max(0) as usize
        }
        Value::SmallF64(f) => f.into_f64().max(0.0) as usize,
        _ => 0,
    }
}

/// The JS prelude. Built once per runtime; a `&'static str` so an un-imported runtime pays nothing.
///
/// It is an IIFE that reads the Rust natives off the private global key, defines `Buffer` as a
/// `Uint8Array` subclass with the full common API, and returns the module exports object. Kept
/// inline (rather than a sibling file) so the whole module is one self-contained unit.
const PRELUDE: &str = r##"
(function () {
  const N = globalThis["__treaty_buffer_natives__"];

  // Normalize an encoding name to a canonical token. Unknown names throw, matching Node.
  function enc(name) {
    if (name === undefined || name === null) return "utf8";
    const e = String(name).toLowerCase();
    switch (e) {
      case "utf8": case "utf-8": return "utf8";
      case "hex": return "hex";
      case "base64": return "base64";
      case "base64url": return "base64url";
      case "latin1": case "binary": return "latin1";
      case "ascii": return "ascii";
      case "ucs2": case "ucs-2": case "utf16le": case "utf-16le": return "utf16le";
      default: throw new TypeError("Unknown encoding: " + name);
    }
  }

  // How many bytes `string` occupies in `encoding` (Buffer.byteLength).
  function byteLengthOf(string, encoding) {
    if (string instanceof Uint8Array) return string.length;
    const s = String(string);
    switch (enc(encoding)) {
      case "utf8": return N.utf8ByteLen(s);
      case "ascii": case "latin1": return s.length;
      case "hex": return s.length >>> 1;
      case "utf16le": return s.length * 2;
      case "base64": case "base64url": {
        // Strip padding/whitespace then 4 chars -> 3 bytes.
        let n = 0;
        for (let i = 0; i < s.length; i++) { const c = s[i]; if (c !== "=" && c.charCodeAt(0) > 32) n++; }
        return Math.floor((n * 3) / 4);
      }
    }
  }

  // Write `string` into the Uint8Array `buf` starting at `offset`, at most `length` bytes, in the
  // given `encoding`. Returns bytes written.
  function writeString(buf, string, offset, length, encoding) {
    const e = enc(encoding);
    const max = length === undefined ? buf.length - offset : Math.min(length, buf.length - offset);
    switch (e) {
      case "utf8": return N.utf8Write(string, buf, offset, max);
      case "ascii": case "latin1": return N.latin1Write(string, buf, offset, max);
      case "hex": {
        // hex decoder writes from index 0 of a sub-view; honor offset/length via a subarray.
        const view = buf.subarray(offset, offset + max);
        return N.hexDecode(string, view);
      }
      case "base64": case "base64url": {
        const view = buf.subarray(offset, offset + max);
        return N.b64Decode(string, view, e === "base64url");
      }
      case "utf16le": {
        let w = 0;
        for (let i = 0; i < string.length && w + 1 < max + 1 && offset + w + 1 < buf.length + 1; i++) {
          if (w + 2 > max) break;
          const c = string.charCodeAt(i);
          buf[offset + w] = c & 0xff;
          buf[offset + w + 1] = (c >>> 8) & 0xff;
          w += 2;
        }
        return w;
      }
    }
  }

  // Decode the bytes of Uint8Array `buf` (already sliced to [start,end)) as `encoding` -> string.
  function decodeBytes(buf, encoding) {
    const e = enc(encoding);
    switch (e) {
      case "utf8": return N.utf8Decode(buf);
      case "latin1": return N.latin1Decode(buf);
      case "ascii": {
        // Node masks to 7 bits for ascii.
        const masked = new Uint8Array(buf.length);
        for (let i = 0; i < buf.length; i++) masked[i] = buf[i] & 0x7f;
        return N.latin1Decode(masked);
      }
      case "hex": return N.hexEncode(buf);
      case "base64": return N.b64Encode(buf, false);
      case "base64url": return N.b64Encode(buf, true);
      case "utf16le": {
        let out = "";
        for (let i = 0; i + 1 < buf.length; i += 2) out += String.fromCharCode(buf[i] | (buf[i + 1] << 8));
        return out;
      }
    }
  }

  class Buffer extends Uint8Array {
    // --- construction -----------------------------------------------------------------------
    static alloc(size, fill, encoding) {
      const b = new Buffer(size >>> 0);
      // `new Uint8Array(n)` is already zero-filled.
      if (fill !== undefined && fill !== 0) b.fill(fill, 0, b.length, encoding);
      return b;
    }
    static allocUnsafe(size) { return new Buffer(size >>> 0); }
    static allocUnsafeSlow(size) { return new Buffer(size >>> 0); }

    static from(value, a, b) {
      if (typeof value === "string") {
        const encoding = a;
        const len = byteLengthOf(value, encoding);
        const buf = new Buffer(len);
        const written = writeString(buf, value, 0, len, encoding);
        // hex/base64 may write fewer bytes than reserved (lenient decode) -> return the exact view.
        return written === len ? buf : buf.subarray(0, written);
      }
      if (value instanceof Uint8Array) {
        const buf = new Buffer(value.length);
        buf.set(value);
        return buf;
      }
      if (value instanceof ArrayBuffer) {
        const offset = a === undefined ? 0 : a >>> 0;
        const length = b === undefined ? value.byteLength - offset : b >>> 0;
        const view = new Uint8Array(value, offset, length);
        const buf = new Buffer(length);
        buf.set(view);
        return buf;
      }
      if (Array.isArray(value) || (value && typeof value.length === "number")) {
        const buf = new Buffer(value.length);
        for (let i = 0; i < value.length; i++) buf[i] = value[i] & 0xff;
        return buf;
      }
      throw new TypeError("Buffer.from: unsupported argument");
    }

    static concat(list, totalLength) {
      let total = totalLength;
      if (total === undefined) { total = 0; for (const it of list) total += it.length; }
      const out = new Buffer(total);
      let pos = 0;
      for (const it of list) {
        if (pos >= total) break;
        const take = Math.min(it.length, total - pos);
        out.set(take === it.length ? it : it.subarray(0, take), pos);
        pos += take;
      }
      return out;
    }

    static isBuffer(x) { return x instanceof Buffer; }
    static byteLength(string, encoding) { return byteLengthOf(string, encoding); }
    static compare(a, b) { return N.cmp(a, b); }
    static isEncoding(name) { try { enc(name); return true; } catch { return false; } }

    // --- instance ---------------------------------------------------------------------------
    toString(encoding, start, end) {
      const s = start === undefined ? 0 : start >>> 0;
      const e = end === undefined ? this.length : Math.min(end >>> 0, this.length);
      const view = this.subarray(s, e);
      return decodeBytes(view, encoding);
    }

    write(string, offset, length, encoding) {
      // Node's flexible overloads: write(str), write(str, enc), write(str, off, enc),
      // write(str, off, len, enc).
      if (typeof offset === "string") { encoding = offset; offset = 0; length = undefined; }
      else if (typeof length === "string") { encoding = length; length = undefined; }
      offset = offset === undefined ? 0 : offset >>> 0;
      return writeString(this, string, offset, length, encoding);
    }

    equals(other) { return this.length === other.length && N.cmp(this, other) === 0; }
    compare(other) { return N.cmp(this, other); }

    // slice/subarray return a VIEW over the same memory (Node semantics) but typed as Buffer.
    slice(start, end) { return this.subarray(start, end); }
    subarray(start, end) {
      const sub = super.subarray(start, end);
      Object.setPrototypeOf(sub, Buffer.prototype);
      return sub;
    }

    fill(value, start, end, encoding) {
      start = start === undefined ? 0 : start >>> 0;
      end = end === undefined ? this.length : Math.min(end >>> 0, this.length);
      if (typeof value === "string") {
        // Repeat the encoded bytes of `value` across [start,end).
        const tmp = Buffer.from(value, encoding);
        if (tmp.length === 0) return this;
        for (let i = start, j = 0; i < end; i++, j = (j + 1) % tmp.length) this[i] = tmp[j];
        return this;
      }
      const byte = (typeof value === "number" ? value : 0) & 0xff;
      for (let i = start; i < end; i++) this[i] = byte;
      return this;
    }

    copy(target, targetStart, sourceStart, sourceEnd) {
      targetStart = targetStart === undefined ? 0 : targetStart >>> 0;
      sourceStart = sourceStart === undefined ? 0 : sourceStart >>> 0;
      sourceEnd = sourceEnd === undefined ? this.length : Math.min(sourceEnd >>> 0, this.length);
      let n = 0;
      for (let i = sourceStart; i < sourceEnd && targetStart + n < target.length; i++, n++) {
        target[targetStart + n] = this[i];
      }
      return n;
    }

    indexOf(value, byteOffset, encoding) {
      const needle = typeof value === "number"
        ? Buffer.from([value & 0xff])
        : (value instanceof Uint8Array ? value : Buffer.from(String(value), encoding));
      let from = byteOffset === undefined ? 0 : (byteOffset | 0);
      if (from < 0) from = Math.max(0, this.length + from);
      if (needle.length === 0) return from <= this.length ? from : -1;
      outer: for (let i = from; i + needle.length <= this.length; i++) {
        for (let j = 0; j < needle.length; j++) if (this[i + j] !== needle[j]) continue outer;
        return i;
      }
      return -1;
    }
    includes(value, byteOffset, encoding) { return this.indexOf(value, byteOffset, encoding) !== -1; }

    toJSON() { return { type: "Buffer", data: Array.from(this) }; }

    // Fixed-width integer accessors via a DataView over our own bytes (zero extra copy).
    _dv() { return new DataView(this.buffer, this.byteOffset, this.byteLength); }
    readUInt8(o) { return this._dv().getUint8(o >>> 0); }
    readInt8(o) { return this._dv().getInt8(o >>> 0); }
    readUInt16LE(o) { return this._dv().getUint16(o >>> 0, true); }
    readUInt16BE(o) { return this._dv().getUint16(o >>> 0, false); }
    readInt16LE(o) { return this._dv().getInt16(o >>> 0, true); }
    readInt16BE(o) { return this._dv().getInt16(o >>> 0, false); }
    readUInt32LE(o) { return this._dv().getUint32(o >>> 0, true); }
    readUInt32BE(o) { return this._dv().getUint32(o >>> 0, false); }
    readInt32LE(o) { return this._dv().getInt32(o >>> 0, true); }
    readInt32BE(o) { return this._dv().getInt32(o >>> 0, false); }
    readFloatLE(o) { return this._dv().getFloat32(o >>> 0, true); }
    readFloatBE(o) { return this._dv().getFloat32(o >>> 0, false); }
    readDoubleLE(o) { return this._dv().getFloat64(o >>> 0, true); }
    readDoubleBE(o) { return this._dv().getFloat64(o >>> 0, false); }
    writeUInt8(v, o) { this._dv().setUint8(o >>> 0, v); return (o >>> 0) + 1; }
    writeInt8(v, o) { this._dv().setInt8(o >>> 0, v); return (o >>> 0) + 1; }
    writeUInt16LE(v, o) { this._dv().setUint16(o >>> 0, v, true); return (o >>> 0) + 2; }
    writeUInt16BE(v, o) { this._dv().setUint16(o >>> 0, v, false); return (o >>> 0) + 2; }
    writeInt16LE(v, o) { this._dv().setInt16(o >>> 0, v, true); return (o >>> 0) + 2; }
    writeInt16BE(v, o) { this._dv().setInt16(o >>> 0, v, false); return (o >>> 0) + 2; }
    writeUInt32LE(v, o) { this._dv().setUint32(o >>> 0, v, true); return (o >>> 0) + 4; }
    writeUInt32BE(v, o) { this._dv().setUint32(o >>> 0, v, false); return (o >>> 0) + 4; }
    writeInt32LE(v, o) { this._dv().setInt32(o >>> 0, v, true); return (o >>> 0) + 4; }
    writeInt32BE(v, o) { this._dv().setInt32(o >>> 0, v, false); return (o >>> 0) + 4; }
    writeFloatLE(v, o) { this._dv().setFloat32(o >>> 0, v, true); return (o >>> 0) + 4; }
    writeFloatBE(v, o) { this._dv().setFloat32(o >>> 0, v, false); return (o >>> 0) + 4; }
    writeDoubleLE(v, o) { this._dv().setFloat64(o >>> 0, v, true); return (o >>> 0) + 8; }
    writeDoubleBE(v, o) { this._dv().setFloat64(o >>> 0, v, false); return (o >>> 0) + 8; }
  }

  // `Buffer.from`/`alloc` return tagged Uint8Arrays; the `length` property already works since
  // Buffer extends Uint8Array. Mark instances so `Buffer.isBuffer` and util.inspect can detect them.

  function atob(data) {
    const s = String(data);
    const view = new Uint8Array(Math.floor((s.length * 3) / 4) + 3);
    const n = N.b64Decode(s, view, false);
    return N.latin1Decode(view.subarray(0, n));
  }
  function btoa(data) {
    const s = String(data);
    const bytes = new Uint8Array(s.length);
    for (let i = 0; i < s.length; i++) {
      const c = s.charCodeAt(i);
      if (c > 0xff) throw new TypeError("btoa: input contains characters outside the Latin1 range");
      bytes[i] = c;
    }
    return N.b64Encode(bytes, false);
  }

  const kMaxLength = 0x7fffffff;
  const constants = { MAX_LENGTH: kMaxLength, MAX_STRING_LENGTH: 0x1fffffff };

  // `SlowBuffer` is legacy; alias to allocUnsafe (a fresh, non-pooled buffer) which is behaviorally
  // adequate for the supported subset.
  function SlowBuffer(size) { return Buffer.allocUnsafe(size); }

  return {
    Buffer,
    SlowBuffer,
    kMaxLength,
    constants,
    atob,
    btoa,
    INSPECT_MAX_BYTES: 50,
  };
})();
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::core::{EnvMap, HostState, NodeCtx};
    use nova_vm::ecmascript::{
        AgentOptions, GcAgent, String as JsString, parse_script, script_evaluation,
    };
    use serde_json::{json, Value as JsonValue};

    /// Install the `node:buffer` module directly and evaluate `src` against it, returning the JSON
    /// of the final expression.
    ///
    /// This is self-contained: it does not depend on `require`/`import` (the loader is owned by
    /// other modules). It mirrors `JsRuntime::with_node_compat`'s agent/realm/host-state setup, then
    /// calls [`install`] and binds the exports object to the global `B` (so test scripts read
    /// `B.Buffer`, `B.atob`, …) before running the script.
    ///
    /// The `HostState` is intentionally leaked for the duration of the test process: a test binary
    /// is short-lived and a couple of leaked host states cost nothing, which keeps the harness free
    /// of the `unsafe` drop-order dance that the real `JsRuntime` owns.
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

        let create_global_object: Option<for<'a> fn(&mut Agent, GcScope<'a, '_>) -> Object<'a>> =
            None;
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
            // Build the ctx from the leaked `&'static HostState` directly, so it does not borrow the
            // agent (which `install` needs mutably).
            let ctx = NodeCtx::new(host_state);
            let exports = install(agent, &ctx, gc.reborrow())
                .expect("buffer install should succeed")
                .unbind();

            // Bind exports as the global `B`.
            {
                let nogc = gc.nogc();
                let global = agent.current_realm(nogc).global_object(agent);
                let key = PropertyKey::from_static_str(agent, "B", nogc);
                let _ = global.unbind().try_define_own_property(
                    agent,
                    key.unbind(),
                    PropertyDescriptor::new_data_descriptor(exports.bind(nogc)),
                    None,
                    nogc,
                );
            }

            // Wrap so the completion is the JSON of the final expression.
            let wrapped = format!("JSON.stringify({{ v: ({src}) }})");
            let source = JsString::from_string(agent, wrapped, gc.nogc());
            let current = agent.current_realm(gc.nogc());
            let script = parse_script(agent, source.unbind(), current.unbind(), true, None, gc.nogc())
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
    fn buffer_is_a_uint8array_subclass() {
        let v = run(
            "(() => { const b = B.Buffer.from([1,2,3]);
              return [b instanceof Uint8Array, b.length, b[0], b[2]]; })()",
        );
        assert_eq!(v, json!([true, 3, 1, 3]));
    }

    #[test]
    fn alloc_zero_fills() {
        let v = run(
            "(() => { const b = B.Buffer.alloc(4);
              return [b.length, b[0], b[1], b[2], b[3]]; })()",
        );
        assert_eq!(v, json!([4, 0, 0, 0, 0]));
    }

    #[test]
    fn hex_round_trip() {
        let v = run(
            "(() => { const b = B.Buffer.from('deadbeef', 'hex');
              return [b.length, b[0], b[3], b.toString('hex')]; })()",
        );
        assert_eq!(v, json!([4, 0xde, 0xef, "deadbeef"]));
    }

    #[test]
    fn base64_round_trip() {
        let v = run(
            "(() => { const enc = B.Buffer.from('hello world').toString('base64');
              const dec = B.Buffer.from(enc, 'base64').toString('utf8');
              return [enc, dec]; })()",
        );
        assert_eq!(v, json!(["aGVsbG8gd29ybGQ=", "hello world"]));
    }

    #[test]
    fn base64url_has_no_padding_and_url_alphabet() {
        // 0xff 0xff 0xff -> "////" in std, "____" in url-safe (no '=').
        let v = run("B.Buffer.from([255,255,255]).toString('base64url')");
        assert_eq!(v, json!("____"));
    }

    #[test]
    fn utf8_round_trip_multibyte() {
        let v = run(
            "(() => { const b = B.Buffer.from('héllo €', 'utf8');
              return [b.toString('utf8'), B.Buffer.byteLength('héllo €', 'utf8')]; })()",
        );
        // "héllo €" = h(1) é(2) l(1) l(1) o(1) ' '(1) €(3) = 10 UTF-8 bytes.
        assert_eq!(v, json!(["héllo €", 10]));
    }

    #[test]
    fn latin1_round_trip() {
        let v = run(
            "(() => { const b = B.Buffer.from('ÿA', 'latin1');
              return [b.length, b[0], b[1], b.toString('latin1')]; })()",
        );
        assert_eq!(v, json!([2, 0xff, 0x41, "ÿA"]));
    }

    #[test]
    fn equals_and_compare() {
        let v = run(
            "(() => { const a = B.Buffer.from([1,2,3]);
              const b = B.Buffer.from([1,2,3]);
              const c = B.Buffer.from([1,2,4]);
              return [a.equals(b), a.equals(c), B.Buffer.compare(a, c), B.Buffer.compare(c, a), a.compare(b)]; })()",
        );
        assert_eq!(v, json!([true, false, -1, 1, 0]));
    }

    #[test]
    fn concat_joins_buffers() {
        let v = run(
            "(() => { const b = B.Buffer.concat([B.Buffer.from([1,2]), B.Buffer.from([3]), B.Buffer.from([4,5])]);
              return [b.length, Array.from(b)]; })()",
        );
        assert_eq!(v, json!([5, [1, 2, 3, 4, 5]]));
    }

    #[test]
    fn read_write_uint_fixed_width() {
        let v = run(
            "(() => { const b = B.Buffer.alloc(8);
              b.writeUInt16BE(0x0102, 0);
              b.writeUInt32LE(0x0a0b0c0d, 2);
              return [b.readUInt16BE(0), b.readUInt32LE(2), b[0], b[1]]; })()",
        );
        assert_eq!(v, json!([0x0102, 0x0a0b0c0d, 0x01, 0x02]));
    }

    #[test]
    fn slice_shares_memory() {
        // Node Buffer.slice is a view: mutating the slice mutates the parent.
        let v = run(
            "(() => { const b = B.Buffer.from([10,20,30,40]);
              const s = b.slice(1, 3);
              s[0] = 99;
              return [s.length, b[1], s[0], s[1]]; })()",
        );
        assert_eq!(v, json!([2, 99, 99, 30]));
    }

    #[test]
    fn fill_and_indexof() {
        let v = run(
            "(() => { const b = B.Buffer.alloc(5);
              b.fill(7);
              const h = B.Buffer.from('abcabc');
              return [Array.from(b), h.indexOf('bc'), h.indexOf('bc', 2), h.includes('zz')]; })()",
        );
        assert_eq!(v, json!([[7, 7, 7, 7, 7], 1, 4, false]));
    }

    #[test]
    fn is_buffer_and_byte_length() {
        let v = run(
            "[B.Buffer.isBuffer(B.Buffer.alloc(1)), B.Buffer.isBuffer([1,2,3]), B.Buffer.byteLength('abc')]",
        );
        assert_eq!(v, json!([true, false, 3]));
    }

    #[test]
    fn atob_btoa_exported() {
        let v = run("(() => { const e = B.btoa('Hi'); return [e, B.atob(e)]; })()");
        assert_eq!(v, json!(["SGk=", "Hi"]));
    }

    #[test]
    fn copy_into_target() {
        let v = run(
            "(() => { const src = B.Buffer.from([1,2,3,4]);
              const dst = B.Buffer.alloc(4);
              const n = src.copy(dst, 1, 0, 2);
              return [n, Array.from(dst)]; })()",
        );
        assert_eq!(v, json!([2, [0, 1, 2, 0]]));
    }
}
