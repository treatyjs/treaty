// === treaty node-conformance harness shim (begin) ===
// A self-contained Node test/parallel-style harness: assert.* + test()/describe().
// Reaches builtins only via require() (the harness eval scope has no bare globals).
const { isDeepStrictEqual } = require("node:util");
function AssertionError(message) {
  const err = new Error(message || "assertion failed");
  err.name = "AssertionError";
  return err;
}
function assert(value, message) {
  if (!value) throw AssertionError(message || ("expected truthy value, got: " + String(value)));
}
assert.ok = assert;
assert.fail = function (message) {
  throw AssertionError(message || "failed");
};
assert.strictEqual = function (actual, expected, message) {
  if (!Object.is(actual, expected)) {
    throw AssertionError(message || (String(actual) + " !== " + String(expected)));
  }
};
assert.notStrictEqual = function (actual, expected, message) {
  if (Object.is(actual, expected)) {
    throw AssertionError(message || ("unexpected equality: " + String(actual)));
  }
};
assert.deepStrictEqual = function (actual, expected, message) {
  if (!isDeepStrictEqual(actual, expected)) {
    throw AssertionError(
      message || ("not deeply equal: " + JSON.stringify(actual) + " vs " + JSON.stringify(expected))
    );
  }
};
assert.throws = function (fn, message) {
  let threw = false;
  try {
    fn();
  } catch (e) {
    threw = true;
  }
  if (!threw) throw AssertionError(message || "expected function to throw");
};
assert.rejects = function (promiseOrFn, message) {
  const p = typeof promiseOrFn === "function" ? promiseOrFn() : promiseOrFn;
  return Promise.resolve(p).then(
    function () {
      throw AssertionError(message || "expected promise to reject");
    },
    function () {
      return undefined;
    }
  );
};
function test(name, fn) {
  const result = fn();
  // An async test returns a promise; surface its rejection as a thrown failure during the drain.
  if (result && typeof result.then === "function") {
    return result.then(undefined, function (e) {
      throw AssertionError("test '" + name + "' rejected: " + (e && e.message ? e.message : String(e)));
    });
  }
  return result;
}
const it = test;
function describe(name, fn) {
  return fn();
}
// === treaty node-conformance harness shim (end) ===

// Node test/parallel-style: node:events EventEmitter core registration surface —
// on/addListener, emit with multiple args, listenerCount, removeListener, off alias, and the
// listeners()/rawListeners()/eventNames() introspection accessors.
const EventEmitter = require("node:events");

test("on and addListener are the same registration, emit forwards every argument", () => {
  const ee = new EventEmitter();
  assert.strictEqual(ee.addListener, ee.on, "addListener is an alias of on");
  let received = null;
  ee.on("data", (a, b, c) => {
    received = [a, b, c];
  });
  const handled = ee.emit("data", 1, 2, 3);
  assert.strictEqual(handled, true, "emit reports a listener handled the event");
  assert.deepStrictEqual(received, [1, 2, 3], "every emitted argument reaches the listener");
});

test("emit returns false and the listener is untouched when nothing is registered", () => {
  const ee = new EventEmitter();
  assert.strictEqual(ee.emit("nobody", 1), false, "emit with no listener returns false");
});

test("listenerCount tracks registration and removeListener removes exactly one", () => {
  const ee = new EventEmitter();
  const a = () => {};
  const b = () => {};
  ee.on("x", a);
  ee.on("x", b);
  assert.strictEqual(ee.listenerCount("x"), 2, "two listeners registered");
  ee.removeListener("x", a);
  assert.strictEqual(ee.listenerCount("x"), 1, "removeListener drops exactly one");
  assert.strictEqual(ee.off, ee.removeListener, "off is an alias of removeListener");
  ee.off("x", b);
  assert.strictEqual(ee.listenerCount("x"), 0, "off removes the remaining listener");
});

test("removeAllListeners(name) clears only the named event", () => {
  const ee = new EventEmitter();
  ee.on("a", () => {});
  ee.on("a", () => {});
  ee.on("b", () => {});
  ee.removeAllListeners("a");
  assert.strictEqual(ee.listenerCount("a"), 0, "named event is cleared");
  assert.strictEqual(ee.listenerCount("b"), 1, "other events are left intact");
});

test("listeners() returns the registered functions and eventNames() the active events", () => {
  const ee = new EventEmitter();
  const fn = () => {};
  ee.on("evt", fn);
  ee.on("other", () => {});
  const listeners = ee.listeners("evt");
  assert.strictEqual(Array.isArray(listeners), true, "listeners() returns an array");
  assert.strictEqual(listeners.length, 1, "one listener registered for evt");
  assert.strictEqual(listeners[0], fn, "the registered function is returned");
  const names = ee.eventNames().slice().sort();
  assert.deepStrictEqual(names, ["evt", "other"], "eventNames lists every active event");
});

test("a listener may be registered for several events independently", () => {
  const ee = new EventEmitter();
  const seen = [];
  const fn = (tag) => seen.push(tag);
  ee.on("one", fn);
  ee.on("two", fn);
  ee.emit("one", "1");
  ee.emit("two", "2");
  assert.deepStrictEqual(seen, ["1", "2"], "the same function fires for each distinct event");
});
