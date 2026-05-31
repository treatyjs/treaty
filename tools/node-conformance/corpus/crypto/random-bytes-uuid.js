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

// Node test/parallel-style: node:crypto CSPRNG surface — randomBytes, randomFillSync, randomUUID,
// randomInt. The outputs are non-deterministic, so the assertions pin the *contracts* Node
// guarantees: exact requested length, in-place fill returning the same view, the RFC 4122 v4 UUID
// shape, distinctness across draws, and randomInt staying within its half-open range.
const crypto = require("node:crypto");

test("randomBytes returns the exact requested length", () => {
  for (const n of [0, 1, 16, 32, 257]) {
    const b = crypto.randomBytes(n);
    assert.strictEqual(b.length, n, "randomBytes(" + n + ").length");
  }
});

test("randomBytes(0) is empty and randomBytes(n) is not all-zero", () => {
  assert.strictEqual(crypto.randomBytes(0).length, 0, "empty draw");
  // 64 CSPRNG bytes being entirely zero is astronomically unlikely; guards a no-op generator.
  const big = crypto.randomBytes(64);
  let anyNonZero = false;
  for (let i = 0; i < big.length; i++) {
    if (big[i] !== 0) anyNonZero = true;
  }
  assert.ok(anyNonZero, "a 64-byte draw must contain a non-zero byte");
});

test("two randomBytes draws differ", () => {
  // Two independent 32-byte draws colliding is a ~2^-256 event; inequality is the practical contract.
  const a = crypto.randomBytes(32);
  const b = crypto.randomBytes(32);
  let same = a.length === b.length;
  for (let i = 0; same && i < a.length; i++) {
    if (a[i] !== b[i]) same = false;
  }
  assert.ok(!same, "two random draws must not be byte-identical");
});

test("randomFillSync fills the view in place and returns it", () => {
  const buf = new Uint8Array(16);
  const returned = crypto.randomFillSync(buf);
  assert.strictEqual(returned, buf, "randomFillSync returns the same view");
  let anyNonZero = false;
  for (let i = 0; i < buf.length; i++) {
    if (buf[i] !== 0) anyNonZero = true;
  }
  assert.ok(anyNonZero, "the buffer must have been filled");
});

test("randomUUID has the canonical v4 shape", () => {
  const u = crypto.randomUUID();
  assert.strictEqual(u.length, 36, "UUID string length");
  assert.strictEqual(u.split("-").length, 5, "UUID has five hyphen groups");
  const v4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
  assert.ok(v4.test(u), "matches the RFC 4122 v4 pattern: " + u);
  // The version nibble is pinned to '4' and the variant nibble to one of 8/9/a/b.
  assert.strictEqual(u[14], "4", "version digit");
  assert.ok("89ab".indexOf(u[19]) >= 0, "variant digit");
});

test("two randomUUIDs are distinct", () => {
  assert.notStrictEqual(crypto.randomUUID(), crypto.randomUUID(), "UUIDs must differ");
});

test("randomInt stays within the half-open range", () => {
  for (let i = 0; i < 50; i++) {
    const n = crypto.randomInt(10, 20);
    assert.ok(n >= 10 && n < 20, "randomInt(10,20) in [10,20): got " + n);
  }
});
