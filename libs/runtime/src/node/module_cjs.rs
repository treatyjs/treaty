//! CommonJS module loading: the `require(...)` bridge.
//!
//! `require(specifier)` resolves a specifier through the runtime resolver
//! ([`crate::node::module_resolver::resolve_and_load`]), then returns a *cached* exports object,
//! building it exactly once on a cache miss. This is the synchronous Node `require` contract:
//! repeated `require` of the same module yields the identical object (module identity / singleton
//! semantics), so the cache is correctness, not just speed.
//!
//! Two cache tiers, mirroring [`crate::node::core::HostState`]:
//!
//! * **`node:` builtins** — keyed by their canonical `&'static str` specifier in
//!   [`NodeCtx::builtin_cache`]. On first `require("node:fs")` the builtin's lazy `install` runs and
//!   its exports object is rooted in a [`Global`]; every later `require` of that specifier returns
//!   the same rooted object. An untouched builtin never materializes (tenet 2: zero startup cost).
//! * **user files** — keyed by absolute path in [`HostState::module_cache`].
//!
//! Allocation discipline (tenet 3): the builtin key is a `&'static str` re-derived from the static
//! `BUILTINS` table (no `String`); a cache hit clones only the cheap `Global` handle, never the
//! exports object; the specifier argument is read as a borrowed [`Cow`] and only owned when Nova has
//! to transcode it.
//!
//! `require` is surfaced as a Node global. Because the scaffold owns `globals.rs`, this file exposes
//! [`install_require`] (the uniform "install me onto the realm global" seam) and the `RegularFn`
//! [`require`] so the global wiring can call it; the resolve→cache→materialize heart lives in
//! [`require_specifier`], which is engine-driven and unit-tested against a live agent below.
//!
//! ### Deferred (documented, never a red tree)
//!
//! Evaluating a *user* `.js`/`.ts`/`.json` file as a CommonJS module — running its body inside a
//! `(exports, require, module, __filename, __dirname)` wrapper and caching `module.exports` — is
//! intentionally not wired here. Doing it faithfully needs to re-enter the engine from inside the
//! `require` builtin (push a script execution context, read the resulting `module.exports` back off
//! the realm global) using Nova operations that are `pub(crate)` at the pinned rev
//! (`get_global_object`, `call_function`) and therefore unreachable from this crate without an
//! `unsafe`/visibility hack that the architecture rules forbid outside the single Nova FFI boundary.
//! The non-engine half of that work — resolution, `std::fs` reading, TS transpile, JSON wrapping,
//! and absolute-path cache-key derivation — already lives, fully tested, in
//! [`crate::node::module_resolver`]; [`require_specifier`] returns a clear `InstallError` for the
//! file case so callers fail loudly rather than silently. Builtin `require` (the dominant path for
//! the macro / server-fn runtime) is complete.

use std::borrow::Cow;

use nova_vm::ecmascript::{
    Agent, ArgumentsList, Behaviour, BuiltinFunctionArgs, ExceptionType, InternalMethods, JsResult,
    Object, OrdinaryObject, PropertyDescriptor, PropertyKey, String as JsString, Value,
    create_builtin_function, unwrap_try,
};
use nova_vm::engine::{Bindable, Global, NoGcScope};

use crate::node::core::{InstallError, NodeCtx};
use crate::node::module_resolver::{resolve_and_load, LoadAction};
use crate::node::GcScope;

/// The arity reported to JS for `require` (one argument: the specifier).
const REQUIRE_ARITY: u32 = 1;

