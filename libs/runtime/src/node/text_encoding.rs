//! `node:text_encoding` — the WHATWG/WinterCG `TextEncoder` / `TextDecoder` UTF-8 codec.
//!
//! Node always exposes `TextEncoder`/`TextDecoder` as eager globals, so the codec itself must be
//! cheap and allocation-conscious. This file owns:
//!
//! * A **pure-Rust UTF-8 codec core** ([`encode_utf8`], [`decode_utf8`], [`normalize_label`]) that
//!   never touches Nova. It is the high-value heart of the module and is exhaustively unit-tested in
//!   isolation (no JS agent needed) — tenets 1 (no `unsafe`) and 3 (borrow / zero-copy over clone):
//!   `encode_utf8` returns the input `str`'s bytes *by borrow* (a `str` already **is** its UTF-8
//!   encoding, so encoding is zero-copy), and `decode_utf8` returns a [`Cow`] that borrows the input
//!   whenever it is already valid UTF-8 with no BOM to strip.
//! * The JS-facing exports object built by the uniform [`install`] seam. Each method is a thin
//!   [`RegularFn`] wrapper that reads its argument, calls the pure core, and hands the result back.
//!
//! ## Faithfulness and the one documented divergence
//!
//! `TextEncoder` in Node/WHATWG is *always* UTF-8; `TextDecoder` defaults to UTF-8 and honours the
//! `fatal` and `ignoreBOM` options. Those are implemented faithfully by the codec core:
//!
//! * `decode` strips a leading UTF-8 BOM (`EF BB BF`) unless `ignoreBOM` is set (WHATWG default is to
//!   strip it).
//! * `fatal: false` (the default) replaces every ill-formed byte sequence with U+FFFD
//!   (REPLACEMENT CHARACTER), exactly matching `String::from_utf8_lossy`'s maximal-subpart behaviour.
//! * `fatal: true` raises a `TypeError` on the first ill-formed sequence.
//!
//! The single divergence from Node is the **shape of the bytes at the JS boundary**: WHATWG
//! `TextEncoder.prototype.encode` returns a `Uint8Array` and `TextDecoder.prototype.decode` accepts a
//! `BufferSource`. The pinned Nova rev (`bece61ac`) exposes no embedder-side API to *construct* a
//! `Uint8Array` from a byte slice nor to read one's backing bytes (`typed_array_create_from_data_block`
//! and the conversion abstract operations are `pub(crate)`), so the native functions here exchange a
//! plain JS `Array` of byte integers instead. The byte *values* and all codec semantics are identical;
//! only the wrapper type differs. Wrapping these in the `Uint8Array`-returning `TextEncoder`/
//! `TextDecoder` **classes** and installing them as eager globals is therefore deferred to
//! `globals.rs` (which owns realm/global wiring and can JS-bootstrap the class shells over these
//! native primitives once Nova surfaces typed-array construction). See the module-level deferral note
//! returned with this task.

use std::borrow::Cow;

use nova_vm::ecmascript::{
    Agent, Array, ArgumentsList, ExceptionType, InternalMethods, JsResult, Object, OrdinaryObject,
    PropertyDescriptor, PropertyKey, String as JsString, TryGetResult, Value, unwrap_try,
};
use nova_vm::engine::NoGcScope;

use crate::node::core::{InstallError, NodeCtx};
use crate::node::globals::define_fn;
use crate::node::{GcScope, NodeModule};

// ---------------------------------------------------------------------------------------------
// Pure UTF-8 codec core (no Nova; unit-tested directly).
// ---------------------------------------------------------------------------------------------

/// The UTF-8 byte-order mark. WHATWG `TextDecoder` strips a leading BOM unless `ignoreBOM` is set.
const UTF8_BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];

