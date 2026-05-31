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

// Node test/parallel-style: node:util.format placeholder substitution — %s/%d/%i/%f/%j/%o, the
// %% literal-percent escape, consumed-but-unmatched directives, and trailing/extra arguments.
const util = require("node:util");

test("%s and %d substitute a string and a number", () => {
  assert.strictEqual(util.format("%s = %d", "x", 7), "x = 7", "%s then %d");
  assert.strictEqual(util.format("%s", "only"), "only", "a single %s");
});

test("%i truncates to an integer while %f keeps the fraction", () => {
  assert.strictEqual(util.format("%i", 3.9), "3", "%i truncates toward zero");
  assert.strictEqual(util.format("%f", 2.5), "2.5", "%f preserves the fraction");
});

test("%d of a non-numeric argument is NaN", () => {
  assert.strictEqual(util.format("%d", "abc"), "NaN", "a non-numeric %d argument formats as NaN");
});

test("%j serializes its argument as JSON", () => {
  assert.strictEqual(util.format("%j", { a: 1 }), '{"a":1}', "%j produces JSON");
  assert.strictEqual(util.format("%j", [1, 2]), "[1,2]", "%j of an array");
});

test("%% is a literal percent and consumes no argument", () => {
  assert.strictEqual(util.format("100%%"), "100%", "%% escapes to a single percent");
  assert.strictEqual(util.format("%d%%", 50), "50%", "%% alongside a real directive");
});

test("extra arguments beyond the format string are space-appended", () => {
  assert.strictEqual(util.format("a", "b", "c"), "a b c", "extra string args are space-joined");
});

test("a plain object given as an extra argument is inspected and appended", () => {
  assert.strictEqual(util.format("x", { a: 1 }), "x { a: 1 }", "an extra object is inspected");
});

test("a format string with no placeholders is returned unchanged", () => {
  assert.strictEqual(util.format("just text"), "just text", "no directives means no substitution");
});
