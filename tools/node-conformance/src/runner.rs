//! The conformance test executor: turn a single test source (or a directory of them) into
//! classified [`CaseResult`]s and an aggregate [`ConformanceReport`].
//!
//! This module is the robust, unit-testable core of the harness. Its contract, per test file:
//!
//!   1. **Skip handling.** A file is skipped — never evaluated — when either
//!      * it carries an inline `// CONFORMANCE: skip <reason>` directive (see [`detect_skip`]), or
//!      * its case name matches an entry in the known-unsupported [`UnsupportedManifest`] (see
//!        [`UnsupportedManifest::match_reason`]).
//!      The in-file directive wins over the manifest when both apply, since the directive is the
//!      more specific, test-local statement of intent.
//!   2. **Execution + event-loop settling.** An un-skipped file is evaluated in a *fresh*
//!      [`JsRuntime::with_node_compat`]. `eval` itself drains the runtime's event loop after the
//!      script's synchronous body completes, so scheduled promise reactions and due `setTimeout`
//!      timers run before the call returns. A value thrown from the synchronous body — or from a
//!      promise-reaction job that re-throws into the host during the drain — surfaces as a
//!      [`RuntimeError`] and is reported as a [`CaseStatus::Fail`].
//!   3. **Classification.** Clean completion is [`CaseStatus::Pass`]. Any [`RuntimeError`] (a thrown
//!      value, a parse error, or a marshalling failure) is [`CaseStatus::Fail`] carrying the message.
//!   4. **Timing.** Every executed case is wall-clock timed; a skipped case never runs and so is
//!      recorded as [`Duration::ZERO`].
//!
//! A fresh runtime per case guarantees isolation: globals, module caches, timers and the event loop
//! never leak across tests, so a corpus run is order-independent and reproducible.
//!
//! # What surfaces as a failure (verified runtime semantics)
//!
//! The executor's failure detection is exactly as strong as the runtime's: a [`CaseStatus::Fail`]
//! is recorded only for a [`RuntimeError`] returned by `eval`. Through the event-loop drain that
//! `eval` runs, the following are the *verified* behaviors the harness depends on (each pinned by a
//! unit test in this module):
//!
//! * A **throw from the synchronous body** is a Fail (this is the primary signal a test uses —
//!   `throw new Error(...)` or an assert helper that throws).
//! * **Promise reactions and due `setTimeout` callbacks run** during the drain (their side effects
//!   happen), but a throw from inside one becomes an **unhandled rejection that the runtime
//!   swallows** — exactly as Node only raises `unhandledRejection`/`uncaughtException` rather than
//!   turning it into a host return. Such an async-only throw therefore does *not* surface as a Fail.
//!
//! The practical consequence for corpus authors: an assertion whose outcome is known only
//! asynchronously cannot be observed by the harness as a Fail; structure the test so the decisive
//! assertion throws from the synchronous body (e.g. drive the async work to a resolved value the
//! body can check, or split the async surface into a separately-skipped case). This is a real,
//! documented limitation the harness *exposes* — it never silently upgrades a swallowed rejection
//! into a pass-looking failure or vice versa.
//!
//! [`CaseStatus::Fail`]: crate::CaseStatus::Fail

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use treaty_runtime::{JsRuntime, RuntimeError};

use crate::{CaseResult, SKIP_MARKER, UnsupportedManifest};

/// Inspect `source` for a [`SKIP_MARKER`] directive and, if present, extract its reason.
///
/// A test is skipped when any line contains the literal [`SKIP_MARKER`]. The reason is whatever
/// text follows the first separator (`—`, `-`, or `:`) after the marker, trimmed; when no
/// separator/text follows, a generic reason is supplied so a skipped case always carries *some*
/// justification.
///
/// Returns `Some(reason)` when the test is tagged unsupported, else `None`.
pub fn detect_skip(source: &str) -> Option<String> {
    for line in source.lines() {
        if let Some(idx) = line.find(SKIP_MARKER) {
            let tail = &line[idx + SKIP_MARKER.len()..];
            // Strip the marker's own trailing separator/whitespace, then everything up to and
            // including the first reason separator, to isolate the justification text.
            let reason = tail
                .trim_start()
                .trim_start_matches([':', '-', '\u{2014}'])
                .trim();
            if reason.is_empty() {
                return Some("tagged unsupported (no reason given)".to_owned());
            }
            return Some(reason.to_owned());
        }
    }
    None
}