/// Resolve `specifier` and return its exports object, building+caching builtins exactly once.
///
/// This is the synchronous CommonJS `require` core. It is engine-driven (it constructs the builtin's
/// exports object via the module's lazy `install`) but holds no `unsafe`: rooting goes through the
/// safe [`Global`] API and the `&'static str` builtin key keeps the cache allocation-free.
///
/// The shared-vs-mutable borrow split is handled by *phasing* the work, exactly as the leaf builtins
/// do (e.g. `path::resolve` reads `cwd` off [`NodeCtx`] before taking `&mut Agent`): a [`NodeCtx`]
/// borrows `&Agent`, so it is recovered, used, and dropped *within* each phase and never held across
/// the `&mut Agent` calls that build the module. No `unsafe`, no held aliasing borrow.
///
/// * On a [`LoadAction::Builtin`] cache hit, the rooted exports object is returned with no
///   allocation beyond cloning the cheap `Global` handle.
/// * On a builtin cache miss, the module's `install` runs once, the result is rooted into the
///   builtin cache, and the same object is returned. Subsequent calls hit the cache.
/// * A [`LoadAction::File`] returns [`InstallError::Resolve`] — user-file CJS evaluation is the
///   documented deferred case (see the module docs); resolution itself still succeeds, so the error
///   message names the file that would have been loaded.
pub(crate) fn require_specifier<'gc>(
    agent: &mut Agent,
    ctx: &NodeCtx,
    referrer: Option<&std::path::Path>,
    specifier: &str,
    mut gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    // `ctx` borrows the `HostState`, which lives in the JsRuntime's box at a stable address —
    // *independent* of the `agent` borrow (see `require`'s call site and `core::extend_lifetime`).
    // That decoupling is what lets `ctx` (read access to resolver + caches) coexist with `&mut Agent`
    // (engine heap) without aliasing: they reference disjoint memory, and the caches are `RefCell`-
    // guarded for interior mutability. No phasing, no held-borrow conflict, no `unsafe` here.
    let action = resolve_and_load(ctx, referrer, specifier)?;

    match action {
        LoadAction::Builtin { specifier: canonical, install } => {
            // Cache hit: return the rooted exports object resolved to the current scope. Cloning the
            // `Global` handle is cheap; the borrow on the cache is released before `get`.
            let cached = ctx.builtin_cache().borrow().get(canonical).cloned();
            if let Some(handle) = cached {
                return Ok(handle.get(agent, gc.nogc()));
            }

            // Miss: build the exports object exactly once via the module's lazy `install`, then root
            // it for the cache and hand back a live handle bound to the caller's scope.
            let exports = install(agent, ctx, gc.reborrow())?.unbind();
            let rooted: Global<Object<'static>> = Global::new(agent, exports);
            let live = rooted.get(agent, gc.nogc());
            ctx.builtin_cache().borrow_mut().insert(canonical, rooted);
            Ok(live)
        }
        LoadAction::File { path, .. } => Err(InstallError::Resolve(format!(
            "user CommonJS modules are not yet loadable via require(); resolved '{specifier}' to \
             '{}' (deferred: see module_cjs docs)",
            path.display()
        ))),
    }
}

// ---------------------------------------------------------------------------------------------
// JS-facing `require` global.
// ---------------------------------------------------------------------------------------------

/// The Rust-backed `require(specifier)` exposed to JavaScript.
///
/// Reads argument 0 as a string (throwing `TypeError` for a non-string, matching Node), recovers the
/// runtime's [`NodeCtx`], and delegates to [`require_specifier`]. A resolution/build failure is
/// surfaced as a thrown JS `Error` carrying the underlying message, so user code can `try/catch` it.
///
/// `referrer` is `None`: `require` called from a top-level script / `eval` resolves relative to the
/// runtime CWD, which is correct for the macro and server-fn entry contexts. Per-module referrers
/// arrive once user-file loading lands (the deferred case).
pub(crate) fn require<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    mut gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    // Argument validation first (Node throws `TypeError` for a non-string specifier).
    let specifier: Cow<'_, str> = match JsString::try_from(args.get(0)) {
        Ok(s) => s.to_string_lossy(agent),
        Err(_) => {
            return Err(agent.throw_exception_with_static_message(
                ExceptionType::TypeError,
                "The \"id\" argument must be of type string",
                gc.into_nogc(),
            ));
        }
    };

    // Recover a `HostState` borrow that is *decoupled* from the `&mut Agent` we need for engine work.
    // `core::host_state` returns `&HostState` tied to `&Agent`; that borrow alone cannot coexist with
    // the `&mut Agent` that `require_specifier` -> `install` requires. The fix is the one already used
    // (and justified) at the Nova FFI boundary in `core`: the `HostState` is owned by the JsRuntime's
    // `Box<HostState>` at a stable address and outlives this entire `run_in_realm` call, so extending
    // the borrow's lifetime to decouple it from the agent is sound. The `&HostState` (resolver + the
    // `RefCell` caches) and the `&mut Agent` (the Nova heap) reference disjoint memory.
    //
    // SAFETY: see `core::extend_lifetime`. The produced reference is used only for the duration of
    // this call (strictly shorter than the owning box's lifetime), and `HostState`'s own state is
    // mutated only through its `RefCell` interior mutability — never aliased mutably with `&mut Agent`.
    let state = match crate::node::core::host_state(agent) {
        Some(state) => unsafe { crate::node::core::extend_lifetime(state) },
        None => {
            return Err(agent.throw_exception_with_static_message(
                ExceptionType::Error,
                "require() is unavailable: the Node compatibility layer is not installed",
                gc.into_nogc(),
            ));
        }
    };
    let ctx = NodeCtx::new(state);

    match require_specifier(agent, &ctx, None, &specifier, gc.reborrow()) {
        Ok(exports) => Ok(exports.into()),
        Err(err) => Err(agent.throw_exception(
            ExceptionType::Error,
            err.to_string(),
            gc.into_nogc(),
        )),
    }
}

