# Treaty Node-compat runtime — architecture

This document is the implementation contract for the module-granular Node-compatibility layer
that turns `treaty_runtime` from a synchronous expression evaluator into a small, lazy, low-memory
Node runtime on top of the Nova engine. It is precise enough to implement directly; each section
below maps to one file under `libs/runtime/src/node/`.

The design is grounded in the real Nova API at pinned rev `bece61ac` (the verified seams are noted
inline), the existing `JsRuntime` in `libs/runtime/src/lib.rs`, the existing oxc transpile in
`transpile.rs`, and `oxc_resolver 11.20.0` (verified to build alongside oxc `0.133`).

---

## 0. Design tenets (hard rules, enforced per file)

1. **No `unsafe` except the single Nova FFI lifetime extension.** Nova's `GcAgent::new` takes
   `&'static dyn HostHooks`, but our hooks live in the `JsRuntime`. The CLI reference solves this
   with one `unsafe fn extend_lifetime` (`nova_cli/src/lib/lib.rs:111`) and a documented drop order
   (host hooks declared last so they drop after the agent). We reuse exactly that one localized
   `unsafe`, in `core.rs`, with the same drop-order comment. No other file contains `unsafe`.
2. **Lazy materialization.** A `node:` builtin's JS object and its Rust-backed functions are built
   only on the *first* `require`/`import` of that specifier. Until then a module costs one
   `&'static str` table entry and a function pointer — zero heap objects, zero Nova handles.
   Always-present globals (`process`, `Buffer`, `console`, the timer functions, `queueMicrotask`,
   `structuredClone`, `TextEncoder`/`TextDecoder`, `URL`) install eagerly because Node code assumes
   they exist without import; everything reachable only through `require`/`import` installs lazily.
3. **Minimize allocation.** Prefer `&str`/`Cow<'_, str>` over `String`; borrow Nova slices
   (`ArrayBuffer::as_slice`, verified at `array_buffer.rs:139`) for zero-copy Buffer/text paths;
   intern fixed property keys with `PropertyKey::from_static_str`; keep one cached resolver and one
   module cache per runtime; never clone a JS value when a `Scoped`/`Global` handle suffices.
4. **Fast IO.** Synchronous `node:fs` calls go straight to `std::fs` with one syscall per op. The
   async `fs/promises` surface wraps the same `std::fs` call in a resolved Nova `Promise` rather
   than spawning a thread (the work is already done synchronously and cheaply); only genuinely
   blocking waits (timers) use the macrotask queue.
5. **One small single-responsibility file per module**, each exposing the identical `install`
   entry point (section 2). The registry and event loop are the only shared core.

---

## 1. scaffold_spec — the shared core

Four core files plus `mod.rs`. Everything else is a leaf module.

### 1.1 `node/mod.rs` — module tree + the `NodeModule` trait + registration table

Declares every submodule and owns the canonical specifier table. It defines the uniform contract
every module file implements:

```rust
/// A Nova GcScope alias kept short so module signatures stay uniform.
pub(crate) use nova_vm::engine::GcScope;
use nova_vm::ecmascript::{Agent, Object, Value};
use crate::node::core::{NodeCtx, InstallError};

/// One Node builtin module. Implemented as a zero-sized unit struct per file
/// (e.g. `pub struct PathModule;`) so the registry table is an array of fn pointers,
/// not boxed trait objects — zero allocation for unused modules.
pub(crate) trait NodeModule {
    /// The canonical bare specifier, e.g. "path". The registry also matches the
    /// "node:" prefixed form by stripping the prefix before lookup.
    const SPECIFIER: &'static str;

    /// Build this module's exports object and return it. Called at most ONCE per
    /// runtime, on first require/import. Must allocate only what the module needs.
    /// `this`-less: receives the agent + a NodeCtx handle for shared services
    /// (resolver, cwd, env) and a GcScope.
    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: GcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError>;
}
```

The uniform free-function entry every module file MUST expose (this is the exact signature the
registry calls; trait + free fn are kept in lockstep so the registry can store a plain fn pointer):

```rust
/// THE uniform per-module entry point. Every file under node/ exposes exactly this.
/// Returns the module's exports object, freshly built on the Nova heap.
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    ctx: &NodeCtx,
    gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError>;
```

`mod.rs` also owns the static dispatch table — an array of `(specifier, fn ptr)` pairs, no heap:

