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

// node:path.win32 drive-letter root semantics: a drive-anchored path is absolute, dirname keeps the
// drive and the parent directories, and relative walks correctly between two drive-anchored paths.
const path = require("node:path");
const win32 = path.win32;

test("win32.isAbsolute treats a drive-letter root as absolute", () => {
  assert.strictEqual(win32.isAbsolute("C:\\a\\b"), true, "a drive-rooted path is absolute");
  assert.strictEqual(win32.isAbsolute("C:a\\b"), false, "a drive-relative path is NOT absolute");
});

test("win32.dirname keeps the drive and the parent directories", () => {
  assert.strictEqual(win32.dirname("C:\\foo\\bar\\baz.txt"), "C:\\foo\\bar", "dirname keeps the drive");
  assert.strictEqual(win32.dirname("C:\\foo"), "C:\\", "dirname of a top-level file is the drive root");
});

test("win32.relative walks between two drive-anchored paths", () => {
  assert.strictEqual(win32.relative("C:\\a\\b\\c", "C:\\a\\b\\d\\e"), "..\\d\\e", "relative on a drive");
});
