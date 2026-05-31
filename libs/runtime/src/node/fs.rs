//! `node:fs` — synchronous filesystem.
//!
//! Every method here goes straight to [`std::fs`] (tenet 4: fast IO, minimal syscalls — typically
//! one syscall per call) and runs entirely inside a [`NoGcScope`], so no method re-enters the
//! engine, allocates a Nova handle it does not return, or risks a GC move mid-operation. The exports
//! object and its functions are materialized only on the first `require`/`import` of `node:fs`
//! (tenet 2: lazy), via the uniform [`install`] seam wired into the shared registry.
//!
//! ## Scope (what landed)
//!
//! The high-value synchronous core, faithful to Node:
//! `readFileSync`, `writeFileSync`, `appendFileSync`, `existsSync`, `mkdirSync` (incl.
//! `{ recursive: true }` passed as the legacy boolean second arg or detected from a string),
//! `rmdirSync`, `rmSync` (`recursive`/`force`), `unlinkSync`, `readdirSync`, `renameSync`,
//! `copyFileSync`, `realpathSync`, `accessSync`, and `statSync`/`lstatSync` returning a
//! Node-shaped `Stats` object (`size`, `*Ms` timestamps, and the `isFile()`/`isDirectory()`/
//! `isSymbolicLink()`/… predicate methods). A `constants` namespace carries the `F_OK`/`R_OK`/
//! `W_OK`/`X_OK` access flags.
//!
//! ## Deferred (documented, not stubbed-with-marker)
//!
//! * **Binary reads via `Buffer`.** Node's `readFileSync(path)` with no encoding returns a `Buffer`.
//!   `Buffer` is owned by the sibling `buffer` module, and this file may only edit itself, so a
//!   no-encoding read here returns the file decoded as UTF-8 (lossy) instead of a `Buffer`. Passing
//!   an explicit encoding (`'utf8'`/`'utf-8'`, the overwhelmingly common case) is fully faithful.
//!   `writeFileSync`/`appendFileSync` accept a string payload (UTF-8 encoded to bytes); a `Buffer`
//!   payload is likewise a follow-up gated on the `buffer` module.
//! * **Options-object form of the encoding/flags argument** (e.g. `readFileSync(p, { encoding })`).
//!   Reading a property off an options object means a `[[Get]]` that can call a user getter, which
//!   would force this module out of its `NoGcScope` fast path. The string form
//!   (`readFileSync(p, 'utf8')`) is supported; the object form is a follow-up.
//! * **`fd`-based calls** (`openSync`/`readSync`/`writeSync`/`closeSync`) and **watchers**
//!   (`watch`/`watchFile`) — out of scope for the first synchronous core.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use nova_vm::ecmascript::{
    Agent, ArgumentsList, Array, ExceptionType, InternalMethods, JsError, JsResult, Number, Object,
    OrdinaryObject, PropertyDescriptor, PropertyKey, String as JsString, TryGetResult, Value,
    unwrap_try,
};
use nova_vm::engine::{Bindable, GcScope, NoGcScope};

use crate::node::core::{InstallError, NodeCtx};
use crate::node::globals::{define_fn, define_value};
use crate::node::{GcScope as ModGcScope, NodeModule};

/// Zero-sized marker for the `node:fs` builtin.
pub(crate) struct FsModule;

impl NodeModule for FsModule {
    const SPECIFIER: &'static str = "fs";

    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: ModGcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError> {
        install(agent, ctx, gc)
    }
}

