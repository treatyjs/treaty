//! `node:stream` — the Node stream object model (`Readable`/`Writable`/`Duplex`/`Transform`/
//! `PassThrough`) plus the module-level `pipeline(...)` and `finished(...)` helpers.
//!
//! ## Why this module is bootstrapped in JS
//!
//! Like `node:events`, `node:assert`, and `node:util`, Node's own `node:stream` is JavaScript over a
//! small set of primitives, and the stream classes are *stateful per instance*: every stream owns an
//! internal read/write buffer, a pile of boolean flags, and a listener map. Re-expressing that state
//! and the push/pull backpressure machinery against Nova's GC-handle FFI would mean rooting a
//! side-table of handles per instance and threading scoped handles through user `_read`/`_write`
//! callbacks — exactly the allocation-heavy, `unsafe`-adjacent boilerplate the architecture forbids.
//!
//! So `stream` materializes by evaluating ONE self-contained bootstrap IIFE (see [`STREAM_SOURCE`])
//! the first time `require("node:stream")` / `import "node:stream"` runs. Its completion value is the
//! `Stream` exports object with `Readable`/`Writable`/`Duplex`/`Transform`/`PassThrough`/`pipeline`/
//! `finished` attached, which the registry caches; the parse+evaluate cost is paid at most once per
//! runtime and an untouched `stream` costs one table entry and a fn pointer (tenet 2).
//!
//! ## How asynchrony is driven
//!
//! The stream machinery is fully asynchronous (data/end/finish events, backpressure, async
//! iteration) and is driven through the shared event loop: every internal "emit later" hop uses the
//! always-present `queueMicrotask` global (wired by [`crate::node::globals`] / [`crate::node::timers`])
//! so reads, writes, pipes, and the `pipeline`/`finished` callbacks settle when the runtime drains
//! microtasks after evaluation. The bootstrap embeds a minimal `EventEmitter` (the same listener
//! model as `node:events`) so it has no install-time dependency on another builtin — it references
//! only realm intrinsics plus `queueMicrotask`, allocating only the closures it exports (tenets 1 & 3).
//!
//! ## Faithfulness
//!
//! * `Readable`: `push`/`read`, `on('data')` flowing mode, `on('end')`, `pause`/`resume`, `pipe`,
//!   async iteration (`Symbol.asyncIterator`), and the static `Readable.from(iterable)`.
//! * `Writable`: `write`/`end`, `on('finish')`/`on('close')`, `_write`/`_final`, backpressure return.
//! * `Duplex`: a readable + writable side over one object (used by `net`/`http` sockets).
//! * `Transform`: `_transform`/`_flush`, mapping written chunks to readable output; `PassThrough` is
//!   the identity transform.
//! * `pipeline(...streams, cb)`: pipes a chain source -> ...transforms -> destination, propagating
//!   errors and invoking `cb(err)` on completion; returns the destination.
//! * `finished(stream, cb)`: invokes `cb(err)` once the stream ends/finishes/errors; returns a
//!   cleanup function.

use nova_vm::ecmascript::{Agent, Object, String as JsString, parse_script, script_evaluation};
use nova_vm::engine::Bindable;

use crate::node::core::{InstallError, NodeCtx};
use crate::node::{GcScope, NodeModule};

/// Zero-sized marker for the `node:stream` builtin.
pub(crate) struct StreamModule;

impl NodeModule for StreamModule {
    const SPECIFIER: &'static str = "stream";

    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: GcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError> {
        install(agent, ctx, gc)
    }
}