```rust
type InstallFn = for<'gc> fn(&mut Agent, &NodeCtx, GcScope<'gc, '_>)
    -> Result<Object<'gc>, InstallError>;

pub(crate) const BUILTINS: &[(&str, InstallFn)] = &[
    ("path", path::install),
    ("process", process::install),
    ("fs", fs::install),
    ("fs/promises", fs_promises::install),
    ("buffer", buffer::install),
    ("os", os::install),
    ("util", util::install),
    ("events", events::install),
    ("console", console::install),
    ("timers", timers::install),
    ("url", url::install),
    // text_encoding, fetch, structured_clone are surfaced as globals, not bare
    // specifiers, but also appear here so `require('node:util/...')`-style and
    // explicit imports resolve. (Entries added as those modules expose specifiers.)
];

/// Look up a builtin by specifier, tolerating the optional `node:` prefix.
pub(crate) fn lookup(specifier: &str) -> Option<InstallFn> {
    let bare = specifier.strip_prefix("node:").unwrap_or(specifier);
    BUILTINS.iter().find(|(s, _)| *s == bare).map(|(_, f)| *f)
}
```

### 1.2 `node/core.rs` — `NodeCtx`, `HostState`, the `unsafe` boundary, the lazy registry

This is the heart. It holds:

* **`HostState`** — the single `#[derive(Debug)] struct` stored behind Nova's `HostHooks`. It owns
  the microtask queue, the timer heap, the module/builtin caches, and a `RefCell` to the resolver
  and CWD/env. It implements `HostHooks` by delegating job/timer enqueue to `event_loop.rs` and
  module loading to `module_esm.rs`. `get_host_data` returns `self` (verified pattern,
  `host_hooks.rs:187`) so any Rust-backed builtin can recover `HostState` via
  `agent.get_host_data().downcast_ref::<HostState>()` (verified at `globals.rs:312`).
* **The single `unsafe`** — `extend_lifetime` to satisfy `GcAgent::new(&'static dyn HostHooks)`.
  Copied verbatim with the WHY comment and the field-drop-order guarantee (`HostState` boxed and
  declared after the agent inside `JsRuntime`).
* **`NodeCtx`** — a lightweight borrow handed to every `install`. It exposes:
  `resolver(&self) -> &Resolver`, `cwd(&self) -> &Path`, `env(&self) -> &EnvMap`, and
  `builtin_cache(&self)` (a `RefCell<HashMap<&'static str, Global<Object<'static>>>>`). `NodeCtx`
  borrows out of `HostState`; it owns nothing, so it costs zero allocation.
* **The lazy registry mechanism.** Two layers:
  - *Builtin require/import cache*: `module_cjs`/`module_esm` call `ctx.builtin_cache()` first; on
    miss they call `mod::lookup(spec)` to get the `install` fn, run it once, root the result in a
    `Global`, insert, and return it. Result: an unused `node:` module never runs its `install`,
    so its JS object and functions never exist — zero startup cost (tenet 2).
  - *Lazy global accessors*: globals that are "always present but rarely touched" (e.g. the `url`
    constructors, `fetch`) install as **accessor properties** on `globalThis` whose getter, on
    first read, materializes the real value, redefines the property as a plain data property
    (self-replacing getter), and returns it. Accessor descriptors are available
    (`property_descriptor.rs:30` `get`/`set` fields), so this is a pure-safe lazy global with zero
    cost until first touch. `globals.rs` (section 1.4) drives this.
* **`InstallError`** — a small error enum (`Nova(String)`, `Resolve(String)`, `Io(String)`)
  convertible into a thrown Nova exception by the caller; modules return it instead of panicking.

### 1.3 `node/event_loop.rs` — the job + timer pump (shared core)

Owns the queues and the drain algorithm; `HostState`'s `HostHooks` impl forwards to it.

* **Microtask queue**: `RefCell<VecDeque<Job>>`, fed by `enqueue_promise_job`
  (verified hook, `host_hooks.rs:107`). Drained FIFO by `run_microtasks`, modeled on the CLI's
  `run_microtask_queue` (`lib.rs:34`): `while let Some(job) = pop() { job.run(agent, gc)?; }`.
* **Macrotask/generic queue**: `RefCell<Vec<Job>>` via `enqueue_generic_job` (`host_hooks.rs:103`).
* **Timer heap**: a `BinaryHeap<TimerEntry>` keyed by `Reverse(deadline_instant)` plus a monotonic
  id, fed by `enqueue_timeout_job` (verified hook, `host_hooks.rs:111`, which the CLI leaves empty —
  we implement it). `timers.rs` registers JS-level `setTimeout`/`setInterval` that push entries.
* **The pump** — the public API the runtime calls to make async progress:

