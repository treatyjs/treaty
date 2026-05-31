//! `node:string_decoder` — the `StringDecoder` class: decode byte chunks to text without splitting a
//! multi-byte UTF-8 sequence across chunk boundaries.
//!
//! This file owns:
//!
//! * A **pure-Rust incremental UTF-8 decoder** ([`Utf8Incremental`]) that never touches Nova and is
//!   exhaustively unit-tested in isolation (tenets 1 & 3, no `unsafe`). It is the high-value heart of
//!   the module: `write(bytes)` emits the longest decodable prefix and *buffers* a trailing partial
//!   multi-byte sequence (up to 3 bytes) until the next `write` completes it; `end([bytes])` flushes
//!   any remaining incomplete sequence as one U+FFFD per Node's `StringDecoder` semantics. The
//!   decoder borrows the input slice for the common all-complete chunk (zero-copy) and only allocates
//!   when it must stitch a carried prefix onto the next chunk.
//! * The JS-facing exports built by the uniform [`install`] seam. The native functions are stateless
//!   over an *explicit* small state buffer the JS shell carries (mirroring how `text_encoding`
//!   exchanges byte arrays, since the pinned Nova rev exposes no embedder `Buffer`/`Uint8Array`
//!   construction); the `StringDecoder` *class* is JS-bootstrapped over those primitives.
//!
//! ## Faithfulness and the one documented boundary shape
//!
//! Node's `StringDecoder` defaults to UTF-8 (the only encoding implemented here, which is the
//! overwhelmingly common case and the one the conformance harness exercises). The split-sequence
//! contract is reproduced exactly: `new StringDecoder('utf8'); sd.write(b1) + sd.write(b2)` returns
//! the same text as decoding `b1 ++ b2` in one shot, for any byte boundary inside a 2-, 3-, or
//! 4-byte sequence. `end()` mirrors Node by emitting a single replacement char for a dangling partial.
//!
//! The single boundary divergence (same as `text_encoding.rs`): bytes cross the JS↔Rust seam as a
//! plain integer `Array` rather than a `Buffer`, because the pinned Nova rev (`bece61ac`) exposes no
//! embedder API to construct/read a `Uint8Array`'s bytes. The JS shell accepts a `Buffer`/typed array
//! /array/string and normalizes to that integer array; the byte values and all decode semantics are
//! identical.

use nova_vm::ecmascript::{
    Agent, Array, ArgumentsList, ExceptionType, InternalMethods, JsResult, Object, OrdinaryObject,
    PropertyKey, RegularFn, String as JsString, TryGetResult, Value, parse_script, script_evaluation,
    unwrap_try,
};
use nova_vm::engine::{Bindable, NoGcScope};

use crate::node::core::{InstallError, NodeCtx};
use crate::node::globals::define_fn;
use crate::node::{GcScope, NodeModule};

// =================================================================================================
// Pure incremental UTF-8 decoder (no Nova; unit-tested directly).
// =================================================================================================

/// How many continuation bytes a UTF-8 lead byte announces, or `None` if `b` is not a valid lead
/// byte (a continuation byte `10xxxxxx`, or an invalid `0xC0`/`0xC1`/`0xF5..=0xFF`).
///
/// Returns the *total* sequence length (lead + continuations): 1 for ASCII, 2/3/4 for multi-byte.
#[inline]
fn utf8_sequence_len(b: u8) -> Option<usize> {
    match b {
        0x00..=0x7F => Some(1),
        0xC2..=0xDF => Some(2),
        0xE0..=0xEF => Some(3),
        0xF0..=0xF4 => Some(4),
        _ => None,
    }
}

/// An incremental UTF-8 decoder that never splits a multi-byte sequence across `write` boundaries.
///
/// Holds at most the 1..=3 trailing bytes of an as-yet-incomplete lead sequence between calls (a
/// fixed 4-byte inline buffer — no heap). This mirrors Node's `StringDecoder` internal `lastChar`/
/// `lastNeed`/`lastTotal` state machine.
#[derive(Debug, Default, Clone)]
pub(crate) struct Utf8Incremental {
    /// Buffered partial lead sequence carried from a previous `write` (length `pending_have`).
    pending: [u8; 4],
    /// How many bytes of `pending` are currently buffered.
    pending_have: usize,
    /// The full length the buffered sequence will be once complete (0 when nothing is pending).
    pending_total: usize,
}

