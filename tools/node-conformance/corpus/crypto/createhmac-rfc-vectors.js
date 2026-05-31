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

// Node test/parallel-style: node:crypto createHmac against published HMAC test vectors.
//
// Pins RFC 4231 test case 2 (HMAC-SHA-256) and RFC 2202 test case 2 (HMAC-SHA-1), both keyed with
// "Jefe" over "what do ya want for nothing?", so the keyed-hash construction is verified against a
// fixed external reference. Also exercises chunked update equivalence and the raw-digest path.
const crypto = require("node:crypto");

const KEY = "Jefe";
const DATA = "what do ya want for nothing?";

test("HMAC-SHA-256 matches RFC 4231 test case 2", () => {
  const hex = crypto.createHmac("sha256", KEY).update(DATA).digest("hex");
  assert.strictEqual(
    hex,
    "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843",
    "HMAC-SHA-256(Jefe, ...)"
  );
});

test("HMAC-SHA-1 matches RFC 2202 test case 2", () => {
  const hex = crypto.createHmac("sha1", KEY).update(DATA).digest("hex");
  assert.strictEqual(
    hex,
    "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79",
    "HMAC-SHA-1(Jefe, ...)"
  );
});

test("chunked HMAC updates concatenate to the one-shot MAC", () => {
  // HMAC(key, a ++ b) === feeding "a" then "b": split DATA at a space boundary.
  const split = DATA.indexOf(" want");
  const head = DATA.slice(0, split);
  const tail = DATA.slice(split);
  const streamed = crypto.createHmac("sha256", KEY).update(head).update(tail).digest("hex");
  assert.strictEqual(
    streamed,
    "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843",
    "chunked HMAC equals one-shot"
  );
});

test("an HMAC digest with no encoding is a 32-byte container for sha256", () => {
  const raw = crypto.createHmac("sha256", KEY).update(DATA).digest();
  assert.strictEqual(raw.length, 32, "HMAC-SHA-256 output is 32 bytes");
  // First byte of the RFC 4231 case-2 MAC is 0x5b.
  assert.strictEqual(raw[0], 0x5b, "first raw MAC byte");
});

test("an unsupported HMAC algorithm throws", () => {
  assert.throws(() => crypto.createHmac("sha3-256", KEY), "sha3-256 is not implemented");
});
