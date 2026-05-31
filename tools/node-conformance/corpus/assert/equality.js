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

// Node test/parallel-style: node:assert scalar equality surface
// (ok / equal / strictEqual / notStrictEqual / notEqual). The module under test is reached via
// require("node:assert") under the name `nodeAssert` so it does not shadow the harness shim's own
// `assert`; we test that the real module throws/does-not-throw exactly as Node specifies.
const nodeAssert = require("node:assert");

test("ok / assert callable accepts truthy and rejects falsy", () => {
  // Truthy values pass silently (return undefined).
  assert.strictEqual(nodeAssert.ok(1), undefined, "ok(truthy) returns undefined");
  assert.strictEqual(nodeAssert(true), undefined, "callable assert(truthy) returns undefined");
  nodeAssert.ok("non-empty");
  nodeAssert(["arrays", "are", "truthy"]);
  // Every falsy value makes ok throw.
  for (const falsy of [false, 0, "", null, undefined, NaN]) {
    assert.throws(() => nodeAssert.ok(falsy), "ok(" + String(falsy) + ") must throw");
    assert.throws(() => nodeAssert(falsy), "assert(" + String(falsy) + ") must throw");
  }
});

test("strictEqual uses Object.is semantics", () => {
  // Equal under Object.is: passes silently.
  nodeAssert.strictEqual(2 + 2, 4);
  nodeAssert.strictEqual("a", "a");
  nodeAssert.strictEqual(null, null);
  nodeAssert.strictEqual(undefined, undefined);
  // NaN is strictEqual to NaN (Object.is), unlike ===.
  nodeAssert.strictEqual(NaN, NaN);
  // Different type or value throws; +0 and -0 are NOT Object.is-equal.
  assert.throws(() => nodeAssert.strictEqual(1, "1"), "number vs string mismatch throws");
  assert.throws(() => nodeAssert.strictEqual(1, 2), "value mismatch throws");
  assert.throws(() => nodeAssert.strictEqual(0, -0), "+0 vs -0 throws under Object.is");
});

test("notStrictEqual is the negation of strictEqual", () => {
  // Different values pass.
  nodeAssert.notStrictEqual(1, 2);
  nodeAssert.notStrictEqual(1, "1");
  nodeAssert.notStrictEqual(0, -0);
  // Identical values throw.
  assert.throws(() => nodeAssert.notStrictEqual(3, 3), "identical numbers throw");
  assert.throws(() => nodeAssert.notStrictEqual(NaN, NaN), "NaN vs NaN throws (Object.is equal)");
});

test("equal / notEqual use loose == / != comparison", () => {
  // Loose equality coerces across types.
  nodeAssert.equal(1, "1");
  nodeAssert.equal(null, undefined);
  nodeAssert.equal(true, 1);
  // notEqual is its negation.
  nodeAssert.notEqual(1, 2);
  nodeAssert.notEqual(1, "2");
  // A loose-equal pair makes equal pass but notEqual throw, and vice versa.
  assert.throws(() => nodeAssert.equal(1, 2), "loose-unequal throws in equal");
  assert.throws(() => nodeAssert.notEqual(1, "1"), "loose-equal throws in notEqual");
});

test("fail always throws with the given message", () => {
  let captured;
  try {
    nodeAssert.fail("explicit failure");
  } catch (e) {
    captured = e;
  }
  assert.ok(captured, "fail must throw");
  assert.strictEqual(captured.message, "explicit failure", "fail carries its message");
});
