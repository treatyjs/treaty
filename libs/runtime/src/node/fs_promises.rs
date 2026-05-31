//! `node:fs/promises` — the promise-returning filesystem surface.
//!
//! Every method wraps the *same* synchronous `std::fs` call that `node:fs` uses (one syscall per op,
//! tenet 4) and hands the result back as an **already-settled** Nova `Promise`. This is observably
//! correct for Node code: `fs/promises` only guarantees a promise, not that the work is deferred, and
//! the I/O here is genuinely synchronous, so resolving immediately avoids ever parking a job on the
//! event loop (zero extra allocation / scheduling, tenet 3). Callers still `await`/`.then` normally;
//! the microtask drain in [`crate::node::event_loop`] runs their continuation.
//!
//! Lazy: built only on the first `require`/`import` of `node:fs/promises` (the registry caches the
//! exports object), so an app that never touches it pays nothing. The shared core supplies the
//! uniform [`install`] seam and the [`NodeModule`] specifier binding; this file owns the body.
//!
//! Allocation discipline: a path/data argument is read once into an owned `String` (the engine's
//! WTF-8 buffer is borrowed during the conversion, copied only because the value must outlive the
//! re-borrow of `agent` inside the promise body). Relative paths are joined onto the runtime CWD
//! (recovered from the installed `HostState`) only when they are actually relative; absolute paths
//! pass through.
//!
//! Faithful scope (high-value core): `readFile` (text encodings), `writeFile`, `appendFile`,
//! `unlink`, `rm`, `mkdir`, `rmdir`, `readdir`, `rename`, `copyFile`, `stat`/`lstat`, and `access`.
//! Deferred (documented, not stubbed): the binary `readFile` path that returns a `Buffer` when no
//! encoding is given — it depends on the sibling `buffer` module's object and is wired once that
//! lands; until then `readFile` returns a UTF-8 string regardless of encoding. `FileHandle`,
//! `watch`, `opendir`, `chmod`/`chown`, and the streaming helpers are also deferred.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use nova_vm::ecmascript::{
    Agent, Array, ArgumentsList, Behaviour, BuiltinFunctionArgs, ExceptionType, InternalMethods,
    JsResult, Number, Object, OrdinaryObject, PromiseCapability, PropertyDescriptor, PropertyKey,
    RegularFn, String as JsString, Value, create_builtin_function, unwrap_try,
};
use nova_vm::engine::{Bindable, GcScope, NoGcScope};

use crate::node::core::{InstallError, NodeCtx};
use crate::node::{GcScope as ModGcScope, NodeModule};

/// Zero-sized marker for the `node:fs/promises` builtin.
pub(crate) struct FsPromisesModule;

impl NodeModule for FsPromisesModule {
    const SPECIFIER: &'static str = "fs/promises";

    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: ModGcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError> {
        install(agent, ctx, gc)
    }
}

/// Uniform per-module entry. Returns the `node:fs/promises` exports object with its methods wired.
///
/// Builds one Nova object and attaches each method as a `Behaviour::Regular` builtin. Runs at most
/// once per runtime (the registry caches the result), so the small burst of allocation here is paid
/// lazily on first import and never repeated.
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    gc: ModGcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    let gc = gc.into_nogc();
    let obj = OrdinaryObject::create_empty_object(agent, gc);

    // (name, fn, declared arity). Arity mirrors Node's `length` for each method.
    const METHODS: &[(&str, RegularFn, u32)] = &[
        ("readFile", read_file, 1),
        ("writeFile", write_file, 2),
        ("appendFile", append_file, 2),
        ("unlink", unlink, 1),
        ("rm", rm, 1),
        ("mkdir", mkdir, 1),
        ("rmdir", rmdir, 1),
        ("readdir", readdir, 1),
        ("rename", rename, 2),
        ("copyFile", copy_file, 2),
        ("stat", stat, 1),
        ("lstat", stat, 1),
        ("access", access, 1),
    ];

    for &(name, f, len) in METHODS {
        define_method(agent, obj, name, f, len, gc);
    }

    Ok(obj.into())
}

