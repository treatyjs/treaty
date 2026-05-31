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

// node:zlib synchronous round-trips: gzip/gunzip, deflate/inflate, and raw deflate/inflate must
// each recover the original bytes exactly. All synchronous, so the assertions throw from the body.
const zlib = require("node:zlib");
const buffer = require("node:buffer");
const Buffer = buffer.Buffer;
assert.ok(typeof zlib.gzipSync === "function", "gzipSync export");
assert.ok(typeof zlib.gunzipSync === "function", "gunzipSync export");
assert.ok(typeof zlib.deflateSync === "function", "deflateSync export");
assert.ok(typeof zlib.inflateSync === "function", "inflateSync export");
assert.ok(typeof zlib.deflateRawSync === "function", "deflateRawSync export");
assert.ok(typeof zlib.inflateRawSync === "function", "inflateRawSync export");

const payload = "the quick brown fox jumps over the lazy dog ".repeat(16);

function toUtf8(buf) {
  // Decode the returned Buffer/Uint8Array back to a string for comparison.
  if (typeof buf.toString === "function") return buf.toString("utf8");
  return Buffer.from(buf).toString("utf8");
}

test("gzipSync then gunzipSync recovers the original text", () => {
  const compressed = zlib.gzipSync(payload);
  assert.ok(compressed.length > 0, "gzip produced output");
  const restored = toUtf8(zlib.gunzipSync(compressed));
  assert.strictEqual(restored, payload, "gzip round-trip is lossless");
});

test("deflateSync then inflateSync recovers the original text", () => {
  const compressed = zlib.deflateSync(payload);
  assert.ok(compressed.length > 0, "deflate produced output");
  const restored = toUtf8(zlib.inflateSync(compressed));
  assert.strictEqual(restored, payload, "deflate round-trip is lossless");
});

test("deflateRawSync then inflateRawSync recovers the original text", () => {
  const compressed = zlib.deflateRawSync(payload);
  assert.ok(compressed.length > 0, "raw deflate produced output");
  const restored = toUtf8(zlib.inflateRawSync(compressed));
  assert.strictEqual(restored, payload, "raw deflate round-trip is lossless");
});

test("a compressible payload actually shrinks", () => {
  const compressed = zlib.gzipSync(payload);
  assert.ok(compressed.length < payload.length, "highly repetitive input compresses smaller");
});