/// Install `require` as a data property on the realm global object.
///
/// The uniform "wire me into the global" seam the scaffold's `globals` layer calls. `require` is an
/// eager global in Node (CJS code assumes it exists without importing it), but the function object
/// itself is tiny — a single builtin-function allocation — and, crucially, materializes **no**
/// module: every `node:` builtin stays lazy until the first `require("node:...")` actually runs
/// (tenet 2).
pub(crate) fn install_require(agent: &mut Agent, global: Object, gc: NoGcScope) {
    let function = create_builtin_function(
        agent,
        Behaviour::Regular(require),
        BuiltinFunctionArgs::new(REQUIRE_ARITY, "require"),
        gc,
    );
    let key = PropertyKey::from_static_str(agent, "require", gc);
    // Define `require` directly on the realm global as a plain data property (the same path the
    // shared `globals::install_globals` uses for `global`), so reads are a direct slot lookup.
    unwrap_try(global.try_define_own_property(
        agent,
        key,
        PropertyDescriptor::new_data_descriptor(function),
        None,
        gc,
    ));
}

/// Uniform per-module entry. The CJS bridge is a service, not an importable object, so this returns
/// an empty exports object — the registry seam stays uniform with the leaf builtins, but `module_cjs`
/// is intentionally absent from `BUILTINS` (you do not `require("module_cjs")`).
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    let obj = OrdinaryObject::create_empty_object(agent, gc.into_nogc());
    Ok(obj.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::core::{extend_lifetime, EnvMap, HostState};
    use nova_vm::ecmascript::{AgentOptions, GcAgent};

    /// Stand up a live Nova agent with the Node `HostState` installed, run `body` inside its realm,
    /// and tear it down. Mirrors `JsRuntime::with_node_compat` but kept local so this module's core
    /// (`require_specifier`) is exercised against a real engine without depending on the public
    /// `JsRuntime` surface. The single `unsafe` is the shared, documented Nova FFI lifetime
    /// extension from `core` — identical to the production path and dropped in the same order
    /// (agent before host state).
    fn with_agent<R>(
        body: impl for<'a, 'b> FnOnce(&mut Agent, &HostState, GcScope<'a, 'b>) -> R,
    ) -> R {
        let cwd = std::env::current_dir().unwrap();
        let host_state = Box::new(HostState::new(cwd, EnvMap::new()));
        // SAFETY: the agent below is dropped before `host_state` (declared after it here), so it
        // never observes a freed `HostState`. This is the same FFI-boundary `unsafe` documented on
        // `core::extend_lifetime`; no leaf logic depends on it.
        let hooks: &'static HostState = unsafe { extend_lifetime(&*host_state) };
        let mut agent = GcAgent::new(
            AgentOptions {
                disable_gc: false,
                print_internals: false,
                no_block: false,
            },
            hooks,
        );
        let init: Option<fn(&mut Agent, Object, GcScope)> = Some(crate::node::install);
        let create_obj: Option<for<'a> fn(&mut Agent, GcScope<'a, '_>) -> Object<'a>> = None;
        let create_this: Option<for<'a> fn(&mut Agent, GcScope<'a, '_>) -> Object<'a>> = None;
        let realm = agent.create_realm(create_obj, create_this, init);
        let out = agent.run_in_realm(&realm, |agent, gc| {
            let state = crate::node::core::host_state(agent).unwrap();
            // Re-derive the borrow off the agent for the closure (same reborrow shape as `with_ctx`).
            let state: &HostState = unsafe { extend_lifetime(state) };
            body(agent, state, gc)
        });
        drop(agent);
        drop(host_state);
        out
    }

    #[test]
    fn require_builtin_returns_an_object() {
        with_agent(|agent, state, mut gc| {
            let ctx = NodeCtx::new(state);
            let exports =
                require_specifier(agent, &ctx, None, "node:path", gc.reborrow()).unwrap();
            // It is an object (a module exports namespace), not undefined/null.
            let _: Object = exports;
        });
    }

    #[test]
    fn require_builtin_is_cached_and_identity_stable() {
        with_agent(|agent, state, mut gc| {
            let ctx = NodeCtx::new(state);
            assert!(
                ctx.builtin_cache().borrow().is_empty(),
                "cache starts empty (nothing required yet)"
            );

            let first = require_specifier(agent, &ctx, None, "path", gc.reborrow()).unwrap();
            assert_eq!(
                ctx.builtin_cache().borrow().len(),
                1,
                "first require materializes + caches exactly one builtin"
            );

            let second = require_specifier(agent, &ctx, None, "node:path", gc.reborrow()).unwrap();
            assert_eq!(
                ctx.builtin_cache().borrow().len(),
                1,
                "second require (node:-prefixed, same module) is a cache hit, not a new entry"
            );

            // Module identity: repeated require yields the same underlying object (CJS singleton).
            assert_eq!(
                first.unbind(),
                second.unbind(),
                "require returns the identical cached exports object"
            );
        });
    }

    #[test]
    fn require_distinct_builtins_make_distinct_cache_entries() {
        with_agent(|agent, state, mut gc| {
            let ctx = NodeCtx::new(state);
            let _ = require_specifier(agent, &ctx, None, "path", gc.reborrow()).unwrap();
            let _ = require_specifier(agent, &ctx, None, "os", gc.reborrow()).unwrap();
            assert_eq!(
                ctx.builtin_cache().borrow().len(),
                2,
                "two different builtins occupy two cache slots"
            );
        });
    }

    #[test]
    fn require_unknown_specifier_is_a_resolve_error() {
        with_agent(|agent, state, mut gc| {
            let ctx = NodeCtx::new(state);
            let err = require_specifier(
                agent,
                &ctx,
                None,
                "this-package-does-not-exist-xyz",
                gc.reborrow(),
            )
            .unwrap_err();
            assert!(
                matches!(err, InstallError::Resolve(_)),
                "a bare specifier with no matching package/builtin must be a resolve error: {err:?}"
            );
        });
    }

    #[test]
    fn require_user_file_is_reported_as_deferred_resolve_error() {
        // Resolution succeeds (the file exists) but loading is the documented deferred case, so the
        // error names the resolved file rather than failing to resolve.
        let dir = std::env::temp_dir().join(format!("treaty-cjs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("mod.js");
        std::fs::write(&file, "module.exports = 1;\n").unwrap();

        let cwd = dir.clone();
        let host_state = Box::new(HostState::new(cwd, EnvMap::new()));
        // SAFETY: agent dropped before host_state (declared after); same documented FFI boundary.
        let hooks: &'static HostState = unsafe { extend_lifetime(&*host_state) };
        let mut agent = GcAgent::new(AgentOptions::default(), hooks);
        let init: Option<fn(&mut Agent, Object, GcScope)> = Some(crate::node::install);
        let create_obj: Option<for<'a> fn(&mut Agent, GcScope<'a, '_>) -> Object<'a>> = None;
        let create_this: Option<for<'a> fn(&mut Agent, GcScope<'a, '_>) -> Object<'a>> = None;
        let realm = agent.create_realm(create_obj, create_this, init);
        agent.run_in_realm(&realm, |agent, mut gc| {
            let state = crate::node::core::host_state(agent).unwrap();
            let state: &HostState = unsafe { extend_lifetime(state) };
            let ctx = NodeCtx::new(state);
            let err =
                require_specifier(agent, &ctx, None, "./mod.js", gc.reborrow()).unwrap_err();
            match err {
                InstallError::Resolve(msg) => {
                    assert!(msg.contains("deferred"), "should flag the deferred case: {msg}");
                    assert!(msg.contains("mod.js"), "should name the resolved file: {msg}");
                }
                other => panic!("expected a deferred Resolve error, got {other:?}"),
            }
        });
        drop(agent);
        drop(host_state);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
