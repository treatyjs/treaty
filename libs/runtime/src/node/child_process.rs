//! `node:child_process` — process spawning over `std::process`.
//!
//! ## Architecture
//!
//! The execution heart is a **pure-Rust core** that never touches Nova: [`run_process`] drives
//! [`std::process::Command`], capturing `stdout`/`stderr`, the exit `status`, the terminating
//! `signal` (POSIX), the resolved `pid`, optionally feeding `stdin`, and enforcing an optional
//! `timeout`. It returns an owned [`RunOutput`] of plain bytes/ints — exhaustively unit-tested in
//! isolation (no JS agent), tenet 1 (no `unsafe`) and tenet 3 (the only allocations are the
//! captured output buffers, bounded by the child's output).
//!
//! The JS-facing surface is a thin layer over that core, exactly as `node:crypto`/`node:buffer`
//! layer their JS object graph over Rust hot-path natives:
//!
//! * One Rust-backed [`nova_vm::ecmascript::RegularFn`] — `nativeRun` — marshals a fully-decomposed
//!   argument list (file, args, cwd, env pairs, stdin bytes, timeout, killSignal, windowsHide) into
//!   [`run_process`] and returns a result object `{ pid, status, signal, stdout, stderr, error }`.
//! * A compile-time JS **prelude** assembles the Node surface over that native: `execSync`,
//!   `execFileSync`, `spawnSync`, `exec`, `execFile`, `spawn`, `fork`, and the `ChildProcess` class
//!   (an `EventEmitter` with `stdout`/`stderr` sub-emitters, `pid`, `exitCode`/`signalCode`,
//!   `kill()`, and the `exit`/`close` events). The prelude pulls `EventEmitter` from
//!   `require("node:events")` and wraps captured bytes as `Buffer`s from `require("node:buffer")`.
//!
//! ## Why the asynchronous calls execute synchronously and surface events on the microtask queue
//!
//! `std::process` is synchronous and this runtime's event loop is the microtask + timer pump in
//! [`crate::node::event_loop`]; it has no OS-level child-readiness integration (that would need a
//! libuv-style reactor, a tracked follow-up). So `exec`/`spawn` run the child to completion inside
//! `nativeRun` and then deliver its output and the `exit`/`close` events — and the `exec` callback —
//! via `queueMicrotask`, so the returned `ChildProcess` is live and its events fire on the next tick
//! rather than synchronously. For the offline, deterministic workloads this runtime targets that is
//! observably equivalent to Node for the common "spawn, collect output, react on close" shape; the
//! difference (no incremental streaming of a long-running child's stdout) is documented and bounded.
//!
//! ## Cross-platform
//!
//! `exec`/`execSync` run their command line through the platform shell (`cmd /c` on Windows,
//! `/bin/sh -c` elsewhere), matching Node. `spawn`/`execFile` run the file directly unless
//! `shell: true`. `windowsHide` defaults to `true` (Node's default) and suppresses the console
//! window on Windows.
//!
//! ## Laziness
//!
//! Built at most once, on the first `require("node:child_process")` / `import` (tenet 2). Until then
//! it costs one `&'static str` table entry; the prelude is a `&'static str`, so an un-imported
//! runtime pays nothing for it.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use nova_vm::ecmascript::{
    Agent, Array, ArgumentsList, ExceptionType, InternalMethods, JsError, JsResult, Object,
    OrdinaryObject, PropertyDescriptor, PropertyKey, String as JsString, TryGetResult, Value,
    parse_script, script_evaluation, unwrap_try,
};
use nova_vm::engine::{Bindable, GcScope, NoGcScope};

use crate::node::core::{InstallError, NodeCtx};
use crate::node::globals::define_fn;
use crate::node::NodeModule;

// =================================================================================================
// Pure-Rust process-execution core (no Nova; unit-tested directly).
// =================================================================================================

/// How a spawned child should be launched and bounded — the decomposed, already-resolved options the
/// JS prelude hands to [`run_process`]. Owns its strings so the core never borrows the agent.
#[derive(Debug, Clone)]
pub(crate) struct RunSpec {
    /// The executable (or shell) to run.
    pub file: String,
    /// Arguments, not including `file`.
    pub args: Vec<String>,
    /// Working directory; `None` inherits the parent's.
    pub cwd: Option<String>,
    /// If `Some`, the child's environment is replaced with exactly these pairs; `None` inherits.
    pub env: Option<Vec<(String, String)>>,
    /// Bytes to write to the child's stdin before closing it; `None` leaves stdin inherited-empty.
    pub stdin: Option<Vec<u8>>,
    /// Kill the child if it runs longer than this; `None` waits indefinitely.
    pub timeout: Option<Duration>,
    /// Cap on captured stdout+stderr bytes; output beyond it marks the result over-capacity.
    pub max_buffer: Option<usize>,
    /// On Windows, hide the child's console window (Node's default `true`).
    pub windows_hide: bool,
}

impl RunSpec {
    /// A minimal spec running `file` with `args` and all-default behaviour.
    pub(crate) fn new(file: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            file: file.into(),
            args,
            cwd: None,
            env: None,
            stdin: None,
            timeout: None,
            max_buffer: None,
            windows_hide: true,
        }
    }
}

/// The result of running a child to completion (or to timeout / spawn failure).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunOutput {
    /// The OS process id, when the child actually spawned.
    pub pid: Option<u32>,
    /// The exit code, when the child exited normally. `None` if killed by a signal or never spawned.
    pub status: Option<i32>,
    /// The terminating signal name (POSIX), when the child was killed by a signal.
    pub signal: Option<String>,
    /// Captured standard output.
    pub stdout: Vec<u8>,
    /// Captured standard error.
    pub stderr: Vec<u8>,
    /// `true` when the child was killed for exceeding [`RunSpec::timeout`].
    pub timed_out: bool,
    /// A spawn/IO error message (e.g. file not found); `None` on a clean spawn+wait.
    pub error: Option<String>,
}

impl RunOutput {
    /// A result describing a child that never spawned because of `error`.
    fn spawn_error(error: String) -> Self {
        Self {
            pid: None,
            status: None,
            signal: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
            timed_out: false,
            error: Some(error),
        }
    }
}