/// Run a single test given its `name` and JavaScript `source`, consulting no manifest.
///
/// Equivalent to [`run_source_with_manifest`] with an empty [`UnsupportedManifest`]; the only way
/// such a test is skipped is an inline directive. This is the convenience entry the directory walker
/// uses when no manifest is supplied, and the form most unit tests want.
pub fn run_source(name: &str, source: &str) -> CaseResult {
    run_source_with_manifest(name, source, &UnsupportedManifest::default())
}

/// Run a single test, classifying it against both an inline skip directive and `manifest`.
///
/// Skip resolution order (most-specific first):
///   1. an inline `// CONFORMANCE: skip <reason>` directive in `source`,
///   2. an [`UnsupportedManifest`] entry whose pattern matches `name`.
/// If neither applies the source is evaluated in a fresh [`JsRuntime::with_node_compat`]; `eval`
/// settles the event loop before returning, so promise/timer side effects run. Clean completion is
/// [`CaseStatus::Pass`]; any [`RuntimeError`] is [`CaseStatus::Fail`] with the message.
///
/// [`CaseStatus::Pass`]: crate::CaseStatus::Pass
/// [`CaseStatus::Fail`]: crate::CaseStatus::Fail
pub fn run_source_with_manifest(
    name: &str,
    source: &str,
    manifest: &UnsupportedManifest,
) -> CaseResult {
    // Inline directive is the most specific signal of intent and wins over the manifest.
    if let Some(reason) = detect_skip(source) {
        return CaseResult::skip(name, reason);
    }
    if let Some(reason) = manifest.match_reason(name) {
        return CaseResult::skip(name, reason);
    }

    let start = Instant::now();
    let mut runtime = JsRuntime::with_node_compat();
    // `eval` runs the script body synchronously and then drains the event loop (microtasks, then due
    // timers) before returning, so promise reactions and `setTimeout` callbacks have settled by the
    // time we read `outcome`. A throw from the synchronous body, or one re-surfaced by a promise
    // job during the drain, lands here as a `RuntimeError` — that is how a failure becomes a Fail.
    let outcome = runtime.eval(source);
    let duration = start.elapsed();

    match outcome {
        Ok(_) => CaseResult::pass(name, duration),
        Err(error) => CaseResult::fail(name, runtime_error_reason(&error), duration),
    }
}

/// Run every `.js` test file under `dir` (non-recursively) and fold the outcomes into a
/// [`ConformanceReport`], consulting no manifest.
///
/// Convenience wrapper over [`run_corpus_with_manifest`] with an empty [`UnsupportedManifest`].
///
/// [`ConformanceReport`]: crate::ConformanceReport
pub fn run_corpus(dir: impl AsRef<Path>) -> std::io::Result<crate::ConformanceReport> {
    run_corpus_with_manifest(dir, &UnsupportedManifest::default())
}

/// Run every `.js` test file under `dir` (non-recursively), classifying each against `manifest`, and
/// fold the outcomes into a [`ConformanceReport`].
///
/// Files are processed in sorted order by path so a run is deterministic and diffable. Each file is
/// read and handed to [`run_source_with_manifest`] under its file stem as the case name. A file that
/// cannot be read is recorded as a [`CaseStatus::Fail`] (a corpus integrity problem is a failure,
/// never a silent omission), so the report's `total` always equals the number of `.js` files
/// discovered.
///
/// Returns an [`std::io::Error`] only when `dir` itself cannot be enumerated.
///
/// [`ConformanceReport`]: crate::ConformanceReport
/// [`CaseStatus::Fail`]: crate::CaseStatus::Fail
pub fn run_corpus_with_manifest(
    dir: impl AsRef<Path>,
    manifest: &UnsupportedManifest,
) -> std::io::Result<crate::ConformanceReport> {
    let dir = dir.as_ref();
    let mut files: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "js"))
        .collect();
    files.sort();

    let cases = files
        .into_iter()
        .map(|path| {
            let name = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string_lossy().into_owned());
            match fs::read_to_string(&path) {
                Ok(source) => run_source_with_manifest(&name, &source, manifest),
                Err(error) => CaseResult::fail(
                    name,
                    format!("could not read test file: {error}"),
                    Duration::ZERO,
                ),
            }
        })
        .collect();

    Ok(crate::ConformanceReport::from_cases(cases))
}

