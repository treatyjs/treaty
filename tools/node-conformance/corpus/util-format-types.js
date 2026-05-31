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

// Node test/parallel-style: node:util format/inspect/isDeepStrictEqual/types/promisify.
const util = require("node:util");

test("format substitutes %s, %d, %j and escapes %%", () => {
  assert.strictEqual(util.format("%s = %d", "x", 7), "x = 7", "%s and %d");
  assert.strictEqual(util.format("%j", { a: 1 }), '{"a":1}', "%j serializes JSON");
  assert.strictEqual(util.format("100%%"), "100%", "%% is a literal percent");
  assert.strictEqual(util.format("a", "b", "c"), "a b c", "extra args are space-joined");
});

test("inspect returns a string", () => {
  assert.strictEqual(typeof util.inspect({ a: 1 }), "string", "inspect of an object");
  assert.strictEqual(util.inspect("plain").length > 0, true, "inspect of a primitive is non-empty");
});

test("isDeepStrictEqual compares structurally", () => {
  assert.strictEqual(util.isDeepStrictEqual({ a: [1, 2] }, { a: [1, 2] }), true, "deep equal");
  assert.strictEqual(util.isDeepStrictEqual({ a: 1 }, { a: 2 }), false, "deep unequal");
});

test("util.types classifies built-in objects", () => {
  assert.strictEqual(util.types.isDate(new Date()), true, "isDate on a Date");
  assert.strictEqual(util.types.isDate({}), false, "isDate on a plain object");
  assert.strictEqual(util.types.isRegExp(/x/), true, "isRegExp on a regex");
});

test("promisify adapts an (err, value) callback API", async () => {
  const doubler = (x, cb) => cb(null, x * 2);
  const doubleAsync = util.promisify(doubler);
  const result = await doubleAsync(21);
  assert.strictEqual(result, 42, "promisified callback resolves with the value");
});
