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

// Node test/parallel-style: node:crypto createHash known-answer vectors.
//
// Pins the published FIPS 180-2 / RFC 1321 digests for sha256, sha1 and md5 over the canonical
// "abc" input (and the empty string for sha256), so a regression in the pure-Rust digest core or in
// the JS hex-encoding layer is caught against a fixed external reference rather than self-consistency.
const crypto = require("node:crypto");

test("sha256 of 'abc' matches the FIPS 180-2 vector", () => {
  const hex = crypto.createHash("sha256").update("abc").digest("hex");
  assert.strictEqual(
    hex,
    "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    "sha256('abc')"
  );
});

test("sha256 of the empty string matches the known vector", () => {
  const hex = crypto.createHash("sha256").update("").digest("hex");
  assert.strictEqual(
    hex,
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    "sha256('')"
  );
});

test("sha1 of 'abc' matches the known vector", () => {
  const hex = crypto.createHash("sha1").update("abc").digest("hex");
  assert.strictEqual(hex, "a9993e364706816aba3e25717850c26c9cd0d89d", "sha1('abc')");
});

test("md5 of 'abc' matches the RFC 1321 vector", () => {
  const hex = crypto.createHash("md5").update("abc").digest("hex");
  assert.strictEqual(hex, "900150983cd24fb0d6963f7d28e17f72", "md5('abc')");
});

test("streaming updates concatenate to the one-shot digest", () => {
  // digest === H(chunk0 ++ chunk1 ++ ...): splitting "abc" into three updates yields the same hash.
  const streamed = crypto
    .createHash("sha256")
    .update("a")
    .update("b")
    .update("c")
    .digest("hex");
  assert.strictEqual(
    streamed,
    "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    "chunked sha256 equals one-shot"
  );
});

test("a digest with no encoding yields a fixed-length byte container", () => {
  // No encoding argument -> the raw 32-byte sha256 digest; first byte of sha256('abc') is 0xba.
  const raw = crypto.createHash("sha256").update("abc").digest();
  assert.strictEqual(raw.length, 32, "sha256 digest is 32 bytes");
  assert.strictEqual(raw[0], 0xba, "first raw digest byte");
});

test("a base64 digest matches the known sha1 encoding", () => {
  const b64 = crypto.createHash("sha1").update("abc").digest("base64");
  assert.strictEqual(b64, "qZk+NkcGgWq6PiVxeFDCbJzQ2J0=", "base64 sha1('abc')");
});

test("an unsupported algorithm throws", () => {
  assert.throws(() => crypto.createHash("sha3-512"), "sha3-512 is not implemented");
});

test("getHashes advertises the supported digests", () => {
  const hashes = crypto.getHashes();
  assert.ok(hashes.includes("sha256"), "sha256 advertised");
  assert.ok(hashes.includes("sha1"), "sha1 advertised");
  assert.ok(hashes.includes("md5"), "md5 advertised");
});
