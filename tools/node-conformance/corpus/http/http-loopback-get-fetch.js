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

// node:http loopback: an http.createServer on 127.0.0.1:0 hit by http.get over the local reactor.
// No external network -- the listener binds an ephemeral loopback port. The decisive assertions run
// inside the request's end callback and resolve a promise the harness awaits, so a mismatch becomes
// a rejected test promise.
const http = require("node:http");
assert.ok(typeof http.createServer === "function", "createServer export");
assert.ok(typeof http.get === "function", "get export");
assert.ok(typeof http.request === "function", "request export");

test("http.get against a local createServer returns the handler body and status", () => {
  return new Promise(function (resolve, reject) {
    const server = http.createServer(function (req, res) {
      res.writeHead(200, { "content-type": "text/plain" });
      res.end("hello " + req.url);
    });
    server.listen(0, function () {
      const port = server.address().port;
      assert.ok(port > 0, "ephemeral loopback port assigned");
      http.get("http://127.0.0.1:" + port + "/world", function (res) {
        let body = "";
        res.on("data", function (d) { body += d; });
        res.on("end", function () {
          server.close();
          try {
            assert.strictEqual(res.statusCode, 200, "status code");
            assert.strictEqual(body, "hello /world", "handler-produced body");
            resolve();
          } catch (e) { reject(e); }
        });
      });
    });
  });
});

test("fetch against the local createServer resolves a real Response", () => {
  return new Promise(function (resolve, reject) {
    const server = http.createServer(function (req, res) {
      res.writeHead(201, { "content-type": "text/plain" });
      res.end("fetched:" + req.url);
    });
    server.listen(0, function () {
      const port = server.address().port;
      fetch("http://127.0.0.1:" + port + "/abc")
        .then(function (r) { return r.text().then(function (t) { return { r: r, t: t }; }); })
        .then(function (o) {
          server.close();
          assert.strictEqual(o.r.status, 201, "status from fetch");
          assert.strictEqual(o.t, "fetched:/abc", "body text from fetch");
          resolve();
        })
        .catch(function (e) { server.close(); reject(e); });
    });
  });
});
