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

// node:stream object model: Readable.from + flowing 'data'/'end', Writable write/end/finish, and a
// Transform (PassThrough-style) piping written chunks to the readable side.
const stream = require("node:stream");
const { Readable, Writable, Transform, PassThrough } = stream;
assert.ok(typeof Readable === "function", "Readable export");
assert.ok(typeof Writable === "function", "Writable export");
assert.ok(typeof Transform === "function", "Transform export");
assert.ok(typeof PassThrough === "function", "PassThrough export");

test("Readable.from emits each item then end in flowing mode", () => {
  const r = Readable.from(["one", "two", "three"]);
  const chunks = [];
  return new Promise(function (resolve, reject) {
    r.on("data", function (c) { chunks.push(String(c)); });
    r.on("end", function () {
      try {
        assert.deepStrictEqual(chunks, ["one", "two", "three"], "all items in order");
        resolve();
      } catch (e) { reject(e); }
    });
    r.on("error", reject);
  });
});

test("Writable collects writes and fires finish on end", () => {
  const seen = [];
  const w = new Writable({
    write(chunk, encoding, cb) {
      seen.push(String(chunk));
      cb();
    },
  });
  return new Promise(function (resolve, reject) {
    w.on("finish", function () {
      try {
        assert.deepStrictEqual(seen, ["a", "b"], "writable saw both chunks");
        resolve();
      } catch (e) { reject(e); }
    });
    w.write("a");
    w.write("b");
    w.end();
  });
});

test("Transform maps chunks from the writable to the readable side", () => {
  const t = new Transform({
    transform(chunk, encoding, cb) {
      cb(null, String(chunk).toUpperCase());
    },
  });
  const out = [];
  return new Promise(function (resolve, reject) {
    t.on("data", function (c) { out.push(String(c)); });
    t.on("end", function () {
      try {
        assert.deepStrictEqual(out, ["AB", "CD"], "transform upper-cased each chunk");
        resolve();
      } catch (e) { reject(e); }
    });
    t.write("ab");
    t.write("cd");
    t.end();
  });
});
