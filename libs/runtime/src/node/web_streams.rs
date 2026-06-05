//! WHATWG Streams globals backing module — the native primitives behind the `ReadableStream`,
//! `WritableStream`, and `TransformStream` globals that Bun and Cloudflare Workers (`nodejs_compat`)
//! expose.
//!
//! This is a **globals-only** leaf, the WinterCG analogue of `text_encoding`/`fetch`: it is wired by
//! [`crate::node::globals`] through the lazy self-replacing accessors and the hidden native-module
//! slot, NOT registered in [`crate::node::BUILTINS`] (these are not importable `node:` specifiers).
//! It exposes the uniform [`install`] seam so the streams bootstrap can pull the stream classes the
//! same way the URL/TextEncoder/fetch families do.
//!
//! ## Why this module is bootstrapped in JS
//!
//! The WHATWG stream classes are pure value-level JS: chunk queues, reader/writer locks, and
//! backpressure expressed through `Promise`s and `Promise` resolvers parked in a pending list.
//! Re-expressing that against Nova's GC-handle FFI would mean threading scoped `Global<…>` handles
//! through reactions that close over user `start`/`pull`/`transform` callbacks — exactly the
//! allocation-heavy, `unsafe`-adjacent boilerplate the architecture forbids (it is the same reasoning
//! `node:util`/`node:assert` document). So this module materializes by evaluating ONE self-contained
//! bootstrap script (see [`WEB_STREAMS_BOOTSTRAP`]) the first time the streams family is touched. The
//! script's completion value is the exports object `{ ReadableStream, WritableStream, TransformStream }`;
//! the shared registry caches that rooted object, so the parse+evaluate cost is paid at most once per
//! runtime and unused streams cost nothing (tenet 2).
//!
//! ## Event-loop integration
//!
//! Reads and writes return `Promise`s. A `read()` that arrives before a chunk is enqueued parks its
//! `(resolve, reject)` resolver in a pending-reader list; the controller's `enqueue`/`close`/`error`
//! settle those parked resolvers. Because every settle goes through the realm's `Promise` machinery,
//! the resolution jobs are enqueued on the runtime's microtask queue and are driven to completion by
//! [`crate::node::event_loop::run_until_idle`] (which `JsRuntime::eval*` runs after every script). The
//! TransformStream pumps a chunk through the user `transform` and re-enqueues it onto the readable side
//! via the readable controller, so a `writer.write(x)` settles a subsequent `reader.read()` after one
//! turn of the loop. No busy-waiting, no thread blocking — pure microtask plumbing.

use nova_vm::ecmascript::{Agent, Object, Value, parse_script, script_evaluation};
use nova_vm::engine::Bindable;

use crate::node::core::{InstallError, NodeCtx};
use crate::node::GcScope;

