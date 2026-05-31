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

// Node test/parallel-style: node:fs copyFileSync / cpSync / renameSync. Mirrors Node's
// test-fs-copyfile / test-fs-cp / test-fs-rename — copyFileSync duplicates a file's contents, cpSync
// with {recursive} copies a whole directory tree, and renameSync moves a path.
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

function tmp(prefix) {
  return fs.mkdtempSync(path.join(os.tmpdir(), prefix));
}

test("copyFileSync duplicates a file's contents, leaving the source intact", () => {
  const dir = tmp("treaty-fs-copyfile-");
  const src = path.join(dir, "src.txt");
  const dst = path.join(dir, "dst.txt");

  fs.writeFileSync(src, "payload-contents");
  fs.copyFileSync(src, dst);

  assert.strictEqual(fs.readFileSync(dst, "utf8"), "payload-contents", "the copy has the contents");
  assert.strictEqual(fs.existsSync(src), true, "the source still exists after the copy");
  assert.strictEqual(fs.readFileSync(src, "utf8"), "payload-contents", "the source is unchanged");

  fs.rmSync(dir, { recursive: true });
});

test("copyFileSync overwrites the destination if it already exists", () => {
  const dir = tmp("treaty-fs-copyfile-over-");
  const src = path.join(dir, "a.txt");
  const dst = path.join(dir, "b.txt");

  fs.writeFileSync(src, "new");
  fs.writeFileSync(dst, "old-and-longer");
  fs.copyFileSync(src, dst);
  assert.strictEqual(fs.readFileSync(dst, "utf8"), "new", "the destination is replaced");

  fs.rmSync(dir, { recursive: true });
});

test("cpSync({recursive}) copies a directory tree including nested files", () => {
  const dir = tmp("treaty-fs-cp-");
  const srcRoot = path.join(dir, "src");
  const dstRoot = path.join(dir, "dst");
  fs.mkdirSync(path.join(srcRoot, "nested"), { recursive: true });
  fs.writeFileSync(path.join(srcRoot, "top.txt"), "top");
  fs.writeFileSync(path.join(srcRoot, "nested", "leaf.txt"), "leaf");

  fs.cpSync(srcRoot, dstRoot, { recursive: true });

  assert.strictEqual(fs.readFileSync(path.join(dstRoot, "top.txt"), "utf8"), "top", "top copied");
  assert.strictEqual(
    fs.readFileSync(path.join(dstRoot, "nested", "leaf.txt"), "utf8"),
    "leaf",
    "the nested file copied too"
  );
  assert.strictEqual(fs.existsSync(srcRoot), true, "the source tree survives the copy");

  fs.rmSync(dir, { recursive: true });
});

test("renameSync moves a file from its old name to a new one", () => {
  const dir = tmp("treaty-fs-rename-");
  const a = path.join(dir, "a.txt");
  const b = path.join(dir, "b.txt");

  fs.writeFileSync(a, "movable");
  fs.renameSync(a, b);

  assert.strictEqual(fs.existsSync(a), false, "the old name is gone");
  assert.strictEqual(fs.existsSync(b), true, "the new name is present");
  assert.strictEqual(fs.readFileSync(b, "utf8"), "movable", "the contents moved with it");

  fs.rmSync(dir, { recursive: true });
});

test("renameSync can move a file into a subdirectory", () => {
  const dir = tmp("treaty-fs-rename-sub-");
  const sub = path.join(dir, "sub");
  fs.mkdirSync(sub);
  const from = path.join(dir, "x.txt");
  const to = path.join(sub, "x.txt");

  fs.writeFileSync(from, "data");
  fs.renameSync(from, to);
  assert.strictEqual(fs.existsSync(from), false, "removed from the original directory");
  assert.deepStrictEqual(fs.readdirSync(sub), ["x.txt"], "now lives in the subdirectory");

  fs.rmSync(dir, { recursive: true });
});
