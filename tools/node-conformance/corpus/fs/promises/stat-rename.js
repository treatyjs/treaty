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

// Node test/parallel-style: node:fs/promises stat / rename / access. Mirrors Node's
// test-fs-promises-stat / test-fs-promises-rename — the promised stat distinguishes files from
// directories and reports size, rename moves a path, and access resolves/rejects on presence.
const fsp = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");

test("stat via promises reports isFile / isDirectory / size", async () => {
  const dir = await fsp.mkdtemp(path.join(os.tmpdir(), "treaty-fsp-stat-"));
  const file = path.join(dir, "f.txt");
  await fsp.writeFile(file, "01234"); // five bytes

  const fileStat = await fsp.stat(file);
  assert.strictEqual(fileStat.isFile(), true, "the file is a file");
  assert.strictEqual(fileStat.isDirectory(), false, "the file is not a directory");
  assert.strictEqual(fileStat.size, 5, "size is the five bytes written");

  const dirStat = await fsp.stat(dir);
  assert.strictEqual(dirStat.isDirectory(), true, "the directory is a directory");

  await fsp.rm(dir, { recursive: true });
});

test("rename via promises moves a file and its contents", async () => {
  const dir = await fsp.mkdtemp(path.join(os.tmpdir(), "treaty-fsp-rename-"));
  const from = path.join(dir, "from.txt");
  const to = path.join(dir, "to.txt");

  await fsp.writeFile(from, "moved-contents");
  await fsp.rename(from, to);

  await assert.rejects(fsp.stat(from), "the old name no longer stats");
  assert.strictEqual(await fsp.readFile(to, "utf8"), "moved-contents", "the new name has the data");

  await fsp.rm(dir, { recursive: true });
});

test("access via promises resolves for a present path and rejects for a missing one", async () => {
  const dir = await fsp.mkdtemp(path.join(os.tmpdir(), "treaty-fsp-access-"));
  const file = path.join(dir, "present.txt");
  await fsp.writeFile(file, "1");

  // A resolving access() means the path is reachable; await must not throw.
  await fsp.access(file);
  await assert.rejects(
    fsp.access(path.join(dir, "absent.txt")),
    "access of a missing path must reject"
  );

  await fsp.rm(dir, { recursive: true });
});
