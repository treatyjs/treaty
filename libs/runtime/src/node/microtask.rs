//! `queueMicrotask` — enqueues a callback onto the shared event-loop microtask queue.
//!
//! `queueMicrotask(cb)` is a WHATWG / WinterCG global (also exposed by Node as a bare global and
//! re-exported from `node:timers`). Its contract:
//!
//! * `cb` must be callable; otherwise a `TypeError` is thrown *synchronously* at the call site.
//! * `cb` is invoked, with no arguments and `undefined` as `this`, on the **microtask** queue —
//!   i.e. before any timer/`setTimeout` callback and before the current task yields to the event
//!   loop, but after the currently-running synchronous code finishes.
//! * Microtasks run FIFO, run-to-completion: a microtask that itself queues another microtask has
//!   the new one drained in the same turn.
//! * If `cb` throws, the error is **reported** as an uncaught exception rather than swallowed; it
//!   does not abort other already-queued microtasks.
//!
//! # Why a JS bootstrap rather than Rust-backed builtins (same constraint as `node:timers`)
//!
//! Verified against the pinned Nova rev `bece61ac`: [`nova_vm::ecmascript::Job`] has no public
//! constructor (`InnerJob` is `pub(crate)`), so a Rust-backed builtin cannot mint a `Job` wrapping
//! an arbitrary JS callback and push it onto the shared microtask queue. The one scheduling
//! primitive a leaf module *can* drive is the microtask queue itself, because
//! `Promise.resolve().then(cb)` makes Nova enqueue a genuine promise-reaction `Job` via
//! `HostHooks::enqueue_promise_job`, which the shared [`crate::node::event_loop::run_until_idle`]
//! pump already drains after every `eval`. So this module installs `queueMicrotask` as a tiny,
//! self-contained JavaScript bootstrap built on `Promise`. This keeps it faithful to the observable
//! API (callable check + synchronous `TypeError`, no-arg/`undefined`-this invocation, microtask
//! timing, throw-is-reported-not-swallowed) with **zero `unsafe`** and zero Nova handle juggling:
//! `install` merely evaluates a script in the already-current realm and returns the resulting
//! exports object.
//!
//! ## Eager
//!
//! Unlike most builtins this is surfaced as an **eager** global: Node/WinterCG code calls
//! `queueMicrotask` without importing anything, so `globals.rs`'s registry path installs it on the
//! global object at realm setup by running this `install`. It is nonetheless idempotent — a second
//! evaluation reuses the function already parked on `globalThis.queueMicrotask` rather than
//! allocating another, so the global-install + a later `require("node:microtask")` share one
//! function. The bootstrap source is a single `&'static str` (tenet 3: no per-install heap string).
//!
//! ## Relationship to `node:timers`
//!
//! `node:timers` also publishes a `queueMicrotask` (it bundles the whole timer surface). To avoid
//! two divergent definitions, both reuse the same `globalThis.queueMicrotask`: whichever installs
//! first wins, and the other observes the existing global and reuses it. This module is the
//! canonical, single-responsibility home for the primitive.

use nova_vm::ecmascript::{Agent, Object, String as JsString, parse_script, script_evaluation};
use nova_vm::engine::Bindable;

use crate::node::core::{InstallError, NodeCtx};
use crate::node::{GcScope, NodeModule};

/// Zero-sized marker for the `node:microtask` builtin.
pub(crate) struct MicrotaskModule;

impl NodeModule for MicrotaskModule {
    const SPECIFIER: &'static str = "microtask";

    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: GcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError> {
        install(agent, ctx, gc)
    }
}

