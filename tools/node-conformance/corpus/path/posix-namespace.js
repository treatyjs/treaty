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

// Node test/parallel-style: node:path.posix — the POSIX path sub-namespace always uses forward
// slashes regardless of host platform: join/normalize/dirname/basename/extname/isAbsolute/relative.
const path = require("node:path");
const posix = path.posix;

test("posix.sep and posix.delimiter are the POSIX constants", () => {
  assert.strictEqual(posix.sep, "/", "posix.sep is a forward slash");
  assert.strictEqual(posix.delimiter, ":", "posix.delimiter is a colon");
});

test("posix.join uses forward slashes and collapses repeats", () => {
  assert.strictEqual(posix.join("a", "b", "c"), "a/b/c", "join with forward slashes");
  assert.strictEqual(posix.join("/a", "b"), "/a/b", "an absolute root is preserved");
  assert.strictEqual(posix.join("a/", "/b"), "a/b", "interior separators are collapsed");
});

test("posix.normalize collapses '.' and '..' against forward slashes", () => {
  assert.strictEqual(posix.normalize("/a/b/../c"), "/a/c", "'..' removes the previous segment");
  assert.strictEqual(posix.normalize("a//b/./c"), "a/b/c", "'.' and doubled slashes collapse");
});

test("posix dirname/basename/extname decompose a forward-slash path", () => {
  assert.strictEqual(posix.dirname("/foo/bar/baz.txt"), "/foo/bar", "dirname");
  assert.strictEqual(posix.basename("/foo/bar/baz.txt"), "baz.txt", "basename");
  assert.strictEqual(posix.basename("/foo/bar/baz.txt", ".txt"), "baz", "basename strips suffix");
  assert.strictEqual(posix.extname("/foo/bar/baz.txt"), ".txt", "extname");
});

test("posix.isAbsolute keys on a leading forward slash", () => {
  assert.strictEqual(posix.isAbsolute("/a/b"), true, "a leading slash is absolute");
  assert.strictEqual(posix.isAbsolute("a/b"), false, "no leading slash is relative");
});

test("posix.relative walks up then down between two absolute paths", () => {
  assert.strictEqual(
    posix.relative("/a/b/c", "/a/b/d/e"),
    "../d/e",
    "relative climbs out of c and into d/e"
  );
  assert.strictEqual(posix.relative("/a/b", "/a/b"), "", "the relative path to itself is empty");
});
