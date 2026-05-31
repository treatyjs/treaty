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
//! `[name, value]` pairs** (the shape `Headers` is constructed from and iterated as). It also exposes
//! the network-independent half of `fetch()` — the WHATWG **`data:` URL processor** — as the native
//! [`js::parse_data_url`] (`parseDataUrl`), so a `fetch("data:...")` resolves to a real `Response`
//! without any transport:
//!
//! * [`DataUrl`] — the WHATWG "data: URL processor": splits `data:[<mediatype>][;base64],<data>` into
//!   a MIME type (defaulting to `text/plain;charset=US-ASCII`) and a decoded body, percent-decoding a
//!   plain payload or [`decode_base64`]-decoding a `;base64` payload. Pure bytes in, pure bytes out —
//!   no Nova, no network — and unit-tested directly.
//! * [`decode_base64`] / [`percent_decode_bytes`] — the two body decoders the processor needs (RFC
//!   4648 base64 with optional padding and ASCII-whitespace tolerance, and WHATWG percent-decoding to
//!   raw bytes), both pure and unit-tested.
//!
//! The reason the stateful `Headers` / `Request` / `Response` **classes** and the *networking* half of
//! `fetch()` are *not* built here:
//!
//! * **No network.** `treaty_runtime` depends only on `nova_vm`, `serde_json`, the `oxc_*` transpile
//!   crates, and `oxc_resolver` (see `libs/runtime/Cargo.toml`). There is no HTTP client crate, and
//!   this task may edit only this file — it may not add a dependency. A *networking* `fetch()` (an
//!   `http(s):` request) therefore cannot be implemented here; wiring it (and the `Promise`-returning
//!   global) is deferred to a follow-up that introduces an HTTP transport behind the event loop. This
//!   mirrors how Node itself layers `fetch` (undici) over a transport rather than the language core.
//!   The transport-free `data:` scheme, by contrast, is implemented in full and is the WinterCG
//!   minimum `fetch` every offline runtime is expected to honor.
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
// WHATWG `data:` URL processor (no Nova, no network; unit-tested directly).
//
// `fetch("data:...")` needs no transport: per the WHATWG "data: URL processor" the response body and
// MIME type are derived purely from the URL itself. This is the offline `fetch` every WinterCG
// runtime is expected to support, so it is implemented in full here and surfaced through the native
// `parseDataUrl` so the `fetch()` global resolves a real `Response` for it.
// =================================================================================================

/// The result of running the WHATWG "data: URL processor" over a `data:` URL.
///
/// Carries the decoded body bytes and the MIME type essence/parameters string the `Response` should
/// report as its `Content-Type`. WHATWG mandates the default `text/plain;charset=US-ASCII` when the
/// URL omits a media type, so [`DataUrl::mime_type`] is never empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DataUrl {
    mime_type: String,
    body: Vec<u8>,
}

impl DataUrl {
    /// The MIME type (e.g. `text/plain;charset=US-ASCII`, `application/json`) the `Response` reports.
    pub(crate) fn mime_type(&self) -> &str {
        &self.mime_type
    }

    /// The decoded response body bytes.
    pub(crate) fn body(&self) -> &[u8] {
        &self.body
    }
}

/// Why a `data:` URL failed the WHATWG "data: URL processor".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DataUrlError {
    /// The URL does not begin with the `data:` scheme (case-insensitively).
    NotDataScheme,
    /// There is no `,` separating the header from the data (the URL is just `data:` + header).
    MissingComma,
    /// The `;base64` payload is not decodable as RFC 4648 base64.
    InvalidBase64,
}

/// The WHATWG default MIME type for a `data:` URL whose header omits a media type.
const DATA_URL_DEFAULT_MIME: &str = "text/plain;charset=US-ASCII";

