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

// Node test/parallel-style: node:querystring percent codec (escape/unescape) and how it interacts
// with parse/stringify for reserved and multibyte characters.
const querystring = require("node:querystring");

test("escape percent-encodes form-reserved bytes, space as %20", () => {
  // Reserved characters become %XX (uppercase hex), space is %20 (encodeURIComponent style).
  assert.strictEqual(querystring.escape("a b&c=d"), "a%20b%26c%3Dd");
  assert.strictEqual(querystring.escape("x y"), "x%20y");
  // The unreserved set (alnum and - . _ ! ~ * ' ( )) is left verbatim.
  assert.strictEqual(querystring.escape("abcXYZ-._09"), "abcXYZ-._09");
  assert.strictEqual(querystring.escape("a!b~c*d'e(f)"), "a!b~c*d'e(f)");
});

test("escape encodes multibyte UTF-8 byte-by-byte", () => {
  // 'é' is U+00E9 -> UTF-8 [0xC3, 0xA9].
  assert.strictEqual(querystring.escape("é"), "%C3%A9");
  // '€' is U+20AC -> UTF-8 [0xE2, 0x82, 0xAC].
  assert.strictEqual(querystring.escape("€"), "%E2%82%AC");
});

test("unescape decodes percent escapes and turns + into a space", () => {
  assert.strictEqual(querystring.unescape("a%20b"), "a b");
  assert.strictEqual(querystring.unescape("a+b"), "a b");
  assert.strictEqual(querystring.unescape("a%26b"), "a&b");
  // Multibyte sequences decode back to the original character.
  assert.strictEqual(querystring.unescape("%C3%A9"), "é");
  assert.strictEqual(querystring.unescape("%E2%82%AC"), "€");
});

test("unescape is lenient: a malformed escape is kept verbatim", () => {
  // Node does not throw on a malformed %XX; it leaves it as-is.
  assert.strictEqual(querystring.unescape("100%done"), "100%done");
  // A clean string with nothing to decode is unchanged.
  assert.strictEqual(querystring.unescape("clean-string_09"), "clean-string_09");
});

test("escape and unescape round-trip arbitrary text", () => {
  for (const original of ["a b&c=d", "é日 mix", "100% sure?", "key with spaces"]) {
    assert.strictEqual(
      querystring.unescape(querystring.escape(original)),
      original,
      "round-trip of " + JSON.stringify(original)
    );
  }
});

test("parse decodes percent- and plus-encoded keys and values", () => {
  // '+' in a value decodes to a space; %XX decodes to its character.
  assert.deepStrictEqual(querystring.parse("first+name=John+Doe&city=New%20York"), {
    "first name": "John Doe",
    city: "New York",
  });
  // An encoded separator in a value survives decoding.
  assert.deepStrictEqual(querystring.parse("q=a%26b"), { q: "a&b" });
});

test("stringify escapes reserved characters so parse can recover them", () => {
  // A value containing '&' and '=' must be escaped so it does not confuse the structure.
  const obj = { "a=b": "c&d", note: "100% done" };
  const encoded = querystring.stringify(obj);
  // The structural separators are not present unescaped in the payload portions.
  assert.deepStrictEqual(querystring.parse(encoded), obj, "reserved chars survive a full round-trip");
});
