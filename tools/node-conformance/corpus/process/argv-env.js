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

// Node test/parallel-style: node:process argv/env (read, write, delete), cwd, and the descriptive
// platform/version/pid identity fields.
const process = require("node:process");

test("argv is an array of strings", () => {
  assert.strictEqual(Array.isArray(process.argv), true, "argv is an array");
  for (const entry of process.argv) {
    assert.strictEqual(typeof entry, "string", "every argv entry is a string");
  }
});

test("env is a string-valued object that round-trips writes and deletes", () => {
  assert.strictEqual(typeof process.env, "object", "env is an object");
  process.env.TREATY_CONFORMANCE_VAR = "set-value";
  assert.strictEqual(
    process.env.TREATY_CONFORMANCE_VAR,
    "set-value",
    "a value written to env reads back"
  );
  delete process.env.TREATY_CONFORMANCE_VAR;
  assert.strictEqual(
    process.env.TREATY_CONFORMANCE_VAR,
    undefined,
    "a deleted env var reads back as undefined"
  );
});

test("distinct env keys are stored and read back independently", () => {
  process.env.TREATY_A = "alpha";
  process.env.TREATY_B = "beta";
  assert.strictEqual(process.env.TREATY_A, "alpha", "the first key reads back its own value");
  assert.strictEqual(process.env.TREATY_B, "beta", "the second key reads back its own value");
  delete process.env.TREATY_A;
  assert.strictEqual(process.env.TREATY_A, undefined, "deleting one key leaves the other intact");
  assert.strictEqual(process.env.TREATY_B, "beta", "the untouched key is still present");
  delete process.env.TREATY_B;
});

test("cwd() returns a non-empty path string", () => {
  assert.strictEqual(typeof process.cwd, "function", "cwd is callable");
  const cwd = process.cwd();
  assert.strictEqual(typeof cwd, "string", "cwd() returns a string");
  assert.strictEqual(cwd.length > 0, true, "cwd() is non-empty");
});

test("platform, version and pid describe the runtime", () => {
  assert.strictEqual(typeof process.platform, "string", "platform is a string");
  assert.strictEqual(process.platform.length > 0, true, "platform is non-empty");
  assert.strictEqual(typeof process.version, "string", "version is a string");
  assert.strictEqual(process.version.length > 0, true, "version is non-empty");
  assert.strictEqual(typeof process.pid, "number", "pid is a number");
});
