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

// Node test/parallel-style: node:buffer construction. Mirrors Node's test-buffer-alloc /
// test-buffer-from — Buffer.from accepts a string, a byte array, a Uint8Array and an ArrayBuffer;
// Buffer.alloc zero-fills (or fills with a given value); Buffer.allocUnsafe yields the right length;
// and Buffer.byteLength counts utf8 bytes (not code units).
const { Buffer } = require("node:buffer");

test("Buffer.from(string) length and contents follow utf8 byte counting", () => {
  const buf = Buffer.from("hello", "utf8");
  assert.strictEqual(buf.length, 5, "'hello' is five bytes");
  assert.strictEqual(buf.toString("utf8"), "hello", "round-trips back to the string");
  // A two-byte code point counts as two bytes, not one character.
  assert.strictEqual(Buffer.from("é", "utf8").length, 2, "'é' is two utf8 bytes");
});

test("Buffer.from(array) interprets numbers as bytes", () => {
  const buf = Buffer.from([72, 73]);
  assert.strictEqual(buf.length, 2, "two elements -> two bytes");
  assert.strictEqual(buf[0], 72, "first byte preserved");
  assert.strictEqual(buf.toString("utf8"), "HI", "the bytes decode to 'HI'");
});

test("Buffer.from(Uint8Array) and Buffer.from(ArrayBuffer) copy the bytes", () => {
  const u8 = new Uint8Array([74, 75]);
  assert.strictEqual(Buffer.from(u8).toString("utf8"), "JK", "from a typed array");
  assert.strictEqual(Buffer.from(u8.buffer).toString("utf8"), "JK", "from the backing ArrayBuffer");
});

test("Buffer.alloc zero-fills by default and honours a fill value", () => {
  const zeros = Buffer.alloc(4);
  assert.strictEqual(zeros.length, 4, "alloc(4) has length 4");
  assert.strictEqual(zeros[0], 0, "byte 0 is zero");
  assert.strictEqual(zeros[3], 0, "byte 3 is zero");

  const filled = Buffer.alloc(3, 65); // 65 == 'A'
  assert.strictEqual(filled.toString("utf8"), "AAA", "alloc with a fill byte repeats it");
});

test("Buffer.allocUnsafe returns a buffer of the requested length", () => {
  const buf = Buffer.allocUnsafe(8);
  assert.strictEqual(buf.length, 8, "allocUnsafe(8) has length 8");
  // Its bytes are unspecified, but it must be writable and read back what we put in.
  buf.fill(0);
  buf.write("ok");
  assert.strictEqual(buf.toString("utf8", 0, 2), "ok", "allocUnsafe yields a usable buffer");
});

test("Buffer.byteLength counts utf8 bytes, not characters", () => {
  assert.strictEqual(Buffer.byteLength("hello"), 5, "ASCII: one byte each");
  assert.strictEqual(Buffer.byteLength("héllo", "utf8"), 6, "the 'é' adds a second byte");
  assert.strictEqual(Buffer.byteLength("", "utf8"), 0, "the empty string is zero bytes");
});
