//! `node:fetch` — the WHATWG / WinterCG `fetch` family: `Headers`, `Request`, `Response`, and the
//! `fetch()` entry point. Surfaced as lazy globals; this module backs them.
//!
//! ## What this file owns, and the one structural divergence
//!
//! The high-value, network-independent heart of the fetch family is a **pure-Rust header model** and
//! the WHATWG **method / status normalization** rules. Those are implemented here in full, touch no
//! network and no Nova, and are exhaustively unit-tested in isolation (tenets 1 + 3):
//!
//! * [`HeaderList`] — the WHATWG "header list" data structure that `Headers`, `Request`, and
//!   `Response` are all defined in terms of: case-insensitive names, append-combines values with
//!   `", "`, byte-validated names (token) and values (no NUL/CR/LF), `Set-Cookie` kept un-combined so
//!   `getSetCookie()` can return each cookie separately, and a deterministic sorted view (the WHATWG
//!   "sort and combine" used when iterating a `Headers` object).
//! * [`normalize_method`] / [`is_forbidden_method`] — WHATWG "normalize a method" (upper-case the four
//!   well-known lower-case spellings) and the forbidden-method guard.
//! * [`is_redirect_status`] / [`is_null_body_status`] / [`is_ok_status`] — the `Response` status
//!   classifications.
//!
//! The JS-facing [`install`] seam exposes these as native functions over a plain header **array of
//! `[name, value]` pairs** (the shape `Headers` is constructed from and iterated as). The reason the
//! stateful `Headers` / `Request` / `Response` **classes** and a live `fetch()` are *not* built here:
//!
//! * **No network.** `treaty_runtime` depends only on `nova_vm`, `serde_json`, the `oxc_*` transpile
//!   crates, and `oxc_resolver` (see `libs/runtime/Cargo.toml`). There is no HTTP client crate, and
//!   this task may edit only this file — it may not add a dependency. A faithful networking `fetch()`
//!   therefore cannot be implemented here; wiring it (and the `Promise`-returning global) is deferred
//!   to a follow-up that introduces an HTTP transport behind the event loop. This mirrors how Node
//!   itself layers `fetch` (undici) over a transport rather than the language core.
//! * **Stateful classes need internal slots.** A WHATWG `Headers` object carries a mutable header
//!   list and a guard in internal slots, and `Request`/`Response` carry a body stream. The pinned Nova
//!   rev (`bece61ac`) exposes no embedder API to attach native internal state to a JS object, so —
//!   exactly as `text_encoding` does for its `TextEncoder`/`TextDecoder` classes — the **class shells**
//!   are deferred to `globals.rs`, which can JS-bootstrap them over the native header primitives this
//!   module exports. The header *semantics* (validation, combine, sort, `Set-Cookie`) are identical;
//!   only the OO wrapper differs.
//!
//! Everything that is implemented here is faithful WHATWG and fully covered by `#[cfg(test)]`.

use std::borrow::Cow;

use nova_vm::ecmascript::{
    Agent, Array, ArgumentsList, ExceptionType, InternalMethods, JsResult, Object, OrdinaryObject,
    PropertyKey, String as JsString, TryGetResult, Value, unwrap_try,
};
use nova_vm::engine::NoGcScope;

use crate::node::core::{InstallError, NodeCtx};
use crate::node::globals::define_fn;
use crate::node::{GcScope, NodeModule};

// =================================================================================================
// Pure WHATWG header / method / status core (no Nova, no network; unit-tested directly).
// =================================================================================================

/// A single header name/value pair as stored in a [`HeaderList`].
///
/// `name` is held in its original spelling for round-tripping, but all comparisons go through the
/// ASCII-case-insensitive helpers below, matching the WHATWG rule that header names are
/// byte-case-insensitive. Values are stored verbatim (already validated on insertion).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Header {
    name: String,
    value: String,
}

impl Header {
    /// The header's name, in the spelling it was first inserted with.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
    /// The header's value.
    pub(crate) fn value(&self) -> &str {
        &self.value
    }
}

