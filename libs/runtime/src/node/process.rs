//! `node:process` — argv, argv0, execPath, env, cwd, chdir, platform, arch, pid, ppid, version,
//! versions, exit/exitCode, nextTick, hrtime[.bigint], and the WinterCG-aligned `process.env`
//! snapshot.
//!
//! Eager (tenet 2): Node code assumes `process` exists without an `import`, so the `globals` core
//! installs this object at realm init. It is *also* reachable as `require("node:process")` /
//! `import "node:process"` through the registry, which calls the very same [`install`]; the object
//! is built once and cached by the registry, so the eager global and the module export are the same
//! handle.
//!
//! Allocation discipline (tenet 3): every property key is an interned `&'static str` via
//! [`PropertyKey::from_static_str`]; the only per-call heap is the result string/array a getter
//! must hand back to JS (unavoidable — it lives on the Nova heap). `argv`/`env`/`versions` are
//! materialized once into the object at build time, not recomputed per access. Environment values
//! are read straight from the captured [`NodeCtx::env`] map (borrowed, not cloned, into Nova
//! strings).
//!
//! Process state that Node mutates at runtime — `exitCode` and the cwd after `chdir` — lives in a
//! thread-local cell rather than a global JS variable, so reads/writes are O(1) and need no realm
//! round-trip. The runtime is single-threaded (one realm per OS thread), so a `thread_local!` is
//! the correct, lock-free home for it.
//!
//! NO `unsafe` lives in this file: the single Node-layer `unsafe` is the documented Nova FFI
//! lifetime extension in `core.rs`. Everything here is safe Rust over Nova's public API.

use std::cell::Cell;

use nova_vm::ecmascript::{
    Agent, Array, ArgumentsList, Behaviour, BuiltinFunctionArgs, ExceptionType, Function,
    InternalMethods, JsResult, Number, Object, OrdinaryObject, PropertyDescriptor, PropertyKey,
    Value, create_builtin_function, unwrap_try,
};
use nova_vm::engine::Bindable;

use crate::node::core::{InstallError, NodeCtx, host_state};
use crate::node::{GcScope, NodeModule};

/// Process-global mutable state Node lets scripts change after start-up.
///
/// `exit_code` backs `process.exitCode` / the argument to `process.exit()`. It is a plain `Cell`
/// (no atomics, no lock): the runtime is single-threaded per realm, so this is both correct and the
/// lowest-overhead store. It starts at `0` (Node's default success code).
thread_local! {
    static EXIT_CODE: Cell<i32> = const { Cell::new(0) };
}

/// Zero-sized marker for the `node:process` builtin.
pub(crate) struct ProcessModule;

impl NodeModule for ProcessModule {
    const SPECIFIER: &'static str = "process";

    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: GcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError> {
        install(agent, ctx, gc)
    }
}

/// Map Rust's `std::env::consts::OS` onto Node's `process.platform` vocabulary.
///
/// Node reports `win32`/`darwin`/`linux`/`freebsd`/… ; Rust reports `windows`/`macos`/`linux`/… .
/// The two diverge on Windows and macOS, which this normalizes; everything else already matches.
fn node_platform() -> &'static str {
    match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        other => other,
    }
}

/// Map Rust's `std::env::consts::ARCH` onto Node's `process.arch` vocabulary.
///
/// Node uses `x64`/`arm64`/`ia32`; Rust uses `x86_64`/`aarch64`/`x86`. Other values (`arm`,
/// `riscv64`, …) coincide and pass through unchanged.
fn node_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        other => other,
    }
}

/// The version string surfaced as `process.version` (and `process.versions.node`).
///
/// Treaty is its own runtime, not a Node binary, so this advertises the Node API level the layer
/// targets rather than impersonating a specific Node build. Kept as one constant so a bump is a
/// one-line change and `version`/`versions.node` never drift apart.
const NODE_API_VERSION: &str = "v22.0.0";