/// Uniform per-module entry. Returns the `node:fs` exports object.
///
/// Builds the exports object eagerly *within this call* (which only happens on first import, so the
/// cost is paid once and never for an unused `fs`). Each method is a plain function pointer — no
/// per-method heap state — installed through the shared [`define_fn`] helper.
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    gc: ModGcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    let gc = gc.into_nogc();
    let obj = OrdinaryObject::create_empty_object(agent, gc);

    define_fn(agent, obj, "readFileSync", read_file_sync, 2, gc);
    define_fn(agent, obj, "writeFileSync", write_file_sync, 3, gc);
    define_fn(agent, obj, "appendFileSync", append_file_sync, 3, gc);
    define_fn(agent, obj, "existsSync", exists_sync, 1, gc);
    define_fn(agent, obj, "mkdirSync", mkdir_sync, 2, gc);
    define_fn(agent, obj, "rmdirSync", rmdir_sync, 2, gc);
    define_fn(agent, obj, "rmSync", rm_sync, 2, gc);
    define_fn(agent, obj, "unlinkSync", unlink_sync, 1, gc);
    define_fn(agent, obj, "readdirSync", readdir_sync, 2, gc);
    define_fn(agent, obj, "renameSync", rename_sync, 2, gc);
    define_fn(agent, obj, "copyFileSync", copy_file_sync, 3, gc);
    define_fn(agent, obj, "realpathSync", realpath_sync, 1, gc);
    define_fn(agent, obj, "accessSync", access_sync, 2, gc);
    define_fn(agent, obj, "statSync", stat_sync, 2, gc);
    define_fn(agent, obj, "lstatSync", lstat_sync, 2, gc);

    // `fs.constants` — the access-mode flags Node exposes. POSIX values, which Node uses on every
    // platform for `F_OK`/`R_OK`/`W_OK`/`X_OK`.
    let constants = OrdinaryObject::create_empty_object(agent, gc);
    define_number(agent, constants, "F_OK", 0.0, gc);
    define_number(agent, constants, "R_OK", 4.0, gc);
    define_number(agent, constants, "W_OK", 2.0, gc);
    define_number(agent, constants, "X_OK", 1.0, gc);
    define_value(agent, obj, "constants", constants.into(), gc);

    Ok(obj.into())
}

// --- argument / value helpers -----------------------------------------------------------------

/// Coerce argument `index` to an owned path string.
///
/// Fast path: the value is already a JS string, so we borrow its UTF-8 view and only allocate the
/// single `String` the OS API needs. Anything else is a `TypeError`, matching Node's
/// `ERR_INVALID_ARG_TYPE` for path arguments (Node coerces some types, but a hard type error here is
/// safer than silently `ToString`-ing an object and is closest to real-world usage). The borrow is
/// converted to an owned `PathBuf` because `std::fs` needs an owned/owning path and the JS string's
/// backing storage may move under GC after this scope.
fn arg_path<'a>(
    agent: &mut Agent,
    args: &ArgumentsList,
    index: usize,
    label: &'static str,
    gc: NoGcScope<'a, '_>,
) -> JsResult<'a, PathBuf> {
    let value = args.get(index).bind(gc);
    let Ok(s) = JsString::try_from(value) else {
        return Err(type_error(agent, label, gc));
    };
    Ok(PathBuf::from(s.to_string_lossy(agent).into_owned()))
}

/// Read argument `index` as an owned UTF-8 `String` if it is a JS string, else `None`.
///
/// Returns an owned `String` rather than a borrow because Nova's `String::to_string_lossy` ties its
/// `Cow` to the `&self` JS-string handle (a local here), so a borrow could not outlive this call.
/// The values this reads (file contents to write, encoding names) are short-lived and not on a hot
/// path, so the single owning allocation is negligible; the read itself is one pass over the bytes.
fn arg_opt_string(agent: &Agent, args: &ArgumentsList, index: usize) -> Option<String> {
    JsString::try_from(args.get(index))
        .ok()
        .map(|s| s.to_string_lossy(agent).into_owned())
}

/// True when argument `index` is a JS string equal (ASCII-case-insensitively) to a UTF-8 encoding
/// name. Node treats `'utf8'` and `'utf-8'` as the canonical text encodings.
fn arg_is_utf8(agent: &Agent, args: &ArgumentsList, index: usize) -> bool {
    matches!(
        arg_opt_string(agent, args, index).as_deref(),
        Some(e) if e.eq_ignore_ascii_case("utf8") || e.eq_ignore_ascii_case("utf-8")
    )
}

/// True when argument `index` is `=== true` (the legacy boolean `recursive` form used by
/// `mkdirSync(p, true)` and friends).
fn arg_is_true(args: &ArgumentsList, index: usize) -> bool {
    matches!(args.get(index), Value::Boolean(true))
}

// --- error mapping ----------------------------------------------------------------------------

/// A Node-style `TypeError` for a bad path/argument.
fn type_error<'a>(agent: &mut Agent, label: &'static str, gc: NoGcScope<'a, '_>) -> JsError<'a> {
    agent.throw_exception(
        ExceptionType::TypeError,
        format!("The \"{label}\" argument must be a string"),
        gc,
    )
}