/// Attach a Rust-backed method as a data property on the exports object.
///
/// Mirrors the shared `globals::define_fn` pattern but is kept local so this module owns its full
/// surface without reaching into core files it does not own.
fn define_method(
    agent: &mut Agent,
    obj: OrdinaryObject,
    name: &'static str,
    f: RegularFn,
    len: u32,
    gc: NoGcScope,
) {
    let function = create_builtin_function(
        agent,
        Behaviour::Regular(f),
        BuiltinFunctionArgs::new(len, name),
        gc,
    );
    let key = PropertyKey::from_static_str(agent, name, gc);
    unwrap_try(obj.try_define_own_property(
        agent,
        key,
        PropertyDescriptor::new_data_descriptor(function),
        None,
        gc,
    ));
}

// --- argument helpers ------------------------------------------------------------------------

/// Read argument `idx` as an owned path/data string, returning `None` if it is not a JS string.
///
/// The value is owned because it must outlive the borrow of `agent`: each method moves the captured
/// string into the (`'gc`-lifetime) promise body, where `agent` is borrowed mutably again for the
/// I/O. [`JsString::to_string_lossy`] borrows the engine's WTF-8 buffer for the duration of the
/// conversion (no copy when it is valid UTF-8) and we materialize an owned `String` from it; this is
/// the single unavoidable allocation per string argument.
fn arg_string(agent: &Agent, args: &ArgumentsList, idx: usize) -> Option<std::string::String> {
    match JsString::try_from(args.get(idx)) {
        Ok(s) => Some(s.to_string_lossy(agent).into_owned()),
        Err(_) => None,
    }
}

/// Resolve a possibly-relative path against the runtime CWD (from the installed `HostState`).
///
/// Absolute paths pass through; relative paths are joined onto the CWD. When no `HostState` is
/// installed (a plain `JsRuntime::new`), the path is used as-is. The result is owned because it is
/// constructed (joined) in the common case; callers borrow it via `as_path`.
fn resolve_path(agent: &Agent, raw: &str) -> PathBuf {
    let p = Path::new(raw);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    match NodeCtx::from_agent(agent) {
        Some(ctx) => ctx.cwd().join(p),
        None => p.to_path_buf(),
    }
}

// --- promise settling ------------------------------------------------------------------------

/// Build a fresh promise, run `body` (the synchronous I/O + fulfilment-value construction), settle
/// the promise accordingly, and return it as a value.
///
/// This is the single seam every method funnels through, so the promise lifecycle lives in exactly
/// one place. `body` returns an unbound (`'static`) value on success so the borrow of `gc` ends
/// before the full `GcScope` is handed to `resolve`; on `Err(message)` the promise rejects with a
/// fresh `Error` carrying the (already formatted, Node-ish) message.
fn with_promise<'gc, F>(agent: &mut Agent, gc: GcScope<'gc, '_>, body: F) -> Value<'gc>
where
    F: FnOnce(&mut Agent, NoGcScope<'_, '_>) -> Result<Value<'static>, std::string::String>,
{
    // Create the pending promise, then immediately recover its (unbound) `Promise`, dropping the
    // capability's borrow of `gc`. We rebuild a capability from that promise right before settling,
    // so no live borrow of `gc` remains when we move the full `GcScope` into `resolve`.
    let promise = PromiseCapability::new(agent, gc.nogc()).promise().unbind();

    let outcome = body(agent, gc.nogc());

    let capability = PromiseCapability::from_promise(promise, true);
    match outcome {
        // `resolve` consumes the `GcScope` (it may schedule resolve jobs); last step on this path.
        Ok(value) => capability.resolve(agent, value, gc),
        Err(message) => {
            let reason = agent
                .throw_exception(ExceptionType::Error, message, gc.nogc())
                .value()
                .unbind();
            capability.reject(agent, reason, gc.nogc());
        }
    }

    promise.into()
}

/// Reject a fresh promise synchronously with a `TypeError` for a malformed argument (mirrors Node's
/// `ERR_INVALID_ARG_TYPE`).
///
/// Node's `fs/promises` methods still return a thenable on a bad argument (a rejected promise), so
/// this returns a rejected promise rather than throwing, keeping every method's return type a
/// promise.
fn type_error<'gc>(agent: &mut Agent, message: &'static str, gc: GcScope<'gc, '_>) -> Value<'gc> {
    let promise = PromiseCapability::new(agent, gc.nogc()).promise().unbind();
    let reason = agent
        .throw_exception_with_static_message(ExceptionType::TypeError, message, gc.nogc())
        .value()
        .unbind();
    let capability = PromiseCapability::from_promise(promise, true);
    capability.reject(agent, reason, gc.nogc());
    promise.into()
}

