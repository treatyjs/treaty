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

// Node test/parallel-style: node:timers ordering — a resolved-promise microtask and a
// queueMicrotask callback both run before a zero-delay timer, and setInterval repeats until cleared.
//
// Observability note (see other timers tests): the relative ordering is recorded into a log the
// callbacks build up and asserted inside the timer callback the drain runs; the synchronous facts
// (nothing has run inline yet) are the decisive, regression-catching assertions.
const timers = require("node:timers");

test("scheduling a microtask and a timer does not run either synchronously", () => {
  const log = [];
  Promise.resolve().then(() => log.push("microtask"));
  timers.queueMicrotask(() => log.push("queueMicrotask"));
  timers.setTimeout(() => log.push("timer"), 0);
  assert.strictEqual(log.length, 0, "no scheduled job runs during the synchronous body");
});

// Ordering exercise: microtasks (both a resolved-promise reaction and queueMicrotask) run before a
// zero-delay timer. Asserted from inside the timer callback the drain runs.
const order = [];
Promise.resolve().then(() => order.push("promise-microtask"));
timers.queueMicrotask(() => order.push("queued-microtask"));
timers.setTimeout(() => {
  assert.strictEqual(
    order.indexOf("promise-microtask") !== -1,
    true,
    "the promise microtask ran before the timer"
  );
  assert.strictEqual(
    order.indexOf("queued-microtask") !== -1,
    true,
    "the queued microtask ran before the timer"
  );
  assert.strictEqual(order.length, 2, "both microtasks ran before the timer fired");
}, 0);

test("setInterval repeats until clearInterval, and clearInterval accepts its handle", () => {
  let n = 0;
  const handle = timers.setInterval(() => {
    n += 1;
    if (n >= 3) {
      timers.clearInterval(handle);
    }
  }, 0);
  assert.notStrictEqual(handle, undefined, "setInterval returns a handle");
  assert.strictEqual(n, 0, "the interval callback has not run synchronously");
  // The final count is observed by the drain; a later timer re-reads it.
  timers.setTimeout(() => {
    assert.strictEqual(n, 3, "the interval fired exactly three times before clearing itself");
  }, 0);
});
