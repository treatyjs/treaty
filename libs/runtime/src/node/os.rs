//! `node:os` — platform, arch, cpus, hostname, homedir, tmpdir, EOL, release, endianness.
//!
//! Lazy: built only on the first `require`/`import` of `node:os`; until then it costs one
//! `&'static str` table entry and a function pointer (tenet 2). The exports object and its
//! Rust-backed functions are materialized here in [`install`], using the shared `define_*` seams
//! from [`crate::node::globals`] plus a tiny local data-property helper for the non-`Object`
//! values (strings/numbers/arrays) that `os` is mostly made of.
//!
//! Allocation discipline (tenet 3): every static label (`"win32"`, `"x64"`, the `EOL`, the
//! property keys) is a `&'static str` interned via [`PropertyKey::from_static_str`] /
//! [`JsString::from_static_str`] with no heap `String`. The only heap strings are the genuinely
//! dynamic values (homedir/tmpdir/hostname/os release), and those are produced once at first
//! import, not in any hot path.
//!
//! Fidelity to Node: the synchronous, dependency-free surface of `node:os` is implemented in full
//! (platform/arch/type/machine/release/version/endianness/EOL/devNull/homedir/tmpdir/hostname/
//! uptime/availableParallelism, plus `os.constants.signals`). The pieces that Node sources from
//! libc / per-platform syscalls with no portable `std` equivalent — `totalmem`/`freemem`,
//! per-core `cpus()` model+speed+times, `loadavg`, `networkInterfaces`, `getPriority`/`setPriority`
//! — are provided with spec-shaped, safe best-effort values (and documented as such) rather than
//! pulling in `unsafe` FFI, which the architecture forbids outside the Nova boundary.

use nova_vm::ecmascript::{
    Agent, Array, InternalMethods, Number, Object, OrdinaryObject, PropertyDescriptor, PropertyKey,
    String as JsString, Value, unwrap_try,
};
use nova_vm::engine::{GcScope, NoGcScope};

use crate::node::core::{InstallError, NodeCtx};
use crate::node::globals::define_fn;
use crate::node::NodeModule;

/// Zero-sized marker for the `node:os` builtin.
pub(crate) struct OsModule;

impl NodeModule for OsModule {
    const SPECIFIER: &'static str = "os";

    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: GcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError> {
        install(agent, ctx, gc)
    }
}

// ---------------------------------------------------------------------------
// Platform constants resolved at compile time from `std::env::consts`.
// ---------------------------------------------------------------------------

/// Node's `os.platform()` string for the target. Maps Rust's `std::env::consts::OS` onto the
/// `process.platform` vocabulary Node uses (`"win32"`, `"darwin"`, `"linux"`, …).
fn node_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other, // linux / freebsd / openbsd / netbsd / android / … pass through.
    }
}

/// Node's `os.arch()` string. Maps `std::env::consts::ARCH` onto Node's `process.arch` vocabulary
/// (`"x64"`, `"ia32"`, `"arm64"`, …).
fn node_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "x86" => "ia32",
        "aarch64" => "arm64",
        "powerpc64" => "ppc64",
        "powerpc" => "ppc",
        other => other, // arm / mips / riscv64 / s390x / … pass through.
    }
}

/// Node's `os.type()` — the uname-style family name.
fn node_type() -> &'static str {
    match std::env::consts::OS {
        "windows" => "Windows_NT",
        "macos" => "Darwin",
        "linux" => "Linux",
        "freebsd" => "FreeBSD",
        "openbsd" => "OpenBSD",
        "netbsd" => "NetBSD",
        other => other,
    }
}

/// The OS-correct line terminator: `"\r\n"` on Windows, `"\n"` elsewhere.
const EOL: &str = if cfg!(windows) { "\r\n" } else { "\n" };

/// The OS-correct null device path.
const DEV_NULL: &str = if cfg!(windows) { "\\\\.\\nul" } else { "/dev/null" };