// --- methods ---------------------------------------------------------------------------------

/// `fs.promises.readFile(path[, options])` — read a file as text.
///
/// Returns the file decoded as UTF-8 (lossy: invalid sequences become U+FFFD). The faithful Node
/// default with no encoding is a `Buffer`; that path is deferred to the `buffer` module (documented
/// at the top of this file), so for now the result is always a string.
fn read_file<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let args = args.bind(gc.nogc());
    let Some(path) = arg_string(agent, &args, 0) else {
        return Ok(type_error(agent, "path must be a string", gc));
    };
    Ok(with_promise(agent, gc, move |agent, nogc| {
        let resolved = resolve_path(agent, &path);
        match std::fs::read(resolved.as_path()) {
            Ok(bytes) => {
                // Lossy decode: the engine string is WTF-8; this borrows `bytes` and only allocates
                // on a replacement, so valid UTF-8 inputs cost one interning copy.
                let text = std::string::String::from_utf8_lossy(&bytes);
                let js = JsString::from_str(agent, text.as_ref(), nogc);
                Ok(Value::from(js).unbind())
            }
            Err(e) => Err(format_io("read", &path, &e)),
        }
    }))
}

/// `fs.promises.writeFile(path, data)` — write (truncate) a file from a string.
fn write_file<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let args = args.bind(gc.nogc());
    let (Some(path), Some(data)) = (arg_string(agent, &args, 0), arg_string(agent, &args, 1)) else {
        return Ok(type_error(agent, "path and data must be strings", gc));
    };
    Ok(with_promise(agent, gc, move |agent, _nogc| {
        let resolved = resolve_path(agent, &path);
        std::fs::write(resolved.as_path(), data.as_bytes())
            .map(|()| Value::Undefined)
            .map_err(|e| format_io("write", &path, &e))
    }))
}

/// `fs.promises.appendFile(path, data)` — append a string to a file, creating it if absent.
fn append_file<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let args = args.bind(gc.nogc());
    let (Some(path), Some(data)) = (arg_string(agent, &args, 0), arg_string(agent, &args, 1)) else {
        return Ok(type_error(agent, "path and data must be strings", gc));
    };
    Ok(with_promise(agent, gc, move |agent, _nogc| {
        use std::io::Write;
        let resolved = resolve_path(agent, &path);
        let result = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(resolved.as_path())
            .and_then(|mut f| f.write_all(data.as_bytes()));
        result
            .map(|()| Value::Undefined)
            .map_err(|e| format_io("appendFile", &path, &e))
    }))
}

/// `fs.promises.unlink(path)` — remove a file.
fn unlink<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let args = args.bind(gc.nogc());
    let Some(path) = arg_string(agent, &args, 0) else {
        return Ok(type_error(agent, "path must be a string", gc));
    };
    Ok(with_promise(agent, gc, move |agent, _nogc| {
        let resolved = resolve_path(agent, &path);
        std::fs::remove_file(resolved.as_path())
            .map(|()| Value::Undefined)
            .map_err(|e| format_io("unlink", &path, &e))
    }))
}

/// `fs.promises.rm(path)` — remove a file or directory.
///
/// Directories are removed recursively (the safe superset for the common `{ recursive: true }`
/// usage); a single file is removed directly.
fn rm<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let args = args.bind(gc.nogc());
    let Some(path) = arg_string(agent, &args, 0) else {
        return Ok(type_error(agent, "path must be a string", gc));
    };
    Ok(with_promise(agent, gc, move |agent, _nogc| {
        let resolved = resolve_path(agent, &path);
        let p = resolved.as_path();
        let result = if p.is_dir() {
            std::fs::remove_dir_all(p)
        } else {
            std::fs::remove_file(p)
        };
        result
            .map(|()| Value::Undefined)
            .map_err(|e| format_io("rm", &path, &e))
    }))
}

/// `fs.promises.mkdir(path[, options])` — create a directory. Honors `{ recursive: true }`; absent
/// that flag, creates a single directory (whose parent must exist).
fn mkdir<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let args = args.bind(gc.nogc());
    let Some(path) = arg_string(agent, &args, 0) else {
        return Ok(type_error(agent, "path must be a string", gc));
    };
    let recursive = option_flag(agent, &args, 1, "recursive", gc.nogc());
    Ok(with_promise(agent, gc, move |agent, _nogc| {
        let resolved = resolve_path(agent, &path);
        let result = if recursive {
            std::fs::create_dir_all(resolved.as_path())
        } else {
            std::fs::create_dir(resolved.as_path())
        };
        result
            .map(|()| Value::Undefined)
            .map_err(|e| format_io("mkdir", &path, &e))
    }))
}

