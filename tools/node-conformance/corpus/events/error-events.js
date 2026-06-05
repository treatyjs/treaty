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

// Node test/parallel-style: node:events error semantics — an 'error' event with no listener
// throws synchronously out of emit, while a registered 'error' listener handles it.
const EventEmitter = require("node:events");

test("emit('error') with no listener throws synchronously", () => {
  const ee = new EventEmitter();
  assert.throws(() => {
    ee.emit("error", new Error("boom"));
  }, "an unhandled 'error' event must throw out of emit");
});

test("a registered 'error' listener handles emit('error') and receives the error", () => {
  const ee = new EventEmitter();
  let captured = null;
  ee.on("error", (err) => {
    captured = err;
  });
  const handled = ee.emit("error", new Error("handled"));
  assert.strictEqual(handled, true, "emit reports the error listener handled it");
  assert.strictEqual(captured instanceof Error, true, "the listener received an Error");
  assert.strictEqual(captured.message, "handled", "the listener received the emitted error");
});

test("removing the error listener restores the throwing behavior", () => {
  const ee = new EventEmitter();
  const onError = () => {};
  ee.on("error", onError);
  ee.removeListener("error", onError);
  assert.throws(() => {
    ee.emit("error", new Error("again"));
  }, "after the error listener is removed, emit('error') throws again");
});