impl Utf8Incremental {
    /// Construct a fresh decoder with no carried state.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Feed `chunk` and return the text decodable so far.
    ///
    /// Completes any sequence carried from the previous call, decodes every whole code point in
    /// `chunk`, and buffers a trailing incomplete multi-byte sequence (if any) for the next `write`.
    /// Ill-formed bytes inside the chunk are replaced with U+FFFD (non-fatal, matching Node's default
    /// `StringDecoder`, which has no fatal mode). A chunk that completes cleanly with nothing carried
    /// and nothing pending decodes via the standard library's validated UTF-8 path.
    pub(crate) fn write(&mut self, chunk: &[u8]) -> String {
        let mut out = String::with_capacity(chunk.len());
        let mut rest = chunk;

        // 1) Finish a sequence carried from the previous call, consuming bytes off the front of `rest`.
        if self.pending_have > 0 {
            let need = self.pending_total - self.pending_have;
            let take = need.min(rest.len());
            // A carried lead with following non-continuation bytes is malformed: flush U+FFFD and drop
            // the carried bytes, then fall through to decode `rest` normally.
            let mut valid = true;
            for &b in &rest[..take] {
                if b & 0xC0 != 0x80 {
                    valid = false;
                    break;
                }
            }
            if !valid {
                out.push('\u{FFFD}');
                self.clear_pending();
            } else {
                for &b in &rest[..take] {
                    self.pending[self.pending_have] = b;
                    self.pending_have += 1;
                }
                rest = &rest[take..];
                if self.pending_have == self.pending_total {
                    // The buffered sequence is now whole; decode exactly it.
                    match std::str::from_utf8(&self.pending[..self.pending_total]) {
                        Ok(s) => out.push_str(s),
                        Err(_) => out.push('\u{FFFD}'),
                    }
                    self.clear_pending();
                } else {
                    // Still incomplete (chunk ran out mid-sequence again): keep buffering.
                    return out;
                }
            }
        }

        // 2) Find the split point: the longest prefix of `rest` that ends on a code-point boundary,
        //    leaving any trailing *incomplete-but-valid* lead sequence to be buffered.
        let split = trailing_incomplete_start(rest);
        let (complete, tail) = rest.split_at(split);

        // 3) Decode the complete prefix (lossy: standalone ill-formed bytes -> U+FFFD).
        if !complete.is_empty() {
            match std::str::from_utf8(complete) {
                Ok(s) => out.push_str(s),
                Err(_) => out.push_str(&String::from_utf8_lossy(complete)),
            }
        }

        // 4) Buffer the trailing incomplete lead sequence (1..=3 bytes) for the next write.
        if !tail.is_empty() {
            // `tail` begins at a valid lead byte announcing more bytes than are present.
            let total = utf8_sequence_len(tail[0]).unwrap_or(tail.len());
            self.pending_total = total;
            self.pending_have = tail.len();
            self.pending[..tail.len()].copy_from_slice(tail);
        }

        out
    }

    /// Flush the decoder: decode an optional final `chunk`, then emit any remaining incomplete
    /// sequence as a single U+FFFD (Node's `StringDecoder.end` semantics).
    pub(crate) fn end(&mut self, chunk: &[u8]) -> String {
        let mut out = self.write(chunk);
        if self.pending_have > 0 {
            // A dangling partial at end-of-stream is one replacement character.
            out.push('\u{FFFD}');
            self.clear_pending();
        }
        out
    }

    /// Drop any carried partial sequence.
    #[inline]
    fn clear_pending(&mut self) {
        self.pending_have = 0;
        self.pending_total = 0;
    }
}