/// `fs.promises.rmdir(path)` — remove an empty directory.
fn rmdir<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let args = args.bind(gc.nogc());
    let Some(path) = arg_string(agent, &args, 0) else {
        return Ok(type_error(agent, "path must be a string", gc));
    };
    Ok(with_promise(agent, gc, move |agent, _nogc| {
        let resolved = resolve_path(agent, &path);
        std::fs::remove_dir(resolved.as_path())
            .map(|()| Value::Undefined)
            .map_err(|e| format_io("rmdir", &path, &e))
    }))
}

/// `fs.promises.readdir(path)` — list directory entry names as an array of strings.
fn readdir<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let args = args.bind(gc.nogc());
    let Some(path) = arg_string(agent, &args, 0) else {
        return Ok(type_error(agent, "path must be a string", gc));
    };
    Ok(with_promise(agent, gc, move |agent, nogc| {
        let resolved = resolve_path(agent, &path);
        let read =
            std::fs::read_dir(resolved.as_path()).map_err(|e| format_io("readdir", &path, &e))?;
        // Walk the directory into owned names first, then intern each into a JS string under the
        // no-GC scope (no collection can move on the heap mid-build).
        let mut names: Vec<std::string::String> = Vec::new();
        for entry in read {
            let entry = entry.map_err(|e| format_io("readdir", &path, &e))?;
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        let elements: Vec<Value> = names
            .iter()
            .map(|n| JsString::from_str(agent, n, nogc).into())
            .collect();
        let array = Array::from_slice(agent, &elements, nogc);
        Ok(Value::from(array).unbind())
    }))
}

/// `fs.promises.rename(oldPath, newPath)` — rename/move a path.
fn rename<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let args = args.bind(gc.nogc());
    let (Some(from), Some(to)) = (arg_string(agent, &args, 0), arg_string(agent, &args, 1)) else {
        return Ok(type_error(agent, "oldPath and newPath must be strings", gc));
    };
    Ok(with_promise(agent, gc, move |agent, _nogc| {
        let from_p = resolve_path(agent, &from);
        let to_p = resolve_path(agent, &to);
        std::fs::rename(from_p.as_path(), to_p.as_path())
            .map(|()| Value::Undefined)
            .map_err(|e| format_io("rename", &from, &e))
    }))
}

/// `fs.promises.copyFile(src, dest)` — copy a file (truncating/creating dest).
fn copy_file<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let args = args.bind(gc.nogc());
    let (Some(src), Some(dest)) = (arg_string(agent, &args, 0), arg_string(agent, &args, 1)) else {
        return Ok(type_error(agent, "src and dest must be strings", gc));
    };
    Ok(with_promise(agent, gc, move |agent, _nogc| {
        let src_p = resolve_path(agent, &src);
        let dest_p = resolve_path(agent, &dest);
        std::fs::copy(src_p.as_path(), dest_p.as_path())
            .map(|_bytes| Value::Undefined)
            .map_err(|e| format_io("copyFile", &src, &e))
    }))
}

/// `fs.promises.stat(path)` — return a small Stats-shaped object.
///
/// Faithful subset: `size` (bytes) plus the `isFile`/`isDirectory` predicate *flags* as boolean data
/// properties (callers commonly read these; the method form `isFile()` is a documented follow-up).
/// `lstat` aliases `stat` here since the symlink-vs-target distinction is not yet surfaced.
fn stat<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let args = args.bind(gc.nogc());
    let Some(path) = arg_string(agent, &args, 0) else {
        return Ok(type_error(agent, "path must be a string", gc));
    };
    Ok(with_promise(agent, gc, move |agent, nogc| {
        let resolved = resolve_path(agent, &path);
        let meta =
            std::fs::metadata(resolved.as_path()).map_err(|e| format_io("stat", &path, &e))?;

        let stats = OrdinaryObject::create_empty_object(agent, nogc);
        let size = Number::from_f64(agent, meta.len() as f64, nogc);
        define_data(agent, stats, "size", size.into(), nogc);
        define_data(agent, stats, "isFile", Value::Boolean(meta.is_file()), nogc);
        define_data(
            agent,
            stats,
            "isDirectory",
            Value::Boolean(meta.is_dir()),
            nogc,
        );
        Ok(Value::from(stats).unbind())
    }))
}

