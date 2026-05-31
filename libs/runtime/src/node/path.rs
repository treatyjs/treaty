//! `node:path` — path manipulation (`join`, `resolve`, `normalize`, `dirname`, `basename`,
//! `extname`, `isAbsolute`, `relative`, `parse`, `format`, `sep`, `delimiter`), with the platform
//! default plus the explicit `posix` and `win32` sub-namespaces.
//!
//! Lazy: built only on first `require("node:path")` / `import`. The shared core supplies the uniform
//! [`install`] seam and the [`NodeModule`] specifier binding; this file owns the body.
//!
//! Design (architecture tenets):
//!
//! * The path algorithms are implemented as **pure Rust free functions** over [`Flavor`]
//!   ([`join`], [`normalize`], [`resolve`], …). They never touch Nova, are allocation-conscious
//!   (returning [`Cow`] where the input already satisfies the result, `&str` slices for
//!   `dirname`/`basename`/`extname`), and are unit-tested in isolation (no JS agent needed) — tenets
//!   1 (no `unsafe`) and 3 (borrow / zero-copy over clone).
//! * The JS-facing [`RegularFn`]s are thin wrappers that read string arguments, delegate to the
//!   pure core, and hand the result back as a Nova string. The function set is materialized once,
//!   on first import (tenet 2: lazy).
//!
//! Faithfulness: the algorithms follow Node's `lib/path.js` (POSIX + Win32 semantics, including
//! Win32 drive-relative roots, UNC, mixed `/`+`\` separators, and `..` collapsing that stops at the
//! root). The platform default mirrors `path` on the host OS (`win32` on Windows, `posix`
//! elsewhere).

use std::borrow::Cow;

use nova_vm::ecmascript::{
    Agent, ArgumentsList, ExceptionType, InternalMethods, JsResult, Object, OrdinaryObject,
    PropertyDescriptor, PropertyKey, String as JsString, TryGetResult, Value, unwrap_try,
};
use nova_vm::engine::{Bindable, NoGcScope};

use crate::node::core::{InstallError, NodeCtx};
use crate::node::globals::define_fn;
use crate::node::{GcScope, NodeModule};

// ---------------------------------------------------------------------------------------------
// Pure path core (no Nova; unit-tested directly).
// ---------------------------------------------------------------------------------------------

/// The two path dialects Node exposes. Carries the separator/delimiter and the per-flavor
/// primitive operations so the higher-level functions stay flavor-agnostic.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Flavor {
    Posix,
    Win32,
}

impl Flavor {
    /// The default flavor for the host OS (mirrors Node's `require('path')`).
    #[cfg(windows)]
    const NATIVE: Flavor = Flavor::Win32;
    #[cfg(not(windows))]
    const NATIVE: Flavor = Flavor::Posix;

    /// Primary path separator (`/` posix, `\` win32).
    const fn sep(self) -> char {
        match self {
            Flavor::Posix => '/',
            Flavor::Win32 => '\\',
        }
    }

    /// `PATH`-style list delimiter (`:` posix, `;` win32).
    const fn delimiter(self) -> &'static str {
        match self {
            Flavor::Posix => ":",
            Flavor::Win32 => ";",
        }
    }

    /// Whether `c` separates path segments for this flavor. Win32 accepts both `/` and `\`.
    const fn is_sep(self, c: char) -> bool {
        match self {
            Flavor::Posix => c == '/',
            Flavor::Win32 => c == '/' || c == '\\',
        }
    }

    fn is_sep_byte(self, b: u8) -> bool {
        match self {
            Flavor::Posix => b == b'/',
            Flavor::Win32 => b == b'/' || b == b'\\',
        }
    }
}

/// A simple ASCII drive-letter check (`A`–`Z` / `a`–`z`).
fn is_drive_letter(c: char) -> bool {
    c.is_ascii_alphabetic()
}

/// Length (in bytes) of the Win32 "root" prefix of `p`, and whether that root is absolute.
///
/// Recognizes: `C:\`/`C:/` (absolute drive), `C:` (drive-relative — root present, not absolute),
/// `\\server\share` (UNC, absolute), and a lone leading separator (absolute on the current drive).
/// Returns `(root_len, is_absolute)`.
fn win32_root(p: &str) -> (usize, bool) {
    let b = p.as_bytes();
    let n = b.len();
    if n == 0 {
        return (0, false);
    }
    let sep = |x: u8| x == b'/' || x == b'\\';

    // UNC: \\server\share  (two leading separators).
    if n >= 2 && sep(b[0]) && sep(b[1]) {
        // Skip the server component, then the share component; the root is up to (not including)
        // the separator after the share.
        let mut i = 2;
        // server
        while i < n && !sep(b[i]) {
            i += 1;
        }
        if i < n {
            // separator after server
            i += 1;
            // share
            let share_start = i;
            while i < n && !sep(b[i]) {
                i += 1;
            }
            if i > share_start {
                // root spans through the share (e.g. \\srv\share). Node treats the trailing
                // separator after the share as part of the root if present.
                return (i, true);
            }
        }
        // Malformed UNC (just "\\") — treat the two separators as an absolute root.
        return (2, true);
    }

    // Drive: C: optionally followed by a separator.
    if n >= 2 && is_drive_letter(b[0] as char) && b[1] == b':' {
        if n >= 3 && sep(b[2]) {
            return (3, true); // C:\  -> absolute
        }
        return (2, false); // C:   -> drive-relative (root present, not absolute)
    }

    // Lone leading separator -> absolute (on current drive).
    if sep(b[0]) {
        return (1, true);
    }

    (0, false)
}