/// The self-contained `node:stream` implementation, evaluated once on first import.
///
/// A strict-mode IIFE whose completion value is the `Stream` exports object. Intrinsic-only except
/// for the `queueMicrotask` global it uses to defer event emission onto the shared event loop.
const STREAM_SOURCE: &str = r#"(function () {
  "use strict";

  // A microtask hop, used everywhere a Node stream would "emit on the next tick". Falls back to a
  // resolved-promise hop if `queueMicrotask` is somehow absent so the module is robust in isolation.
  var defer = (typeof queueMicrotask === "function")
    ? queueMicrotask
    : function (fn) { Promise.resolve().then(fn); };

  // --- minimal EventEmitter (same model as node:events; embedded to avoid an install-time require) -
  function EventEmitter() {
    this._events = Object.create(null);
  }
  EventEmitter.prototype.on = function (type, fn) {
    var list = this._events[type] || (this._events[type] = []);
    list.push(fn);
    return this;
  };
  EventEmitter.prototype.addListener = EventEmitter.prototype.on;
  EventEmitter.prototype.once = function (type, fn) {
    var self = this;
    function wrapper() {
      self.removeListener(type, wrapper);
      return fn.apply(self, arguments);
    }
    wrapper.listener = fn;
    return this.on(type, wrapper);
  };
  EventEmitter.prototype.removeListener = function (type, fn) {
    var list = this._events[type];
    if (!list) return this;
    for (var i = list.length - 1; i >= 0; i--) {
      if (list[i] === fn || list[i].listener === fn) { list.splice(i, 1); break; }
    }
    if (list.length === 0) delete this._events[type];
    return this;
  };
  EventEmitter.prototype.off = EventEmitter.prototype.removeListener;
  EventEmitter.prototype.removeAllListeners = function (type) {
    if (type === undefined) this._events = Object.create(null);
    else delete this._events[type];
    return this;
  };
  EventEmitter.prototype.listeners = function (type) {
    var list = this._events[type];
    return list ? list.slice() : [];
  };
  EventEmitter.prototype.listenerCount = function (type) {
    var list = this._events[type];
    return list ? list.length : 0;
  };
  EventEmitter.prototype.emit = function (type) {
    var list = this._events[type];
    var args = [];
    for (var i = 1; i < arguments.length; i++) args.push(arguments[i]);
    if (!list) {
      // An unhandled 'error' is fatal, matching Node.
      if (type === "error") {
        var err = args[0];
        if (err instanceof Error) throw err;
        throw new Error("Unhandled stream error");
      }
      return false;
    }
    var copy = list.slice();
    for (var j = 0; j < copy.length; j++) copy[j].apply(this, args);
    return true;
  };

  // Chain a constructor's prototype onto EventEmitter so every stream is an emitter.
  function inheritEmitter(ctor) {
    ctor.prototype = Object.create(EventEmitter.prototype);
    ctor.prototype.constructor = ctor;
    return ctor;
  }

  // ============================================================================================
  // Readable
  // ============================================================================================
  function Readable(options) {
    // Redirect a bare `Readable(opts)` (no `new`) to construction. Subclasses invoke this via
    // `Readable.call(this, ...)` where `this` is a Duplex/Transform — those inherit EventEmitter but
    // NOT Readable, so we must test the shared base, not `instanceof Readable`, or the inherited init
    // would wrongly allocate a throwaway instance.
    if (!(this instanceof EventEmitter)) return new Readable(options);
    EventEmitter.call(this);
    options = options || {};
    this._readableState = {
      buffer: [],          // queued chunks awaiting consumption
      flowing: null,       // null = not yet decided, true = flowing (data events), false = paused
      ended: false,        // upstream signalled EOF (push(null))
      endEmitted: false,   // the 'end' event has fired
      reading: false,      // a _read() is in flight
      stepScheduled: false, // a flow `step` is queued (reentrancy guard)
      objectMode: !!options.objectMode,
      destroyed: false,
      errored: null
    };
    if (typeof options.read === "function") this._read = options.read;
    this.readable = true;
  }
  inheritEmitter(Readable);

  // Default _read is a no-op; subclasses / `from` / options override it.
  Readable.prototype._read = function () {};

  // Queue a chunk (or signal EOF with null). Returns false once buffered past the (soft) limit so a
  // producer can apply backpressure, true otherwise — matching Node's push() contract.
  Readable.prototype.push = function (chunk) {
    var state = this._readableState;
    if (state.ended || state.destroyed) return false;
    if (chunk === null) {
      state.ended = true;
      state.reading = false;
      this._maybeEmitEnd();
      return false;
    }
    state.buffer.push(chunk);
    state.reading = false;
    if (state.flowing) {
      this._flow();
    } else {
      var self = this;
      defer(function () { self.emit("readable"); });
    }
    return state.buffer.length < 16;
  };

  // Pull one chunk synchronously (paused mode). With no argument returns the next buffered chunk or
  // null; triggers a _read() when the buffer is empty and upstream has not ended.
  Readable.prototype.read = function () {
    var state = this._readableState;
    if (state.buffer.length > 0) {
      var chunk = state.buffer.shift();
      this._maybeEmitEnd();
      return chunk;
    }
    if (!state.ended && !state.reading && !state.destroyed) {
      state.reading = true;
      var self = this;
      defer(function () { if (!self._readableState.destroyed) self._read(); });
    }
    this._maybeEmitEnd();
    return null;
  };

  // Deliver buffered chunks as 'data' events while flowing; pull more from `_read` when the buffer
  // empties and upstream has not ended; emit 'end' once drained after EOF.
  Readable.prototype._flow = function () {
    var state = this._readableState;
    var self = this;
    // The reentrancy guard keeps at most one `step` loop in flight: push() resuming a drained flow
    // and the loop's own re-arm cannot stack two concurrent walkers (which would double-pull _read).
    if (state.stepScheduled || !state.flowing || state.destroyed) return;
    function step() {
      state.stepScheduled = false;
      if (!state.flowing || state.destroyed) return;
      if (state.buffer.length > 0) {
        var chunk = state.buffer.shift();
        self.emit("data", chunk);
        state.stepScheduled = true;
        defer(step);
        return;
      }
      // Buffer is empty. If upstream ended, finish; otherwise request another chunk via _read and
      // let the resulting push() resume the flow.
      if (state.ended) {
        self._maybeEmitEnd();
      } else if (!state.reading) {
        state.reading = true;
        defer(function () { if (!state.destroyed) self._read(); });
      }
    }
    state.stepScheduled = true;
    defer(step);
  };

  // Emit 'end' exactly once, after EOF with the buffer fully drained.
  Readable.prototype._maybeEmitEnd = function () {
    var state = this._readableState;
    if (state.ended && !state.endEmitted && state.buffer.length === 0) {
      state.endEmitted = true;
      this.readable = false;
      var self = this;
      defer(function () { self.emit("end"); });
    }
  };

  Readable.prototype.pause = function () {
    this._readableState.flowing = false;
    return this;
  };

  Readable.prototype.resume = function () {
    var state = this._readableState;
    if (state.flowing !== true) {
      state.flowing = true;
      this._flow();
    }
    return this;
  };

  Readable.prototype.isPaused = function () {
    return this._readableState.flowing === false;
  };

  Readable.prototype.destroy = function (err) {
    var state = this._readableState;
    if (state.destroyed) return this;
    state.destroyed = true;
    state.buffer = [];
    this.readable = false;
    var self = this;
    defer(function () {
      if (err) self.emit("error", err);
      self.emit("close");
    });
    return this;
  };

  // on('data', ...) switches the stream into flowing mode (Node's behavior). We wrap `on` so adding
  // a data listener starts the flow, and so listeners added before push() still receive the chunks.
  var readableOn = Readable.prototype.on = function (type, fn) {
    EventEmitter.prototype.on.call(this, type, fn);
    if (type === "data") {
      var state = this._readableState;
      if (state.flowing !== false) {
        state.flowing = true;
        this._flow();
      }
    }
    return this;
  };
  Readable.prototype.addListener = readableOn;

  Readable.prototype.pipe = function (dest, options) {
    var src = this;
    options = options || {};
    var endDest = options.end !== false;

    function onData(chunk) {
      var ok = dest.write(chunk);
      if (ok === false) src.pause();
    }
    function onDrain() { src.resume(); }
    function onEnd() { if (endDest) dest.end(); }
    function onError(err) {
      cleanup();
      if (dest.listenerCount("error") > 0) dest.emit("error", err);
    }

    function cleanup() {
      src.removeListener("data", onData);
      src.removeListener("end", onEnd);
      src.removeListener("error", onError);
      dest.removeListener("drain", onDrain);
    }

    src.on("data", onData);
    src.on("end", onEnd);
    src.on("error", onError);
    dest.on("drain", onDrain);
    dest.emit("pipe", src);
    return dest;
  };

  // Async iteration: `for await (const chunk of readable)`. Each next() resolves to the next chunk
  // or { done: true } at end, and rejects on 'error'.
  Readable.prototype[Symbol.asyncIterator] = function () {
    var self = this;
    var state = this._readableState;
    var queue = [];      // buffered chunks awaiting a pending next()
    var pending = [];     // pending { resolve, reject } awaiting a chunk
    var done = false;
    var error = null;

    function onData(chunk) {
      if (pending.length > 0) pending.shift().resolve({ value: chunk, done: false });
      else queue.push(chunk);
    }
    function onEnd() {
      done = true;
      while (pending.length > 0) pending.shift().resolve({ value: undefined, done: true });
    }
    function onError(err) {
      error = err;
      while (pending.length > 0) pending.shift().reject(err);
    }

    self.on("data", onData);
    self.on("end", onEnd);
    self.on("error", onError);

    return {
      next: function () {
        if (queue.length > 0) return Promise.resolve({ value: queue.shift(), done: false });
        if (error) return Promise.reject(error);
        if (done) return Promise.resolve({ value: undefined, done: true });
        return new Promise(function (resolve, reject) {
          pending.push({ resolve: resolve, reject: reject });
        });
      },
      return: function () {
        self.removeListener("data", onData);
        self.removeListener("end", onEnd);
        self.removeListener("error", onError);
        return Promise.resolve({ value: undefined, done: true });
      },
      "throw": function (err) {
        self.removeListener("data", onData);
        self.removeListener("end", onEnd);
        self.removeListener("error", onError);
        return Promise.reject(err);
      }
    };
  };

  // Readable.from(iterable | asyncIterable): a readable that pulls from the iterator on demand.
  Readable.from = function (iterable, options) {
    var r = new Readable(options || {});
    var iterator;
    if (iterable && typeof iterable[Symbol.asyncIterator] === "function") {
      iterator = iterable[Symbol.asyncIterator]();
    } else if (iterable && typeof iterable[Symbol.iterator] === "function") {
      iterator = iterable[Symbol.iterator]();
    } else {
      throw new TypeError("Readable.from() requires an iterable");
    }
    var reading = false;
    r._read = function () {
      if (reading) return;
      reading = true;
      Promise.resolve(iterator.next()).then(
        function (res) {
          reading = false;
          if (res.done) {
            r.push(null);
          } else {
            r.push(res.value);
          }
        },
        function (err) { reading = false; r.destroy(err); }
      );
    };
    return r;
  };

  // ============================================================================================
  // Writable
  // ============================================================================================
  function Writable(options) {
    // See Readable: a Duplex/Transform `this` inherits EventEmitter but not Writable, so guard on the
    // shared base so `Writable.call(this, ...)` initializes the writable side in place.
    if (!(this instanceof EventEmitter)) return new Writable(options);
    EventEmitter.call(this);
    options = options || {};
    this._writableState = {
      buffer: [],          // chunks queued while a _write is in flight
      writing: false,
      ended: false,        // end() called
      finished: false,     // 'finish' emitted
      destroyed: false,
      needDrain: false,
      objectMode: !!options.objectMode,
      finalCalled: false
    };
    if (typeof options.write === "function") this._write = options.write;
    if (typeof options.final === "function") this._final = options.final;
    this.writable = true;
  }
  inheritEmitter(Writable);

  Writable.prototype._write = function (chunk, encoding, cb) { cb(); };

  Writable.prototype.write = function (chunk, encoding, cb) {
    var state = this._writableState;
    if (typeof encoding === "function") { cb = encoding; encoding = undefined; }
    if (state.ended) {
      var err = new Error("write after end");
      if (typeof cb === "function") defer(function () { cb(err); });
      this.emit("error", err);
      return false;
    }
    state.buffer.push({ chunk: chunk, encoding: encoding, cb: cb });
    if (!state.writing) this._processBuffer();
    var backpressured = state.buffer.length >= 16;
    if (backpressured) state.needDrain = true;
    return !backpressured;
  };

  // Drain the write buffer one chunk at a time, invoking _write and waiting for its callback.
  Writable.prototype._processBuffer = function () {
    var state = this._writableState;
    var self = this;
    if (state.writing || state.destroyed) return;
    if (state.buffer.length === 0) {
      if (state.needDrain) {
        state.needDrain = false;
        defer(function () { self.emit("drain"); });
      }
      if (state.ended && !state.finalCalled) self._finishMaybe();
      return;
    }
    var entry = state.buffer.shift();
    state.writing = true;
    var called = false;
    this._write(entry.chunk, entry.encoding, function (err) {
      if (called) return;
      called = true;
      state.writing = false;
      if (typeof entry.cb === "function") entry.cb(err);
      if (err) { self.emit("error", err); return; }
      defer(function () { self._processBuffer(); });
    });
  };

  Writable.prototype.end = function (chunk, encoding, cb) {
    var state = this._writableState;
    if (typeof chunk === "function") { cb = chunk; chunk = undefined; encoding = undefined; }
    else if (typeof encoding === "function") { cb = encoding; encoding = undefined; }
    if (chunk !== undefined && chunk !== null) this.write(chunk, encoding);
    state.ended = true;
    if (typeof cb === "function") this.once("finish", cb);
    if (!state.writing) this._finishMaybe();
    return this;
  };

  // Run _final (if any) once writes are flushed, then emit 'finish' and 'close' exactly once.
  Writable.prototype._finishMaybe = function () {
    var state = this._writableState;
    var self = this;
    if (state.finished || state.destroyed) return;
    if (state.writing || state.buffer.length > 0) return;
    if (this._final && !state.finalCalled) {
      state.finalCalled = true;
      this._final(function (err) {
        if (err) { self.emit("error", err); return; }
        self._emitFinish();
      });
      return;
    }
    this._emitFinish();
  };

  Writable.prototype._emitFinish = function () {
    var state = this._writableState;
    var self = this;
    if (state.finished) return;
    state.finished = true;
    this.writable = false;
    defer(function () {
      self.emit("finish");
      self.emit("close");
    });
  };

  Writable.prototype.destroy = function (err) {
    var state = this._writableState;
    if (state.destroyed) return this;
    state.destroyed = true;
    this.writable = false;
    var self = this;
    defer(function () {
      if (err) self.emit("error", err);
      self.emit("close");
    });
    return this;
  };

  // ============================================================================================
  // Duplex — a readable and a writable side over one object.
  // ============================================================================================
  function Duplex(options) {
    if (!(this instanceof Duplex)) return new Duplex(options);
    Readable.call(this, options);
    Writable.call(this, options);
    this.allowHalfOpen = options && options.allowHalfOpen !== undefined
      ? !!options.allowHalfOpen : true;
  }
  // Duplex's prototype carries the Readable surface; the Writable methods are copied on top.
  Duplex.prototype = Object.create(Readable.prototype);
  Duplex.prototype.constructor = Duplex;
  (function () {
    var skip = { constructor: true };
    var names = Object.getOwnPropertyNames(Writable.prototype);
    for (var i = 0; i < names.length; i++) {
      var name = names[i];
      if (skip[name]) continue;
      // Don't clobber the readable `on`/`addListener` wrappers — the writable base uses the plain
      // EventEmitter ones, and the readable wrapper already calls through to it.
      if (name === "on" || name === "addListener") continue;
      Duplex.prototype[name] = Writable.prototype[name];
    }
  })();

  // ============================================================================================
  // Transform — writable input mapped through _transform to readable output.
  // ============================================================================================
  function Transform(options) {
    if (!(this instanceof Transform)) return new Transform(options);
    Duplex.call(this, options);
    options = options || {};
    if (typeof options.transform === "function") this._transform = options.transform;
    if (typeof options.flush === "function") this._flush = options.flush;
    var self = this;
    // The writable side feeds _transform; pushing inside the callback queues readable output.
    this._write = function (chunk, encoding, cb) {
      self._transform(chunk, encoding, function (err, data) {
        if (err) { cb(err); return; }
        if (data !== undefined && data !== null) self.push(data);
        cb();
      });
    };
    // When the writable side finishes, run _flush then signal readable EOF.
    this._final = function (cb) {
      if (self._flush) {
        self._flush(function (err, data) {
          if (err) { cb(err); return; }
          if (data !== undefined && data !== null) self.push(data);
          self.push(null);
          cb();
        });
      } else {
        self.push(null);
        cb();
      }
    };
  }
  Transform.prototype = Object.create(Duplex.prototype);
  Transform.prototype.constructor = Transform;
  Transform.prototype._transform = function (chunk, encoding, cb) {
    cb(null, chunk);
  };

  // PassThrough — the identity transform.
  function PassThrough(options) {
    if (!(this instanceof PassThrough)) return new PassThrough(options);
    Transform.call(this, options);
  }
  PassThrough.prototype = Object.create(Transform.prototype);
  PassThrough.prototype.constructor = PassThrough;
  PassThrough.prototype._transform = function (chunk, encoding, cb) {
    cb(null, chunk);
  };

  // ============================================================================================
  // finished(stream, [options], cb) — invoke cb(err) once the stream completes; returns cleanup fn.
  // ============================================================================================
  function finished(stream, options, cb) {
    if (typeof options === "function") { cb = options; options = {}; }
    options = options || {};
    var called = false;
    function done(err) {
      if (called) return;
      called = true;
      cleanup();
      defer(function () { cb(err); });
    }
    function onEnd() {
      // A readable is done at 'end'; a pure writable is done at 'finish'.
      if (!stream.writable || stream._writableState === undefined) done();
      else if (stream._writableState && stream._writableState.finished) done();
    }
    function onFinish() {
      if (!stream.readable || stream._readableState === undefined) done();
      else if (stream._readableState && stream._readableState.endEmitted) done();
    }
    function onError(err) { done(err); }
    function onClose() { done(); }

    function cleanup() {
      stream.removeListener("end", onEnd);
      stream.removeListener("finish", onFinish);
      stream.removeListener("error", onError);
      stream.removeListener("close", onClose);
    }

    stream.on("error", onError);
    if (stream._readableState !== undefined) stream.on("end", onEnd);
    if (stream._writableState !== undefined) stream.on("finish", onFinish);
    stream.on("close", onClose);

    return cleanup;
  }

  // ============================================================================================
  // pipeline(source, ...transforms, destination, cb) — pipe a chain, propagating errors.
  // ============================================================================================
  function pipeline() {
    var args = Array.prototype.slice.call(arguments);
    var cb = typeof args[args.length - 1] === "function" ? args.pop() : function () {};
    if (args.length < 2) {
      throw new TypeError("pipeline requires at least two streams");
    }
    // A plain iterable/array as the first argument becomes a Readable source.
    var streams = args.map(function (s, i) {
      if (i === 0 && s && typeof s[Symbol.iterator] === "function" &&
          typeof s.pipe !== "function") {
        return Readable.from(s);
      }
      return s;
    });

    var finished_ = false;
    function finish(err) {
      if (finished_) return;
      finished_ = true;
      defer(function () { cb(err); });
    }

    var last = streams[streams.length - 1];

    // Wire error handling on every stream, and resolution on the destination.
    for (var i = 0; i < streams.length; i++) {
      (function (s, isLast) {
        s.on("error", function (err) {
          // Tear down the rest of the chain and report the first error.
          for (var k = 0; k < streams.length; k++) {
            if (streams[k] !== s && typeof streams[k].destroy === "function") {
              streams[k].destroy();
            }
          }
          finish(err);
        });
        if (isLast) {
          // Destination completion: 'finish' for writables, 'end' for a terminal readable.
          if (s._writableState !== undefined) s.on("finish", function () { finish(); });
          else if (s._readableState !== undefined) s.on("end", function () { finish(); });
          s.on("close", function () { finish(); });
        }
      })(streams[i], i === streams.length - 1);
    }

    // Pipe each stage into the next.
    for (var j = 0; j < streams.length - 1; j++) {
      streams[j].pipe(streams[j + 1]);
    }

    return last;
  }

  // ============================================================================================
  // The legacy `Stream` base, and the exports namespace.
  // ============================================================================================
  // Node's `require('stream')` value is the legacy `Stream` constructor with the class hierarchy and
  // helpers hung off it. We expose that same `Stream` base (so `require('stream').Stream` and the
  // base prototype's `pipe` are available), but the *module value itself* is a plain namespace object
  // carrying `Readable`/`Writable`/`Duplex`/`Transform`/`PassThrough`/`pipeline`/`finished`/`promises`
  // — the surface consumers actually use (`require('stream').Readable`, etc.). The only fidelity gap
  // is `typeof require('stream')` (`'object'` here vs Node's `'function'`); the base is reachable as
  // `.Stream` for code that depends on it.
  function Stream(options) {
    EventEmitter.call(this);
  }
  inheritEmitter(Stream);
  Stream.prototype.pipe = Readable.prototype.pipe;

  // `stream/promises` interop: promise-returning pipeline/finished.
  var promises = {
    pipeline: function () {
      var streams = Array.prototype.slice.call(arguments);
      return new Promise(function (resolve, reject) {
        streams.push(function (err) { if (err) reject(err); else resolve(); });
        pipeline.apply(null, streams);
      });
    },
    finished: function (stream, options) {
      return new Promise(function (resolve, reject) {
        finished(stream, options || {}, function (err) {
          if (err) reject(err); else resolve();
        });
      });
    }
  };

  var exports = {
    Stream: Stream,
    Readable: Readable,
    Writable: Writable,
    Duplex: Duplex,
    Transform: Transform,
    PassThrough: PassThrough,
    pipeline: pipeline,
    finished: finished,
    EventEmitter: EventEmitter,
    promises: promises
  };
  // Mirror the namespace onto the legacy base too, so either access shape works.
  Stream.Stream = Stream;
  Stream.Readable = Readable;
  Stream.Writable = Writable;
  Stream.Duplex = Duplex;
  Stream.Transform = Transform;
  Stream.PassThrough = PassThrough;
  Stream.pipeline = pipeline;
  Stream.finished = finished;
  Stream.promises = promises;

  return exports;
})()"#;

