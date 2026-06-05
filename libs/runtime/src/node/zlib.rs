//! `node:zlib` — synchronous DEFLATE/GZIP compression.
//!
//! ## Architecture
//!
//! Mirrors `node:crypto`: a **pure-Rust core** that never touches Nova, a thin set of Rust-backed
//! natives that marshal typed-array bytes in/out, and a compile-time JS **prelude** that assembles
//! the Node-shaped exports object (`gzipSync`/`gunzipSync`, `deflateSync`/`inflateSync`,
//! `deflateRawSync`/`inflateRawSync`, the `constants` table, and the `Z_*` level aliases) over those
//! natives.
//!
//! The core is built on [`flate2`] compiled with its pure-Rust `rust_backend` (miniz_oxide) feature,
//! so there is no `libz`/C toolchain dependency and it compiles fully offline (tenet 1 — no `unsafe`,
//! tenet 3 — the only allocations are the unavoidable output buffers). The `*Sync` calls run a
//! one-shot encoder/decoder over a byte buffer; each of the three wire formats is handled directly:
//!
//! * **gzip** — RFC 1952, the `Gzip`/`Gunzip` family.
//! * **zlib (deflate)** — RFC 1950, a zlib header + DEFLATE body; Node's `deflate`/`inflate`.
//! * **raw deflate** — RFC 1951, a bare DEFLATE stream with no header; Node's `deflateRaw`/
//!   `inflateRaw`.
//! * **brotli** — RFC 7932, Node's `brotliCompressSync`/`brotliDecompressSync`. Backed by the
//!   pure-Rust [`brotli`] crate (reference encoder/decoder over `alloc-stdlib`), so it stays
//!   C-toolchain-free and offline-clean like the DEFLATE family.
//!
//! The async (callback / `util.promisify`-able) and streaming `Transform` forms layer onto the same
//! core and are tracked separately; the synchronous surface is what the conformance corpus exercises.
//!
//! ## Laziness
//!
//! Built at most once, on the first `require("node:zlib")` / `import` (tenet 2). Until then it costs
//! one `&'static str` table entry; the prelude is a `&'static str`, so an un-imported runtime pays
//! nothing for it.

use std::io::{Read, Write};

use flate2::Compression;
use flate2::read::{DeflateDecoder, GzDecoder, ZlibDecoder};
use flate2::write::{DeflateEncoder, GzEncoder, ZlibEncoder};

use nova_vm::ecmascript::{
    Agent, Array, ArgumentsList, ArrayBuffer, ExceptionType, JsError, JsResult, Object,
    OrdinaryObject, PropertyDescriptor, PropertyKey, String as JsString, TypedArray, Value,
    InternalMethods, parse_script, script_evaluation,
};
use nova_vm::engine::{Bindable, GcScope, NoGcScope};

use crate::node::core::{InstallError, NodeCtx};
use crate::node::globals::define_fn;
use crate::node::NodeModule;

// =================================================================================================
// Pure-Rust zlib core (no Nova; unit-tested directly).
// =================================================================================================

/// The DEFLATE-family wire format a one-shot call targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    /// RFC 1952 gzip (header + CRC32 + ISIZE trailer). Node `gzipSync`/`gunzipSync`.
    Gzip,
    /// RFC 1950 zlib (2-byte header + Adler-32 trailer around a DEFLATE body). Node
    /// `deflateSync`/`inflateSync`.
    Zlib,
    /// RFC 1951 raw DEFLATE, no header/trailer. Node `deflateRawSync`/`inflateRawSync`.
    Raw,
    /// RFC 7932 Brotli. Node `brotliCompressSync`/`brotliDecompressSync`.
    Brotli,
}

/// Brotli encoder window size (`lgwin`), in bits. 22 is the reference/Node default (a 4 MiB window).
const BROTLI_LGWIN: u32 = 22;
/// I/O buffer size handed to the Brotli `Write` wrappers; 4 KiB matches the crate's own default.
const BROTLI_BUFFER: usize = 4096;

