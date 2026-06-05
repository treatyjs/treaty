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

// Node test/parallel-style: node:fs/promises async API. The harness drains the event loop after
// evaluation, so an async test()'s awaited operations settle and any rejection surfaces as a Fail.
const fsp = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const process = require("node:process");

const dir = path.join(os.tmpdir(), "treaty-fsp-" + process.pid + "-" + Date.now());

test("writeFile/readFile/readdir/unlink round-trip via promises", async () => {
  await fsp.mkdir(dir, { recursive: true });
  const file = path.join(dir, "data.txt");

  await fsp.writeFile(file, "async-payload");
  const back = await fsp.readFile(file, "utf8");
  assert.strictEqual(back, "async-payload", "readFile returns what writeFile wrote");

  const entries = await fsp.readdir(dir);
  assert.strictEqual(entries.includes("data.txt"), true, "readdir lists the written file");

  await fsp.appendFile(file, "-more");
  assert.strictEqual(await fsp.readFile(file, "utf8"), "async-payload-more", "appendFile appends");

  const copy = path.join(dir, "copy.txt");
  await fsp.copyFile(file, copy);
  assert.strictEqual(await fsp.readFile(copy, "utf8"), "async-payload-more", "copyFile duplicates");

  await fsp.unlink(file);
  await fsp.unlink(copy);
});

test("readFile of a missing path rejects", async () => {
  await assert.rejects(
    fsp.readFile(path.join(dir, "does-not-exist.txt"), "utf8"),
    "reading a missing file must reject"
  );
});