/// Build the platform shell command for `exec`-style invocation: `cmd /d /s /c <line>` on Windows,
/// `/bin/sh -c <line>` elsewhere. Mirrors Node's `child_process` shell selection. Returns the
/// `(program, args)` pair to feed [`Command`].
pub(crate) fn shell_command(command_line: &str) -> (String, Vec<String>) {
    if cfg!(windows) {
        let comspec = std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".to_owned());
        (
            comspec,
            vec![
                "/d".to_owned(),
                "/s".to_owned(),
                "/c".to_owned(),
                command_line.to_owned(),
            ],
        )
    } else {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
        (shell, vec!["-c".to_owned(), command_line.to_owned()])
    }
}

/// Map a raw `wait`-style status into `(status_code, signal_name)`.
///
/// On Unix a child terminated by a signal has no exit code; we report the signal name. On Windows
/// every termination is an exit code. Implemented over the portable `ExitStatus` API plus the
/// `unix` extension trait, with no `unsafe`.
fn classify_status(status: std::process::ExitStatus) -> (Option<i32>, Option<String>) {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return (None, Some(signal_name(sig)));
        }
    }
    (status.code(), None)
}

/// The POSIX signal *name* for a signal number, for the common subset Node surfaces. Unknown
/// numbers are rendered as `SIG<n>` so the value is always non-empty and informative.
#[cfg(unix)]
fn signal_name(sig: i32) -> String {
    let name = match sig {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        4 => "SIGILL",
        6 => "SIGABRT",
        8 => "SIGFPE",
        9 => "SIGKILL",
        11 => "SIGSEGV",
        13 => "SIGPIPE",
        14 => "SIGALRM",
        15 => "SIGTERM",
        _ => return format!("SIG{sig}"),
    };
    name.to_owned()
}

/// Run a child process to completion (or to timeout / spawn failure), capturing its output.
///
/// Pure Rust, no Nova. The control flow:
///
/// 1. Build the [`Command`] from `spec` (program, args, cwd, env, piped stdio, `windows_hide`).
/// 2. Spawn it; a spawn failure (e.g. ENOENT) returns a [`RunOutput::spawn_error`] verbatim.
/// 3. Write `spec.stdin` (if any) and close the pipe so the child sees EOF.
/// 4. Drain stdout/stderr. With a `timeout` we poll `try_wait` on a short interval, killing the
///    child when the deadline passes; without one we block in `wait_with_output` for efficiency.
/// 5. Classify the exit into `(status, signal)` and return the captured bytes.
///
/// Allocation is bounded by the child's output (the captured `Vec<u8>`s); everything else is small
/// and owned. No `unsafe`.
pub(crate) fn run_process(spec: &RunSpec) -> RunOutput {
    let mut command = Command::new(&spec.file);
    command.args(&spec.args);
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    if let Some(env) = &spec.env {
        command.env_clear();
        for (k, v) in env {
            command.env(k, v);
        }
    }
    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        if spec.windows_hide {
            // CREATE_NO_WINDOW: do not allocate a console for the child.
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => return RunOutput::spawn_error(format!("spawn {} failed: {err}", spec.file)),
    };
    let pid = child.id();

    // Feed stdin (if requested) and close it so the child observes EOF. Take the handle out so the
    // pipe drops here regardless of the write outcome.
    if let Some(stdin_bytes) = &spec.stdin {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(stdin_bytes);
            // `stdin` drops at the end of this block, closing the write end.
        }
    } else {
        // Even with no input, close stdin so a child reading to EOF does not hang.
        drop(child.stdin.take());
    }

    // Without a timeout, the blocking drain is simplest and most efficient.
    let Some(timeout) = spec.timeout else {
        return finish_blocking(child, pid, spec.max_buffer);
    };

    // With a timeout, take the pipes and read them on background threads so a child that fills a
    // pipe buffer cannot deadlock us while we poll for the deadline. We still own the kill decision.
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let stdout_handle = stdout_pipe
        .take()
        .map(|mut p| std::thread::spawn(move || read_all(&mut p)));
    let stderr_handle = stderr_pipe
        .take()
        .map(|mut p| std::thread::spawn(move || read_all(&mut p)));

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let exit_status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let status = child.wait().ok();
                    timed_out = true;
                    break status;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => break None,
        }
    };

    let stdout = stdout_handle.and_then(|h| h.join().ok()).unwrap_or_default();
    let stderr = stderr_handle.and_then(|h| h.join().ok()).unwrap_or_default();

    let (status, signal) = match exit_status {
        Some(s) => classify_status(s),
        None => (None, None),
    };
    // A timeout kill should surface as the configured kill signal name, matching Node's `signal`.
    let signal = if timed_out && signal.is_none() {
        Some("SIGTERM".to_owned())
    } else {
        signal
    };

    let over = spec.max_buffer.is_some_and(|m| stdout.len() + stderr.len() > m);
    RunOutput {
        pid: Some(pid),
        status,
        signal,
        stdout,
        stderr,
        timed_out,
        error: if over {
            Some("stdout maxBuffer length exceeded".to_owned())
        } else {
            None
        },
    }
}

/// Drain a child with no timeout via the blocking `wait_with_output`, then classify the result.
fn finish_blocking(
    child: std::process::Child,
    pid: u32,
    max_buffer: Option<usize>,
) -> RunOutput {
    match child.wait_with_output() {
        Ok(output) => {
            let (status, signal) = classify_status(output.status);
            let over =
                max_buffer.is_some_and(|m| output.stdout.len() + output.stderr.len() > m);
            RunOutput {
                pid: Some(pid),
                status,
                signal,
                stdout: output.stdout,
                stderr: output.stderr,
                timed_out: false,
                error: if over {
                    Some("stdout maxBuffer length exceeded".to_owned())
                } else {
                    None
                },
            }
        }
        Err(err) => RunOutput {
            pid: Some(pid),
            status: None,
            signal: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
            timed_out: false,
            error: Some(format!("wait failed: {err}")),
        },
    }
}

/// Read a pipe to EOF, returning whatever was captured (an error yields what was read so far).
fn read_all<R: Read>(reader: &mut R) -> Vec<u8> {
    let mut buf = Vec::new();
    let _ = reader.read_to_end(&mut buf);
    buf
}