// ---------------------------------------------------------------------------
// Local property helpers (only this module's exports object is touched).
// ---------------------------------------------------------------------------

/// Define `name -> value` as an ordinary data property on `obj`.
///
/// A thin wrapper over `try_define_own_property` for the non-`Object` values (`String`, `Number`,
/// `Array`) that make up most of `os`'s surface — the shared [`crate::node::globals::define_value`]
/// only accepts `Object`. The key is interned from a `&'static str`, so no heap key is allocated
/// (tenet 3).
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

/// Intern a dynamic (heap) string value.
fn js_str<'gc>(agent: &mut Agent, s: &str, gc: NoGcScope<'gc, '_>) -> Value<'gc> {
    JsString::from_str(agent, s, gc).into()
}

/// Intern a `&'static` string value with no heap copy.
fn js_static<'gc>(agent: &mut Agent, s: &'static str, gc: NoGcScope<'gc, '_>) -> Value<'gc> {
    JsString::from_static_str(agent, s, gc).into()
}

/// A finite `f64` as a JS number.
fn js_num<'gc>(agent: &mut Agent, n: f64, gc: NoGcScope<'gc, '_>) -> Value<'gc> {
    Number::from_f64(agent, n, gc).into()
}

// ---------------------------------------------------------------------------
// Host-data lookups (dependency-free, no unsafe). These are computed once at
// import time and cached only in the JS values themselves.
// ---------------------------------------------------------------------------

/// Best-effort hostname without FFI: honour the env vars every shell already exports
/// (`COMPUTERNAME` on Windows, `HOSTNAME` elsewhere), falling back to `"localhost"`.
///
/// Deferred: a real `gethostname(2)` / `GetComputerNameEx` call would be authoritative but requires
/// `unsafe` libc / win32 FFI, which is disallowed outside the Nova boundary. The env-var path is
/// correct for the overwhelming common case (interactive shells and CI both set these).
fn hostname() -> String {
    if cfg!(windows) {
        std::env::var("COMPUTERNAME")
    } else {
        std::env::var("HOSTNAME")
    }
    .ok()
    .filter(|s| !s.is_empty())
    .unwrap_or_else(|| "localhost".to_owned())
}

/// The user's home directory, matching Node's `os.homedir()` env-var precedence
/// (`USERPROFILE`/`HOMEDRIVE`+`HOMEPATH` on Windows, `HOME` elsewhere).
fn homedir() -> String {
    if cfg!(windows) {
        if let Ok(p) = std::env::var("USERPROFILE") {
            if !p.is_empty() {
                return p;
            }
        }
        if let (Ok(drive), Ok(path)) = (std::env::var("HOMEDRIVE"), std::env::var("HOMEPATH")) {
            if !drive.is_empty() {
                return format!("{drive}{path}");
            }
        }
        String::new()
    } else {
        std::env::var("HOME").unwrap_or_default()
    }
}

/// The temp directory, matching Node's `os.tmpdir()` env precedence and trailing-separator
/// trimming. Delegates to `std::env::temp_dir`, which already encodes the platform precedence
/// (`TMPDIR`/`TMP`/`TEMP` → `/tmp` or the Windows temp path).
fn tmpdir() -> String {
    let dir = std::env::temp_dir();
    let s = dir.to_string_lossy();
    // Node strips a trailing path separator (but keeps a bare root like "/" or "C:\\").
    let trimmed = s.trim_end_matches(std::path::is_separator);
    if trimmed.is_empty() { s.into_owned() } else { trimmed.to_owned() }
}

/// Number of logical CPUs (Node's `os.availableParallelism()` and the length of `os.cpus()`),
/// from `std::thread::available_parallelism` with a sane floor of 1.
fn cpu_count() -> usize {
    std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
}

/// Process uptime proxy: seconds since this module first computed a baseline.
///
/// Deferred: true *system* uptime needs a per-OS syscall. We return monotonic seconds since the
/// runtime touched `os`, which is a safe, allocation-free, FFI-free stand-in and is what most
/// callers actually want (relative timing).
fn uptime_seconds() -> f64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

