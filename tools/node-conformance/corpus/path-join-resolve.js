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

// Node test/parallel-style: node:path join/resolve/dirname/basename/extname/normalize and the
// platform sep/delimiter + posix/win32 sub-namespaces.
const path = require("node:path");

test("join concatenates with the platform separator", () => {
  assert.strictEqual(path.join("a", "b", "c"), ["a", "b", "c"].join(path.sep), "join uses sep");
});

test("resolve produces an absolute path", () => {
  assert.strictEqual(path.isAbsolute(path.resolve("x")), true, "resolve yields an absolute path");
});

test("dirname/basename/extname decompose a path", () => {
  assert.strictEqual(path.dirname("/foo/bar/baz.txt"), "/foo/bar", "dirname");
  assert.strictEqual(path.basename("/foo/bar/baz.txt"), "baz.txt", "basename");
  assert.strictEqual(path.basename("baz.txt", ".txt"), "baz", "basename strips a suffix");
  assert.strictEqual(path.extname("baz.txt"), ".txt", "extname");
  assert.strictEqual(path.extname("noext"), "", "extname of a name with no dot is empty");
});

test("normalize collapses . and .. segments", () => {
  assert.strictEqual(path.normalize("a//b/../c"), ["a", "c"].join(path.sep), "normalize collapses");
});

test("isAbsolute distinguishes absolute from relative", () => {
  assert.strictEqual(path.isAbsolute("/a"), true, "leading slash is absolute");
  assert.strictEqual(path.isAbsolute("a"), false, "bare name is relative");
});

test("sep and delimiter are non-empty strings", () => {
  assert.strictEqual(typeof path.sep, "string", "sep is a string");
  assert.strictEqual(path.sep.length, 1, "sep is a single character");
  assert.strictEqual(typeof path.delimiter, "string", "delimiter is a string");
});

test("posix namespace always joins with forward slashes", () => {
  assert.strictEqual(path.posix.join("a", "b", "c"), "a/b/c", "posix.join uses /");
  assert.strictEqual(path.posix.sep, "/", "posix.sep is /");
});

test("win32 namespace uses backslash separator", () => {
  assert.strictEqual(path.win32.sep, "\\", "win32.sep is backslash");
});