/// `TextEncoder.encode(input)` — encode a string to UTF-8 bytes.
///
/// Zero-copy (tenet 3): a Rust `&str` is, by definition, already valid UTF-8, so its encoding is just
/// its bytes. We borrow them directly rather than allocating a fresh buffer. WHATWG's encoder is
/// UTF-8-only and never fails, so there is no error path.
#[inline]
pub(crate) fn encode_utf8(input: &str) -> &[u8] {
    input.as_bytes()
}

/// `TextDecoder.decode(bytes, { fatal, ignoreBOM })` — decode UTF-8 bytes to a string.
///
/// * When the input is already valid UTF-8 (and no BOM needs stripping), the result borrows the input
///   with no allocation (tenet 3).
/// * `ignore_bom == false` (the WHATWG default) strips a single leading UTF-8 BOM.
/// * `fatal == false` (the default) substitutes U+FFFD for each ill-formed sequence, matching
///   `String::from_utf8_lossy`'s maximal-subpart-of-an-ill-formed-sequence replacement.
/// * `fatal == true` returns [`DecodeError`] on the first ill-formed sequence.
pub(crate) fn decode_utf8(
    bytes: &[u8],
    fatal: bool,
    ignore_bom: bool,
) -> Result<Cow<'_, str>, DecodeError> {
    // Strip a leading UTF-8 BOM unless asked to keep it (WHATWG default: strip).
    let bytes = if !ignore_bom && bytes.starts_with(&UTF8_BOM) {
        &bytes[UTF8_BOM.len()..]
    } else {
        bytes
    };

    if fatal {
        // Strict: any ill-formed sequence is an error. `str::from_utf8` borrows on success.
        match std::str::from_utf8(bytes) {
            Ok(s) => Ok(Cow::Borrowed(s)),
            Err(_) => Err(DecodeError::Invalid),
        }
    } else {
        // Lossy: valid input borrows; invalid input allocates a corrected copy with U+FFFD.
        Ok(String::from_utf8_lossy(bytes))
    }
}

/// A fatal-mode decode failure (`TextDecoder` constructed with `{ fatal: true }`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecodeError {
    /// The byte sequence is not well-formed for the decoder's encoding.
    Invalid,
}

/// Normalize a WHATWG encoding label to its canonical name, or `None` if unsupported here.
///
/// WHATWG defines a large label table; this codec implements the UTF-8 family only (which is all
/// `TextEncoder` ever uses and the `TextDecoder` default). The recognized labels are matched
/// case-insensitively after trimming ASCII whitespace, mirroring the WHATWG "get an encoding"
/// preprocessing. Returns the canonical `"utf-8"` for every accepted alias.
pub(crate) fn normalize_label(label: &str) -> Option<&'static str> {
    let trimmed = label.trim_matches(|c: char| c.is_ascii_whitespace());
    // The UTF-8 label set from the WHATWG Encoding Standard's index.
    const UTF8_LABELS: [&str; 5] = ["utf-8", "utf8", "unicode-1-1-utf-8", "unicode11utf8", "unicode20utf8"];
    if UTF8_LABELS.iter().any(|l| trimmed.eq_ignore_ascii_case(l)) {
        Some("utf-8")
    } else {
        None
    }
}

// ---------------------------------------------------------------------------------------------
// JS-facing wiring.
// ---------------------------------------------------------------------------------------------

/// Zero-sized marker for the `node:text_encoding` builtin.
pub(crate) struct TextEncodingModule;

impl NodeModule for TextEncodingModule {
    const SPECIFIER: &'static str = "text_encoding";

    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: GcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError> {
        install(agent, ctx, gc)
    }
}

/// Uniform per-module entry. Returns the `node:text_encoding` exports object.
///
/// The exports expose the UTF-8 codec as native functions over a plain byte-integer `Array` (see the
/// module-level divergence note): `encode(string) -> number[]`, `decode(number[], { fatal,
/// ignoreBOM }?) -> string`, and the canonical `encoding` label. They are materialized once, lazily,
/// on first import (tenet 2).
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    let gc = gc.into_nogc();
    let obj = OrdinaryObject::create_empty_object(agent, gc);

    define_fn(agent, obj, "encode", js::encode, 1, gc);
    define_fn(agent, obj, "decode", js::decode, 1, gc);
    define_static_string(agent, obj, "encoding", "utf-8", gc);

    Ok(obj.into())
}