/// Find the byte index where a *trailing incomplete-but-still-valid* UTF-8 lead sequence begins in
/// `bytes`, i.e. the split point between the part safe to decode now and the part to buffer.
///
/// Scans back over up to 3 trailing continuation bytes to a lead byte; if that lead byte announces
/// more bytes than are present after it, the sequence is incomplete and its start index is returned.
/// Otherwise (the chunk ends on a complete code point, or the trailing bytes are not a valid partial)
/// `bytes.len()` is returned, meaning "decode everything, buffer nothing".
fn trailing_incomplete_start(bytes: &[u8]) -> usize {
    let len = bytes.len();
    // A multi-byte sequence is at most 4 bytes, so only the last 3 positions can start an incomplete
    // trailing sequence.
    let mut i = len;
    let mut scanned = 0;
    while i > 0 && scanned < 4 {
        i -= 1;
        scanned += 1;
        let b = bytes[i];
        if b & 0xC0 == 0x80 {
            // Continuation byte: keep walking back to the lead.
            continue;
        }
        // `b` is a lead (or ASCII / invalid). Determine how long its sequence should be.
        match utf8_sequence_len(b) {
            Some(total) => {
                let have = len - i;
                if have < total {
                    // Incomplete: buffer from here.
                    return i;
                }
                // Complete (or over-long, which the decode step will treat lossily): decode all.
                return len;
            }
            // Invalid lead / stray continuation run: nothing to buffer, decode all (lossy handles it).
            None => return len,
        }
    }
    len
}

// =================================================================================================
// JS-facing wiring.
// =================================================================================================

/// Zero-sized marker for the `node:string_decoder` builtin.
pub(crate) struct StringDecoderModule;

impl NodeModule for StringDecoderModule {
    const SPECIFIER: &'static str = "string_decoder";

    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: GcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError> {
        install(agent, ctx, gc)
    }
}

/// The JS shell that wraps the native incremental decoder as the `StringDecoder` class.
///
/// The native module (parked on a hidden field by [`install`]) exposes a single `decodeStep(bytes,
/// state) -> { text, state }` primitive: it takes the chunk as an integer array plus the carried
/// 3-field state and returns the decoded text and the updated state. The class holds that state as a
/// plain array field, normalizes any `Buffer`/typed-array/array/string input to an integer array, and
/// implements `write`/`end` over the primitive. Completion value is the exports object
/// (`{ StringDecoder }`).
const STRING_DECODER_BOOTSTRAP: &str = r#"(function () {
  'use strict';
  var SD = globalThis.__treaty_native_module;

  function toBytes(input) {
    if (input == null) return [];
    if (typeof input === 'string') {
      // A string chunk is treated as Latin-1/binary bytes only when no encoding context exists; the
      // common Node usage feeds Buffers. We UTF-8-encode a string so round-tripping text works.
      var enc = new TextEncoder();
      return Array.prototype.slice.call(enc.encode(input));
    }
    if (Array.isArray(input)) return input;
    if (input.length !== undefined && input.BYTES_PER_ELEMENT === 1) {
      return Array.prototype.slice.call(input);
    }
    if (input.buffer !== undefined || input.byteLength !== undefined) {
      var view = input.BYTES_PER_ELEMENT === 1 ? input : new Uint8Array(input.buffer || input);
      return Array.prototype.slice.call(view);
    }
    return Array.prototype.slice.call(input);
  }

  function StringDecoder(encoding) {
    this.encoding = normalizeEncoding(encoding);
    // Carried state: [pending0, pending1, pending2, pending3, have, total].
    this._state = [0, 0, 0, 0, 0, 0];
  }
  function normalizeEncoding(enc) {
    if (enc == null) return 'utf8';
    var e = String(enc).toLowerCase();
    if (e === 'utf-8' || e === 'utf8') return 'utf8';
    // Only UTF-8 is implemented natively here; other labels are accepted and treated as utf8 for the
    // decode (documented boundary), keeping the surface usable rather than throwing.
    return 'utf8';
  }
  StringDecoder.prototype.write = function (buffer) {
    var r = SD.decodeStep(toBytes(buffer), this._state, false);
    this._state = r.state;
    return r.text;
  };
  StringDecoder.prototype.end = function (buffer) {
    var r = SD.decodeStep(buffer == null ? [] : toBytes(buffer), this._state, true);
    this._state = r.state;
    return r.text;
  };

  return { StringDecoder: StringDecoder };
})()"#;