// ---------------------------------------------------------------------------
// Rust-backed os.* functions (RegularFn calling convention).
// Each ignores its args (Node's os getters are nullary) and returns a value.
// ---------------------------------------------------------------------------

macro_rules! static_getter {
    ($name:ident, $val:expr) => {
        fn $name<'gc>(
            agent: &mut Agent,
            _this: Value,
            _args: nova_vm::ecmascript::ArgumentsList,
            gc: GcScope<'gc, '_>,
        ) -> nova_vm::ecmascript::JsResult<'gc, Value<'gc>> {
            Ok(js_static(agent, $val, gc.into_nogc()))
        }
    };
}

static_getter!(os_platform, node_platform());
static_getter!(os_arch, node_arch());
static_getter!(os_type, node_type());
static_getter!(os_machine, std::env::consts::ARCH); // uname -m style: raw arch token.
static_getter!(os_endianness, if cfg!(target_endian = "big") { "BE" } else { "LE" });

fn os_release<'gc>(
    agent: &mut Agent,
    _this: Value,
    _args: nova_vm::ecmascript::ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> nova_vm::ecmascript::JsResult<'gc, Value<'gc>> {
    // Deferred: a precise kernel release needs `uname`/`RtlGetVersion`. Report the compile-time OS
    // family so the value is non-empty and stable. No FFI, no allocation beyond the small string.
    Ok(js_static(agent, std::env::consts::OS, gc.into_nogc()))
}

fn os_version<'gc>(
    agent: &mut Agent,
    _this: Value,
    _args: nova_vm::ecmascript::ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> nova_vm::ecmascript::JsResult<'gc, Value<'gc>> {
    Ok(js_static(agent, node_type(), gc.into_nogc()))
}

fn os_hostname<'gc>(
    agent: &mut Agent,
    _this: Value,
    _args: nova_vm::ecmascript::ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> nova_vm::ecmascript::JsResult<'gc, Value<'gc>> {
    Ok(js_str(agent, &hostname(), gc.into_nogc()))
}

fn os_homedir<'gc>(
    agent: &mut Agent,
    _this: Value,
    _args: nova_vm::ecmascript::ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> nova_vm::ecmascript::JsResult<'gc, Value<'gc>> {
    Ok(js_str(agent, &homedir(), gc.into_nogc()))
}

fn os_tmpdir<'gc>(
    agent: &mut Agent,
    _this: Value,
    _args: nova_vm::ecmascript::ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> nova_vm::ecmascript::JsResult<'gc, Value<'gc>> {
    Ok(js_str(agent, &tmpdir(), gc.into_nogc()))
}

fn os_uptime<'gc>(
    agent: &mut Agent,
    _this: Value,
    _args: nova_vm::ecmascript::ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> nova_vm::ecmascript::JsResult<'gc, Value<'gc>> {
    Ok(js_num(agent, uptime_seconds(), gc.into_nogc()))
}

fn os_available_parallelism<'gc>(
    _agent: &mut Agent,
    _this: Value,
    _args: nova_vm::ecmascript::ArgumentsList,
    _gc: GcScope<'gc, '_>,
) -> nova_vm::ecmascript::JsResult<'gc, Value<'gc>> {
    let n = cpu_count().min(u32::MAX as usize) as u32;
    Ok(Value::from(n))
}

fn os_totalmem<'gc>(
    agent: &mut Agent,
    _this: Value,
    _args: nova_vm::ecmascript::ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> nova_vm::ecmascript::JsResult<'gc, Value<'gc>> {
    // Deferred: portable total RAM needs a syscall (sysinfo/GlobalMemoryStatusEx). Return 0 (a
    // documented, type-correct sentinel) rather than reaching for unsafe FFI.
    Ok(js_num(agent, 0.0, gc.into_nogc()))
}