/// `(root_len, is_absolute)` for `p` under `flavor`.
fn root_of(flavor: Flavor, p: &str) -> (usize, bool) {
    match flavor {
        Flavor::Posix => {
            if p.as_bytes().first() == Some(&b'/') {
                (1, true)
            } else {
                (0, false)
            }
        }
        Flavor::Win32 => win32_root(p),
    }
}

/// `true` if `p` is an absolute path for `flavor`.
pub(crate) fn is_absolute(flavor: Flavor, p: &str) -> bool {
    root_of(flavor, p).1
}

/// Collapse `.`/`..`/duplicate-separator noise in the non-root portion `body`.
///
/// `allow_above_root` lets leading `..` segments survive (used by `resolve`/`relative` on relative
/// inputs; `false` once an absolute root has been established, matching Node where `..` cannot
/// escape `/`). Returns the segments joined by `sep` (no leading/trailing separator).
fn normalize_segments(flavor: Flavor, body: &str, allow_above_root: bool) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut above = 0usize; // count of surviving leading ".." when allow_above_root.
    for seg in body.split(|c| flavor.is_sep(c)) {
        match seg {
            "" | "." => {}
            ".." => {
                if let Some(last) = out.last() {
                    if *last != ".." {
                        out.pop();
                        continue;
                    }
                }
                if allow_above_root {
                    out.push("..");
                    above += 1;
                }
                // else: drop (cannot go above an absolute root).
            }
            other => out.push(other),
        }
    }
    let _ = above;
    let sep = flavor.sep();
    let mut s = String::new();
    for (i, seg) in out.iter().enumerate() {
        if i > 0 {
            s.push(sep);
        }
        s.push_str(seg);
    }
    s
}

/// `path.normalize(p)` for `flavor`.
///
/// Preserves the root, collapses interior `.`/`..`/redundant separators, and keeps a single
/// trailing separator iff the input ended in one and the normalized body is non-empty.
pub(crate) fn normalize(flavor: Flavor, p: &str) -> Cow<'_, str> {
    if p.is_empty() {
        return Cow::Borrowed(".");
    }
    let (root_len, is_abs) = root_of(flavor, p);
    let root = &p[..root_len];
    let body = &p[root_len..];

    let trailing = body
        .chars()
        .next_back()
        .is_some_and(|c| flavor.is_sep(c));

    let normalized_body = normalize_segments(flavor, body, !is_abs);

    // Build the result root, normalizing separators inside it for win32 (so `C:/` -> `C:\`).
    let mut result = String::with_capacity(p.len());
    if root_len > 0 {
        match flavor {
            Flavor::Win32 => {
                for c in root.chars() {
                    result.push(if c == '/' { '\\' } else { c });
                }
            }
            Flavor::Posix => result.push_str(root),
        }
    }

    if normalized_body.is_empty() {
        if root_len > 0 {
            return Cow::Owned(result);
        }
        // No root and nothing left: ".", or "../.." style chains handled above.
        return Cow::Borrowed(".");
    }

    // Ensure a separator between an *absolute* root that lacks a trailing one (a UNC `\\srv\share`)
    // and the body. `C:\` and a leading `/` already end in a separator; a drive-relative `C:` root
    // is deliberately left separatorless (Node keeps `C:foo` relative to the drive's CWD).
    if root_len > 0 && is_abs {
        let last = result.chars().next_back().unwrap();
        if !flavor.is_sep(last) {
            result.push(flavor.sep());
        }
    }

    result.push_str(&normalized_body);
    if trailing {
        result.push(flavor.sep());
    }
    Cow::Owned(result)
}

/// `path.join(parts…)` for `flavor`: concatenate with the separator, then normalize.
pub(crate) fn join(flavor: Flavor, parts: &[&str]) -> String {
    let mut joined = String::new();
    for part in parts {
        if part.is_empty() {
            continue;
        }
        if joined.is_empty() {
            joined.push_str(part);
        } else {
            joined.push(flavor.sep());
            joined.push_str(part);
        }
    }
    if joined.is_empty() {
        return ".".to_owned();
    }
    normalize(flavor, &joined).into_owned()
}

/// `path.resolve(parts…)` for `flavor`, resolving right-to-left against `cwd` until absolute.
pub(crate) fn resolve(flavor: Flavor, cwd: &str, parts: &[&str]) -> String {
    // Accumulate from the rightmost argument leftward until we have an absolute path, then prepend
    // the CWD if still relative. This mirrors Node's `path.resolve`.
    let mut resolved = String::new();
    let mut resolved_absolute = false;

    for part in parts.iter().rev() {
        if part.is_empty() {
            continue;
        }
        prepend(&mut resolved, part, flavor);
        if is_absolute(flavor, part) {
            resolved_absolute = true;
            break;
        }
    }

    if !resolved_absolute {
        prepend(&mut resolved, cwd, flavor);
    }

    let (root_len, is_abs) = root_of(flavor, &resolved);
    let root = &resolved[..root_len];
    let body = &resolved[root_len..];
    let normalized_body = normalize_segments(flavor, body, !is_abs);

    let mut out = String::with_capacity(resolved.len());
    match flavor {
        Flavor::Win32 => {
            for c in root.chars() {
                out.push(if c == '/' { '\\' } else { c });
            }
        }
        Flavor::Posix => out.push_str(root),
    }

    if normalized_body.is_empty() {
        if out.is_empty() {
            return ".".to_owned();
        }
        return out;
    }
    // Ensure a separator between a bare root and the body when the root has no trailing sep
    // (e.g. win32 "C:" + "foo").
    if !out.is_empty() {
        let last = out.chars().next_back().unwrap();
        if !flavor.is_sep(last) {
            out.push(flavor.sep());
        }
    }
    out.push_str(&normalized_body);
    out
}