/// Run the WHATWG "data: URL processor" over `url`.
///
/// Splits a `data:[<mediatype>][;base64],<data>` URL into its MIME type and decoded body:
///
/// * The scheme must be `data:` (matched case-insensitively, since URL schemes are ASCII-case-
///   insensitive); anything else is [`DataUrlError::NotDataScheme`].
/// * Everything up to the **first** `,` is the header; the remainder is the data. A header ending in
///   `;base64` (case-insensitively, ASCII-whitespace tolerated around it) selects base64 decoding of
///   the data via [`decode_base64`]; otherwise the data is percent-decoded to raw bytes via
///   [`percent_decode_bytes`].
/// * The MIME type is the header with any trailing `;base64` removed; an empty media type (header was
///   empty, or only `;base64`) yields the WHATWG default `text/plain;charset=US-ASCII`. A header that
///   begins with `;` (a bare parameter list, e.g. `;charset=utf-8`) is prefixed with `text/plain`,
///   matching the processor's "if mimeType starts with ';' prepend 'text/plain'" step.
///
/// Pure: borrows `url`, allocates only the returned body/MIME strings.
pub(crate) fn parse_data_url(url: &str) -> Result<DataUrl, DataUrlError> {
    // Scheme is ASCII-case-insensitive. Strip it without lowercasing the rest of the URL.
    let rest = url
        .get(..5)
        .filter(|p| p.eq_ignore_ascii_case("data:"))
        .map(|_| &url[5..])
        .ok_or(DataUrlError::NotDataScheme)?;

    let comma = rest.find(',').ok_or(DataUrlError::MissingComma)?;
    let header = &rest[..comma];
    let data = &rest[comma + 1..];

    // A trailing `;base64` (ASCII-whitespace tolerated) selects base64; strip it off the MIME header.
    let (mime_part, is_base64) = match strip_base64_suffix(header) {
        Some(prefix) => (prefix, true),
        None => (header, false),
    };

    let body = if is_base64 {
        decode_base64(mime_part_data(data)).ok_or(DataUrlError::InvalidBase64)?
    } else {
        percent_decode_bytes(data)
    };

    let mime_type = normalize_data_url_mime(mime_part);
    Ok(DataUrl { mime_type, body })
}

/// Identity helper kept readable at the call site: the base64 payload is exactly the post-comma data.
/// (Factored so the `is_base64` branch reads symmetrically with the percent-decode branch.)
#[inline]
fn mime_part_data(data: &str) -> &str {
    data
}

/// If `header` ends in `;base64` (case-insensitively, with optional ASCII whitespace before the `;`
/// and after `base64`), return the header with that suffix removed; otherwise `None`.
///
/// WHATWG matches `;base64` only as the final parameter of the media-type header. Surrounding ASCII
/// whitespace is tolerated because a header like `text/plain ;base64` is produced by lenient authors
/// and Node/browsers accept it.
fn strip_base64_suffix(header: &str) -> Option<&str> {
    let trimmed = header.trim_end_matches(|c: char| c.is_ascii_whitespace());
    // The suffix is `;base64`, case-insensitive on the `base64` token.
    let cut = trimmed.len().checked_sub(7)?;
    let (prefix, suffix) = trimmed.split_at(cut);
    if suffix.eq_ignore_ascii_case(";base64") {
        Some(prefix.trim_end_matches(|c: char| c.is_ascii_whitespace()))
    } else {
        None
    }
}

/// Build the MIME-type string the `Response` reports from the (base64-stripped) media-type header.
///
/// Empty -> the WHATWG default `text/plain;charset=US-ASCII`. A header beginning with `;` (a bare
/// parameter list) is prefixed with `text/plain`. Otherwise the header is used verbatim (trimmed of
/// surrounding ASCII whitespace).
fn normalize_data_url_mime(mime_part: &str) -> String {
    let trimmed = mime_part.trim_matches(|c: char| c.is_ascii_whitespace());
    if trimmed.is_empty() {
        DATA_URL_DEFAULT_MIME.to_owned()
    } else if trimmed.starts_with(';') {
        let mut s = String::with_capacity("text/plain".len() + trimmed.len());
        s.push_str("text/plain");
        s.push_str(trimmed);
        s
    } else {
        trimmed.to_owned()
    }
}

