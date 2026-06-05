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

// Node test/parallel-style: node:util.inspect rendering + util.types predicates and
// util.isDeepStrictEqual structural equality.
const util = require("node:util");

test("inspect quotes strings and renders primitives bare", () => {
  assert.strictEqual(util.inspect("hi"), "'hi'", "a string is single-quoted");
  assert.strictEqual(util.inspect(42), "42", "a number is rendered bare");
  assert.strictEqual(util.inspect(true), "true", "a boolean is rendered bare");
  assert.strictEqual(util.inspect(null), "null", "null renders as the keyword");
  assert.strictEqual(util.inspect(undefined), "undefined", "undefined renders as the keyword");
});

test("inspect renders nested plain objects up to the default depth", () => {
  assert.strictEqual(util.inspect({ a: 1, b: 2 }), "{ a: 1, b: 2 }", "a flat object");
  assert.strictEqual(util.inspect({ a: { b: 1 } }), "{ a: { b: 1 } }", "one level of nesting");
});

test("inspect honors an explicit depth option", () => {
  assert.strictEqual(
    util.inspect({ a: { b: 1 } }, { depth: 0 }),
    "{ a: [Object] }",
    "beyond the depth limit, a nested object collapses to [Object]"
  );
});

test("inspect renders Map and Set with their sizes", () => {
  assert.strictEqual(
    util.inspect(new Map([["k", "v"]])),
    "Map(1) { 'k' => 'v' }",
    "a Map shows its size and entries"
  );
  assert.strictEqual(util.inspect(new Set([1, 2])), "Set(2) { 1, 2 }", "a Set shows its size and members");
});

test("util.types classifies built-in objects", () => {
  assert.strictEqual(util.types.isDate(new Date()), true, "isDate on a Date");
  assert.strictEqual(util.types.isDate({}), false, "isDate on a plain object");
  assert.strictEqual(util.types.isRegExp(/x/), true, "isRegExp on a regex literal");
  assert.strictEqual(util.types.isMap(new Map()), true, "isMap on a Map");
  assert.strictEqual(util.types.isSet(new Set()), true, "isSet on a Set");
  assert.strictEqual(util.types.isPromise(Promise.resolve()), true, "isPromise on a Promise");
  assert.strictEqual(util.types.isNativeError(new Error()), true, "isNativeError on an Error");
  assert.strictEqual(util.types.isArrayBuffer(new ArrayBuffer(2)), true, "isArrayBuffer");
  assert.strictEqual(util.types.isTypedArray(new Uint8Array(2)), true, "isTypedArray on a Uint8Array");
  assert.strictEqual(util.types.isAsyncFunction(async () => {}), true, "isAsyncFunction");
  assert.strictEqual(
    util.types.isGeneratorFunction(function* () {}),
    true,
    "isGeneratorFunction on a generator"
  );
});

test("isDeepStrictEqual compares structurally", () => {
  assert.strictEqual(util.isDeepStrictEqual({ a: [1, 2] }, { a: [1, 2] }), true, "deeply equal");
  assert.strictEqual(util.isDeepStrictEqual({ a: 1 }, { a: 2 }), false, "deeply unequal values");
  assert.strictEqual(util.isDeepStrictEqual([1, 2, 3], [1, 2, 3]), true, "arrays compare elementwise");
  assert.strictEqual(util.isDeepStrictEqual({ a: 1 }, { a: "1" }), false, "strict: 1 !== '1'");
});