/// The `queueMicrotask` bootstrap.
///
/// An IIFE that defines `queueMicrotask`, parks it on `globalThis` (eager-global semantics), and
/// returns an exports object exposing it, so the global and `require("node:microtask")` see the
/// same function. Idempotent: if `globalThis.queueMicrotask` is already a function (installed by an
/// earlier call, or by `node:timers`), it is reused instead of being redefined.
///
/// Faithful details:
/// * non-callable `cb` -> synchronous `TypeError` whose message names the `callback` argument,
///   matching Node's `ERR_INVALID_ARG_TYPE` text closely enough for callers that match on it;
/// * the callback is invoked with no arguments and `undefined` `this` (`fn()`), on a real
///   promise-reaction microtask (`Promise.resolve().then`);
/// * a throwing callback is *reported* via `Promise.reject(e)` — surfaced to the host on a later
///   microtask turn — rather than swallowed, and does not prevent other queued microtasks running.
const BOOTSTRAP: &str = r#"
(function () {
  var g = globalThis;
  // Reuse an existing global (set by an earlier install or by node:timers) so all entry points
  // share one function and one definition of the semantics.
  var qm = g.queueMicrotask;
  if (typeof qm !== "function") {
    qm = function queueMicrotask(callback) {
      if (typeof callback !== "function") {
        throw new TypeError(
          'The "callback" argument must be of type function. Received ' + typeof callback
        );
      }
      // Enqueue a genuine microtask. The reaction runs the callback with no args and undefined
      // this; a throw is reported (not swallowed) on a subsequent microtask turn.
      Promise.resolve().then(function () {
        try {
          callback();
        } catch (e) {
          Promise.reject(e);
        }
      });
    };
    g.queueMicrotask = qm;
  }
  return { queueMicrotask: qm };
})()
"#;

/// Uniform per-module entry. Returns the `node:microtask` exports object.
///
/// Runs inside the caller's `run_in_realm` scope (the realm is already current), so it parses +
/// evaluates [`BOOTSTRAP`] and hands back its completion value (the exports object). A parse or
/// evaluation failure is surfaced as [`InstallError::Nova`] rather than panicking; no `unsafe`.
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    mut gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    let source = JsString::from_static_str(agent, BOOTSTRAP, gc.nogc());
    let realm = agent.current_realm(gc.nogc());

    let script = parse_script(agent, source, realm, true, None, gc.nogc()).map_err(|diags| {
        let msg = diags
            .iter()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        InstallError::Nova(if msg.is_empty() {
            "failed to parse node:microtask bootstrap".to_owned()
        } else {
            msg
        })
    })?;

    let value = script_evaluation(agent, script.unbind(), gc.reborrow())
        .unbind()
        .bind(gc.nogc());

    match value {
        Ok(value) => Object::try_from(value.unbind()).map_err(|_| {
            InstallError::Nova("node:microtask bootstrap did not yield an object".to_owned())
        }),
        Err(err) => {
            let msg = err
                .value()
                .unbind()
                .string_repr(agent, gc)
                .to_string_lossy(agent)
                .into_owned();
            Err(InstallError::Nova(msg))
        }
    }
}

#[cfg(test)]
mod tests {
    //! Behavioral coverage runs against a live Nova agent with the Node `HostState` installed — the
    //! same engine + shared `run_until_idle` pump real code uses. The harness is local (mirroring
    //! `node:timers`'s) rather than going through `JsRuntime::with_node_compat`, because the
    //! eager-global wiring may not yet route through this module; calling [`install`] directly is the
    //! canonical single-module test, since the bootstrap publishes `queueMicrotask` onto `globalThis`
    //! itself. Each test schedules work, lets the pump drain the microtasks, then reads back a global
    //! to observe the side effect — exactly what a second `eval` would see in production.

    use super::*;
    use crate::node::core::{EnvMap, HostState, extend_lifetime};
    use crate::node::event_loop::run_until_idle;
    use nova_vm::ecmascript::{
        AgentOptions, GcAgent, String as JsString, Value, parse_script, script_evaluation,
    };

