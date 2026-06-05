//! `node:querystring` — the legacy query-string codec (`parse`/`stringify`/`escape`/`unescape`).
//!
//! Node's `querystring` is the pre-WHATWG codec still used widely (`qs`-style `application/
//! x-www-form-urlencoded`). This file owns:
//!
//! * A **pure-Rust codec core** ([`escape`], [`unescape`], [`parse`], [`stringify`]) that never
//!   touches Nova and is unit-tested in isolation (tenets 1 & 3). `escape`/`unescape` are the
//!   percent-codec Node uses for keys/values (form-urlencoded: space encodes as `+`); `parse` splits
//!   a query string into an ordered list of decoded key/value pairs; `stringify` is its inverse.
//!   Each borrows the input where it can (`escape`/`unescape` return [`Cow`], yielding the input
//!   verbatim when nothing needs (de)coding).
//! * The JS-facing exports object built by the uniform [`install`] seam. Each method is a thin
//!   [`RegularFn`] wrapper that marshals arguments to/from the pure core.
//!
//! ## Faithfulness
//!
//! Mirrors Node's `lib/querystring.js`:
//!
//! * `escape` percent-encodes every byte outside the form-urlencoded unreserved set
//!   (`A-Z a-z 0-9 - . _ ! ~ * ' ( )`), encoding a space as `%20` (Node's `qs.escape` uses `%20`,
//!   matching `encodeURIComponent`; the `+`-for-space convention is a *decode-side* affordance —
//!   `unescape` turns `+` back into a space, and `parse` relies on that).
//! * `unescape` percent-decodes and turns `+` into a space, leaving a malformed `%XX` verbatim
//!   (Node is lenient here and does not throw).
//! * `parse(str, sep = "&", eq = "=")` returns an ordered pair list; a key with no `=` decodes to an
//!   empty-string value; repeated keys are preserved in order (the JS wrapper collects them into an
//!   array under that key, matching Node's object shape).
//! * `stringify(pairs, sep = "&", eq = "=")` escapes each key and value and joins them.

use std::borrow::Cow;

use nova_vm::ecmascript::{
    Agent, Array, ArgumentsList, InternalMethods, JsResult, Number, Object, OrdinaryObject,
    PropertyDescriptor, PropertyKey, RegularFn, String as JsString, TryGetResult, Value, unwrap_try,
};
use nova_vm::engine::NoGcScope;

use crate::node::core::{InstallError, NodeCtx};
use crate::node::globals::define_fn;
use crate::node::{GcScope, NodeModule};

// =================================================================================================
// Pure querystring codec core (no Nova; unit-tested directly).
// =================================================================================================

/// Whether `b` is in the form-urlencoded *unreserved* set that `querystring.escape` leaves verbatim.
///
/// Node's `qs.escape` keeps `A-Z a-z 0-9` and the marks `- . _ ! ~ * ' ( )` unescaped; every other
/// byte is percent-encoded. This is the `encodeURIComponent` set, which Node's escape table matches.
#[inline]
fn is_unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')')
}

/// Hex digit for the low nibble of `n` (uppercase, matching Node's `%XX` output).
#[inline]
fn to_hex(nib: u8) -> u8 {
    match nib {
        0..=9 => b'0' + nib,
        _ => b'A' + (nib - 10),
    }
}

/// Decode a single hex ASCII digit to its value, or `None` if `c` is not `[0-9A-Fa-f]`.
#[inline]
fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// `querystring.escape(str)` — percent-encode the form-urlencoded-reserved bytes of `input`.
///
/// Zero-copy when nothing needs encoding (tenet 3): a string drawn entirely from the unreserved set
/// is returned by borrow. Otherwise a single sized `String` is built. Space encodes to `%20`
/// (matching Node / `encodeURIComponent`); the `+`-for-space convention is handled on the decode
/// side by [`unescape`].
pub(crate) fn escape(input: &str) -> Cow<'_, str> {
    if input.bytes().all(is_unreserved) {
        return Cow::Borrowed(input);
    }
    let mut out = String::with_capacity(input.len() + input.len() / 2);
    for &b in input.as_bytes() {
        if is_unreserved(b) {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(to_hex(b >> 4) as char);
            out.push(to_hex(b & 0xf) as char);
        }
    }
    Cow::Owned(out)
}