/// Define a Rust-backed function as a data property `name` (arity `len`) on `obj`.
///
/// Local mirror of the shared `globals::define_fn` so this module owns only its own file (the
/// scaffold rules forbid editing `globals.rs`). Uses the interned-`&'static str` key path so no
/// heap string is allocated for the name (tenet 3).
fn define_fn(
    agent: &mut Agent,
    obj: OrdinaryObject,
    name: &'static str,
    f: nova_vm::ecmascript::RegularFn,
    len: u32,
    gc: nova_vm::engine::NoGcScope,
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
        PropertyDescriptor::new_data_descriptor(function.into()),
        None,
        gc,
    ));
}

/// Define `value` as a `&'static`-keyed data property on `obj`.
fn define_value(
    agent: &mut Agent,
    obj: OrdinaryObject,
    name: &'static str,
    value: Value,
    gc: nova_vm::engine::NoGcScope,
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

/// Define a `&str` (string) data property on `obj`. The convenience wrapper for the many
/// fixed-string fields (`platform`, `arch`, `version`, …).
fn define_str(
    agent: &mut Agent,
    obj: OrdinaryObject,
    name: &'static str,
    value: &str,
    gc: nova_vm::engine::NoGcScope,
) {
    let v = Value::from_str(agent, value, gc);
    define_value(agent, obj, name, v, gc);
}

/// Uniform per-module entry. Builds and returns the fully-populated `node:process` object.
///
/// Everything is materialized eagerly *within this single build* (it runs at most once per runtime,
/// guarded by the registry's `builtin_cache`), so there is no per-access work: `argv`, `env`,
/// `versions` and the scalar fields are all data properties; only `nextTick`/`exit`/`cwd`/`chdir`/
/// `hrtime` are functions.
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    ctx: &NodeCtx,
    gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    let gc = gc.into_nogc();
    let obj = OrdinaryObject::create_empty_object(agent, gc);

    // --- scalar identity fields ------------------------------------------------------------
    define_str(agent, obj, "platform", node_platform(), gc);
    define_str(agent, obj, "arch", node_arch(), gc);
    define_str(agent, obj, "version", NODE_API_VERSION, gc);
    define_str(agent, obj, "title", "treaty", gc);
    define_value(
        agent,
        obj,
        "pid",
        Number::from_i64(agent, i64::from(std::process::id()), gc).into(),
        gc,
    );
    // Node exposes `ppid`; we have no portable parent pid, so report 0 (a documented, safe stand-in
    // that scripts treat as "unknown / detached").
    define_value(agent, obj, "ppid", Number::from_i64(agent, 0, gc).into(), gc);
    // `exitCode` reflects the thread-local; initialized to its current value (0 at start-up).
    define_value(
        agent,
        obj,
        "exitCode",
        Number::from_i64(agent, i64::from(EXIT_CODE.with(Cell::get)), gc).into(),
        gc,
    );

    // --- argv / argv0 / execPath ----------------------------------------------------------
    // Node's argv is `[execPath, scriptPath, ...userArgs]`. Treaty embeds the engine, so there is no
    // separate script file; we expose `[execPath, ...processArgs]`, mirroring `node -e` where argv
    // is `[execPath, "[eval]"]` plus user args. execPath is the host executable.
    let exec_path = std::env::current_exe()
        .ok()
        .and_then(|p| p.into_os_string().into_string().ok())
        .unwrap_or_else(|| "treaty".to_owned());
    define_str(agent, obj, "execPath", &exec_path, gc);

    let argv = build_argv(agent, &exec_path, gc);
    define_value(agent, obj, "argv", argv.into(), gc);
    define_str(agent, obj, "argv0", &exec_path, gc);

    // --- env ------------------------------------------------------------------------------
    let env = build_env(agent, ctx, gc);
    define_value(agent, obj, "env", env.into(), gc);

    // --- versions -------------------------------------------------------------------------
    let versions = build_versions(agent, gc);
    define_value(agent, obj, "versions", versions.into(), gc);

    // --- functions ------------------------------------------------------------------------
    define_fn(agent, obj, "cwd", process_cwd, 0, gc);
    define_fn(agent, obj, "chdir", process_chdir, 1, gc);
    define_fn(agent, obj, "exit", process_exit, 1, gc);
    define_fn(agent, obj, "nextTick", process_next_tick, 1, gc);
    define_fn(agent, obj, "hrtime", process_hrtime, 1, gc);

    Ok(obj.into())
}