/// Decode an RFC 4648 base64 string to bytes, tolerating ASCII whitespace and optional `=` padding.
///
/// Accepts the standard alphabet (`A-Z a-z 0-9 + /`). ASCII whitespace (space, `\t`, `\n`, `\r`,
/// `\x0c`) is skipped anywhere (data: URLs and MIME bodies commonly fold base64 across lines). `=`
/// padding is honored if present but not required; a trailing partial group of length 2 or 3 decodes
/// to 1 or 2 bytes respectively. A length-1 trailing group, or any non-alphabet / non-whitespace byte,
/// is an error (`None`). Pure: a single output `Vec` sized to the worst-case byte count.
pub(crate) fn decode_base64(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() / 4 * 3 + 3);
    // The current group of up to four 6-bit sextets, and how many we have buffered.
    let mut acc: u32 = 0;
    let mut have: u8 = 0;
    let mut saw_pad = false;
    for &b in input.as_bytes() {
        match b {
            b' ' | b'\t' | b'\n' | b'\r' | 0x0c => continue,
            b'=' => {
                saw_pad = true;
                continue;
            }
            _ => {}
        }
        // A non-whitespace, non-`=` byte after padding has begun is malformed.
        if saw_pad {
            return None;
        }
        let sextet = base64_value(b)?;
        acc = (acc << 6) | u32::from(sextet);
        have += 1;
        if have == 4 {
            out.push((acc >> 16) as u8);
            out.push((acc >> 8) as u8);
            out.push(acc as u8);
            acc = 0;
            have = 0;
        }
    }
    match have {
        0 => {}
        // A single trailing sextet carries only 6 bits — not a whole byte; malformed.
        1 => return None,
        2 => out.push((acc >> 4) as u8),
        3 => {
            out.push((acc >> 10) as u8);
            out.push((acc >> 2) as u8);
        }
        _ => unreachable!("`have` is reset to 0 once it reaches 4"),
    }
    Some(out)
}

