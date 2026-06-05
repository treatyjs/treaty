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

// Node test/parallel-style: node:timers setTimeout/clearTimeout/setInterval/clearInterval +
// queueMicrotask ordering, validated through the post-eval event-loop drain.
const timers = require("node:timers");

test("the timers module exposes the timer functions", () => {
  assert.strictEqual(typeof timers.setTimeout, "function", "setTimeout");
  assert.strictEqual(typeof timers.clearTimeout, "function", "clearTimeout");
  assert.strictEqual(typeof timers.setInterval, "function", "setInterval");
  assert.strictEqual(typeof timers.clearInterval, "function", "clearInterval");
  assert.strictEqual(typeof timers.queueMicrotask, "function", "queueMicrotask");
});

// A resolved-promise microtask must run before a zero-delay timer. We record an ordering log and
// assert it inside the timer callback, which the drain runs (a throw there becomes a Fail).
const order = [];
Promise.resolve().then(() => {
  order.push("microtask");
});
timers.queueMicrotask(() => {
  order.push("queueMicrotask");
});
timers.setTimeout(() => {
  assert.strictEqual(order[0], "microtask", "promise microtask ran before the timer");
  assert.strictEqual(order.includes("queueMicrotask"), true, "queueMicrotask ran before the timer");
  assert.strictEqual(order.indexOf("queueMicrotask") < order.length, true, "queueMicrotask ordered");
}, 0);

test("clearTimeout cancels a pending timer", () => {
  let fired = false;
  const id = timers.setTimeout(() => {
    fired = true;
  }, 0);
  timers.clearTimeout(id);
  // Schedule a later check; if the cleared timer had fired, this would observe it.
  timers.setTimeout(() => {
    assert.strictEqual(fired, false, "cleared timer must not fire");
  }, 0);
});

test("setInterval repeats until cleared", () => {
  let n = 0;
  const id = timers.setInterval(() => {
    n += 1;
    if (n >= 3) {
      timers.clearInterval(id);
    }
  }, 0);
  // The assertion on the final count runs after the interval self-clears.
  timers.setTimeout(() => {
    assert.strictEqual(n, 3, "interval fired exactly three times before clear");
  }, 0);
});