/// Why a header name or value was rejected by [`HeaderList`].
///
/// WHATWG throws a `TypeError` for each of these; the carried context lets the JS wrapper build a
/// faithful message without allocating until the (rare) error path is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeaderError {
    /// The name is empty or contains a byte outside the HTTP token set.
    InvalidName,
    /// The value contains a forbidden byte (NUL, CR, or LF), or leading/trailing HTTP whitespace
    /// that WHATWG forbids after trimming would be required.
    InvalidValue,
}

/// The WHATWG "header list": an ordered multimap of header name/value pairs.
///
/// This is the shared substrate of `Headers`, `Request`, and `Response`. It is deliberately a thin
/// `Vec` (header lists are tiny — a handful of entries — so linear scans beat a hash map and avoid
/// per-entry heap nodes; tenet 3). Names compare ASCII-case-insensitively; `append` combines into an
/// existing entry with `", "` per spec, *except* `Set-Cookie`, which is always kept as a separate
/// entry so [`HeaderList::get_set_cookie`] can return each cookie individually.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HeaderList {
    entries: Vec<Header>,
}

/// The canonical lower-case name of the one header WHATWG never combines on `append` and exposes
/// individually via `getSetCookie()`.
const SET_COOKIE: &str = "set-cookie";

impl HeaderList {
    /// An empty header list. No allocation until the first header is added (tenet 3).
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Number of stored entries (post-combine; `Set-Cookie`s each count once).
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the list holds no entries.
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Borrow the entries in insertion order (the order `append`/`set` produced them).
    pub(crate) fn entries(&self) -> &[Header] {
        &self.entries
    }

    /// WHATWG `Headers.append(name, value)`.
    ///
    /// Validates the name (token) and value (no NUL/CR/LF), trimming the WHATWG-defined leading and
    /// trailing HTTP whitespace from the value first. If a non-`Set-Cookie` header of the same name
    /// already exists, the new value is combined onto the first match with `", "`; otherwise a new
    /// entry is pushed. `Set-Cookie` is always pushed as its own entry.
    pub(crate) fn append(&mut self, name: &str, value: &str) -> Result<(), HeaderError> {
        let value = normalize_value(value)?;
        validate_name(name)?;

        if !name.eq_ignore_ascii_case(SET_COOKIE) {
            if let Some(existing) = self
                .entries
                .iter_mut()
                .find(|h| h.name.eq_ignore_ascii_case(name))
            {
                // Combine: "<old>, <new>".
                existing.value.reserve(value.len() + 2);
                existing.value.push_str(", ");
                existing.value.push_str(&value);
                return Ok(());
            }
        }
        self.entries.push(Header {
            name: name.to_owned(),
            value: value.into_owned(),
        });
        Ok(())
    }

    /// WHATWG `Headers.set(name, value)`: replace all entries of `name` with a single combined entry.
    ///
    /// Removes every existing entry of the name (case-insensitively) and inserts one entry with the
    /// new value. Unlike `append`, `set` does not combine — it overwrites — and this is true for
    /// `Set-Cookie` as well (a `set` collapses prior cookies into one entry, matching WHATWG).
    pub(crate) fn set(&mut self, name: &str, value: &str) -> Result<(), HeaderError> {
        let value = normalize_value(value)?;
        validate_name(name)?;
        self.entries.retain(|h| !h.name.eq_ignore_ascii_case(name));
        self.entries.push(Header {
            name: name.to_owned(),
            value: value.into_owned(),
        });
        Ok(())
    }