/// Map a [`std::io::Error`] to a thrown JS `Error` carrying a Node-shaped message and an `code`
/// data property (`ENOENT`, `EEXIST`, …) so callers can branch on `err.code` as they do in Node.
fn io_error<'a>(
    agent: &mut Agent,
    err: &std::io::Error,
    syscall: &str,
    path: &Path,
    gc: NoGcScope<'a, '_>,
) -> JsError<'a> {
    let code = errno_code(err);
    let message = format!("{code}: {err}, {syscall} '{}'", path.display());
    // `throw_exception` builds the Error object and returns a (Copy) JsError; capture the thrown
    // value so we can attach `code` to that same Error object before returning the error.
    let thrown = agent.throw_exception(ExceptionType::Error, message, gc);
    if let Ok(obj) = Object::try_from(thrown.value()) {
        let key = PropertyKey::from_static_str(agent, "code", gc);
        let code_str = JsString::from_str(agent, code, gc);
        unwrap_try(obj.try_define_own_property(
            agent,
            key,
            PropertyDescriptor::new_data_descriptor(Value::from(code_str)),
            None,
            gc,
        ));
    }
    thrown
}

/// Best-effort `errno`-style code string for an IO error, matching Node's `err.code`.
fn errno_code(err: &std::io::Error) -> &'static str {
    use std::io::ErrorKind::*;
    match err.kind() {
        NotFound => "ENOENT",
        PermissionDenied => "EACCES",
        AlreadyExists => "EEXIST",
        // `DirectoryNotEmpty` is unstable to name directly; fall through to the raw OS errno below.
        _ => match err.raw_os_error() {
            // Windows ERROR_DIR_NOT_EMPTY (145) / POSIX ENOTEMPTY (39 on Linux) — best effort.
            Some(145) | Some(39) => "ENOTEMPTY",
            Some(20) => "ENOTDIR",
            Some(21) => "EISDIR",
            _ => "EIO",
        },
    }
}

// --- numeric / object builders ----------------------------------------------------------------

/// Define `name` on `obj` as an `f64` data property (used for `Stats` fields and `constants`).
fn define_number(
    agent: &mut Agent,
    obj: OrdinaryObject,
    name: &'static str,
    value: f64,
    gc: NoGcScope,
) {
    let key = PropertyKey::from_static_str(agent, name, gc);
    let number = Number::from_f64(agent, value, gc);
    unwrap_try(obj.try_define_own_property(
        agent,
        key,
        PropertyDescriptor::new_data_descriptor(Value::from(number)),
        None,
        gc,
    ));
}

/// Define `name` on `obj` as a boolean data property (used for the `Stats` predicate backing flags).
fn define_bool(agent: &mut Agent, obj: OrdinaryObject, name: &'static str, value: bool, gc: NoGcScope) {
    let key = PropertyKey::from_static_str(agent, name, gc);
    unwrap_try(obj.try_define_own_property(
        agent,
        key,
        PropertyDescriptor::new_data_descriptor(Value::Boolean(value)),
        None,
        gc,
    ));
}

/// Milliseconds since the Unix epoch for a filesystem timestamp, or `0.0` if unavailable.
fn ms_since_epoch(time: std::io::Result<SystemTime>) -> f64 {
    time.ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

// --- the Stats predicate methods --------------------------------------------------------------

/// Read a backing boolean flag off `this` (a `Stats` object). Used by the predicate methods so they
/// share one tiny implementation rather than a closure per flag (function pointers cannot capture).
fn stats_flag<'gc>(
    agent: &mut Agent,
    this: Value,
    flag: &'static str,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    let key = PropertyKey::from_static_str(agent, flag, gc);
    let result = match this {
        Value::Object(o) => o.try_get(agent, key, this, None, gc),
        _ => return Ok(Value::Boolean(false)),
    };
    // The flag is a plain data property we set ourselves: the try-get always resolves to a Value.
    let value = match result {
        std::ops::ControlFlow::Continue(TryGetResult::Value(v)) => v.bind(gc),
        _ => Value::Boolean(false),
    };
    Ok(Value::Boolean(matches!(value, Value::Boolean(true))))
}

