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

// Node test/parallel-style: node:string_decoder StringDecoder.end() semantics — flushing a dangling
// incomplete multibyte sequence as a single U+FFFD replacement character, and decoding a final chunk
// passed to end(). The replacement character U+FFFD is written as "�" to keep the source ASCII.
const { StringDecoder } = require("node:string_decoder");
const REPLACEMENT = "�";

test("end() on a clean stream emits nothing", () => {
  const d = new StringDecoder("utf8");
  assert.strictEqual(d.write([111, 107]), "ok", "ascii decodes");
  assert.strictEqual(d.end(), "", "a stream that ended on a code-point boundary flushes nothing");
});

test("end() flushes a dangling partial as one replacement character", () => {
  // A lone lead byte of a three-byte sequence has no continuation; end() emits exactly one U+FFFD.
  const d = new StringDecoder("utf8");
  assert.strictEqual(d.write([0xe6]), "", "a buffered lead byte emits nothing yet");
  assert.strictEqual(d.end(), REPLACEMENT, "the dangling partial flushes as one U+FFFD");
});

test("end() flushes a partially-filled multibyte sequence as one replacement", () => {
  // Two of the three bytes of '日' arrive, then the stream ends: one replacement char, not two.
  const d = new StringDecoder("utf8");
  assert.strictEqual(d.write([0xe6, 0x97]), "", "two of three bytes buffer with no output");
  assert.strictEqual(d.end(), REPLACEMENT, "an incomplete sequence flushes as a single U+FFFD");
});

test("end(buffer) decodes a final chunk before flushing", () => {
  // 'é' = [0xC3, 0xA9]: write the lead, then complete it inside end().
  const d = new StringDecoder("utf8");
  assert.strictEqual(d.write([0xc3]), "", "lead byte buffered");
  assert.strictEqual(d.end([0xa9]), "é", "end() completes the carried sequence");
});

test("end(buffer) decodes the final chunk then flushes a fresh dangling partial", () => {
  // Carry 'é' lead, then end() supplies its continuation plus a new lone lead byte: the completed
  // 'é' is emitted and the trailing lone lead flushes as one U+FFFD.
  const d = new StringDecoder("utf8");
  assert.strictEqual(d.write([0xc3]), "", "carry the 'é' lead");
  assert.strictEqual(d.end([0xa9, 0xf0]), "é" + REPLACEMENT, "complete 'é' then flush the new partial");
});

test("a standalone invalid byte becomes a replacement character", () => {
  // 0xFF is never valid UTF-8; non-fatal decoding (Node's default) substitutes U+FFFD in place.
  const d = new StringDecoder("utf8");
  assert.strictEqual(d.write([97, 0xff, 98]), "a" + REPLACEMENT + "b", "invalid byte -> U+FFFD between ascii");
  assert.strictEqual(d.end(), "", "nothing dangling after a standalone invalid byte");
});

test("end() is idempotent after it has already flushed", () => {
  const d = new StringDecoder("utf8");
  d.write([0xe6]);
  assert.strictEqual(d.end(), REPLACEMENT, "first end() flushes the partial");
  assert.strictEqual(d.end(), "", "a second end() has nothing left to flush");
});