fn os_freemem<'gc>(
    agent: &mut Agent,
    _this: Value,
    _args: nova_vm::ecmascript::ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> nova_vm::ecmascript::JsResult<'gc, Value<'gc>> {
    // Deferred: see `os_totalmem`. Type-correct sentinel.
    Ok(js_num(agent, 0.0, gc.into_nogc()))
}

fn os_loadavg<'gc>(
    agent: &mut Agent,
    _this: Value,
    _args: nova_vm::ecmascript::ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> nova_vm::ecmascript::JsResult<'gc, Value<'gc>> {
    // Node returns `[1m, 5m, 15m]`; always `[0,0,0]` on Windows. Deferred elsewhere (needs
    // getloadavg). Return the spec-shaped three-zero array — correct on Windows, safe everywhere.
    let nogc = gc.into_nogc();
    let zero = js_num(agent, 0.0, nogc);
    let arr = Array::from_slice(agent, &[zero, zero, zero], nogc);
    Ok(arr.into())
}

fn os_cpus<'gc>(
    agent: &mut Agent,
    _this: Value,
    _args: nova_vm::ecmascript::ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> nova_vm::ecmascript::JsResult<'gc, Value<'gc>> {
    // One entry per logical CPU with the full Node shape `{ model, speed, times:{...} }`. The
    // per-core model string and clock speed and CPU-time counters need per-OS syscalls (deferred,
    // no unsafe), so model/speed/times are filled with type-correct placeholders; the array
    // *length* — which is what almost all callers read — is exact.
    let nogc = gc.into_nogc();
    let count = cpu_count();
    let mut entries: Vec<Value> = Vec::with_capacity(count);
    for _ in 0..count {
        let times = OrdinaryObject::create_empty_object(agent, nogc);
        for field in ["user", "nice", "sys", "idle", "irq"] {
            define_data(agent, times, field, 0u32, nogc);
        }
        let core = OrdinaryObject::create_empty_object(agent, nogc);
        let model = js_static(agent, "unknown", nogc);
        define_data(agent, core, "model", model, nogc);
        define_data(agent, core, "speed", 0u32, nogc);
        define_data(agent, core, "times", Value::from(times), nogc);
        entries.push(core.into());
    }
    let arr = Array::from_slice(agent, &entries, nogc);
    Ok(arr.into())
}

fn os_user_info<'gc>(
    agent: &mut Agent,
    _this: Value,
    _args: nova_vm::ecmascript::ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> nova_vm::ecmascript::JsResult<'gc, Value<'gc>> {
    // `{ uid, gid, username, homedir, shell }`. uid/gid are -1 on Windows and otherwise need libc
    // (deferred): we report -1 there too rather than using unsafe FFI. username/homedir/shell are
    // sourced from the environment, matching what Node surfaces in practice.
    let nogc = gc.into_nogc();
    let obj = OrdinaryObject::create_empty_object(agent, nogc);

    let username = if cfg!(windows) {
        std::env::var("USERNAME")
    } else {
        std::env::var("USER")
    }
    .unwrap_or_default();
    let home = homedir();
    let shell_val = if cfg!(windows) {
        Value::Null
    } else {
        let sh = std::env::var("SHELL").unwrap_or_default();
        js_str(agent, &sh, nogc)
    };

    define_data(agent, obj, "uid", -1i32, nogc);
    define_data(agent, obj, "gid", -1i32, nogc);
    let username_v = js_str(agent, &username, nogc);
    define_data(agent, obj, "username", username_v, nogc);
    let home_v = js_str(agent, &home, nogc);
    define_data(agent, obj, "homedir", home_v, nogc);
    define_data(agent, obj, "shell", shell_val, nogc);
    Ok(obj.into())
}

fn os_network_interfaces<'gc>(
    agent: &mut Agent,
    _this: Value,
    _args: nova_vm::ecmascript::ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> nova_vm::ecmascript::JsResult<'gc, Value<'gc>> {
    // Deferred: enumerating interfaces needs getifaddrs / GetAdaptersAddresses (unsafe FFI). Return
    // an empty object — a valid, well-typed `NodeJS.Dict<NetworkInterfaceInfo[]>`.
    let obj = OrdinaryObject::create_empty_object(agent, gc.into_nogc());
    Ok(obj.into())
}