/// A zlib operation failure. The only fallible part of one-shot (de)compression is malformed input
/// to a decoder (a truncated stream, a bad header, a CRC/Adler mismatch); compression never fails for
/// a valid level. Carries the underlying `std::io` message so the JS layer can surface it.
#[derive(Debug)]
pub(crate) struct ZlibError(pub(crate) String);

impl std::fmt::Display for ZlibError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Clamp a Node compression `level` (`-1` = default, `0..=9`) to a [`Compression`].
///
/// Node's `Z_DEFAULT_COMPRESSION` is `-1`; any out-of-range value falls back to the default, matching
/// the lenient coercion the JS prelude already applies before calling in.
fn compression_for(level: i32) -> Compression {
    match level {
        0..=9 => Compression::new(level as u32),
        _ => Compression::default(),
    }
}

/// Compress `data` into `format` at `level`.
///
/// Borrows the input bytes (tenet 3); the single allocation is the output `Vec`. Compression of a
/// valid buffer cannot fail, but the `flate2` writers are fallible by signature, so any I/O error is
/// surfaced rather than `unwrap`ed.
pub(crate) fn compress(format: Format, data: &[u8], level: i32) -> Result<Vec<u8>, ZlibError> {
    let comp = compression_for(level);
    let map = |e: std::io::Error| ZlibError(e.to_string());
    match format {
        Format::Gzip => {
            let mut enc = GzEncoder::new(Vec::new(), comp);
            enc.write_all(data).map_err(map)?;
            enc.finish().map_err(map)
        }
        Format::Zlib => {
            let mut enc = ZlibEncoder::new(Vec::new(), comp);
            enc.write_all(data).map_err(map)?;
            enc.finish().map_err(map)
        }
        Format::Raw => {
            let mut enc = DeflateEncoder::new(Vec::new(), comp);
            enc.write_all(data).map_err(map)?;
            enc.finish().map_err(map)
        }
        Format::Brotli => {
            // Brotli's "level" is the encoder quality (0..=11); Node defaults to 11. `-1`/out-of-range
            // (the DEFLATE default sentinel) maps to the Brotli default rather than erroring.
            let quality = match level {
                0..=11 => level as u32,
                _ => 11,
            };
            let mut out = Vec::new();
            {
                let mut enc =
                    brotli::CompressorWriter::new(&mut out, BROTLI_BUFFER, quality, BROTLI_LGWIN);
                enc.write_all(data).map_err(map)?;
                enc.flush().map_err(map)?;
            }
            Ok(out)
        }
    }
}

/// Decompress a `format`-framed `data` buffer.
///
/// Returns `Err` for malformed input (truncated stream, bad header, checksum mismatch) — exactly the
/// cases Node surfaces as a thrown `Error` from `gunzipSync`/`inflateSync`.
pub(crate) fn decompress(format: Format, data: &[u8]) -> Result<Vec<u8>, ZlibError> {
    let map = |e: std::io::Error| ZlibError(e.to_string());
    let mut out = Vec::new();
    match format {
        Format::Gzip => {
            GzDecoder::new(data).read_to_end(&mut out).map_err(map)?;
        }
        Format::Zlib => {
            ZlibDecoder::new(data).read_to_end(&mut out).map_err(map)?;
        }
        Format::Raw => {
            DeflateDecoder::new(data).read_to_end(&mut out).map_err(map)?;
        }
        Format::Brotli => {
            // Feed the framed bytes through the Brotli decompressor `Write` wrapper, collecting the
            // plaintext. A truncated/garbage stream surfaces as an `io::Error`, matching the
            // DEFLATE-family decoders (and Node throwing from `brotliDecompressSync`).
            {
                let mut dec = brotli::DecompressorWriter::new(&mut out, BROTLI_BUFFER);
                dec.write_all(data).map_err(map)?;
                dec.flush().map_err(map)?;
            }
        }
    }
    Ok(out)
}