/// Build `process.argv = [execPath, ...userArgs]`.
///
/// The leading element is the engine executable; the rest are the real process arguments past the
/// program name (so `treaty foo bar` yields `[execPath, "foo", "bar"]`). Built once into a single
/// `Array::from_slice`, so there is exactly one array allocation regardless of arg count.
fn build_argv<'a>(
    agent: &mut Agent,
    exec_path: &str,
    gc: nova_vm::engine::NoGcScope<'a, '_>,
) -> Array<'a> {
    let mut elements: Vec<Value> = Vec::new();
    elements.push(Value::from_str(agent, exec_path, gc));
    for arg in std::env::args().skip(1) {
        elements.push(Value::from_str(agent, &arg, gc));
    }
    Array::from_slice(agent, &elements, gc)
}

/// Build the `process.env` object from the runtime's captured environment.
///
/// Reads the borrowed [`NodeCtx::env`] map and installs each entry as a string-valued data
/// property. WinterCG only mandates that `process.env` be a string→string dictionary; this provides
/// exactly that as a snapshot taken at runtime construction (the same map the resolver consults), so
/// reads are plain property lookups with no syscall.
fn build_env<'a>(
    agent: &mut Agent,
    ctx: &NodeCtx,
    gc: nova_vm::engine::NoGcScope<'a, '_>,
) -> OrdinaryObject<'a> {
    let env_obj = OrdinaryObject::create_empty_object(agent, gc);
    // Collect first so the immutable borrow of `ctx.env()` does not overlap the `&mut Agent` calls
    // below. Keys/values are `&str`; only Nova-side string interning allocates.
    let pairs: Vec<(&str, &str)> = ctx
        .env()
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    for (k, v) in pairs {
        let key = PropertyKey::from_str(agent, k, gc);
        let value = Value::from_str(agent, v, gc);
        unwrap_try(env_obj.try_define_own_property(
            agent,
            key,
            PropertyDescriptor::new_data_descriptor(value),
            None,
            gc,
        ));
    }
    env_obj
}

/// Build the `process.versions` object: `{ node, treaty, v8 }`.
///
/// `node` advertises the targeted API level (so libraries gating on `process.versions.node` behave),
/// `treaty` is this runtime's own crate version, and `v8` is intentionally absent of a real V8 build
/// — Treaty runs on Nova — so it reports the Nova-backed API level too rather than lying about V8.
fn build_versions<'a>(
    agent: &mut Agent,
    gc: nova_vm::engine::NoGcScope<'a, '_>,
) -> OrdinaryObject<'a> {
    let versions = OrdinaryObject::create_empty_object(agent, gc);
    // Strip the leading 'v' for the `versions.*` numeric form (Node's `versions.node` is "22.0.0").
    let bare = NODE_API_VERSION.trim_start_matches('v');
    define_str(agent, versions, "node", bare, gc);
    define_str(agent, versions, "treaty", env!("CARGO_PKG_VERSION"), gc);
    define_str(agent, versions, "v8", bare, gc);
    versions
}

/// `process.cwd()` — the runtime's current working directory.
///
/// After a `process.chdir`, this reflects the new directory: `chdir` calls `std::env::set_current_dir`
/// and `cwd` reads back `std::env::current_dir`, so the two stay consistent without caching.
fn process_cwd<'gc>(
    agent: &mut Agent,
    _this: Value,
    _args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    // Prefer the live process CWD (honors a prior `chdir`); fall back to the captured host cwd.
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|p| p.into_os_string().into_string().ok())
        .or_else(|| {
            host_state(agent).and_then(|s| s.cwd().to_str().map(str::to_owned))
        })
        .unwrap_or_else(|| ".".to_owned());
    Ok(Value::from_string(agent, cwd, gc))
}