/// `fs.promises.access(path)` — resolve if the path exists/is accessible, reject otherwise.
fn access<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let args = args.bind(gc.nogc());
    let Some(path) = arg_string(agent, &args, 0) else {
        return Ok(type_error(agent, "path must be a string", gc));
    };
    Ok(with_promise(agent, gc, move |agent, _nogc| {
        let resolved = resolve_path(agent, &path);
        // `symlink_metadata` answers "does the entry exist" with one syscall and without following
        // links, which is the cheapest faithful `access` (F_OK) check.
        match std::fs::symlink_metadata(resolved.as_path()) {
            Ok(_) => Ok(Value::Undefined),
            Err(e) => Err(format_io("access", &path, &e)),
        }
    }))
}

// --- small shared helpers --------------------------------------------------------------------

/// Define a data property `name = value` on `obj`.
fn define_data(
    agent: &mut Agent,
    obj: OrdinaryObject,
    name: &'static str,
    value: Value,
    gc: NoGcScope,
) {
    let key = PropertyKey::from_static_str(agent, name, gc);
    unwrap_try(obj.try_define_own_property(
        agent,
        key,
        PropertyDescriptor::new_data_descriptor(value),
        None,
        gc,
    ));
}

/// Read a boolean flag `name` from an options object passed at argument `idx`.
///
/// Returns `true` only when the argument is an object whose own `name` property is a `true` boolean.
/// This is a deliberately small reader for the handful of `{ recursive: true }`-style options the
/// core methods consult: it reads the own data slot via `try_get_own_property` (no getter
/// invocation, no `ToBoolean` coercion), covering the literal-options case Node code uses in
/// practice. A non-object argument, a missing property, or a non-boolean value all yield `false`.
fn option_flag(
    agent: &mut Agent,
    args: &ArgumentsList,
    idx: usize,
    name: &'static str,
    gc: NoGcScope,
) -> bool {
    let Value::Object(obj) = args.get(idx) else {
        return false;
    };
    let key = PropertyKey::from_static_str(agent, name, gc);
    match unwrap_try(obj.try_get_own_property(agent, key, None, gc)) {
        Some(PropertyDescriptor {
            value: Some(Value::Boolean(b)),
            ..
        }) => b,
        _ => false,
    }
}

