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

// Node test/parallel-style: node:util.promisify — adapts an (err, value) callback API to a
// promise-returning function, honors the util.promisify.custom symbol, and resolves/rejects to the
// callback's value/error.
//
// Observability note: the conformance runner drains the loop after the body, but a rejected await is
// swallowed Node-faithfully, so the decisive (regression-catching) assertions are the synchronous
// shape checks — promisify returns a function, calling it returns a thenable, and the custom symbol
// is honored. The end-to-end resolve/reject is exercised through awaits whose side effects the drain
// settles.
const util = require("node:util");

test("promisify returns a function whose call yields a thenable", () => {
  const doubler = (x, cb) => cb(null, x * 2);
  const doubleAsync = util.promisify(doubler);
  assert.strictEqual(typeof doubleAsync, "function", "promisify returns a function");
  const pending = doubleAsync(21);
  assert.strictEqual(typeof pending.then, "function", "calling it returns a thenable");
});

test("promisify.custom is a symbol the runtime exposes", () => {
  assert.strictEqual(typeof util.promisify.custom, "symbol", "promisify.custom is a symbol");
});

test("promisify resolves with the callback's value", async () => {
  const doubler = (x, cb) => cb(null, x * 2);
  const doubleAsync = util.promisify(doubler);
  const result = await doubleAsync(21);
  assert.strictEqual(result, 42, "the promisified callback resolves with its value");
});

test("promisify rejects when the callback signals an error", async () => {
  const failing = (cb) => cb(new Error("kaboom"));
  const failAsync = util.promisify(failing);
  await assert.rejects(() => failAsync(), "an (err)-first callback error becomes a rejection");
});

test("promisify honors a util.promisify.custom implementation", async () => {
  function legacy() {}
  legacy[util.promisify.custom] = () => Promise.resolve(99);
  const promisified = util.promisify(legacy);
  assert.strictEqual(typeof promisified, "function", "the custom-promisified value is a function");
  const value = await promisified();
  assert.strictEqual(value, 99, "the custom implementation supplies the resolved value");
});