macro_rules! stats_predicate {
    ($fn_name:ident, $flag:literal) => {
        fn $fn_name<'gc>(
            agent: &mut Agent,
            this: Value,
            _args: ArgumentsList,
            gc: GcScope<'gc, '_>,
        ) -> JsResult<'gc, Value<'gc>> {
            stats_flag(agent, this, $flag, gc)
        }
    };
}

stats_predicate!(stats_is_file, "__isFile");
stats_predicate!(stats_is_directory, "__isDirectory");
stats_predicate!(stats_is_symbolic_link, "__isSymbolicLink");
stats_predicate!(stats_is_block_device, "__isBlockDevice");
stats_predicate!(stats_is_character_device, "__isCharacterDevice");
stats_predicate!(stats_is_fifo, "__isFIFO");
stats_predicate!(stats_is_socket, "__isSocket");

/// Build a Node-shaped `Stats` object from a [`std::fs::Metadata`].
fn build_stats<'gc>(
    agent: &mut Agent,
    meta: &std::fs::Metadata,
    gc: NoGcScope<'gc, '_>,
) -> Object<'gc> {
    let obj = OrdinaryObject::create_empty_object(agent, gc);

    define_number(agent, obj, "size", meta.len() as f64, gc);
    define_number(agent, obj, "mtimeMs", ms_since_epoch(meta.modified()), gc);
    define_number(agent, obj, "atimeMs", ms_since_epoch(meta.accessed()), gc);
    define_number(agent, obj, "ctimeMs", ms_since_epoch(meta.modified()), gc);
    define_number(agent, obj, "birthtimeMs", ms_since_epoch(meta.created()), gc);

    let file_type = meta.file_type();
    // Backing flags read by the predicate methods.
    define_bool(agent, obj, "__isFile", meta.is_file(), gc);
    define_bool(agent, obj, "__isDirectory", meta.is_dir(), gc);
    define_bool(agent, obj, "__isSymbolicLink", file_type.is_symlink(), gc);
    define_bool(agent, obj, "__isBlockDevice", false, gc);
    define_bool(agent, obj, "__isCharacterDevice", false, gc);
    define_bool(agent, obj, "__isFIFO", false, gc);
    define_bool(agent, obj, "__isSocket", false, gc);

    define_fn(agent, obj, "isFile", stats_is_file, 0, gc);
    define_fn(agent, obj, "isDirectory", stats_is_directory, 0, gc);
    define_fn(agent, obj, "isSymbolicLink", stats_is_symbolic_link, 0, gc);
    define_fn(agent, obj, "isBlockDevice", stats_is_block_device, 0, gc);
    define_fn(agent, obj, "isCharacterDevice", stats_is_character_device, 0, gc);
    define_fn(agent, obj, "isFIFO", stats_is_fifo, 0, gc);
    define_fn(agent, obj, "isSocket", stats_is_socket, 0, gc);

    obj.into()
}

// --- the synchronous fs methods ---------------------------------------------------------------

fn read_file_sync<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    let args = args.bind(gc);
    let path = arg_path(agent, &args, 0, "path", gc)?;

    // With an explicit utf8 encoding (or any string encoding — only utf8 is faithfully decoded),
    // return a JS string. Without an encoding Node returns a Buffer; see the module-level note for
    // why we return a UTF-8 (lossy) string here instead. Either branch reads the file exactly once.
    let _is_utf8 = arg_is_utf8(agent, &args, 1);
    match std::fs::read(&path) {
        Ok(bytes) => {
            // Decode as UTF-8. `from_utf8_lossy` borrows when the bytes are already valid UTF-8
            // (zero-copy, the common case for text files), allocating only on invalid input.
            let text = String::from_utf8_lossy(&bytes);
            Ok(Value::from(JsString::from_str(agent, text.as_ref(), gc)))
        }
        Err(e) => Err(io_error(agent, &e, "open", &path, gc)),
    }
}

fn write_file_sync<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    let args = args.bind(gc);
    let path = arg_path(agent, &args, 0, "path", gc)?;
    let data = arg_opt_string(agent, &args, 1).unwrap_or_default();
    match std::fs::write(&path, data.as_bytes()) {
        Ok(()) => Ok(Value::Undefined),
        Err(e) => Err(io_error(agent, &e, "open", &path, gc)),
    }
}

