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

// Node test/parallel-style: node:fs appendFileSync. Mirrors Node's test-fs-append-file-sync —
// appendFileSync appends to an existing file and creates the file when it is absent.
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

function tmp(prefix) {
  return fs.mkdtempSync(path.join(os.tmpdir(), prefix));
}

test("appendFileSync concatenates onto an existing file", () => {
  const dir = tmp("treaty-fs-append-");
  const file = path.join(dir, "log.txt");

  fs.writeFileSync(file, "abc");
  fs.appendFileSync(file, "de");
  fs.appendFileSync(file, "f");
  assert.strictEqual(fs.readFileSync(file, "utf8"), "abcdef", "appends in order, never truncates");

  fs.rmSync(dir, { recursive: true });
});

test("appendFileSync creates the file when it does not yet exist", () => {
  const dir = tmp("treaty-fs-append-new-");
  const file = path.join(dir, "fresh.txt");

  assert.strictEqual(fs.existsSync(file), false, "no file before the append");
  fs.appendFileSync(file, "created-by-append");
  assert.strictEqual(fs.existsSync(file), true, "append created the file");
  assert.strictEqual(
    fs.readFileSync(file, "utf8"),
    "created-by-append",
    "the created file holds exactly the appended bytes"
  );

  fs.rmSync(dir, { recursive: true });
});

test("a sequence of appends builds the full content", () => {
  const dir = tmp("treaty-fs-append-seq-");
  const file = path.join(dir, "seq.txt");

  const parts = ["one", "-two", "-three", "-four"];
  for (const part of parts) {
    fs.appendFileSync(file, part);
  }
  assert.strictEqual(
    fs.readFileSync(file, "utf8"),
    parts.join(""),
    "the file equals the concatenation of every appended chunk"
  );

  fs.rmSync(dir, { recursive: true });
});
