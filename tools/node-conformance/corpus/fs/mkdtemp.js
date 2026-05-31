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

// Node test/parallel-style: node:fs mkdtempSync. Mirrors Node's test-fs-mkdtemp — mkdtempSync creates
// a brand-new directory whose name extends the given prefix, returns its full path, and yields a
// distinct directory on each call.
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

test("mkdtempSync creates a real directory under the given prefix", () => {
  const prefix = path.join(os.tmpdir(), "treaty-mkdtemp-");
  const made = fs.mkdtempSync(prefix);

  assert.strictEqual(typeof made, "string", "mkdtempSync returns the path string");
  assert.strictEqual(made.length > prefix.length, true, "the prefix is extended with random chars");
  assert.strictEqual(made.indexOf(prefix), 0, "the returned path begins with the prefix");
  assert.strictEqual(fs.existsSync(made), true, "the directory really exists");
  assert.strictEqual(fs.statSync(made).isDirectory(), true, "and it is a directory");

  fs.rmSync(made, { recursive: true });
});

test("each mkdtempSync call yields a distinct directory", () => {
  const prefix = path.join(os.tmpdir(), "treaty-mkdtemp-uniq-");
  const a = fs.mkdtempSync(prefix);
  const b = fs.mkdtempSync(prefix);

  assert.notStrictEqual(a, b, "two calls produce different paths");
  assert.strictEqual(fs.existsSync(a), true, "first still exists");
  assert.strictEqual(fs.existsSync(b), true, "second exists independently");

  fs.rmSync(a, { recursive: true });
  fs.rmSync(b, { recursive: true });
});

test("a mkdtemp directory is writable and usable like any directory", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "treaty-mkdtemp-use-"));
  const file = path.join(dir, "inside.txt");

  fs.writeFileSync(file, "content");
  assert.deepStrictEqual(fs.readdirSync(dir), ["inside.txt"], "the file lands inside the temp dir");
  assert.strictEqual(fs.readFileSync(file, "utf8"), "content", "and reads back");

  fs.rmSync(dir, { recursive: true });
});