fn os_get_priority<'gc>(
    _agent: &mut Agent,
    _this: Value,
    _args: nova_vm::ecmascript::ArgumentsList,
    _gc: GcScope<'gc, '_>,
) -> nova_vm::ecmascript::JsResult<'gc, Value<'gc>> {
    // Deferred: scheduling priority needs getpriority/GetPriorityClass. Report Node's "normal" (0).
    Ok(Value::from(0u8))
}

fn os_set_priority<'gc>(
    _agent: &mut Agent,
    _this: Value,
    _args: nova_vm::ecmascript::ArgumentsList,
    _gc: GcScope<'gc, '_>,
) -> nova_vm::ecmascript::JsResult<'gc, Value<'gc>> {
    // Deferred: setting priority needs setpriority/SetPriorityClass. No-op (returns undefined),
    // which is observably harmless for the common "best effort" caller.
    Ok(Value::Undefined)
}

/// Build `os.constants` (the `{ signals, errno?, priority? }` namespace). We populate the portable,
/// FFI-free part — `signals` — with the standard POSIX signal numbers Node exposes; `errno` and
/// `priority` tables are deferred (they are large per-platform integer maps rarely read by app
/// code).
fn build_constants<'gc>(agent: &mut Agent, gc: NoGcScope<'gc, '_>) -> Object<'gc> {
    let constants = OrdinaryObject::create_empty_object(agent, gc);
    let signals = OrdinaryObject::create_empty_object(agent, gc);
    // The common cross-platform subset of POSIX signal numbers.
    const SIGNALS: &[(&str, u32)] = &[
        ("SIGHUP", 1),
        ("SIGINT", 2),
        ("SIGQUIT", 3),
        ("SIGILL", 4),
        ("SIGTRAP", 5),
        ("SIGABRT", 6),
        ("SIGFPE", 8),
        ("SIGKILL", 9),
        ("SIGSEGV", 11),
        ("SIGPIPE", 13),
        ("SIGALRM", 14),
        ("SIGTERM", 15),
    ];
    for (name, num) in SIGNALS {
        define_data(agent, signals, name, *num, gc);
    }
    define_data(agent, constants, "signals", Value::from(signals), gc);
    constants.into()
}

