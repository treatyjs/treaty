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

// node:zlib Brotli (RFC 7932) synchronous round-trips: brotliCompressSync then
// brotliDecompressSync must recover the original bytes exactly, the compressed form of a highly
// repetitive payload must be smaller than the input, and the BROTLI_PARAM_QUALITY encoder knob
// must be honored (a higher quality produces output no larger than a lower one).
const zlib = require("node:zlib");
const buffer = require("node:buffer");
const Buffer = buffer.Buffer;

assert.ok(typeof zlib.brotliCompressSync === "function", "brotliCompressSync export");
assert.ok(typeof zlib.brotliDecompressSync === "function", "brotliDecompressSync export");
assert.ok(zlib.constants && typeof zlib.constants.BROTLI_PARAM_QUALITY === "number",
  "BROTLI_PARAM_QUALITY constant");

const payload = "the quick brown fox jumps over the lazy dog ".repeat(16);

function toUtf8(buf) {
  if (typeof buf.toString === "function") return buf.toString("utf8");
  return Buffer.from(buf).toString("utf8");
}

test("brotliCompressSync then brotliDecompressSync recovers the original text", () => {
  const compressed = zlib.brotliCompressSync(payload);
  assert.ok(compressed.length > 0, "brotli produced output");
  const restored = toUtf8(zlib.brotliDecompressSync(compressed));
  assert.strictEqual(restored, payload, "brotli round-trip is lossless");
});

test("a compressible payload actually shrinks under brotli", () => {
  const compressed = zlib.brotliCompressSync(payload);
  assert.ok(compressed.length < payload.length, "highly repetitive input compresses smaller");
});

test("BROTLI_PARAM_QUALITY is honored", () => {
  const q = zlib.constants.BROTLI_PARAM_QUALITY;
  const low = zlib.brotliCompressSync(payload, { params: { [q]: 1 } });
  const high = zlib.brotliCompressSync(payload, { params: { [q]: 11 } });
  const lowRestored = toUtf8(zlib.brotliDecompressSync(low));
  const highRestored = toUtf8(zlib.brotliDecompressSync(high));
  assert.strictEqual(lowRestored, payload, "low-quality round-trip is lossless");
  assert.strictEqual(highRestored, payload, "high-quality round-trip is lossless");
  assert.ok(high.length <= low.length, "higher quality is no larger than lower quality");
});
