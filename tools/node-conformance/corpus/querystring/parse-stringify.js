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

// Node test/parallel-style: node:querystring parse/stringify round-trips, including repeated keys
// collapsing into arrays and custom separators.
const querystring = require("node:querystring");

test("parse splits a basic query into a key/value object", () => {
  assert.deepStrictEqual(querystring.parse("a=1&b=2&c=3"), { a: "1", b: "2", c: "3" });
  // A key with no '=' yields an empty-string value.
  assert.deepStrictEqual(querystring.parse("a&b=2"), { a: "", b: "2" });
  // Empty segments are skipped; an empty input yields an empty object.
  assert.deepStrictEqual(querystring.parse("a=1&&b=2"), { a: "1", b: "2" });
  assert.deepStrictEqual(querystring.parse(""), {});
});

test("parse collects repeated keys into an ordered array", () => {
  assert.deepStrictEqual(querystring.parse("a=1&a=2&a=3"), { a: ["1", "2", "3"] });
  // A mix of single and repeated keys.
  assert.deepStrictEqual(querystring.parse("x=1&y=a&y=b"), { x: "1", y: ["a", "b"] });
});

test("stringify serializes an object back into a query string", () => {
  assert.strictEqual(querystring.stringify({ a: "1", b: "2" }), "a=1&b=2");
  // Numeric and boolean values render to their JS string form.
  assert.strictEqual(querystring.stringify({ n: 42, ok: true }), "n=42&ok=true");
  // A null/undefined value renders as an empty value (key= with nothing after).
  assert.strictEqual(querystring.stringify({ a: "1", b: null }), "a=1&b=");
});

test("stringify expands an array value into one pair per element", () => {
  assert.strictEqual(querystring.stringify({ a: ["1", "2", "3"] }), "a=1&a=2&a=3");
  assert.strictEqual(querystring.stringify({ x: "v", y: ["a", "b"] }), "x=v&y=a&y=b");
});

test("parse and stringify round-trip a multi-value object", () => {
  const obj = { name: "Ada", roles: ["admin", "user"], active: "true" };
  const encoded = querystring.stringify(obj);
  const decoded = querystring.parse(encoded);
  assert.deepStrictEqual(decoded, obj, "round-trip preserves keys, order and array values");
});

test("parse honors custom separator and equals tokens", () => {
  assert.deepStrictEqual(querystring.parse("a:1;b:2", ";", ":"), { a: "1", b: "2" });
  // stringify uses the same custom tokens.
  assert.strictEqual(querystring.stringify({ a: "1", b: "2" }, ";", ":"), "a:1;b:2");
  // Round-trip with custom tokens.
  const obj = { k: "1", j: "2" };
  assert.deepStrictEqual(querystring.parse(querystring.stringify(obj, ";", ":"), ";", ":"), obj);
});

test("decode and encode are aliases of parse and stringify", () => {
  assert.strictEqual(typeof querystring.decode, "function", "decode is exposed");
  assert.strictEqual(typeof querystring.encode, "function", "encode is exposed");
  assert.deepStrictEqual(querystring.decode("a=1&b=2"), { a: "1", b: "2" });
  assert.strictEqual(querystring.encode({ a: "1", b: "2" }), "a=1&b=2");
});
