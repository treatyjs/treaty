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

// Node test/parallel-style: node:events EventEmitter on/emit/once/removeListener/listenerCount and
// the unhandled-'error' throw.
const EventEmitter = require("node:events");

test("on/emit delivers the payload and reports a handler ran", () => {
  const ee = new EventEmitter();
  let seen = null;
  ee.on("ping", (payload) => {
    seen = payload;
  });
  const handled = ee.emit("ping", 42);
  assert.strictEqual(handled, true, "emit reports a listener handled the event");
  assert.strictEqual(seen, 42, "listener received the emitted payload");
});

test("listenerCount and removeListener", () => {
  const ee = new EventEmitter();
  const fn = () => {};
  ee.on("x", fn);
  assert.strictEqual(ee.listenerCount("x"), 1, "one listener after on");
  ee.removeListener("x", fn);
  assert.strictEqual(ee.listenerCount("x"), 0, "zero listeners after removeListener");
  assert.strictEqual(ee.emit("x"), false, "emit reports no listener handled it");
});

test("once fires at most one time", () => {
  const ee = new EventEmitter();
  let count = 0;
  ee.once("y", () => {
    count += 1;
  });
  ee.emit("y");
  ee.emit("y");
  assert.strictEqual(count, 1, "once listener fired exactly once");
});

test("emit('error') throws when there is no error listener", () => {
  const ee = new EventEmitter();
  assert.throws(() => {
    ee.emit("error", new Error("boom"));
  }, "unhandled 'error' must throw");
});

test("a registered error listener handles emit('error')", () => {
  const ee = new EventEmitter();
  let captured = null;
  ee.on("error", (err) => {
    captured = err.message;
  });
  ee.emit("error", new Error("handled"));
  assert.strictEqual(captured, "handled", "error listener received the error");
});
