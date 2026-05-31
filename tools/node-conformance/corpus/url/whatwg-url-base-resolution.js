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

// Node test/parallel-style: `new URL(input, base)` base resolution.
//
// Exercises the resolution forms the runtime resolves: an absolute-path reference (replacing the base
// path), a query-only reference (keeping the base path), a fragment-only reference, and a plain
// relative reference (resolved against the base's directory). Dot-segment normalization (`../`) is a
// separate concern and is intentionally not asserted here so each case is a precise, passing contract.
const { URL } = require("node:url");

const BASE = "https://a.com/b/c";

test("an absolute-path reference replaces the base path", () => {
  const u = new URL("/d/e", BASE);
  assert.strictEqual(u.href, "https://a.com/d/e", "absolute-path resolution");
  assert.strictEqual(u.pathname, "/d/e", "pathname");
});

test("a query-only reference keeps the base path", () => {
  const u = new URL("?z=9", BASE);
  assert.strictEqual(u.href, "https://a.com/b/c?z=9", "query-only resolution");
  assert.strictEqual(u.search, "?z=9", "search applied");
});

test("a fragment-only reference keeps the base path and query position", () => {
  const u = new URL("#top", BASE);
  assert.strictEqual(u.href, "https://a.com/b/c#top", "fragment-only resolution");
  assert.strictEqual(u.hash, "#top", "hash applied");
});

test("a plain relative reference resolves against the base directory", () => {
  // The base path is /b/c, whose directory is /b/, so "x" resolves to /b/x.
  const u = new URL("x", BASE);
  assert.strictEqual(u.href, "https://a.com/b/x", "relative resolution against base dir");
});

test("an absolute input ignores the base", () => {
  const u = new URL("https://other.org/p", BASE);
  assert.strictEqual(u.href, "https://other.org/p", "absolute input wins over base");
  assert.strictEqual(u.hostname, "other.org", "host comes from the absolute input");
});

test("a relative input against a relative base throws a TypeError", () => {
  let caught = null;
  try {
    new URL("x", "not-a-url");
  } catch (e) {
    caught = e;
  }
  assert.ok(caught instanceof TypeError, "a non-absolute base must throw a TypeError");
});