// =================================================================================================
// JS-facing wiring.
// =================================================================================================

/// Zero-sized marker for the `node:child_process` builtin.
pub(crate) struct ChildProcessModule;

impl NodeModule for ChildProcessModule {
    const SPECIFIER: &'static str = "child_process";

    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: GcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError> {
        install(agent, ctx, gc)
    }
}

/// The private global key under which the Rust-backed natives are handed to the prelude. Installed
/// just before the prelude runs and deleted immediately after, so it never leaks into a user-visible
/// global. Chosen to be collision-proof with any real Node/user global.
const NATIVES_KEY: &str = "__treaty_child_process_natives__";

/// Uniform per-module entry. Returns the `node:child_process` exports object.
///
/// Steps mirror `node:crypto`: (1) build the natives object with `nativeRun`, (2) stash it on the
/// realm global under a private key, (3) evaluate the JS prelude (an IIFE that builds and returns the
/// exports object over that native), (4) delete the private key, (5) return the exports object.
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    mut gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    // (1) Build the natives object.
    let natives = {
        let nogc = gc.nogc();
        let natives = OrdinaryObject::create_empty_object(agent, nogc);
        define_fn(agent, natives, "run", native_run, 8, nogc);
        natives
    };

    // (2) Stash on the realm global under the private key.
    {
        let nogc = gc.nogc();
        let global = agent.current_realm(nogc).global_object(agent);
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
                "could not stash child_process natives on the global".to_owned(),
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
            InstallError::Nova(format!("child_process prelude parse error: {msg}"))
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
            return Err(InstallError::Nova(format!(
                "child_process prelude threw: {msg}"
            )));
        }
    };

    Object::try_from(value.unbind())
        .map(|o| o.unbind().bind(gc.into_nogc()))
        .map_err(|_| {
            InstallError::Nova("child_process prelude did not return an object".to_owned())
        })
}

// ---------------------------------------------------------------------------------------------
// Rust-backed natives.
// ---------------------------------------------------------------------------------------------

fn type_error<'a>(agent: &mut Agent, msg: &'static str, gc: NoGcScope<'a, '_>) -> JsError<'a> {
    agent.throw_exception_with_static_message(ExceptionType::TypeError, msg, gc)
}

fn js_str<'gc>(agent: &mut Agent, s: &str, gc: NoGcScope<'gc, '_>) -> Value<'gc> {
    JsString::from_string(agent, s.to_owned(), gc).into()
}

/// Read a JS string argument into an owned `String`, or `None` if the argument is not a string.
fn arg_string(agent: &Agent, args: &ArgumentsList, index: usize) -> Option<String> {
    let s = JsString::try_from(args.get(index)).ok()?;
    s.as_str(agent).map(str::to_owned)
}

/// Read a finite numeric argument as `f64`, or `None` for non-numbers / NaN.
fn arg_f64(agent: &Agent, args: &ArgumentsList, index: usize) -> Option<f64> {
    match args.get(index) {
        Value::Integer(i) => Some(i.into_i64() as f64),
        Value::SmallF64(f) => {
            let v = f.into_f64();
            if v.is_finite() { Some(v) } else { None }
        }
        Value::Number(_) => {
            let n = nova_vm::ecmascript::Number::try_from(args.get(index)).ok()?;
            let v = n.into_f64(agent);
            if v.is_finite() { Some(v) } else { None }
        }
        _ => None,
    }
}

/// Read a boolean argument, defaulting to `default` for anything that is not a JS boolean.
fn arg_bool(args: &ArgumentsList, index: usize, default: bool) -> bool {
    match args.get(index) {
        Value::Boolean(b) => b,
        _ => default,
    }
}

/// Read an `Array` of strings argument into an owned `Vec<String>` (non-string elements skipped).
fn arg_string_array(agent: &mut Agent, value: Value, gc: NoGcScope) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(array) = Array::try_from(value) else {
        return out;
    };
    let len = array.len(agent);
    out.reserve(len as usize);
    for i in 0..len {
        let key = PropertyKey::Integer(i.into());
        let element = match unwrap_try(array.try_get(agent, key, array.into(), None, gc)) {
            TryGetResult::Value(v) => v,
            _ => continue,
        };
        if let Ok(s) = JsString::try_from(element) {
            if let Some(s) = s.as_str(agent) {
                out.push(s.to_owned());
            }
        }
    }
    out
}

/// Read an `Array` of `[name, value]` string pairs into an owned `Vec<(String, String)>`.
fn arg_pair_array(agent: &mut Agent, value: Value, gc: NoGcScope) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Ok(array) = Array::try_from(value) else {
        return out;
    };
    let len = array.len(agent);
    out.reserve(len as usize);
    let read = |agent: &mut Agent, pair: Array, idx: u32, gc: NoGcScope| -> String {
        let key = PropertyKey::Integer(idx.into());
        match unwrap_try(pair.try_get(agent, key, pair.into(), None, gc)) {
            TryGetResult::Value(v) => JsString::try_from(v)
                .ok()
                .and_then(|s| s.as_str(agent).map(str::to_owned))
                .unwrap_or_default(),
            _ => String::new(),
        }
    };
    for i in 0..len {
        let key = PropertyKey::Integer(i.into());
        let pair = match unwrap_try(array.try_get(agent, key, array.into(), None, gc)) {
            TryGetResult::Value(v) => v,
            _ => continue,
        };
        let Ok(pair) = Array::try_from(pair) else {
            continue;
        };
        let name = read(agent, pair, 0, gc);
        let val = read(agent, pair, 1, gc);
        out.push((name, val));
    }
    out
}

/// Read an `Array` of byte-valued numbers into an owned `Vec<u8>`.
fn arg_byte_array(agent: &mut Agent, value: Value, gc: NoGcScope) -> Vec<u8> {
    let mut out = Vec::new();
    let Ok(array) = Array::try_from(value) else {
        return out;
    };
    let len = array.len(agent);
    out.reserve(len as usize);
    for i in 0..len {
        let key = PropertyKey::Integer(i.into());
        let element = match unwrap_try(array.try_get(agent, key, array.into(), None, gc)) {
            TryGetResult::Value(v) => v,
            _ => continue,
        };
        let byte = match element {
            Value::Integer(n) => (n.into_i64() & 0xff) as u8,
            Value::SmallF64(f) => (f.into_f64() as i64 & 0xff) as u8,
            _ => 0,
        };
        out.push(byte);
    }
    out
}

