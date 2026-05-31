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

// Node test/parallel-style: node:buffer Buffer.from/alloc and utf8/base64/hex round-trips, plus
// concat/compare/equals/isBuffer.
const { Buffer } = require("node:buffer");

test("Buffer.from(utf8) reports the correct byte length", () => {
  const buf = Buffer.from("hello", "utf8");
  assert.strictEqual(buf.length, 5, "byte length of 'hello'");
  assert.strictEqual(buf.toString("utf8"), "hello", "utf8 round-trip");
});

test("hex encoding round-trips", () => {
  const hex = Buffer.from("hello", "utf8").toString("hex");
  assert.strictEqual(hex, "68656c6c6f", "hex of 'hello'");
  assert.strictEqual(Buffer.from(hex, "hex").toString("utf8"), "hello", "hex decode");
});

test("base64 encoding round-trips", () => {
  const b64 = Buffer.from("Man", "utf8").toString("base64");
  assert.strictEqual(b64, "TWFu", "base64 of 'Man'");
  assert.strictEqual(Buffer.from(b64, "base64").toString("utf8"), "Man", "base64 decode");
});

test("Buffer.alloc creates a zero-length-correct buffer", () => {
  const z = Buffer.alloc(4);
  assert.strictEqual(z.length, 4, "alloc(4) has length 4");
});

test("Buffer.concat joins buffers", () => {
  const joined = Buffer.concat([Buffer.from("foo"), Buffer.from("bar")]);
  assert.strictEqual(joined.toString("utf8"), "foobar", "concat preserves order");
  assert.strictEqual(joined.length, 6, "concat length is the sum");
});

test("compare/equals/isBuffer behave per Node", () => {
  assert.strictEqual(Buffer.compare(Buffer.from("a"), Buffer.from("b")), -1, "'a' < 'b'");
  assert.strictEqual(Buffer.from("x").equals(Buffer.from("x")), true, "equal buffers");
  assert.strictEqual(Buffer.from("x").equals(Buffer.from("y")), false, "unequal buffers");
  assert.strictEqual(Buffer.isBuffer(Buffer.from("z")), true, "isBuffer of a buffer");
  assert.strictEqual(Buffer.isBuffer("z"), false, "isBuffer of a string");
});
