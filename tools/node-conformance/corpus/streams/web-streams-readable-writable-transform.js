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

// WHATWG Streams globals: ReadableStream / WritableStream / TransformStream. These are top-level
// globals (Bun / Cloudflare Workers `nodejs_compat` surface), referenced via globalThis.
const RS = globalThis.ReadableStream;
const WS = globalThis.WritableStream;
const TS = globalThis.TransformStream;
assert.ok(typeof RS === "function", "ReadableStream global present");
assert.ok(typeof WS === "function", "WritableStream global present");
assert.ok(typeof TS === "function", "TransformStream global present");

test("ReadableStream getReader yields enqueued chunks then done", () => {
  const rs = new RS({
    start(controller) {
      controller.enqueue("a");
      controller.enqueue("b");
      controller.close();
    },
  });
  const reader = rs.getReader();
  return reader.read().then(function (r1) {
    assert.strictEqual(r1.done, false, "first read not done");
    assert.strictEqual(r1.value, "a", "first chunk");
    return reader.read().then(function (r2) {
      assert.strictEqual(r2.value, "b", "second chunk");
      return reader.read().then(function (r3) {
        assert.strictEqual(r3.done, true, "stream is done after both chunks");
      });
    });
  });
});

test("WritableStream collects written chunks", () => {
  const seen = [];
  const ws = new WS({
    write(chunk) {
      seen.push(chunk);
    },
  });
  const writer = ws.getWriter();
  return writer.write("x").then(function () {
    return writer.write("y").then(function () {
      return writer.close().then(function () {
        assert.deepStrictEqual(seen, ["x", "y"], "writer delivered both chunks in order");
      });
    });
  });
});

test("TransformStream maps written chunks to the readable side", () => {
  const ts = new TS({
    transform(chunk, controller) {
      controller.enqueue(chunk.toUpperCase());
    },
  });
  const writer = ts.writable.getWriter();
  const reader = ts.readable.getReader();
  writer.write("hi");
  writer.close();
  return reader.read().then(function (r) {
    assert.strictEqual(r.value, "HI", "transform upper-cased the chunk");
  });
});