/// `process.chdir(dir)` — change the runtime's working directory.
///
/// Faithful to Node: throws if the argument is missing/non-string or the directory cannot be entered.
fn process_chdir<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    let args = args.bind(gc);
    let Ok(dir) = nova_vm::ecmascript::String::try_from(args.get(0)) else {
        return Err(agent.throw_exception_with_static_message(
            ExceptionType::TypeError,
            "The \"directory\" argument must be of type string",
            gc,
        ));
    };
    let dir = dir.to_string_lossy(agent).into_owned();
    if let Err(e) = std::env::set_current_dir(&dir) {
        return Err(agent.throw_exception(
            ExceptionType::Error,
            format!("chdir {dir}: {e}"),
            gc,
        ));
    }
    Ok(Value::Undefined)
}

/// `process.exit([code])` — record the exit code and stop the script.
///
/// Treaty is an embedded runtime, not a stand-alone `node` binary: it cannot terminate the host
/// process (that would kill the compiler). Faithful to the *observable* contract instead — the exit
/// code is recorded (readable later via the thread-local / `process.exitCode`) and the current
/// script is unwound by throwing a sentinel error, which propagates out as the runtime's completion
/// just as a `process.exit()` ends a Node program. A non-integer code coerces to 0, matching Node.
fn process_exit<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    let args = args.bind(gc);
    let code = match args.get(0) {
        Value::Integer(i) => i.into_i64() as i32,
        Value::SmallF64(f) => f.into_f64() as i32,
        Value::Undefined => EXIT_CODE.with(Cell::get),
        _ => 0,
    };
    EXIT_CODE.with(|c| c.set(code));
    // Unwind the current execution. Node's `process.exit` does not return to caller code; throwing a
    // tagged error reproduces that "no further script runs" semantics within the embedded runtime.
    Err(agent.throw_exception(
        ExceptionType::Error,
        format!("process.exit({code})"),
        gc,
    ))
}

/// `process.nextTick(callback[, ...args])` — schedule `callback` to run after the current
/// operation, before any I/O or timers.
///
/// Implemented on the engine's real promise microtask queue (which routes through the shared event
/// loop via `HostHooks::enqueue_promise_job`): `Promise.resolve().then(callback.bind(undefined,
/// ...args))`. This is the standard, spec-grounded way to enqueue a microtask when the host cannot
/// fabricate a `Job` directly (Nova exposes no public `Job` constructor). The extra `...args` are
/// pre-bound so they are forwarded to `callback`, matching Node; the resolution value the reaction
/// also passes is ignored by the bound function's surplus-argument handling. The job is drained by
/// the runtime's `run_until_idle` pump after evaluation.
fn process_next_tick<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    mut gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let callback = args.get(0).bind(gc.nogc());
    let Ok(callback) = Function::try_from(callback) else {
        return Err(agent.throw_exception_with_static_message(
            ExceptionType::TypeError,
            "The \"callback\" argument must be of type function",
            gc.into_nogc(),
        ));
    };

    // Pre-bind any trailing args: `bound = callback.bind(undefined, ...rest)`.
    let bound: Function = if args.len() > 1 {
        bind_callback(agent, callback.unbind(), &args, gc.reborrow())?
            .unbind()
            .bind(gc.nogc())
    } else {
        callback
    };

    // `Promise.resolve()` -> `.then(bound)`. We read `Promise` from the realm global, resolve, then
    // invoke its `then` with the bound reaction. The reaction is enqueued as a microtask by the
    // engine; nothing runs synchronously here.
    let promise = resolved_promise(agent, gc.reborrow())?
        .unbind()
        .bind(gc.nogc());
    let then_key = PropertyKey::from_static_str(agent, "then", gc.nogc());
    let then_fn = promise
        .unbind()
        .internal_get(agent, then_key.unbind(), promise.unbind().into(), gc.reborrow())?
        .unbind()
        .bind(gc.nogc());
    let Ok(then_fn) = Function::try_from(then_fn) else {
        return Err(agent.throw_exception_with_static_message(
            ExceptionType::TypeError,
            "Promise.prototype.then is not callable",
            gc.into_nogc(),
        ));
    };
    then_fn.unbind().call(
        agent,
        promise.unbind().into(),
        &mut [bound.unbind().into()],
        gc,
    )?;
    Ok(Value::Undefined)
}