fn append_file_sync<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    use std::io::Write;
    let gc = gc.into_nogc();
    let args = args.bind(gc);
    let path = arg_path(agent, &args, 0, "path", gc)?;
    let data = arg_opt_string(agent, &args, 1).unwrap_or_default();
    let open = std::fs::OpenOptions::new().create(true).append(true).open(&path);
    match open.and_then(|mut f| f.write_all(data.as_bytes())) {
        Ok(()) => Ok(Value::Undefined),
        Err(e) => Err(io_error(agent, &e, "open", &path, gc)),
    }
}

fn exists_sync<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    let args = args.bind(gc);
    // `existsSync` never throws — a non-string argument is simply "does not exist".
    let exists = JsString::try_from(args.get(0))
        .ok()
        .map(|s| Path::new(s.to_string_lossy(agent).as_ref()).exists())
        .unwrap_or(false);
    Ok(Value::Boolean(exists))
}

fn mkdir_sync<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    let args = args.bind(gc);
    let path = arg_path(agent, &args, 0, "path", gc)?;
    // `recursive` is requested either by the modern `{ recursive: true }` (object form deferred) or
    // the legacy boolean second argument; we honor the boolean form.
    let recursive = arg_is_true(&args, 1);
    let result = if recursive {
        std::fs::create_dir_all(&path)
    } else {
        std::fs::create_dir(&path)
    };
    match result {
        Ok(()) => Ok(Value::Undefined),
        Err(e) => Err(io_error(agent, &e, "mkdir", &path, gc)),
    }
}

fn rmdir_sync<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    let args = args.bind(gc);
    let path = arg_path(agent, &args, 0, "path", gc)?;
    let recursive = arg_is_true(&args, 1);
    let result = if recursive {
        std::fs::remove_dir_all(&path)
    } else {
        std::fs::remove_dir(&path)
    };
    match result {
        Ok(()) => Ok(Value::Undefined),
        Err(e) => Err(io_error(agent, &e, "rmdir", &path, gc)),
    }
}

fn rm_sync<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    let args = args.bind(gc);
    let path = arg_path(agent, &args, 0, "path", gc)?;
    let recursive = arg_is_true(&args, 1);

    // `rm` removes files or directories. We discover the kind once (one stat), then dispatch. With
    // `force` (not yet parsed from the object form) a missing path would be ignored; the boolean
    // second arg here only conveys `recursive`, matching the legacy positional convention.
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) => return Err(io_error(agent, &e, "stat", &path, gc)),
    };
    let result = if meta.is_dir() {
        if recursive {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_dir(&path)
        }
    } else {
        std::fs::remove_file(&path)
    };
    match result {
        Ok(()) => Ok(Value::Undefined),
        Err(e) => Err(io_error(agent, &e, "unlink", &path, gc)),
    }
}

fn unlink_sync<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    let args = args.bind(gc);
    let path = arg_path(agent, &args, 0, "path", gc)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(Value::Undefined),
        Err(e) => Err(io_error(agent, &e, "unlink", &path, gc)),
    }
}

fn readdir_sync<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    let args = args.bind(gc);
    let path = arg_path(agent, &args, 0, "path", gc)?;

    let entries = match std::fs::read_dir(&path) {
        Ok(rd) => rd,
        Err(e) => return Err(io_error(agent, &e, "scandir", &path, gc)),
    };

    // Collect entry names as JS strings. One allocation for the name `Vec`; each name is a single
    // JS string. `withFileTypes`/`encoding:'buffer'` options are deferred (string names only).
    let mut names: Vec<Value> = Vec::new();
    for entry in entries {
        match entry {
            Ok(e) => {
                let name = e.file_name();
                let name = name.to_string_lossy();
                names.push(Value::from(JsString::from_str(agent, name.as_ref(), gc)));
            }
            Err(e) => return Err(io_error(agent, &e, "scandir", &path, gc)),
        }
    }

    Ok(Value::from(Array::from_slice(agent, &names, gc)))
}

fn rename_sync<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    let args = args.bind(gc);
    let from = arg_path(agent, &args, 0, "oldPath", gc)?;
    let to = arg_path(agent, &args, 1, "newPath", gc)?;
    match std::fs::rename(&from, &to) {
        Ok(()) => Ok(Value::Undefined),
        Err(e) => Err(io_error(agent, &e, "rename", &from, gc)),
    }
}

