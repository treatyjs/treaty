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
function test(name, fn) {
  const result = fn();
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

// node:tls + node:https loopback: a real TLS handshake on 127.0.0.1:0, no external network.
// An https.createServer (using the runtime's embedded self-signed loopback cert) is hit by https.get
// with that exact cert pinned via the `ca` option, so the client performs genuine webpki chain + SAN
// (`localhost`) verification before the encrypted HTTP/1.1 exchange. The decisive assertions run in
// the response 'end' callback and resolve the promise the harness awaits.
const tls = require("node:tls");
const https = require("node:https");

assert.ok(typeof tls.connect === "function", "tls.connect export");
assert.ok(typeof tls.createServer === "function", "tls.createServer export");
assert.ok(typeof https.createServer === "function", "https.createServer export");
assert.ok(typeof https.request === "function", "https.request export");
assert.ok(typeof https.get === "function", "https.get export");

test("https.get over a real TLS session returns the handler body and status", () => {
  const ca = tls.testCert().cert; // the embedded cert the default server presents
  return new Promise(function (resolve, reject) {
    const server = https.createServer(function (req, res) {
      res.writeHead(200, { "content-type": "text/plain" });
      res.end("secure " + req.url);
    });
    server.listen(0, function () {
      const port = server.address().port;
      assert.ok(port > 0, "ephemeral loopback port assigned");
      https
        .get(
          { host: "localhost", port: port, path: "/world", ca: ca, rejectUnauthorized: true },
          function (res) {
            let body = "";
            res.on("data", function (d) { body += d; });
            res.on("end", function () {
              server.close();
              try {
                assert.strictEqual(res.statusCode, 200, "status code");
                assert.strictEqual(body, "secure /world", "handler-produced body over TLS");
                resolve();
              } catch (e) { reject(e); }
            });
          }
        )
        .on("error", function (e) { server.close(); reject(e); });
    });
  });
});

test("https.get rejects an unpinned self-signed cert (verification is real)", () => {
  return new Promise(function (resolve, reject) {
    const server = https.createServer(function (req, res) { res.end("nope"); });
    server.listen(0, function () {
      const port = server.address().port;
      // No `ca`, verification on (default) => empty system roots => must reject, not silently trust.
      https
        .get({ host: "localhost", port: port, path: "/x" }, function (res) {
          server.close();
          reject(new Error("expected verification failure, got status " + res.statusCode));
        })
        .on("error", function () { server.close(); resolve(); });
    });
  });
});
