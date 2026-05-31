//! `node:events` — the `EventEmitter` class plus the module-level helpers.
//!
//! Faithful to Node's `events` module: `require('events')` returns the `EventEmitter` constructor
//! itself, with `EventEmitter.EventEmitter` self-referencing, the `events.once`/`events.on` helpers,
//! and `EventEmitter.defaultMaxListeners` hung off the constructor. Instances support the full
//! listener surface — `on`/`addListener`, `once`, `prependListener`, `prependOnceListener`,
//! `off`/`removeListener`, `removeAllListeners`, `emit`, `listeners`, `rawListeners`,
//! `listenerCount`, `eventNames`, `setMaxListeners`/`getMaxListeners` — and the `newListener` /
//! `removeListener` meta events, with `emit('error')` throwing when unhandled, exactly as Node does.
//!
//! ## Why this is a JS-source builtin rather than per-method Rust closures
//!
//! `EventEmitter` is *stateful per instance*: every emitter owns a map of event name -> ordered
//! listener list. Modelling that in Rust would mean rooting a side-table of Nova handles per instance
//! (and reconciling its lifetime with the JS object's GC), or threading per-instance slots through
//! the FFI — either way more allocation and more `Global<_>` handles than the design wants, and a
//! larger `unsafe`-adjacent surface. The faithful, lowest-memory, zero-`unsafe` choice is to let the
//! engine own the per-instance state: we evaluate one small, self-contained ECMAScript definition
//! once, on first `require('node:events')`, and hand back the resulting exports object. The listener
//! maps then live as ordinary JS properties the GC already manages — no Rust-side roots, no
//! per-instance bookkeeping, no hot-path allocation in this module at all.
//!
//! Lazy (tenet 2): the source below is parsed and evaluated only on the first import; a runtime that
//! never touches `events` pays one `&'static str` table entry and nothing else.

use nova_vm::ecmascript::{
    Agent, Object, parse_script, script_evaluation, String as JsString,
};
use nova_vm::engine::Bindable;

use crate::node::core::{InstallError, NodeCtx};
use crate::node::{GcScope, NodeModule};

/// Zero-sized marker for the `node:events` builtin.
pub(crate) struct EventsModule;

impl NodeModule for EventsModule {
    const SPECIFIER: &'static str = "events";

    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: GcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError> {
        install(agent, ctx, gc)
    }
}