/// Resolve a one-character format tag (the prelude passes a stable single-letter code rather than a
/// full method name) to a [`Format`]. `g` = gzip, `z` = zlib, `r` = raw deflate, `b` = brotli.
fn format_from_tag(tag: &str) -> Option<Format> {
    match tag {
        "g" => Some(Format::Gzip),
        "z" => Some(Format::Zlib),
        "r" => Some(Format::Raw),
        "b" => Some(Format::Brotli),
        _ => None,
    }
}

// =================================================================================================
// JS-facing wiring.
// =================================================================================================

/// Zero-sized marker for the `node:zlib` builtin.
pub(crate) struct ZlibModule;

impl NodeModule for ZlibModule {
    const SPECIFIER: &'static str = "zlib";

    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: GcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError> {
        install(agent, ctx, gc)
    }
}

/// The private global key under which the Rust-backed zlib natives are handed to the prelude.
/// Installed just before the prelude runs and deleted immediately after, so it never leaks.
const NATIVES_KEY: &str = "__treaty_zlib_natives__";

/// Uniform per-module entry. Returns the `node:zlib` exports object, built once lazily on first
/// import (tenet 2). Steps mirror `node:crypto`/`node:buffer`.
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    mut gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    // (1) Build the natives object.
    let natives = {
        let nogc = gc.nogc();
        let natives = OrdinaryObject::create_empty_object(agent, nogc);
        define_fn(agent, natives, "compress", native_compress, 3, nogc);
        define_fn(agent, natives, "decompress", native_decompress, 2, nogc);
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
                "could not stash zlib natives on the global".to_owned(),
            ));
        }
    }

    // (3) Evaluate the prelude; its completion value is the exports object.
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
            InstallError::Nova(format!("zlib prelude parse error: {msg}"))
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
            return Err(InstallError::Nova(format!("zlib prelude threw: {msg}")));
        }
    };

    Object::try_from(value.unbind())
        .map(|o| o.unbind().bind(gc.into_nogc()))
        .map_err(|_| InstallError::Nova("zlib prelude did not return an object".to_owned()))
}

// ---------------------------------------------------------------------------------------------
// Rust-backed natives. Each reads typed-array bytes and returns a JS byte `Array`.
// ---------------------------------------------------------------------------------------------

fn type_error<'a>(agent: &mut Agent, msg: &'static str, gc: NoGcScope<'a, '_>) -> JsError<'a> {
    agent.throw_exception_with_static_message(ExceptionType::TypeError, msg, gc)
}

/// Inspect a typed-array argument and return its backing `(buffer, byte_offset, len)`. `None` when
/// the value is not a typed array or its backing buffer is detached.
fn ta_view(agent: &Agent, value: Value) -> Option<(ArrayBuffer<'static>, usize, usize)> {
    let ta = TypedArray::try_from(value).ok()?;
    let buf = ta.get_viewed_array_buffer(agent);
    if buf.is_detached(agent) {
        return None;
    }
    let offset = ta.byte_offset(agent);
    let len = ta
        .array_length(agent)
        .unwrap_or_else(|| buf.byte_length(agent).saturating_sub(offset));
    Some((buf.unbind(), offset, len))
}

/// Read the bytes of typed-array argument `index` as an owned `Vec<u8>` (owned so the compression
/// call does not hold a borrow of `agent` while it later re-borrows mutably to build the result).
fn arg_bytes(agent: &Agent, args: &ArgumentsList, index: usize) -> Option<Vec<u8>> {
    let (buf, offset, len) = ta_view(agent, args.get(index))?;
    let slice = buf.as_slice(agent);
    slice
        .get(offset..offset.saturating_add(len))
        .map(<[u8]>::to_vec)
}

/// Read a JS string argument into an owned `String`.
fn arg_string(agent: &Agent, args: &ArgumentsList, index: usize) -> Option<String> {
    let s = JsString::try_from(args.get(index)).ok()?;
    s.as_str(agent).map(str::to_owned)
}

