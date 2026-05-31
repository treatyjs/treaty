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

// Node test/parallel-style: node:fs rmSync(recursive) + rmdirSync. Mirrors Node's
// test-fs-rm — recursive rmSync deletes a populated tree (files and nested directories), while
// rmdirSync removes an already-empty directory.
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

function tmp(prefix) {
  return fs.mkdtempSync(path.join(os.tmpdir(), prefix));
}

test("rmSync({recursive}) deletes a populated tree of files and subdirectories", () => {
  const dir = tmp("treaty-fs-rm-tree-");
  fs.mkdirSync(path.join(dir, "sub", "deep"), { recursive: true });
  fs.writeFileSync(path.join(dir, "top.txt"), "t");
  fs.writeFileSync(path.join(dir, "sub", "mid.txt"), "m");
  fs.writeFileSync(path.join(dir, "sub", "deep", "leaf.txt"), "l");

  assert.strictEqual(fs.existsSync(path.join(dir, "sub", "deep", "leaf.txt")), true, "tree built");

  fs.rmSync(dir, { recursive: true });
  assert.strictEqual(fs.existsSync(dir), false, "the whole tree is gone after recursive rmSync");
});

test("rmdirSync removes an empty directory", () => {
  const dir = tmp("treaty-fs-rmdir-");
  const empty = path.join(dir, "empty");
  fs.mkdirSync(empty);

  assert.strictEqual(fs.existsSync(empty), true, "empty dir created");
  fs.rmdirSync(empty);
  assert.strictEqual(fs.existsSync(empty), false, "rmdirSync removed the empty directory");

  fs.rmSync(dir, { recursive: true });
});

test("rmSync({recursive}) on a single file removes just that file", () => {
  const dir = tmp("treaty-fs-rm-file-");
  const file = path.join(dir, "solo.txt");
  fs.writeFileSync(file, "x");

  fs.rmSync(file, { recursive: true });
  assert.strictEqual(fs.existsSync(file), false, "the file is removed");
  assert.strictEqual(fs.existsSync(dir), true, "its parent directory survives");

  fs.rmSync(dir, { recursive: true });
});

test("rmSync({recursive}) empties a directory that still has children, leaving nothing", () => {
  const dir = tmp("treaty-fs-rm-children-");
  for (let i = 0; i < 5; i++) {
    fs.writeFileSync(path.join(dir, "f" + i + ".txt"), String(i));
  }
  fs.mkdirSync(path.join(dir, "nestedA", "nestedB"), { recursive: true });
  fs.writeFileSync(path.join(dir, "nestedA", "x.txt"), "x");

  fs.rmSync(dir, { recursive: true });
  assert.strictEqual(fs.existsSync(dir), false, "a directory full of children is fully removed");
});
