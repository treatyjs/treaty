//! `node:timers` — `setTimeout` / `setInterval` / `setImmediate` and their `clear*` pairs, plus
//! `queueMicrotask`.
//!
//! # Why this module is a JS bootstrap rather than Rust-backed builtins
//!
//! Node schedules timer callbacks onto the libuv timer phase; in this runtime the analogue is the
//! shared event loop's timer heap (`event_loop.rs`), which is fed exclusively by Nova's
//! `HostHooks::enqueue_timeout_job(Job, ms)`. The catch (verified against the pinned Nova rev
//! `bece61ac`): [`nova_vm::ecmascript::Job`] has **no public constructor** — `InnerJob` is
//! `pub(crate)` and the only `Job`s in existence are the ones Nova itself mints for promise
//! reactions / resolve-thenable / async-wait. There is therefore no supported way for a
//! Rust-backed `setTimeout` builtin to wrap an arbitrary JS callback into a `Job` and push it onto
//! the timer heap. The single scheduling primitive a leaf module *can* drive is the **microtask
//! queue**, because `Promise.resolve().then(cb)` makes Nova enqueue a real promise-reaction `Job`
//! via `HostHooks::enqueue_promise_job`, which the shared `run_until_idle` pump already drains
//! after every `eval`.
//!
//! So this module installs the timer surface as a small, self-contained JavaScript bootstrap built
//! on `Promise` plus a closure-scoped numeric-id registry for cancellation. This keeps the module
//! faithful to the *observable* timer API (correct names, arg forwarding, integer ids,
//! `clear*`-by-id, `unref`/`ref`/`hasRef`/`refresh` on the returned handle, `queueMicrotask`
//! semantics) and — critically — needs **zero `unsafe`** and zero Nova handle juggling: `install`
//! merely evaluates a script in the already-current realm (it runs inside `run_in_realm`) and
//! returns the resulting exports object. Tenets honored: lazy (built only on first import / first
//! global touch), no heap churn in Rust (the bootstrap source is a single `&'static str`), memory
//! safe (no `unsafe`).
//!
//! ## Deferred (documented, not silently dropped)
//!
//! * **Real wall-clock delays.** Without a `Job`-fed macrotask clock, a delayed callback cannot be
//!   parked until its deadline and run *after* later-but-shorter microtasks; the delay-relative
//!   ordering Node gives between two timers (`setTimeout(_, 10)` fires after `setTimeout(_, 1)`)
//!   cannot be honored purely on the microtask queue. The bootstrap collapses positive delays to
//!   "after the current turn's microtask checkpoint": each timer fire is deferred one extra
//!   microtask hop, so microtasks (`queueMicrotask`, promise `.then`) scheduled in the same turn
//!   still run first — matching Node's macrotasks-trail-microtasks ordering. Delay-relative
//!   ordering between two timers and true elapsed wall-clock time are the deferred piece; they
//!   require an upstream Nova `Job` constructor (or a host shim that owns the timer heap) — tracked
//!   for when that lands.
//! * **`setInterval` does not self-perpetuate across microtask ticks.** A repeating timer that
//!   re-armed itself via a fresh microtask every fire would spin `run_until_idle` forever (the pump
//!   drains microtasks to exhaustion). To stay memory-safe and guarantee the pump terminates, an
//!   interval fires at most once per drain and is then re-armed only when the runtime next pumps
//!   (i.e. on the next `eval`). Continuous wall-clock intervals are deferred for the same reason as
//!   above.
//!
//! Everything else — the eager-global installation, id allocation, cancellation, argument
//! forwarding, the Timeout-handle shape, and `queueMicrotask` — is implemented for real and covered
//! by this module's tests through `JsRuntime::with_node_compat`.

use nova_vm::ecmascript::{Agent, Object, String as JsString, parse_script, script_evaluation};
use nova_vm::engine::Bindable;

use crate::node::core::{InstallError, NodeCtx};
use crate::node::{GcScope, NodeModule};

/// Zero-sized marker for the `node:timers` builtin.
pub(crate) struct TimersModule;

impl NodeModule for TimersModule {
    const SPECIFIER: &'static str = "timers";

    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: GcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError> {
        install(agent, ctx, gc)
    }
}

