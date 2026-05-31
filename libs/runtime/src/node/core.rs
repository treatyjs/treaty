//! Shared core for the Node-compat layer: the host state behind Nova's [`HostHooks`], the single
//! localized `unsafe` FFI lifetime extension, the lazy builtin registry, and the [`NodeCtx`] borrow
//! handed to every module's `install`.
//!
//! Everything here is allocation-conscious (tenet 3): the builtin cache and module cache start
//! empty and only grow as modules are actually touched; `NodeCtx` owns nothing and borrows out of
//! [`HostState`]; fixed strings are `&'static`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use nova_vm::ecmascript::{
    Agent, GraphLoadingStateRecord, HostDefined, HostHooks, Job, ModuleRequest, Object, Promise,
    PromiseRejectionTrackerOperation, Referrer,
};
use nova_vm::engine::{Global, NoGcScope};

use crate::node::event_loop::EventLoop;
use crate::node::resolver::ModuleResolver;

/// The environment variable map exposed to `process.env` and consulted by the resolver.
///
/// A plain `String -> String` map captured once at runtime construction. Stored behind the
/// [`HostState`] so both `process.env` reads and resolver condition logic see the same values.
pub(crate) type EnvMap = HashMap<String, String>;

/// An error raised while building a builtin module's exports object.
///
/// Modules return this instead of panicking; the registry caller converts it into a thrown Nova
/// exception (or, at the `JsRuntime` boundary, a [`crate::RuntimeError`]). Variants are kept coarse
/// on purpose — the carried `String` holds the human-readable detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InstallError {
    /// A Nova-level failure while constructing the module object (e.g. a thrown intrinsic).
    Nova(String),
    /// Module resolution failed (bad specifier, file not found, bad `package.json`).
    Resolve(String),
    /// An IO failure while loading a file-backed module.
    Io(String),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::Nova(m) => write!(f, "module build error: {m}"),
            InstallError::Resolve(m) => write!(f, "module resolve error: {m}"),
            InstallError::Io(m) => write!(f, "module io error: {m}"),
        }
    }
}

impl std::error::Error for InstallError {}

/// The single host state stored behind Nova's [`HostHooks`].
///
/// It owns the [`EventLoop`] (microtask + timer queues), the resolver, the CWD/env, and the two
/// lazy caches:
///
/// * `builtin_cache` — `specifier -> rooted exports object`, populated on first `require`/`import`
///   of a `node:` builtin so each builtin's `install` runs at most once.
/// * `module_cache` — `absolute path -> rooted module record / exports`, populated as user files
///   are loaded.
///
/// All interior state is wrapped in [`RefCell`] because `HostHooks` methods take `&self` while
/// needing to mutate the queues/caches; the runtime is single-threaded so there is no contention.
///
/// `HostState` implements [`HostHooks`] by forwarding job/timer enqueue to the [`EventLoop`] and
/// module loading to [`crate::node::module_esm`]. [`HostHooks::get_host_data`] returns `self`, so any
/// Rust-backed builtin can recover it via `agent.get_host_data().downcast_ref::<HostState>()`.
#[derive(Debug)]
pub(crate) struct HostState {
    /// The microtask + timer pump. Shared core, not a stub.
    event_loop: EventLoop,
    /// The module resolver, built once per runtime (`oxc_resolver`-backed).
    resolver: ModuleResolver,
    /// The current working directory used as the default resolution base.
    cwd: PathBuf,
    /// The captured process environment (`process.env`, resolver conditions).
    env: EnvMap,
    /// Lazy cache of materialized builtin exports objects, keyed by canonical bare specifier.
    builtin_cache: RefCell<HashMap<&'static str, Global<Object<'static>>>>,
    /// Lazy cache of loaded user modules, keyed by absolute resolved path.
    module_cache: RefCell<HashMap<PathBuf, Global<Object<'static>>>>,
}

impl HostState {
    /// Build a fresh host state rooted at `cwd`, capturing the current process environment.
    pub(crate) fn new(cwd: PathBuf, env: EnvMap) -> Self {
        let resolver = ModuleResolver::new(&cwd);
        Self {
            event_loop: EventLoop::new(),
            resolver,
            cwd,
            env,
            builtin_cache: RefCell::new(HashMap::new()),
            module_cache: RefCell::new(HashMap::new()),
        }
    }

    /// Borrow the event loop (microtask + timer queues and the drain pump).
    pub(crate) fn event_loop(&self) -> &EventLoop {
        &self.event_loop
    }

    /// Borrow the module resolver.
    pub(crate) fn resolver(&self) -> &ModuleResolver {
        &self.resolver
    }

    /// The runtime's current working directory (default resolution base).
    pub(crate) fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// The captured environment map.
    pub(crate) fn env(&self) -> &EnvMap {
        &self.env
    }

    /// The lazy builtin-exports cache. `module_cjs`/`module_esm` consult this before running a
    /// builtin's `install`.
    pub(crate) fn builtin_cache(&self) -> &RefCell<HashMap<&'static str, Global<Object<'static>>>> {
        &self.builtin_cache
    }

    /// The lazy user-module cache, keyed by absolute resolved path.
    pub(crate) fn module_cache(&self) -> &RefCell<HashMap<PathBuf, Global<Object<'static>>>> {
        &self.module_cache
    }
}

impl HostHooks for HostState {
    fn enqueue_generic_job(&self, job: Job) {
        self.event_loop.enqueue_generic(job);
    }

    fn enqueue_promise_job(&self, job: Job) {
        self.event_loop.enqueue_microtask(job);
    }

