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

// Node test/parallel-style: node:buffer slice / subarray / concat / copy / write / fill. Mirrors
// Node's test-buffer-slice / test-buffer-concat / test-buffer-copy — slice and subarray return a view
// that shares memory with the parent, concat joins (optionally truncated to a total length), copy
// transfers bytes between buffers, write places bytes at an offset, and fill paints a range.
const { Buffer } = require("node:buffer");

test("slice returns a view that shares memory with its parent", () => {
  const parent = Buffer.from("hello");
  const view = parent.slice(0, 2);
  assert.strictEqual(view.toString("utf8"), "he", "slice(0,2) is the first two bytes");

  // Mutating the view writes through to the parent (shared memory, Node semantics).
  view[0] = 72; // 'H'
  assert.strictEqual(parent.toString("utf8"), "Hello", "the parent sees the view's mutation");
});

test("subarray behaves like slice and shares memory", () => {
  const buf = Buffer.from("world");
  const sub = buf.subarray(1, 4);
  assert.strictEqual(sub.toString("utf8"), "orl", "subarray(1,4)");
  assert.strictEqual(sub.length, 3, "its length is end - start");
});

test("Buffer.concat joins in order and can be truncated by totalLength", () => {
  const joined = Buffer.concat([Buffer.from("foo"), Buffer.from("bar")]);
  assert.strictEqual(joined.toString("utf8"), "foobar", "concat preserves order");
  assert.strictEqual(joined.length, 6, "length is the sum of the parts");

  const truncated = Buffer.concat([Buffer.from("ab"), Buffer.from("cd")], 3);
  assert.strictEqual(truncated.length, 3, "totalLength caps the result length");
  assert.strictEqual(truncated.toString("utf8"), "abc", "and keeps the first three bytes");
});

test("copy transfers bytes from one buffer into another", () => {
  const src = Buffer.from("hello");
  const dst = Buffer.alloc(5);
  const copied = src.copy(dst);
  assert.strictEqual(copied, 5, "copy returns the number of bytes written");
  assert.strictEqual(dst.toString("utf8"), "hello", "the destination now holds the source bytes");
});

test("write places bytes at an offset, leaving the rest untouched", () => {
  const buf = Buffer.alloc(5, 48); // "00000"
  const written = buf.write("ab", 1);
  assert.strictEqual(written, 2, "write returns the byte count");
  assert.strictEqual(buf.toString("utf8"), "0ab00", "'ab' lands at offset 1");
});

test("fill paints a value across an optional range", () => {
  const all = Buffer.alloc(4);
  all.fill(65); // 'A'
  assert.strictEqual(all.toString("utf8"), "AAAA", "fill with no range covers the whole buffer");

  const ranged = Buffer.alloc(5, 48); // "00000"
  ranged.fill(65, 1, 3);
  assert.strictEqual(ranged.toString("utf8"), "0AA00", "fill(value, start, end) paints just the range");
});
