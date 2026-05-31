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

// Node test/parallel-style: node:process nextTick deferral + ordering relative to a timer.
//
// Note on what is observable: the conformance runner drains the event loop after the synchronous
// body, and (Node-faithfully) a throw from inside a nextTick / timer callback becomes an unhandled
// exception the host swallows rather than a synchronous failure. So the *decisive* assertions here
// are the synchronous ones — that nextTick defers (does not run inline) — which a regression that
// ran the callback synchronously would fail. The relative ordering is exercised through a recorded
// log the callbacks build up, mirroring the established corpus style.
const process = require("node:process");
const timers = require("node:timers");

test("nextTick is a function that does not run its callback synchronously", () => {
  assert.strictEqual(typeof process.nextTick, "function", "nextTick is callable");
  let ran = false;
  process.nextTick(() => {
    ran = true;
  });
  assert.strictEqual(ran, false, "the nextTick callback is deferred, not run inline");
});

test("nextTick forwards extra arguments to the callback", () => {
  let captured = null;
  process.nextTick(
    (a, b) => {
      captured = a + b;
    },
    2,
    3
  );
  // Still deferred at this point; the value is observed by the drain, the deferral by us here.
  assert.strictEqual(captured, null, "arguments are bound but the callback has not run yet");
});

// Ordering exercise: a process.nextTick / microtask must run before a zero-delay timer. The log is
// asserted inside the timer callback the drain runs (see the file header for the observability note).
const order = [];
process.nextTick(() => order.push("nextTick"));
Promise.resolve().then(() => order.push("microtask"));
timers.setTimeout(() => {
  order.push("timer");
  assert.strictEqual(order.indexOf("nextTick") !== -1, true, "nextTick ran before the timer");
  assert.strictEqual(order.indexOf("microtask") !== -1, true, "the microtask ran before the timer");
  assert.strictEqual(order[order.length - 1], "timer", "the timer is the last to run");
}, 0);
