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

// node:path parse()/format() round-trip: parse(p) decomposes into {root, dir, base, ext, name} and
// format(parse(p)) re-derives p. Exercised on the explicit posix and win32 namespaces so the
// contract holds regardless of the host platform default.
const path = require("node:path");

test("path.posix.parse decomposes into {root, dir, base, ext, name}", () => {
  const parsed = path.posix.parse("/home/user/file.txt");
  assert.strictEqual(parsed.root, "/", "root");
  assert.strictEqual(parsed.dir, "/home/user", "dir");
  assert.strictEqual(parsed.base, "file.txt", "base");
  assert.strictEqual(parsed.ext, ".txt", "ext");
  assert.strictEqual(parsed.name, "file", "name");
});

test("path.posix.format is the inverse of path.posix.parse", () => {
  const p = "/home/user/file.txt";
  assert.strictEqual(path.posix.format(path.posix.parse(p)), p, "format(parse(p)) === p");
});

test("path.win32.parse decomposes a drive-rooted path", () => {
  const parsed = path.win32.parse("C:\\Users\\dev\\notes.md");
  assert.strictEqual(parsed.root, "C:\\", "root keeps the drive");
  assert.strictEqual(parsed.dir, "C:\\Users\\dev", "dir keeps the drive and parents");
  assert.strictEqual(parsed.base, "notes.md", "base");
  assert.strictEqual(parsed.ext, ".md", "ext");
  assert.strictEqual(parsed.name, "notes", "name");
});

test("path.win32.format is the inverse of path.win32.parse", () => {
  const p = "C:\\Users\\dev\\notes.md";
  assert.strictEqual(path.win32.format(path.win32.parse(p)), p, "format(parse(p)) === p");
});