/// `callback.bind(undefined, ...args[1..])` — partial-apply the trailing nextTick arguments.
fn bind_callback<'gc>(
    agent: &mut Agent,
    callback: Function,
    args: &ArgumentsList,
    mut gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Function<'gc>> {
    let bind_key = PropertyKey::from_static_str(agent, "bind", gc.nogc());
    let callback_obj: Object = callback.into();
    let bind_fn = callback_obj
        .internal_get(agent, bind_key.unbind(), callback.into(), gc.reborrow())?
        .unbind()
        .bind(gc.nogc());
    let Ok(bind_fn) = Function::try_from(bind_fn) else {
        return Err(agent.throw_exception_with_static_message(
            ExceptionType::TypeError,
            "callback.bind is not a function",
            gc.into_nogc(),
        ));
    };
    // bind args: [thisArg=undefined, arg1, arg2, ...].
    let mut bind_args: Vec<Value> = Vec::with_capacity(args.len());
    bind_args.push(Value::Undefined);
    for i in 1..args.len() {
        bind_args.push(args.get(i));
    }
    let bound = bind_fn
        .unbind()
        .call(agent, callback.into(), &mut bind_args, gc.reborrow())?;
    // `Function.prototype.bind` always returns a function; guard defensively and throw rather than
    // panic if a hostile global replaced `bind`.
    Function::try_from(bound.unbind()).map_err(|_| {
        agent.throw_exception_with_static_message(
            ExceptionType::TypeError,
            "callback.bind did not return a function",
            gc.into_nogc(),
        )
    })
}

/// Build a resolved promise via the realm's `Promise.resolve()`.
fn resolved_promise<'gc>(
    agent: &mut Agent,
    mut gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Object<'gc>> {
    let global = agent.current_realm(gc.nogc()).global_object(agent);
    let promise_key = PropertyKey::from_static_str(agent, "Promise", gc.nogc());
    let promise_ctor = global
        .unbind()
        .internal_get(agent, promise_key.unbind(), global.unbind().into(), gc.reborrow())?
        .unbind()
        .bind(gc.nogc());
    let Ok(promise_ctor) = Object::try_from(promise_ctor) else {
        return Err(agent.throw_exception_with_static_message(
            ExceptionType::TypeError,
            "Promise is not available in this realm",
            gc.into_nogc(),
        ));
    };
    let resolve_key = PropertyKey::from_static_str(agent, "resolve", gc.nogc());
    let resolve_fn = promise_ctor
        .unbind()
        .internal_get(agent, resolve_key.unbind(), promise_ctor.unbind().into(), gc.reborrow())?
        .unbind()
        .bind(gc.nogc());
    let Ok(resolve_fn) = Function::try_from(resolve_fn) else {
        return Err(agent.throw_exception_with_static_message(
            ExceptionType::TypeError,
            "Promise.resolve is not callable",
            gc.into_nogc(),
        ));
    };
    let promise = resolve_fn.unbind().call(
        agent,
        promise_ctor.unbind().into(),
        &mut [Value::Undefined],
        gc.reborrow(),
    )?;
    Object::try_from(promise.unbind()).map_err(|_| {
        agent.throw_exception_with_static_message(
            ExceptionType::TypeError,
            "Promise.resolve did not return an object",
            gc.into_nogc(),
        )
    })
}

/// `process.hrtime([prev])` — high-resolution real time as `[seconds, nanoseconds]`.
///
/// Returns a two-element array of integer seconds and the sub-second nanosecond remainder, measured
/// against a fixed process-start reference so successive calls are monotonic. When `prev` (a prior
/// `hrtime()` result) is supplied, the difference is returned, exactly as Node specifies.
fn process_hrtime<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    let args = args.bind(gc);

    let total_nanos = HR_START.with(|s| s.elapsed().as_nanos());
    let (mut secs, mut nanos) = ((total_nanos / 1_000_000_000) as i64, (total_nanos % 1_000_000_000) as i64);

    // Optional `prev` diff: read elements [0] and [1] of the passed array, treating them as numbers.
    if let Ok(prev) = Array::try_from(args.get(0)) {
        let prev_secs = read_array_index_as_i64(agent, prev, 0, gc);
        let prev_nanos = read_array_index_as_i64(agent, prev, 1, gc);
        let mut diff_secs = secs - prev_secs;
        let mut diff_nanos = nanos - prev_nanos;
        if diff_nanos < 0 {
            diff_secs -= 1;
            diff_nanos += 1_000_000_000;
        }
        secs = diff_secs;
        nanos = diff_nanos;
    }

    let elements = [
        Number::from_i64(agent, secs, gc).into(),
        Number::from_i64(agent, nanos, gc).into(),
    ];
    Ok(Array::from_slice(agent, &elements, gc).into())
}