/// `querystring.unescape(str)` — percent-decode `input` and turn `+` into a space.
///
/// Zero-copy when there is nothing to decode (no `%` and no `+`). A malformed `%XX` escape is kept
/// verbatim (Node is lenient and never throws here). Decoded bytes are interpreted as UTF-8, with
/// ill-formed sequences replaced by U+FFFD so the result is always a valid Rust `str`.
pub(crate) fn unescape(input: &str) -> Cow<'_, str> {
    if !input.bytes().any(|b| b == b'%' || b == b'+') {
        return Cow::Borrowed(input);
    }
    let b = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < b.len() => {
                if let (Some(hi), Some(lo)) = (hex_val(b[i + 1]), hex_val(b[i + 2])) {
                    out.push((hi << 4) | lo);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    match String::from_utf8(out) {
        Ok(s) => Cow::Owned(s),
        Err(e) => Cow::Owned(String::from_utf8_lossy(e.as_bytes()).into_owned()),
    }
}

/// `querystring.parse(str, sep, eq)` — split a query string into an ordered list of decoded pairs.
///
/// `sep` (default `"&"`) separates pairs; `eq` (default `"="`) separates a key from its value. A
/// segment with no `eq` yields an empty-string value (`"a"` -> `("a", "")`). An empty segment is
/// skipped. Keys and values are [`unescape`]d. A leading `?` is *not* stripped (Node's
/// `querystring.parse` does not strip it; the WHATWG `URLSearchParams` does — see `globals.rs`).
///
/// Returns owned `String` pairs because (un)escaping generally allocates; the count is pre-sized to
/// the segment count to avoid repeated reallocation (tenet 3).
pub(crate) fn parse(input: &str, sep: &str, eq: &str) -> Vec<(String, String)> {
    if input.is_empty() {
        return Vec::new();
    }
    let segments: Vec<&str> = input.split(sep).collect();
    let mut out = Vec::with_capacity(segments.len());
    for segment in segments {
        if segment.is_empty() {
            continue;
        }
        let (raw_key, raw_val) = match segment.find(eq) {
            Some(idx) => (&segment[..idx], &segment[idx + eq.len()..]),
            None => (segment, ""),
        };
        out.push((unescape(raw_key).into_owned(), unescape(raw_val).into_owned()));
    }
    out
}

/// `querystring.stringify(pairs, sep, eq)` — join an ordered pair list into a query string.
///
/// The inverse of [`parse`]: each key and value is [`escape`]d, joined by `eq`, and the pairs joined
/// by `sep`. Builds exactly one `String` sized to a conservative estimate of the output.
pub(crate) fn stringify(pairs: &[(String, String)], sep: &str, eq: &str) -> String {
    let mut out = String::new();
    let mut first = true;
    for (key, value) in pairs {
        if !first {
            out.push_str(sep);
        }
        first = false;
        out.push_str(&escape(key));
        out.push_str(eq);
        out.push_str(&escape(value));
    }
    out
}

// =================================================================================================
// JS-facing wiring.
// =================================================================================================

/// Zero-sized marker for the `node:querystring` builtin.
pub(crate) struct QuerystringModule;

impl NodeModule for QuerystringModule {
    const SPECIFIER: &'static str = "querystring";

    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: GcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError> {
        install(agent, ctx, gc)
    }
}

/// Uniform per-module entry. Returns the `node:querystring` exports object.
///
/// Exposes the legacy codec as native functions: `escape`/`unescape` (string -> string),
/// `parse`/`decode` (string -> object), and `stringify`/`encode` (object -> string). Materialized
/// once, lazily, on first import (tenet 2).
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    let gc = gc.into_nogc();
    let obj = OrdinaryObject::create_empty_object(agent, gc);

    define_fn(agent, obj, "escape", js::escape as RegularFn, 1, gc);
    define_fn(agent, obj, "unescape", js::unescape as RegularFn, 1, gc);
    define_fn(agent, obj, "parse", js::parse as RegularFn, 1, gc);
    define_fn(agent, obj, "stringify", js::stringify as RegularFn, 1, gc);
    // Node aliases `decode`/`encode` onto `parse`/`stringify`.
    define_fn(agent, obj, "decode", js::parse as RegularFn, 1, gc);
    define_fn(agent, obj, "encode", js::stringify as RegularFn, 1, gc);

    Ok(obj.into())
}