```rust
/// Drain microtasks, then run due timers (sleeping until the nearest deadline if the
/// queue is otherwise empty), repeating until both queues are empty. Bounded by an
/// optional deadline for server request handling. Returns the count of jobs run.
pub(crate) fn run_until_idle<'gc>(
    agent: &mut Agent,
    state: &HostState,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, ()>;
```

The pump uses `GcAgent::run_job` (verified at `agent.rs:812`) semantics inside the realm: drain all
microtasks first (run-to-completion), then pop the earliest-due timer, `std::thread::sleep` the
delta to its deadline only when no microtask is ready (mirroring the CLI macrotask wait at
`host_hooks.rs:92`), run it, and loop. This is what lets `async`/`await` and `setTimeout` progress —
the current `JsRuntime` has no loop at all (documented gap, `lib.rs:16`).

`JsRuntime::eval` is extended (without changing its signature or return type) to call
`run_until_idle` after `script_evaluation` so a script that schedules microtasks/timers settles
before the completion value is read. Existing synchronous tests are unaffected (empty queues ⇒
immediate return).

### 1.4 `node/globals.rs` — eager + lazy global installation

The single place that wires globals into a realm. Called once from `JsRuntime::new` via
`create_realm`'s `initialize_global_object` hook (verified seam: `Instance::new` passes
`initialize_global_object` to `create_realm`, `lib.rs:139`). It:

* installs **eagerly**: `process`, `Buffer`, `console`, `setTimeout`/`setInterval`/`clearTimeout`/
  `clearInterval`/`setImmediate`, `queueMicrotask`, `structuredClone`, `TextEncoder`/`TextDecoder`,
  `globalThis`/`global` self-reference;
* installs **lazily** (self-replacing accessor getters per 1.2): `URL`, `URLSearchParams`, `fetch`,
  `Headers`/`Request`/`Response`. Each getter delegates to the owning module's `install`.

The shared helper every module reuses for defining a function on an object (lifted from the
verified CLI `create_obj_func`, `globals.rs:193`) lives here and is `pub(crate)`:

```rust
pub(crate) fn define_fn(
    agent: &mut Agent, obj: OrdinaryObject, name: &'static str,
    f: RegularFn, len: u32, gc: NoGcScope,
); // create_builtin_function + try_define_own_property(new_data_descriptor)
```

### 1.5 `node/resolver.rs` — the oxc_resolver seam (shared core)

Wraps `oxc_resolver::Resolver` (`pub type Resolver = ResolverGeneric<FileSystemOs>`, verified
`lib.rs:119`). One resolver instance per `JsRuntime`, built from `ResolveOptions` configured for
Node + TS: `condition_names` (`["node","import","require","default"]`), `extensions`
(`[".js",".mjs",".cjs",".ts",".mts",".cts",".json",".node"]`), `main_fields`/`main_files`,
`exports`/`imports` and `tsconfig` paths support (all first-class in `ResolveOptions`, verified
exports at `lib.rs:73`). Public API:

```rust
pub(crate) struct ModuleResolver { inner: oxc_resolver::Resolver }
impl ModuleResolver {
    pub fn new(cwd: &Path) -> Self;
    /// Classify + resolve. Returns Builtin(spec) for node: / bare builtins,
    /// or File(PathBuf, ModuleType) for everything else, via inner.resolve().
    pub fn resolve(&self, from_dir: &Path, specifier: &str)
        -> Result<Resolved, ResolveError>;
}
pub(crate) enum Resolved { Builtin(&'static str), File(PathBuf, oxc_resolver::ModuleType) }
```

`resolve` first checks `mod::lookup` (builtin short-circuit, no filesystem touch), else delegates to
`inner.resolve(from_dir, specifier)` (verified `Resolver::resolve`, `lib.rs:217`) and reads
`Resolution::module_type` to decide ESM vs CJS. This replaces any hand-rolled resolver entirely.

---

## 2. The per-module split

Every file below is small (single responsibility), exposes the exact
`pub(crate) fn install<'gc>(agent, ctx, gc) -> Result<Object<'gc>, InstallError>` entry, and — unless
listed as an eager global — is built lazily on first require/import. Memory/lazy strategy is noted
per module. Files are disjoint (these become parallel implementation subagents).

See the structured `modules` list for the full table.

---

## 3. Public-surface invariant

`JsRuntime::{eval, eval_with_input}`, `run_macro`, `run_server_fn`, and `transpile_ts` keep their
exact signatures and behavior. The Node layer is additive: `JsRuntime::new` gains a `HostState` +
`initialize_global_object` wiring, and `eval*` gains a trailing `run_until_idle` drain. All 29
existing tests must stay green (`cargo test -p treaty_runtime`).
