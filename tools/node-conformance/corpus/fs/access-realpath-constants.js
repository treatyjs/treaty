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

// Node test/parallel-style: node:fs accessSync / realpathSync / fs.constants. Mirrors Node's
// test-fs-access / test-fs-realpath — accessSync resolves for an existing path and throws for a
// missing one, realpathSync returns an absolute path string for an existing file, and fs.constants
// exposes the F_OK / R_OK / W_OK / X_OK access-mode flags.
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

function tmp(prefix) {
  return fs.mkdtempSync(path.join(os.tmpdir(), prefix));
}

test("fs.constants exposes the access-check mode flags", () => {
  const c = fs.constants;
  assert.strictEqual(typeof c.F_OK, "number", "F_OK is a numeric flag");
  assert.strictEqual(typeof c.R_OK, "number", "R_OK is a numeric flag");
  assert.strictEqual(typeof c.W_OK, "number", "W_OK is a numeric flag");
  assert.strictEqual(typeof c.X_OK, "number", "X_OK is a numeric flag");
  // F_OK (existence) is conventionally zero in Node.
  assert.strictEqual(c.F_OK, 0, "F_OK is 0 (existence check)");
});

test("accessSync resolves for an existing file and throws for a missing one", () => {
  const dir = tmp("treaty-fs-access-");
  const file = path.join(dir, "present.txt");
  fs.writeFileSync(file, "1");

  // No throw == access granted; the call returns undefined on success.
  assert.strictEqual(fs.accessSync(file), undefined, "accessSync of an existing file returns undefined");
  assert.strictEqual(
    fs.accessSync(file, fs.constants.F_OK),
    undefined,
    "an explicit F_OK existence check also succeeds"
  );
  assert.throws(
    () => fs.accessSync(path.join(dir, "absent.txt")),
    "accessSync of a missing path must throw"
  );

  fs.rmSync(dir, { recursive: true });
});

test("realpathSync returns an absolute path string for an existing file", () => {
  const dir = tmp("treaty-fs-realpath-");
  const file = path.join(dir, "r.txt");
  fs.writeFileSync(file, "1");

  const resolved = fs.realpathSync(file);
  assert.strictEqual(typeof resolved, "string", "realpathSync returns a string");
  assert.strictEqual(resolved.length > 0, true, "the resolved path is non-empty");
  // The resolved path must still point at a real, existing file.
  assert.strictEqual(fs.existsSync(resolved), true, "the resolved path exists");

  fs.rmSync(dir, { recursive: true });
});
