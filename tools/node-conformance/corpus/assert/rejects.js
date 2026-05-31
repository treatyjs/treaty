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

// Node test/parallel-style: node:assert async rejection helpers (rejects / doesNotReject).
// These return promises, so the tests are `async` and `await` the helper; the harness drains the
// event loop and the shim re-surfaces a rejected test promise. Bound as `nodeAssert` so the module
// under test never shadows the harness shim's own `assert`.
const nodeAssert = require("node:assert");

test("rejects resolves when the awaited promise rejects", async () => {
  // A rejecting promise satisfies rejects(): awaiting it completes without throwing.
  await nodeAssert.rejects(Promise.reject(new Error("boom")));
  // A rejecting async function (passed as a thunk) is also accepted.
  await nodeAssert.rejects(async () => {
    throw new TypeError("async boom");
  });
});

test("rejects matches the rejection against a constructor and a RegExp", async () => {
  await nodeAssert.rejects(Promise.reject(new TypeError("bad type")), TypeError);
  await nodeAssert.rejects(Promise.reject(new Error("value out of range")), /out of range/);
});

test("rejects' own promise rejects when the input does not reject", async () => {
  // rejects() of a fulfilling promise must itself reject; awaiting it should throw, which we catch
  // and assert on synchronously inside this async body.
  let threw = false;
  try {
    await nodeAssert.rejects(Promise.resolve("fulfilled"));
  } catch (e) {
    threw = true;
  }
  assert.strictEqual(threw, true, "rejects(non-rejecting) must reject");
});

test("rejects returns a thenable synchronously", () => {
  // Independent of the async outcome, the return value is a promise (verified synchronously so the
  // harness observes it deterministically).
  const p = nodeAssert.rejects(Promise.reject(new Error("x")));
  assert.strictEqual(typeof p.then, "function", "rejects returns a promise");
  // Swallow the (already-handled) rejection so it does not surface as unhandled.
  p.then(undefined, () => undefined);
});

test("doesNotReject resolves when the awaited promise fulfills", async () => {
  // A fulfilling promise satisfies doesNotReject().
  await nodeAssert.doesNotReject(Promise.resolve(42));
  await nodeAssert.doesNotReject(async () => "ok");
});

test("doesNotReject' own promise rejects when the input rejects", async () => {
  let threw = false;
  try {
    await nodeAssert.doesNotReject(Promise.reject(new Error("unexpected rejection")));
  } catch (e) {
    threw = true;
  }
  assert.strictEqual(threw, true, "doesNotReject(rejecting) must reject");
});