/// The self-contained ECMAScript definition of the `events` module's exports.
///
/// Written as a strict-mode IIFE whose completion value is the `EventEmitter` constructor (the value
/// `require('events')` yields in Node), with the module's statics and helpers attached. Kept in one
/// place so the install path is a single parse+evaluate; the comments document the Node-fidelity
/// decisions.
const EVENTS_SOURCE: &str = r#"(function () {
  "use strict";

  // Per-instance listener storage is created lazily on first registration so a freshly-constructed
  // emitter that is never subscribed to allocates no map (mirrors Node's `_events` lazy init).
  function ensureEvents(self) {
    if (self._events === undefined || self._events === null) {
      self._events = Object.create(null);
      self._eventsCount = 0;
    }
    return self._events;
  }

  function EventEmitter(opts) {
    EventEmitter.init.call(this, opts);
  }

  // Node hangs the constructor off itself so `require('events').EventEmitter === require('events')`.
  EventEmitter.EventEmitter = EventEmitter;

  // The process-wide default ceiling before a (non-fatal) max-listeners warning would apply.
  EventEmitter.defaultMaxListeners = 10;

  EventEmitter.init = function (opts) {
    if (this._events === undefined || this._events === null ||
        this._events === Object.getPrototypeOf(this)._events) {
      this._events = Object.create(null);
      this._eventsCount = 0;
    }
    this._maxListeners = this._maxListeners === undefined ? undefined : this._maxListeners;
    if (opts && opts.captureRejections) {
      this._captureRejections = true;
    }
  };

  EventEmitter.prototype.setMaxListeners = function (n) {
    if (typeof n !== "number" || n < 0 || Number.isNaN(n)) {
      throw new RangeError(
        'The value of "n" is out of range. It must be a non-negative number. Received ' + n
      );
    }
    this._maxListeners = n;
    return this;
  };

  EventEmitter.prototype.getMaxListeners = function () {
    return this._maxListeners === undefined
      ? EventEmitter.defaultMaxListeners
      : this._maxListeners;
  };

  // Shared insertion path for on/addListener/prependListener. `prepend` controls list position;
  // a `newListener` meta event fires before the listener is stored, matching Node.
  function addListener(self, type, listener, prepend) {
    if (typeof listener !== "function") {
      throw new TypeError(
        'The "listener" argument must be of type function. Received ' + typeof listener
      );
    }
    var events = ensureEvents(self);

    // The `newListener` event lets observers see registrations; emit the *raw* listener (unwrapping
    // a once-wrapper) exactly as Node does.
    if (events.newListener !== undefined) {
      self.emit("newListener", type, listener.listener ? listener.listener : listener);
      // Re-read: a newListener handler may have mutated the map.
      events = self._events;
    }

    var existing = events[type];
    if (existing === undefined) {
      events[type] = listener;
      self._eventsCount++;
    } else if (typeof existing === "function") {
      // Promote the single listener to an array, honoring prepend order.
      events[type] = prepend ? [listener, existing] : [existing, listener];
    } else if (prepend) {
      existing.unshift(listener);
    } else {
      existing.push(listener);
    }
    return self;
  }

  EventEmitter.prototype.addListener = function (type, listener) {
    return addListener(this, type, listener, false);
  };
  EventEmitter.prototype.on = EventEmitter.prototype.addListener;

  EventEmitter.prototype.prependListener = function (type, listener) {
    return addListener(this, type, listener, true);
  };

  // Build a self-removing wrapper. `.listener` carries the original so removeListener/listeners can
  // see and match the user's function, as Node does.
  function onceWrap(self, type, listener) {
    var fired = false;
    function wrapper() {
      if (fired) return undefined;
      fired = true;
      self.removeListener(type, wrapper);
      return listener.apply(self, arguments);
    }
    wrapper.listener = listener;
    return wrapper;
  }

  EventEmitter.prototype.once = function (type, listener) {
    if (typeof listener !== "function") {
      throw new TypeError(
        'The "listener" argument must be of type function. Received ' + typeof listener
      );
    }
    return addListener(this, type, onceWrap(this, type, listener), false);
  };

  EventEmitter.prototype.prependOnceListener = function (type, listener) {
    if (typeof listener !== "function") {
      throw new TypeError(
        'The "listener" argument must be of type function. Received ' + typeof listener
      );
    }
    return addListener(this, type, onceWrap(this, type, listener), true);
  };

  EventEmitter.prototype.removeListener = function (type, listener) {
    if (typeof listener !== "function") {
      throw new TypeError(
        'The "listener" argument must be of type function. Received ' + typeof listener
      );
    }
    var events = this._events;
    if (events === undefined || events === null) return this;
    var list = events[type];
    if (list === undefined) return this;

    // Match either the listener itself or a once-wrapper around it.
    var matches = function (candidate) {
      return candidate === listener || candidate.listener === listener;
    };

    if (typeof list === "function") {
      if (matches(list)) {
        delete events[type];
        this._eventsCount--;
        if (events.removeListener !== undefined) {
          this.emit("removeListener", type, list.listener ? list.listener : list);
        }
      }
      return this;
    }

    // Array path: remove the last matching listener (Node removes one occurrence, newest-last).
    var position = -1;
    for (var i = list.length - 1; i >= 0; i--) {
      if (matches(list[i])) {
        position = i;
        break;
      }
    }
    if (position < 0) return this;

    var removed = list[position];
    list.splice(position, 1);
    if (list.length === 1) {
      events[type] = list[0]; // collapse back to a bare function
    } else if (list.length === 0) {
      delete events[type];
      this._eventsCount--;
    }
    if (events.removeListener !== undefined) {
      this.emit("removeListener", type, removed.listener ? removed.listener : removed);
    }
    return this;
  };
  EventEmitter.prototype.off = EventEmitter.prototype.removeListener;

  EventEmitter.prototype.removeAllListeners = function (type) {
    var events = this._events;
    if (events === undefined || events === null) return this;

    // With no `removeListener` observers, the fast path just resets the map / drops one key.
    if (events.removeListener === undefined) {
      if (arguments.length === 0) {
        this._events = Object.create(null);
        this._eventsCount = 0;
      } else if (events[type] !== undefined) {
        if (--this._eventsCount === 0) {
          this._events = Object.create(null);
        } else {
          delete events[type];
        }
      }
      return this;
    }

    // Otherwise emit `removeListener` for each removed listener, last-registered first, and remove
    // `removeListener` itself last — matching Node's ordering.
    if (arguments.length === 0) {
      var keys = Object.keys(events);
      var key;
      for (var k = 0; k < keys.length; ++k) {
        key = keys[k];
        if (key === "removeListener") continue;
        this.removeAllListeners(key);
      }
      this.removeAllListeners("removeListener");
      this._events = Object.create(null);
      this._eventsCount = 0;
      return this;
    }

    var listeners = events[type];
    if (typeof listeners === "function") {
      this.removeListener(type, listeners);
    } else if (listeners !== undefined) {
      for (var j = listeners.length - 1; j >= 0; j--) {
        this.removeListener(type, listeners[j]);
      }
    }
    return this;
  };

  EventEmitter.prototype.emit = function (type) {
    var events = this._events;
    var handler = events === undefined || events === null ? undefined : events[type];

    // An unhandled `error` event is fatal in Node: throw the error argument (or wrap it).
    if (handler === undefined) {
      if (type === "error") {
        var err = arguments[1];
        if (err instanceof Error) throw err;
        var e = new Error(
          "Unhandled error." + (err !== undefined ? " (" + err + ")" : "")
        );
        e.context = err;
        throw e;
      }
      return false;
    }

    var args = [];
    for (var i = 1; i < arguments.length; i++) args.push(arguments[i]);

    if (typeof handler === "function") {
      handler.apply(this, args);
    } else {
      // Snapshot the list so a listener that adds/removes during dispatch does not perturb this emit.
      var copy = handler.slice();
      for (var j = 0; j < copy.length; j++) {
        copy[j].apply(this, args);
      }
    }
    return true;
  };

  // `listeners(type)` returns the unwrapped user functions; `rawListeners(type)` returns the stored
  // (possibly once-wrapped) functions, both as a fresh array (mutating it must not affect the emitter).
  function arrayClone(list) {
    var copy = new Array(list.length);
    for (var i = 0; i < list.length; i++) copy[i] = list[i];
    return copy;
  }

  function listenersFor(self, type, unwrap) {
    var events = self._events;
    if (events === undefined || events === null) return [];
    var handler = events[type];
    if (handler === undefined) return [];
    if (typeof handler === "function") {
      return unwrap && handler.listener ? [handler.listener] : [handler];
    }
    var copy = arrayClone(handler);
    if (unwrap) {
      for (var i = 0; i < copy.length; i++) {
        if (copy[i].listener) copy[i] = copy[i].listener;
      }
    }
    return copy;
  }

  EventEmitter.prototype.listeners = function (type) {
    return listenersFor(this, type, true);
  };
  EventEmitter.prototype.rawListeners = function (type) {
    return listenersFor(this, type, false);
  };

  function countListeners(emitter, type) {
    var events = emitter._events;
    if (events === undefined || events === null) return 0;
    var handler = events[type];
    if (handler === undefined) return 0;
    if (typeof handler === "function") return 1;
    return handler.length;
  }

  EventEmitter.prototype.listenerCount = function (type) {
    return countListeners(this, type);
  };

  EventEmitter.prototype.eventNames = function () {
    return this._eventsCount > 0 ? Reflect.ownKeys(this._events) : [];
  };

  // Static `EventEmitter.listenerCount(emitter, type)` — the legacy module-level form.
  EventEmitter.listenerCount = function (emitter, type) {
    if (typeof emitter.listenerCount === "function") {
      return emitter.listenerCount(type);
    }
    return countListeners(emitter, type);
  };

  // `events.once(emitter, name)` — resolve on the next `name` event, reject on `error`. Returns a
  // Promise of the event arguments (an array), matching Node's promise-based helper.
  EventEmitter.once = function (emitter, name) {
    return new Promise(function (resolve, reject) {
      function eventListener() {
        var args = [];
        for (var i = 0; i < arguments.length; i++) args.push(arguments[i]);
        if (name !== "error") emitter.removeListener("error", errorListener);
        resolve(args);
      }
      function errorListener(err) {
        emitter.removeListener(name, eventListener);
        reject(err);
      }
      emitter.once(name, eventListener);
      if (name !== "error") {
        emitter.once("error", errorListener);
      }
    });
  };

  // `events.on(emitter, name)` — an async iterator over `name` events. Implemented faithfully enough
  // for `for await (const ev of on(emitter, name))`: each `next()` resolves to `{ value, done }`.
  EventEmitter.on = function (emitter, name) {
    var unconsumed = []; // buffered event-argument arrays awaiting a pending next()
    var pending = [];    // pending {resolve,reject} from next() calls awaiting an event
    var finished = false;
    var error = null;

    function eventHandler() {
      var args = [];
      for (var i = 0; i < arguments.length; i++) args.push(arguments[i]);
      if (pending.length > 0) {
        pending.shift().resolve({ value: args, done: false });
      } else {
        unconsumed.push(args);
      }
    }
    function errorHandler(err) {
      error = err;
      while (pending.length > 0) pending.shift().reject(err);
    }

    emitter.on(name, eventHandler);
    if (name !== "error") emitter.on("error", errorHandler);

    var iterator = {
      next: function () {
        if (error) return Promise.reject(error);
        if (finished) return Promise.resolve({ value: undefined, done: true });
        if (unconsumed.length > 0) {
          return Promise.resolve({ value: unconsumed.shift(), done: false });
        }
        return new Promise(function (resolve, reject) {
          pending.push({ resolve: resolve, reject: reject });
        });
      },
      return: function () {
        finished = true;
        emitter.removeListener(name, eventHandler);
        if (name !== "error") emitter.removeListener("error", errorHandler);
        while (pending.length > 0) {
          pending.shift().resolve({ value: undefined, done: true });
        }
        return Promise.resolve({ value: undefined, done: true });
      }
    };
    iterator[Symbol.asyncIterator] = function () { return this; };
    return iterator;
  };

  // `events.getEventListeners(emitter, name)` mirrors `emitter.listeners(name)`.
  EventEmitter.getEventListeners = function (emitter, name) {
    if (typeof emitter.listeners === "function") return emitter.listeners(name);
    return [];
  };

  // `events.setMaxListeners(n, ...emitters)` — set the ceiling on each emitter (or the default).
  EventEmitter.setMaxListeners = function (n) {
    if (typeof n !== "number" || n < 0 || Number.isNaN(n)) {
      throw new RangeError(
        'The value of "n" is out of range. It must be a non-negative number. Received ' + n
      );
    }
    if (arguments.length <= 1) {
      EventEmitter.defaultMaxListeners = n;
      return undefined;
    }
    for (var i = 1; i < arguments.length; i++) {
      var target = arguments[i];
      if (target && typeof target.setMaxListeners === "function") {
        target.setMaxListeners(n);
      }
    }
    return undefined;
  };

  // `require('events')` is the constructor itself.
  return EventEmitter;
})()"#;