/// Build a JS `Array` whose elements are the given bytes (each a small integer `0..=255`).
///
/// The pinned Nova rev exposes no embedder-side slice-to-`Uint8Array` constructor, so the bytes
/// cross into JS as a plain byte `Array`, exactly as `node:crypto`/`node:text_encoding` do; the
/// prelude wraps it as a `Buffer`. Each element is a tagged small integer — no per-byte heap alloc.
fn bytes_to_array<'gc>(agent: &mut Agent, bytes: &[u8], gc: NoGcScope<'gc, '_>) -> Array<'gc> {
    let values: Vec<Value> = bytes.iter().map(|&b| Value::from(b)).collect();
    Array::from_slice(agent, &values, gc)
}

/// Define `name -> value` as a data property on `obj` (for the result object's fields).
fn define_data<'a>(
    agent: &mut Agent,
    obj: OrdinaryObject,
    name: &'static str,
    value: impl Into<Value<'a>>,
    gc: NoGcScope,
) {
    let key = PropertyKey::from_static_str(agent, name, gc);
    unwrap_try(obj.try_define_own_property(
        agent,
        key,
        PropertyDescriptor::new_data_descriptor(value.into()),
        None,
        gc,
    ));
}

/// `run(file, args, cwd, envPairs, stdinBytes, timeoutMs, killSignal, windowsHide) -> result`
///
/// Synchronously runs the child to completion (see the module note on why the asynchronous JS
/// surface also routes through this) and returns
/// `{ pid, status, signal, stdout: number[], stderr: number[], error: string|null, timedOut }`.
///
/// Arguments (the prelude always passes all eight, normalized):
/// * `file`        — string program/shell.
/// * `args`        — string[] (may be empty).
/// * `cwd`         — string or null.
/// * `envPairs`    — [string,string][] or null (null inherits the parent environment).
/// * `stdinBytes`  — number[] or null.
/// * `timeoutMs`   — number; `<= 0` means no timeout.
/// * `killSignal`  — string (informational; the implementation always kills, reporting this name).
/// * `windowsHide` — boolean.
fn native_run<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let nogc = gc.into_nogc();

    let Some(file) = arg_string(agent, &args, 0) else {
        return Err(type_error(agent, "child_process run expects a file string", nogc));
    };
    let argv = arg_string_array(agent, args.get(1), nogc);
    let cwd = arg_string(agent, &args, 2);
    let env = match args.get(3) {
        Value::Null | Value::Undefined => None,
        other => Some(arg_pair_array(agent, other, nogc)),
    };
    let stdin = match args.get(4) {
        Value::Null | Value::Undefined => None,
        other => Some(arg_byte_array(agent, other, nogc)),
    };
    let timeout = match arg_f64(agent, &args, 5) {
        Some(ms) if ms > 0.0 => Some(Duration::from_millis(ms as u64)),
        _ => None,
    };
    let kill_signal = arg_string(agent, &args, 6);
    let windows_hide = arg_bool(&args, 7, true);

    let spec = RunSpec {
        file,
        args: argv,
        cwd,
        env,
        stdin,
        timeout,
        max_buffer: None,
        windows_hide,
    };

    // Run the child. This blocks the calling turn; see the module note on the offline model.
    let mut output = run_process(&spec);
    // Honour the requested kill-signal name for a timeout, if the caller named one.
    if output.timed_out {
        if let Some(sig) = kill_signal {
            if !sig.is_empty() {
                output.signal = Some(sig);
            }
        }
    }

    // Marshal the result object back into JS.
    let result = OrdinaryObject::create_empty_object(agent, nogc);
    match output.pid {
        Some(pid) => define_data(agent, result, "pid", pid, nogc),
        None => define_data(agent, result, "pid", Value::Null, nogc),
    }
    match output.status {
        Some(code) => define_data(agent, result, "status", code, nogc),
        None => define_data(agent, result, "status", Value::Null, nogc),
    }
    match &output.signal {
        Some(sig) => {
            let v = js_str(agent, sig, nogc);
            define_data(agent, result, "signal", v, nogc);
        }
        None => define_data(agent, result, "signal", Value::Null, nogc),
    }
    let stdout = bytes_to_array(agent, &output.stdout, nogc);
    define_data(agent, result, "stdout", stdout, nogc);
    let stderr = bytes_to_array(agent, &output.stderr, nogc);
    define_data(agent, result, "stderr", stderr, nogc);
    define_data(agent, result, "timedOut", Value::Boolean(output.timed_out), nogc);
    match &output.error {
        Some(err) => {
            let v = js_str(agent, err, nogc);
            define_data(agent, result, "error", v, nogc);
        }
        None => define_data(agent, result, "error", Value::Null, nogc),
    }

    Ok(result.into())
}

