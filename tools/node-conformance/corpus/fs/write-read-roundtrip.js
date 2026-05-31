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

// Node test/parallel-style: node:fs synchronous write / read / exists / unlink round-trips, mirroring
// Node's test-fs-write-file-sync / test-fs-read-file-sync. Each case uses a unique temp directory
// created with mkdtempSync under os.tmpdir() and removes its tree with the recursive rmSync, so cases
// neither collide nor leak files.
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

function tmp(prefix) {
  return fs.mkdtempSync(path.join(os.tmpdir(), prefix));
}

test("writeFileSync then readFileSync('utf8') returns exactly what was written", () => {
  const dir = tmp("treaty-fs-rw-");
  const file = path.join(dir, "data.txt");

  assert.strictEqual(fs.existsSync(file), false, "file must not exist before the write");
  fs.writeFileSync(file, "forty-two");
  assert.strictEqual(fs.existsSync(file), true, "file exists after writeFileSync");
  assert.strictEqual(fs.readFileSync(file, "utf8"), "forty-two", "read returns the written bytes");

  fs.rmSync(dir, { recursive: true });
});

test("writeFileSync overwrites (truncates) an existing file's contents", () => {
  const dir = tmp("treaty-fs-overwrite-");
  const file = path.join(dir, "x.txt");

  fs.writeFileSync(file, "longer original contents");
  fs.writeFileSync(file, "short");
  assert.strictEqual(
    fs.readFileSync(file, "utf8"),
    "short",
    "a second writeFileSync replaces, never appends"
  );

  fs.rmSync(dir, { recursive: true });
});

test("readFileSync with no encoding decodes the file as a utf8 string", () => {
  // The Treaty runtime's encoding-less readFileSync returns the file decoded as a utf8 string (it
  // does not return a Buffer object); a multi-byte payload must round-trip exactly through that path.
  const dir = tmp("treaty-fs-default-");
  const file = path.join(dir, "u.txt");

  fs.writeFileSync(file, "héllo wörld");
  assert.strictEqual(
    fs.readFileSync(file),
    "héllo wörld",
    "no-encoding read returns the decoded utf8 string"
  );
  assert.strictEqual(
    fs.readFileSync(file, "utf8"),
    fs.readFileSync(file),
    "explicit utf8 and the default agree"
  );

  fs.rmSync(dir, { recursive: true });
});

test("unlinkSync removes a file so existsSync flips to false", () => {
  const dir = tmp("treaty-fs-unlink-");
  const file = path.join(dir, "gone.txt");

  fs.writeFileSync(file, "transient");
  assert.strictEqual(fs.existsSync(file), true, "present after write");
  fs.unlinkSync(file);
  assert.strictEqual(fs.existsSync(file), false, "absent after unlink");

  fs.rmSync(dir, { recursive: true });
});

test("readFileSync on a missing path throws", () => {
  const dir = tmp("treaty-fs-missing-");
  assert.throws(
    () => fs.readFileSync(path.join(dir, "does-not-exist.txt"), "utf8"),
    "reading a nonexistent file must throw"
  );
  fs.rmSync(dir, { recursive: true });
});