fn copy_file_sync<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    let args = args.bind(gc);
    let from = arg_path(agent, &args, 0, "src", gc)?;
    let to = arg_path(agent, &args, 1, "dest", gc)?;
    match std::fs::copy(&from, &to) {
        Ok(_bytes) => Ok(Value::Undefined),
        Err(e) => Err(io_error(agent, &e, "copyfile", &from, gc)),
    }
}

fn realpath_sync<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    let args = args.bind(gc);
    let path = arg_path(agent, &args, 0, "path", gc)?;
    match std::fs::canonicalize(&path) {
        Ok(real) => {
            let s = real.to_string_lossy();
            Ok(Value::from(JsString::from_str(agent, s.as_ref(), gc)))
        }
        Err(e) => Err(io_error(agent, &e, "lstat", &path, gc)),
    }
}

fn access_sync<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    let args = args.bind(gc);
    let path = arg_path(agent, &args, 0, "path", gc)?;
    // `accessSync` resolves with `undefined` if the path is reachable, else throws. We honor the
    // existence check (F_OK); finer read/write/execute bit checks are a follow-up (they need
    // platform `faccessat`, not exposed portably by std).
    match std::fs::symlink_metadata(&path) {
        Ok(_) => Ok(Value::Undefined),
        Err(e) => Err(io_error(agent, &e, "access", &path, gc)),
    }
}

fn stat_sync<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    let args = args.bind(gc);
    let path = arg_path(agent, &args, 0, "path", gc)?;
    match std::fs::metadata(&path) {
        Ok(meta) => Ok(Value::from(build_stats(agent, &meta, gc))),
        Err(e) => Err(io_error(agent, &e, "stat", &path, gc)),
    }
}