/// Map a base64 alphabet byte to its 6-bit value, or `None` if it is not an alphabet character.
#[inline]
fn base64_value(b: u8) -> Option<u8> {
    match b {
        b'A'..=b'Z' => Some(b - b'A'),
        b'a'..=b'z' => Some(b - b'a' + 26),
        b'0'..=b'9' => Some(b - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// WHATWG percent-decode `input` to raw bytes: every `%HH` (two hex digits) becomes the byte `0xHH`;
/// every other byte is copied verbatim (a `%` not followed by two hex digits is left as a literal
/// `%`). The result is the body of a non-base64 `data:` URL. Pure: one output `Vec`.
pub(crate) fn percent_decode_bytes(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        // A `%` followed by two hex digits decodes to one byte; otherwise the `%` is a literal.
        match (bytes[i], bytes.get(i + 1).copied(), bytes.get(i + 2).copied()) {
            (b'%', Some(h), Some(l)) if hex_value(h).is_some() && hex_value(l).is_some() => {
                out.push((hex_value(h).unwrap() << 4) | hex_value(l).unwrap());
                i += 3;
            }
            (byte, _, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    out
}

/// Map an ASCII hex digit to its 0..=15 value, or `None` if it is not a hex digit.
#[inline]
fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
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
/// * `parseDataUrl(url) -> { mimeType: string, bytes: number[] }` — the WHATWG `data:` URL processor;
///   throws a `TypeError` for a non-`data:` / malformed URL so `fetch()` can translate it to a
///   rejected promise. `bytes` is an integer array (the pinned Nova rev exposes no embedder
///   `Uint8Array` construction — see `text_encoding.rs`); the bootstrap turns it into a body.
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
    define_fn(agent, obj, "parseDataUrl", js::parse_data_url, 1, gc);

    // The networking half of `fetch()`: start a background `http://` request and poll it for
    // completion. These drive the same reactor `node:http`/`node:net` use, so the global `fetch`
    // performs a real request without `require`-ing `node:http`.
    define_fn(agent, obj, "httpStart", js::http_start, 4, gc);
    define_fn(agent, obj, "httpsStart", js::https_start, 4, gc);
    define_fn(agent, obj, "httpPoll", js::http_poll, 1, gc);

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

    /// `parseDataUrl(url)` -> `{ mimeType: string, bytes: number[] }`.
    ///
    /// Runs the WHATWG `data:` URL processor ([`parse_data_url`]) and marshals the result into a plain
    /// JS object the fetch bootstrap turns into a `Response`: `mimeType` is the `Content-Type` the
    /// response reports, and `bytes` is the decoded body as an integer `Array` (each `0..=255`), the
    /// same byte-exchange shape `text_encoding` uses because the pinned Nova rev exposes no embedder
    /// `Uint8Array` construction. A non-`data:` or malformed URL throws a `TypeError`, which the
    /// bootstrap catches and turns into a rejected `fetch()` promise — matching how a browser surfaces
    /// a failed `data:` fetch.
    pub(super) fn parse_data_url<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let gc = gc.into_nogc();
        let url = read_str(agent, args.get(0));
        let parsed = match super::parse_data_url(&url) {
            Ok(parsed) => parsed,
            Err(e) => {
                let message = match e {
                    DataUrlError::NotDataScheme => "fetch failed: not a data: URL",
                    DataUrlError::MissingComma => "fetch failed: malformed data: URL (no comma)",
                    DataUrlError::InvalidBase64 => "fetch failed: invalid base64 in data: URL",
                };
                return Err(agent.throw_exception_with_static_message(
                    ExceptionType::TypeError,
                    message,
                    gc,
                ));
            }
        };

        let mime: Value = JsString::from_str(agent, parsed.mime_type(), gc).into();
        // Body bytes as an integer Array (each element a small integer 0..=255).
        let byte_values: Vec<Value> = parsed
            .body()
            .iter()
            .map(|&b| Value::Integer(i32::from(b).into()))
            .collect();
        let bytes: Value = Array::from_slice(agent, &byte_values, gc).into();

        let obj = OrdinaryObject::create_empty_object(agent, gc);
        let mime_key = PropertyKey::from_static_str(agent, "mimeType", gc);
        unwrap_try(obj.try_define_own_property(
            agent,
            mime_key,
            nova_vm::ecmascript::PropertyDescriptor::new_data_descriptor(mime),
            None,
            gc,
        ));
        let bytes_key = PropertyKey::from_static_str(agent, "bytes", gc);
        unwrap_try(obj.try_define_own_property(
            agent,
            bytes_key,
            nova_vm::ecmascript::PropertyDescriptor::new_data_descriptor(bytes),
            None,
            gc,
        ));
        Ok(obj.into())
    }

    /// `httpStart(method, url, headerPairs, body)` -> client handle (number).
    ///
    /// Splits the `http://` URL and spawns a background request on the shared reactor (the same one
    /// `node:http`/`node:net` use). Throws a `TypeError` for a non-`http://` URL (notably `https://`,
    /// which has no TLS transport here) so `fetch()` rejects with a catchable error.
    pub(super) fn http_start<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let method = read_str(agent, args.get(0));
        let url = read_str(agent, args.get(1));
        let header_value = args.get(2);
        let body = read_str(agent, args.get(3));
        let gc = gc.into_nogc();

        let headers = read_header_pairs(agent, header_value, gc);
        let parsed = match crate::node::http::split_http_url(&url) {
            Ok(p) => p,
            Err(e) => return Err(agent.throw_exception(ExceptionType::TypeError, e, gc)),
        };
        let req = crate::node::http::ParsedRequest {
            method: method.to_ascii_uppercase(),
            path: parsed.path,
            headers,
            body: body.into_bytes(),
        };
        let handle = crate::node::net::client_start(req, parsed.host, parsed.port);
        Ok(Value::Integer((handle as i32).into()))
    }

    /// `httpsStart(method, url, headerPairs, body)` -> client handle (number).
    ///
    /// The TLS counterpart of [`http_start`]: splits the `https://` URL and starts a real TLS request
    /// on the shared reactor, polled by the same [`http_poll`]. Verification uses the runtime's system
    /// trust (an empty root store — there is no bundled CA bundle), which is the honest offline
    /// behavior: a public-CA endpoint reachable only with the system roots will reject rather than be
    /// silently trusted. WHATWG `fetch` exposes no per-call trust override, so this is the only mode.
    pub(super) fn https_start<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let method = read_str(agent, args.get(0));
        let url = read_str(agent, args.get(1));
        let header_value = args.get(2);
        let body = read_str(agent, args.get(3));
        let gc = gc.into_nogc();

        let headers = read_header_pairs(agent, header_value, gc);
        let parsed = match crate::node::http::split_https_url(&url) {
            Ok(p) => p,
            Err(e) => return Err(agent.throw_exception(ExceptionType::TypeError, e, gc)),
        };
        let req = crate::node::http::ParsedRequest {
            method: method.to_ascii_uppercase(),
            path: parsed.path,
            headers,
            body: body.into_bytes(),
        };
        let handle = crate::node::net::tls_client_start(
            req,
            parsed.host,
            parsed.port,
            // System trust: real verification against the (empty) bundled root store.
            crate::node::tls::ClientTrust::Pinned(Vec::new()),
        );
        Ok(Value::Integer((handle as i32).into()))
    }

    /// `httpPoll(handle)` -> `{ pending } | { error } | { response: { status, statusText, headers, body } }`.
    pub(super) fn http_poll<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let handle = match args.get(0) {
            Value::Integer(i) => u64::try_from(i.into_i64()).unwrap_or(u64::MAX),
            _ => u64::MAX,
        };
        let gc = gc.into_nogc();
        let obj = OrdinaryObject::create_empty_object(agent, gc);
        let set_str = |agent: &mut Agent, obj: OrdinaryObject, key: &'static str, value: &str| {
            let v: Value = JsString::from_str(agent, value, gc).into();
            let k = PropertyKey::from_static_str(agent, key, gc);
            unwrap_try(obj.try_define_own_property(
                agent,
                k,
                nova_vm::ecmascript::PropertyDescriptor::new_data_descriptor(v),
                None,
                gc,
            ));
        };
        let set_val = |agent: &mut Agent, obj: OrdinaryObject, key: &'static str, value: Value| {
            let k = PropertyKey::from_static_str(agent, key, gc);
            unwrap_try(obj.try_define_own_property(
                agent,
                k,
                nova_vm::ecmascript::PropertyDescriptor::new_data_descriptor(value),
                None,
                gc,
            ));
        };
        match crate::node::net::client_poll(handle) {
            crate::node::net::ClientPoll::Pending => {
                set_val(agent, obj, "pending", Value::Boolean(true));
            }
            crate::node::net::ClientPoll::Unknown => {
                set_str(agent, obj, "error", "unknown client handle");
            }
            crate::node::net::ClientPoll::Done(Err(e)) => {
                set_str(agent, obj, "error", &e);
            }
            crate::node::net::ClientPoll::Done(Ok(resp)) => {
                let response = OrdinaryObject::create_empty_object(agent, gc);
                set_val(
                    agent,
                    response,
                    "status",
                    Value::Integer(i32::from(resp.status).into()),
                );
                set_str(agent, response, "statusText", &resp.status_text);
                let pairs: Vec<Value> = resp
                    .headers
                    .iter()
                    .map(|(n, v)| {
                        let name: Value = JsString::from_str(agent, n, gc).into();
                        let value: Value = JsString::from_str(agent, v, gc).into();
                        Array::from_slice(agent, &[name, value], gc).into()
                    })
                    .collect();
                let headers_arr: Value = Array::from_slice(agent, &pairs, gc).into();
                set_val(agent, response, "headers", headers_arr);
                let body = std::string::String::from_utf8_lossy(&resp.body);
                set_str(agent, response, "body", &body);
                set_val(agent, obj, "response", response.into());
            }
        }
        Ok(obj.into())
    }

    /// Read a JS `[name, value][]` header array into Rust tuples (defensive: skips non-pair items).
    fn read_header_pairs(
        agent: &mut Agent,
        value: Value,
        gc: NoGcScope,
    ) -> Vec<(std::string::String, std::string::String)> {
        let mut out = Vec::new();
        let Ok(array) = Array::try_from(value) else {
            return out;
        };
        let len = array.len(agent);
        for i in 0..len {
            let key = PropertyKey::Integer(i.into());
            let pair = get_value(unwrap_try(array.try_get(agent, key, array.into(), None, gc)));
            let Ok(pair) = Array::try_from(pair) else {
                continue;
            };
            let name_v = get_value(unwrap_try(pair.try_get(
                agent,
                PropertyKey::Integer(0.into()),
                pair.into(),
                None,
                gc,
            )));
            let value_v = get_value(unwrap_try(pair.try_get(
                agent,
                PropertyKey::Integer(1.into()),
                pair.into(),
                None,
                gc,
            )));
            out.push((read_str(agent, name_v), read_str(agent, value_v)));
        }
        out
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

    // =============================================================================================
    // WHATWG `data:` URL processor (pure Rust core).
    // =============================================================================================

    #[test]
    fn base64_decodes_standard_alphabet_with_and_without_padding() {
        // "Man" -> "TWFu" (no padding); "Ma" -> "TWE=" (one pad); "M" -> "TQ==" (two pads).
        assert_eq!(decode_base64("TWFu").unwrap(), b"Man");
        assert_eq!(decode_base64("TWE=").unwrap(), b"Ma");
        assert_eq!(decode_base64("TQ==").unwrap(), b"M");
        // Padding is optional: the same partial groups decode without the `=`.
        assert_eq!(decode_base64("TWE").unwrap(), b"Ma");
        assert_eq!(decode_base64("TQ").unwrap(), b"M");
        // Empty input -> empty output.
        assert_eq!(decode_base64("").unwrap(), b"");
    }

    #[test]
    fn base64_tolerates_ascii_whitespace_anywhere() {
        // MIME base64 is commonly folded across lines; whitespace between sextets is ignored.
        assert_eq!(decode_base64("TW Fu").unwrap(), b"Man");
        assert_eq!(decode_base64("TWFu\n").unwrap(), b"Man");
        assert_eq!(decode_base64("  TWFu  ").unwrap(), b"Man");
        assert_eq!(decode_base64("T\tW\rF\nu").unwrap(), b"Man");
    }

    #[test]
    fn base64_rejects_malformed_input() {
        // A non-alphabet byte is an error.
        assert!(decode_base64("TW*u").is_none());
        // A lone trailing sextet carries only 6 bits — not a whole byte.
        assert!(decode_base64("T").is_none());
        assert!(decode_base64("TWFuT").is_none());
        // Data after padding has begun is malformed.
        assert!(decode_base64("TWE=TWFu").is_none());
    }

    #[test]
    fn percent_decode_bytes_decodes_escapes_and_keeps_the_rest() {
        assert_eq!(percent_decode_bytes("Hello%2C%20World"), b"Hello, World");
        // A `%` not followed by two hex digits is a literal `%`.
        assert_eq!(percent_decode_bytes("100%done"), b"100%done");
        assert_eq!(percent_decode_bytes("trailing%"), b"trailing%");
        assert_eq!(percent_decode_bytes("bad%zz"), b"bad%zz");
        // Lower- and upper-case hex are both accepted.
        assert_eq!(percent_decode_bytes("%e2%9c%93"), [0xe2, 0x9c, 0x93]);
        assert_eq!(percent_decode_bytes("%E2%9C%93"), [0xe2, 0x9c, 0x93]);
    }

    #[test]
    fn data_url_plain_text_uses_default_mime() {
        let d = parse_data_url("data:,Hello%2C%20World").unwrap();
        assert_eq!(d.mime_type(), "text/plain;charset=US-ASCII");
        assert_eq!(d.body(), b"Hello, World");
    }

    #[test]
    fn data_url_explicit_mime_is_preserved() {
        let d = parse_data_url("data:application/json,%7B%22a%22%3A1%7D").unwrap();
        assert_eq!(d.mime_type(), "application/json");
        assert_eq!(d.body(), br#"{"a":1}"#);
    }

    #[test]
    fn data_url_base64_payload_is_decoded() {
        // base64("Hello") == "SGVsbG8=".
        let d = parse_data_url("data:text/plain;base64,SGVsbG8=").unwrap();
        assert_eq!(d.mime_type(), "text/plain");
        assert_eq!(d.body(), b"Hello");
    }

    #[test]
    fn data_url_bare_parameter_list_gets_text_plain_prefix() {
        // A header that is only a parameter list (`;charset=utf-8`) is prefixed with `text/plain`.
        let d = parse_data_url("data:;charset=utf-8,abc").unwrap();
        assert_eq!(d.mime_type(), "text/plain;charset=utf-8");
        assert_eq!(d.body(), b"abc");
    }

    #[test]
    fn data_url_scheme_is_case_insensitive_and_first_comma_splits() {
        // Scheme matched case-insensitively; only the FIRST comma splits header from data, so a comma
        // inside the (percent-decoded) data is preserved.
        let d = parse_data_url("DATA:text/plain,a,b,c").unwrap();
        assert_eq!(d.mime_type(), "text/plain");
        assert_eq!(d.body(), b"a,b,c");
    }

    #[test]
    fn data_url_errors_are_classified() {
        assert_eq!(parse_data_url("https://x"), Err(DataUrlError::NotDataScheme));
        assert_eq!(parse_data_url("data:no-comma-here"), Err(DataUrlError::MissingComma));
        // `*` is not a base64 alphabet byte.
        assert_eq!(
            parse_data_url("data:text/plain;base64,****"),
            Err(DataUrlError::InvalidBase64)
        );
    }

    // =============================================================================================
    // JS round-trip: the production WHATWG object model + a `data:` URL `fetch()`.
    //
    // These exercise the same Headers/Request/Response globals user code sees (materialized by the
    // globals layer) plus this module's native `parseDataUrl`, end-to-end through `JsRuntime`. The
    // `fetch()` global itself has no networking transport in this offline runtime (it lives in the
    // globals layer and rejects for `http(s):`); the transport-free `data:` scheme is driven here
    // over the native `parseDataUrl` this module exports, proving a `data:` fetch resolves to a real
    // `Response` whose `text()`/`json()`/`arrayBuffer()` work.
    // =============================================================================================

    use crate::JsRuntime;
    use crate::node::core::HostState;
    use nova_vm::ecmascript::PropertyDescriptor;
    use nova_vm::engine::Bindable;
    use serde_json::{json, Value as JsonValue};

    /// The `fetch()`-over-`data:` bootstrap used by the round-trip tests. It mirrors the production
    /// shape: parse the `data:` URL with the native `parseDataUrl`, build a real `Response` from the
    /// decoded bytes (decoded to text via the real `TextDecoder` global), and return it as a resolved
    /// promise — exactly what a WinterCG `fetch("data:...")` does. Parked on `globalThis.__dataFetch`.
    const DATA_FETCH_BOOTSTRAP: &str = r#"
      globalThis.__dataFetch = function (url, init) {
        return Promise.resolve().then(function () {
          var parsed = globalThis.__fetch_native.parseDataUrl(String(url));
          var text = new TextDecoder().decode(Uint8Array.from(parsed.bytes));
          return new Response(text, { status: 200, headers: { "content-type": parsed.mimeType } });
        });
      };
      true
    "#;

    /// Build a Node-compat runtime and park this module's native exports on `globalThis.__fetch_native`
    /// so a JS bootstrap can drive `parseDataUrl` end-to-end. The native module backing `fetch` is not
    /// an importable `node:` builtin (it is a globals-only module), so — exactly as the globals layer
    /// does via its hidden slot — the host installs it directly here for the test.
    fn runtime_with_fetch_native() -> JsRuntime {
        let mut rt = JsRuntime::with_node_compat();
        let JsRuntime {
            agent,
            realm,
            host_state,
        } = &mut rt;
        // Borrow the HostState separately from the agent (the decoupled borrow the module path needs).
        let host: &HostState = host_state
            .as_deref()
            .expect("with_node_compat installs a HostState");
        agent.run_in_realm(realm, |agent, mut gc| {
            let ctx = NodeCtx::new(host);
            let exports = install(agent, &ctx, gc.reborrow())
                .expect("fetch module installs")
                .unbind();
            let nogc = gc.into_nogc();
            let global = agent.current_realm(nogc).global_object(agent);
            let key = PropertyKey::from_static_str(agent, "__fetch_native", nogc);
            unwrap_try(global.try_define_own_property(
                agent,
                key,
                PropertyDescriptor::new_data_descriptor(exports.bind(nogc)),
                None,
                nogc,
            ));
        });
        rt
    }

    /// Evaluate `source` in a runtime that has this module's native exports parked on
    /// `globalThis.__fetch_native`, returning the JSON completion value.
    fn eval_with_fetch_native(source: &str) -> JsonValue {
        let mut rt = runtime_with_fetch_native();
        rt.eval(source).expect("script evaluates")
    }

    /// Drive an async body to completion and read its result.
    ///
    /// `JsRuntime::eval` reads the completion value *synchronously* and only then drains the event
    /// loop, so a top-level promise (e.g. the result of an `async function`) has not settled when the
    /// completion value is captured. The established runtime pattern (see `fs_promises` tests) is to
    /// let the promise's continuation stash its result on a `globalThis` slot and return a synchronous
    /// value; the post-eval drain runs the continuation, and a *second* eval reads the now-settled
    /// slot. This helper wraps that: `body` is the inside of an `async function`, expected to
    /// `return` the JSON-able result; we run it, route its resolution/rejection onto `globalThis.__out`
    /// (rejections as `"rejected: <name>"`), drain, then read `__out` back.
    fn run_async(prelude: &str, body: &str) -> JsonValue {
        let mut rt = runtime_with_fetch_native();
        let scheduler = format!(
            r#"
            {prelude}
            globalThis.__out = null;
            (async function () {{ {body} }})()
              .then(function (v) {{ globalThis.__out = v; }})
              .catch(function (e) {{ globalThis.__out = "rejected: " + (e && e.name); }});
            0
            "#,
        );
        rt.eval(&scheduler).expect("async scheduler evaluates");
        // The drain after the first eval has run the .then/.catch continuation; read the result.
        rt.eval("globalThis.__out").expect("result read evaluates")
    }

    #[test]
    fn headers_object_model_round_trips_through_the_global() {
        // The real `Headers` global: case-insensitive get, append-combine, has/delete, and the
        // Set-Cookie carve-out (kept separate, returned by getSetCookie).
        let out = eval_with_fetch_native(
            r#"
            const h = new Headers({ "Content-Type": "text/plain" });
            h.append("Accept", "text/html");
            h.append("accept", "application/json");
            h.append("Set-Cookie", "a=1");
            h.append("Set-Cookie", "b=2");
            ({
              ct: h.get("content-type"),
              accept: h.get("ACCEPT"),
              hasAccept: h.has("accept"),
              cookies: h.getSetCookie(),
            })
            "#,
        );
        assert_eq!(
            out,
            json!({
                "ct": "text/plain",
                "accept": "text/html, application/json",
                "hasAccept": true,
                "cookies": ["a=1", "b=2"],
            })
        );
    }

    #[test]
    fn response_text_and_json_resolve_through_the_event_loop() {
        // `Response.text()` and `Response.json()` are promise-returning; the runtime drains the event
        // loop after the script so the awaited values settle before the result is read.
        let out = run_async(
            "",
            r#"
              const r = new Response('{"x":42,"y":[1,2]}', {
                status: 201,
                headers: { "content-type": "application/json" },
              });
              const text = await r.clone().text();
              const data = await r.json();
              return { ok: r.ok, status: r.status, text, x: data.x, y: data.y, ct: r.headers.get("content-type") };
            "#,
        );
        assert_eq!(
            out,
            json!({
                // 201 is within the 200..=299 "ok" range.
                "ok": true,
                "status": 201,
                "text": "{\"x\":42,\"y\":[1,2]}",
                "x": 42,
                "y": [1, 2],
                "ct": "application/json",
            })
        );
    }

    #[test]
    fn response_static_json_helper_sets_content_type() {
        // `Response.json(data)` is the WHATWG static helper: serializes to JSON and defaults the
        // content-type. `ok` is true for the default 200 status.
        let out = run_async(
            "",
            r#"
              const r = Response.json({ hello: "world" });
              return { ok: r.ok, status: r.status, ct: r.headers.get("content-type"), body: await r.text() };
            "#,
        );
        assert_eq!(
            out,
            json!({
                "ok": true,
                "status": 200,
                "ct": "application/json",
                "body": "{\"hello\":\"world\"}",
            })
        );
    }

    #[test]
    fn data_url_fetch_resolves_to_a_real_response_text() {
        // A `data:` URL fetch (driven over the native `parseDataUrl`) resolves to a `Response` whose
        // body and content-type come straight from the URL — no transport involved.
        let out = run_async(
            DATA_FETCH_BOOTSTRAP,
            r#"
              const r = await globalThis.__dataFetch("data:,Hello%2C%20World");
              return { ok: r.ok, status: r.status, ct: r.headers.get("content-type"), body: await r.text() };
            "#,
        );
        assert_eq!(
            out,
            json!({
                "ok": true,
                "status": 200,
                "ct": "text/plain;charset=US-ASCII",
                "body": "Hello, World",
            })
        );
    }

    #[test]
    fn data_url_fetch_decodes_base64_json_and_response_json_parses_it() {
        // base64('{"n":7}') == 'eyJuIjo3fQ=='. The `data:` fetch decodes it; `Response.json()` parses
        // the decoded body — proving the base64 path and `json()` compose end-to-end.
        let out = run_async(
            DATA_FETCH_BOOTSTRAP,
            r#"
              const r = await globalThis.__dataFetch("data:application/json;base64,eyJuIjo3fQ==");
              const data = await r.json();
              return { ct: r.headers.get("content-type"), n: data.n };
            "#,
        );
        assert_eq!(out, json!({ "ct": "application/json", "n": 7 }));
    }

    #[test]
    fn data_url_fetch_rejects_a_malformed_url() {
        // A non-`data:` URL makes `parseDataUrl` throw a TypeError, which the bootstrap surfaces as a
        // rejected promise — the WHATWG failure mode for an unfetchable `data:` request. `run_async`
        // routes the rejection to `"rejected: <name>"`.
        let out = run_async(
            DATA_FETCH_BOOTSTRAP,
            r#"
              await globalThis.__dataFetch("https://example.com");
              return "resolved";
            "#,
        );
        assert_eq!(out, json!("rejected: TypeError"));
    }
}
