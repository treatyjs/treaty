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

// Node test/parallel-style: node:fs mkdirSync (recursive) + readdirSync. Mirrors Node's
// test-fs-mkdir-recursive / test-fs-readdir — a recursive mkdir creates intermediate directories, and
// readdirSync lists exactly the names directly inside a directory.
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

function tmp(prefix) {
  return fs.mkdtempSync(path.join(os.tmpdir(), prefix));
}

test("mkdirSync({recursive}) creates a deep directory chain", () => {
  const dir = tmp("treaty-fs-mkdir-");
  const deep = path.join(dir, "a", "b", "c");

  fs.mkdirSync(deep, { recursive: true });
  assert.strictEqual(fs.existsSync(deep), true, "the deepest directory exists");
  assert.strictEqual(fs.existsSync(path.join(dir, "a", "b")), true, "an intermediate exists");
  assert.strictEqual(fs.statSync(deep).isDirectory(), true, "the created leaf is a directory");

  fs.rmSync(dir, { recursive: true });
});

test("readdirSync lists exactly the direct entries, not nested ones", () => {
  const dir = tmp("treaty-fs-readdir-");
  fs.writeFileSync(path.join(dir, "alpha.txt"), "1");
  fs.writeFileSync(path.join(dir, "beta.txt"), "2");
  fs.mkdirSync(path.join(dir, "nested"));
  fs.writeFileSync(path.join(dir, "nested", "hidden.txt"), "3");

  const entries = fs.readdirSync(dir).slice().sort();
  assert.deepStrictEqual(
    entries,
    ["alpha.txt", "beta.txt", "nested"],
    "readdir lists the two files and the subdir, but not the nested file"
  );

  fs.rmSync(dir, { recursive: true });
});

test("readdirSync of a freshly-made empty directory is an empty array", () => {
  const dir = tmp("treaty-fs-readdir-empty-");
  const empty = path.join(dir, "empty");
  fs.mkdirSync(empty);

  const entries = fs.readdirSync(empty);
  assert.strictEqual(Array.isArray(entries), true, "readdir returns an array");
  assert.strictEqual(entries.length, 0, "an empty directory lists nothing");

  fs.rmSync(dir, { recursive: true });
});

test("readdirSync reflects a file added then removed", () => {
  const dir = tmp("treaty-fs-readdir-delta-");
  assert.strictEqual(fs.readdirSync(dir).length, 0, "starts empty");

  const f = path.join(dir, "temp.txt");
  fs.writeFileSync(f, "x");
  assert.deepStrictEqual(fs.readdirSync(dir), ["temp.txt"], "shows the added file");

  fs.unlinkSync(f);
  assert.strictEqual(fs.readdirSync(dir).length, 0, "empty again after unlink");

  fs.rmSync(dir, { recursive: true });
});