    /// WHATWG `Headers.get(name)`: the combined value, or `None` if absent.
    ///
    /// For a name with multiple entries (only possible for `Set-Cookie`, which `append` does not
    /// combine), the values are joined with `", "` on read, matching `Headers.prototype.get`'s
    /// behavior of returning a single combined string. Returns a borrowed `&str` when there is exactly
    /// one matching entry (zero-copy; tenet 3) and only allocates the joined string when there is more
    /// than one match.
    pub(crate) fn get(&self, name: &str) -> Option<Cow<'_, str>> {
        let mut matches = self
            .entries
            .iter()
            .filter(|h| h.name.eq_ignore_ascii_case(name));
        let first = matches.next()?;
        match matches.next() {
            None => Some(Cow::Borrowed(first.value.as_str())),
            Some(second) => {
                let mut combined = String::with_capacity(first.value.len() + second.value.len() + 2);
                combined.push_str(&first.value);
                for h in std::iter::once(second).chain(matches) {
                    combined.push_str(", ");
                    combined.push_str(&h.value);
                }
                Some(Cow::Owned(combined))
            }
        }
    }

    /// WHATWG `Headers.has(name)`: whether any entry matches the name (case-insensitively).
    pub(crate) fn has(&self, name: &str) -> bool {
        self.entries
            .iter()
            .any(|h| h.name.eq_ignore_ascii_case(name))
    }

    /// WHATWG `Headers.delete(name)`: remove every entry of the name. Returns whether anything was
    /// removed (so the caller can distinguish a no-op).
    pub(crate) fn delete(&mut self, name: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|h| !h.name.eq_ignore_ascii_case(name));
        self.entries.len() != before
    }

    /// WHATWG `Headers.getSetCookie()`: every `Set-Cookie` value, in insertion order, each kept
    /// separate (never combined). Borrows the stored values (tenet 3).
    pub(crate) fn get_set_cookie(&self) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|h| h.name.eq_ignore_ascii_case(SET_COOKIE))
            .map(|h| h.value.as_str())
            .collect()
    }

    /// WHATWG "sort and combine": the header list as `(lower-case name, value)` pairs, sorted by name
    /// byte-wise, with same-name values combined by `", "` — *except* `Set-Cookie`, which yields one
    /// pair per cookie (kept separate, in insertion order). This is exactly what iterating a `Headers`
    /// object (`entries()`/`keys()`/`values()`/`for..of`) produces.
    pub(crate) fn sorted_combined(&self) -> Vec<(String, String)> {
        // Collect lower-cased names so the sort is byte-wise over the canonical (lower) spelling.
        let mut out: Vec<(String, String)> = Vec::new();
        for h in &self.entries {
            let lower = h.name.to_ascii_lowercase();
            if lower == SET_COOKIE {
                // Each Set-Cookie is its own pair; never combined.
                out.push((lower, h.value.clone()));
            } else if let Some(slot) = out.iter_mut().find(|(n, _)| *n == lower) {
                slot.1.push_str(", ");
                slot.1.push_str(&h.value);
            } else {
                out.push((lower, h.value.clone()));
            }
        }
        out.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        out
    }
}

/// Validate a header *name* as an HTTP token (RFC 7230 / WHATWG "header name").
///
/// A name must be non-empty and consist only of token characters: ASCII letters, digits, and the
/// punctuation `!#$%&'*+-.^_`|~`. Any other byte (space, separators, control chars, non-ASCII) is
/// rejected. Pure byte scan — no allocation.
fn validate_name(name: &str) -> Result<(), HeaderError> {
    if name.is_empty() || !name.bytes().all(is_token_byte) {
        Err(HeaderError::InvalidName)
    } else {
        Ok(())
    }
}

/// Trim WHATWG "HTTP whitespace" (` \t\n\r`) from both ends of a header value, then validate that the
/// remainder contains no NUL, CR, or LF byte (those can never appear inside a header value).
///
/// Returns a [`Cow`] that borrows the input when no trimming was needed (zero-copy; tenet 3) and only
/// allocates when a trim actually shortened the slice.
fn normalize_value(value: &str) -> Result<Cow<'_, str>, HeaderError> {
    // WHATWG strips leading/trailing HTTP whitespace from a header value before storing it.
    let trimmed = value.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\n' | '\r'));
    if trimmed.bytes().any(|b| matches!(b, 0x00 | b'\r' | b'\n')) {
        return Err(HeaderError::InvalidValue);
    }
    if trimmed.len() == value.len() {
        Ok(Cow::Borrowed(value))
    } else {
        Ok(Cow::Owned(trimmed.to_owned()))
    }
}

/// Whether `b` is an RFC 7230 `tchar` (valid in an HTTP header name / method token).
#[inline]
fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