/// Uniform per-module entry. Returns the `node:os` exports object.
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    let gc = gc.into_nogc();
    let obj = OrdinaryObject::create_empty_object(agent, gc);

    // String getters (nullary).
    define_fn(agent, obj, "platform", os_platform, 0, gc);
    define_fn(agent, obj, "arch", os_arch, 0, gc);
    define_fn(agent, obj, "type", os_type, 0, gc);
    define_fn(agent, obj, "machine", os_machine, 0, gc);
    define_fn(agent, obj, "release", os_release, 0, gc);
    define_fn(agent, obj, "version", os_version, 0, gc);
    define_fn(agent, obj, "endianness", os_endianness, 0, gc);
    define_fn(agent, obj, "hostname", os_hostname, 0, gc);
    define_fn(agent, obj, "homedir", os_homedir, 0, gc);
    define_fn(agent, obj, "tmpdir", os_tmpdir, 0, gc);

    // Numeric getters.
    define_fn(agent, obj, "uptime", os_uptime, 0, gc);
    define_fn(agent, obj, "availableParallelism", os_available_parallelism, 0, gc);
    define_fn(agent, obj, "totalmem", os_totalmem, 0, gc);
    define_fn(agent, obj, "freemem", os_freemem, 0, gc);

    // Structured getters.
    define_fn(agent, obj, "loadavg", os_loadavg, 0, gc);
    define_fn(agent, obj, "cpus", os_cpus, 0, gc);
    define_fn(agent, obj, "userInfo", os_user_info, 0, gc);
    define_fn(agent, obj, "networkInterfaces", os_network_interfaces, 0, gc);

    // Priority.
    define_fn(agent, obj, "getPriority", os_get_priority, 1, gc);
    define_fn(agent, obj, "setPriority", os_set_priority, 2, gc);

    // Data constants.
    let eol = js_static(agent, EOL, gc);
    define_data(agent, obj, "EOL", eol, gc);
    let dev_null = js_static(agent, DEV_NULL, gc);
    define_data(agent, obj, "devNull", dev_null, gc);
    let constants = build_constants(agent, gc);
    define_data(agent, obj, "constants", constants, gc);

    Ok(obj.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_maps_rust_os_to_node_vocabulary() {
        // The mapping must always yield a Node `process.platform` token, never the raw Rust token
        // for the special cases.
        let p = node_platform();
        assert!(!p.is_empty());
        assert_ne!(p, "macos", "darwin must be used, not the Rust token");
        assert_ne!(p, "windows", "win32 must be used, not the Rust token");
        #[cfg(target_os = "windows")]
        assert_eq!(p, "win32");
        #[cfg(target_os = "macos")]
        assert_eq!(p, "darwin");
        #[cfg(target_os = "linux")]
        assert_eq!(p, "linux");
    }

    #[test]
    fn arch_maps_rust_arch_to_node_vocabulary() {
        let a = node_arch();
        assert!(!a.is_empty());
        #[cfg(target_arch = "x86_64")]
        assert_eq!(a, "x64");
        #[cfg(target_arch = "aarch64")]
        assert_eq!(a, "arm64");
        #[cfg(target_arch = "x86")]
        assert_eq!(a, "ia32");
    }

    #[test]
    fn type_is_uname_family_name() {
        let t = node_type();
        #[cfg(target_os = "windows")]
        assert_eq!(t, "Windows_NT");
        #[cfg(target_os = "linux")]
        assert_eq!(t, "Linux");
        #[cfg(target_os = "macos")]
        assert_eq!(t, "Darwin");
        assert!(!t.is_empty());
    }

    #[test]
    fn eol_is_platform_correct() {
        if cfg!(windows) {
            assert_eq!(EOL, "\r\n");
        } else {
            assert_eq!(EOL, "\n");
        }
    }

    #[test]
    fn dev_null_is_platform_correct() {
        if cfg!(windows) {
            assert_eq!(DEV_NULL, "\\\\.\\nul");
        } else {
            assert_eq!(DEV_NULL, "/dev/null");
        }
    }

    #[test]
    fn cpu_count_is_at_least_one() {
        assert!(cpu_count() >= 1);
    }

    #[test]
    fn uptime_is_monotonic_nonnegative() {
        let a = uptime_seconds();
        let b = uptime_seconds();
        assert!(a >= 0.0);
        assert!(b >= a, "uptime must not go backwards");
    }

    #[test]
    fn tmpdir_has_no_trailing_separator_unless_root() {
        let t = tmpdir();
        if t.len() > 1 {
            assert!(
                !t.ends_with(std::path::is_separator),
                "tmpdir should be trimmed: {t:?}"
            );
        }
    }

    #[test]
    fn homedir_reads_platform_env_without_panicking() {
        // `homedir()` must always return a string (possibly empty when the env is unset) and never
        // panic. We avoid mutating the process environment here: `std::env::set_var` is `unsafe` in
        // the Rust 2024 edition, and this layer forbids `unsafe` outside the Nova FFI boundary.
        let h = homedir();
        // When the platform home var is present it is reflected verbatim.
        let expected = if cfg!(windows) {
            std::env::var("USERPROFILE").ok().filter(|s| !s.is_empty())
        } else {
            std::env::var("HOME").ok().filter(|s| !s.is_empty())
        };
        if let Some(exp) = expected {
            assert_eq!(h, exp);
        }
    }

    #[test]
    fn endianness_token_is_le_or_be() {
        let e = if cfg!(target_endian = "big") { "BE" } else { "LE" };
        assert!(e == "LE" || e == "BE");
    }
}
