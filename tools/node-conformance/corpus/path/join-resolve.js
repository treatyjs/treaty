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

// Node test/parallel-style: node:path join/resolve/dirname/basename/extname/normalize/isAbsolute on
// the platform-default path object, written separator-agnostically via path.sep so it holds on both
// the POSIX and Windows flavors.
const path = require("node:path");

test("join concatenates segments with the platform separator and collapses extra separators", () => {
  assert.strictEqual(path.join("a", "b", "c"), ["a", "b", "c"].join(path.sep), "join uses sep");
  assert.strictEqual(path.join("a", "", "b"), ["a", "b"].join(path.sep), "empty segments are dropped");
});

test("resolve produces an absolute path", () => {
  assert.strictEqual(path.isAbsolute(path.resolve("x")), true, "resolve yields an absolute path");
  assert.strictEqual(
    path.isAbsolute(path.resolve("a", "b", "c")),
    true,
    "resolving several segments is still absolute"
  );
});

test("basename and extname decompose the final segment", () => {
  assert.strictEqual(path.basename("foo" + path.sep + "bar" + path.sep + "baz.txt"), "baz.txt", "basename");
  assert.strictEqual(path.basename("baz.txt", ".txt"), "baz", "basename strips a matching suffix");
  assert.strictEqual(path.basename("baz.txt"), "baz.txt", "basename without a suffix");
  assert.strictEqual(path.extname("baz.txt"), ".txt", "extname returns the dotted extension");
  assert.strictEqual(path.extname("noext"), "", "extname of a name with no dot is empty");
  assert.strictEqual(path.extname(".bashrc"), "", "a leading-dot name has no extension");
  assert.strictEqual(path.extname("archive.tar.gz"), ".gz", "extname is the last extension only");
});

test("dirname returns the parent of a multi-segment path", () => {
  const p = ["foo", "bar", "baz.txt"].join(path.sep);
  assert.strictEqual(path.dirname(p), ["foo", "bar"].join(path.sep), "dirname drops the last segment");
});

test("normalize collapses '.' and '..' segments using the platform separator", () => {
  assert.strictEqual(
    path.normalize(["a", "b", "..", "c"].join(path.sep)),
    ["a", "c"].join(path.sep),
    "'..' removes the preceding segment"
  );
});

test("isAbsolute and the separator/delimiter constants are well-formed", () => {
  assert.strictEqual(path.isAbsolute("relative"), false, "a bare name is relative");
  assert.strictEqual(typeof path.sep, "string", "sep is a string");
  assert.strictEqual(path.sep.length, 1, "sep is a single character");
  assert.strictEqual(typeof path.delimiter, "string", "delimiter is a string");
  assert.strictEqual(path.delimiter.length, 1, "delimiter is a single character");
});