    fn enqueue_timeout_job(&self, timeout_job: Job, milliseconds: u64) {
        self.event_loop.enqueue_timeout(timeout_job, milliseconds);
    }

    fn promise_rejection_tracker(
        &self,
        _promise: Promise,
        _operation: PromiseRejectionTrackerOperation,
    ) {
        // Unhandled-rejection tracking is a documented follow-up; the default no-op is spec-legal.
    }

    fn load_imported_module<'gc>(
        &self,
        agent: &mut Agent,
        referrer: Referrer<'gc>,
        module_request: ModuleRequest<'gc>,
        host_defined: Option<HostDefined>,
        payload: &mut GraphLoadingStateRecord<'gc>,
        gc: NoGcScope<'gc, '_>,
    ) {
        // ESM graph loading is owned by `module_esm`, which resolves via `self.resolver()` and reads
        // source through `std::fs`. Kept here as the single forwarding seam.
        crate::node::module_esm::load_imported_module(
            self,
            agent,
            referrer,
            module_request,
            host_defined,
            payload,
            gc,
        );
    }

    fn get_host_data(&self) -> &dyn std::any::Any {
        self
    }
}

/// Recover the [`HostState`] from a running agent.
///
/// Every Rust-backed builtin function uses this to reach the shared services (event loop, resolver,
/// env, caches). Returns `None` when the runtime was created without the Node layer (i.e. a plain
/// [`crate::JsRuntime::new`] using Nova's `DefaultHostHooks`), so callers degrade gracefully rather
/// than panicking.
pub(crate) fn host_state<'a>(agent: &'a Agent) -> Option<&'a HostState> {
    agent.get_host_data().downcast_ref::<HostState>()
}

/// A lightweight borrow of the shared services, handed to every module's `install`.
///
/// `NodeCtx` owns nothing — it is a bundle of references out of [`HostState`], so constructing one
/// costs zero allocation. Modules read the resolver / cwd / env through it and never touch
/// [`HostState`] directly, keeping the install signature uniform and the dependency surface small.
pub(crate) struct NodeCtx<'a> {
    state: &'a HostState,
}

impl<'a> NodeCtx<'a> {
    /// Wrap a [`HostState`] borrow.
    pub(crate) fn new(state: &'a HostState) -> Self {
        Self { state }
    }

    /// Recover a [`NodeCtx`] for the agent's installed [`HostState`], if any.
    pub(crate) fn from_agent(agent: &'a Agent) -> Option<Self> {
        host_state(agent).map(NodeCtx::new)
    }

    /// The module resolver.
    pub(crate) fn resolver(&self) -> &ModuleResolver {
        self.state.resolver()
    }

    /// The current working directory.
    pub(crate) fn cwd(&self) -> &Path {
        self.state.cwd()
    }

    /// The environment map (`process.env`).
    pub(crate) fn env(&self) -> &EnvMap {
        self.state.env()
    }

    /// The event loop, for modules (e.g. timers) that enqueue work.
    pub(crate) fn event_loop(&self) -> &EventLoop {
        self.state.event_loop()
    }

    /// The lazy builtin-exports cache.
    pub(crate) fn builtin_cache(
        &self,
    ) -> &RefCell<HashMap<&'static str, Global<Object<'static>>>> {
        self.state.builtin_cache()
    }

    /// Borrow the underlying [`HostState`] for the rare module that needs more than the above.
    pub(crate) fn state(&self) -> &HostState {
        self.state
    }
}

/// # Safety
///
/// Extends a borrow to `'static` so it satisfies `GcAgent::new(&'static dyn HostHooks)`.
///
/// WHY this `unsafe` is unavoidable and why it is sound here: Nova's [`nova_vm::ecmascript::GcAgent`]
/// stores its host hooks as a `&'static dyn HostHooks`, but our [`HostState`] lives inside the
/// owning [`crate::JsRuntime`], not in static memory. The Nova CLI reference solves this identically
/// with one `extend_lifetime` (`nova_cli/src/lib/lib.rs:111`). Soundness rests on a documented
/// drop order: [`crate::JsRuntime`] boxes the [`HostState`] and declares that box **after** the
/// agent, so Rust's declaration-order field drop runs the agent's destructor first — the agent
/// never observes a freed `HostState`. This is the ONLY `unsafe` in the entire Node layer; no leaf
/// module contains any.
#[allow(clippy::needless_lifetimes)]
pub(crate) unsafe fn extend_lifetime<'a, 'b, T>(reference: &'a T) -> &'b T {
    // SAFETY: the caller (JsRuntime) guarantees `reference` outlives every use via field drop order.
    unsafe { &*std::ptr::from_ref(reference) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_state_starts_with_empty_caches() {
        let state = HostState::new(std::env::current_dir().unwrap(), EnvMap::new());
        assert!(state.builtin_cache().borrow().is_empty());
        assert!(state.module_cache().borrow().is_empty());
    }

    #[test]
    fn node_ctx_exposes_cwd_and_env() {
        let cwd = std::env::current_dir().unwrap();
        let mut env = EnvMap::new();
        env.insert("TREATY_TEST".to_owned(), "1".to_owned());
        let state = HostState::new(cwd.clone(), env);
        let ctx = NodeCtx::new(&state);
        assert_eq!(ctx.cwd(), cwd.as_path());
        assert_eq!(ctx.env().get("TREATY_TEST").map(String::as_str), Some("1"));
    }

    #[test]
    fn install_error_displays_each_variant() {
        assert!(InstallError::Nova("x".into()).to_string().contains("build"));
        assert!(InstallError::Resolve("x".into()).to_string().contains("resolve"));
        assert!(InstallError::Io("x".into()).to_string().contains("io"));
    }
}