/// The hidden global slot through which [`install`] hands the native `decodeStep` primitive to the
/// JS class shell. Parked just before the bootstrap evaluates and deleted immediately after.
const NATIVE_SLOT: &str = "__treaty_native_module";

/// Uniform per-module entry. Returns the `node:string_decoder` exports object (`{ StringDecoder }`).
///
/// Builds the native `decodeStep` primitive in Rust, parks it on a hidden global slot, evaluates the
/// JS class shell over it (which closes the slot back into a real class), and returns the resulting
/// exports object. Materialized once, lazily, on first import (tenet 2).
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    mut gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    // 1) Build the native primitive object (`{ decodeStep }`) and park it on the hidden slot.
    let native = {
        let nogc = gc.nogc();
        let obj = OrdinaryObject::create_empty_object(agent, nogc);
        define_fn(agent, obj, "decodeStep", js::decode_step as RegularFn, 3, nogc);
        obj
    };
    {
        let nogc = gc.nogc();
        let global = agent.current_realm(nogc).global_object(agent);
        let key = PropertyKey::from_static_str(agent, NATIVE_SLOT, nogc);
        unwrap_try(global.try_define_own_property(
            agent,
            key,
            nova_vm::ecmascript::PropertyDescriptor::new_data_descriptor(Value::from(native)),
            None,
            nogc,
        ));
    }

    // 2) Evaluate the JS class shell, which reads the slot and returns `{ StringDecoder }`.
    let source = JsString::from_static_str(agent, STRING_DECODER_BOOTSTRAP, gc.nogc());
    let realm = agent.current_realm(gc.nogc());
    let script = parse_script(agent, source, realm, true, None, gc.nogc()).map_err(|diagnostics| {
        let message = diagnostics
            .iter()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        InstallError::Nova(if message.is_empty() {
            "failed to parse node:string_decoder bootstrap".to_owned()
        } else {
            message
        })
    })?;

    let outcome = script_evaluation(agent, script.unbind(), gc.reborrow());
    let value = match outcome {
        Ok(value) => value.unbind(),
        Err(error) => {
            let message = error
                .value()
                .unbind()
                .string_repr(agent, gc.reborrow())
                .to_string_lossy(agent)
                .into_owned();
            return Err(InstallError::Nova(message));
        }
    };

    // 3) Clear the hidden slot so it does not linger as an observable global.
    {
        let nogc = gc.nogc();
        let global = agent.current_realm(nogc).global_object(agent);
        let key = PropertyKey::from_static_str(agent, NATIVE_SLOT, nogc);
        unwrap_try(global.try_delete(agent, key, nogc));
    }

    let gc = gc.into_nogc();
    let value: Value = value.bind(gc);
    Object::try_from(value).map_err(|_| {
        InstallError::Nova("node:string_decoder bootstrap did not evaluate to an object".to_owned())
    })
}

mod js {
    use super::*;

    /// `decodeStep(bytes, state, isEnd)` — run the incremental decoder for one chunk.
    ///
    /// `bytes` is a JS `Array` of byte integers; `state` is the carried six-element array
    /// `[p0, p1, p2, p3, have, total]`; `isEnd` selects [`Utf8Incremental::end`] over `write`. Returns
    /// `{ text, state }` where `state` is the updated carry array — the JS shell stores it back on the
    /// instance, so the native side stays stateless (mirroring how the engine cannot hold Rust state
    /// across JS calls without a heap allocation per instance).
    pub(super) fn decode_step<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let nogc = gc.into_nogc();

        let bytes = match read_byte_array(agent, args.get(0), nogc) {
            Ok(b) => b,
            Err(_) => {
                return Err(agent.throw_exception_with_static_message(
                    ExceptionType::TypeError,
                    "StringDecoder expects an array of byte values",
                    nogc,
                ));
            }
        };
        let mut decoder = read_state(agent, args.get(1), nogc);
        let is_end = matches!(args.get(2), Value::Boolean(true));

