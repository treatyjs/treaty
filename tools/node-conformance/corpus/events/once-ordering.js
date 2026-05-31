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

// Node test/parallel-style: node:events ordering + one-shot semantics —
// once fires at most once, FIFO listener order, prependListener/prependOnceListener prepend, and
// the meta 'newListener' event fires before a listener is added.
const EventEmitter = require("node:events");

test("once fires at most one time then is removed", () => {
  const ee = new EventEmitter();
  let count = 0;
  ee.once("y", () => {
    count += 1;
  });
  assert.strictEqual(ee.listenerCount("y"), 1, "the once listener is registered");
  ee.emit("y");
  ee.emit("y");
  assert.strictEqual(count, 1, "the once listener fired exactly once");
  assert.strictEqual(ee.listenerCount("y"), 0, "the once listener removed itself after firing");
});

test("listeners fire in registration order (FIFO)", () => {
  const ee = new EventEmitter();
  const log = [];
  ee.on("e", () => log.push("first"));
  ee.on("e", () => log.push("second"));
  ee.on("e", () => log.push("third"));
  ee.emit("e");
  assert.deepStrictEqual(log, ["first", "second", "third"], "listeners run in the order added");
});

test("prependListener puts a listener at the front of the queue", () => {
  const ee = new EventEmitter();
  const log = [];
  ee.on("e", () => log.push("existing"));
  ee.prependListener("e", () => log.push("prepended"));
  ee.emit("e");
  assert.deepStrictEqual(log, ["prepended", "existing"], "prepended listener runs first");
});

test("prependOnceListener prepends and is one-shot", () => {
  const ee = new EventEmitter();
  const log = [];
  ee.on("e", () => log.push("existing"));
  ee.prependOnceListener("e", () => log.push("once"));
  ee.emit("e");
  ee.emit("e");
  assert.deepStrictEqual(log, ["once", "existing", "existing"], "prepend-once runs first, only once");
});

test("the 'newListener' meta event fires with the name before the listener is added", () => {
  const ee = new EventEmitter();
  let announced = null;
  let countWhenAnnounced = -1;
  ee.on("newListener", (name) => {
    announced = name;
    // The real listener has not been added yet when 'newListener' fires.
    countWhenAnnounced = ee.listenerCount("real");
  });
  ee.on("real", () => {});
  assert.strictEqual(announced, "real", "newListener reported the event name");
  assert.strictEqual(countWhenAnnounced, 0, "newListener fires before the listener is installed");
});