/// Define a `&'static str` as a string-valued data property `name` on `obj`.
fn define_static_string(
    agent: &mut Agent,
    obj: OrdinaryObject,
    name: &'static str,
    value: &'static str,
    gc: NoGcScope,
) {
    let key = PropertyKey::from_static_str(agent, name, gc);
    let js = JsString::from_static_str(agent, value, gc);
    unwrap_try(obj.try_define_own_property(
        agent,
        key,
        PropertyDescriptor::new_data_descriptor(js),
        None,
        gc,
    ));
}

/// The JS wrapper functions: thin [`nova_vm::ecmascript::RegularFn`]s that marshal arguments to/from
/// the pure codec core above.
mod js {
    use super::*;

    /// `encode(string)` — UTF-8 encode the first argument, returning a JS `Array` of byte integers.
    ///
    /// A non-string first argument is coerced through WHATWG's "USVString" expectation by simply
    /// requiring a JS string (the WHATWG algorithm `ToString`-coerces; we keep the strict, allocation-
    /// free path and treat a missing/undefined argument as the empty string, matching
    /// `new TextEncoder().encode()` returning an empty array).
    pub(super) fn encode<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let arg = args.get(0);
        // `undefined` -> "" (Node coerces `undefined` to the string "undefined", but the no-arg form
        // `encode()` yields an empty array; we match the common no-arg case and the empty string).
        // Materialize to an owned `String` so the `agent` borrow taken by `to_string_lossy` is
        // released before we re-borrow `agent` mutably to build the result array.
        let text: String = match JsString::try_from(arg) {
            Ok(s) => s.to_string_lossy(agent).into_owned(),
            Err(_) if arg.is_undefined() => String::new(),
            Err(_) => {
                return Err(agent.throw_exception_with_static_message(
                    ExceptionType::TypeError,
                    "TextEncoder.encode expects a string",
                    gc.into_nogc(),
                ));
            }
        };
        let bytes = encode_utf8(&text);
        Ok(bytes_to_array(agent, bytes, gc.into_nogc()).into())
    }

    /// `decode(bytes, { fatal?, ignoreBOM? })` — UTF-8 decode an array of byte integers.
    ///
    /// `bytes` is a JS `Array` of integers in `0..=255` (see the module divergence note). The options
    /// object is read for the boolean `fatal` and `ignoreBOM` flags (both default `false`). A missing
    /// `bytes` argument decodes the empty input to `""`.
    pub(super) fn decode<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let nogc = gc.into_nogc();
        let bytes = match read_byte_array(agent, args.get(0), nogc) {
            Ok(b) => b,
            Err(ByteReadError::NotArray) => {
                if args.get(0).is_undefined() {
                    Vec::new()
                } else {
                    return Err(agent.throw_exception_with_static_message(
                        ExceptionType::TypeError,
                        "TextDecoder.decode expects an array of byte values",
                        nogc,
                    ));
                }
            }
            Err(ByteReadError::BadByte) => {
                return Err(agent.throw_exception_with_static_message(
                    ExceptionType::TypeError,
                    "TextDecoder.decode array contains a non-byte value",
                    nogc,
                ));
            }
        };

        let (fatal, ignore_bom) = read_decode_options(agent, args.get(1), nogc);

        match decode_utf8(&bytes, fatal, ignore_bom) {
            Ok(text) => Ok(JsString::from_str(agent, &text, nogc).into()),
            Err(DecodeError::Invalid) => Err(agent.throw_exception_with_static_message(
                ExceptionType::TypeError,
                "The encoded data was not valid for encoding utf-8",
                nogc,
            )),
        }
    }

    /// Build a JS `Array` whose elements are the given bytes (each as a small integer `0..=255`).
    fn bytes_to_array<'gc>(agent: &mut Agent, bytes: &[u8], gc: NoGcScope<'gc, '_>) -> Array<'gc> {
        // One `Value` per byte; `Value::from(u8)` is a tagged small integer (no heap per element).
        let values: Vec<Value> = bytes.iter().map(|&b| Value::from(b)).collect();
        Array::from_slice(agent, &values, gc)
    }

    /// A failure while reading the `bytes` argument of `decode`.
    enum ByteReadError {
        /// The argument is not a JS array.
        NotArray,
        /// An element was not an integer in `0..=255`.
        BadByte,
    }

    /// Read a JS `Array` of byte integers into a `Vec<u8>`.
    ///
    /// Reads each indexed element through the array's own `[[Get]]`; an element that is not an integer
    /// in `0..=255` is rejected (`BadByte`). Pre-sizes the `Vec` to the array length to avoid repeated
    /// reallocation (tenet 3).
    fn read_byte_array(
        agent: &mut Agent,
        value: Value,
        gc: NoGcScope,
    ) -> Result<Vec<u8>, ByteReadError> {
        let array = Array::try_from(value).map_err(|_| ByteReadError::NotArray)?;
        let len = array.len(agent);
        let mut out = Vec::with_capacity(len as usize);
        for i in 0..len {
            let key = PropertyKey::Integer(i.into());
            // Dense byte arrays hold plain data properties, so the result is `Value`; an absent index
            // (`Unset`) reads as a `0` byte (a hole), and anything needing a getter/Proxy call is
            // non-conforming input and rejected as a bad byte.
            let element = match unwrap_try(array.try_get(agent, key, array.into(), None, gc)) {
                TryGetResult::Value(v) => v,
                TryGetResult::Unset => Value::from(0u8),
                _ => return Err(ByteReadError::BadByte),
            };
            let byte = value_to_byte(element).ok_or(ByteReadError::BadByte)?;
            out.push(byte);
        }
        Ok(out)
    }

    /// Coerce a JS value to a byte (`0..=255`), accepting only an exact small integer in range. A
    /// non-integer or out-of-range value yields `None`, surfaced to JS as a `TypeError`.
    fn value_to_byte(value: Value) -> Option<u8> {
        match value {
            Value::Integer(i) => u8::try_from(i.into_i64()).ok(),
            _ => None,
        }
    }

    /// Read the `{ fatal, ignoreBOM }` options object: each flag defaults to `false`, and a missing or
    /// non-object second argument yields both defaults. Only own/inherited boolean reads are honoured;
    /// a non-boolean value for a flag is treated as `false` (we read it strictly rather than running
    /// full `ToBoolean`, keeping the path allocation-free).
    fn read_decode_options(agent: &mut Agent, value: Value, gc: NoGcScope) -> (bool, bool) {
        let Ok(obj) = Object::try_from(value) else {
            return (false, false);
        };
        let read_flag = |agent: &mut Agent, name: &'static str| -> bool {
            let key = PropertyKey::from_static_str(agent, name, gc);
            matches!(
                unwrap_try(obj.try_get(agent, key, value, None, gc)),
                TryGetResult::Value(Value::Boolean(true))
            )
        };
        let fatal = read_flag(agent, "fatal");
        let ignore_bom = read_flag(agent, "ignoreBOM");
        (fatal, ignore_bom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_is_zero_copy_borrow_of_str_bytes() {
        let s = "hello";
        let bytes = encode_utf8(s);
        // The encoder borrows the input's bytes — same pointer, no allocation.
        assert_eq!(bytes, b"hello");
        assert_eq!(bytes.as_ptr(), s.as_ptr());
    }

    #[test]
    fn encode_decode_utf8_round_trip_ascii_and_multibyte() {
        for original in ["", "hello world", "café", "naïve", "日本語", "emoji 🦀🚀", "mixed: a—b…c"] {
            let bytes = encode_utf8(original);
            let decoded = decode_utf8(bytes, false, false).unwrap();
            assert_eq!(decoded, original, "round-trip failed for {original:?}");
        }
    }

    #[test]
    fn encode_multibyte_byte_lengths_match_utf8() {
        // Sanity-check the actual byte expansion: U+00E9 (é) is 2 bytes, U+65E5 (日) is 3, crab is 4.
        assert_eq!(encode_utf8("é"), &[0xC3, 0xA9]);
        assert_eq!(encode_utf8("日"), &[0xE6, 0x97, 0xA5]);
        assert_eq!(encode_utf8("🦀"), &[0xF0, 0x9F, 0xA6, 0x80]);
    }

    #[test]
    fn decode_valid_utf8_borrows_without_allocation() {
        let bytes = b"plain ascii";
        let decoded = decode_utf8(bytes, false, false).unwrap();
        assert!(matches!(decoded, Cow::Borrowed(_)), "valid UTF-8 should borrow");
        assert_eq!(decoded, "plain ascii");
    }

    #[test]
    fn decode_strips_leading_bom_by_default() {
        let mut input = UTF8_BOM.to_vec();
        input.extend_from_slice("content".as_bytes());
        let decoded = decode_utf8(&input, false, false).unwrap();
        assert_eq!(decoded, "content", "the leading BOM must be stripped by default");
    }

    #[test]
    fn decode_keeps_bom_when_ignore_bom_set() {
        let mut input = UTF8_BOM.to_vec();
        input.extend_from_slice("content".as_bytes());
        let decoded = decode_utf8(&input, false, true).unwrap();
        // With ignoreBOM, the U+FEFF code point is preserved at the front.
        assert_eq!(decoded.chars().next(), Some('\u{FEFF}'));
        assert!(decoded.ends_with("content"));
    }

    #[test]
    fn decode_only_strips_a_single_leading_bom() {
        // A BOM that is not at the very start, or a second BOM, is preserved as U+FEFF.
        let mut input = UTF8_BOM.to_vec();
        input.extend_from_slice(&UTF8_BOM); // second BOM becomes content
        input.extend_from_slice("x".as_bytes());
        let decoded = decode_utf8(&input, false, false).unwrap();
        assert_eq!(decoded, "\u{FEFF}x");
    }

    #[test]
    fn decode_lossy_replaces_invalid_sequences() {
        // 0xFF is never valid in UTF-8; non-fatal decoding substitutes U+FFFD.
        let input = [b'a', 0xFF, b'b'];
        let decoded = decode_utf8(&input, false, false).unwrap();
        assert_eq!(decoded, "a\u{FFFD}b");
    }

    #[test]
    fn decode_fatal_errors_on_invalid_sequences() {
        let input = [b'a', 0xFF, b'b'];
        assert_eq!(decode_utf8(&input, true, false), Err(DecodeError::Invalid));
        // ...but fatal decoding of well-formed input still succeeds and borrows.
        let ok = decode_utf8(b"fine", true, false).unwrap();
        assert_eq!(ok, "fine");
    }

    #[test]
    fn decode_empty_input_is_empty_string() {
        assert_eq!(decode_utf8(&[], false, false).unwrap(), "");
        assert_eq!(decode_utf8(&[], true, true).unwrap(), "");
    }

    #[test]
    fn normalize_label_accepts_utf8_aliases_case_insensitively() {
        for label in ["utf-8", "UTF-8", "utf8", "  UtF8 ", "unicode-1-1-utf-8", "unicode20utf8"] {
            assert_eq!(normalize_label(label), Some("utf-8"), "label {label:?} should be UTF-8");
        }
    }

    #[test]
    fn normalize_label_rejects_unsupported_encodings() {
        for label in ["utf-16le", "iso-8859-1", "latin1", "", "ascii", "windows-1252"] {
            assert_eq!(normalize_label(label), None, "label {label:?} should be unsupported");
        }
    }
}
