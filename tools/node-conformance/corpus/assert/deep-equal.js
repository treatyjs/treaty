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

// Node test/parallel-style: node:assert structural deep equality
// (deepStrictEqual / notDeepStrictEqual / deepEqual / notDeepEqual) over nested objects, arrays,
// Maps and Sets. The module is bound as `nodeAssert` so it does not shadow the shim's `assert`.
const nodeAssert = require("node:assert");

test("deepStrictEqual matches structurally-identical nested objects", () => {
  nodeAssert.deepStrictEqual({ a: 1, b: { c: 2 } }, { a: 1, b: { c: 2 } });
  // Key order does not matter for object deep equality.
  nodeAssert.deepStrictEqual({ a: 1, b: 2 }, { b: 2, a: 1 });
  // A differing nested value throws.
  assert.throws(
    () => nodeAssert.deepStrictEqual({ a: { b: 1 } }, { a: { b: 2 } }),
    "nested value mismatch throws"
  );
  // A missing key throws (different own-key count).
  assert.throws(
    () => nodeAssert.deepStrictEqual({ a: 1 }, { a: 1, b: 2 }),
    "extra key on expected throws"
  );
});

test("deepStrictEqual matches nested arrays and is order-sensitive", () => {
  nodeAssert.deepStrictEqual([1, [2, 3], 4], [1, [2, 3], 4]);
  nodeAssert.deepStrictEqual([{ x: 1 }, { y: 2 }], [{ x: 1 }, { y: 2 }]);
  // Arrays of differing length are not deep-equal.
  assert.throws(() => nodeAssert.deepStrictEqual([1, 2], [1, 2, 3]), "length mismatch throws");
  // Element order matters for arrays.
  assert.throws(() => nodeAssert.deepStrictEqual([1, 2], [2, 1]), "order mismatch throws");
});

test("deepStrictEqual is type-strict (no coercion) and handles NaN", () => {
  // Strict deep equality does not coerce 1 to "1".
  assert.throws(
    () => nodeAssert.deepStrictEqual({ n: 1 }, { n: "1" }),
    "number vs string is not deep-strict-equal"
  );
  // NaN deep-strict-equals NaN.
  nodeAssert.deepStrictEqual({ n: NaN }, { n: NaN });
  nodeAssert.deepStrictEqual([NaN], [NaN]);
});

test("deepStrictEqual compares Map and Set by contents", () => {
  nodeAssert.deepStrictEqual(new Map([["a", 1], ["b", 2]]), new Map([["a", 1], ["b", 2]]));
  // Set membership is order-independent.
  nodeAssert.deepStrictEqual(new Set([1, 2, 3]), new Set([3, 2, 1]));
  // A differing Map value throws.
  assert.throws(
    () => nodeAssert.deepStrictEqual(new Map([["a", 1]]), new Map([["a", 2]])),
    "Map value mismatch throws"
  );
  // A differing Set size throws.
  assert.throws(
    () => nodeAssert.deepStrictEqual(new Set([1, 2]), new Set([1, 2, 3])),
    "Set size mismatch throws"
  );
});

test("deepStrictEqual handles deeply-nested mixed structures", () => {
  const a = { id: 1, tags: ["x", "y"], meta: { nested: [{ k: [1, 2] }], flag: true } };
  const b = { id: 1, tags: ["x", "y"], meta: { nested: [{ k: [1, 2] }], flag: true } };
  nodeAssert.deepStrictEqual(a, b);
  // Mutating a single deep leaf breaks equality.
  const c = { id: 1, tags: ["x", "y"], meta: { nested: [{ k: [1, 3] }], flag: true } };
  assert.throws(() => nodeAssert.deepStrictEqual(a, c), "deep leaf mismatch throws");
});

test("notDeepStrictEqual is the negation of deepStrictEqual", () => {
  // Structurally-different values pass notDeepStrictEqual.
  nodeAssert.notDeepStrictEqual({ a: 1 }, { a: 2 });
  nodeAssert.notDeepStrictEqual([1, 2], [1, 2, 3]);
  // Structurally-equal values throw.
  assert.throws(
    () => nodeAssert.notDeepStrictEqual({ a: [1] }, { a: [1] }),
    "deeply-equal values throw in notDeepStrictEqual"
  );
});

test("deepEqual is loose: coerces leaf primitives", () => {
  // Loose deep equality coerces 1 == "1" at the leaves.
  nodeAssert.deepEqual({ n: 1 }, { n: "1" });
  // notDeepEqual is its negation.
  nodeAssert.notDeepEqual({ n: 1 }, { n: 2 });
  assert.throws(
    () => nodeAssert.notDeepEqual({ n: 1 }, { n: "1" }),
    "loose-deep-equal values throw in notDeepEqual"
  );
});