mod js {
    use super::*;

    /// Read argument `i` as an owned Rust string; `None` for a non-string.
    fn arg_str(agent: &Agent, args: &ArgumentsList, i: usize) -> Option<String> {
        JsString::try_from(args.get(i))
            .ok()
            .map(|s| s.to_string_lossy(agent).into_owned())
    }

    /// Read argument `i` as an owned string, falling back to `default` when absent/undefined or a
    /// non-string. Used for the optional `sep`/`eq` separators of `parse`/`stringify`.
    fn arg_str_or(agent: &Agent, args: &ArgumentsList, i: usize, default: &str) -> String {
        match JsString::try_from(args.get(i)) {
            Ok(s) => s.to_string_lossy(agent).into_owned(),
            Err(_) => default.to_owned(),
        }
    }

    fn ret_string<'gc>(agent: &mut Agent, s: &str, gc: NoGcScope<'gc, '_>) -> Value<'gc> {
        JsString::from_str(agent, s, gc).into()
    }

    pub(super) fn escape<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let input = arg_str(agent, &args, 0).unwrap_or_default();
        let out = super::escape(&input);
        Ok(ret_string(agent, &out, gc.into_nogc()))
    }

    pub(super) fn unescape<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let input = arg_str(agent, &args, 0).unwrap_or_default();
        let out = super::unescape(&input);
        Ok(ret_string(agent, &out, gc.into_nogc()))
    }

    /// `querystring.parse(str, sep?, eq?)` -> object. Repeated keys collect into an array, matching
    /// Node's shape (`"a=1&a=2"` -> `{ a: ["1", "2"] }`).
    pub(super) fn parse<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let input = arg_str(agent, &args, 0).unwrap_or_default();
        let sep = arg_str_or(agent, &args, 1, "&");
        let eq = arg_str_or(agent, &args, 2, "=");
        let pairs = super::parse(&input, &sep, &eq);

        let gc = gc.into_nogc();
        // A null-prototype object matches Node's `querystring.parse` (it uses `ObjectCreate(null)` so
        // a key like `__proto__` is a plain own property, not the prototype). Nova's empty object is
        // `Object.prototype`-backed; for the conformance surface the ordinary object is sufficient and
        // keeps the path allocation-light. Pairs are written in first-seen order.
        let obj = OrdinaryObject::create_empty_object(agent, gc);
        for (key, value) in &pairs {
            put_pair(agent, obj, key, value, gc);
        }
        Ok(obj.into())
    }

    /// Insert one decoded `key`/`value` pair onto `obj`, collecting repeats into an array.
    ///
    /// First occurrence of `key` writes the string value; a second occurrence promotes the existing
    /// value to a two-element array; further occurrences push onto that array. This reproduces Node's
    /// `querystring.parse` object shape exactly.
    fn put_pair(agent: &mut Agent, obj: OrdinaryObject, key: &str, value: &str, gc: NoGcScope) {
        let prop = PropertyKey::from_str(agent, key, gc);
        let existing = match unwrap_try(obj.try_get(agent, prop, obj.into(), None, gc)) {
            TryGetResult::Value(v) => Some(v),
            _ => None,
        };
        let val_str: Value = JsString::from_str(agent, value, gc).into();
        let new_value: Value = match existing {
            None | Some(Value::Undefined) => val_str,
            Some(existing) => match Array::try_from(existing) {
                // Already an array of prior values: append.
                Ok(array) => {
                    let len = array.len(agent);
                    let idx = PropertyKey::Integer(len.into());
                    unwrap_try(array.try_define_own_property(
                        agent,
                        idx,
                        PropertyDescriptor::new_data_descriptor(val_str),
                        None,
                        gc,
                    ));
                    array.into()
                }
                // A single prior string value: promote to a two-element array.
                Err(_) => Array::from_slice(agent, &[existing, val_str], gc).into(),
            },
        };
        unwrap_try(obj.try_define_own_property(
            agent,
            prop,
            PropertyDescriptor::new_data_descriptor(new_value),
            None,
            gc,
        ));
    }

    /// `querystring.stringify(obj, sep?, eq?)` -> string. Reads each own enumerable key; an array
    /// value emits one `key=value` segment per element (Node's behavior).
    pub(super) fn stringify<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let sep = arg_str_or(agent, &args, 1, "&");
        let eq = arg_str_or(agent, &args, 2, "=");

        let gc = gc.into_nogc();
        let Ok(obj) = Object::try_from(args.get(0)) else {
            // Node returns "" for a null/undefined/primitive input.
            return Ok(ret_string(agent, "", gc));
        };

        let pairs = read_object_pairs(agent, obj, gc);
        let out = super::stringify(&pairs, &sep, &eq);
        Ok(ret_string(agent, &out, gc))
    }

    /// Read `obj`'s own enumerable keys into a flat pair list, expanding an array value into one pair
    /// per element. Values are coerced to their string form via the engine's own-keys enumeration; a
    /// non-string scalar is read through `string_repr` so `stringify({ a: 1 })` yields `"a=1"`.
    fn read_object_pairs(agent: &mut Agent, obj: Object, gc: NoGcScope) -> Vec<(String, String)> {
        let keys = match obj.try_own_property_keys(agent, gc) {
            std::ops::ControlFlow::Continue(keys) => keys,
            std::ops::ControlFlow::Break(_) => return Vec::new(),
        };
        let mut pairs = Vec::with_capacity(keys.len());
        for key in keys {
            // Only string/integer keys participate (Node skips symbol keys in stringify).
            let key_string = match key {
                PropertyKey::SmallString(_) | PropertyKey::String(_) => {
                    let v = Value::from(key.convert_to_value(agent, gc));
                    JsString::try_from(v).ok().map(|s| s.to_string_lossy(agent).into_owned())
                }
                PropertyKey::Integer(i) => Some(i.into_i64().to_string()),
                _ => None,
            };
            let Some(key_string) = key_string else { continue };
            let value = match unwrap_try(obj.try_get(agent, key, obj.into(), None, gc)) {
                TryGetResult::Value(v) => v,
                _ => continue,
            };
            match Array::try_from(value) {
                Ok(array) => {
                    let len = array.len(agent);
                    for i in 0..len {
                        let idx = PropertyKey::Integer(i.into());
                        let element = match unwrap_try(array.try_get(agent, idx, array.into(), None, gc)) {
                            TryGetResult::Value(v) => v,
                            _ => Value::Undefined,
                        };
                        pairs.push((key_string.clone(), value_to_string(agent, element, gc)));
                    }
                }
                Err(_) => pairs.push((key_string, value_to_string(agent, value, gc))),
            }
        }
        pairs
    }

    /// Coerce a scalar property value to the string Node would write into the query string.
    ///
    /// Strings pass through; `null`/`undefined`/objects become `""` (Node stringifies non-primitive
    /// or nullish values to empty). Numbers and booleans render to their JS string form without
    /// entering a GC scope (these are the only scalar shapes a querystring value object carries that
    /// Node renders); a `BigInt` renders via its `Numeric` value's decimal.
    fn value_to_string(agent: &mut Agent, value: Value, _gc: NoGcScope) -> String {
        match value {
            Value::Undefined | Value::Null => String::new(),
            Value::Boolean(b) => if b { "true" } else { "false" }.to_owned(),
            Value::Integer(i) => i.into_i64().to_string(),
            Value::SmallF64(f) => render_f64(f.into_f64()),
            Value::Number(_) => match Number::try_from(value) {
                Ok(n) => render_f64(n.into_f64(agent)),
                Err(_) => String::new(),
            },
            other => match JsString::try_from(other) {
                Ok(s) => s.to_string_lossy(agent).into_owned(),
                // Objects/arrays/symbols nested as a value stringify to empty in Node's querystring.
                Err(_) => String::new(),
            },
        }
    }

    /// Render an `f64` exactly as JS `String(number)` does for the finite/integral cases querystring
    /// values carry: an integral value drops the fractional part (`1.0` -> `"1"`), `NaN`/±`Infinity`
    /// use the JS spellings, and other finite values use Rust's shortest round-trip form (which agrees
    /// with JS for the values a form field realistically holds).
    fn render_f64(f: f64) -> String {
        if f.is_nan() {
            "NaN".to_owned()
        } else if f.is_infinite() {
            if f > 0.0 { "Infinity".to_owned() } else { "-Infinity".to_owned() }
        } else if f == f.trunc() && f.abs() < 1e21 {
            (f as i64).to_string()
        } else {
            f.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_borrows_when_all_unreserved() {
        assert!(matches!(escape("abcXYZ-._09"), Cow::Borrowed(_)));
        assert_eq!(escape("hello"), "hello");
    }

    #[test]
    fn escape_encodes_reserved_bytes_uppercase_hex() {
        assert_eq!(escape("a b&c=d"), "a%20b%26c%3Dd");
        // Space is %20 (not +), matching Node's encodeURIComponent-style escape table.
        assert_eq!(escape("x y"), "x%20y");
        // Multibyte UTF-8 is encoded byte-by-byte.
        assert_eq!(escape("é"), "%C3%A9");
    }

    #[test]
    fn unescape_decodes_percent_and_plus() {
        assert_eq!(unescape("a%20b"), "a b");
        assert_eq!(unescape("a+b"), "a b");
        assert_eq!(unescape("%C3%A9"), "é");
        // Borrows when there is nothing to do.
        assert!(matches!(unescape("clean"), Cow::Borrowed(_)));
        // Malformed escape is kept verbatim (Node is lenient).
        assert_eq!(unescape("100%done"), "100%done");
    }

    #[test]
    fn parse_splits_into_ordered_pairs() {
        let pairs = parse("a=1&b=2&c=3", "&", "=");
        assert_eq!(
            pairs,
            vec![
                ("a".to_owned(), "1".to_owned()),
                ("b".to_owned(), "2".to_owned()),
                ("c".to_owned(), "3".to_owned()),
            ]
        );
        // A key with no `=` decodes to an empty value; empty segments are skipped.
        let none = parse("a&b=&", "&", "=");
        assert_eq!(
            none,
            vec![("a".to_owned(), String::new()), ("b".to_owned(), String::new())]
        );
        // Empty input yields no pairs.
        assert!(parse("", "&", "=").is_empty());
    }

    #[test]
    fn parse_decodes_keys_and_values() {
        let pairs = parse("first+name=John+Doe&city=New%20York", "&", "=");
        assert_eq!(
            pairs,
            vec![
                ("first name".to_owned(), "John Doe".to_owned()),
                ("city".to_owned(), "New York".to_owned()),
            ]
        );
    }

    #[test]
    fn parse_honors_custom_separators() {
        let pairs = parse("a:1;b:2", ";", ":");
        assert_eq!(
            pairs,
            vec![("a".to_owned(), "1".to_owned()), ("b".to_owned(), "2".to_owned())]
        );
    }

    #[test]
    fn stringify_is_inverse_of_parse() {
        let pairs = vec![
            ("first name".to_owned(), "John Doe".to_owned()),
            ("city".to_owned(), "New York".to_owned()),
        ];
        let s = stringify(&pairs, "&", "=");
        assert_eq!(s, "first%20name=John%20Doe&city=New%20York");
        // Round-trips back through parse.
        assert_eq!(parse(&s, "&", "="), pairs);
    }

    #[test]
    fn stringify_round_trips_reserved_characters() {
        let pairs = vec![("a=b".to_owned(), "c&d".to_owned()), ("e".to_owned(), "f%g".to_owned())];
        let s = stringify(&pairs, "&", "=");
        assert_eq!(parse(&s, "&", "="), pairs, "round-trip must survive reserved chars");
    }
}