/// Prepend `prefix` + separator (if needed) onto the front of `acc`.
fn prepend(acc: &mut String, prefix: &str, flavor: Flavor) {
    if acc.is_empty() {
        acc.push_str(prefix);
        return;
    }
    let needs_sep = !flavor.is_sep(prefix.chars().next_back().unwrap_or(' '))
        && !flavor.is_sep(acc.chars().next().unwrap_or(' '));
    let mut new = String::with_capacity(prefix.len() + 1 + acc.len());
    new.push_str(prefix);
    if needs_sep {
        new.push(flavor.sep());
    }
    new.push_str(acc);
    *acc = new;
}

/// `path.dirname(p)` for `flavor`. Returns a borrowed slice of `p` (zero-copy) where possible.
pub(crate) fn dirname(flavor: Flavor, p: &str) -> Cow<'_, str> {
    if p.is_empty() {
        return Cow::Borrowed(".");
    }
    let (root_len, _) = root_of(flavor, p);
    let b = p.as_bytes();
    let n = b.len();

    // Strip trailing separators (but keep at least the root).
    let mut end = n;
    while end > root_len && flavor.is_sep_byte(b[end - 1]) {
        end -= 1;
    }
    // Find the last separator before `end`, not inside the root.
    let mut last_sep: Option<usize> = None;
    let mut i = root_len;
    while i < end {
        if flavor.is_sep_byte(b[i]) {
            last_sep = Some(i);
        }
        i += 1;
    }

    match last_sep {
        Some(idx) => {
            // Directory is everything up to (and including the root's portion of) idx.
            let dir_end = idx.max(root_len);
            if dir_end == root_len && root_len > 0 {
                // e.g. "/foo" -> "/", "C:\foo" -> "C:\"
                Cow::Borrowed(&p[..root_len])
            } else if dir_end == 0 {
                Cow::Borrowed(".")
            } else {
                Cow::Borrowed(&p[..dir_end])
            }
        }
        None => {
            if root_len > 0 {
                Cow::Borrowed(&p[..root_len])
            } else {
                Cow::Borrowed(".")
            }
        }
    }
}

/// `path.basename(p)` for `flavor`, optionally stripping a trailing `ext`. Zero-copy slice of `p`.
pub(crate) fn basename<'a>(flavor: Flavor, p: &'a str, ext: Option<&str>) -> &'a str {
    let (root_len, _) = root_of(flavor, p);
    let b = p.as_bytes();
    let n = b.len();

    let mut end = n;
    while end > root_len && flavor.is_sep_byte(b[end - 1]) {
        end -= 1;
    }
    let mut start = root_len;
    let mut i = root_len;
    while i < end {
        if flavor.is_sep_byte(b[i]) {
            start = i + 1;
        }
        i += 1;
    }
    let base = &p[start..end];
    if let Some(ext) = ext {
        if !ext.is_empty() && base.len() > ext.len() && base.ends_with(ext) {
            return &base[..base.len() - ext.len()];
        }
    }
    base
}

/// `path.extname(p)` for `flavor` — the final `.ext` of the basename (incl. the dot), else "".
pub(crate) fn extname<'a>(flavor: Flavor, p: &'a str) -> &'a str {
    let base = basename(flavor, p, None);
    // Node: a leading dot (dotfile) is not an extension; the dot must be after the first char.
    match base.bytes().rposition(|c| c == b'.') {
        Some(0) | None => "",
        Some(idx) => &base[idx..],
    }
}

/// The components Node's `path.parse` produces.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub(crate) struct Parsed<'a> {
    pub root: &'a str,
    pub dir: Cow<'a, str>,
    pub base: &'a str,
    pub ext: &'a str,
    pub name: &'a str,
}

/// `path.parse(p)` for `flavor`. Slices borrow from `p`; `dir` may be borrowed.
pub(crate) fn parse(flavor: Flavor, p: &str) -> Parsed<'_> {
    let (root_len, _) = root_of(flavor, p);
    let root = &p[..root_len];
    let dir = dirname(flavor, p);
    let base = basename(flavor, p, None);
    let ext = extname(flavor, p);
    let name = &base[..base.len() - ext.len()];
    Parsed {
        root,
        dir,
        base,
        ext,
        name,
    }
}

