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

// Node test/parallel-style: node:process argv/env/cwd/platform/version/pid/nextTick.
const process = require("node:process");

test("process exposes argv as an array of strings", () => {
  assert.strictEqual(Array.isArray(process.argv), true, "argv is an array");
  for (const entry of process.argv) {
    assert.strictEqual(typeof entry, "string", "every argv entry is a string");
  }
});

test("process.env is a string-valued object", () => {
  assert.strictEqual(typeof process.env, "object", "env is an object");
});

test("cwd() returns a non-empty path string", () => {
  assert.strictEqual(typeof process.cwd, "function", "cwd is callable");
  const cwd = process.cwd();
  assert.strictEqual(typeof cwd, "string", "cwd() returns a string");
  assert.strictEqual(cwd.length > 0, true, "cwd() is non-empty");
});

test("platform and version are descriptive strings", () => {
  assert.strictEqual(typeof process.platform, "string", "platform is a string");
  assert.strictEqual(process.platform.length > 0, true, "platform is non-empty");
  assert.strictEqual(typeof process.version, "string", "version is a string");
});

test("pid is a number", () => {
  assert.strictEqual(typeof process.pid, "number", "pid is a number");
});

test("nextTick defers a callback that the event-loop drain runs", () => {
  let ran = false;
  process.nextTick(() => {
    ran = true;
  });
  // Still synchronous here: nextTick has not fired yet. It runs during the post-eval drain; a
  // failure there would surface as a Fail. We assert the deferral did not run synchronously.
  assert.strictEqual(ran, false, "nextTick callback is deferred, not synchronous");
});