/// Uniform per-module entry. Returns the `node:stream` exports object (the `Stream` base with the
/// `Readable`/`Writable`/`Duplex`/`Transform`/`PassThrough` classes and the `pipeline`/`finished`
/// helpers attached), built once lazily on first import (tenet 2).
///
/// Builds the exports by parsing and evaluating [`STREAM_SOURCE`] once in the current realm. The
/// engine owns all the resulting state (the constructors, their prototypes, the per-instance buffers
/// that live as ordinary JS properties), so this function holds no `Global` handles and performs no
/// per-instance allocation. Any failure to parse or evaluate the fixed in-tree source is surfaced as
/// [`InstallError::Nova`] rather than panicking, so a mis-edit can never take down the host.
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    mut gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    let source_text = JsString::from_string(agent, STREAM_SOURCE.to_owned(), gc.nogc());
    let realm = agent.current_realm(gc.nogc());
    let script = parse_script(agent, source_text, realm, true, None, gc.nogc()).map_err(|diags| {
        let message = diags
            .iter()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        InstallError::Nova(format!("failed to parse node:stream source: {message}"))
    })?;

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
                "evaluating node:stream source threw: {message}"
            )));
        }
    };

    let value = value.bind(gc.into_nogc());
    Object::try_from(value).map_err(|_| {
        InstallError::Nova("node:stream source did not produce an exports object".to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::STREAM_SOURCE;
    use crate::JsRuntime;
    use serde_json::json;

    /// Evaluate `body` with the `stream` exports bound to the global `stream`.
    ///
    /// The CJS `require` bridge is owned by a separate loader module, so these unit tests do not
    /// depend on it: they exercise the *actual* `STREAM_SOURCE` by evaluating it directly through the
    /// real Nova engine and binding its completion value (the `Stream` namespace) onto the realm
    /// global. This proves the module end-to-end exactly as `install` will once `require` is wired.
    fn eval(body: &str) -> serde_json::Value {
        let mut rt = JsRuntime::with_node_compat();
        let prelude = format!(
            "globalThis.stream = (0, eval)({});\n",
            serde_json::to_string(STREAM_SOURCE).expect("source encodes as a JSON string literal")
        );
        rt.eval(&format!("{prelude}{body}"))
            .expect("script should evaluate without error")
    }

    #[test]
    fn exports_carry_the_class_hierarchy_and_helpers() {
        let v = eval(
            "[typeof stream.Readable, typeof stream.Writable, typeof stream.Duplex, \
              typeof stream.Transform, typeof stream.PassThrough, \
              typeof stream.pipeline, typeof stream.finished]",
        );
        assert_eq!(
            v,
            json!([
                "function", "function", "function", "function", "function", "function", "function"
            ])
        );
    }

    #[test]
    fn readable_from_emits_each_chunk_then_ends() {
        // Readable.from([...]) must deliver every chunk as a 'data' event and then 'end'. The chunks
        // and the end flag land across the microtask drain that happens between evals.
        let mut rt = JsRuntime::with_node_compat();
        let prelude = format!(
            "globalThis.stream = (0, eval)({});\n",
            serde_json::to_string(STREAM_SOURCE).unwrap()
        );
        let immediate = rt
            .eval(&format!(
                "{prelude}\
                 globalThis.__chunks = [];\
                 globalThis.__ended = false;\
                 const r = stream.Readable.from(['a', 'b', 'c']);\
                 r.on('data', (c) => globalThis.__chunks.push(c));\
                 r.on('end', () => {{ globalThis.__ended = true; }});\
                 globalThis.__chunks.length"
            ))
            .unwrap();
        // Nothing has flowed synchronously.
        assert_eq!(immediate, json!(0));
        // After the drain the chunks arrived in order and 'end' fired.
        assert_eq!(rt.eval("globalThis.__chunks").unwrap(), json!(["a", "b", "c"]));
        assert_eq!(rt.eval("globalThis.__ended").unwrap(), json!(true));
    }

    #[test]
    fn transform_maps_each_written_chunk() {
        let mut rt = JsRuntime::with_node_compat();
        let prelude = format!(
            "globalThis.stream = (0, eval)({});\n",
            serde_json::to_string(STREAM_SOURCE).unwrap()
        );
        rt.eval(&format!(
            "{prelude}\
             globalThis.__out = [];\
             const upper = new stream.Transform({{\
               transform(chunk, enc, cb) {{ cb(null, String(chunk).toUpperCase()); }}\
             }});\
             upper.on('data', (c) => globalThis.__out.push(c));\
             upper.write('foo');\
             upper.write('bar');\
             upper.end();"
        ))
        .unwrap();
        // The transform output surfaces after the microtask drain between evals.
        assert_eq!(rt.eval("globalThis.__out").unwrap(), json!(["FOO", "BAR"]));
    }

    #[test]
    fn pipeline_pipes_readable_through_transform_to_writable_and_calls_back() {
        // The canonical pipeline: a source Readable -> a mapping Transform -> a collecting Writable,
        // with the completion callback firing once the destination finishes.
        let mut rt = JsRuntime::with_node_compat();
        let prelude = format!(
            "globalThis.stream = (0, eval)({});\n",
            serde_json::to_string(STREAM_SOURCE).unwrap()
        );
        rt.eval(&format!(
            "{prelude}\
             globalThis.__collected = [];\
             globalThis.__cbErr = 'unset';\
             const src = stream.Readable.from([1, 2, 3]);\
             const double = new stream.Transform({{\
               transform(chunk, enc, cb) {{ cb(null, chunk * 2); }}\
             }});\
             const sink = new stream.Writable({{\
               write(chunk, enc, cb) {{ globalThis.__collected.push(chunk); cb(); }}\
             }});\
             stream.pipeline(src, double, sink, (err) => {{\
               globalThis.__cbErr = err ? String(err) : null;\
             }});"
        ))
        .unwrap();
        // After the drain: the chunks flowed through the transform, doubled, into the sink, and the
        // pipeline callback fired with no error.
        assert_eq!(rt.eval("globalThis.__collected").unwrap(), json!([2, 4, 6]));
        assert_eq!(rt.eval("globalThis.__cbErr").unwrap(), json!(null));
    }

    #[test]
    fn writable_emits_finish_after_end() {
        let mut rt = JsRuntime::with_node_compat();
        let prelude = format!(
            "globalThis.stream = (0, eval)({});\n",
            serde_json::to_string(STREAM_SOURCE).unwrap()
        );
        rt.eval(&format!(
            "{prelude}\
             globalThis.__finished = false;\
             globalThis.__written = [];\
             const w = new stream.Writable({{\
               write(chunk, enc, cb) {{ globalThis.__written.push(chunk); cb(); }}\
             }});\
             w.on('finish', () => {{ globalThis.__finished = true; }});\
             w.write('x');\
             w.write('y');\
             w.end();"
        ))
        .unwrap();
        assert_eq!(rt.eval("globalThis.__written").unwrap(), json!(["x", "y"]));
        assert_eq!(rt.eval("globalThis.__finished").unwrap(), json!(true));
    }

    #[test]
    fn finished_invokes_callback_when_readable_ends() {
        let mut rt = JsRuntime::with_node_compat();
        let prelude = format!(
            "globalThis.stream = (0, eval)({});\n",
            serde_json::to_string(STREAM_SOURCE).unwrap()
        );
        rt.eval(&format!(
            "{prelude}\
             globalThis.__done = 'unset';\
             const r = stream.Readable.from(['only']);\
             r.on('data', () => {{}});\
             stream.finished(r, (err) => {{ globalThis.__done = err ? String(err) : 'ok'; }});"
        ))
        .unwrap();
        assert_eq!(rt.eval("globalThis.__done").unwrap(), json!("ok"));
    }

    #[test]
    fn passthrough_forwards_chunks_unchanged() {
        let mut rt = JsRuntime::with_node_compat();
        let prelude = format!(
            "globalThis.stream = (0, eval)({});\n",
            serde_json::to_string(STREAM_SOURCE).unwrap()
        );
        rt.eval(&format!(
            "{prelude}\
             globalThis.__pt = [];\
             const pt = new stream.PassThrough();\
             pt.on('data', (c) => globalThis.__pt.push(c));\
             pt.write('p');\
             pt.write('q');\
             pt.end();"
        ))
        .unwrap();
        assert_eq!(rt.eval("globalThis.__pt").unwrap(), json!(["p", "q"]));
    }

    #[test]
    fn async_iteration_yields_each_chunk() {
        // `for await (const c of readable)` collects the chunks in order, proving the
        // Symbol.asyncIterator path settles through the event loop.
        let mut rt = JsRuntime::with_node_compat();
        let prelude = format!(
            "globalThis.stream = (0, eval)({});\n",
            serde_json::to_string(STREAM_SOURCE).unwrap()
        );
        rt.eval(&format!(
            "{prelude}\
             globalThis.__iter = [];\
             (async () => {{\
               const r = stream.Readable.from(['m', 'n']);\
               for await (const c of r) globalThis.__iter.push(c);\
             }})();"
        ))
        .unwrap();
        assert_eq!(rt.eval("globalThis.__iter").unwrap(), json!(["m", "n"]));
    }
}
