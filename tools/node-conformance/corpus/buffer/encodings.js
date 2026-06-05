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

// Node test/parallel-style: node:buffer encodings. Mirrors Node's test-buffer-tostring /
// test-buffer-bytelength — utf8 / hex / base64 / latin1 / ascii / utf16le encode and decode through
// Buffer.from(...).toString(...) as Node specifies, including a partial-range toString.
const { Buffer } = require("node:buffer");

test("utf8 round-trips and toString accepts a start/end range", () => {
  const buf = Buffer.from("hello", "utf8");
  assert.strictEqual(buf.toString("utf8"), "hello", "full utf8 decode");
  assert.strictEqual(buf.toString("utf8", 1, 4), "ell", "toString(enc, start, end) slices");
  assert.strictEqual(buf.toString(), "hello", "utf8 is the default encoding");
});

test("hex encodes each byte as two lowercase hex digits and decodes back", () => {
  assert.strictEqual(Buffer.from("hello", "utf8").toString("hex"), "68656c6c6f", "hex of 'hello'");
  assert.strictEqual(Buffer.from([255, 0, 16]).toString("hex"), "ff0010", "0xff, 0x00, 0x10");
  assert.strictEqual(Buffer.from("68656c6c6f", "hex").toString("utf8"), "hello", "hex decode");
});

test("base64 encodes and decodes per RFC 4648 (with padding)", () => {
  assert.strictEqual(Buffer.from("Man", "utf8").toString("base64"), "TWFu", "3 bytes -> 4 chars");
  assert.strictEqual(
    Buffer.from("Many hands", "utf8").toString("base64"),
    "TWFueSBoYW5kcw==",
    "padded base64"
  );
  assert.strictEqual(Buffer.from("TWFu", "base64").toString("utf8"), "Man", "base64 decode");
});

test("latin1 and ascii treat each byte as one character", () => {
  const buf = Buffer.from([65, 66, 67]);
  assert.strictEqual(buf.toString("latin1"), "ABC", "latin1 of A,B,C");
  assert.strictEqual(buf.toString("ascii"), "ABC", "ascii of A,B,C");
  assert.strictEqual(Buffer.from("ABC", "latin1").length, 3, "latin1 is one byte per char");
});

test("utf16le uses two bytes per BMP code unit", () => {
  const buf = Buffer.from("AB", "utf16le");
  assert.strictEqual(buf.length, 4, "two characters -> four utf16le bytes");
  assert.strictEqual(buf.toString("utf16le"), "AB", "utf16le round-trips");
});
