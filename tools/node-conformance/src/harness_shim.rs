//! The in-test harness shim — the small `assert` + `test`/`describe` surface that the curated Node
//! conformance tests in `corpus/` rely on.
//!
//! # Why the shim is embedded, not injected
//!
//! The runner ([`crate::run_source`]) evaluates each corpus file through
//! [`treaty_runtime::JsRuntime::with_node_compat`], which wraps the body in an *indirect* `eval`
//! (see `wrap_source` in `libs/runtime/src/lib.rs`). That wrapper has two consequences the shim must
//! work within:
//!
//! 1. **No prelude injection seam.** The runner hands the file's bytes straight to `eval`; there is
//!    no hook to prepend a shared module. So the shim cannot live in a separate file the runner
//!    splices in — each test must carry it.
//! 2. **Lazy globals are unreachable.** Node's global niceties (`process`, `Buffer`, `setTimeout`,
//!    `TextEncoder`, `URL`, `console`, `queueMicrotask`, …) are installed as lazy, self-replacing
//!    accessors on the realm global. The indirect-`eval` scope never triggers those accessors, and
//!    they are not visible via `globalThis` either, so a test can only reach a builtin through
//!    `require(...)`. The shim is therefore written using **only** `require` + plain JS (no bare
//!    globals), and so are the curated tests.
//!
//! The chosen design: the shim is a single, compact, **byte-stable** JavaScript prelude
//! ([`ASSERT_SHIM`]). Every curated corpus file begins with exactly this block (delimited by
//! [`SHIM_BEGIN`] / [`SHIM_END`] marker comments). Keeping it byte-identical everywhere — rather
//! than each test hand-rolling its own `assert` — is what makes the corpus a faithful Node
//! `test/parallel`-style suite: tests read like Node's own, using `assert.strictEqual`,
//! `assert.deepStrictEqual`, `assert.throws`, `assert.rejects`, and a tiny `test()` / `describe()`
//! runner. The crate test [`every_corpus_file_embeds_the_canonical_shim`] enforces that the embedded
//! copies never drift from the canonical source here.
//!
//! # The shim surface
//!
//! Installed on a `const assert = makeAssert()` (and `test` / `describe`), implemented on top of the
//! genuinely-reachable builtins (`node:util` for deep equality, `node:assert` is *not* relied upon so
//! the shim is self-contained):
//!
//! - `assert(value[, message])` — throws unless `value` is truthy.
//! - `assert.ok` — alias of `assert`.
//! - `assert.strictEqual(actual, expected[, message])` — `Object.is` equality.
//! - `assert.notStrictEqual(actual, expected[, message])` — its negation.
//! - `assert.deepStrictEqual(actual, expected[, message])` — structural equality via
//!   `require('node:util').isDeepStrictEqual`.
//! - `assert.throws(fn[, message])` — asserts `fn()` throws.
//! - `assert.rejects(promiseOrFn[, message])` — asserts the promise (or `fn()`'s promise) rejects;
//!   returns a promise that the event-loop drain settles, so an `await assert.rejects(...)` inside an
//!   async test is observed.
//! - `test(name, fn)` / `it(name, fn)` — run `fn` immediately; a throw (sync) or rejection (async,
//!   because the runtime drains the returned promise) fails the test, surfacing as a runner `Fail`.
//! - `describe(name, fn)` — run `fn` immediately as a grouping; purely organizational.
//!
//! `test`/`describe` run their bodies eagerly rather than registering into a queue, because the
//! runner has no "now run the suite" callback — the file *is* the suite, and a clean evaluation
//! (plus a clean event-loop drain) is the pass signal.

/// Opening marker comment that delimits the embedded shim inside a corpus file.
pub const SHIM_BEGIN: &str = "// === treaty node-conformance harness shim (begin) ===";

/// Closing marker comment that delimits the embedded shim inside a corpus file.
pub const SHIM_END: &str = "// === treaty node-conformance harness shim (end) ===";

/// The canonical harness prelude every curated corpus file embeds verbatim.
///
/// It is written against `require` + plain JS only (no bare globals — see the module docs for why),
/// and depends on no unimplemented API: deep equality routes through `node:util`'s
/// `isDeepStrictEqual`, which the runtime implements. The block is bracketed by [`SHIM_BEGIN`] /
/// [`SHIM_END`] so the corpus-integrity test can locate and compare it.
pub const ASSERT_SHIM: &str = r#"// === treaty node-conformance harness shim (begin) ===
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
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    /// The bundled corpus directory.
    fn corpus_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus")
    }

    /// Collect every `.js` corpus file path, sorted for determinism.
    fn corpus_js_files() -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = fs::read_dir(corpus_dir())
            .expect("corpus directory must be readable")
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "js"))
            .collect();
        files.sort();
        files
    }

    #[test]
    fn shim_is_delimited_by_its_markers() {
        assert!(
            ASSERT_SHIM.contains(SHIM_BEGIN),
            "the shim must contain its begin marker"
        );
        assert!(
            ASSERT_SHIM.contains(SHIM_END),
            "the shim must contain its end marker"
        );
        let begin = ASSERT_SHIM.find(SHIM_BEGIN).unwrap();
        let end = ASSERT_SHIM.find(SHIM_END).unwrap();
        assert!(begin < end, "begin marker must precede end marker");
    }

    #[test]
    fn shim_uses_only_require_no_bare_globals() {
        // The shim must not reference a bare lazy global (unreachable in the harness eval scope).
        for forbidden in ["globalThis.", "\nprocess", "\nBuffer", "setTimeout", "TextEncoder"] {
            assert!(
                !ASSERT_SHIM.contains(forbidden),
                "shim must not reference unreachable global token: {forbidden:?}"
            );
        }
        assert!(
            ASSERT_SHIM.contains("require(\"node:util\")"),
            "shim must reach util via require"
        );
    }

    #[test]
    fn every_corpus_file_embeds_the_canonical_shim() {
        // A skipped test may reference unimplemented APIs and need no shim; an executed (non-skip)
        // test must embed the byte-identical canonical shim so the suite never drifts.
        for path in corpus_js_files() {
            let source = fs::read_to_string(&path).expect("read corpus file");
            if source.contains(crate::SKIP_MARKER) {
                continue;
            }
            assert!(
                source.contains(ASSERT_SHIM.trim_end()),
                "executed corpus file {} must embed the canonical harness shim verbatim",
                path.display()
            );
        }
    }
}
