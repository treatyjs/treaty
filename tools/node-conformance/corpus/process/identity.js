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

// Node test/parallel-style: node:process identity + introspection fields — platform is one of the
// known Node values, version is a semver-ish string, argv0 is a string, and hrtime measures elapsed
// time as a [seconds, nanoseconds] tuple.
const process = require("node:process");

test("platform is one of the values Node reports", () => {
  const known = ["aix", "darwin", "freebsd", "linux", "openbsd", "sunos", "win32"];
  assert.strictEqual(
    known.indexOf(process.platform) !== -1,
    true,
    "platform is a recognized Node platform string: " + process.platform
  );
});

test("version is a non-empty string beginning with 'v'", () => {
  assert.strictEqual(typeof process.version, "string", "version is a string");
  assert.strictEqual(process.version[0], "v", "Node-style version is prefixed with 'v'");
});

test("argv0 is a string", () => {
  assert.strictEqual(typeof process.argv0, "string", "argv0 is a string");
});

test("hrtime returns a [seconds, nanoseconds] tuple of non-negative integers", () => {
  assert.strictEqual(typeof process.hrtime, "function", "hrtime is callable");
  const t = process.hrtime();
  assert.strictEqual(Array.isArray(t), true, "hrtime returns an array");
  assert.strictEqual(t.length, 2, "the tuple has two elements");
  assert.strictEqual(Number.isFinite(t[0]), true, "seconds is a finite number");
  assert.strictEqual(Number.isFinite(t[1]), true, "nanoseconds is a finite number");
  assert.strictEqual(t[0] >= 0, true, "seconds is non-negative");
  assert.strictEqual(t[1] >= 0, true, "nanoseconds is non-negative");
});

test("hrtime(prev) measures a non-negative delta against an earlier reading", () => {
  const start = process.hrtime();
  // A little synchronous work so some time elapses.
  let acc = 0;
  for (let i = 0; i < 1000; i += 1) acc += i;
  const delta = process.hrtime(start);
  assert.strictEqual(Array.isArray(delta), true, "the delta is a tuple");
  assert.strictEqual(delta[0] >= 0, true, "the elapsed seconds component is non-negative");
  assert.strictEqual(acc > 0, true, "the timed work actually ran");
});