/// Read an integer argument (defaulting to `default` when absent / not a number).
fn arg_i32(args: &ArgumentsList, index: usize, default: i32) -> i32 {
    match args.get(index) {
        Value::Integer(i) => i.into_i64() as i32,
        Value::SmallF64(f) => f.into_f64() as i32,
        _ => default,
    }
}

/// `compress(tag: string, data: Uint8Array, level: number) -> number[]`
fn native_compress<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let nogc = gc.into_nogc();
    let Some(tag) = arg_string(agent, &args, 0) else {
        return Err(type_error(agent, "zlib expects a format tag", nogc));
    };
    let Some(format) = format_from_tag(&tag) else {
        return Err(type_error(agent, "zlib: unknown format", nogc));
    };
    let Some(data) = arg_bytes(agent, &args, 1) else {
        return Err(type_error(agent, "zlib expects a Uint8Array of input", nogc));
    };
    let level = arg_i32(&args, 2, -1);
    match compress(format, &data, level) {
        Ok(out) => Ok(bytes_to_array(agent, &out, nogc).into()),
        Err(e) => Err(throw_zlib(agent, &e.0, nogc)),
    }
}

/// `decompress(tag: string, data: Uint8Array) -> number[]`
fn native_decompress<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let nogc = gc.into_nogc();
    let Some(tag) = arg_string(agent, &args, 0) else {
        return Err(type_error(agent, "zlib expects a format tag", nogc));
    };
    let Some(format) = format_from_tag(&tag) else {
        return Err(type_error(agent, "zlib: unknown format", nogc));
    };
    let Some(data) = arg_bytes(agent, &args, 1) else {
        return Err(type_error(agent, "zlib expects a Uint8Array of input", nogc));
    };
    match decompress(format, &data) {
        Ok(out) => Ok(bytes_to_array(agent, &out, nogc).into()),
        Err(e) => Err(throw_zlib(agent, &e.0, nogc)),
    }
}

/// Throw a JS `Error` carrying a (dynamic) zlib failure message. The message is interned as a JS
/// string so the underlying `flate2`/`std::io` detail survives across the FFI boundary.
fn throw_zlib<'a>(agent: &mut Agent, msg: &str, gc: NoGcScope<'a, '_>) -> JsError<'a> {
    agent.throw_exception(ExceptionType::Error, format!("zlib: {msg}"), gc)
}

/// Build a JS `Array` whose elements are the given bytes. Mirrors `node:crypto`/`node:text_encoding`:
/// the pinned Nova rev exposes no embedder-side slice-to-`Uint8Array` constructor, so bytes cross into
/// JS as a plain byte `Array` and the prelude wraps them with `Buffer.from`/`Uint8Array.from`.
fn bytes_to_array<'gc>(agent: &mut Agent, bytes: &[u8], gc: NoGcScope<'gc, '_>) -> Array<'gc> {
    let values: Vec<Value> = bytes.iter().map(|&b| Value::from(b)).collect();
    Array::from_slice(agent, &values, gc)
}

