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

// Node test/parallel-style: node:fs statSync. Mirrors Node's test-fs-stat — the returned Stats object
// distinguishes files from directories (isFile / isDirectory) and reports the byte size, and statSync
// of a missing path throws.
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

function tmp(prefix) {
  return fs.mkdtempSync(path.join(os.tmpdir(), prefix));
}

test("statSync of a file: isFile true, isDirectory false, size is the byte length", () => {
  const dir = tmp("treaty-fs-stat-file-");
  const file = path.join(dir, "payload.txt");
  fs.writeFileSync(file, "0123456789"); // ten ASCII bytes

  const st = fs.statSync(file);
  assert.strictEqual(st.isFile(), true, "a regular file is a file");
  assert.strictEqual(st.isDirectory(), false, "a regular file is not a directory");
  assert.strictEqual(st.size, 10, "size equals the ten bytes written");

  fs.rmSync(dir, { recursive: true });
});

test("statSync of a directory: isDirectory true, isFile false", () => {
  const dir = tmp("treaty-fs-stat-dir-");
  const sub = path.join(dir, "child");
  fs.mkdirSync(sub);

  const st = fs.statSync(sub);
  assert.strictEqual(st.isDirectory(), true, "a directory is a directory");
  assert.strictEqual(st.isFile(), false, "a directory is not a file");

  fs.rmSync(dir, { recursive: true });
});

test("size tracks content changes across rewrites", () => {
  const dir = tmp("treaty-fs-stat-size-");
  const file = path.join(dir, "grow.txt");

  fs.writeFileSync(file, "ab");
  assert.strictEqual(fs.statSync(file).size, 2, "two bytes after the first write");
  fs.writeFileSync(file, "abcdef");
  assert.strictEqual(fs.statSync(file).size, 6, "six bytes after the rewrite");
  fs.appendFileSync(file, "gh");
  assert.strictEqual(fs.statSync(file).size, 8, "eight bytes after the append");

  fs.rmSync(dir, { recursive: true });
});

test("statSync of a nonexistent path throws", () => {
  const dir = tmp("treaty-fs-stat-missing-");
  assert.throws(
    () => fs.statSync(path.join(dir, "nope")),
    "statSync of a missing path must throw"
  );
  fs.rmSync(dir, { recursive: true });
});
