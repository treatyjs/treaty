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

// Node test/parallel-style: URLSearchParams construction/mutation and the live link back to a URL.
//
// Both classes are reached via `require("node:url")`. Covers string/array/record construction, the
// get/getAll/has/set/append/delete/sort surface, form-encoded toString, iteration, and the live
// back-reference that re-serializes a URL's search/href when its searchParams is mutated.
const url = require("node:url");
const URL = url.URL;
const URLSearchParams = url.URLSearchParams;

test("a query string parses into ordered, repeatable pairs", () => {
  const p = new URLSearchParams("a=1&b=2&a=3");
  assert.strictEqual(p.get("a"), "1", "get returns the first value");
  assert.deepStrictEqual(p.getAll("a"), ["1", "3"], "getAll returns all values in order");
  assert.strictEqual(p.has("b"), true, "has(b)");
  assert.strictEqual(p.has("z"), false, "has(z)");
  assert.strictEqual(p.size, 3, "size counts every pair");
  assert.strictEqual(p.toString(), "a=1&b=2&a=3", "toString round-trips");
});

test("append, set and delete behave per the WHATWG surface", () => {
  const p = new URLSearchParams();
  p.append("k", "v1");
  p.append("k", "v2");
  p.append("m", "x");
  // set replaces all existing entries for the key with a single one (kept at first position).
  p.set("k", "only");
  assert.strictEqual(p.toString(), "k=only&m=x", "set collapses duplicates");
  p.delete("m");
  assert.strictEqual(p.toString(), "k=only", "delete removes the key");
  assert.deepStrictEqual(p.getAll("k"), ["only"], "only the set value remains");
});

test("toString form-encodes spaces as '+' and reserved characters as %XX", () => {
  const p = new URLSearchParams();
  p.append("a b", "c&d");
  assert.strictEqual(p.toString(), "a+b=c%26d", "form-encoding of key and value");
});

test("iteration yields decoded key/value pairs", () => {
  const p = new URLSearchParams("a+b=c%26d");
  const pairs = [];
  for (const [k, v] of p) {
    pairs.push([k, v]);
  }
  assert.deepStrictEqual(pairs, [["a b", "c&d"]], "decoded pairs from iteration");
  assert.deepStrictEqual([...p.keys()], ["a b"], "keys() iterates decoded keys");
});

test("construction from a record and from an array of pairs", () => {
  const fromObj = new URLSearchParams({ a: "1", b: "2" });
  const fromArr = new URLSearchParams([["x", "9"], ["y", "8"]]);
  assert.strictEqual(fromObj.toString(), "a=1&b=2", "record construction");
  assert.strictEqual(fromArr.toString(), "x=9&y=8", "array construction");
});

test("sort orders the pairs by key", () => {
  const p = new URLSearchParams("c=3&a=1&b=2");
  p.sort();
  assert.strictEqual(p.toString(), "a=1&b=2&c=3", "sorted by key");
});

test("a URL's searchParams reflects its query", () => {
  const u = new URL("https://a.com/p?x=1&y=2");
  assert.strictEqual(u.searchParams.get("x"), "1", "x");
  assert.strictEqual(u.searchParams.get("y"), "2", "y");
  assert.deepStrictEqual(u.searchParams.getAll("x"), ["1"], "getAll(x)");
});

test("mutating a URL's searchParams re-serializes its search and href", () => {
  const u = new URL("https://a.com/p?x=1");
  u.searchParams.append("y", "2");
  assert.strictEqual(u.search, "?x=1&y=2", "search updated via live back-reference");
  assert.strictEqual(u.href, "https://a.com/p?x=1&y=2", "href updated via live back-reference");
});

test("setting a URL's search re-parses its searchParams", () => {
  const u = new URL("https://a.com/p?x=1");
  u.search = "?a=9&b=8";
  assert.strictEqual(u.searchParams.get("a"), "9", "search setter repopulates params");
  assert.strictEqual(u.searchParams.get("b"), "8", "second param");
});