/// The JS prelude. Built once per runtime; a `&'static str` so an un-imported runtime pays nothing.
///
/// Reads the Rust natives off the private key, then defines the synchronous compression surface and
/// the Node `constants`/`Z_*` tables, and returns the module exports object.
const PRELUDE: &str = r##"
(function () {
  const N = globalThis["__treaty_zlib_natives__"];

  // Coerce a `gzipSync(buf)` input to a Uint8Array. Accepts strings (utf8), ArrayBuffer, and any
  // ArrayBuffer view (Buffer included).
  function toBytes(value) {
    if (value instanceof Uint8Array) return value;
    if (ArrayBuffer.isView(value)) return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
    if (value instanceof ArrayBuffer) return new Uint8Array(value);
    if (typeof value === "string") {
      const out = [];
      for (let i = 0; i < value.length; i++) {
        let c = value.charCodeAt(i);
        if (c >= 0xd800 && c <= 0xdbff && i + 1 < value.length) {
          const c2 = value.charCodeAt(i + 1);
          if (c2 >= 0xdc00 && c2 <= 0xdfff) { c = 0x10000 + ((c - 0xd800) << 10) + (c2 - 0xdc00); i++; }
        }
        if (c < 0x80) out.push(c);
        else if (c < 0x800) out.push(0xc0 | (c >> 6), 0x80 | (c & 0x3f));
        else if (c < 0x10000) out.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 0x3f), 0x80 | (c & 0x3f));
        else out.push(0xf0 | (c >> 18), 0x80 | ((c >> 12) & 0x3f), 0x80 | ((c >> 6) & 0x3f), 0x80 | (c & 0x3f));
      }
      return Uint8Array.from(out);
    }
    throw new TypeError("zlib input must be a string, Buffer, TypedArray, DataView, or ArrayBuffer");
  }

  // Wrap a byte array as a Buffer when node:buffer is loaded; otherwise a plain Uint8Array (Node
  // returns a Buffer from the *Sync calls).
  function asBuffer(byteArray) {
    const u8 = byteArray instanceof Uint8Array ? byteArray : Uint8Array.from(byteArray);
    try {
      const buf = (typeof require === "function") ? require("node:buffer") : null;
      if (buf && buf.Buffer) return buf.Buffer.from(u8);
    } catch (_) { /* buffer not available */ }
    return u8;
  }

  // Pull `level` out of an options object (Node accepts `{ level }`); default -1 (Z_DEFAULT).
  function levelOf(options) {
    if (options && typeof options === "object" && options.level !== undefined && options.level !== null) {
      const n = options.level | 0;
      return n;
    }
    return -1;
  }

  function comp(tag, buf, options) { return asBuffer(N.compress(tag, toBytes(buf), levelOf(options))); }
  function decomp(tag, buf) { return asBuffer(N.decompress(tag, toBytes(buf))); }

  // Brotli quality from a Node options object. Node spells it `{ params: { [BROTLI_PARAM_QUALITY]: q } }`
  // (BROTLI_PARAM_QUALITY === 1); we also accept a bare `{ quality }` shorthand. Default -1 => the
  // Rust core's reference default (quality 11).
  function brotliQuality(options) {
    if (options && typeof options === "object") {
      const params = options.params;
      if (params && typeof params === "object" && params[1] !== undefined && params[1] !== null) {
        return params[1] | 0;
      }
      if (options.quality !== undefined && options.quality !== null) {
        return options.quality | 0;
      }
    }
    return -1;
  }
  function brotliCompressSync(buf, options) {
    return asBuffer(N.compress("b", toBytes(buf), brotliQuality(options)));
  }
  function brotliDecompressSync(buf, options) { return decomp("b", buf); }

  function gzipSync(buf, options) { return comp("g", buf, options); }
  function gunzipSync(buf, options) { return decomp("g", buf); }
  function deflateSync(buf, options) { return comp("z", buf, options); }
  function inflateSync(buf, options) { return decomp("z", buf); }
  function deflateRawSync(buf, options) { return comp("r", buf, options); }
  function inflateRawSync(buf, options) { return decomp("r", buf); }
  // `unzipSync` auto-detects gzip vs zlib by the magic header (Node behavior); fall back to zlib.
  function unzipSync(buf, options) {
    const b = toBytes(buf);
    if (b.length >= 2 && b[0] === 0x1f && b[1] === 0x8b) return decomp("g", b);
    return decomp("z", b);
  }

  // Node's zlib.constants table (the subset that callers actually read for levels / strategies /
  // flush modes). Values match Node/zlib so user code comparing against them behaves identically.
  const constants = {
    Z_NO_FLUSH: 0, Z_PARTIAL_FLUSH: 1, Z_SYNC_FLUSH: 2, Z_FULL_FLUSH: 3, Z_FINISH: 4, Z_BLOCK: 5, Z_TREES: 6,
    Z_OK: 0, Z_STREAM_END: 1, Z_NEED_DICT: 2, Z_ERRNO: -1, Z_STREAM_ERROR: -2, Z_DATA_ERROR: -3,
    Z_MEM_ERROR: -4, Z_BUF_ERROR: -5, Z_VERSION_ERROR: -6,
    Z_NO_COMPRESSION: 0, Z_BEST_SPEED: 1, Z_BEST_COMPRESSION: 9, Z_DEFAULT_COMPRESSION: -1,
    Z_FILTERED: 1, Z_HUFFMAN_ONLY: 2, Z_RLE: 3, Z_FIXED: 4, Z_DEFAULT_STRATEGY: 0,
    Z_DEFAULT_LEVEL: -1, Z_MIN_LEVEL: -1, Z_MAX_LEVEL: 9,
    // Brotli parameter ids + quality bounds (RFC 7932 / Node's zlib.constants). BROTLI_PARAM_QUALITY
    // is the encoder quality knob brotliCompressSync reads out of `options.params`.
    BROTLI_OPERATION_PROCESS: 0, BROTLI_OPERATION_FLUSH: 1, BROTLI_OPERATION_FINISH: 2,
    BROTLI_OPERATION_EMIT_METADATA: 3,
    BROTLI_PARAM_MODE: 0, BROTLI_PARAM_QUALITY: 1, BROTLI_PARAM_LGWIN: 2, BROTLI_PARAM_LGBLOCK: 3,
    BROTLI_PARAM_DISABLE_LITERAL_CONTEXT_MODELING: 4, BROTLI_PARAM_SIZE_HINT: 5,
    BROTLI_PARAM_LARGE_WINDOW: 6, BROTLI_PARAM_NPOSTFIX: 7, BROTLI_PARAM_NDIRECT: 8,
    BROTLI_MODE_GENERIC: 0, BROTLI_MODE_TEXT: 1, BROTLI_MODE_FONT: 2,
    BROTLI_MIN_QUALITY: 0, BROTLI_MAX_QUALITY: 11, BROTLI_DEFAULT_QUALITY: 11,
    BROTLI_MIN_WINDOW_BITS: 10, BROTLI_MAX_WINDOW_BITS: 24, BROTLI_DEFAULT_WINDOW: 22,
  };

  const exports = {
    gzipSync, gunzipSync,
    deflateSync, inflateSync,
    deflateRawSync, inflateRawSync,
    brotliCompressSync, brotliDecompressSync,
    unzipSync,
    constants,
  };
  // Node also surfaces the Z_* constants directly on the module object.
  for (const k in constants) exports[k] = constants[k];
  return exports;
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

    // ----- pure-core unit tests (no JS agent) -------------------------------------------------

    #[test]
    fn gzip_round_trips_a_string() {
        let msg = b"the quick brown fox jumps over the lazy dog";
        let z = compress(Format::Gzip, msg, -1).unwrap();
        // A gzip stream starts with the magic bytes 0x1f 0x8b and the deflate method 0x08.
        assert_eq!(&z[..3], &[0x1f, 0x8b, 0x08]);
        let back = decompress(Format::Gzip, &z).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn zlib_round_trips_a_buffer() {
        let data: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
        let z = compress(Format::Zlib, &data, 9).unwrap();
        // A zlib stream starts with a 0x78 CMF byte for the 32K window / deflate method.
        assert_eq!(z[0], 0x78);
        let back = decompress(Format::Zlib, &z).unwrap();
        assert_eq!(back, data);
    }

    #[test]
    fn raw_deflate_round_trips() {
        let data = b"raw deflate has no zlib/gzip header";
        let z = compress(Format::Raw, data, 6).unwrap();
        let back = decompress(Format::Raw, &z).unwrap();
        assert_eq!(back, data);
        // Raw deflate must NOT inflate as zlib (no 2-byte header), proving the framing differs.
        assert!(decompress(Format::Zlib, &z).is_err() || decompress(Format::Zlib, &z).unwrap() != data);
    }

    #[test]
    fn levels_all_round_trip_and_compress() {
        let data: Vec<u8> = b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_vec();
        for level in -1..=9 {
            let z = compress(Format::Gzip, &data, level).unwrap();
            assert!(z.len() < data.len() + 64, "level {level} produced output");
            assert_eq!(decompress(Format::Gzip, &z).unwrap(), data);
        }
    }

    #[test]
    fn decompress_rejects_garbage() {
        assert!(decompress(Format::Gzip, b"not a gzip stream").is_err());
        assert!(decompress(Format::Zlib, &[0x00, 0x01, 0x02, 0x03]).is_err());
    }

    #[test]
    fn empty_input_round_trips() {
        for f in [Format::Gzip, Format::Zlib, Format::Raw] {
            let z = compress(f, b"", -1).unwrap();
            assert_eq!(decompress(f, &z).unwrap(), b"");
        }
    }

    #[test]
    fn format_tag_mapping() {
        assert_eq!(format_from_tag("g"), Some(Format::Gzip));
        assert_eq!(format_from_tag("z"), Some(Format::Zlib));
        assert_eq!(format_from_tag("r"), Some(Format::Raw));
        assert_eq!(format_from_tag("b"), Some(Format::Brotli));
        assert_eq!(format_from_tag("q"), None);
    }

    #[test]
    fn brotli_round_trips_a_string() {
        let msg = b"the quick brown fox jumps over the lazy dog";
        let z = compress(Format::Brotli, msg, -1).unwrap();
        assert!(!z.is_empty(), "brotli produced output");
        let back = decompress(Format::Brotli, &z).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn brotli_round_trips_at_every_quality_and_compresses() {
        // A highly repetitive buffer must shrink, and every quality (0..=11, plus the -1 default
        // sentinel) must losslessly round-trip.
        let data: Vec<u8> = b"the quick brown fox jumps over the lazy dog "
            .iter()
            .cycle()
            .take(4096)
            .copied()
            .collect();
        for quality in -1..=11 {
            let z = compress(Format::Brotli, &data, quality).unwrap();
            assert!(z.len() < data.len(), "quality {quality} compressed smaller");
            assert_eq!(decompress(Format::Brotli, &z).unwrap(), data);
        }
    }

    #[test]
    fn brotli_empty_input_round_trips() {
        let z = compress(Format::Brotli, b"", -1).unwrap();
        assert_eq!(decompress(Format::Brotli, &z).unwrap(), b"");
    }

    #[test]
    fn brotli_decompress_rejects_garbage() {
        // A non-brotli byte stream must surface as an error, not silent empty output.
        assert!(decompress(Format::Brotli, b"\xff\xfe\xfd not a brotli stream at all \x00\x01").is_err());
    }

    // ----- JS-surface integration tests (live engine) -----------------------------------------

    /// Install `node:zlib` directly and evaluate `src`, returning the JSON of `({ v: (<src>) })`.
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

        agent.run_in_realm(&realm, |agent, mut gc| -> JsonValue {
            let ctx = NodeCtx::new(host_state);
            let exports = install(agent, &ctx, gc.reborrow())
                .expect("zlib install should succeed")
                .unbind();

            {
                let nogc = gc.nogc();
                let global = agent.current_realm(nogc).global_object(agent);
                let key = PropertyKey::from_static_str(agent, "Z", nogc);
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
            let out = match value {
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
            };
            let envelope: JsonValue = serde_json::from_str(&out).expect("result is JSON");
            match envelope {
                JsonValue::Object(mut m) => m.remove("v").unwrap_or(JsonValue::Null),
                other => other,
            }
        })
    }

    #[test]
    fn gzip_sync_then_gunzip_sync_round_trips_a_string() {
        let v = run(
            "(() => {
               const s = 'the quick brown fox jumps over the lazy dog';
               const z = Z.gzipSync(s);
               const back = Z.gunzipSync(z);
               // Decode back to a string without depending on TextDecoder.
               let out = '';
               for (const b of back) out += String.fromCharCode(b);
               return [z[0], z[1], out]; })()",
        );
        assert_eq!(
            v,
            json!([0x1f, 0x8b, "the quick brown fox jumps over the lazy dog"])
        );
    }

    #[test]
    fn deflate_inflate_and_raw_round_trip_in_js() {
        let v = run(
            "(() => {
               const data = new Uint8Array([1,2,3,4,5,6,7,8,9,10]);
               const z = Z.inflateSync(Z.deflateSync(data));
               const r = Z.inflateRawSync(Z.deflateRawSync(data));
               const eq = (a) => a.length === 10 && a[0] === 1 && a[9] === 10;
               return [eq(z), eq(r)]; })()",
        );
        assert_eq!(v, json!([true, true]));
    }

    #[test]
    fn level_option_is_honored() {
        // A highly compressible buffer at level 0 (store) is larger than at level 9.
        let v = run(
            "(() => {
               const data = 'a'.repeat(2000);
               const big = Z.gzipSync(data, { level: 0 }).length;
               const small = Z.gzipSync(data, { level: 9 }).length;
               return small < big; })()",
        );
        assert_eq!(v, json!(true));
    }

    #[test]
    fn constants_are_exposed() {
        let v = run("[Z.constants.Z_BEST_COMPRESSION, Z.Z_DEFAULT_COMPRESSION, Z.constants.Z_NO_FLUSH]");
        assert_eq!(v, json!([9, -1, 0]));
    }

    #[test]
    fn unzip_sync_auto_detects_gzip() {
        let v = run(
            "(() => {
               const data = 'hello unzip';
               const gz = Z.unzipSync(Z.gzipSync(data));
               const zl = Z.unzipSync(Z.deflateSync(data));
               const dec = (a) => { let s=''; for (const b of a) s += String.fromCharCode(b); return s; };
               return [dec(gz), dec(zl)]; })()",
        );
        assert_eq!(v, json!(["hello unzip", "hello unzip"]));
    }

    #[test]
    fn brotli_compress_then_decompress_round_trips_a_string() {
        let v = run(
            "(() => {
               const s = 'the quick brown fox jumps over the lazy dog '.repeat(16);
               const z = Z.brotliCompressSync(s);
               const back = Z.brotliDecompressSync(z);
               let out = '';
               for (const b of back) out += String.fromCharCode(b);
               return [z.length > 0, z.length < s.length, out === s]; })()",
        );
        assert_eq!(v, json!([true, true, true]));
    }

    #[test]
    fn brotli_quality_option_is_honored() {
        // The same payload at the lowest quality is no smaller than the highest; both round-trip.
        let v = run(
            "(() => {
               const C = Z.constants;
               const data = 'a'.repeat(4000);
               const lo = Z.brotliCompressSync(data, { params: { [C.BROTLI_PARAM_QUALITY]: 0 } });
               const hi = Z.brotliCompressSync(data, { params: { [C.BROTLI_PARAM_QUALITY]: 11 } });
               const ok = (b) => { let s=''; for (const x of Z.brotliDecompressSync(b)) s += String.fromCharCode(x); return s === data; };
               return [hi.length <= lo.length, ok(lo), ok(hi)]; })()",
        );
        assert_eq!(v, json!([true, true, true]));
    }

    #[test]
    fn brotli_decompress_throws_on_garbage() {
        let v = run(
            "(() => { try { Z.brotliDecompressSync(new Uint8Array([255,254,253,1,2,3,4,5])); return 'no-throw'; } catch (e) { return 'threw'; } })()",
        );
        assert_eq!(v, json!("threw"));
    }

    #[test]
    fn gunzip_sync_throws_on_garbage() {
        let v = run(
            "(() => { try { Z.gunzipSync(new Uint8Array([1,2,3,4])); return 'no-throw'; } catch (e) { return 'threw'; } })()",
        );
        assert_eq!(v, json!("threw"));
    }
}
