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

// Node test/parallel-style: the WHATWG `URL` class reached via `require("node:url").URL`.
//
// Unlike the global-only `URL` accessor (unreachable from this eval scope), the constructor is also a
// named export of node:url, so it is exercised here. Covers full-component decomposition, href
// round-trip, origin, the port/host pairing and a credentialed authority.
const { URL } = require("node:url");

test("a full URL decomposes into all its components", () => {
  const u = new URL("https://example.com:8080/a/b?q=1&q=2#frag");
  assert.strictEqual(u.protocol, "https:", "protocol");
  assert.strictEqual(u.hostname, "example.com", "hostname");
  assert.strictEqual(u.port, "8080", "port");
  assert.strictEqual(u.host, "example.com:8080", "host pairs hostname:port");
  assert.strictEqual(u.pathname, "/a/b", "pathname");
  assert.strictEqual(u.search, "?q=1&q=2", "search");
  assert.strictEqual(u.hash, "#frag", "hash");
});

test("href round-trips the original input", () => {
  const input = "https://example.com:8080/a/b?q=1&q=2#frag";
  const u = new URL(input);
  assert.strictEqual(u.href, input, "href reconstructs the input");
  assert.strictEqual(String(u), input, "toString equals href");
});

test("origin reflects protocol + host", () => {
  const u = new URL("https://example.com:8080/a/b");
  assert.strictEqual(u.origin, "https://example.com:8080", "origin");
});

test("a URL without a port reports an empty port and a bare host", () => {
  const u = new URL("http://host/path");
  assert.strictEqual(u.protocol, "http:", "protocol");
  assert.strictEqual(u.hostname, "host", "hostname");
  assert.strictEqual(u.port, "", "absent port is the empty string");
  assert.strictEqual(u.host, "host", "host has no colon when port is absent");
  assert.strictEqual(u.pathname, "/path", "pathname");
});

test("a query-less, fragment-less URL reports empty search and hash", () => {
  const u = new URL("http://host/p");
  assert.strictEqual(u.search, "", "no query -> empty search");
  assert.strictEqual(u.hash, "", "no fragment -> empty hash");
});

test("an input with no scheme and no base throws a TypeError", () => {
  let caught = null;
  try {
    new URL("not a url");
  } catch (e) {
    caught = e;
  }
  assert.ok(caught instanceof TypeError, "must throw a TypeError for an invalid URL");
});

test("toJSON returns the href", () => {
  const u = new URL("https://example.com/x");
  assert.strictEqual(u.toJSON(), u.href, "toJSON equals href");
});
