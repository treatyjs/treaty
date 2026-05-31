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

// Node test/parallel-style: node:fs constants and statSync metadata.
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const process = require("node:process");

test("fs.constants exposes the access-mode flags", () => {
  const c = fs.constants;
  assert.strictEqual(typeof c, "object", "constants is an object");
  assert.strictEqual(typeof c.F_OK, "number", "F_OK is a number");
  assert.strictEqual(typeof c.R_OK, "number", "R_OK is a number");
  assert.strictEqual(typeof c.W_OK, "number", "W_OK is a number");
  assert.strictEqual(typeof c.X_OK, "number", "X_OK is a number");
});

test("statSync reports size and file/directory kind", () => {
  const dir = path.join(os.tmpdir(), "treaty-stat-" + process.pid + "-" + Date.now());
  fs.mkdirSync(dir, { recursive: true });
  const file = path.join(dir, "f.txt");
  fs.writeFileSync(file, "abcde");

  const fileStat = fs.statSync(file);
  assert.strictEqual(fileStat.size, 5, "size equals byte length written");
  assert.strictEqual(fileStat.isFile(), true, "isFile() true for a file");
  assert.strictEqual(fileStat.isDirectory(), false, "isDirectory() false for a file");

  const dirStat = fs.statSync(dir);
  assert.strictEqual(dirStat.isDirectory(), true, "isDirectory() true for a directory");
  assert.strictEqual(dirStat.isFile(), false, "isFile() false for a directory");

  fs.unlinkSync(file);
  fs.rmdirSync(dir);
});

test("readdirSync lists directory entries", () => {
  const dir = path.join(os.tmpdir(), "treaty-readdir-" + process.pid + "-" + Date.now());
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, "one.txt"), "1");
  fs.writeFileSync(path.join(dir, "two.txt"), "2");

  const entries = fs.readdirSync(dir).sort();
  assert.deepStrictEqual(entries, ["one.txt", "two.txt"], "readdir returns both entries sorted");

  fs.unlinkSync(path.join(dir, "one.txt"));
  fs.unlinkSync(path.join(dir, "two.txt"));
  fs.rmdirSync(dir);
});