/// The self-contained WHATWG-streams implementation, evaluated once on first touch of the family.
///
/// An IIFE whose completion value is `{ ReadableStream, WritableStream, TransformStream }`. It
/// references only realm intrinsics (`Promise`, `Object`, `Symbol`, `TypeError`, `Array`), so it
/// allocates only the closures it exports — no Rust-side heap, no `unsafe` (tenets 1 & 3).
///
/// Backpressure / settling is done entirely with `Promise`s: pending `read()`s park their resolver,
/// the controller settles them on `enqueue`/`close`/`error`, and every settle rides the realm's
/// microtask queue (drained by the host event loop). That is why a `read()` issued before its chunk
/// exists still resolves once the producer enqueues — one turn of the loop later.
const WEB_STREAMS_BOOTSTRAP: &str = r#"(function () {
  'use strict';

  function isCallable(f) { return typeof f === "function"; }
  function invoke(method, thisArg, args) {
    // Run a (possibly user-supplied) algorithm and normalize its result to a resolved Promise so a
    // throw becomes a rejection and a returned thenable is awaited. Algorithms that are absent resolve
    // to undefined.
    if (!isCallable(method)) return Promise.resolve(undefined);
    try { return Promise.resolve(method.apply(thisArg, args)); }
    catch (e) { return Promise.reject(e); }
  }

  // ===========================================================================================
  // ReadableStream
  // ===========================================================================================

  function ReadableStreamDefaultController(stream) {
    this._stream = stream;
    this._queue = [];           // buffered chunks awaiting a reader
    this._closeRequested = false;
    this._pulling = false;      // a pull() call is in flight
    this._pullAgain = false;    // enqueue/read asked for another pull while one was in flight
    this._started = false;
  }
  ReadableStreamDefaultController.prototype.enqueue = function (chunk) {
    var stream = this._stream;
    if (this._closeRequested || stream._state !== "readable") {
      throw new TypeError("Cannot enqueue a chunk into a closed or errored readable stream");
    }
    // Hand the chunk straight to the oldest waiting reader if one is parked; otherwise buffer it.
    if (stream._readRequests.length > 0) {
      var req = stream._readRequests.shift();
      req.resolve({ value: chunk, done: false });
    } else {
      this._queue.push(chunk);
    }
    pullIfNeeded(this);
  };
  ReadableStreamDefaultController.prototype.close = function () {
    var stream = this._stream;
    if (this._closeRequested || stream._state !== "readable") {
      throw new TypeError("Cannot close an already-closed or errored readable stream");
    }
    this._closeRequested = true;
    if (this._queue.length === 0) finishClose(stream);
  };
  ReadableStreamDefaultController.prototype.error = function (e) {
    errorReadable(this._stream, e);
  };
  Object.defineProperty(ReadableStreamDefaultController.prototype, "desiredSize", {
    get: function () {
      var s = this._stream._state;
      if (s === "errored") return null;
      if (s === "closed") return 0;
      return 1 - this._queue.length; // HWM of 1 (count strategy); negative => backpressure
    },
    configurable: true
  });

  function finishClose(stream) {
    if (stream._state !== "readable") return;
    stream._state = "closed";
    // Drain any waiting readers with a done result; settle closed.
    while (stream._readRequests.length > 0) {
      stream._readRequests.shift().resolve({ value: undefined, done: true });
    }
    stream._closedResolve();
  }
  function errorReadable(stream, e) {
    if (stream._state !== "readable") return;
    stream._state = "errored";
    stream._storedError = e;
    stream._controller._queue = [];
    while (stream._readRequests.length > 0) {
      stream._readRequests.shift().reject(e);
    }
    stream._closedReject(e);
  }
  function pullIfNeeded(controller) {
    var stream = controller._stream;
    if (!controller._started || stream._state !== "readable") return;
    // Only pull when there is demand (a parked reader or an empty buffer below HWM) and the source
    // has a pull algorithm.
    var shouldPull = controller._queue.length === 0 || stream._readRequests.length > 0;
    if (!shouldPull) return;
    if (controller._pulling) { controller._pullAgain = true; return; }
    if (!isCallable(controller._pullAlgorithm)) return;
    controller._pulling = true;
    invoke(controller._pullAlgorithm, undefined, [controller]).then(
      function () {
        controller._pulling = false;
        if (controller._pullAgain) { controller._pullAgain = false; pullIfNeeded(controller); }
      },
      function (e) { errorReadable(stream, e); }
    );
  }

  function ReadableStream(underlyingSource, strategy) {
    underlyingSource = underlyingSource || {};
    this._state = "readable";       // "readable" | "closed" | "errored"
    this._storedError = undefined;
    this._reader = null;            // the active default reader, if locked
    this._readRequests = [];        // parked { resolve, reject } for read()s awaiting a chunk
    var self = this;
    this._closedPromise = new Promise(function (res, rej) { self._closedResolve = res; self._closedReject = rej; });
    // The closed promise must not become an unhandled rejection if no one observes it.
    this._closedPromise.catch(function () {});

    var controller = new ReadableStreamDefaultController(this);
    controller._pullAlgorithm = isCallable(underlyingSource.pull)
      ? function (c) { return underlyingSource.pull.call(underlyingSource, c); } : null;
    controller._cancelAlgorithm = isCallable(underlyingSource.cancel)
      ? function (reason) { return underlyingSource.cancel.call(underlyingSource, reason); } : null;
    this._controller = controller;

    // Run start(), then mark started and kick the first pull. start() may be sync or return a promise.
    invoke(underlyingSource.start, underlyingSource, [controller]).then(
      function () { controller._started = true; pullIfNeeded(controller); },
      function (e) { errorReadable(self, e); }
    );
  }
  Object.defineProperty(ReadableStream.prototype, "locked", {
    get: function () { return this._reader !== null; }, configurable: true
  });
  ReadableStream.prototype.getReader = function (options) {
    if (options && options.mode === "byob") {
      throw new TypeError("BYOB readers are not supported in this runtime");
    }
    if (this.locked) throw new TypeError("ReadableStream is already locked to a reader");
    return new ReadableStreamDefaultReader(this);
  };
  ReadableStream.prototype.cancel = function (reason) {
    if (this.locked) return Promise.reject(new TypeError("Cannot cancel a locked stream"));
    return readableCancel(this, reason);
  };
  ReadableStream.prototype[Symbol.asyncIterator] = function () {
    var reader = this.getReader();
    return {
      next: function () {
        return reader.read().then(function (r) {
          if (r.done) { reader.releaseLock(); }
          return r;
        });
      },
      "return": function (value) {
        return reader.cancel(value).then(function () {
          reader.releaseLock();
          return { value: value, done: true };
        });
      },
      "throw": function (e) {
        reader.releaseLock();
        return Promise.reject(e);
      },
      // The async iterator IS its own iterable so `for await (const x of stream)` works.
      _isAsyncStreamIterator: true
    };
  };
  ReadableStream.prototype.values = ReadableStream.prototype[Symbol.asyncIterator];

  function readableCancel(stream, reason) {
    if (stream._state === "closed") return Promise.resolve(undefined);
    if (stream._state === "errored") return Promise.reject(stream._storedError);
    // Empty the buffer, close the stream, and run the source's cancel algorithm.
    stream._controller._queue = [];
    var cancelAlgorithm = stream._controller._cancelAlgorithm;
    finishClose(stream);
    return invoke(cancelAlgorithm, undefined, [reason]).then(function () { return undefined; });
  }

  function ReadableStreamDefaultReader(stream) {
    this._stream = stream;
    stream._reader = this;
    var self = this;
    if (stream._state === "readable") {
      this._closedPromise = new Promise(function (res, rej) { self._closedResolve = res; self._closedReject = rej; });
    } else if (stream._state === "closed") {
      this._closedPromise = Promise.resolve(undefined);
    } else {
      this._closedPromise = Promise.reject(stream._storedError);
    }
    this._closedPromise.catch(function () {});
  }
  ReadableStreamDefaultReader.prototype.read = function () {
    var stream = this._stream;
    if (stream === null) return Promise.reject(new TypeError("Reader has no associated stream (lock released)"));
    var controller = stream._controller;
    if (stream._state === "errored") return Promise.reject(stream._storedError);
    if (controller._queue.length > 0) {
      var chunk = controller._queue.shift();
      // A dequeue may unblock a pending close, or create room for another pull.
      if (controller._closeRequested && controller._queue.length === 0) { finishClose(stream); }
      else { pullIfNeeded(controller); }
      return Promise.resolve({ value: chunk, done: false });
    }
    if (stream._state === "closed") return Promise.resolve({ value: undefined, done: true });
    // No buffered chunk and the stream is still open: park the request and ask for a pull.
    var request;
    var p = new Promise(function (res, rej) { request = { resolve: res, reject: rej }; });
    stream._readRequests.push(request);
    pullIfNeeded(controller);
    return p;
  };
  ReadableStreamDefaultReader.prototype.cancel = function (reason) {
    var stream = this._stream;
    if (stream === null) return Promise.reject(new TypeError("Reader has no associated stream (lock released)"));
    return readableCancel(stream, reason);
  };
  ReadableStreamDefaultReader.prototype.releaseLock = function () {
    var stream = this._stream;
    if (stream === null) return;
    if (stream._readRequests.length > 0) {
      throw new TypeError("Cannot release a reader with outstanding read requests");
    }
    // A still-active reader's closed promise rejects on release (per spec).
    if (stream._state === "readable" && isCallable(this._closedReject)) {
      this._closedReject(new TypeError("Reader was released"));
    }
    stream._reader = null;
    this._stream = null;
  };
  Object.defineProperty(ReadableStreamDefaultReader.prototype, "closed", {
    get: function () { return this._closedPromise; }, configurable: true
  });
  // When the stream finishes closing / errors, settle a live reader's closed promise too.
  function settleReaderClosed(stream) {
    var reader = stream._reader;
    if (!reader) return;
    if (stream._state === "closed" && isCallable(reader._closedResolve)) reader._closedResolve(undefined);
    else if (stream._state === "errored" && isCallable(reader._closedReject)) reader._closedReject(stream._storedError);
  }
  // Patch finishClose/errorReadable to also settle the reader's closed promise.
  var _finishClose = finishClose;
  finishClose = function (stream) { _finishClose(stream); settleReaderClosed(stream); };
  var _errorReadable = errorReadable;
  errorReadable = function (stream, e) { _errorReadable(stream, e); settleReaderClosed(stream); };

  // ===========================================================================================
  // WritableStream
  // ===========================================================================================

  function WritableStreamDefaultController(stream) {
    this._stream = stream;
  }
  WritableStreamDefaultController.prototype.error = function (e) {
    if (this._stream._state === "writable") errorWritable(this._stream, e);
  };
  Object.defineProperty(WritableStreamDefaultController.prototype, "signal", {
    get: function () { return this._stream._abortSignal; }, configurable: true
  });

  function WritableStream(underlyingSink, strategy) {
    underlyingSink = underlyingSink || {};
    this._state = "writable";       // "writable" | "closed" | "erroring" | "errored"
    this._storedError = undefined;
    this._writer = null;
    this._queue = Promise.resolve(); // serializes writes/close (one in-flight algorithm at a time)
    this._abortSignal = (typeof AbortController === "function") ? new AbortController().signal : undefined;
    var controller = new WritableStreamDefaultController(this);
    this._controller = controller;
    this._writeAlgorithm = isCallable(underlyingSink.write)
      ? function (chunk, c) { return underlyingSink.write.call(underlyingSink, chunk, c); } : null;
    this._closeAlgorithm = isCallable(underlyingSink.close)
      ? function () { return underlyingSink.close.call(underlyingSink); } : null;
    this._abortAlgorithm = isCallable(underlyingSink.abort)
      ? function (reason) { return underlyingSink.abort.call(underlyingSink, reason); } : null;
    var self = this;
    this._startPromise = invoke(underlyingSink.start, underlyingSink, [controller]);
    this._startPromise.catch(function (e) { errorWritable(self, e); });
  }
  Object.defineProperty(WritableStream.prototype, "locked", {
    get: function () { return this._writer !== null; }, configurable: true
  });
  WritableStream.prototype.getWriter = function () {
    if (this.locked) throw new TypeError("WritableStream is already locked to a writer");
    return new WritableStreamDefaultWriter(this);
  };
  WritableStream.prototype.abort = function (reason) {
    if (this.locked) return Promise.reject(new TypeError("Cannot abort a locked stream"));
    return writableAbort(this, reason);
  };
  WritableStream.prototype.close = function () {
    if (this.locked) return Promise.reject(new TypeError("Cannot close a locked stream"));
    return writableClose(this);
  };

  function errorWritable(stream, e) {
    if (stream._state === "errored") return;
    stream._state = "errored";
    stream._storedError = e;
  }
  function writableWrite(stream, chunk) {
    if (stream._state === "errored") return Promise.reject(stream._storedError);
    if (stream._state !== "writable") return Promise.reject(new TypeError("Cannot write to a closing or closed stream"));
    var writeAlgorithm = stream._writeAlgorithm, controller = stream._controller;
    // Chain after the start promise and all prior queued operations so writes run in order, one at a
    // time. Each step rides the microtask queue, so the host event loop drives them to settlement.
    var step = stream._queue.then(function () {
      if (stream._state === "errored") throw stream._storedError;
      return invoke(writeAlgorithm, undefined, [chunk, controller]);
    });
    // Keep the internal queue chained but swallow its rejection there (the caller's promise reports it).
    stream._queue = step.then(function () {}, function (e) { errorWritable(stream, e); });
    return stream._startPromise.then(function () { return step; });
  }
  function writableClose(stream) {
    if (stream._state === "closed") return Promise.resolve(undefined);
    if (stream._state === "errored") return Promise.reject(stream._storedError);
    var closeAlgorithm = stream._closeAlgorithm;
    var result = stream._queue.then(function () {
      if (stream._state === "errored") throw stream._storedError;
      return invoke(closeAlgorithm, undefined, []);
    });
    stream._queue = result.then(function () {}, function () {});
    return result.then(function () {
      if (stream._state === "writable") stream._state = "closed";
      return undefined;
    });
  }
  function writableAbort(stream, reason) {
    if (stream._state === "closed") return Promise.resolve(undefined);
    if (stream._state === "errored") return Promise.reject(stream._storedError);
    var abortAlgorithm = stream._abortAlgorithm;
    errorWritable(stream, reason !== undefined ? reason : new TypeError("The stream was aborted"));
    if (stream._abortSignal && stream._abortSignal.aborted === false) {
      // best-effort: nothing to dispatch here beyond marking errored.
    }
    return invoke(abortAlgorithm, undefined, [reason]).then(function () { return undefined; });
  }

  function WritableStreamDefaultWriter(stream) {
    this._stream = stream;
    stream._writer = this;
    var self = this;
    this._readyPromise = stream._startPromise.then(function () { return undefined; });
    this._readyPromise.catch(function () {});
    if (stream._state === "writable") {
      this._closedPromise = new Promise(function (res, rej) { self._closedResolve = res; self._closedReject = rej; });
    } else if (stream._state === "closed") {
      this._closedPromise = Promise.resolve(undefined);
    } else {
      this._closedPromise = Promise.reject(stream._storedError);
    }
    this._closedPromise.catch(function () {});
  }
  WritableStreamDefaultWriter.prototype.write = function (chunk) {
    var stream = this._stream;
    if (stream === null) return Promise.reject(new TypeError("Writer has no associated stream (lock released)"));
    return writableWrite(stream, chunk);
  };
  WritableStreamDefaultWriter.prototype.close = function () {
    var stream = this._stream;
    if (stream === null) return Promise.reject(new TypeError("Writer has no associated stream (lock released)"));
    var self = this;
    return writableClose(stream).then(function (v) {
      if (isCallable(self._closedResolve)) self._closedResolve(undefined);
      return v;
    }, function (e) {
      if (isCallable(self._closedReject)) self._closedReject(e);
      throw e;
    });
  };
  WritableStreamDefaultWriter.prototype.abort = function (reason) {
    var stream = this._stream;
    if (stream === null) return Promise.reject(new TypeError("Writer has no associated stream (lock released)"));
    return writableAbort(stream, reason);
  };
  WritableStreamDefaultWriter.prototype.releaseLock = function () {
    var stream = this._stream;
    if (stream === null) return;
    if (stream._state === "writable" && isCallable(this._closedReject)) {
      this._closedReject(new TypeError("Writer was released"));
    }
    stream._writer = null;
    this._stream = null;
  };
  Object.defineProperty(WritableStreamDefaultWriter.prototype, "closed", {
    get: function () { return this._closedPromise; }, configurable: true
  });
  Object.defineProperty(WritableStreamDefaultWriter.prototype, "ready", {
    get: function () { return this._readyPromise; }, configurable: true
  });
  Object.defineProperty(WritableStreamDefaultWriter.prototype, "desiredSize", {
    get: function () {
      var s = this._stream._state;
      if (s === "errored") return null;
      if (s === "closed") return 0;
      return 1;
    }, configurable: true
  });

  // ===========================================================================================
  // TransformStream  (writable -> transformer.transform -> readable)
  // ===========================================================================================

  function TransformStream(transformer, writableStrategy, readableStrategy) {
    transformer = transformer || {};
    var readableController = null;
    var transformAlgorithm = isCallable(transformer.transform)
      ? function (chunk, c) { return transformer.transform.call(transformer, chunk, c); }
      : function (chunk, c) { c.enqueue(chunk); }; // identity transform
    var flushAlgorithm = isCallable(transformer.flush)
      ? function (c) { return transformer.flush.call(transformer, c); } : null;

    // The transform controller bridges the writable side into the readable side.
    var transformController = {
      enqueue: function (chunk) {
        if (readableController) readableController.enqueue(chunk);
      },
      terminate: function () {
        if (readableController) { try { readableController.close(); } catch (e) {} }
      },
      error: function (e) {
        if (readableController) readableController.error(e);
      }
    };
    Object.defineProperty(transformController, "desiredSize", {
      get: function () { return readableController ? readableController.desiredSize : null; },
      configurable: true
    });

    this.readable = new ReadableStream({
      start: function (c) { readableController = c; }
    }, readableStrategy);

    this.writable = new WritableStream({
      write: function (chunk) {
        return invoke(transformAlgorithm, undefined, [chunk, transformController]);
      },
      close: function () {
        var p = flushAlgorithm ? invoke(flushAlgorithm, undefined, [transformController]) : Promise.resolve();
        return p.then(function () { transformController.terminate(); });
      },
      abort: function (reason) { transformController.error(reason); }
    }, writableStrategy);

    // Run transformer.start once the readable controller exists.
    var self = this;
    Promise.resolve().then(function () {
      return invoke(transformer.start, transformer, [transformController]);
    }).catch(function (e) { transformController.error(e); });
  }

  return {
    ReadableStream: ReadableStream,
    WritableStream: WritableStream,
    TransformStream: TransformStream
  };
})()"#;