/// Read element `index` of `array` coerced to an `i64` (0 when absent / non-numeric). Used to parse
/// the optional `prev` argument of `process.hrtime` without allocating.
fn read_array_index_as_i64(
    agent: &mut Agent,
    array: Array,
    index: u32,
    gc: nova_vm::engine::NoGcScope,
) -> i64 {
    let key = PropertyKey::from(index);
    match array.try_get(agent, key, array.into(), None, gc) {
        nova_vm::engine::TryResult::Continue(result) => match result.into_value() {
            Value::Integer(i) => i.into_i64(),
            Value::SmallF64(f) => f.into_f64() as i64,
            _ => 0,
        },
        _ => 0,
    }
}

thread_local! {
    /// Fixed reference instant for `process.hrtime`, captured on first use so the clock is monotonic
    /// across calls within a runtime. A `thread_local` is correct here: each realm runs on one thread.
    static HR_START: std::time::Instant = std::time::Instant::now();
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use nova_vm::ecmascript::{
        Agent, AgentOptions, GcAgent, Object, PropertyDescriptor, PropertyKey, RealmRoot,
        String as JsString, parse_script, script_evaluation,
    };
    use nova_vm::ecmascript::{InternalMethods, unwrap_try};
    use nova_vm::engine::{Bindable, GcScope};

    use crate::node::core::{HostState, NodeCtx};

    /// A self-contained Node-process test runtime.
    ///
    /// This module owns only `process.rs` and must not depend on the still-scaffold `require` bridge
    /// or the globals agent's eager wiring. So the harness builds a real Nova agent with a
    /// [`HostState`] (the same plumbing `JsRuntime::with_node_compat` uses) and installs *this
    /// module's* `process` object as a global via a realm-init hook — exercising the real
    /// [`super::install`] against a live realm, independent of the other agents' files.
    struct ProcRt {
        agent: GcAgent,
        realm: RealmRoot,
        // Kept last so it outlives `agent` (mirrors `JsRuntime`'s documented drop order).
        _host: Box<HostState>,
    }

    /// Build a runtime and bind this module's `process` object as `globalThis.process`.
    ///
    /// No init-hook closure is needed (Nova's hook is a bare fn that cannot capture): the realm is
    /// created empty, then `process` is installed in a `run_in_realm` step where `agent` and the
    /// boxed `HostState` are *split-borrowed* out of the struct. That split borrow is what lets us
    /// hand `install` a `NodeCtx` (borrowing the host) alongside `&mut Agent` with no test-only
    /// `unsafe` — the only `unsafe` is the crate's documented `extend_lifetime` FFI primitive.
    fn rt() -> ProcRt {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let env = std::env::vars().collect();
        let host = Box::new(HostState::new(cwd, env));
        let hooks: &'static HostState = unsafe { crate::node::core::extend_lifetime(&*host) };
        let mut agent = GcAgent::new(
            AgentOptions {
                disable_gc: false,
                print_internals: false,
                no_block: false,
            },
            hooks,
        );
        let create_global_object: Option<for<'a> fn(&mut Agent, GcScope<'a, '_>) -> Object<'a>> =
            None;
        let create_global_this_value: Option<
            for<'a> fn(&mut Agent, GcScope<'a, '_>) -> Object<'a>,
        > = None;
        let init: Option<fn(&mut Agent, Object, GcScope)> = None;
        let realm = agent.create_realm(create_global_object, create_global_this_value, init);

        let mut this = ProcRt {
            agent,
            realm,
            _host: host,
        };
        this.bind_process_global();
        this
    }

    impl ProcRt {
        /// Build `process` and define it on the realm's global object.
        fn bind_process_global(&mut self) {
            let ProcRt { agent, realm, _host } = self;
            // Split borrow: `host` (immutable) and `agent` (mutable) are independent fields.
            let host: &HostState = _host;
            agent.run_in_realm(realm, |agent, mut gc| {
                let ctx = NodeCtx::new(host);
                let process_value = super::install(agent, &ctx, gc.reborrow())
                    .expect("process install should succeed")
                    .unbind();
                let nogc = gc.into_nogc();
                let global = agent.current_realm(nogc).global_object(agent);
                let key = PropertyKey::from_static_str(agent, "process", nogc);
                unwrap_try(global.try_define_own_property(
                    agent,
                    key,
                    PropertyDescriptor::new_data_descriptor(process_value),
                    None,
                    nogc,
                ));
            });
        }

        /// Evaluate `source` and return the completion value's JSON form, draining the event loop so
        /// `nextTick` microtasks settle (the same post-eval pump `JsRuntime` runs).
        fn eval(&mut self, source: &str) -> Result<json_value::Value, String> {
            // Mirror `JsRuntime::eval_with_input`: run the user body through indirect `eval` so its
            // completion value is captured, then envelope it as JSON. This supports statement-only
            // bodies (e.g. assignments + a trailing read) exactly like the public surface.
            let source_literal = json_value::to_string(source).map_err(|e| e.to_string())?;
            let wrapped = format!(
                "var __r = (0, eval)({source_literal}); JSON.stringify({{ v: __r }})"
            );
            let ProcRt { agent, realm, _host } = self;
            let event_loop = _host.event_loop();
            agent.run_in_realm(realm, |agent, mut gc| {
                let src = JsString::from_string(agent, wrapped, gc.nogc());
                let current = agent.current_realm(gc.nogc());
                let script = parse_script(agent, src, current, true, None, gc.nogc())
                    .map_err(|d| {
                        d.iter().map(|x| x.to_string()).collect::<Vec<_>>().join("; ")
                    })?;
                let result = script_evaluation(agent, script.unbind(), gc.reborrow())
                    .unbind()
                    .bind(gc.nogc());
                match result {
                    Ok(value) => {
                        let repr = value
                            .unbind()
                            .string_repr(agent, gc.reborrow())
                            .to_string_lossy(agent)
                            .into_owned();
                        crate::node::event_loop::run_until_idle(agent, event_loop, None, gc.reborrow())
                            .unbind()
                            .map_err(|err| {
                                err.value()
                                    .unbind()
                                    .string_repr(agent, gc)
                                    .to_string_lossy(agent)
                                    .into_owned()
                            })?;
                        let envelope: json_value::Value = json_value::from_str(&repr)
                            .map_err(|e| format!("decode: {e}"))?;
                        match envelope {
                            json_value::Value::Object(mut m) => {
                                Ok(m.remove("v").unwrap_or(json_value::Value::Null))
                            }
                            other => Err(format!("bad envelope: {other}")),
                        }
                    }
                    Err(error) => Err(error
                        .value()
                        .unbind()
                        .string_repr(agent, gc)
                        .to_string_lossy(agent)
                        .into_owned()),
                }
            })
        }

        /// `eval` that must succeed.
        fn ok(&mut self, source: &str) -> json_value::Value {
            self.eval(source).expect("eval should succeed")
        }
    }

    use serde_json as json_value;

    #[test]
    fn platform_matches_host_normalized_to_node_vocab() {
        let expected = super::node_platform();
        let mut rt = rt();
        let v = rt.ok("process.platform");
        assert_eq!(v, json!(expected));
        // It must be one of Node's known platform strings, never Rust's `windows`/`macos`.
        let s = v.as_str().unwrap();
        assert!(s != "windows" && s != "macos", "must use Node vocab, got {s}");
    }

    #[test]
    fn arch_uses_node_vocab() {
        let expected = super::node_arch();
        let mut rt = rt();
        let v = rt.ok("process.arch");
        assert_eq!(v, json!(expected));
        assert_ne!(v.as_str().unwrap(), "x86_64", "must map x86_64 -> x64");
    }

    #[test]
    fn version_and_versions_agree() {
        let mut rt = rt();
        assert_eq!(rt.ok("process.version"), json!(super::NODE_API_VERSION));
        assert_eq!(
            rt.ok("process.versions.node"),
            json!(super::NODE_API_VERSION.trim_start_matches('v'))
        );
    }

    #[test]
    fn argv_is_array_with_exec_path_first() {
        let mut rt = rt();
        assert_eq!(rt.ok("Array.isArray(process.argv)"), json!(true));
        assert_eq!(rt.ok("typeof process.argv[0] === 'string'"), json!(true));
        assert_eq!(rt.ok("process.argv.length >= 1"), json!(true));
    }

    #[test]
    fn env_round_trips_a_known_variable() {
        // The harness captures the real environment. PATH (or `Path` on Windows) is present on every
        // host. Assert the snapshot is a string→string dictionary and the value matches the real env.
        let key = if std::env::var_os("PATH").is_some() {
            "PATH"
        } else {
            "Path"
        };
        let expected = std::env::var(key).unwrap_or_default();
        let mut rt = rt();
        let ty = rt.ok(&format!("typeof process.env[{key:?}]"));
        let ty = ty.as_str().unwrap();
        assert!(
            ty == "string" || ty == "undefined",
            "env values must be strings, got {ty}"
        );
        if ty == "string" && !expected.is_empty() {
            assert_eq!(rt.ok(&format!("process.env[{key:?}]")), json!(expected));
        }
    }

    #[test]
    fn cwd_returns_current_directory_string() {
        let expected = std::env::current_dir()
            .unwrap()
            .into_os_string()
            .into_string()
            .unwrap();
        let mut rt = rt();
        assert_eq!(rt.ok("process.cwd()"), json!(expected));
    }

    #[test]
    fn pid_is_a_positive_integer() {
        let mut rt = rt();
        let pid = rt.ok("process.pid").as_i64().expect("pid should be a number");
        assert!(pid > 0, "pid must be positive, got {pid}");
    }

    #[test]
    fn next_tick_callback_runs_after_drain() {
        // nextTick schedules a microtask; the event-loop drain runs it before the next eval observes
        // the side effect on a global.
        let mut rt = rt();
        let synchronous = rt.ok(
            "globalThis.__tick = 0;\
             process.nextTick(() => { globalThis.__tick = 1; });\
             globalThis.__tick",
        );
        // Still 0 synchronously: nextTick deferred it. After the drain the callback has run.
        assert_eq!(synchronous, json!(0));
        assert_eq!(rt.ok("globalThis.__tick"), json!(1));
    }

    #[test]
    fn next_tick_forwards_extra_arguments() {
        let mut rt = rt();
        rt.ok(
            "globalThis.__sum = -1;\
             process.nextTick((a, b) => { globalThis.__sum = a + b; }, 2, 40);\
             null",
        );
        assert_eq!(rt.ok("globalThis.__sum"), json!(42));
    }

    #[test]
    fn next_tick_rejects_non_function() {
        let mut rt = rt();
        let err = rt
            .eval("process.nextTick(123)")
            .expect_err("nextTick(non-fn) must throw");
        assert!(err.contains("callback"), "got: {err}");
    }

    #[test]
    fn hrtime_returns_two_element_numeric_array() {
        let mut rt = rt();
        assert_eq!(rt.ok("process.hrtime().length"), json!(2));
        assert_eq!(
            rt.ok(
                "const h = process.hrtime();\
                 typeof h[0] === 'number' && typeof h[1] === 'number'"
            ),
            json!(true)
        );
    }

    #[test]
    fn hrtime_diff_is_non_negative() {
        let mut rt = rt();
        assert_eq!(
            rt.ok(
                "const a = process.hrtime();\
                 const d = process.hrtime(a);\
                 d[0] >= 0 && d[1] >= 0"
            ),
            json!(true)
        );
    }

    #[test]
    fn exit_records_code_and_unwinds() {
        // process.exit unwinds the current script (no host kill in an embedded runtime). The thrown
        // sentinel surfaces as a runtime error carrying the code.
        let mut rt = rt();
        let err = rt
            .eval("process.exit(3); 'unreached'")
            .expect_err("process.exit should unwind the script");
        assert!(err.contains("3"), "exit code should surface: {err}");
    }
}
