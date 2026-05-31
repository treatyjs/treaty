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

// Node test/parallel-style: node:string_decoder StringDecoder reassembling a multibyte UTF-8
// character split across write() chunk boundaries. The decoder must buffer an incomplete trailing
// sequence and emit it only once the continuation byte(s) arrive — exactly Node's StringDecoder
// contract. Byte chunks are plain integer arrays (the runtime's documented JS<->Rust byte-exchange
// shape, since the pinned engine exposes no embedder Buffer construction).
const { StringDecoder } = require("node:string_decoder");

test("a complete chunk decodes in one write", () => {
  const d = new StringDecoder("utf8");
  // ASCII and whole multibyte code points decode immediately.
  assert.strictEqual(d.write([104, 105]), "hi", "ascii decodes whole");
  const d2 = new StringDecoder("utf8");
  // 'é' = [0xC3, 0xA9] delivered whole.
  assert.strictEqual(d2.write([0xc3, 0xa9]), "é", "whole two-byte sequence decodes");
});

test("a two-byte sequence split across two writes is reassembled", () => {
  // 'é' = U+00E9 = [0xC3, 0xA9]; the lead byte alone yields nothing.
  const d = new StringDecoder("utf8");
  assert.strictEqual(d.write([0xc3]), "", "lead byte alone emits nothing");
  assert.strictEqual(d.write([0xa9]), "é", "continuation completes the character");
});

test("a three-byte sequence is reassembled at any internal boundary", () => {
  // '日' = U+65E5 = [0xE6, 0x97, 0xA5]; try both split points.
  const bytes = [0xe6, 0x97, 0xa5];
  for (let split = 1; split < bytes.length; split++) {
    const d = new StringDecoder("utf8");
    const first = d.write(bytes.slice(0, split));
    const second = d.write(bytes.slice(split));
    assert.strictEqual(first + second, "日", "split at " + split + " reassembles 日");
  }
});

test("a four-byte sequence is reassembled at any internal boundary", () => {
  // '🦀' = U+1F980 = [0xF0, 0x9F, 0xA6, 0x80]; try every split point.
  const bytes = [0xf0, 0x9f, 0xa6, 0x80];
  for (let split = 1; split < bytes.length; split++) {
    const d = new StringDecoder("utf8");
    const first = d.write(bytes.slice(0, split));
    const second = d.write(bytes.slice(split));
    assert.strictEqual(first + second, "🦀", "split at " + split + " reassembles 🦀");
  }
});

test("byte-by-byte streaming reconstructs a mixed-width string", () => {
  const original = "aé日🦀z";
  // Encode to UTF-8 bytes via TextEncoder (reached through the global accessor installed by the
  // runtime); feed one byte per write and accumulate the output.
  const bytes = Array.prototype.slice.call(new TextEncoder().encode(original));
  const d = new StringDecoder("utf8");
  let out = "";
  for (const b of bytes) {
    out += d.write([b]);
  }
  assert.strictEqual(out, original, "one-byte-at-a-time streaming reconstructs the original");
  // No partial should remain after a complete stream.
  assert.strictEqual(d.end(), "", "end() after a complete stream emits nothing");
});

test("a sequence split across many chunks of varying size round-trips", () => {
  const original = "héllo, 世界 🌍!";
  const bytes = Array.prototype.slice.call(new TextEncoder().encode(original));
  const d = new StringDecoder("utf8");
  let out = "";
  // Walk the byte stream in irregular chunk sizes to exercise carries across boundaries.
  let i = 0;
  for (const size of [1, 3, 2, 5, 4, 1, 7, 100]) {
    out += d.write(bytes.slice(i, i + size));
    i += size;
  }
  out += d.write(bytes.slice(i));
  out += d.end();
  assert.strictEqual(out, original, "irregular chunking reassembles the full string");
});
