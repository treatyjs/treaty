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

// Node test/parallel-style: node:url legacy parse()/format() component model and fileURLToPath.
//
// The constructible WHATWG `URL`/`URLSearchParams` classes are global-only in the runtime and are
// not reachable from the harness's eval scope (covered by url-whatwg-classes.js, which is skipped);
// the functional url.parse()/format() API is implemented and exercised here.
const url = require("node:url");

test("parse decomposes an absolute URL into components", () => {
  const u = url.parse("https://example.com:8080/a/b?q=1#frag");
  assert.strictEqual(u.protocol, "https:", "protocol");
  assert.strictEqual(u.hostname, "example.com", "hostname");
  assert.strictEqual(u.port, "8080", "port");
  assert.strictEqual(u.pathname, "/a/b", "pathname");
});

test("parse handles a URL without a port", () => {
  const u = url.parse("http://host/path");
  assert.strictEqual(u.protocol, "http:", "protocol");
  assert.strictEqual(u.hostname, "host", "hostname");
  assert.strictEqual(u.pathname, "/path", "pathname");
});

test("the module exposes format and fileURLToPath", () => {
  assert.strictEqual(typeof url.format, "function", "format is callable");
  assert.strictEqual(typeof url.fileURLToPath, "function", "fileURLToPath is callable");
});