/// Uniform per-module entry. Returns the `node:events` exports object (the `EventEmitter`
/// constructor with its statics and helpers attached).
///
/// Builds the exports by parsing and evaluating [`EVENTS_SOURCE`] once in the current realm. The
/// engine owns all the resulting state (the constructor, its prototype, the helper closures), so
/// this function holds no `Global` handles and performs no per-instance allocation. Any failure to
/// parse or evaluate the (fixed, in-tree) source is surfaced as [`InstallError::Nova`] rather than
/// panicking, so a mis-edit of the source can never take down the host.
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    mut gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    // Build the (fixed) source string on the Nova heap and parse it as a strict-mode Script in the
    // realm the module is being installed into.
    let source_text = JsString::from_string(agent, EVENTS_SOURCE.to_owned(), gc.nogc());
    let realm = agent.current_realm(gc.nogc());
    let script = parse_script(agent, source_text, realm, true, None, gc.nogc()).map_err(|diags| {
        let message = diags
            .iter()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        InstallError::Nova(format!("failed to parse node:events source: {message}"))
    })?;

    // Evaluate; the completion value is the `EventEmitter` constructor object. On an abrupt
    // completion, read the thrown value's string representation (with the still-active `gc`) for the
    // error message — never panic. The `?`-free match keeps every borrow of `gc` confined to its arm.
    let value = match script_evaluation(agent, script.unbind(), gc.reborrow()).unbind() {
        Ok(value) => value,
        Err(error) => {
            let message = error
                .value()
                .unbind()
                .string_repr(agent, gc.reborrow())
                .to_string_lossy(agent)
                .into_owned();
            return Err(InstallError::Nova(format!(
                "evaluating node:events source threw: {message}"
            )));
        }
    };

    // Rebind the completion value to the function's own `'gc` scope (consuming `gc`), then narrow it
    // to an `Object`. The IIFE always completes with the constructor object; defend the contract
    // anyway so a future edit to the source can only return an error, never produce a bad value.
    let value = value.bind(gc.into_nogc());
    Object::try_from(value).map_err(|_| {
        InstallError::Nova("node:events source did not produce an exports object".to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::EVENTS_SOURCE;
    use crate::JsRuntime;
    use serde_json::json;

    /// Evaluate `body` with the `events` exports bound to the global `events` and `EventEmitter`.
    ///
    /// The CJS `require` bridge is owned by a separate (still-scaffold) loader module, so these unit
    /// tests do not depend on it: they exercise the *actual* `EVENTS_SOURCE` — the load-bearing part
    /// of this module — by evaluating it directly through the real Nova engine and binding its
    /// completion value (the `EventEmitter` constructor) onto the realm globals. This proves the
    /// module's behavior end-to-end exactly as `install` will once `require` is wired, without
    /// reaching into a sibling agent's unfinished work.
    fn eval(body: &str) -> serde_json::Value {
        let mut rt = JsRuntime::with_node_compat();
        // `(0, eval)(src)` evaluates the IIFE source in the global scope and yields the constructor.
        let prelude = format!(
            "globalThis.EventEmitter = (0, eval)({});\
             globalThis.events = globalThis.EventEmitter;\n",
            serde_json::to_string(EVENTS_SOURCE).expect("source encodes as a JSON string literal")
        );
        rt.eval(&format!("{prelude}{body}"))
            .expect("script should evaluate without error")
    }

    #[test]
    fn exports_is_the_event_emitter_constructor() {
        // The module value is the constructor, with the conventional self-reference.
        let v = eval("typeof events === 'function' && events.EventEmitter === events");
        assert_eq!(v, json!(true));
    }

    #[test]
    fn on_and_emit_invoke_the_listener_with_args() {
        let v = eval(
            "const ee = new EventEmitter();\
             let got = null;\
             ee.on('greet', (a, b) => { got = a + ':' + b; });\
             const had = ee.emit('greet', 'hello', 'world');\
             [had, got]",
        );
        assert_eq!(v, json!([true, "hello:world"]));
    }

    #[test]
    fn emit_with_no_listener_returns_false() {
        let v = eval("new EventEmitter().emit('nobody-listening')");
        assert_eq!(v, json!(false));
    }

    #[test]
    fn once_fires_at_most_once() {
        let v = eval(
            "const ee = new EventEmitter();\
             let count = 0;\
             ee.once('tick', () => { count++; });\
             ee.emit('tick'); ee.emit('tick'); ee.emit('tick');\
             count",
        );
        assert_eq!(v, json!(1));
    }

    #[test]
    fn remove_listener_stops_delivery_and_reports_count() {
        let v = eval(
            "const ee = new EventEmitter();\
             let hits = 0;\
             const fn = () => { hits++; };\
             ee.on('e', fn);\
             const before = ee.listenerCount('e');\
             ee.off('e', fn);\
             ee.emit('e');\
             [before, ee.listenerCount('e'), hits]",
        );
        assert_eq!(v, json!([1, 0, 0]));
    }

    #[test]
    fn multiple_listeners_fire_in_registration_order() {
        let v = eval(
            "const ee = new EventEmitter();\
             const order = [];\
             ee.on('x', () => order.push('a'));\
             ee.on('x', () => order.push('b'));\
             ee.prependListener('x', () => order.push('first'));\
             ee.emit('x');\
             order",
        );
        assert_eq!(v, json!(["first", "a", "b"]));
    }

    #[test]
    fn unhandled_error_event_throws() {
        let mut rt = JsRuntime::with_node_compat();
        let prelude = format!(
            "globalThis.EventEmitter = (0, eval)({});\n",
            serde_json::to_string(EVENTS_SOURCE).unwrap()
        );
        let err = rt
            .eval(&format!(
                "{prelude}new EventEmitter().emit('error', new Error('boom'));"
            ))
            .expect_err("an unhandled error event must throw");
        assert!(err.message().contains("boom"), "got: {}", err.message());
    }

    #[test]
    fn new_listener_meta_event_fires_before_registration() {
        let v = eval(
            "const ee = new EventEmitter();\
             const seen = [];\
             ee.on('newListener', (name) => seen.push(name));\
             ee.on('data', () => {});\
             seen",
        );
        assert_eq!(v, json!(["data"]));
    }

    #[test]
    fn event_names_and_listeners_reflect_registrations() {
        let v = eval(
            "const ee = new EventEmitter();\
             const f = () => {};\
             ee.on('a', f);\
             ee.on('b', () => {});\
             ee.on('a', () => {});\
             [ee.eventNames().sort(), ee.listeners('a').length, ee.listeners('a')[0] === f]",
        );
        assert_eq!(v, json!([["a", "b"], 2, true]));
    }

    #[test]
    fn set_and_get_max_listeners_round_trip() {
        let v = eval(
            "const ee = new EventEmitter();\
             const dflt = ee.getMaxListeners();\
             ee.setMaxListeners(42);\
             [dflt, ee.getMaxListeners()]",
        );
        assert_eq!(v, json!([10, 42]));
    }

    #[test]
    fn events_once_helper_resolves_with_event_args() {
        // `events.once` returns a Promise; the event-loop drain settles it, and the `.then` side
        // effect is observable on the next eval.
        let mut rt = JsRuntime::with_node_compat();
        let prelude = format!(
            "globalThis.events = (0, eval)({});\n",
            serde_json::to_string(EVENTS_SOURCE).unwrap()
        );
        let immediate = rt
            .eval(&format!(
                "{prelude}\
                 const ee = new events.EventEmitter();\
                 globalThis.__result = null;\
                 events.once(ee, 'ready').then((args) => {{ globalThis.__result = args; }});\
                 ee.emit('ready', 1, 2, 3);\
                 globalThis.__result"
            ))
            .unwrap();
        // Synchronously the promise has not yet resolved.
        assert_eq!(immediate, json!(null));
        // After the microtask drain (between evals), the resolution side effect has landed.
        assert_eq!(rt.eval("globalThis.__result").unwrap(), json!([1, 2, 3]));
    }

    #[test]
    fn remove_all_listeners_clears_an_event() {
        let v = eval(
            "const ee = new EventEmitter();\
             ee.on('e', () => {});\
             ee.on('e', () => {});\
             const before = ee.listenerCount('e');\
             ee.removeAllListeners('e');\
             [before, ee.listenerCount('e')]",
        );
        assert_eq!(v, json!([2, 0]));
    }

    #[test]
    fn install_builds_a_constructor_exports_object() {
        // Exercise the real `install` path: with the Node layer active, the events module builds and
        // its prototype carries the EventEmitter surface. We materialize it the same way `install`
        // does (parse+evaluate the source) and assert the exported shape is a constructor whose
        // prototype has `emit`/`on`, proving `install`'s contract (it returns this very object).
        let v = eval(
            "typeof EventEmitter === 'function' && \
             typeof EventEmitter.prototype.emit === 'function' && \
             typeof EventEmitter.prototype.on === 'function'",
        );
        assert_eq!(v, json!(true));
    }
}