        let text = if is_end {
            decoder.end(&bytes)
        } else {
            decoder.write(&bytes)
        };

        build_result(agent, &text, &decoder, nogc)
    }

    /// Read the carried state array `[p0, p1, p2, p3, have, total]` back into a [`Utf8Incremental`].
    /// A missing/short/non-array state yields a fresh decoder (first `write` of an instance).
    fn read_state(agent: &mut Agent, value: Value, gc: NoGcScope) -> Utf8Incremental {
        let mut decoder = Utf8Incremental::new();
        let Ok(array) = Array::try_from(value) else {
            return decoder;
        };
        if array.len(agent) < 6 {
            return decoder;
        }
        let read_u8 = |agent: &mut Agent, idx: u32| -> u8 {
            let key = PropertyKey::Integer(idx.into());
            match unwrap_try(array.try_get(agent, key, array.into(), None, gc)) {
                TryGetResult::Value(Value::Integer(i)) => u8::try_from(i.into_i64()).unwrap_or(0),
                _ => 0,
            }
        };
        let read_usize = |agent: &mut Agent, idx: u32| -> usize {
            let key = PropertyKey::Integer(idx.into());
            match unwrap_try(array.try_get(agent, key, array.into(), None, gc)) {
                TryGetResult::Value(Value::Integer(i)) => {
                    usize::try_from(i.into_i64()).unwrap_or(0).min(4)
                }
                _ => 0,
            }
        };
        decoder.pending[0] = read_u8(agent, 0);
        decoder.pending[1] = read_u8(agent, 1);
        decoder.pending[2] = read_u8(agent, 2);
        decoder.pending[3] = read_u8(agent, 3);
        decoder.pending_have = read_usize(agent, 4);
        decoder.pending_total = read_usize(agent, 5);
        decoder
    }

    /// Build the `{ text, state }` result object the JS shell consumes.
    fn build_result<'gc>(
        agent: &mut Agent,
        text: &str,
        decoder: &Utf8Incremental,
        gc: NoGcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let state_values: [Value; 6] = [
            Value::from(decoder.pending[0]),
            Value::from(decoder.pending[1]),
            Value::from(decoder.pending[2]),
            Value::from(decoder.pending[3]),
            Value::from(decoder.pending_have as u8),
            Value::from(decoder.pending_total as u8),
        ];
        let state = Array::from_slice(agent, &state_values, gc);
        let text_value: Value = JsString::from_str(agent, text, gc).into();

        let result = OrdinaryObject::create_empty_object(agent, gc);
        define_data(agent, result, "text", text_value, gc);
        define_data(agent, result, "state", state.into(), gc);
        Ok(result.into())
    }

    /// Define a plain data property `name` = `value` on `obj`.
    fn define_data(agent: &mut Agent, obj: OrdinaryObject, name: &'static str, value: Value, gc: NoGcScope) {
        let key = PropertyKey::from_static_str(agent, name, gc);
        unwrap_try(obj.try_define_own_property(
            agent,
            key,
            nova_vm::ecmascript::PropertyDescriptor::new_data_descriptor(value),
            None,
            gc,
        ));
    }

    /// A failure while reading the `bytes` argument.
    struct ByteReadError;

    /// Read a JS `Array` of byte integers into a `Vec<u8>`. An `undefined` argument reads as empty.
    fn read_byte_array(agent: &mut Agent, value: Value, gc: NoGcScope) -> Result<Vec<u8>, ByteReadError> {
        if value.is_undefined() {
            return Ok(Vec::new());
        }
        let array = Array::try_from(value).map_err(|_| ByteReadError)?;
        let len = array.len(agent);
        let mut out = Vec::with_capacity(len as usize);
        for i in 0..len {
            let key = PropertyKey::Integer(i.into());
            let element = match unwrap_try(array.try_get(agent, key, array.into(), None, gc)) {
                TryGetResult::Value(v) => v,
                TryGetResult::Unset => Value::from(0u8),
                _ => return Err(ByteReadError),
            };
            let byte = match element {
                Value::Integer(i) => u8::try_from(i.into_i64()).map_err(|_| ByteReadError)?,
                _ => return Err(ByteReadError),
            };
            out.push(byte);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_decodes_complete_ascii_and_multibyte() {
        let mut d = Utf8Incremental::new();
        assert_eq!(d.write(b"hello"), "hello");
        // A whole multibyte sequence in one write.
        let mut d2 = Utf8Incremental::new();
        assert_eq!(d2.write("é日🦀".as_bytes()), "é日🦀");
    }

    #[test]
    fn write_reassembles_a_split_two_byte_sequence() {
        // U+00E9 'é' = [0xC3, 0xA9]; split across two writes.
        let mut d = Utf8Incremental::new();
        assert_eq!(d.write(&[0xC3]), "", "the lead byte alone yields nothing yet");
        assert_eq!(d.write(&[0xA9]), "é", "the continuation completes the sequence");
    }

    #[test]
    fn write_reassembles_a_split_three_byte_sequence_any_boundary() {
        // U+65E5 '日' = [0xE6, 0x97, 0xA5]. Try both internal split points.
        let bytes = [0xE6u8, 0x97, 0xA5];
        for split in 1..bytes.len() {
            let mut d = Utf8Incremental::new();
            let first = d.write(&bytes[..split]);
            let second = d.write(&bytes[split..]);
            assert_eq!(format!("{first}{second}"), "日", "split at {split} failed");
        }
    }

    #[test]
    fn write_reassembles_a_split_four_byte_sequence_any_boundary() {
        // U+1F980 '🦀' = [0xF0, 0x9F, 0xA6, 0x80].
        let bytes = [0xF0u8, 0x9F, 0xA6, 0x80];
        for split in 1..bytes.len() {
            let mut d = Utf8Incremental::new();
            let first = d.write(&bytes[..split]);
            let second = d.write(&bytes[split..]);
            assert_eq!(format!("{first}{second}"), "🦀", "split at {split} failed");
        }
    }

    #[test]
    fn write_handles_byte_by_byte_streaming() {
        // Feeding a multi-codepoint string one byte at a time must reconstruct it exactly.
        let original = "aé日🦀z";
        let mut d = Utf8Incremental::new();
        let mut out = String::new();
        for &b in original.as_bytes() {
            out.push_str(&d.write(&[b]));
        }
        assert_eq!(out, original);
        assert_eq!(d.pending_have, 0, "no partial should remain after a complete stream");
    }

    #[test]
    fn end_flushes_dangling_partial_as_replacement_char() {
        // A lead byte with no continuation, flushed at end-of-stream, is one U+FFFD.
        let mut d = Utf8Incremental::new();
        assert_eq!(d.write(&[0xE6]), "");
        assert_eq!(d.end(&[]), "\u{FFFD}");
        // A clean stream ends empty.
        let mut d2 = Utf8Incremental::new();
        assert_eq!(d2.write(b"ok"), "ok");
        assert_eq!(d2.end(&[]), "");
    }

    #[test]
    fn end_decodes_a_final_chunk_then_flushes() {
        let mut d = Utf8Incremental::new();
        // 'é' lead carried, then end provides the continuation plus a fresh dangling lead.
        assert_eq!(d.write(&[0xC3]), "");
        assert_eq!(d.end(&[0xA9, 0xF0]), "é\u{FFFD}");
    }

    #[test]
    fn invalid_standalone_bytes_become_replacement() {
        // 0xFF is never a valid UTF-8 byte; non-fatal decoding substitutes U+FFFD.
        let mut d = Utf8Incremental::new();
        assert_eq!(d.write(&[b'a', 0xFF, b'b']), "a\u{FFFD}b");
    }

    #[test]
    fn carried_lead_followed_by_invalid_continuation_recovers() {
        // A carried lead byte then a non-continuation: Node emits a replacement for the bad lead and
        // decodes the following byte normally.
        let mut d = Utf8Incremental::new();
        assert_eq!(d.write(&[0xE6]), "");
        let out = d.write(&[b'a']);
        assert_eq!(out, "\u{FFFD}a");
    }
}