/// `path.format(obj)` for `flavor`. Honors Node's precedence: `dir`+`base`, else `root`+`base`,
/// where `base` falls back to `name`+`ext`.
pub(crate) fn format(
    flavor: Flavor,
    root: &str,
    dir: &str,
    base: &str,
    name: &str,
    ext: &str,
) -> String {
    let base_owned: Cow<'_, str> = if base.is_empty() {
        let mut s = String::with_capacity(name.len() + ext.len());
        s.push_str(name);
        s.push_str(ext);
        Cow::Owned(s)
    } else {
        Cow::Borrowed(base)
    };

    let dir_part = if !dir.is_empty() { dir } else { root };
    if dir_part.is_empty() {
        return base_owned.into_owned();
    }
    if dir_part == root {
        // root + base, no extra separator.
        let mut s = String::with_capacity(dir_part.len() + base_owned.len());
        s.push_str(dir_part);
        s.push_str(&base_owned);
        return s;
    }
    let mut s = String::with_capacity(dir_part.len() + 1 + base_owned.len());
    s.push_str(dir_part);
    s.push(flavor.sep());
    s.push_str(&base_owned);
    s
}

/// `path.relative(from, to)` for `flavor`, computed against `cwd` (both args are first resolved).
pub(crate) fn relative(flavor: Flavor, cwd: &str, from: &str, to: &str) -> String {
    let from_abs = resolve(flavor, cwd, &[from]);
    let to_abs = resolve(flavor, cwd, &[to]);
    if from_abs == to_abs {
        return String::new();
    }

    let split = |s: &str| -> Vec<String> {
        let (rl, _) = root_of(flavor, s);
        s[rl..]
            .split(|c| flavor.is_sep(c))
            .filter(|seg| !seg.is_empty())
            .map(|seg| seg.to_owned())
            .collect()
    };
    // Win32 path comparison is case-insensitive.
    let eq = |a: &str, b: &str| match flavor {
        Flavor::Win32 => a.eq_ignore_ascii_case(b),
        Flavor::Posix => a == b,
    };

    let from_parts = split(&from_abs);
    let to_parts = split(&to_abs);

    let mut common = 0;
    while common < from_parts.len()
        && common < to_parts.len()
        && eq(&from_parts[common], &to_parts[common])
    {
        common += 1;
    }

    let mut out: Vec<&str> = Vec::new();
    for _ in common..from_parts.len() {
        out.push("..");
    }
    for seg in &to_parts[common..] {
        out.push(seg);
    }

    let sep = flavor.sep();
    let mut s = String::new();
    for (i, seg) in out.iter().enumerate() {
        if i > 0 {
            s.push(sep);
        }
        s.push_str(seg);
    }
    s
}

// ---------------------------------------------------------------------------------------------
// JS-facing wiring.
// ---------------------------------------------------------------------------------------------

pub(crate) struct PathModule;

impl NodeModule for PathModule {
    const SPECIFIER: &'static str = "path";

    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: GcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError> {
        install(agent, ctx, gc)
    }
}

/// Uniform per-module entry. Returns the `node:path` exports object (platform default flavor) with
/// nested `posix` and `win32` namespaces.
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    let gc = gc.into_nogc();
    let obj = OrdinaryObject::create_empty_object(agent, gc);

    // Default (native) flavor: the functions read `Flavor::NATIVE`.
    install_flavor_fns(agent, obj, Flavor::NATIVE, gc);

    // Nested namespaces.
    let posix = OrdinaryObject::create_empty_object(agent, gc);
    install_flavor_fns(agent, posix, Flavor::Posix, gc);
    let win32 = OrdinaryObject::create_empty_object(agent, gc);
    install_flavor_fns(agent, win32, Flavor::Win32, gc);

    define_object(agent, obj, "posix", posix, gc);
    define_object(agent, obj, "win32", win32, gc);
    // Node exposes both names; `posix` is also reachable as itself, and each namespace re-points to
    // the same two children so `path.posix.win32` works like Node.
    define_object(agent, posix, "posix", posix, gc);
    define_object(agent, posix, "win32", win32, gc);
    define_object(agent, win32, "posix", posix, gc);
    define_object(agent, win32, "win32", win32, gc);

    Ok(obj.into())
}

/// Install the per-flavor function set and `sep`/`delimiter` constants onto `obj`.
fn install_flavor_fns(agent: &mut Agent, obj: OrdinaryObject, flavor: Flavor, gc: NoGcScope) {
    // Dispatch by flavor through distinct monomorphized fn pointers (RegularFn cannot capture).
    let (
        join_fn,
        resolve_fn,
        normalize_fn,
        dirname_fn,
        basename_fn,
        extname_fn,
        is_absolute_fn,
        relative_fn,
        parse_fn,
        format_fn,
    ): (_, _, _, _, _, _, _, _, _, _) = match flavor {
        Flavor::Posix => (
            js::join_posix as nova_vm::ecmascript::RegularFn,
            js::resolve_posix as nova_vm::ecmascript::RegularFn,
            js::normalize_posix as nova_vm::ecmascript::RegularFn,
            js::dirname_posix as nova_vm::ecmascript::RegularFn,
            js::basename_posix as nova_vm::ecmascript::RegularFn,
            js::extname_posix as nova_vm::ecmascript::RegularFn,
            js::is_absolute_posix as nova_vm::ecmascript::RegularFn,
            js::relative_posix as nova_vm::ecmascript::RegularFn,
            js::parse_posix as nova_vm::ecmascript::RegularFn,
            js::format_posix as nova_vm::ecmascript::RegularFn,
        ),
        Flavor::Win32 => (
            js::join_win32 as nova_vm::ecmascript::RegularFn,
            js::resolve_win32 as nova_vm::ecmascript::RegularFn,
            js::normalize_win32 as nova_vm::ecmascript::RegularFn,
            js::dirname_win32 as nova_vm::ecmascript::RegularFn,
            js::basename_win32 as nova_vm::ecmascript::RegularFn,
            js::extname_win32 as nova_vm::ecmascript::RegularFn,
            js::is_absolute_win32 as nova_vm::ecmascript::RegularFn,
            js::relative_win32 as nova_vm::ecmascript::RegularFn,
            js::parse_win32 as nova_vm::ecmascript::RegularFn,
            js::format_win32 as nova_vm::ecmascript::RegularFn,
        ),
    };

    define_fn(agent, obj, "join", join_fn, 2, gc);
    define_fn(agent, obj, "resolve", resolve_fn, 2, gc);
    define_fn(agent, obj, "normalize", normalize_fn, 1, gc);
    define_fn(agent, obj, "dirname", dirname_fn, 1, gc);
    define_fn(agent, obj, "basename", basename_fn, 2, gc);
    define_fn(agent, obj, "extname", extname_fn, 1, gc);
    define_fn(agent, obj, "isAbsolute", is_absolute_fn, 1, gc);
    define_fn(agent, obj, "relative", relative_fn, 2, gc);
    define_fn(agent, obj, "parse", parse_fn, 1, gc);
    define_fn(agent, obj, "format", format_fn, 1, gc);

    define_static_string(agent, obj, "sep", flavor_sep_static(flavor), gc);
    define_static_string(agent, obj, "delimiter", flavor.delimiter(), gc);
}

