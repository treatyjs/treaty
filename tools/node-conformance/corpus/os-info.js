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

// Node test/parallel-style: node:os platform/arch/type/release/hostname/tmpdir/homedir/EOL/cpus.
const os = require("node:os");

test("platform/arch/type/release/hostname return strings", () => {
  assert.strictEqual(typeof os.platform(), "string", "platform()");
  assert.strictEqual(typeof os.arch(), "string", "arch()");
  assert.strictEqual(typeof os.type(), "string", "type()");
  assert.strictEqual(typeof os.release(), "string", "release()");
  assert.strictEqual(typeof os.hostname(), "string", "hostname()");
});

test("tmpdir and homedir return non-empty path strings", () => {
  assert.strictEqual(typeof os.tmpdir(), "string", "tmpdir() is a string");
  assert.strictEqual(os.tmpdir().length > 0, true, "tmpdir() is non-empty");
  assert.strictEqual(typeof os.homedir(), "string", "homedir() is a string");
  assert.strictEqual(os.homedir().length > 0, true, "homedir() is non-empty");
});

test("EOL is a line terminator string", () => {
  assert.strictEqual(typeof os.EOL, "string", "EOL is a string");
  assert.strictEqual(os.EOL === "\n" || os.EOL === "\r\n", true, "EOL is \\n or \\r\\n");
});

test("totalmem/freemem are numbers and cpus() is a non-empty array", () => {
  assert.strictEqual(typeof os.totalmem(), "number", "totalmem() is a number");
  assert.strictEqual(typeof os.freemem(), "number", "freemem() is a number");
  const cpus = os.cpus();
  assert.strictEqual(Array.isArray(cpus), true, "cpus() returns an array");
  assert.strictEqual(cpus.length > 0, true, "cpus() reports at least one core");
});

test("endianness reports BE or LE", () => {
  const e = os.endianness();
  assert.strictEqual(e === "BE" || e === "LE", true, "endianness is BE or LE");
});