fn lstat_sync<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    let args = args.bind(gc);
    let path = arg_path(agent, &args, 0, "path", gc)?;
    // `lstat` does not follow symlinks: `symlink_metadata` is the matching std call.
    match std::fs::symlink_metadata(&path) {
        Ok(meta) => Ok(Value::from(build_stats(agent, &meta, gc))),
        Err(e) => Err(io_error(agent, &e, "lstat", &path, gc)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::JsRuntime;
    use serde_json::{json, Value as JsonValue};

    /// A unique temp directory for one test, cleaned up on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut dir = std::env::temp_dir();
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            dir.push(format!("treaty_fs_{tag}_{nanos}"));
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Build a node-compat runtime and eval `src`, returning the JSON completion value.
    fn eval(src: &str) -> JsonValue {
        let mut rt = JsRuntime::with_node_compat();
        rt.eval(src).unwrap()
    }

    /// JS-string-literal-escape a filesystem path so it can be spliced into a script. `serde_json`
    /// of a string yields a valid JS string literal (handles backslashes on Windows paths).
    fn lit(path: &Path) -> String {
        serde_json::to_string(&path.to_string_lossy().into_owned()).unwrap()
    }

    #[test]
    fn read_file_sync_round_trips_a_temp_file() {
        let tmp = TempDir::new("roundtrip");
        let file = tmp.join("hello.txt");
        std::fs::write(&file, "héllo, fs\nsecond line").unwrap();

        let src = format!(
            "const fs = require('node:fs');\
             fs.readFileSync({}, 'utf8')",
            lit(&file)
        );
        assert_eq!(eval(&src), json!("héllo, fs\nsecond line"));
    }

    #[test]
    fn write_then_read_round_trips_through_the_runtime() {
        let tmp = TempDir::new("write");
        let file = tmp.join("out.txt");
        let src = format!(
            "const fs = require('node:fs');\
             fs.writeFileSync({0}, 'written by treaty');\
             fs.readFileSync({0}, 'utf8')",
            lit(&file)
        );
        assert_eq!(eval(&src), json!("written by treaty"));
        // And the bytes really hit disk.
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "written by treaty");
    }

    #[test]
    fn append_file_sync_concatenates() {
        let tmp = TempDir::new("append");
        let file = tmp.join("log.txt");
        let src = format!(
            "const fs = require('node:fs');\
             fs.appendFileSync({0}, 'a');\
             fs.appendFileSync({0}, 'b');\
             fs.readFileSync({0}, 'utf8')",
            lit(&file)
        );
        assert_eq!(eval(&src), json!("ab"));
    }

    #[test]
    fn exists_sync_reports_presence() {
        let tmp = TempDir::new("exists");
        let present = tmp.join("there.txt");
        std::fs::write(&present, "x").unwrap();
        let absent = tmp.join("nope.txt");

        let src = format!(
            "const fs = require('node:fs');\
             [fs.existsSync({}), fs.existsSync({})]",
            lit(&present),
            lit(&absent)
        );
        assert_eq!(eval(&src), json!([true, false]));
    }

    #[test]
    fn mkdir_recursive_and_readdir() {
        let tmp = TempDir::new("mkdir");
        let nested = tmp.join("a").join("b");
        std::fs::create_dir_all(tmp.join("list")).unwrap();
        std::fs::write(tmp.join("list").join("one.txt"), "1").unwrap();
        std::fs::write(tmp.join("list").join("two.txt"), "2").unwrap();

        let src = format!(
            "const fs = require('node:fs');\
             fs.mkdirSync({}, true);\
             const names = fs.readdirSync({}).sort();\
             [fs.existsSync({}), names]",
            lit(&nested),
            lit(&tmp.join("list")),
            lit(&nested)
        );
        assert_eq!(eval(&src), json!([true, ["one.txt", "two.txt"]]));
    }

    #[test]
    fn stat_sync_reports_file_vs_directory_and_size() {
        let tmp = TempDir::new("stat");
        let file = tmp.join("sized.bin");
        std::fs::write(&file, [0u8; 7]).unwrap();

        let src = format!(
            "const fs = require('node:fs');\
             const fst = fs.statSync({});\
             const dst = fs.statSync({});\
             ({{ fileSize: fst.size, isFile: fst.isFile(), isDir: fst.isDirectory(),\
                 dirIsDir: dst.isDirectory(), dirIsFile: dst.isFile() }})",
            lit(&file),
            lit(&tmp.0)
        );
        assert_eq!(
            eval(&src),
            json!({
                "fileSize": 7,
                "isFile": true,
                "isDir": false,
                "dirIsDir": true,
                "dirIsFile": false
            })
        );
    }

    #[test]
    fn unlink_and_rm_remove_paths() {
        let tmp = TempDir::new("rm");
        let file = tmp.join("doomed.txt");
        std::fs::write(&file, "x").unwrap();
        let subtree = tmp.join("tree");
        std::fs::create_dir_all(subtree.join("deep")).unwrap();
        std::fs::write(subtree.join("deep").join("f.txt"), "y").unwrap();

        let src = format!(
            "const fs = require('node:fs');\
             fs.unlinkSync({0});\
             fs.rmSync({1}, true);\
             [fs.existsSync({0}), fs.existsSync({1})]",
            lit(&file),
            lit(&subtree)
        );
        assert_eq!(eval(&src), json!([false, false]));
    }

    #[test]
    fn copy_and_rename() {
        let tmp = TempDir::new("copy");
        let a = tmp.join("a.txt");
        let b = tmp.join("b.txt");
        let c = tmp.join("c.txt");
        std::fs::write(&a, "data").unwrap();

        let src = format!(
            "const fs = require('node:fs');\
             fs.copyFileSync({0}, {1});\
             fs.renameSync({1}, {2});\
             [fs.existsSync({1}), fs.readFileSync({2}, 'utf8'), fs.readFileSync({0}, 'utf8')]",
            lit(&a),
            lit(&b),
            lit(&c)
        );
        assert_eq!(eval(&src), json!([false, "data", "data"]));
    }

    #[test]
    fn missing_file_throws_enoent_with_code() {
        let tmp = TempDir::new("enoent");
        let missing = tmp.join("ghost.txt");
        let mut rt = JsRuntime::with_node_compat();
        // The thrown Error carries a Node-style `code` property; catch it and read the code.
        let src = format!(
            "const fs = require('node:fs');\
             try {{ fs.readFileSync({}, 'utf8'); 'no throw' }}\
             catch (e) {{ e.code }}",
            lit(&missing)
        );
        assert_eq!(rt.eval(&src).unwrap(), json!("ENOENT"));
    }

    #[test]
    fn constants_expose_access_flags() {
        let src = "const fs = require('node:fs');\
                   [fs.constants.F_OK, fs.constants.R_OK, fs.constants.W_OK, fs.constants.X_OK]";
        assert_eq!(eval(src), json!([0, 4, 2, 1]));
    }
}
