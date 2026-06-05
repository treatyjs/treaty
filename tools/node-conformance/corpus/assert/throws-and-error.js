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

// Node test/parallel-style: node:assert synchronous throws/doesNotThrow matching (constructor,
// RegExp, validation object, predicate) and the AssertionError value shape. Bound as `nodeAssert`
// so the module under test never shadows the harness shim's own `assert`.
const nodeAssert = require("node:assert");

test("throws detects a thrown error and matches by constructor", () => {
  // A function that throws passes throws() with no matcher.
  nodeAssert.throws(() => {
    throw new Error("boom");
  });
  // Constructor matcher: the thrown error must be an instance.
  nodeAssert.throws(() => {
    throw new TypeError("bad type");
  }, TypeError);
  // A function that does not throw makes throws() itself throw.
  assert.throws(() => nodeAssert.throws(() => 42), "non-throwing fn makes throws() throw");
  // A wrong constructor matcher rethrows the original error (a RangeError is not a TypeError).
  assert.throws(
    () =>
      nodeAssert.throws(() => {
        throw new RangeError("range");
      }, TypeError),
    "mismatched constructor rethrows"
  );
});

test("throws matches the message against a RegExp", () => {
  nodeAssert.throws(() => {
    throw new Error("value out of bounds");
  }, /out of bounds/);
  // A non-matching RegExp rethrows the original.
  assert.throws(
    () =>
      nodeAssert.throws(() => {
        throw new Error("totally different");
      }, /out of bounds/),
    "non-matching RegExp rethrows"
  );
});

test("throws matches against a validation object and a predicate", () => {
  // Validation object: each listed property must deep-strict-match the thrown error's property.
  nodeAssert.throws(
    () => {
      const e = new TypeError("nope");
      e.code = "ERR_BAD";
      throw e;
    },
    { name: "TypeError", code: "ERR_BAD" }
  );
  // Predicate matcher: returns true to accept the error.
  nodeAssert.throws(
    () => {
      throw new Error("predicate target");
    },
    (err) => err instanceof Error && err.message === "predicate target"
  );
});

test("doesNotThrow passes for a clean function and throws on an unexpected throw", () => {
  // A function that completes cleanly passes.
  nodeAssert.doesNotThrow(() => 1 + 1);
  // A function that throws makes doesNotThrow throw.
  assert.throws(
    () =>
      nodeAssert.doesNotThrow(() => {
        throw new Error("unexpected");
      }),
    "doesNotThrow surfaces an unexpected throw"
  );
});

test("AssertionError carries Node's documented shape", () => {
  let err;
  try {
    nodeAssert.strictEqual(1, 2);
  } catch (e) {
    err = e;
  }
  assert.ok(err, "strictEqual mismatch must throw");
  // It is a real Error subclass and an AssertionError.
  assert.strictEqual(err instanceof Error, true, "AssertionError is an Error");
  assert.strictEqual(err instanceof nodeAssert.AssertionError, true, "instanceof AssertionError");
  assert.strictEqual(err.name, "AssertionError", "name is AssertionError");
  assert.strictEqual(err.code, "ERR_ASSERTION", "code is ERR_ASSERTION");
  // The diagnostic fields reflect the comparison that failed.
  assert.strictEqual(err.actual, 1, "actual is the left operand");
  assert.strictEqual(err.expected, 2, "expected is the right operand");
  assert.strictEqual(err.operator, "strictEqual", "operator names the failing assertion");
  assert.strictEqual(err.generatedMessage, true, "message was generated, not user-supplied");
});

test("a user-supplied message overrides the generated one", () => {
  let err;
  try {
    nodeAssert.strictEqual(1, 2, "custom explanation");
  } catch (e) {
    err = e;
  }
  assert.strictEqual(err.message, "custom explanation", "the supplied message is used verbatim");
  assert.strictEqual(err.generatedMessage, false, "generatedMessage is false for a supplied message");
});

test("ifError throws for a non-null value and passes for null/undefined", () => {
  nodeAssert.ifError(null);
  nodeAssert.ifError(undefined);
  assert.throws(() => nodeAssert.ifError(new Error("err")), "ifError throws on a real error");
  assert.throws(() => nodeAssert.ifError("non-empty"), "ifError throws on a truthy value");
});

test("assert.strict aliases equal/deepEqual to their strict forms", () => {
  // assert.strict.equal is Object.is-based, so 1 vs '1' fails.
  assert.throws(() => nodeAssert.strict.equal(1, "1"), "strict.equal rejects cross-type coercion");
  nodeAssert.strict.equal(2, 2);
  // assert.strict.deepEqual is deepStrictEqual.
  assert.throws(
    () => nodeAssert.strict.deepEqual({ n: 1 }, { n: "1" }),
    "strict.deepEqual is type-strict"
  );
  nodeAssert.strict.deepEqual({ a: [1, 2] }, { a: [1, 2] });
});