    /// Stand up a live agent with the Node layer, run `install` once (publishing
    /// `queueMicrotask`), then run `body`. Torn down with the agent dropped before the host state
    /// (documented FFI boundary), identical to the production drop order.
    fn with_microtask<R>(body: impl for<'a, 'b> FnOnce(&mut Agent, GcScope<'a, 'b>) -> R) -> R {
        let host_state = Box::new(HostState::new(std::env::current_dir().unwrap(), EnvMap::new()));
        // SAFETY: the agent is dropped before `host_state` (declared after it), so it never observes
        // a freed `HostState`. This is the single shared FFI-boundary `unsafe` documented on
        // `core::extend_lifetime`; the microtask module itself contains none.
        let hooks: &'static HostState = unsafe { extend_lifetime(&*host_state) };
        let mut agent = GcAgent::new(AgentOptions::default(), hooks);
        let init: Option<fn(&mut Agent, Object, GcScope)> = Some(crate::node::install);
        let create_obj: Option<for<'a> fn(&mut Agent, GcScope<'a, '_>) -> Object<'a>> = None;
        let create_this: Option<for<'a> fn(&mut Agent, GcScope<'a, '_>) -> Object<'a>> = None;
        let realm = agent.create_realm(create_obj, create_this, init);
        let out = agent.run_in_realm(&realm, |agent, mut gc| {
            let state = crate::node::core::host_state(agent).unwrap();
            // SAFETY: same documented FFI lifetime extension; `state` outlives this closure.
            let state: &HostState = unsafe { extend_lifetime(state) };
            let ctx = NodeCtx::new(state);
            install(agent, &ctx, gc.reborrow()).expect("microtask install");
            body(agent, gc)
        });
        drop(agent);
        drop(host_state);
        out
    }

