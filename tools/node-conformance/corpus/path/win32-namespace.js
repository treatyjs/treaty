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

// Node test/parallel-style: node:path.win32 — the Windows path sub-namespace uses the backslash
// separator and semicolon delimiter and joins/normalizes with backslashes regardless of host
// platform. (Drive-letter root semantics — isAbsolute("C:\\…"), drive-anchored dirname/relative —
// are exercised separately in path/win32-drive-roots so the precise, currently-incomplete surface is
// documented rather than silently asserted here.)
const path = require("node:path");
const win32 = path.win32;

test("win32.sep and win32.delimiter are the Windows constants", () => {
  assert.strictEqual(win32.sep, "\\", "win32.sep is a backslash");
  assert.strictEqual(win32.delimiter, ";", "win32.delimiter is a semicolon");
});

test("win32.join concatenates with backslashes and collapses doubled separators", () => {
  assert.strictEqual(win32.join("a", "b", "c"), "a\\b\\c", "join uses backslashes");
  assert.strictEqual(win32.join("a\\", "b"), "a\\b", "a trailing separator is collapsed");
  assert.strictEqual(win32.join("a", "", "b"), "a\\b", "empty segments are dropped");
});

test("win32.extname returns the dotted extension of the final segment", () => {
  assert.strictEqual(win32.extname("baz.txt"), ".txt", "extname of a simple name");
  assert.strictEqual(win32.extname("noext"), "", "no dot means no extension");
  assert.strictEqual(win32.extname("a.tar.gz"), ".gz", "only the last extension is returned");
});

test("win32.isAbsolute recognizes a UNC root and rejects a bare relative name", () => {
  assert.strictEqual(win32.isAbsolute("\\\\server\\share"), true, "a UNC path is absolute");
  assert.strictEqual(win32.isAbsolute("a\\b"), false, "a bare relative name is not absolute");
});

test("win32.resolve yields an absolute path", () => {
  assert.strictEqual(win32.isAbsolute(win32.resolve("x")), true, "resolve produces an absolute path");
});
