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

// node:child_process is now implemented: spawnSync / execSync run a child to completion over
// std::process and capture its output. This is the promotion of the former child-process-skip case
// to a live test. A portable `echo` (a builtin of both cmd.exe and /bin/sh) is driven through the
// platform shell so one command line works cross-platform. All synchronous: assertions throw from
// the body.
const cp = require("node:child_process");
const process = require("node:process");
assert.ok(typeof cp.spawnSync === "function", "spawnSync export");
assert.ok(typeof cp.execSync === "function", "execSync export");
assert.ok(typeof cp.exec === "function", "exec export");

test("execSync captures stdout of an echo as a utf8 string", () => {
  const out = cp.execSync("echo treaty-conformance", { encoding: "utf8" });
  assert.ok(out.indexOf("treaty-conformance") >= 0, "execSync stdout: " + out);
});

test("spawnSync runs a shelled command and reports status + stdout + pid", () => {
  const r = cp.spawnSync("echo spawned-ok", [], { shell: true, encoding: "utf8" });
  assert.strictEqual(r.status, 0, "exit status 0");
  assert.ok(r.stdout.indexOf("spawned-ok") >= 0, "spawnSync stdout: " + r.stdout);
  assert.strictEqual(typeof r.pid, "number", "a numeric pid is recorded");
});

test("spawnSync surfaces a non-zero exit status", () => {
  const r = cp.spawnSync("exit 7", [], { shell: true, encoding: "utf8" });
  assert.strictEqual(r.status, 7, "non-zero exit code round-trips");
});

test("execSync throws on a non-zero exit, carrying the status on the error", () => {
  let caught = null;
  try {
    cp.execSync("exit 5");
  } catch (e) {
    caught = e;
  }
  assert.ok(caught !== null, "execSync must throw on a failing command");
  assert.strictEqual(caught.status, 5, "thrown error carries the child status");
});

test("spawnSync feeds stdin through to a child that echoes it back", () => {
  // `sort` (cmd) / `cat` (sh) read stdin and write it back; a single token is order-stable.
  const isWin = process.platform === "win32";
  const r = isWin
    ? cp.spawnSync("sort", [], { input: "piped-line", encoding: "utf8" })
    : cp.spawnSync("/bin/cat", [], { input: "piped-line", encoding: "utf8" });
  assert.strictEqual(r.status, 0, "stdin pass-through exits 0");
  assert.ok(r.stdout.indexOf("piped-line") >= 0, "stdin was echoed to stdout: " + r.stdout);
});