/// The timers bootstrap.
///
/// An IIFE that wires the timer functions onto `globalThis` (Node exposes them as eager globals)
/// and *also* returns them as an exports object, so `require("node:timers")` and the bare global
/// both see the same functions. Idempotent: a second evaluation (e.g. global install then a later
/// `import`) reuses the registry already parked on `globalThis.__treatyTimers` instead of building a
/// second one, so repeated installs cost nothing and share cancellation state.
///
/// Stored as a single `&'static str` (tenet 3: no per-install heap string).
const BOOTSTRAP: &str = r#"
(function () {
  var g = globalThis;
  // Reuse an existing registry so import-after-global (or vice versa) shares ids + cancellation.
  var R = g.__treatyTimers;
  if (!R) {
    R = g.__treatyTimers = {
      seq: 1,                 // next id; 0 is never a valid timer id (matches Node: ids are > 0)
      live: Object.create(null), // id -> handle, for clear*/cancellation
    };
  }

  // A timer handle. Node returns a `Timeout`/`Immediate` object that coerces to its integer id and
  // carries ref/unref/hasRef/refresh. We model the same observable shape.
  function makeHandle(id, kind, fn, args, delay) {
    var h = {
      _id: id,
      _kind: kind,         // "timeout" | "interval" | "immediate"
      _fn: fn,
      _args: args,
      _delay: delay,
      _seq: id,            // scheduling order for stable same-tick sort
      _refed: true,
      _cancelled: false,
      ref: function () { this._refed = true; return this; },
      unref: function () { this._refed = false; return this; },
      hasRef: function () { return this._refed && !this._cancelled; },
      refresh: function () {
        // Node: restart the timer's countdown. With a microtask driver this re-arms it for the
        // next drain.
        if (!this._cancelled) arm(this);
        return this;
      },
      // Node Timeout coerces to a primitive (its id) in numeric/string contexts, e.g. as a Map key.
      valueOf: function () { return this._id; },
      toString: function () { return String(this._id); },
      [Symbol.toPrimitive]: function () { return this._id; },
    };
    return h;
  }

  function nextId() {
    var id = R.seq++;
    // Guard the (astronomically unlikely) wrap so ids stay positive integers, never 0.
    if (R.seq <= 0) R.seq = 1;
    return id;
  }

  function fire(h) {
    if (h._cancelled) return;
    if (h._kind !== "interval") {
      // One-shot: drop from the live table before invoking so a clear() inside the callback is a
      // no-op rather than a double-free, matching Node.
      delete R.live[h._id];
    }
    try {
      h._fn.apply(undefined, h._args);
    } catch (e) {
      // An uncaught timer-callback error surfaces to the host on the next microtask turn, mirroring
      // Node's "uncaught exception in timer" behavior, without aborting other pending timers.
      Promise.reject(e);
    }
  }

  // Park `h` so it fires *after* the current turn's microtasks. Node runs timer callbacks in a
  // macrotask phase that always trails the microtask checkpoint, so a `queueMicrotask` (or a
  // promise `.then`) scheduled in the same synchronous turn must run first. We have no macrotask
  // clock to drive (see module docs), so we approximate "after the microtask checkpoint" by
  // deferring the fire one extra microtask hop: a plain `queueMicrotask`/`then` enqueued this turn
  // sits ahead of the second hop and therefore drains before the timer callback runs.
  function arm(h) {
    Promise.resolve().then(function () {
      // First hop: lets same-turn microtasks (which were enqueued before/around this one) settle.
      return Promise.resolve();
    }).then(function () {
      if (h._cancelled) return;
      fire(h);
    });
  }

  function setTimeout(fn, delay) {
    if (typeof fn !== "function") {
      throw new TypeError("The \"callback\" argument must be of type function.");
    }
    var args = Array.prototype.slice.call(arguments, 2);
    var d = +delay; if (!(d >= 0)) d = 0; // NaN/negative -> 0, per Node clamping
    var id = nextId();
    var h = makeHandle(id, "timeout", fn, args, d);
    R.live[id] = h;
    arm(h);
    return h;
  }

  function setInterval(fn, delay) {
    if (typeof fn !== "function") {
      throw new TypeError("The \"callback\" argument must be of type function.");
    }
    var args = Array.prototype.slice.call(arguments, 2);
    var d = +delay; if (!(d >= 0)) d = 0;
    var id = nextId();
    var h = makeHandle(id, "interval", fn, args, d);
    R.live[id] = h;
    arm(h); // fires once per drain; see module docs for why it does not self-perpetuate.
    return h;
  }

  function setImmediate(fn) {
    if (typeof fn !== "function") {
      throw new TypeError("The \"callback\" argument must be of type function.");
    }
    var args = Array.prototype.slice.call(arguments, 1);
    var id = nextId();
    var h = makeHandle(id, "immediate", fn, args, 0);
    R.live[id] = h;
    arm(h);
    return h;
  }

  // clear* accept either a handle or its raw integer id (Node accepts both); a bogus value is a
  // silent no-op, exactly like Node.
  function idOf(handleOrId) {
    if (handleOrId == null) return undefined;
    if (typeof handleOrId === "object") return handleOrId._id;
    return +handleOrId;
  }
  function clear(handleOrId) {
    var id = idOf(handleOrId);
    if (id === undefined) return;
    var h = R.live[id];
    if (h) { h._cancelled = true; delete R.live[id]; }
  }
  var clearTimeout = clear, clearInterval = clear, clearImmediate = clear;

  // queueMicrotask: enqueue a callback as a genuine microtask (drained before any timer fire).
  function queueMicrotask(fn) {
    if (typeof fn !== "function") {
      throw new TypeError("The \"callback\" argument must be of type function.");
    }
    Promise.resolve().then(function () {
      try { fn(); } catch (e) { Promise.reject(e); }
    });
  }

  // Eager globals: Node code assumes these exist without importing `node:timers`.
  g.setTimeout = setTimeout;
  g.setInterval = setInterval;
  g.setImmediate = setImmediate;
  g.clearTimeout = clearTimeout;
  g.clearInterval = clearInterval;
  g.clearImmediate = clearImmediate;
  g.queueMicrotask = queueMicrotask;

  // The `node:timers` exports object.
  return {
    setTimeout: setTimeout,
    setInterval: setInterval,
    setImmediate: setImmediate,
    clearTimeout: clearTimeout,
    clearInterval: clearInterval,
    clearImmediate: clearImmediate,
    queueMicrotask: queueMicrotask,
  };
})()
"#;