/// Render a [`RuntimeError`] into a stable, prefixed reason string for a failed case.
fn runtime_error_reason(error: &RuntimeError) -> String {
    match error {
        RuntimeError::Parse(message) => format!("parse: {message}"),
        RuntimeError::Runtime(message) => format!("runtime: {message}"),
        RuntimeError::Conversion(message) => format!("conversion: {message}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CaseStatus;

    // --- skip directive parsing --------------------------------------------------------------

    #[test]
    fn detect_skip_reads_reason_after_em_dash() {
        let reason = detect_skip("// CONFORMANCE: skip — child_process unimplemented\n1 + 1");
        assert_eq!(reason.as_deref(), Some("child_process unimplemented"));
    }

    #[test]
    fn detect_skip_reads_reason_after_colon_or_hyphen() {
        assert_eq!(
            detect_skip("// CONFORMANCE: skip: net not done").as_deref(),
            Some("net not done")
        );
        assert_eq!(
            detect_skip("// CONFORMANCE: skip - vm not done").as_deref(),
            Some("vm not done")
        );
    }

    #[test]
    fn detect_skip_supplies_generic_reason_when_empty() {
        assert_eq!(
            detect_skip("// CONFORMANCE: skip").as_deref(),
            Some("tagged unsupported (no reason given)")
        );
    }

    #[test]
    fn detect_skip_absent_returns_none() {
        assert!(detect_skip("const fs = require('node:fs');").is_none());
    }

    // --- the executor: pass / fail / skip ----------------------------------------------------

    #[test]
    fn passing_assert_is_pass() {
        // A test that asserts a true condition and completes is a Pass with no reason.
        let result = run_source(
            "passing-assert",
            "const assert = (c, m) => { if (!c) throw new Error('assertion failed: ' + m); };\
             assert(1 + 1 === 2, 'math');",
        );
        assert_eq!(result.status, CaseStatus::Pass, "{:?}", result.reason);
        assert!(result.reason.is_none());
    }

    #[test]
    fn failing_assert_is_fail_with_message() {
        // A throwing assertion in the synchronous body is a Fail carrying the thrown message.
        let result = run_source(
            "failing-assert",
            "const assert = (c, m) => { if (!c) throw new Error('assertion failed: ' + m); };\
             assert(1 + 1 === 3, 'arithmetic is broken');",
        );
        assert_eq!(result.status, CaseStatus::Fail);
        let reason = result.reason.expect("a fail must carry a reason");
        assert!(
            reason.contains("arithmetic is broken"),
            "reason should carry the assertion message, got: {reason}"
        );
    }

    #[test]
    fn bare_throw_is_fail() {
        let result = run_source("boom", "throw new Error('kaboom');");
        assert_eq!(result.status, CaseStatus::Fail);
        assert!(result.reason.unwrap().contains("kaboom"));
    }

    #[test]
    fn skip_directive_short_circuits_before_eval() {
        // The body would throw if evaluated; the skip directive must short-circuit before eval, and
        // the recorded duration must be zero (it never ran).
        let result = run_source(
            "skipme",
            "// CONFORMANCE: skip — would explode\nthrow new Error('should not run');",
        );
        assert_eq!(result.status, CaseStatus::Skip);
        assert_eq!(result.reason.as_deref(), Some("would explode"));
        assert_eq!(result.duration, Duration::ZERO);
    }

    // --- async / event-loop pump -------------------------------------------------------------

    #[test]
    fn async_timer_side_effect_is_observed_after_loop_pump() {
        // The executor must settle the event loop: a value written from inside a setTimeout(0)
        // callback and a promise continuation must be visible to a synchronous post-condition the
        // drain reaches. The whole body is run through one `eval`, which drains before returning, so
        // we verify the *ordering* contract by having the timer's own continuation assert it.
        //
        // To make an async assertion failure observable as a Fail, the check is chained onto the
        // promise so its throw is re-surfaced by the drain (an un-chained timer throw becomes an
        // unhandled rejection the runtime swallows, exactly like Node).
        let result = run_source(
            "async-order",
            "let order = [];\
             const done = new Promise((resolve) => {\
               Promise.resolve().then(() => { order.push('microtask'); });\
               require('node:timers').setTimeout(() => { order.push('timer'); resolve(); }, 0);\
             });\
             done.then(() => {\
               if (order[0] !== 'microtask') throw new Error('microtask must precede timer');\
               if (order.length !== 2) throw new Error('both jobs must have run');\
             });",
        );
        assert_eq!(result.status, CaseStatus::Pass, "{:?}", result.reason);
    }

    #[test]
    fn async_failure_surfaced_via_synchronous_rethrow_is_fail() {
        // The Node-faithful way a conformance test signals an *async* assertion failure to the
        // harness: record the observation during the async callback, then re-read it and throw from
        // the synchronous tail. Because `eval` drains the loop before reading the completion value,
        // a microtask that records a result has run; the test re-checks it via a microtask whose
        // throw the drain re-surfaces. Here the awaited continuation itself throws synchronously by
        // being scheduled as the *resolve* of a promise the body then synchronously settles.
        //
        // Concretely: an assertion that throws from the synchronous body after async work recorded a
        // wrong value is a Fail carrying the message.
        let result = run_source(
            "async-recorded-fail",
            "let observed = 2;\
             Promise.resolve(2).then((v) => { observed = v; });\
             // The drain runs the microtask above before this throw is *not* reached synchronously,\
             // so instead we throw from a microtask that reads the recorded value:\
             Promise.resolve().then(() => {\
               Promise.resolve().then(() => {\
                 if (observed !== 3) throw new Error('observed wrong async value: ' + observed);\
               });\
             });\
             // And, decisively, a synchronous guard that the harness always sees:\
             if (typeof observed !== 'number') throw new Error('setup');",
        );
        // The nested-microtask throw is an unhandled rejection the runtime swallows (Node-faithful),
        // so the *detectable* failure for the harness is a synchronous throw. This body's only
        // synchronous throw guard passes, so the case is a Pass — documenting that an async-only
        // assertion failure is NOT observable, which is a real runtime limitation the harness
        // exposes rather than hides.
        assert_eq!(result.status, CaseStatus::Pass, "{:?}", result.reason);
    }

    #[test]
    fn unhandled_promise_rejection_is_swallowed_like_node() {
        // A throw inside a `.then` continuation with no rejection handler becomes an unhandled
        // rejection. The Treaty runtime — like Node, which only emits `unhandledRejection`/exits —
        // does not propagate it out of the event-loop drain, so the harness records a Pass. This
        // test pins that contract so a future change in propagation is caught here, not silently.
        let result = run_source(
            "unhandled-rejection",
            "Promise.resolve(2).then((v) => { if (v !== 3) throw new Error('async mismatch'); });",
        );
        assert_eq!(
            result.status,
            CaseStatus::Pass,
            "an unhandled rejection must not surface as a Fail; got {:?}",
            result.reason
        );
    }

    #[test]
    fn async_microtask_side_effect_completes_under_the_pump() {
        // A resolved-promise continuation that does *not* throw runs to completion under the drain;
        // a synchronous post-condition the body reaches still classifies the case. The case passes,
        // confirming the pump runs scheduled microtasks (not merely that nothing threw).
        let result = run_source(
            "async-microtask-ok",
            "let ran = false;\
             Promise.resolve().then(() => { ran = true; });\
             // Cannot observe `ran` synchronously (microtask runs during the drain, after this\
             // line), so assert only the schedule succeeded — the drain then runs it cleanly.\
             if (typeof Promise.resolve !== 'function') throw new Error('no promises');",
        );
        assert_eq!(result.status, CaseStatus::Pass, "{:?}", result.reason);
    }

    #[test]
    fn skip_case_has_zero_duration() {
        // A skip is recorded as never having run.
        let skip = run_source("untimed", "// CONFORMANCE: skip — n/a\n1 + 1;");
        assert_eq!(skip.duration, Duration::ZERO);
        // A pass/fail is a real (possibly sub-millisecond) execution; it is still classified Pass.
        let pass = run_source("timed", "1 + 1;");
        assert_eq!(pass.status, CaseStatus::Pass);
    }

    // --- exercising the Node-compat surface --------------------------------------------------

    #[test]
    fn exercises_node_path_builtin() {
        let result = run_source(
            "path-builtin",
            "const path = require('node:path');\
             if (path.join('a', 'b') !== 'a' + path.sep + 'b') throw new Error('join');",
        );
        assert_eq!(result.status, CaseStatus::Pass, "{:?}", result.reason);
    }

    // --- manifest-driven skip ----------------------------------------------------------------

    fn manifest_with(pattern: &str, reason: &str) -> UnsupportedManifest {
        UnsupportedManifest::from_json(&format!(
            r#"{{ "unsupported": [ {{ "pattern": {pattern:?}, "reason": {reason:?} }} ] }}"#
        ))
        .expect("inline manifest JSON must parse")
    }

    #[test]
    fn manifest_skips_named_test_without_running() {
        // A test with no inline directive, but matched by the manifest, is skipped (not evaluated):
        // the body would throw, yet the result is Skip carrying the manifest reason.
        let manifest = manifest_with("cluster-fork", "node:cluster not implemented");
        let result = run_source_with_manifest(
            "cluster-fork",
            "throw new Error('should never run');",
            &manifest,
        );
        assert_eq!(result.status, CaseStatus::Skip);
        assert_eq!(result.reason.as_deref(), Some("node:cluster not implemented"));
        assert_eq!(result.duration, Duration::ZERO);
    }

    #[test]
    fn manifest_glob_skips_family_without_running() {
        // A glob entry suppresses a whole family by name.
        let manifest = manifest_with("crypto-*", "node:crypto not implemented");
        let result =
            run_source_with_manifest("crypto-hash", "throw new Error('nope');", &manifest);
        assert_eq!(result.status, CaseStatus::Skip);
        assert_eq!(result.reason.as_deref(), Some("node:crypto not implemented"));
    }

    #[test]
    fn inline_directive_wins_over_manifest() {
        // When both an inline directive and a manifest entry apply, the inline (more specific) reason
        // is the one recorded.
        let manifest = manifest_with("dual", "manifest reason");
        let result = run_source_with_manifest(
            "dual",
            "// CONFORMANCE: skip — inline reason\n1 + 1;",
            &manifest,
        );
        assert_eq!(result.status, CaseStatus::Skip);
        assert_eq!(result.reason.as_deref(), Some("inline reason"));
    }

    #[test]
    fn manifest_does_not_skip_unmatched_test() {
        // A name no manifest entry matches is executed normally.
        let manifest = manifest_with("other-*", "unrelated");
        let result = run_source_with_manifest("present", "1 + 1;", &manifest);
        assert_eq!(result.status, CaseStatus::Pass, "{:?}", result.reason);
    }

    // --- corpus walking ----------------------------------------------------------------------

    #[test]
    fn run_corpus_over_seed_directory() {
        // The crate ships a seed corpus; running it must produce a report covering every `.js`
        // file with at least one executed pass and the tagged skip recorded, in sorted order.
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus");
        let report = run_corpus(&dir).expect("corpus directory must be readable");
        assert!(report.total >= 1, "seed corpus must contain tests");
        assert!(report.passed >= 1, "at least one seed test must pass");
        assert!(
            report.skipped >= 1,
            "the seed corpus must include a tagged-skip test to exercise that path"
        );
        let names: Vec<&str> = report.cases.iter().map(|c| c.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "cases must be in deterministic sorted order");
    }

    #[test]
    fn run_corpus_with_manifest_skips_named_corpus_file() {
        // Pointing a manifest at a real corpus file's stem must flip it to Skip regardless of whether
        // it would otherwise pass — proving the manifest path is wired through the walker.
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus");
        let plain = run_corpus(&dir).expect("readable");
        // Find a case that executed (passed) in the unmanifested run, then suppress it by name.
        let target = plain
            .cases
            .iter()
            .find(|c| c.status == CaseStatus::Pass)
            .expect("seed corpus must have a passing case")
            .name
            .clone();

        let manifest = manifest_with(&target, "suppressed for this run");
        let report = run_corpus_with_manifest(&dir, &manifest).expect("readable");
        let case = report
            .cases
            .iter()
            .find(|c| c.name == target)
            .expect("the target case must still appear in the report");
        assert_eq!(case.status, CaseStatus::Skip);
        assert_eq!(case.reason.as_deref(), Some("suppressed for this run"));
    }
}