/// WHATWG "normalize a method": upper-case the four well-known methods when they are given in any
/// case, leaving every other (custom) method untouched — and reject any method that is not a valid
/// HTTP token.
///
/// The normalized set per spec is exactly `DELETE GET HEAD OPTIONS POST PUT`. A method like `patch`
/// is *not* upper-cased (it stays `patch`), matching the WHATWG algorithm, which only normalizes the
/// listed six.
pub(crate) fn normalize_method(method: &str) -> Result<Cow<'_, str>, HeaderError> {
    if method.is_empty() || !method.bytes().all(is_token_byte) {
        return Err(HeaderError::InvalidName);
    }
    const NORMALIZED: [&str; 6] = ["DELETE", "GET", "HEAD", "OPTIONS", "POST", "PUT"];
    for m in NORMALIZED {
        if method.eq_ignore_ascii_case(m) {
            // Already in canonical spelling: borrow it; otherwise hand back the canonical constant.
            return Ok(if method == m {
                Cow::Borrowed(method)
            } else {
                Cow::Borrowed(m)
            });
        }
    }
    Ok(Cow::Borrowed(method))
}

/// WHATWG "forbidden method": `CONNECT`, `TRACE`, `TRACK` (case-insensitively). A `Request` may not be
/// constructed with one of these.
pub(crate) fn is_forbidden_method(method: &str) -> bool {
    ["CONNECT", "TRACE", "TRACK"]
        .iter()
        .any(|m| method.eq_ignore_ascii_case(m))
}

/// A redirect status per WHATWG: 301, 302, 303, 307, 308.
pub(crate) fn is_redirect_status(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

/// A "null body status" per WHATWG (`Response` must have a null body): 101, 103, 204, 205, 304.
pub(crate) fn is_null_body_status(status: u16) -> bool {
    matches!(status, 101 | 103 | 204 | 205 | 304)
}

/// An "ok status" per WHATWG (`Response.ok`): the inclusive range 200..=299.
pub(crate) fn is_ok_status(status: u16) -> bool {
    (200..=299).contains(&status)
}

// =================================================================================================
// JS-facing wiring.
// =================================================================================================

/// Zero-sized marker for the `node:fetch` builtin.
pub(crate) struct FetchModule;

impl NodeModule for FetchModule {
    const SPECIFIER: &'static str = "fetch";

    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: GcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError> {
        install(agent, ctx, gc)
    }
}

/// Uniform per-module entry. Returns the `node:fetch` exports object.
///
/// The exports expose the pure WHATWG header / method / status primitives as native functions over a
/// plain JS `Array` of `[name, value]` pairs (the shape a `Headers` object is initialized from and
/// iterated as — see the module-level divergence note for why the stateful classes live in
/// `globals.rs`):
///
/// * `headersAppend(list, name, value) -> list` — validate + combine, returning the new pair array.
/// * `headersSet(list, name, value) -> list` — validate + overwrite.
/// * `headersGet(list, name) -> string | null` — the combined value.
/// * `headersHas(list, name) -> boolean`.
/// * `headersDelete(list, name) -> list`.
/// * `headersGetSetCookie(list) -> string[]`.
/// * `headersSortedCombined(list) -> [name, value][]` — the canonical iteration order.
/// * `normalizeMethod(method) -> string` and `isForbiddenMethod(method) -> boolean`.
/// * `isRedirectStatus` / `isNullBodyStatus` / `isOkStatus` `(number) -> boolean`.
///
/// Materialized once, lazily, on first import (tenet 2).
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    let gc = gc.into_nogc();
    let obj = OrdinaryObject::create_empty_object(agent, gc);

    define_fn(agent, obj, "headersAppend", js::headers_append, 3, gc);
    define_fn(agent, obj, "headersSet", js::headers_set, 3, gc);
    define_fn(agent, obj, "headersGet", js::headers_get, 2, gc);
    define_fn(agent, obj, "headersHas", js::headers_has, 2, gc);
    define_fn(agent, obj, "headersDelete", js::headers_delete, 2, gc);
    define_fn(agent, obj, "headersGetSetCookie", js::headers_get_set_cookie, 1, gc);
    define_fn(agent, obj, "headersSortedCombined", js::headers_sorted_combined, 1, gc);
    define_fn(agent, obj, "normalizeMethod", js::normalize_method_fn, 1, gc);
    define_fn(agent, obj, "isForbiddenMethod", js::is_forbidden_method_fn, 1, gc);
    define_fn(agent, obj, "isRedirectStatus", js::is_redirect_status_fn, 1, gc);
    define_fn(agent, obj, "isNullBodyStatus", js::is_null_body_status_fn, 1, gc);
    define_fn(agent, obj, "isOkStatus", js::is_ok_status_fn, 1, gc);

    Ok(obj.into())
}

