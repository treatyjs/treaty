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

// WinterCG Web Crypto: the `crypto` global's getRandomValues / randomUUID (synchronous, in-place)
// and the asynchronous subtle.digest. These are top-level globals (not node:crypto), so they are
// referenced through globalThis to avoid relying on a bare lexical binding in the harness scope.
const webcrypto = globalThis.crypto;
assert.ok(webcrypto && typeof webcrypto.getRandomValues === "function", "crypto global is present");

test("getRandomValues fills the typed array in place and returns it", () => {
  const view = new Uint8Array(32);
  const returned = webcrypto.getRandomValues(view);
  assert.strictEqual(returned, view, "returns the same view it was given");
  let anyNonZero = false;
  for (let i = 0; i < view.length; i++) {
    if (view[i] !== 0) anyNonZero = true;
  }
  assert.ok(anyNonZero, "a 32-byte draw must contain a non-zero byte");
});

test("two getRandomValues draws differ", () => {
  const a = webcrypto.getRandomValues(new Uint8Array(32));
  const b = webcrypto.getRandomValues(new Uint8Array(32));
  let same = true;
  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) same = false;
  }
  assert.ok(!same, "two independent draws must not be byte-identical");
});

test("randomUUID has the canonical RFC 4122 v4 shape", () => {
  const u = webcrypto.randomUUID();
  assert.strictEqual(u.length, 36, "UUID string length");
  const v4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
  assert.ok(v4.test(u), "matches the RFC 4122 v4 pattern: " + u);
});

// subtle.digest returns a Promise<ArrayBuffer>. The decisive SHA-256 check runs inside the returned
// promise chain so the harness re-surfaces a mismatch as a rejected test promise.
test("subtle.digest computes the SHA-256 of 'abc'", () => {
  const data = new Uint8Array([0x61, 0x62, 0x63]); // "abc"
  // The well-known SHA-256("abc") digest.
  const expectedHex =
    "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
  return webcrypto.subtle.digest("SHA-256", data).then(function (buf) {
    const bytes = new Uint8Array(buf);
    let hex = "";
    for (let i = 0; i < bytes.length; i++) {
      hex += bytes[i].toString(16).padStart(2, "0");
    }
    assert.strictEqual(hex, expectedHex, "SHA-256('abc') digest");
  });
});
