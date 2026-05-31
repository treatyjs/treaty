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

// Node test/parallel-style: node:buffer compare / equals / indexOf / includes / readUInt8 / toJSON /
// INSPECT_MAX_BYTES. Mirrors Node's test-buffer-compare / test-buffer-indexof / test-buffer-tojson —
// comparison is lexicographic (-1/0/1), equals is byte-equality, indexOf/includes search for a
// subsequence, readUInt8 reads a single byte, toJSON yields the { type, data } form, and the module
// exposes INSPECT_MAX_BYTES.
const buffer = require("node:buffer");
const { Buffer } = buffer;

test("Buffer.compare and the instance .compare are lexicographic (-1 / 0 / 1)", () => {
  assert.strictEqual(Buffer.compare(Buffer.from("a"), Buffer.from("b")), -1, "'a' < 'b'");
  assert.strictEqual(Buffer.compare(Buffer.from("b"), Buffer.from("a")), 1, "'b' > 'a'");
  assert.strictEqual(Buffer.compare(Buffer.from("x"), Buffer.from("x")), 0, "equal -> 0");
  assert.strictEqual(Buffer.from("hello").compare(Buffer.from("hello")), 0, "instance compare equal");
});

test("equals and isBuffer distinguish buffers by their bytes", () => {
  assert.strictEqual(Buffer.from("xy").equals(Buffer.from("xy")), true, "equal byte content");
  assert.strictEqual(Buffer.from("xy").equals(Buffer.from("xz")), false, "differing byte content");
  assert.strictEqual(Buffer.isBuffer(Buffer.from("z")), true, "a Buffer is a Buffer");
  assert.strictEqual(Buffer.isBuffer("z"), false, "a string is not a Buffer");
});

test("indexOf and includes search for a byte subsequence", () => {
  const buf = Buffer.from("hello");
  assert.strictEqual(buf.indexOf("ll"), 2, "'ll' starts at index 2");
  assert.strictEqual(buf.indexOf("z"), -1, "a missing needle gives -1");
  assert.strictEqual(buf.includes("ell"), true, "includes finds the subsequence");
  assert.strictEqual(buf.includes("xyz"), false, "includes is false when absent");
});

test("readUInt8 reads a single byte at an index", () => {
  const buf = Buffer.from("hi"); // 'h' == 104, 'i' == 105
  assert.strictEqual(buf.readUInt8(0), 104, "byte 0 is 'h'");
  assert.strictEqual(buf.readUInt8(1), 105, "byte 1 is 'i'");
});

test("toJSON yields the canonical { type: 'Buffer', data: [...] } form", () => {
  const json = Buffer.from("AB").toJSON(); // 'A' == 65, 'B' == 66
  assert.strictEqual(json.type, "Buffer", "the JSON tag is 'Buffer'");
  assert.deepStrictEqual(json.data, [65, 66], "data is the byte array");
});

test("the buffer module exposes a numeric INSPECT_MAX_BYTES", () => {
  assert.strictEqual(typeof buffer.INSPECT_MAX_BYTES, "number", "INSPECT_MAX_BYTES is a number");
  assert.strictEqual(buffer.INSPECT_MAX_BYTES > 0, true, "and it is positive");
});