/// Uniform per-module entry. Materializes the WHATWG-streams classes by evaluating
/// [`WEB_STREAMS_BOOTSTRAP`] once against the current realm and returning the exports object
/// `{ ReadableStream, WritableStream, TransformStream }`.
///
/// Built lazily the first time one of the stream globals is touched (tenet 2); the shared registry /
/// global accessor caches the returned object, so the parse+evaluate cost is paid at most once.
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    mut gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    // The bootstrap source is `&'static`, so this is the only allocation the module makes beyond the
    // closures the script itself creates.
    let source = nova_vm::ecmascript::String::from_static_str(agent, WEB_STREAMS_BOOTSTRAP, gc.nogc());
    let realm = agent.current_realm(gc.nogc());

    let script = parse_script(agent, source, realm, true, None, gc.nogc()).map_err(|diagnostics| {
        let message = diagnostics
            .iter()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        InstallError::Nova(if message.is_empty() {
            "failed to parse web streams bootstrap".to_owned()
        } else {
            message
        })
    })?;

    let value = match script_evaluation(agent, script.unbind(), gc.reborrow()) {
        Ok(value) => value.unbind(),
        Err(error) => {
            let message = error
                .value()
                .unbind()
                .string_repr(agent, gc.reborrow())
                .to_string_lossy(agent)
                .into_owned();
            return Err(InstallError::Nova(message));
        }
    };

    let gc = gc.into_nogc();
    let value: Value = value.bind(gc);
    Object::try_from(value)
        .map_err(|_| InstallError::Nova("web streams bootstrap did not evaluate to an object".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nova_vm::ecmascript::{
        GcAgent, InternalMethods, PropertyDescriptor, PropertyKey, String as JsString,
        parse_script as parse, script_evaluation as run, unwrap_try,
    };
    use nova_vm::engine::{Bindable, Scopable};

    use crate::node::core::{EnvMap, HostState};
    use crate::node::event_loop::run_until_idle;

    /// Run `script` in a realm that has the WHATWG stream classes installed as globals, draining the
    /// event loop afterward so promise-driven reads/writes settle, then return the completion value
    /// rendered as a Rust `String`.
    ///
    /// This drives the real [`install`] on a live agent that uses a [`HostState`] as its host hooks —
    /// the same hooks `JsRuntime::with_node_compat` wires — so the microtask queue the streams rely on
    /// is the production one and [`run_until_idle`] is the production pump. The test script is expected
    /// to stash its observable result on `globalThis.__out` (a string), which we read back after the
    /// loop is idle.
    fn eval_streams(script: &str) -> String {
        // The HostState must outlive the agent; declare it first and extend its borrow to `'static`
        // exactly as `JsRuntime::with_node_compat` does (the agent is dropped at end of scope first).
        let state = Box::new(HostState::new(
            std::env::current_dir().unwrap(),
            EnvMap::new(),
        ));
        // SAFETY: `state` outlives `agent` (declared before, dropped after); the borrow is only used as
        // Nova's host hooks for the lifetime of this function. Mirrors `core::extend_lifetime`'s contract.
        let hooks: &'static HostState = unsafe { crate::node::core::extend_lifetime(&*state) };
        let mut agent = GcAgent::new(Default::default(), hooks);
        let realm = agent.create_default_realm();

        let out = agent.run_in_realm(&realm, |agent, mut gc| {
            // Build the stream classes and bind each as a global so the test script can reach them.
            let ctx = crate::node::core::NodeCtx::new(hooks);
            let exports = install(agent, &ctx, gc.reborrow())
                .expect("web streams install")
                .unbind()
                .scope(agent, gc.nogc());

            for name in ["ReadableStream", "WritableStream", "TransformStream"] {
                let key = PropertyKey::from_str(agent, name, gc.nogc());
                let value = {
                    let exports = exports.get(agent).bind(gc.nogc());
                    match exports.try_get(agent, key, exports.into(), None, gc.nogc()) {
                        std::ops::ControlFlow::Continue(
                            nova_vm::ecmascript::TryGetResult::Value(v),
                        ) => v.unbind(),
                        _ => panic!("missing stream class {name}"),
                    }
                };
                let global = agent.current_realm(gc.nogc()).global_object(agent);
                unwrap_try(global.try_define_own_property(
                    agent,
                    key,
                    PropertyDescriptor::new_data_descriptor(value),
                    None,
                    gc.nogc(),
                ));
            }

            // Run the test script. It returns nothing meaningful synchronously; it parks its result on
            // `globalThis.__out` once its promises settle.
            let src = JsString::from_string(agent, script.to_owned(), gc.nogc());
            let r = agent.current_realm(gc.nogc());
            let parsed = parse(agent, src, r, true, None, gc.nogc()).expect("parse test script");
            run(agent, parsed.unbind(), gc.reborrow()).expect("script ran");

            // Drain microtasks (and any timers) so the streams' promise chains settle.
            run_until_idle(agent, hooks.event_loop(), None, gc.reborrow()).expect("loop drained");

            // Read back `globalThis.__out`.
            let key = PropertyKey::from_static_str(agent, "__out", gc.nogc());
            let global = agent.current_realm(gc.nogc()).global_object(agent);
            let value = match global.try_get(agent, key, global.into(), None, gc.nogc()) {
                std::ops::ControlFlow::Continue(nova_vm::ecmascript::TryGetResult::Value(v)) => {
                    v.unbind()
                }
                _ => Value::Undefined,
            };
            value
                .bind(gc.nogc())
                .unbind()
                .string_repr(agent, gc.reborrow())
                .to_string_lossy(agent)
                .into_owned()
        });

        // Drop the agent before `state` (run_in_realm's borrow has ended); explicit for clarity.
        drop(agent);
        drop(state);
        out
    }

    #[test]
    fn install_returns_the_three_stream_classes() {
        let present = eval_streams(
            "globalThis.__out = String(\
               typeof ReadableStream === 'function' && \
               typeof WritableStream === 'function' && \
               typeof TransformStream === 'function');",
        );
        assert_eq!(present, "true");
    }

    #[test]
    fn readable_enqueue_then_read_round_trips() {
        // A source that enqueues two chunks and closes; reading drains them in order then reports done.
        let out = eval_streams(
            "var rs = new ReadableStream({\
               start: function (c) { c.enqueue('a'); c.enqueue('b'); c.close(); }\
             });\
             var reader = rs.getReader();\
             var seen = [];\
             function step() {\
               return reader.read().then(function (r) {\
                 if (r.done) { globalThis.__out = seen.join(',') + '|done'; return; }\
                 seen.push(r.value);\
                 return step();\
               });\
             }\
             step();",
        );
        assert_eq!(out, "a,b|done");
    }

    #[test]
    fn read_before_enqueue_settles_via_event_loop() {
        // Issue a read() while the buffer is empty (parking the resolver), then enqueue from a pull.
        // The read must settle once the loop drains — proving the pending-reader plumbing.
        let out = eval_streams(
            "var pulled = false;\
             var rs = new ReadableStream({\
               pull: function (c) { if (!pulled) { pulled = true; c.enqueue('late'); c.close(); } }\
             });\
             var reader = rs.getReader();\
             reader.read().then(function (r) { globalThis.__out = 'got:' + r.value; });",
        );
        assert_eq!(out, "got:late");
    }

    #[test]
    fn async_iteration_consumes_all_chunks() {
        let out = eval_streams(
            "var rs = new ReadableStream({\
               start: function (c) { c.enqueue(1); c.enqueue(2); c.enqueue(3); c.close(); }\
             });\
             (async function () {\
               var sum = 0;\
               for await (var x of rs) { sum += x; }\
               globalThis.__out = 'sum:' + sum;\
             })();",
        );
        assert_eq!(out, "sum:6");
    }

    #[test]
    fn writable_stream_collects_writes() {
        // A sink that pushes each chunk into an array; writer.write/close settle through the loop.
        let out = eval_streams(
            "var collected = [];\
             var ws = new WritableStream({\
               write: function (chunk) { collected.push(chunk); },\
               close: function () {}\
             });\
             var writer = ws.getWriter();\
             writer.write('x');\
             writer.write('y');\
             writer.write('z');\
             writer.close().then(function () { globalThis.__out = collected.join('-'); });",
        );
        assert_eq!(out, "x-y-z");
    }

    #[test]
    fn writable_write_rejects_after_sink_error() {
        // A sink that throws on the second write errors the stream; the writer's close rejects.
        let out = eval_streams(
            "var n = 0;\
             var ws = new WritableStream({\
               write: function (chunk) { n++; if (n === 2) throw new Error('boom'); }\
             });\
             var writer = ws.getWriter();\
             writer.write('ok');\
             writer.write('bad').then(\
               function () { globalThis.__out = 'unexpected-resolve'; },\
               function (e) { globalThis.__out = 'rejected:' + e.message; }\
             );",
        );
        assert_eq!(out, "rejected:boom");
    }

    #[test]
    fn transform_stream_uppercases_chunks() {
        // Write lowercase chunks to the writable side; read uppercased chunks off the readable side.
        let out = eval_streams(
            "var ts = new TransformStream({\
               transform: function (chunk, controller) { controller.enqueue(chunk.toUpperCase()); }\
             });\
             var writer = ts.writable.getWriter();\
             var reader = ts.readable.getReader();\
             writer.write('foo');\
             writer.write('bar');\
             writer.close();\
             var seen = [];\
             function step() {\
               return reader.read().then(function (r) {\
                 if (r.done) { globalThis.__out = seen.join(','); return; }\
                 seen.push(r.value);\
                 return step();\
               });\
             }\
             step();",
        );
        assert_eq!(out, "FOO,BAR");
    }

    #[test]
    fn transform_stream_flush_runs_on_close() {
        // The transformer's flush() can enqueue a final chunk before the readable side closes.
        let out = eval_streams(
            "var ts = new TransformStream({\
               transform: function (chunk, c) { c.enqueue(chunk); },\
               flush: function (c) { c.enqueue('END'); }\
             });\
             var writer = ts.writable.getWriter();\
             var reader = ts.readable.getReader();\
             writer.write('a');\
             writer.close();\
             var seen = [];\
             function step() {\
               return reader.read().then(function (r) {\
                 if (r.done) { globalThis.__out = seen.join(','); return; }\
                 seen.push(r.value);\
                 return step();\
               });\
             }\
             step();",
        );
        assert_eq!(out, "a,END");
    }

    #[test]
    fn get_reader_throws_when_already_locked() {
        let out = eval_streams(
            "var rs = new ReadableStream({ start: function (c) { c.close(); } });\
             rs.getReader();\
             var threw = false;\
             try { rs.getReader(); } catch (e) { threw = e instanceof TypeError; }\
             globalThis.__out = String(threw && rs.locked === true);",
        );
        assert_eq!(out, "true");
    }

    #[test]
    fn reader_releaselock_unlocks_the_stream() {
        let out = eval_streams(
            "var rs = new ReadableStream({ start: function (c) { c.close(); } });\
             var reader = rs.getReader();\
             reader.releaseLock();\
             globalThis.__out = String(rs.locked === false);",
        );
        assert_eq!(out, "true");
    }

    #[test]
    fn readable_error_propagates_to_read() {
        let out = eval_streams(
            "var rs = new ReadableStream({\
               start: function (c) { c.error(new Error('nope')); }\
             });\
             var reader = rs.getReader();\
             reader.read().then(\
               function () { globalThis.__out = 'unexpected'; },\
               function (e) { globalThis.__out = 'err:' + e.message; }\
             );",
        );
        assert_eq!(out, "err:nope");
    }
}