/// The JS wrapper functions: thin [`nova_vm::ecmascript::RegularFn`]s that marshal a `[name, value]`
/// pair array to/from the pure [`HeaderList`] core above.
mod js {
    use super::*;

    /// Throw a WHATWG-style `TypeError` for an invalid header name/value or method.
    fn throw_header_error<'gc>(
        agent: &mut Agent,
        err: HeaderError,
        gc: NoGcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let message = match err {
            HeaderError::InvalidName => "Invalid header/method name",
            HeaderError::InvalidValue => "Invalid header value",
        };
        Err(agent.throw_exception_with_static_message(ExceptionType::TypeError, message, gc))
    }

    /// Read a JS string argument, treating any non-string value as the empty string (WHATWG coerces
    /// missing values via `ToString`/`ByteString`; the empty string is the benign default used by the
    /// pair-array protocol).
    fn read_str(agent: &mut Agent, value: Value) -> String {
        match JsString::try_from(value) {
            Ok(s) => s.to_string_lossy(agent).into_owned(),
            Err(_) => String::new(),
        }
    }

    /// Narrow a [`TryGetResult`] from a (non-side-effecting) indexed `[[Get]]` to a [`Value`].
    ///
    /// Indexed reads of an `Array`'s own elements yield plain data properties, so the result is
    /// [`TryGetResult::Value`]; the absent / getter / proxy variants are treated defensively as
    /// `undefined` (which `read_str` maps to the empty string), keeping the marshalling total and
    /// panic-free.
    fn get_value<'gc>(result: TryGetResult<'gc>) -> Value<'gc> {
        match result {
            TryGetResult::Value(v) => v,
            _ => Value::Undefined,
        }
    }

    /// Parse a `[name, value]`-pair JS `Array` into a [`HeaderList`] WITHOUT re-running validation or
    /// combine (the array is the already-built model; we trust it). A missing/empty argument yields an
    /// empty list. Each element is read as a 2-element `[name, value]` sub-array.
    fn read_pair_array(agent: &mut Agent, value: Value, gc: NoGcScope) -> HeaderList {
        let mut list = HeaderList::new();
        let Ok(array) = Array::try_from(value) else {
            return list;
        };
        let len = array.len(agent);
        list.entries.reserve(len as usize);
        for i in 0..len {
            let key = PropertyKey::Integer(i.into());
            let pair = get_value(unwrap_try(array.try_get(agent, key, array.into(), None, gc)));
            let Ok(pair) = Array::try_from(pair) else {
                continue;
            };
            let name_key = PropertyKey::Integer(0.into());
            let value_key = PropertyKey::Integer(1.into());
            let name_v = get_value(unwrap_try(pair.try_get(agent, name_key, pair.into(), None, gc)));
            let value_v =
                get_value(unwrap_try(pair.try_get(agent, value_key, pair.into(), None, gc)));
            let name = read_str(agent, name_v);
            let val = read_str(agent, value_v);
            // Trust the stored model: push verbatim, preserving order and any prior combine.
            list.entries.push(Header { name, value: val });
        }
        list
    }

    /// Build a JS `[name, value][]` pair `Array` from a [`HeaderList`]'s entries (insertion order).
    fn list_to_pair_array<'gc>(
        agent: &mut Agent,
        list: &HeaderList,
        gc: NoGcScope<'gc, '_>,
    ) -> Array<'gc> {
        let pairs: Vec<Value> = list
            .entries()
            .iter()
            .map(|h| {
                let name = JsString::from_str(agent, h.name(), gc).into();
                let value = JsString::from_str(agent, h.value(), gc).into();
                Array::from_slice(agent, &[name, value], gc).into()
            })
            .collect();
        Array::from_slice(agent, &pairs, gc)
    }

    /// Build a JS `[name, value][]` pair `Array` from `(name, value)` tuples (used for sorted view).
    fn tuples_to_pair_array<'gc>(
        agent: &mut Agent,
        tuples: &[(String, String)],
        gc: NoGcScope<'gc, '_>,
    ) -> Array<'gc> {
        let pairs: Vec<Value> = tuples
            .iter()
            .map(|(n, v)| {
                let name = JsString::from_str(agent, n, gc).into();
                let value = JsString::from_str(agent, v, gc).into();
                Array::from_slice(agent, &[name, value], gc).into()
            })
            .collect();
        Array::from_slice(agent, &pairs, gc)
    }

    /// `headersAppend(list, name, value)` -> the new pair array.
    pub(super) fn headers_append<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let gc = gc.into_nogc();
        let mut list = read_pair_array(agent, args.get(0), gc);
        let name = read_str(agent, args.get(1));
        let value = read_str(agent, args.get(2));
        if let Err(e) = list.append(&name, &value) {
            return throw_header_error(agent, e, gc);
        }
        Ok(list_to_pair_array(agent, &list, gc).into())
    }

    /// `headersSet(list, name, value)` -> the new pair array.
    pub(super) fn headers_set<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let gc = gc.into_nogc();
        let mut list = read_pair_array(agent, args.get(0), gc);
        let name = read_str(agent, args.get(1));
        let value = read_str(agent, args.get(2));
        if let Err(e) = list.set(&name, &value) {
            return throw_header_error(agent, e, gc);
        }
        Ok(list_to_pair_array(agent, &list, gc).into())
    }

    /// `headersGet(list, name)` -> combined value string, or `null`.
    pub(super) fn headers_get<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let gc = gc.into_nogc();
        let list = read_pair_array(agent, args.get(0), gc);
        let name = read_str(agent, args.get(1));
        match list.get(&name) {
            Some(v) => Ok(JsString::from_str(agent, &v, gc).into()),
            None => Ok(Value::Null),
        }
    }

    /// `headersHas(list, name)` -> boolean.
    pub(super) fn headers_has<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let gc = gc.into_nogc();
        let list = read_pair_array(agent, args.get(0), gc);
        let name = read_str(agent, args.get(1));
        Ok(Value::Boolean(list.has(&name)))
    }

    /// `headersDelete(list, name)` -> the new pair array.
    pub(super) fn headers_delete<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let gc = gc.into_nogc();
        let mut list = read_pair_array(agent, args.get(0), gc);
        let name = read_str(agent, args.get(1));
        list.delete(&name);
        Ok(list_to_pair_array(agent, &list, gc).into())
    }

    /// `headersGetSetCookie(list)` -> `string[]` of each Set-Cookie value.
    pub(super) fn headers_get_set_cookie<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let gc = gc.into_nogc();
        let list = read_pair_array(agent, args.get(0), gc);
        let cookies = list.get_set_cookie();
        let values: Vec<Value> = cookies
            .iter()
            .map(|c| JsString::from_str(agent, c, gc).into())
            .collect();
        Ok(Array::from_slice(agent, &values, gc).into())
    }

    /// `headersSortedCombined(list)` -> `[name, value][]` in canonical iteration order.
    pub(super) fn headers_sorted_combined<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let gc = gc.into_nogc();
        let list = read_pair_array(agent, args.get(0), gc);
        let sorted = list.sorted_combined();
        Ok(tuples_to_pair_array(agent, &sorted, gc).into())
    }

    /// `normalizeMethod(method)` -> normalized method string (throws on a non-token method).
    pub(super) fn normalize_method_fn<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let gc = gc.into_nogc();
        let method = read_str(agent, args.get(0));
        match normalize_method(&method) {
            Ok(m) => Ok(JsString::from_str(agent, &m, gc).into()),
            Err(e) => throw_header_error(agent, e, gc),
        }
    }

    /// `isForbiddenMethod(method)` -> boolean.
    pub(super) fn is_forbidden_method_fn<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        _gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let method = read_str(agent, args.get(0));
        Ok(Value::Boolean(is_forbidden_method(&method)))
    }

    /// Read a status argument as a `u16`, clamping out-of-range / non-integer values to `0` (which no
    /// classifier treats as special), so a bad input simply reports `false`.
    fn read_status(value: Value) -> u16 {
        match value {
            Value::Integer(i) => u16::try_from(i.into_i64()).unwrap_or(0),
            _ => 0,
        }
    }

    /// `isRedirectStatus(status)` -> boolean.
    pub(super) fn is_redirect_status_fn<'gc>(
        _agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        _gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        Ok(Value::Boolean(is_redirect_status(read_status(args.get(0)))))
    }

    /// `isNullBodyStatus(status)` -> boolean.
    pub(super) fn is_null_body_status_fn<'gc>(
        _agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        _gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        Ok(Value::Boolean(is_null_body_status(read_status(args.get(0)))))
    }

    /// `isOkStatus(status)` -> boolean.
    pub(super) fn is_ok_status_fn<'gc>(
        _agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        _gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        Ok(Value::Boolean(is_ok_status(read_status(args.get(0)))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_combines_same_name_with_comma_space() {
        let mut h = HeaderList::new();
        h.append("Accept", "text/html").unwrap();
        h.append("accept", "application/json").unwrap();
        // Case-insensitive name match; values combine with ", "; one entry remains.
        assert_eq!(h.len(), 1);
        assert_eq!(h.get("ACCEPT").as_deref(), Some("text/html, application/json"));
    }

    #[test]
    fn set_overwrites_all_existing_entries() {
        let mut h = HeaderList::new();
        h.append("X-Test", "a").unwrap();
        h.append("x-test", "b").unwrap();
        h.set("X-TEST", "final").unwrap();
        assert_eq!(h.len(), 1);
        assert_eq!(h.get("x-test").as_deref(), Some("final"));
    }

    #[test]
    fn delete_removes_case_insensitively_and_reports() {
        let mut h = HeaderList::new();
        h.append("Content-Type", "text/plain").unwrap();
        assert!(h.has("content-type"));
        assert!(h.delete("CONTENT-TYPE"));
        assert!(!h.has("content-type"));
        // A second delete is a no-op and reports false.
        assert!(!h.delete("content-type"));
    }

    #[test]
    fn get_returns_none_for_absent_header() {
        let h = HeaderList::new();
        assert!(h.get("nope").is_none());
    }

    #[test]
    fn single_value_get_borrows_without_allocation() {
        let mut h = HeaderList::new();
        h.append("X-One", "value").unwrap();
        // Exactly one match -> the stored value is borrowed, not a freshly joined String.
        assert!(matches!(h.get("x-one"), Some(Cow::Borrowed(_))));
    }

    #[test]
    fn set_cookie_is_never_combined_and_get_set_cookie_keeps_separate() {
        let mut h = HeaderList::new();
        h.append("Set-Cookie", "a=1").unwrap();
        h.append("set-cookie", "b=2").unwrap();
        // Two distinct entries (append does not combine Set-Cookie).
        assert_eq!(h.len(), 2);
        assert_eq!(h.get_set_cookie(), vec!["a=1", "b=2"]);
        // But .get() still returns the combined view per WHATWG.
        assert_eq!(h.get("set-cookie").as_deref(), Some("a=1, b=2"));
    }

    #[test]
    fn value_whitespace_is_trimmed_on_insertion() {
        let mut h = HeaderList::new();
        h.append("X-Pad", "  spaced \t").unwrap();
        assert_eq!(h.get("x-pad").as_deref(), Some("spaced"));
    }

    #[test]
    fn invalid_header_name_is_rejected() {
        let mut h = HeaderList::new();
        assert_eq!(h.append("", "v"), Err(HeaderError::InvalidName));
        assert_eq!(h.append("bad name", "v"), Err(HeaderError::InvalidName));
        assert_eq!(h.append("bad\u{0000}", "v"), Err(HeaderError::InvalidName));
        // A valid token name with punctuation is accepted.
        assert!(h.append("X-Custom_Header.1", "v").is_ok());
    }

    #[test]
    fn invalid_header_value_with_control_bytes_is_rejected() {
        let mut h = HeaderList::new();
        assert_eq!(h.append("X", "line1\nline2"), Err(HeaderError::InvalidValue));
        assert_eq!(h.append("X", "carriage\rreturn"), Err(HeaderError::InvalidValue));
        assert_eq!(h.append("X", "nul\u{0000}byte"), Err(HeaderError::InvalidValue));
    }

    #[test]
    fn sorted_combined_sorts_by_name_and_combines() {
        let mut h = HeaderList::new();
        h.append("b-header", "2").unwrap();
        h.append("A-Header", "1").unwrap();
        h.append("a-header", "1b").unwrap();
        let sorted = h.sorted_combined();
        assert_eq!(
            sorted,
            vec![
                ("a-header".to_owned(), "1, 1b".to_owned()),
                ("b-header".to_owned(), "2".to_owned()),
            ]
        );
    }

    #[test]
    fn sorted_combined_keeps_each_set_cookie_separate() {
        let mut h = HeaderList::new();
        h.append("Set-Cookie", "a=1").unwrap();
        h.append("Accept", "x").unwrap();
        h.append("Set-Cookie", "b=2").unwrap();
        let sorted = h.sorted_combined();
        // Both set-cookie entries survive as separate pairs, sorted after "accept".
        assert_eq!(
            sorted,
            vec![
                ("accept".to_owned(), "x".to_owned()),
                ("set-cookie".to_owned(), "a=1".to_owned()),
                ("set-cookie".to_owned(), "b=2".to_owned()),
            ]
        );
    }

    #[test]
    fn empty_list_has_no_allocation_until_first_insert() {
        let h = HeaderList::new();
        assert!(h.is_empty());
        assert_eq!(h.len(), 0);
        assert_eq!(h.entries().len(), 0);
    }

    #[test]
    fn normalize_method_upper_cases_known_methods_case_insensitively() {
        assert_eq!(normalize_method("get").unwrap(), "GET");
        assert_eq!(normalize_method("Post").unwrap(), "POST");
        assert_eq!(normalize_method("DELETE").unwrap(), "DELETE");
        assert_eq!(normalize_method("head").unwrap(), "HEAD");
        assert_eq!(normalize_method("options").unwrap(), "OPTIONS");
        assert_eq!(normalize_method("put").unwrap(), "PUT");
    }

    #[test]
    fn normalize_method_leaves_custom_methods_untouched() {
        // WHATWG normalizes only the six listed methods; `patch`/`QUERY` stay as written.
        assert_eq!(normalize_method("patch").unwrap(), "patch");
        assert_eq!(normalize_method("QUERY").unwrap(), "QUERY");
        assert_eq!(normalize_method("CustomVerb").unwrap(), "CustomVerb");
    }

    #[test]
    fn normalize_method_rejects_non_token_methods() {
        assert_eq!(normalize_method(""), Err(HeaderError::InvalidName));
        assert_eq!(normalize_method("bad method"), Err(HeaderError::InvalidName));
        assert_eq!(normalize_method("with\nnewline"), Err(HeaderError::InvalidName));
    }

    #[test]
    fn forbidden_methods_detected_case_insensitively() {
        for m in ["CONNECT", "connect", "Trace", "TRACK", "track"] {
            assert!(is_forbidden_method(m), "{m} should be forbidden");
        }
        for m in ["GET", "POST", "PATCH", "custom"] {
            assert!(!is_forbidden_method(m), "{m} should be allowed");
        }
    }

    #[test]
    fn status_classifications_match_whatwg() {
        for s in [301, 302, 303, 307, 308] {
            assert!(is_redirect_status(s), "{s} is a redirect status");
        }
        assert!(!is_redirect_status(200));
        assert!(!is_redirect_status(304));

        for s in [101, 103, 204, 205, 304] {
            assert!(is_null_body_status(s), "{s} is a null-body status");
        }
        assert!(!is_null_body_status(200));

        assert!(is_ok_status(200));
        assert!(is_ok_status(299));
        assert!(!is_ok_status(199));
        assert!(!is_ok_status(300));
    }
}