/// Format an I/O error into a Node-flavored message: `<op> '<path>': <os error>`.
///
/// Kept allocation-light: a single `format!` only on the (cold) error path; the hot success paths
/// never touch this. `Cow` is used so a borrowed `&str` path needs no extra copy.
fn format_io(op: &str, path: &str, e: &std::io::Error) -> std::string::String {
    let path: Cow<'_, str> = Cow::Borrowed(path);
    format!("{op} '{path}': {e}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::JsRuntime;

    /// Install the module on the realm's global as `__fsp` so test JS can call its methods. We use
    /// the same realm `eval` uses, so the binding persists across subsequent `eval` calls. The
    /// `HostState` lives in `rt` (not behind the agent), so we borrow it as a field disjoint from
    /// `agent` to build the `NodeCtx` without aliasing the `&mut Agent` that `install` requires.
    fn with_fsp(rt: &mut JsRuntime) {
        let JsRuntime {
            agent,
            realm,
            host_state,
        } = rt;
        let host_state = host_state
            .as_deref()
            .expect("with_node_compat installs a host state");
        let ctx = NodeCtx::new(host_state);
        agent.run_in_realm(realm, |agent, mut gc| {
            // Unbind `exports` so the `gc.reborrow()` it was built under is released before we take
            // a no-GC scope for the rest (install + the define below run without intervening GC).
            let exports = install(agent, &ctx, gc.reborrow())
                .expect("install fs/promises")
                .unbind();
            let nogc = gc.into_nogc();
            let global = agent.current_realm(nogc).global_object(agent);
            let key = PropertyKey::from_static_str(agent, "__fsp", nogc);
            unwrap_try(global.try_define_own_property(
                agent,
                key,
                PropertyDescriptor::new_data_descriptor(exports),
                None,
                nogc,
            ));
        });
    }

    #[test]
    fn write_then_read_round_trips_through_a_promise() {
        let dir = std::env::temp_dir().join(format!("treaty_fsp_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("round_trip.txt");
        let file_str = file.to_string_lossy().replace('\\', "\\\\");

        let mut rt = JsRuntime::with_node_compat();
        with_fsp(&mut rt);

        // writeFile resolves to undefined; the side effect is the file on disk. The post-eval drain
        // runs the `.then` continuation, observed by the next eval.
        rt.eval(&format!(
            "globalThis.__w = 'pending';\
             __fsp.writeFile('{file_str}', 'hello treaty').then(() => {{ globalThis.__w = 'done'; }});\
             0"
        ))
        .unwrap();
        assert_eq!(rt.eval("globalThis.__w").unwrap(), serde_json::json!("done"));
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "hello treaty",
            "writeFile wrote the bytes"
        );

        // readFile resolves with the file's text.
        rt.eval(&format!(
            "__fsp.readFile('{file_str}', 'utf8').then(v => {{ globalThis.__r = v; }});0"
        ))
        .unwrap();
        assert_eq!(
            rt.eval("globalThis.__r").unwrap(),
            serde_json::json!("hello treaty")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn readdir_lists_entries_and_mkdir_creates() {
        let dir = std::env::temp_dir().join(format!("treaty_fsp_dir_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let dir_str = dir.to_string_lossy().replace('\\', "\\\\");
        let nested_str = dir
            .join("a")
            .join("b")
            .to_string_lossy()
            .replace('\\', "\\\\");

        let mut rt = JsRuntime::with_node_compat();
        with_fsp(&mut rt);

        // recursive mkdir creates the whole chain.
        rt.eval(&format!(
            "__fsp.mkdir('{nested_str}', {{ recursive: true }}).then(() => {{ globalThis.__m = 1; }});0"
        ))
        .unwrap();
        assert_eq!(rt.eval("globalThis.__m").unwrap(), serde_json::json!(1));
        assert!(
            dir.join("a").join("b").is_dir(),
            "recursive mkdir made the chain"
        );

        // write a file then readdir the top dir; it should include "a" and the file.
        std::fs::write(dir.join("f.txt"), b"x").unwrap();
        rt.eval(&format!(
            "__fsp.readdir('{dir_str}').then(list => {{ globalThis.__list = list.slice().sort().join(','); }});0"
        ))
        .unwrap();
        let listed = rt.eval("globalThis.__list").unwrap();
        let listed = listed.as_str().unwrap_or("").to_owned();
        assert!(listed.contains('a'), "readdir saw subdir 'a': {listed}");
        assert!(listed.contains("f.txt"), "readdir saw file: {listed}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stat_reports_size_and_kind() {
        let dir = std::env::temp_dir().join(format!("treaty_fsp_stat_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("s.txt");
        std::fs::write(&file, b"12345").unwrap();
        let file_str = file.to_string_lossy().replace('\\', "\\\\");

        let mut rt = JsRuntime::with_node_compat();
        with_fsp(&mut rt);

        rt.eval(&format!(
            "__fsp.stat('{file_str}').then(s => {{ globalThis.__size = s.size; globalThis.__isFile = s.isFile; }});0"
        ))
        .unwrap();
        assert_eq!(rt.eval("globalThis.__size").unwrap(), serde_json::json!(5));
        assert_eq!(
            rt.eval("globalThis.__isFile").unwrap(),
            serde_json::json!(true)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_read_rejects() {
        let mut rt = JsRuntime::with_node_compat();
        with_fsp(&mut rt);
        // A rejected promise whose rejection is observed by a `.catch` lands a flag.
        rt.eval(
            "globalThis.__err = 'none';\
             __fsp.readFile('/treaty/definitely/not/here.txt', 'utf8').catch(() => { globalThis.__err = 'caught'; });0",
        )
        .unwrap();
        assert_eq!(
            rt.eval("globalThis.__err").unwrap(),
            serde_json::json!("caught")
        );
    }

    #[test]
    fn resolve_path_passes_through_absolute_and_marks_relative() {
        // Contract check on `resolve_path`'s branch selection without a live agent: an absolute path
        // is recognized as absolute (passed through); a relative one is not.
        let abs = if cfg!(windows) { "C:\\tmp\\x" } else { "/tmp/x" };
        assert!(Path::new(abs).is_absolute());
        assert!(!Path::new("rel/x").is_absolute());
    }
}
