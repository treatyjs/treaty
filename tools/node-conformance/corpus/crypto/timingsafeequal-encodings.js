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

// Node test/parallel-style: node:crypto digest output encodings and timingSafeEqual.
//
// Verifies the same sha256 digest re-encodes consistently across hex / base64 / base64url, that the
// digest a chosen encoding produces decodes (via createHash again) to the same bytes, and that
// timingSafeEqual reports equal/unequal correctly and throws on a length mismatch (Node's contract).
const crypto = require("node:crypto");

const HEX = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

test("the same input hashes identically across calls (hex)", () => {
  const a = crypto.createHash("sha256").update("abc").digest("hex");
  const b = crypto.createHash("sha256").update("abc").digest("hex");
  assert.strictEqual(a, b, "deterministic digest");
  assert.strictEqual(a, HEX, "matches the known vector");
});

test("base64 and base64url encodings of sha256('abc') are the known forms", () => {
  const b64 = crypto.createHash("sha256").update("abc").digest("base64");
  const b64url = crypto.createHash("sha256").update("abc").digest("base64url");
  assert.strictEqual(b64, "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=", "base64");
  // base64url uses -/_ and drops '=' padding.
  assert.strictEqual(b64url, "ungWv48Bz-pBQUDeXa4iI7ADYaOWF3qctBD_YfIAFa0", "base64url");
});

test("a Hash cannot be digested twice", () => {
  const h = crypto.createHash("sha256");
  h.update("abc");
  h.digest("hex");
  assert.throws(() => h.digest("hex"), "second digest must throw");
});

test("timingSafeEqual reports equality for equal digests", () => {
  const a = crypto.createHash("sha256").update("payload").digest();
  const b = crypto.createHash("sha256").update("payload").digest();
  assert.strictEqual(crypto.timingSafeEqual(a, b), true, "equal digests");
});

test("timingSafeEqual reports inequality for differing digests", () => {
  const a = crypto.createHash("sha256").update("payload-A").digest();
  const b = crypto.createHash("sha256").update("payload-B").digest();
  assert.strictEqual(crypto.timingSafeEqual(a, b), false, "differing digests");
});

test("timingSafeEqual throws on a length mismatch", () => {
  const short = crypto.createHash("sha1").update("x").digest();
  const long = crypto.createHash("sha256").update("x").digest();
  assert.throws(() => crypto.timingSafeEqual(short, long), "length mismatch must throw");
});