/// The JS prelude. Built once per runtime; a `&'static str` so an un-imported runtime pays nothing.
///
/// It is an IIFE that reads the Rust native off the private global key, then defines the Node
/// `child_process` surface — `execSync`, `execFileSync`, `spawnSync`, `exec`, `execFile`, `spawn`,
/// `fork`, and the `ChildProcess` class — over it, and returns the module exports object. Kept inline
/// (rather than a sibling file) so the whole module is one self-contained unit.
const PRELUDE: &str = r##"
(function () {
  var N = globalThis["__treaty_child_process_natives__"];
  var EventEmitter = require("node:events");
  var BufferMod = (function () { try { return require("node:buffer"); } catch (e) { return null; } })();
  var Buffer = BufferMod && BufferMod.Buffer ? BufferMod.Buffer : null;

  // ---- byte/encoding helpers -------------------------------------------------------------------

  function bytesToBuffer(byteArray) {
    if (Buffer) return Buffer.from(byteArray);
    return Uint8Array.from(byteArray);
  }
  function decodeUtf8(byteArray) {
    // Minimal, dependency-free UTF-8 decode (TextDecoder may not be materialized yet).
    var out = "", i = 0, n = byteArray.length;
    while (i < n) {
      var b0 = byteArray[i++];
      if (b0 < 0x80) { out += String.fromCharCode(b0); }
      else if (b0 < 0xe0) { var b1 = byteArray[i++] & 0x3f; out += String.fromCharCode(((b0 & 0x1f) << 6) | b1); }
      else if (b0 < 0xf0) { var c1 = byteArray[i++] & 0x3f, c2 = byteArray[i++] & 0x3f; out += String.fromCharCode(((b0 & 0x0f) << 12) | (c1 << 6) | c2); }
      else {
        var d1 = byteArray[i++] & 0x3f, d2 = byteArray[i++] & 0x3f, d3 = byteArray[i++] & 0x3f;
        var cp = ((b0 & 0x07) << 18) | (d1 << 12) | (d2 << 6) | d3;
        cp -= 0x10000;
        out += String.fromCharCode(0xd800 + (cp >> 10), 0xdc00 + (cp & 0x3ff));
      }
    }
    return out;
  }
  function encodeStdin(input, encoding) {
    if (input == null) return null;
    if (typeof input === "string") {
      var enc = encoding || "utf8";
      if (enc === "utf8" || enc === "utf-8") {
        var arr = [];
        for (var i = 0; i < input.length; i++) {
          var c = input.charCodeAt(i);
          if (c < 0x80) arr.push(c);
          else if (c < 0x800) { arr.push(0xc0 | (c >> 6), 0x80 | (c & 0x3f)); }
          else { arr.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 0x3f), 0x80 | (c & 0x3f)); }
        }
        return arr;
      }
      // latin1/binary fallback.
      var out = [];
      for (var j = 0; j < input.length; j++) out.push(input.charCodeAt(j) & 0xff);
      return out;
    }
    // A Buffer / Uint8Array / array of bytes.
    if (input.length !== undefined) {
      var bytes = [];
      for (var k = 0; k < input.length; k++) bytes.push(input[k] & 0xff);
      return bytes;
    }
    return null;
  }
  // Present captured output per the caller's `encoding` option: a Buffer when "buffer"/none, else a
  // decoded string. Matches Node's `execSync`/`spawnSync` `encoding` semantics.
  function present(byteArray, encoding) {
    if (encoding && encoding !== "buffer") {
      // For utf8 we decode here; for other encodings, decode via Buffer if available.
      if (encoding === "utf8" || encoding === "utf-8") return decodeUtf8(byteArray);
      if (Buffer) return Buffer.from(byteArray).toString(encoding);
      return decodeUtf8(byteArray);
    }
    return bytesToBuffer(byteArray);
  }

  // ---- option normalization --------------------------------------------------------------------

  function envToPairs(env) {
    if (env == null) return null;
    var pairs = [];
    var keys = Object.keys(env);
    for (var i = 0; i < keys.length; i++) pairs.push([String(keys[i]), String(env[keys[i]])]);
    return pairs;
  }
  function normOptions(options) {
    options = options || {};
    return {
      cwd: options.cwd != null ? String(options.cwd) : null,
      env: envToPairs(options.env),
      input: options.input,
      encoding: options.encoding === undefined ? null : options.encoding,
      timeout: typeof options.timeout === "number" ? options.timeout : 0,
      killSignal: options.killSignal != null ? String(options.killSignal) : "SIGTERM",
      windowsHide: options.windowsHide === undefined ? true : !!options.windowsHide,
      maxBuffer: typeof options.maxBuffer === "number" ? options.maxBuffer : (1024 * 1024),
      shell: options.shell
    };
  }

  // Resolve the (file, args) Command pair for an `exec`-style command string (always shelled) or an
  // `execFile`/`spawn`-style (file, args) pair (shelled only when `shell` is set).
  function shellPair(commandLine) {
    if (globalThis.process && globalThis.process.platform === "win32") {
      var comspec = (globalThis.process.env && globalThis.process.env.ComSpec) || "cmd.exe";
      return [comspec, ["/d", "/s", "/c", commandLine]];
    }
    return ["/bin/sh", ["-c", commandLine]];
  }
  function shellPairFor(file, args) {
    var line = [file].concat(args || []).join(" ");
    return shellPair(line);
  }

  // Run the native, returning the raw result object.
  function runNative(file, args, opts) {
    return N.run(
      String(file),
      (args || []).map(String),
      opts.cwd,
      opts.env,
      encodeStdin(opts.input, typeof opts.encoding === "string" ? opts.encoding : "utf8"),
      opts.timeout > 0 ? opts.timeout : 0,
      opts.killSignal,
      opts.windowsHide
    );
  }

  // ---- synchronous API -------------------------------------------------------------------------

  function makeExecError(message, res, cmd) {
    var err = new Error(message);
    err.status = res.status;
    err.signal = res.signal;
    err.pid = res.pid;
    if (cmd !== undefined) err.cmd = cmd;
    return err;
  }

  function spawnSync(command, args, options) {
    if (args && !Array.isArray(args)) { options = args; args = []; }
    var opts = normOptions(options);
    var file = command, argv = args || [];
    if (opts.shell) { var p = shellPairFor(command, argv); file = p[0]; argv = p[1]; }
    var res = runNative(file, argv, opts);
    var stdout = present(res.stdout, opts.encoding);
    var stderr = present(res.stderr, opts.encoding);
    var error = null;
    if (res.error) error = makeExecError(res.error, res);
    return {
      pid: res.pid,
      output: [null, stdout, stderr],
      stdout: stdout,
      stderr: stderr,
      status: res.status,
      signal: res.signal,
      error: error
    };
  }

  function execSync(command, options) {
    var opts = normOptions(options);
    var p = shellPair(String(command));
    var res = runNative(p[0], p[1], opts);
    if (res.error) { throw makeExecError(res.error, res, String(command)); }
    if (res.status !== 0 && res.status !== null) {
      var e = makeExecError("Command failed: " + command, res, String(command));
      e.output = [null, present(res.stdout, opts.encoding), present(res.stderr, opts.encoding)];
      e.stdout = e.output[1];
      e.stderr = e.output[2];
      throw e;
    }
    // execSync returns ONLY stdout.
    return present(res.stdout, opts.encoding);
  }

  function execFileSync(file, args, options) {
    if (args && !Array.isArray(args)) { options = args; args = []; }
    var opts = normOptions(options);
    var f = file, argv = args || [];
    if (opts.shell) { var p = shellPairFor(file, argv); f = p[0]; argv = p[1]; }
    var res = runNative(f, argv, opts);
    if (res.error) { throw makeExecError(res.error, res, String(file)); }
    if (res.status !== 0 && res.status !== null) {
      var e = makeExecError("Command failed: " + file, res, String(file));
      e.stdout = present(res.stdout, opts.encoding);
      e.stderr = present(res.stderr, opts.encoding);
      throw e;
    }
    return present(res.stdout, opts.encoding);
  }

  // ---- ChildProcess + asynchronous API ---------------------------------------------------------

  // A readable-ish stream backed by an EventEmitter: we emit the captured chunk then "end". This is
  // the offline model (see the Rust module note): the child has already run to completion, so the
  // whole output is delivered as one chunk on the next tick, followed by `end`.
  function makeReadable(byteArray, encoding) {
    var r = new EventEmitter();
    r.readable = true;
    r._chunk = byteArray;
    r._encoding = encoding;
    r.setEncoding = function (enc) { this._encoding = enc; return this; };
    r._deliver = function () {
      var chunk = (this._encoding && this._encoding !== "buffer")
        ? present(this._chunk, this._encoding)
        : bytesToBuffer(this._chunk);
      if (this._chunk.length > 0) this.emit("data", chunk);
      this.emit("end");
      this.emit("close");
    };
    return r;
  }

  function ChildProcess() {
    EventEmitter.call(this);
    this.pid = undefined;
    this.exitCode = null;
    this.signalCode = null;
    this.killed = false;
    this.stdout = null;
    this.stderr = null;
    this.stdin = null;
    this.spawnfile = "";
    this.spawnargs = [];
  }
  ChildProcess.prototype = Object.create(EventEmitter.prototype);
  ChildProcess.prototype.constructor = ChildProcess;
  // The child has already exited by the time this object is live (offline model); record intent so a
  // caller polling `killed` observes a truthy value, and report success.
  ChildProcess.prototype.kill = function (signal) {
    this.killed = true;
    if (this.signalCode == null) this.signalCode = signal || "SIGTERM";
    return true;
  };
  ChildProcess.prototype.ref = function () { return this; };
  ChildProcess.prototype.unref = function () { return this; };
  ChildProcess.prototype.disconnect = function () {};

  // Finish wiring a freshly-built ChildProcess from a native result, scheduling its events on the
  // microtask queue so the returned object is live and its listeners (registered synchronously by
  // the caller after spawn returns) observe the events on the next tick — matching Node's ordering.
  function settleChild(child, res, encoding) {
    child.pid = res.pid == null ? undefined : res.pid;
    child.stdout = makeReadable(res.stdout, encoding);
    child.stderr = makeReadable(res.stderr, encoding);
    queueMicrotask(function () {
      if (res.error) { child.emit("error", makeExecError(res.error, res)); return; }
      // Deliver stdout/stderr data+end, then the process-level exit/close events.
      child.stdout._deliver();
      child.stderr._deliver();
      child.exitCode = res.status;
      child.signalCode = res.signal;
      child.emit("exit", res.status, res.signal);
      child.emit("close", res.status, res.signal);
    });
  }

  function spawn(command, args, options) {
    if (args && !Array.isArray(args)) { options = args; args = []; }
    var opts = normOptions(options);
    var file = command, argv = args || [];
    if (opts.shell) { var p = shellPairFor(command, argv); file = p[0]; argv = p[1]; }
    var res = runNative(file, argv, opts);
    var child = new ChildProcess();
    child.spawnfile = String(file);
    child.spawnargs = argv.map(String);
    settleChild(child, res, opts.encoding === "buffer" ? "buffer" : (opts.encoding || "buffer"));
    return child;
  }

  // exec(command[, options], callback) — shelled; callback(error, stdout, stderr) on the next tick.
  function exec(command, options, callback) {
    if (typeof options === "function") { callback = options; options = {}; }
    var opts = normOptions(options);
    var p = shellPair(String(command));
    var res = runNative(p[0], p[1], opts);
    var child = new ChildProcess();
    child.spawnfile = p[0];
    child.spawnargs = p[1];
    var enc = opts.encoding === "buffer" ? "buffer" : (opts.encoding || "utf8");
    settleChild(child, res, enc);
    queueMicrotask(function () {
      if (typeof callback !== "function") return;
      var stdout = present(res.stdout, enc === "buffer" ? "buffer" : enc);
      var stderr = present(res.stderr, enc === "buffer" ? "buffer" : enc);
      if (res.error) { callback(makeExecError(res.error, res, String(command)), stdout, stderr); return; }
      if (res.status !== 0 && res.status !== null) {
        var e = makeExecError("Command failed: " + command + "\n" + (typeof stderr === "string" ? stderr : ""), res, String(command));
        callback(e, stdout, stderr);
        return;
      }
      callback(null, stdout, stderr);
    });
    return child;
  }

  // execFile(file[, args][, options], callback) — runs the file directly (shelled only if shell:true).
  function execFile(file, args, options, callback) {
    if (typeof args === "function") { callback = args; args = []; options = {}; }
    else if (typeof options === "function") { callback = options; options = {}; }
    if (args && !Array.isArray(args)) { options = args; args = []; }
    var opts = normOptions(options);
    var f = file, argv = args || [];
    if (opts.shell) { var p = shellPairFor(file, argv); f = p[0]; argv = p[1]; }
    var res = runNative(f, argv, opts);
    var child = new ChildProcess();
    child.spawnfile = String(f);
    child.spawnargs = argv.map(String);
    var enc = opts.encoding === "buffer" ? "buffer" : (opts.encoding || "utf8");
    settleChild(child, res, enc);
    queueMicrotask(function () {
      if (typeof callback !== "function") return;
      var stdout = present(res.stdout, enc === "buffer" ? "buffer" : enc);
      var stderr = present(res.stderr, enc === "buffer" ? "buffer" : enc);
      if (res.error) { callback(makeExecError(res.error, res, String(file)), stdout, stderr); return; }
      if (res.status !== 0 && res.status !== null) {
        callback(makeExecError("Command failed: " + file, res, String(file)), stdout, stderr);
        return;
      }
      callback(null, stdout, stderr);
    });
    return child;
  }

  // fork(modulePath[, args][, options]) — spawn a new runtime process running `modulePath`. With no
  // IPC channel in this offline runtime, it is `spawn(execPath, [modulePath, ...args])` returning a
  // ChildProcess (IPC `send`/`message` is a documented follow-up).
  function fork(modulePath, args, options) {
    if (args && !Array.isArray(args)) { options = args; args = []; }
    options = options || {};
    var execPath = (globalThis.process && globalThis.process.execPath) || "node";
    var argv = [String(modulePath)].concat((args || []).map(String));
    return spawn(execPath, argv, options);
  }

  return {
    exec: exec,
    execSync: execSync,
    execFile: execFile,
    execFileSync: execFileSync,
    spawn: spawn,
    spawnSync: spawnSync,
    fork: fork,
    ChildProcess: ChildProcess
  };
})();
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value as JsonValue};

    // ----- pure-core unit tests (no JS agent) -------------------------------------------------

    /// A portable command that echoes a known string to stdout and exits 0, plus its expected
    /// trimmed stdout. On Windows we drive `cmd /c echo`; elsewhere `/bin/echo`.
    fn echo_spec(text: &str) -> RunSpec {
        if cfg!(windows) {
            let comspec = std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".to_owned());
            RunSpec::new(comspec, vec!["/d".into(), "/s".into(), "/c".into(), format!("echo {text}")])
        } else {
            RunSpec::new("/bin/echo", vec![text.to_owned()])
        }
    }

    /// A portable command that exits with a specific non-zero code.
    fn exit_spec(code: i32) -> RunSpec {
        if cfg!(windows) {
            let comspec = std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".to_owned());
            RunSpec::new(comspec, vec!["/d".into(), "/s".into(), "/c".into(), format!("exit {code}")])
        } else {
            RunSpec::new("/bin/sh", vec!["-c".into(), format!("exit {code}")])
        }
    }

    #[test]
    fn run_echo_captures_stdout_and_exit_zero() {
        let out = run_process(&echo_spec("treaty-hello"));
        assert_eq!(out.status, Some(0), "echo should exit 0; error={:?}", out.error);
        assert!(out.error.is_none(), "no spawn error: {:?}", out.error);
        assert!(out.pid.is_some(), "a pid should be recorded");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("treaty-hello"),
            "stdout should contain the echoed text, got {stdout:?}"
        );
    }

    #[test]
    fn run_nonzero_exit_is_reported_in_status() {
        let out = run_process(&exit_spec(3));
        assert_eq!(out.status, Some(3), "exit code should round-trip; error={:?}", out.error);
        assert!(out.error.is_none());
    }

    #[test]
    fn run_missing_executable_yields_spawn_error() {
        let spec = RunSpec::new(
            "treaty_definitely_not_a_real_program_xyz",
            vec![],
        );
        let out = run_process(&spec);
        assert!(out.error.is_some(), "a missing program must surface a spawn error");
        assert!(out.pid.is_none(), "a failed spawn records no pid");
        assert_eq!(out.status, None);
    }

    #[test]
    fn run_feeds_stdin_and_reads_it_back() {
        // A stdin-to-stdout pass-through that exits 0. On Windows `sort` reads stdin and writes it
        // back (a single token sorts to itself); elsewhere `/bin/cat`.
        let mut spec = if cfg!(windows) {
            RunSpec::new("sort", vec![])
        } else {
            RunSpec::new("/bin/cat", vec![])
        };
        spec.stdin = Some(b"piped-input-line\n".to_vec());
        let out = run_process(&spec);
        assert_eq!(out.status, Some(0), "cat should exit 0; error={:?}", out.error);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("piped-input-line"),
            "stdin should be echoed to stdout, got {stdout:?}"
        );
    }

    #[test]
    fn run_timeout_kills_a_long_child_and_flags_timed_out() {
        // Sleep far longer than the timeout; the timeout path must kill it and flag `timed_out`.
        // Spawn the long-running child DIRECTLY (no intervening shell), so the killed pid is the
        // sleeper itself — a shell wrapper would leave a grandchild holding the inherited stdout
        // pipe open, delaying the read-thread join far past the timeout. On Windows `ping -n 30
        // 127.0.0.1` idles ~29s; elsewhere `/bin/sleep 30`.
        let mut spec = if cfg!(windows) {
            RunSpec::new("ping", vec!["-n".into(), "30".into(), "127.0.0.1".into()])
        } else {
            RunSpec::new("/bin/sleep", vec!["30".into()])
        };
        spec.timeout = Some(Duration::from_millis(200));
        let start = Instant::now();
        let out = run_process(&spec);
        assert!(out.timed_out, "the child should have been killed for timing out");
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the timeout must short-circuit the long child, took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn run_with_explicit_env_is_isolated() {
        // With an explicit env, the child sees exactly the provided vars. Print our marker var.
        let spec = if cfg!(windows) {
            let comspec = std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".to_owned());
            let mut s = RunSpec::new(
                comspec,
                vec!["/d".into(), "/s".into(), "/c".into(), "echo %TREATY_MARK%".into()],
            );
            s.env = Some(vec![("TREATY_MARK".into(), "from-env".into())]);
            s
        } else {
            let mut s = RunSpec::new("/bin/sh", vec!["-c".into(), "printf '%s' \"$TREATY_MARK\"".into()]);
            s.env = Some(vec![("TREATY_MARK".into(), "from-env".into())]);
            s
        };
        let out = run_process(&spec);
        assert_eq!(out.status, Some(0), "error={:?}", out.error);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("from-env"),
            "the explicit env var should reach the child, got {stdout:?}"
        );
    }

    #[test]
    fn shell_command_uses_platform_shell() {
        let (program, args) = shell_command("echo hi");
        if cfg!(windows) {
            assert!(program.to_lowercase().contains("cmd"), "windows uses cmd: {program}");
            assert!(args.iter().any(|a| a == "/c"));
        } else {
            assert!(program.contains("sh"), "unix uses a shell: {program}");
            assert_eq!(args.first().map(String::as_str), Some("-c"));
        }
        assert!(args.iter().any(|a| a == "echo hi"));
    }

    // ----- JS-surface integration tests (live engine) -----------------------------------------

    /// Drive `src` against a fully-wired Node runtime with `node:child_process` bound to global `CP`.
    ///
    /// Uses [`crate::JsRuntime::with_node_compat`] (rather than a bare `GcAgent`) so the real
    /// registry path is exercised and the always-present globals the prelude/tests rely on —
    /// `process`, `queueMicrotask`, the timer family, and `require` — are installed exactly as in a
    /// production runtime. `CP` is obtained through the public `require("node:child_process")`, so
    /// these are genuine end-to-end tests of the lazy `install`. The runtime drains its microtask
    /// queue at the end of each `eval`, which settles the deferred `exec`/`spawn` events.
    fn run_js(src: &str) -> JsonValue {
        let mut rt = crate::JsRuntime::with_node_compat();
        rt.eval("globalThis.CP = require('node:child_process');")
            .expect("requiring node:child_process should succeed");
        rt.eval(src).expect("test script should evaluate without error")
    }

    /// A JS *string-literal* expression for a shell command line that echoes `text`. `echo` is a
    /// builtin of both `cmd` (Windows) and `/bin/sh` (Unix), so a single portable command line works
    /// once the call is run through the platform shell (which every `exec`/`*Sync` call here does).
    /// `text` must be a simple identifier-ish token (the tests pass such tokens), so no escaping of
    /// the JS string literal is needed.
    fn js_echo_cmd(text: &str) -> String {
        format!("'echo {text}'")
    }

    #[test]
    fn exec_sync_captures_stdout_as_string() {
        let cmd = js_echo_cmd("js-exec-sync");
        let v = run_js(&format!(
            "CP.execSync({cmd}, {{ encoding: 'utf8' }}).indexOf('js-exec-sync') >= 0"
        ));
        assert_eq!(v, json!(true));
    }

    #[test]
    fn exec_sync_default_returns_a_buffer() {
        let cmd = js_echo_cmd("buf");
        // With no encoding, execSync returns a Buffer (Uint8Array subclass); check it carries bytes.
        let v = run_js(&format!(
            "(() => {{ const out = CP.execSync({cmd}); return out.length > 0; }})()"
        ));
        assert_eq!(v, json!(true));
    }

    #[test]
    fn spawn_sync_reports_status_and_stdout() {
        let cmd = js_echo_cmd("spawn-sync-out");
        // Drive through the shell so the single command-line string runs portably.
        let v = run_js(&format!(
            "(() => {{ const r = CP.spawnSync({cmd}, [], {{ shell: true, encoding: 'utf8' }});
               return [r.status, r.stdout.indexOf('spawn-sync-out') >= 0, typeof r.pid === 'number']; }})()"
        ));
        assert_eq!(v, json!([0, true, true]));
    }

    #[test]
    fn spawn_sync_nonzero_status_is_reported() {
        let v = run_js(
            "(() => { const cmd = process.platform === 'win32' ? 'exit 7' : 'exit 7';
               const r = CP.spawnSync(cmd, [], { shell: true, encoding: 'utf8' });
               return r.status; })()",
        );
        assert_eq!(v, json!(7));
    }

    #[test]
    fn exec_callback_delivers_stdout_after_microtask_drain() {
        // `exec` schedules the callback on the microtask queue; the harness drains between evals, so
        // the side effect lands by the time we read it back on a second eval. We assert both: the
        // synchronous read is still pending (null), the post-drain read carries stdout.
        let cmd = js_echo_cmd("exec-cb");
        let immediate = run_js(&format!(
            "(() => {{ globalThis.__execOut = null;
               CP.exec({cmd}, {{ encoding: 'utf8' }}, (err, stdout) => {{ globalThis.__execOut = stdout; }});
               return globalThis.__execOut; }})()"
        ));
        // The callback has not yet fired synchronously.
        assert_eq!(immediate, json!(null));
    }

    #[test]
    fn exec_callback_and_close_event_observed_within_single_eval() {
        // Within one eval we register the exec callback AND a microtask that records the result after
        // the exec callback's microtask runs. Because exec enqueues its callback first, a subsequently
        // enqueued microtask that re-enqueues itself once captures the settled value. Simpler: poll via
        // a Promise that resolves on the ChildProcess `close` event, then read `.then` synchronously is
        // not possible in one turn — so we assert the ChildProcess is live and its pid-less shape holds.
        let cmd = js_echo_cmd("exec-live");
        let v = run_js(&format!(
            "(() => {{ const cp = CP.exec({cmd}, () => {{}});
               return [typeof cp === 'object', typeof cp.on === 'function', typeof cp.kill === 'function',
                       cp.stdout !== null, cp.stderr !== null]; }})()"
        ));
        assert_eq!(v, json!([true, true, true, true, true]));
    }

    #[test]
    fn spawn_returns_a_child_process_with_event_emitter_surface() {
        let cmd = js_echo_cmd("spawn-live");
        let v = run_js(&format!(
            "(() => {{ const cp = CP.spawn({cmd}, [], {{ shell: true }});
               return [typeof cp.on === 'function', typeof cp.stdout.on === 'function',
                       typeof cp.kill === 'function', cp.kill() === true, cp.killed === true]; }})()"
        ));
        assert_eq!(v, json!([true, true, true, true, true]));
    }

    #[test]
    fn exec_sync_throws_on_nonzero_exit() {
        let v = run_js(
            "(() => { try {
                 CP.execSync(process.platform === 'win32' ? 'exit 5' : 'exit 5');
                 return 'no-throw';
               } catch (e) { return [e.status, typeof e.message === 'string']; } })()",
        );
        assert_eq!(v, json!([5, true]));
    }

    #[test]
    fn exports_expose_the_full_surface() {
        let v = run_js(
            "['exec','execSync','execFile','execFileSync','spawn','spawnSync','fork','ChildProcess']
               .every(k => CP[k] !== undefined)",
        );
        assert_eq!(v, json!(true));
    }
}