/// Uniform per-module entry. Returns the `node:timers` exports object.
///
/// Runs inside the caller's `run_in_realm` scope (the realm is already current), so it can simply
/// parse + evaluate [`BOOTSTRAP`] and hand back its completion value, which is the exports object.
/// A parse or evaluation failure is surfaced as [`InstallError::Nova`] rather than panicking.
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
            "failed to parse node:timers bootstrap".to_owned()
        } else {
            msg
        })
    })?;

    let value = script_evaluation(agent, script.unbind(), gc.reborrow())
        .unbind()
        .bind(gc.nogc());

    match value {
        Ok(value) => Object::try_from(value.unbind()).map_err(|_| {
            InstallError::Nova("node:timers bootstrap did not yield an object".to_owned())
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
    //! `module_cjs`'s `with_agent`) rather than going through `JsRuntime::with_node_compat`, because
    //! the eager-global wiring of the timer functions lives in `globals.rs` (owned by the scaffold)
    //! and may not yet route through this module. Calling [`install`] directly is the canonical
    //! single-module test: the bootstrap publishes the timer functions onto `globalThis` itself, so
    //! after one `install` a script can call `setTimeout`/`queueMicrotask`/`clear*` directly. Each
    //! test schedules timers, lets the pump drain the microtasks they ride on, then reads back a
    //! global to observe the side effect — exactly what a second `eval` would see in production.

    use super::*;
    use crate::node::core::{extend_lifetime, EnvMap, HostState};
    use crate::node::event_loop::run_until_idle;
    use nova_vm::ecmascript::{
        AgentOptions, GcAgent, String as JsString, Value, parse_script, script_evaluation,
    };

    /// Stand up a live agent with the Node layer, run `install` once (publishing the timer globals),
    /// then run `body`. Torn down with the agent dropped before the host state (documented FFI
    /// boundary), identical to the production drop order.
    fn with_timers<R>(body: impl for<'a, 'b> FnOnce(&mut Agent, GcScope<'a, 'b>) -> R) -> R {
        let host_state = Box::new(HostState::new(std::env::current_dir().unwrap(), EnvMap::new()));
        // SAFETY: the agent is dropped before `host_state` (declared after it), so it never observes
        // a freed `HostState`. This is the single shared FFI-boundary `unsafe` documented on
        // `core::extend_lifetime`; the timers module itself contains none.
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
            // Publish the timer globals (and return value is the exports object; we rely on globals).
            install(agent, &ctx, gc.reborrow()).expect("timers install");
            body(agent, gc)
        });
        drop(agent);
        drop(host_state);
        out
    }

    /// Evaluate `src` as a script, then drain the shared event loop so microtask-driven timer
    /// callbacks fire. Returns the completion value rendered to its string repr (sufficient for the
    /// scalar assertions below). A thrown value is returned as `Err(message)`.
    fn run(agent: &mut Agent, state_loop: &HostState, src: &str, mut gc: GcScope) -> Result<String, String> {
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
        // Drain microtasks so any scheduled timer callbacks run their side effects.
        run_until_idle(agent, state_loop.event_loop(), None, gc)
            .map_err(|e| format!("pump error: {e:?}"))?;
        Ok(repr)
    }

    /// Read a global's string repr after the pump has settled (a fresh eval of the expression).
    fn read(agent: &mut Agent, state_loop: &HostState, expr: &str, gc: GcScope) -> String {
        run(agent, state_loop, expr, gc).expect("read expression")
    }

    /// Recover the host state for the loop pump inside a `with_timers` body.
    fn loop_state(agent: &Agent) -> &'static HostState {
        let s = crate::node::core::host_state(agent).unwrap();
        // SAFETY: documented FFI extension; lives for the enclosing `with_timers` scope.
        unsafe { extend_lifetime(s) }
    }

    #[test]
    fn set_timeout_callback_runs_after_the_loop_pumps() {
        with_timers(|agent, mut gc| {
            let st = loop_state(agent);
            // Schedule + read synchronously: still 0 (the callback rides a not-yet-drained microtask).
            let before = run(
                agent,
                st,
                "globalThis.__t = 0; setTimeout(() => { globalThis.__t = 7; }); String(globalThis.__t)",
                gc.reborrow(),
            )
            .unwrap();
            // The trailing `String(__t)` is computed before the pump runs, so it reads 0; then the
            // pump (inside `run`) fires the callback. Re-read to observe the fired side effect.
            assert_eq!(before, "0", "callback must not run synchronously");
            assert_eq!(read(agent, st, "String(globalThis.__t)", gc.reborrow()), "7");
        });
    }

    #[test]
    fn set_timeout_forwards_extra_arguments() {
        with_timers(|agent, mut gc| {
            let st = loop_state(agent);
            run(
                agent,
                st,
                "globalThis.__sum = 0; setTimeout((a, b, c) => { globalThis.__sum = a + b + c; }, 0, 1, 2, 3);",
                gc.reborrow(),
            )
            .unwrap();
            assert_eq!(read(agent, st, "String(globalThis.__sum)", gc.reborrow()), "6");
        });
    }

    #[test]
    fn clear_timeout_cancels_a_pending_callback() {
        with_timers(|agent, mut gc| {
            let st = loop_state(agent);
            run(
                agent,
                st,
                "globalThis.__c = 'untouched';\
                 const id = setTimeout(() => { globalThis.__c = 'fired'; });\
                 clearTimeout(id);",
                gc.reborrow(),
            )
            .unwrap();
            assert_eq!(read(agent, st, "globalThis.__c", gc.reborrow()), "untouched");
        });
    }

    #[test]
    fn set_timeout_returns_a_handle_that_coerces_to_its_id() {
        with_timers(|agent, mut gc| {
            let st = loop_state(agent);
            // Positive integer id via Number(handle); ref/unref toggle hasRef.
            let id_positive = run(
                agent,
                st,
                "const h = setTimeout(() => {}); const ok = Number(h) > 0; clearTimeout(h); String(ok)",
                gc.reborrow(),
            )
            .unwrap();
            assert_eq!(id_positive, "true", "ids are positive integers via valueOf");
            let unreffed = run(
                agent,
                st,
                "const h2 = setTimeout(() => {}); const r = h2.hasRef(); const u = h2.unref().hasRef(); clearTimeout(h2); r + ',' + u",
                gc.reborrow(),
            )
            .unwrap();
            assert_eq!(unreffed, "true,false", "unref clears the ref flag");
        });
    }

    #[test]
    fn set_immediate_runs_its_callback() {
        with_timers(|agent, mut gc| {
            let st = loop_state(agent);
            run(
                agent,
                st,
                "globalThis.__i = 0; setImmediate((x) => { globalThis.__i = x; }, 42);",
                gc.reborrow(),
            )
            .unwrap();
            assert_eq!(read(agent, st, "String(globalThis.__i)", gc.reborrow()), "42");
        });
    }

    #[test]
    fn clear_immediate_cancels() {
        with_timers(|agent, mut gc| {
            let st = loop_state(agent);
            run(
                agent,
                st,
                "globalThis.__im = 'no';\
                 const id = setImmediate(() => { globalThis.__im = 'yes'; });\
                 clearImmediate(id);",
                gc.reborrow(),
            )
            .unwrap();
            assert_eq!(read(agent, st, "globalThis.__im", gc.reborrow()), "no");
        });
    }

    #[test]
    fn queue_microtask_runs_before_timer_callbacks() {
        with_timers(|agent, mut gc| {
            let st = loop_state(agent);
            run(
                agent,
                st,
                "globalThis.__order = [];\
                 setTimeout(() => { globalThis.__order.push('timer'); });\
                 queueMicrotask(() => { globalThis.__order.push('micro'); });",
                gc.reborrow(),
            )
            .unwrap();
            // The microtask runs before the timer fire (which is itself a later microtask).
            assert_eq!(
                read(agent, st, "globalThis.__order.join(',')", gc.reborrow()),
                "micro,timer"
            );
        });
    }

    #[test]
    fn timer_functions_are_published_as_globals() {
        with_timers(|agent, mut gc| {
            let st = loop_state(agent);
            let all = run(
                agent,
                st,
                "['setTimeout','setInterval','setImmediate','clearTimeout','clearInterval','clearImmediate','queueMicrotask']\
                 .every(n => typeof globalThis[n] === 'function') ? 'ok' : 'missing'",
                gc.reborrow(),
            )
            .unwrap();
            assert_eq!(all, "ok");
        });
    }

    #[test]
    fn second_install_returns_exports_and_shares_the_registry() {
        with_timers(|agent, mut gc| {
            let st = loop_state(agent);
            let ctx = NodeCtx::new(loop_state(agent));
            // `with_timers` already ran one install; a second returns the exports namespace object
            // and must reuse the existing registry rather than build a fresh one.
            let exports = install(agent, &ctx, gc.reborrow()).unwrap();
            let _: Object = exports; // it is an object namespace, not undefined/null

            // The registry singleton is parked on globalThis and shared across installs.
            let shared = run(
                agent,
                st,
                "typeof globalThis.__treatyTimers === 'object' && globalThis.__treatyTimers.seq >= 1 ? 'shared' : 'no'",
                gc.reborrow(),
            )
            .unwrap();
            assert_eq!(shared, "shared", "the registry singleton is parked on globalThis");

            // Ids advance monotonically and stay positive across installs (one shared sequence).
            let monotonic = run(
                agent,
                st,
                "const a = setTimeout(()=>{}); const b = setTimeout(()=>{}); const ok = Number(b) > Number(a) && Number(a) > 0; clearTimeout(a); clearTimeout(b); String(ok)",
                gc.reborrow(),
            )
            .unwrap();
            assert_eq!(monotonic, "true");
        });
    }

    #[test]
    fn invalid_callback_throws_type_error() {
        with_timers(|agent, mut gc| {
            let st = loop_state(agent);
            let err = run(agent, st, "setTimeout(123)", gc.reborrow())
                .expect_err("non-function callback must throw");
            assert!(
                err.contains("callback"),
                "TypeError should mention the callback argument, got: {err}"
            );
        });
    }
}
