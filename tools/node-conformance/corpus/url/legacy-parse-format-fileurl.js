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

// Node test/parallel-style: the legacy functional node:url API — parse/format and the file-URL
// conversions.
//
// `url.parse` decomposes into the legacy Url-shaped object; `url.format` reverses it. The file-URL
// conversions are platform-native, so they are exercised with a `process.platform`-aware fixture and
// a path -> fileURL -> path round-trip, which holds identically on POSIX and Win32.
const url = require("node:url");
const process = require("node:process");

test("parse decomposes an absolute URL into the legacy component fields", () => {
  const u = url.parse("https://example.com:8080/a/b?q=1#frag");
  assert.strictEqual(u.protocol, "https:", "protocol");
  assert.strictEqual(u.hostname, "example.com", "hostname");
  assert.strictEqual(u.port, "8080", "port");
  assert.strictEqual(u.host, "example.com:8080", "host includes the port");
  assert.strictEqual(u.pathname, "/a/b", "pathname");
  assert.strictEqual(u.search, "?q=1", "search includes the leading '?'");
  assert.strictEqual(u.query, "q=1", "query excludes the leading '?'");
  assert.strictEqual(u.hash, "#frag", "hash includes the leading '#'");
});

test("parse handles a URL without a port", () => {
  const u = url.parse("http://host/path");
  assert.strictEqual(u.protocol, "http:", "protocol");
  assert.strictEqual(u.hostname, "host", "hostname");
  assert.strictEqual(u.port, null, "absent port is null");
  assert.strictEqual(u.pathname, "/path", "pathname");
});

test("format reverses parse for an absolute URL", () => {
  const input = "https://example.com:8080/a/b?q=1#frag";
  const formatted = url.format(url.parse(input));
  assert.strictEqual(formatted, input, "format(parse(x)) === x");
});

test("format accepts a components object", () => {
  const out = url.format({
    protocol: "https:",
    hostname: "host.example",
    port: "443",
    pathname: "/p",
    search: "?a=1",
    hash: "#h",
  });
  assert.strictEqual(out, "https://host.example:443/p?a=1#h", "assembled from components");
});

test("pathToFileURL then fileURLToPath round-trips a native path", () => {
  // Use a platform-native fixture so the conversion is deterministic on whichever OS runs the harness.
  const isWin = process.platform === "win32";
  const nativePath = isWin ? "C:\\Users\\foo\\bar.txt" : "/usr/local/bin/node";
  const fileUrl = url.pathToFileURL(nativePath);
  assert.strictEqual(fileUrl.slice(0, 5), "file:", "pathToFileURL produces a file: URL");
  const back = url.fileURLToPath(fileUrl);
  assert.strictEqual(back, nativePath, "fileURLToPath inverts pathToFileURL");
});

test("pathToFileURL percent-encodes spaces in the path", () => {
  const isWin = process.platform === "win32";
  const nativePath = isWin ? "C:\\a b\\c" : "/a/b c/d";
  const fileUrl = url.pathToFileURL(nativePath);
  assert.ok(fileUrl.indexOf("%20") >= 0, "space is percent-encoded as %20: " + fileUrl);
  // And it still round-trips back to the original native path.
  assert.strictEqual(url.fileURLToPath(fileUrl), nativePath, "encoded round-trip");
});

test("fileURLToPath rejects a non-file scheme", () => {
  assert.throws(() => url.fileURLToPath("https://example.com/x"), "non-file scheme must throw");
});