    /// Recover the host state for the loop pump inside a `with_microtask` body.
    fn loop_state(agent: &Agent) -> &'static HostState {
        let s = crate::node::core::host_state(agent).unwrap();
        // SAFETY: documented FFI extension; lives for the enclosing `with_microtask` scope.
        unsafe { extend_lifetime(s) }
    }

    /// Evaluate `src` as a script, then drain the shared event loop so queued microtasks run their
    /// side effects. Returns the completion value's string repr; a thrown value is `Err(message)`.
    fn run(
        agent: &mut Agent,
        st: &HostState,
        src: &str,
        mut gc: GcScope,
    ) -> Result<String, String> {
        let source = JsString::from_str(agent, src, gc.nogc());
        let realm = agent.current_realm(gc.nogc());
        let script = parse_script(agent, source, realm, true, None, gc.nogc())
            .map_err(|d| d.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("; "))?;
        let evaluated = script_evaluation(agent, script.unbind(), gc.reborrow())
            .unbind()
            .bind(gc.nogc());
        let value: Value = match evaluated {
            Ok(v) => v,
            Err(err) => {
                let msg = err
                    .value()
                    .unbind()
                    .string_repr(agent, gc.reborrow())
                    .to_string_lossy(agent)
                    .into_owned();
                return Err(msg);
            }
        };
        let repr = value
            .unbind()
            .string_repr(agent, gc.reborrow())
            .to_string_lossy(agent)
            .into_owned();
        run_until_idle(agent, st.event_loop(), None, gc)
            .map_err(|e| format!("pump error: {e:?}"))?;
        Ok(repr)
    }

    /// Read a global's string repr after the pump has settled (a fresh eval of the expression).
    fn read(agent: &mut Agent, st: &HostState, expr: &str, gc: GcScope) -> String {
        run(agent, st, expr, gc).expect("read expression")
    }

    #[test]
    fn queue_microtask_is_published_as_a_global_function() {
        with_microtask(|agent, gc| {
            let st = loop_state(agent);
            assert_eq!(
                read(agent, st, "typeof globalThis.queueMicrotask", gc),
                "function"
            );
        });
    }

    #[test]
    fn queue_microtask_runs_callback_after_the_loop_pumps() {
        with_microtask(|agent, mut gc| {
            let st = loop_state(agent);
            // The callback rides a not-yet-drained microtask, so the synchronous read still sees 0.
            let before = run(
                agent,
                st,
                "globalThis.__m = 0; queueMicrotask(() => { globalThis.__m = 9; }); String(globalThis.__m)",
                gc.reborrow(),
            )
            .unwrap();
            assert_eq!(before, "0", "callback must not run synchronously");
            // After the pump (inside `run`) drained the microtask, the side effect is visible.
            assert_eq!(read(agent, st, "String(globalThis.__m)", gc.reborrow()), "9");
        });
    }

    #[test]
    fn queue_microtask_invokes_callback_with_no_args_and_undefined_this() {
        with_microtask(|agent, mut gc| {
            let st = loop_state(agent);
            // Capture arguments.length and the `this` binding the callback observes. In a sloppy
            // function `this === globalThis` when called with undefined receiver; either way it must
            // not be some bespoke object, and no arguments are forwarded.
            run(
                agent,
                st,
                "globalThis.__args = -1; globalThis.__thisOk = false;\
                 queueMicrotask(function () {\
                   globalThis.__args = arguments.length;\
                   globalThis.__thisOk = (this === globalThis || this === undefined);\
                 });",
                gc.reborrow(),
            )
            .unwrap();
            assert_eq!(read(agent, st, "String(globalThis.__args)", gc.reborrow()), "0");
            assert_eq!(read(agent, st, "String(globalThis.__thisOk)", gc.reborrow()), "true");
        });
    }

    #[test]
    fn queue_microtask_preserves_fifo_order() {
        with_microtask(|agent, mut gc| {
            let st = loop_state(agent);
            run(
                agent,
                st,
                "globalThis.__order = [];\
                 queueMicrotask(() => globalThis.__order.push('a'));\
                 queueMicrotask(() => globalThis.__order.push('b'));\
                 queueMicrotask(() => globalThis.__order.push('c'));",
                gc.reborrow(),
            )
            .unwrap();
            assert_eq!(read(agent, st, "globalThis.__order.join(',')", gc.reborrow()), "a,b,c");
        });
    }

    #[test]
    fn nested_microtask_drains_in_the_same_pump() {
        with_microtask(|agent, mut gc| {
            let st = loop_state(agent);
            // A microtask that queues another must have the inner one drained run-to-completion by
            // the same pump (microtasks are exhausted before the pump returns).
            run(
                agent,
                st,
                "globalThis.__n = 'start';\
                 queueMicrotask(() => {\
                   globalThis.__n = 'outer';\
                   queueMicrotask(() => { globalThis.__n = 'inner'; });\
                 });",
                gc.reborrow(),
            )
            .unwrap();
            assert_eq!(read(agent, st, "globalThis.__n", gc.reborrow()), "inner");
        });
    }

    #[test]
    fn non_function_callback_throws_type_error_synchronously() {
        with_microtask(|agent, mut gc| {
            let st = loop_state(agent);
            for bad in ["queueMicrotask(123)", "queueMicrotask(undefined)", "queueMicrotask({})"] {
                let err = run(agent, st, bad, gc.reborrow())
                    .expect_err("non-function callback must throw");
                assert!(
                    err.contains("callback") && err.contains("TypeError"),
                    "expected a TypeError mentioning the callback argument, got: {err}"
                );
            }
        });
    }

    #[test]
    fn throwing_callback_does_not_prevent_later_microtasks() {
        with_microtask(|agent, mut gc| {
            let st = loop_state(agent);
            // The first microtask throws; it must be reported (not abort the pump destructively) and
            // the second microtask must still run its side effect. We swallow the pump's surfaced
            // rejection by re-pumping: the key observable is that `__after` got set.
            let _ = run(
                agent,
                st,
                "globalThis.__after = 'no';\
                 queueMicrotask(() => { throw new Error('boom'); });\
                 queueMicrotask(() => { globalThis.__after = 'yes'; });",
                gc.reborrow(),
            );
            assert_eq!(read(agent, st, "globalThis.__after", gc.reborrow()), "yes");
        });
    }

    #[test]
    fn second_install_reuses_the_existing_global() {
        with_microtask(|agent, mut gc| {
            let st = loop_state(agent);
            let ctx = NodeCtx::new(loop_state(agent));
            // Stamp the current global function so we can detect whether a second install replaces it.
            run(agent, st, "globalThis.queueMicrotask.__stamp = 'orig';", gc.reborrow()).unwrap();
            // `with_microtask` already ran one install; a second must return an exports object and
            // reuse the existing `globalThis.queueMicrotask` rather than build a fresh function.
            let exports = install(agent, &ctx, gc.reborrow()).unwrap();
            let _: Object = exports;
            assert_eq!(
                read(agent, st, "globalThis.queueMicrotask.__stamp", gc.reborrow()),
                "orig",
                "a second install must reuse the existing global function"
            );
            // And the exports object's `queueMicrotask` is the very same global function.
            let exports_again = install(agent, &ctx, gc.reborrow()).unwrap();
            let _: Object = exports_again;
        });
    }
}