const fn flavor_sep_static(flavor: Flavor) -> &'static str {
    match flavor {
        Flavor::Posix => "/",
        Flavor::Win32 => "\\",
    }
}

/// Define a child object as a data property `name` on `obj` (private helper; the shared
/// `globals::define_value` requires `Object`, which is what we have here).
fn define_object(
    agent: &mut Agent,
    obj: OrdinaryObject,
    name: &'static str,
    value: OrdinaryObject,
    gc: NoGcScope,
) {
    let key = PropertyKey::from_static_str(agent, name, gc);
    let value: Object = value.into();
    unwrap_try(obj.try_define_own_property(
        agent,
        key,
        PropertyDescriptor::new_data_descriptor(value),
        None,
        gc,
    ));
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

/// The JS wrapper functions. Each is a per-flavor [`RegularFn`] that reads string arguments, calls
/// the pure core, and returns a Nova string. They are grouped here so the public surface above
/// reads as a table.
mod js {
    use super::*;

    /// Read argument `i` as an owned Rust string, returning `None` for a non-string (the caller
    /// turns that into Node's `TypeError`). The owned copy is unavoidable: a Nova `String`'s
    /// `to_string_lossy` borrows from the handle, which does not outlive this read.
    fn arg_str(agent: &Agent, args: &ArgumentsList, i: usize) -> Option<String> {
        let v = args.get(i);
        JsString::try_from(v)
            .ok()
            .map(|s| s.to_string_lossy(agent).into_owned())
    }

    fn type_error<'gc>(agent: &mut Agent, gc: GcScope<'gc, '_>) -> JsResult<'gc, Value<'gc>> {
        Err(agent.throw_exception_with_static_message(
            ExceptionType::TypeError,
            "Path arguments must be of type string",
            gc.into_nogc(),
        ))
    }

    /// Collect every argument as a string, throwing `TypeError` on the first non-string.
    fn collect_strings(agent: &Agent, args: &ArgumentsList) -> Option<Vec<String>> {
        let mut out = Vec::with_capacity(args.len());
        for i in 0..args.len() {
            match JsString::try_from(args.get(i)) {
                Ok(s) => out.push(s.to_string_lossy(agent).into_owned()),
                Err(_) => return None,
            }
        }
        Some(out)
    }

    fn ret_string<'gc>(agent: &mut Agent, s: &str, gc: GcScope<'gc, '_>) -> Value<'gc> {
        JsString::from_str(agent, s, gc.into_nogc()).into()
    }

    // --- variadic (join / resolve) --------------------------------------------------------------

    fn join_impl<'gc>(
        agent: &mut Agent,
        args: ArgumentsList,
        flavor: Flavor,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let Some(parts) = collect_strings(agent, &args) else {
            return type_error(agent, gc);
        };
        let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
        let out = super::join(flavor, &refs);
        Ok(ret_string(agent, &out, gc))
    }

    fn resolve_impl<'gc>(
        agent: &mut Agent,
        args: ArgumentsList,
        flavor: Flavor,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let Some(parts) = collect_strings(agent, &args) else {
            return type_error(agent, gc);
        };
        let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
        // CWD for `resolve` comes from the host state when present, else the process CWD.
        let cwd = NodeCtx::from_agent(agent)
            .map(|c| c.cwd().to_string_lossy().into_owned())
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
        let out = super::resolve(flavor, &cwd, &refs);
        Ok(ret_string(agent, &out, gc))
    }

    // --- single-arg string -> string ------------------------------------------------------------

    fn one_arg<'gc>(
        agent: &mut Agent,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
        f: impl FnOnce(&str) -> String,
    ) -> JsResult<'gc, Value<'gc>> {
        let Some(s) = arg_str(agent, &args, 0) else {
            return type_error(agent, gc);
        };
        let out = f(&s);
        Ok(ret_string(agent, &out, gc))
    }

    fn relative_impl<'gc>(
        agent: &mut Agent,
        args: ArgumentsList,
        flavor: Flavor,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let Some(from) = arg_str(agent, &args, 0) else {
            return type_error(agent, gc);
        };
        let Some(to) = arg_str(agent, &args, 1) else {
            return type_error(agent, gc);
        };
        let cwd = NodeCtx::from_agent(agent)
            .map(|c| c.cwd().to_string_lossy().into_owned())
            .unwrap_or_default();
        let out = super::relative(flavor, &cwd, &from, &to);
        Ok(ret_string(agent, &out, gc))
    }

    fn basename_impl<'gc>(
        agent: &mut Agent,
        args: ArgumentsList,
        flavor: Flavor,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let Some(s) = arg_str(agent, &args, 0) else {
            return type_error(agent, gc);
        };
        let ext = if args.len() > 1 {
            match arg_str(agent, &args, 1) {
                Some(e) => Some(e),
                None => return type_error(agent, gc),
            }
        } else {
            None
        };
        let out = super::basename(flavor, &s, ext.as_deref()).to_owned();
        Ok(ret_string(agent, &out, gc))
    }

    fn is_absolute_impl<'gc>(
        agent: &mut Agent,
        args: ArgumentsList,
        flavor: Flavor,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let Some(s) = arg_str(agent, &args, 0) else {
            return type_error(agent, gc);
        };
        let _ = gc;
        Ok(Value::Boolean(super::is_absolute(flavor, &s)))
    }

    fn parse_impl<'gc>(
        agent: &mut Agent,
        args: ArgumentsList,
        flavor: Flavor,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let Some(s) = arg_str(agent, &args, 0) else {
            return type_error(agent, gc);
        };
        // Materialize the borrowed slices into owned strings before touching `agent` mutably to build
        // the result object (the `Parsed` borrows `s`, which we still own here).
        let parsed = super::parse(flavor, &s);
        let root = parsed.root.to_owned();
        let dir = parsed.dir.into_owned();
        let base = parsed.base.to_owned();
        let ext = parsed.ext.to_owned();
        let name = parsed.name.to_owned();

        let nogc = gc.into_nogc();
        let obj = OrdinaryObject::create_empty_object(agent, nogc);
        define_own_string(agent, obj, "root", &root, nogc);
        define_own_string(agent, obj, "dir", &dir, nogc);
        define_own_string(agent, obj, "base", &base, nogc);
        define_own_string(agent, obj, "ext", &ext, nogc);
        define_own_string(agent, obj, "name", &name, nogc);
        Ok(obj.into())
    }

    fn format_impl<'gc>(
        agent: &mut Agent,
        args: ArgumentsList,
        flavor: Flavor,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        let Ok(obj) = Object::try_from(args.get(0)) else {
            return type_error(agent, gc);
        };
        let obj = obj.unbind();
        let nogc = gc.nogc();
        let root = read_string_prop(agent, obj, "root", nogc);
        let dir = read_string_prop(agent, obj, "dir", nogc);
        let base = read_string_prop(agent, obj, "base", nogc);
        let name = read_string_prop(agent, obj, "name", nogc);
        let ext = read_string_prop(agent, obj, "ext", nogc);
        let out = super::format(flavor, &root, &dir, &base, &name, &ext);
        Ok(ret_string(agent, &out, gc))
    }

    /// Read property `name` off `obj` as a string, returning `""` when absent or non-string (mirrors
    /// Node's `path.format`, which treats a missing/empty component as the empty string).
    fn read_string_prop(agent: &mut Agent, obj: Object, name: &'static str, gc: NoGcScope) -> String {
        let key = PropertyKey::from_static_str(agent, name, gc);
        match unwrap_try(obj.try_get(agent, key, obj.into(), None, gc)) {
            TryGetResult::Value(v) => JsString::try_from(v)
                .ok()
                .map(|s| s.to_string_lossy(agent).into_owned())
                .unwrap_or_default(),
            _ => String::new(),
        }
    }

    /// Define `name = value` (string data property) on `obj`.
    fn define_own_string(agent: &mut Agent, obj: OrdinaryObject, name: &'static str, value: &str, gc: NoGcScope) {
        let key = PropertyKey::from_static_str(agent, name, gc);
        let js = JsString::from_str(agent, value, gc);
        unwrap_try(obj.try_define_own_property(
            agent,
            key,
            PropertyDescriptor::new_data_descriptor(js),
            None,
            gc,
        ));
    }

    // --- per-flavor monomorphized entry points (RegularFn pointers) -----------------------------

    macro_rules! flavored {
        ($posix:ident, $win32:ident, $body:expr) => {
            pub(super) fn $posix<'gc>(
                agent: &mut Agent,
                _this: Value,
                args: ArgumentsList,
                gc: GcScope<'gc, '_>,
            ) -> JsResult<'gc, Value<'gc>> {
                $body(agent, args, Flavor::Posix, gc)
            }
            pub(super) fn $win32<'gc>(
                agent: &mut Agent,
                _this: Value,
                args: ArgumentsList,
                gc: GcScope<'gc, '_>,
            ) -> JsResult<'gc, Value<'gc>> {
                $body(agent, args, Flavor::Win32, gc)
            }
        };
    }

    flavored!(join_posix, join_win32, join_impl);
    flavored!(resolve_posix, resolve_win32, resolve_impl);
    flavored!(relative_posix, relative_win32, relative_impl);
    flavored!(basename_posix, basename_win32, basename_impl);
    flavored!(is_absolute_posix, is_absolute_win32, is_absolute_impl);
    flavored!(parse_posix, parse_win32, parse_impl);
    flavored!(format_posix, format_win32, format_impl);

    macro_rules! flavored_one {
        ($posix:ident, $win32:ident, $func:path) => {
            pub(super) fn $posix<'gc>(
                agent: &mut Agent,
                _this: Value,
                args: ArgumentsList,
                gc: GcScope<'gc, '_>,
            ) -> JsResult<'gc, Value<'gc>> {
                one_arg(agent, args, gc, |s| $func(Flavor::Posix, s).into_owned())
            }
            pub(super) fn $win32<'gc>(
                agent: &mut Agent,
                _this: Value,
                args: ArgumentsList,
                gc: GcScope<'gc, '_>,
            ) -> JsResult<'gc, Value<'gc>> {
                one_arg(agent, args, gc, |s| $func(Flavor::Win32, s).into_owned())
            }
        };
    }

    // `normalize` returns Cow; `dirname` returns Cow; `extname` returns &str -> wrap to owned.
    flavored_one!(normalize_posix, normalize_win32, super::normalize);
    flavored_one!(dirname_posix, dirname_win32, super::dirname);

    pub(super) fn extname_posix<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        one_arg(agent, args, gc, |s| super::extname(Flavor::Posix, s).to_owned())
    }
    pub(super) fn extname_win32<'gc>(
        agent: &mut Agent,
        _this: Value,
        args: ArgumentsList,
        gc: GcScope<'gc, '_>,
    ) -> JsResult<'gc, Value<'gc>> {
        one_arg(agent, args, gc, |s| super::extname(Flavor::Win32, s).to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: Flavor = Flavor::Posix;
    const W: Flavor = Flavor::Win32;

    #[test]
    fn join_posix_matches_node() {
        assert_eq!(join(P, &["/foo", "bar", "baz/asdf", "quux", ".."]), "/foo/bar/baz/asdf");
        assert_eq!(join(P, &["foo", "bar"]), "foo/bar");
        assert_eq!(join(P, &["/foo", "../bar"]), "/bar");
        assert_eq!(join(P, &[]), ".");
        assert_eq!(join(P, &["foo", "", "bar"]), "foo/bar");
        assert_eq!(join(P, &["a/", "b"]), "a/b");
    }

    #[test]
    fn join_win32_matches_node() {
        assert_eq!(join(W, &["foo", "bar", "baz\\asdf", "quux", ".."]), "foo\\bar\\baz\\asdf");
        assert_eq!(join(W, &["C:\\foo", "bar"]), "C:\\foo\\bar");
        assert_eq!(join(W, &["C:\\foo", "..\\bar"]), "C:\\bar");
        // forward slashes normalize to backslashes
        assert_eq!(join(W, &["foo/bar", "baz"]), "foo\\bar\\baz");
    }

    #[test]
    fn normalize_posix() {
        assert_eq!(normalize(P, "/foo/bar//baz/asdf/quux/.."), "/foo/bar/baz/asdf");
        assert_eq!(normalize(P, "a/b/../c"), "a/c");
        assert_eq!(normalize(P, ""), ".");
        assert_eq!(normalize(P, "/.."), "/"); // cannot escape root
        assert_eq!(normalize(P, "foo/"), "foo/"); // trailing sep preserved
        assert_eq!(normalize(P, "../../a"), "../../a"); // relative .. kept
    }

    #[test]
    fn normalize_win32() {
        assert_eq!(normalize(W, "C:\\temp\\\\foo\\bar\\..\\"), "C:\\temp\\foo\\");
        assert_eq!(normalize(W, "C:/temp/foo/"), "C:\\temp\\foo\\");
        assert_eq!(normalize(W, "\\\\server\\share\\foo\\..\\bar"), "\\\\server\\share\\bar");
    }

    #[test]
    fn dirname_basename_extname_posix() {
        assert_eq!(dirname(P, "/foo/bar/baz/asdf/quux"), "/foo/bar/baz/asdf");
        assert_eq!(dirname(P, "/foo"), "/");
        assert_eq!(dirname(P, "foo"), ".");
        assert_eq!(dirname(P, "/"), "/");
        assert_eq!(basename(P, "/foo/bar/baz/asdf/quux.html", None), "quux.html");
        assert_eq!(basename(P, "/foo/bar/baz/asdf/quux.html", Some(".html")), "quux");
        assert_eq!(basename(P, "/foo/bar/", None), "bar");
        assert_eq!(extname(P, "index.html"), ".html");
        assert_eq!(extname(P, "index.coffee.md"), ".md");
        assert_eq!(extname(P, "index."), ".");
        assert_eq!(extname(P, "index"), "");
        assert_eq!(extname(P, ".index"), ""); // dotfile, no ext
    }

    #[test]
    fn dirname_basename_win32() {
        assert_eq!(dirname(W, "C:\\foo\\bar\\baz"), "C:\\foo\\bar");
        assert_eq!(dirname(W, "C:\\foo"), "C:\\");
        assert_eq!(basename(W, "C:\\foo\\bar\\quux.txt", None), "quux.txt");
        assert_eq!(basename(W, "C:\\foo\\bar\\quux.txt", Some(".txt")), "quux");
        assert_eq!(extname(W, "C:\\a\\b.JSON"), ".JSON");
    }

    #[test]
    fn is_absolute_both_flavors() {
        assert!(is_absolute(P, "/foo/bar"));
        assert!(!is_absolute(P, "qux/"));
        assert!(!is_absolute(P, "."));
        assert!(is_absolute(W, "C:\\foo"));
        assert!(is_absolute(W, "\\\\server\\share"));
        assert!(is_absolute(W, "\\foo"));
        assert!(!is_absolute(W, "C:foo")); // drive-relative is NOT absolute
        assert!(!is_absolute(W, "bar\\baz"));
    }

    #[test]
    fn resolve_posix() {
        assert_eq!(resolve(P, "/cwd", &["/foo/bar", "./baz"]), "/foo/bar/baz");
        assert_eq!(resolve(P, "/cwd", &["/foo/bar", "/tmp/file/"]), "/tmp/file");
        assert_eq!(resolve(P, "/home/user", &["a", "b"]), "/home/user/a/b");
        assert_eq!(resolve(P, "/a/b", &["../c"]), "/a/c");
    }

    #[test]
    fn resolve_win32() {
        assert_eq!(resolve(W, "C:\\cwd", &["C:\\foo\\bar", ".\\baz"]), "C:\\foo\\bar\\baz");
        assert_eq!(resolve(W, "C:\\cwd", &["foo", "bar"]), "C:\\cwd\\foo\\bar");
    }

    #[test]
    fn relative_posix() {
        assert_eq!(
            relative(P, "/", "/data/orandea/test/aaa", "/data/orandea/impl/bbb"),
            "../../impl/bbb"
        );
        assert_eq!(relative(P, "/", "/a/b/c", "/a/b/c"), "");
        assert_eq!(relative(P, "/", "/a/b", "/a/b/c/d"), "c/d");
    }

    #[test]
    fn relative_win32_is_case_insensitive() {
        assert_eq!(
            relative(W, "C:\\", "C:\\Foo\\Bar", "C:\\foo\\baz"),
            "..\\baz"
        );
    }

    #[test]
    fn parse_and_format_roundtrip_posix() {
        let p = parse(P, "/home/user/dir/file.txt");
        assert_eq!(p.root, "/");
        assert_eq!(p.dir, "/home/user/dir");
        assert_eq!(p.base, "file.txt");
        assert_eq!(p.ext, ".txt");
        assert_eq!(p.name, "file");
        // format(dir + base) round-trips the original.
        let formatted = format(P, p.root, &p.dir, p.base, p.name, p.ext);
        assert_eq!(formatted, "/home/user/dir/file.txt");
        // format(root + base) when no dir.
        assert_eq!(format(P, "/", "", "", "file", ".txt"), "/file.txt");
    }

    #[test]
    fn parse_win32() {
        let p = parse(W, "C:\\path\\dir\\file.txt");
        assert_eq!(p.root, "C:\\");
        assert_eq!(p.dir, "C:\\path\\dir");
        assert_eq!(p.base, "file.txt");
        assert_eq!(p.ext, ".txt");
        assert_eq!(p.name, "file");
    }

    #[test]
    fn parse_format_roundtrip_win32_drive_root() {
        // A drive-rooted win32 path decomposes and re-derives exactly (the parse-format contract on
        // the win32 namespace).
        let p = parse(W, "C:\\Users\\dev\\notes.md");
        assert_eq!(p.root, "C:\\");
        assert_eq!(p.dir, "C:\\Users\\dev");
        assert_eq!(p.base, "notes.md");
        assert_eq!(p.ext, ".md");
        assert_eq!(p.name, "notes");
        assert_eq!(
            format(W, p.root, &p.dir, p.base, p.name, p.ext),
            "C:\\Users\\dev\\notes.md"
        );
        // A bare drive root: dir == root, format must not double the separator.
        let r = parse(W, "C:\\file.txt");
        assert_eq!(r.root, "C:\\");
        assert_eq!(r.dir, "C:\\");
        assert_eq!(format(W, r.root, &r.dir, r.base, r.name, r.ext), "C:\\file.txt");
    }

    #[test]
    fn win32_drive_root_contract_matches_corpus() {
        // The exact assertions the win32-drive-roots conformance file pins down.
        assert!(is_absolute(W, "C:\\a\\b"));
        assert_eq!(dirname(W, "C:\\foo\\bar\\baz.txt"), "C:\\foo\\bar");
        assert_eq!(relative(W, "C:\\", "C:\\a\\b\\c", "C:\\a\\b\\d\\e"), "..\\d\\e");
    }

    #[test]
    fn parse_format_unc_root() {
        // UNC share: the root is the whole `\\server\share`, dir falls back to it for a top-level file.
        let p = parse(W, "\\\\server\\share\\dir\\f.txt");
        assert_eq!(p.root, "\\\\server\\share");
        assert_eq!(p.dir, "\\\\server\\share\\dir");
        assert_eq!(p.base, "f.txt");
        assert_eq!(
            format(W, p.root, &p.dir, p.base, p.name, p.ext),
            "\\\\server\\share\\dir\\f.txt"
        );
    }

    #[test]
    fn native_flavor_is_consistent() {
        // The native flavor's separator matches the compiled-for OS.
        #[cfg(windows)]
        assert_eq!(Flavor::NATIVE.sep(), '\\');
        #[cfg(not(windows))]
        assert_eq!(Flavor::NATIVE.sep(), '/');
    }
}
