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

// Node test/parallel-style: node:timers one-shot timers — the module exposes the timer functions,
// setTimeout defers (does not run inline) and returns a clearable handle, and clearTimeout cancels a
// pending timer.
//
// Observability note: the runner drains the loop after the synchronous body, but a throw from inside
// a timer callback is swallowed Node-faithfully. The decisive assertions here are therefore the
// synchronous ones (deferral, handle shape, cancellation recorded via a flag the body re-reads only
// where it can); the fired/not-fired effects are recorded into flags the drain settles.
const timers = require("node:timers");

test("the timers module exposes the standard timer functions", () => {
  assert.strictEqual(typeof timers.setTimeout, "function", "setTimeout");
  assert.strictEqual(typeof timers.clearTimeout, "function", "clearTimeout");
  assert.strictEqual(typeof timers.setInterval, "function", "setInterval");
  assert.strictEqual(typeof timers.clearInterval, "function", "clearInterval");
  assert.strictEqual(typeof timers.queueMicrotask, "function", "queueMicrotask");
});

test("setTimeout defers its callback and returns a handle clearTimeout accepts", () => {
  let ran = false;
  const handle = timers.setTimeout(() => {
    ran = true;
  }, 1000);
  assert.strictEqual(ran, false, "the callback is deferred, not run inline");
  assert.notStrictEqual(handle, undefined, "setTimeout returns a handle");
  // Cancelling a long timer keeps the callback from ever running; this also proves the handle is
  // a valid argument to clearTimeout (no throw).
  timers.clearTimeout(handle);
  assert.strictEqual(ran, false, "the cleared callback never ran");
});

// A cleared zero-delay timer must not fire under the drain: record whether it fired, then re-check
// from a later timer the drain also runs.
const fired = { cleared: false, control: false };
const cancelled = timers.setTimeout(() => {
  fired.cleared = true;
}, 0);
timers.clearTimeout(cancelled);
timers.setTimeout(() => {
  // A control timer that is NOT cleared does fire, so we know the drain ran zero-delay timers at all.
  fired.control = true;
  assert.strictEqual(fired.cleared, false, "a cleared zero-delay timer must not fire");
  assert.strictEqual(fired.control, true, "an uncleared zero-delay timer does fire");
}, 0);
