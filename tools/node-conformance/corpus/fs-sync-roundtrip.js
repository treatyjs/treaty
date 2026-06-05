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

// Node test/parallel-style: node:fs synchronous write/read/exists/append/copy/rename/unlink in a
// per-process temp dir. Mirrors test-fs-* sync round-trips. Cleanup is explicit (unlink then
// rmdir) because the runtime's recursive rmSync does not yet recurse into non-empty directories.
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const process = require("node:process");

const dir = path.join(os.tmpdir(), "treaty-fs-" + process.pid + "-" + Date.now());
fs.mkdirSync(dir, { recursive: true });

test("writeFileSync then readFileSync round-trips utf8", () => {
  const file = path.join(dir, "data.txt");
  fs.writeFileSync(file, "forty-two");
  assert.strictEqual(fs.existsSync(file), true, "file exists after write");
  assert.strictEqual(fs.readFileSync(file, "utf8"), "forty-two", "read returns what was written");
  fs.unlinkSync(file);
  assert.strictEqual(fs.existsSync(file), false, "file gone after unlink");
});

test("appendFileSync appends to an existing file", () => {
  const file = path.join(dir, "log.txt");
  fs.writeFileSync(file, "abc");
  fs.appendFileSync(file, "de");
  assert.strictEqual(fs.readFileSync(file, "utf8"), "abcde", "append concatenates");
  fs.unlinkSync(file);
});

test("copyFileSync duplicates contents", () => {
  const src = path.join(dir, "src.txt");
  const dst = path.join(dir, "dst.txt");
  fs.writeFileSync(src, "payload");
  fs.copyFileSync(src, dst);
  assert.strictEqual(fs.readFileSync(dst, "utf8"), "payload", "copy preserves contents");
  fs.unlinkSync(src);
  fs.unlinkSync(dst);
});

test("renameSync moves a file", () => {
  const a = path.join(dir, "a.txt");
  const b = path.join(dir, "b.txt");
  fs.writeFileSync(a, "x");
  fs.renameSync(a, b);
  assert.strictEqual(fs.existsSync(a), false, "old name gone");
  assert.strictEqual(fs.existsSync(b), true, "new name present");
  fs.unlinkSync(b);
});

// Empty dir removal is supported; remove the temp dir we created.
fs.rmdirSync(dir);
assert.strictEqual(fs.existsSync(dir), false, "temp dir removed");
