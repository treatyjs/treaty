//! `node:assert` — the assertion API (`assert`/`ok`/`equal`/`deepEqual`/`strictEqual`/`throws`/…).
//!
//! ## Why this module is bootstrapped in JS
//!
//! Node's own `node:assert` is written in JavaScript (`lib/assert.js`), and for the same reason
//! `node:util` is: every entry point is pure value-level JS logic — strict/loose comparison, the
//! structural `deepEqual` walk, and the `throws`/`rejects` helpers that *invoke a passed function /
//! await a passed promise* and inspect the thrown value. Re-expressing callbacks-that-throw and
//! promise-awaiting against Nova's GC-handle FFI would mean threading scoped handles through user
//! closures — exactly the allocation-heavy, `unsafe`-adjacent boilerplate the architecture forbids.
//!
//! So `assert` materializes by evaluating ONE small self-contained bootstrap script (see
//! [`ASSERT_BOOTSTRAP`]) the first time `require("node:assert")` / `import "node:assert"` runs. The
//! script's completion value is the callable `assert` function with its static methods attached; the
//! shared registry caches that rooted object, so the parse+evaluate cost is paid at most once per
//! runtime and unused `assert` costs nothing (tenet 2). The bootstrap references only realm
//! intrinsics (`Error`, `Object`, `Array`, `Map`, `Set`, `Promise`, `Symbol`), so it allocates only
//! the closures it exports — no Rust-side heap, no `unsafe` (tenets 1 & 3).
//!
//! ## Faithfulness
//!
//! * The thrown error is a real `AssertionError` subclass of `Error` carrying `actual`, `expected`,
//!   `operator`, `generatedMessage`, and `code === "ERR_ASSERTION"`, matching Node.
//! * `ok`/`assert` (truthiness), `equal`/`notEqual` (loose `==`/`!=`), `strictEqual`/`notStrictEqual`
//!   (`Object.is`-based, so `NaN`/`±0` behave per Node), `deepEqual`/`notDeepEqual` (loose deep) and
//!   `deepStrictEqual`/`notDeepStrictEqual` (strict deep) are all provided.
//! * `throws`/`doesNotThrow` run the function and match the error against a constructor, RegExp,
//!   validation object, or predicate. `rejects`/`doesNotReject` are their async counterparts and
//!   return a promise. `fail`, `ifError`, and `match`/`doesNotMatch` round out the surface.

use nova_vm::ecmascript::{Agent, Object, Value, parse_script, script_evaluation};
use nova_vm::engine::Bindable;

use crate::node::core::{InstallError, NodeCtx};
use crate::node::{GcScope, NodeModule};

/// Zero-sized marker for the `node:assert` builtin.
pub(crate) struct AssertModule;

impl NodeModule for AssertModule {
    const SPECIFIER: &'static str = "assert";

    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: GcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError> {
        install(agent, ctx, gc)
    }
}

/// The self-contained `node:assert` implementation, evaluated once on first import.
///
/// An IIFE whose completion value is the callable `assert` function (with its static methods and the
/// `AssertionError` class attached, plus a `strict` self-reference). Intrinsic-only so it parses fast
/// and allocates only the exported closures.
const ASSERT_BOOTSTRAP: &str = r#"(function () {
  'use strict';

  function tag(v) { return Object.prototype.toString.call(v); }

  // --- AssertionError (a real Error subclass) ------------------------------------------------
  function AssertionError(options) {
    options = options || {};
    var message;
    if (options.message != null) {
      message = String(options.message);
      this.generatedMessage = false;
    } else {
      message = String(stringify(options.actual)) + ' ' + (options.operator || '==') + ' ' +
                String(stringify(options.expected));
      this.generatedMessage = true;
    }
    var err = Error.call(this, message);
    this.message = message;
    this.name = 'AssertionError';
    this.code = 'ERR_ASSERTION';
    this.actual = options.actual;
    this.expected = options.expected;
    this.operator = options.operator;
    if (Error.captureStackTrace) { Error.captureStackTrace(this, options.stackStartFn || fail); }
    else if (err && err.stack) { this.stack = err.stack; }
  }
  AssertionError.prototype = Object.create(Error.prototype);
  AssertionError.prototype.constructor = AssertionError;
  AssertionError.prototype.name = 'AssertionError';
  AssertionError.prototype.toString = function () {
    return this.name + ' [ERR_ASSERTION]: ' + this.message;
  };

  function stringify(v) {
    if (typeof v === 'string') return JSON.stringify(v);
    if (typeof v === 'bigint') return String(v) + 'n';
    if (typeof v === 'function') return '[Function' + (v.name ? ': ' + v.name : ' (anonymous)') + ']';
    if (v === undefined) return 'undefined';
    try { return JSON.stringify(v); } catch (e) { return String(v); }
  }

  function innerFail(opts) { throw new AssertionError(opts); }

  // --- deep equality (loose + strict) --------------------------------------------------------
  function isPrimitive(v) { return v === null || (typeof v !== 'object' && typeof v !== 'function'); }

  function deepEqual(a, b, strict, seen) {
    if (strict) {
      if (Object.is(a, b)) return true;
    } else {
      if (a === b) return true;
      // loose: NaN handled below; allow == on primitives
      if (isPrimitive(a) && isPrimitive(b)) { return a == b; }
    }
    if (typeof a === 'number' && typeof b === 'number' &&
        Number.isNaN(a) && Number.isNaN(b)) return true;
    if (isPrimitive(a) || isPrimitive(b)) return false;
    if (strict) {
      if (tag(a) !== tag(b)) return false;
      if (Object.getPrototypeOf(a) !== Object.getPrototypeOf(b)) return false;
    }
    if (tag(a) === '[object Date]' && tag(b) === '[object Date]') {
      return a.getTime() === b.getTime();
    }
    if (tag(a) === '[object RegExp]' && tag(b) === '[object RegExp]') {
      return a.source === b.source && a.flags === b.flags;
    }
    for (var s = 0; s < seen.length; s++) {
      if (seen[s][0] === a && seen[s][1] === b) return true;
    }
    seen.push([a, b]);
    var result;
    if (Array.isArray(a) && Array.isArray(b)) {
      if (a.length !== b.length) { result = false; }
      else {
        result = true;
        for (var i = 0; i < a.length; i++) {
          if (!deepEqual(a[i], b[i], strict, seen)) { result = false; break; }
        }
      }
    } else if (tag(a) === '[object Map]' && tag(b) === '[object Map]') {
      if (a.size !== b.size) { result = false; }
      else {
        result = true;
        var me = Array.from(a.entries());
        for (var m = 0; m < me.length; m++) {
          if (!b.has(me[m][0]) || !deepEqual(me[m][1], b.get(me[m][0]), strict, seen)) {
            result = false; break;
          }
        }
      }
    } else if (tag(a) === '[object Set]' && tag(b) === '[object Set]') {
      if (a.size !== b.size) { result = false; }
      else {
        result = true;
        var sv = Array.from(a);
        for (var n = 0; n < sv.length; n++) { if (!b.has(sv[n])) { result = false; break; } }
      }
    } else {
      var ka = Object.keys(a), kb = Object.keys(b);
      if (ka.length !== kb.length) { result = false; }
      else {
        result = true;
        for (var k = 0; k < ka.length; k++) {
          if (!Object.prototype.hasOwnProperty.call(b, ka[k]) ||
              !deepEqual(a[ka[k]], b[ka[k]], strict, seen)) { result = false; break; }
        }
      }
    }
    seen.pop();
    return result;
  }

  // --- error-matching for throws/rejects -----------------------------------------------------
  function matchError(actual, expected) {
    if (expected == null) return true;
    if (typeof expected === 'function') {
      // Constructor: instanceof check (Error subclasses, custom classes).
      if (expected.prototype !== undefined && (actual instanceof expected)) return true;
      // Otherwise treat as a validation predicate.
      if (expected.prototype === undefined || !(actual instanceof Error && expected === Error)) {
        try { return expected(actual) === true; } catch (e) { throw e; }
      }
      return actual instanceof expected;
    }
    if (tag(expected) === '[object RegExp]') {
      return expected.test(actual && actual.message != null ? String(actual.message) : String(actual));
    }
    if (typeof expected === 'object') {
      var keys = Object.keys(expected);
      for (var i = 0; i < keys.length; i++) {
        var k = keys[i];
        if (tag(expected[k]) === '[object RegExp]') {
          if (!expected[k].test(String(actual[k]))) return false;
        } else if (!deepEqual(actual[k], expected[k], true, [])) {
          return false;
        }
      }
      return true;
    }
    return false;
  }

  // --- public surface ------------------------------------------------------------------------
  function ok(value, message) {
    if (!value) {
      innerFail({ actual: value, expected: true, message: message, operator: '==',
                  stackStartFn: ok });
    }
  }

  function assert(value, message) { ok(value, message); }

  function equal(actual, expected, message) {
    if (actual != expected) {
      innerFail({ actual: actual, expected: expected, message: message, operator: '==',
                  stackStartFn: equal });
    }
  }
  function notEqual(actual, expected, message) {
    if (actual == expected) {
      innerFail({ actual: actual, expected: expected, message: message, operator: '!=',
                  stackStartFn: notEqual });
    }
  }
  function strictEqual(actual, expected, message) {
    if (!Object.is(actual, expected)) {
      innerFail({ actual: actual, expected: expected, message: message, operator: 'strictEqual',
                  stackStartFn: strictEqual });
    }
  }
  function notStrictEqual(actual, expected, message) {
    if (Object.is(actual, expected)) {
      innerFail({ actual: actual, expected: expected, message: message, operator: 'notStrictEqual',
                  stackStartFn: notStrictEqual });
    }
  }
  function deepEqualPublic(actual, expected, message) {
    if (!deepEqual(actual, expected, false, [])) {
      innerFail({ actual: actual, expected: expected, message: message, operator: 'deepEqual',
                  stackStartFn: deepEqualPublic });
    }
  }
  function notDeepEqual(actual, expected, message) {
    if (deepEqual(actual, expected, false, [])) {
      innerFail({ actual: actual, expected: expected, message: message, operator: 'notDeepEqual',
                  stackStartFn: notDeepEqual });
    }
  }
  function deepStrictEqual(actual, expected, message) {
    if (!deepEqual(actual, expected, true, [])) {
      innerFail({ actual: actual, expected: expected, message: message, operator: 'deepStrictEqual',
                  stackStartFn: deepStrictEqual });
    }
  }
  function notDeepStrictEqual(actual, expected, message) {
    if (deepEqual(actual, expected, true, [])) {
      innerFail({ actual: actual, expected: expected, message: message, operator: 'notDeepStrictEqual',
                  stackStartFn: notDeepStrictEqual });
    }
  }

  function fail(message) {
    if (message instanceof Error) throw message;
    innerFail({
      actual: undefined, expected: undefined,
      message: message != null ? message : 'Failed',
      operator: 'fail', stackStartFn: fail
    });
  }

  function ifError(value) {
    if (value !== null && value !== undefined) {
      var msg = 'ifError got unwanted exception: ' +
        (value instanceof Error ? (value.message || value) : stringify(value));
      innerFail({ actual: value, expected: null, message: msg, operator: 'ifError',
                  stackStartFn: ifError });
    }
  }

  function throws(fn, expected, message) {
    if (typeof fn !== 'function') {
      throw new TypeError('The "fn" argument must be of type function');
    }
    if (typeof expected === 'string' && message === undefined) { message = expected; expected = undefined; }
    var thrown = false, caught;
    try { fn(); } catch (e) { thrown = true; caught = e; }
    if (!thrown) {
      innerFail({ actual: undefined, expected: expected,
                  message: (message || 'Missing expected exception.'),
                  operator: 'throws', stackStartFn: throws });
    }
    if (expected !== undefined && !matchError(caught, expected)) {
      throw caught;
    }
    return true;
  }

  function doesNotThrow(fn, expected, message) {
    if (typeof fn !== 'function') {
      throw new TypeError('The "fn" argument must be of type function');
    }
    if (typeof expected === 'string' && message === undefined) { message = expected; expected = undefined; }
    var caught;
    try { fn(); return; } catch (e) { caught = e; }
    if (expected !== undefined && !matchError(caught, expected)) { throw caught; }
    innerFail({ actual: caught, expected: expected,
                message: 'Got unwanted exception.' + (message ? ' ' + message : ''),
                operator: 'doesNotThrow', stackStartFn: doesNotThrow });
  }

  function rejects(fnOrPromise, expected, message) {
    if (typeof expected === 'string' && message === undefined) { message = expected; expected = undefined; }
    var p;
    try {
      p = typeof fnOrPromise === 'function' ? fnOrPromise() : fnOrPromise;
    } catch (e) { return Promise.reject(e); }
    if (!p || typeof p.then !== 'function') {
      return Promise.reject(new TypeError(
        'The "promiseFn" argument must be of type function or an instance of Promise'));
    }
    return Promise.resolve(p).then(
      function () {
        innerFail({ actual: undefined, expected: expected,
                    message: (message || 'Missing expected rejection.'),
                    operator: 'rejects', stackStartFn: rejects });
      },
      function (err) {
        if (expected !== undefined && !matchError(err, expected)) { throw err; }
        return undefined;
      }
    );
  }

  function doesNotReject(fnOrPromise, expected, message) {
    if (typeof expected === 'string' && message === undefined) { message = expected; expected = undefined; }
    var p;
    try {
      p = typeof fnOrPromise === 'function' ? fnOrPromise() : fnOrPromise;
    } catch (e) { return Promise.reject(e); }
    if (!p || typeof p.then !== 'function') {
      return Promise.reject(new TypeError(
        'The "promiseFn" argument must be of type function or an instance of Promise'));
    }
    return Promise.resolve(p).then(
      function () { return undefined; },
      function (err) {
        if (expected !== undefined && !matchError(err, expected)) { throw err; }
        innerFail({ actual: err, expected: expected,
                    message: 'Got unwanted rejection.' + (message ? ' ' + message : ''),
                    operator: 'doesNotReject', stackStartFn: doesNotReject });
      }
    );
  }

  function match(value, regexp, message) {
    if (tag(regexp) !== '[object RegExp]') {
      throw new TypeError('The "regexp" argument must be an instance of RegExp');
    }
    if (!regexp.test(String(value))) {
      innerFail({ actual: value, expected: regexp, message: message, operator: 'match',
                  stackStartFn: match });
    }
  }
  function doesNotMatch(value, regexp, message) {
    if (tag(regexp) !== '[object RegExp]') {
      throw new TypeError('The "regexp" argument must be an instance of RegExp');
    }
    if (regexp.test(String(value))) {
      innerFail({ actual: value, expected: regexp, message: message, operator: 'doesNotMatch',
                  stackStartFn: doesNotMatch });
    }
  }

  // Attach the static surface to the callable `assert`.
  assert.AssertionError = AssertionError;
  assert.ok = ok;
  assert.equal = equal;
  assert.notEqual = notEqual;
  assert.strictEqual = strictEqual;
  assert.notStrictEqual = notStrictEqual;
  assert.deepEqual = deepEqualPublic;
  assert.notDeepEqual = notDeepEqual;
  assert.deepStrictEqual = deepStrictEqual;
  assert.notDeepStrictEqual = notDeepStrictEqual;
  assert.throws = throws;
  assert.doesNotThrow = doesNotThrow;
  assert.rejects = rejects;
  assert.doesNotReject = doesNotReject;
  assert.fail = fail;
  assert.ifError = ifError;
  assert.match = match;
  assert.doesNotMatch = doesNotMatch;

  // `assert.strict` is the all-strict variant: in Node it is `assert` with `equal`/`deepEqual`
  // aliased to their strict forms. Build a callable that mirrors that.
  function strict(value, message) { ok(value, message); }
  Object.keys(assert).forEach(function (k) { strict[k] = assert[k]; });
  strict.equal = strictEqual;
  strict.deepEqual = deepStrictEqual;
  strict.notEqual = notStrictEqual;
  strict.notDeepEqual = notDeepStrictEqual;
  strict.strict = strict;
  assert.strict = strict;

  return assert;
})()"#;

/// Uniform per-module entry. Materializes the `node:assert` exports (the callable `assert` with its
/// static methods) by evaluating [`ASSERT_BOOTSTRAP`] once against the current realm.
///
/// Lazy by construction: the shared registry only calls this on the first `require`/`import` of
/// `node:assert`, then caches the returned object, so an untouched `assert` costs nothing (tenet 2).
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    mut gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    let source = nova_vm::ecmascript::String::from_static_str(agent, ASSERT_BOOTSTRAP, gc.nogc());
    let realm = agent.current_realm(gc.nogc());

    let script = parse_script(agent, source, realm, true, None, gc.nogc()).map_err(|diagnostics| {
        let message = diagnostics
            .iter()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        InstallError::Nova(if message.is_empty() {
            "failed to parse node:assert bootstrap".to_owned()
        } else {
            message
        })
    })?;

    let outcome = script_evaluation(agent, script.unbind(), gc.reborrow());
    let value = match outcome {
        Ok(value) => value.unbind(),
        Err(error) => {
            let message = error
                .value()
                .unbind()
                .string_repr(agent, gc.reborrow())
                .to_string_lossy(agent)
                .into_owned();
            return Err(InstallError::Nova(message));
        }
    };

    let gc = gc.into_nogc();
    let value: Value = value.bind(gc);
    Object::try_from(value).map_err(|_| {
        InstallError::Nova("node:assert bootstrap did not evaluate to a callable object".to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nova_vm::ecmascript::{
        DefaultHostHooks, GcAgent, InternalMethods, PropertyDescriptor, PropertyKey,
        String as JsString, parse_script as parse, script_evaluation as run, unwrap_try,
    };
    use nova_vm::engine::{Bindable, Scopable};

    use crate::node::core::{EnvMap, HostState, NodeCtx};

    /// Run `script` against a realm that has `assert` installed as a global, returning the completion
    /// value rendered as a Rust `String`. Exercises the real [`install`] path end-to-end on a live
    /// Nova agent without depending on the (sibling-owned) module loader.
    fn eval_with_assert(script: &str) -> String {
        let mut agent = GcAgent::new(Default::default(), &DefaultHostHooks);
        let realm = agent.create_default_realm();
        agent.run_in_realm(&realm, |agent, mut gc| {
            let state = HostState::new(std::env::current_dir().unwrap(), EnvMap::new());
            let ctx = NodeCtx::new(&state);
            let assert = install(agent, &ctx, gc.reborrow())
                .expect("assert installs")
                .unbind()
                .scope(agent, gc.nogc());
            let global = agent.current_realm(gc.nogc()).global_object(agent);
            let key = PropertyKey::from_static_str(agent, "assert", gc.nogc());
            unwrap_try(global.try_define_own_property(
                agent,
                key,
                PropertyDescriptor::new_data_descriptor(assert.get(agent)),
                None,
                gc.nogc(),
            ));

            let src = JsString::from_string(agent, script.to_owned(), gc.nogc());
            let r = agent.current_realm(gc.nogc());
            let parsed = parse(agent, src, r, true, None, gc.nogc()).expect("parse test script");
            let out = run(agent, parsed.unbind(), gc.reborrow()).expect("script ran");
            out.unbind()
                .string_repr(agent, gc.reborrow())
                .to_string_lossy(agent)
                .into_owned()
        })
    }

    #[test]
    fn install_returns_callable_with_static_surface() {
        let present = eval_with_assert(
            "typeof assert === 'function' && \
             ['ok','equal','strictEqual','deepStrictEqual','throws','rejects','fail',\
              'AssertionError'].every(function (k) { return k in assert; })",
        );
        assert_eq!(present, "true");
    }

    #[test]
    fn strict_equal_passes_silently_and_throws_on_mismatch() {
        // A passing strictEqual returns undefined (no throw).
        assert_eq!(
            eval_with_assert("assert.strictEqual(1 + 1, 2); 'ok'"),
            "ok"
        );
        // A failing strictEqual throws an AssertionError with the right code.
        assert_eq!(
            eval_with_assert(
                "var c; try { assert.strictEqual(1, 2); } catch (e) { c = e.code; } c"
            ),
            "ERR_ASSERTION"
        );
        // The thrown value is a real AssertionError instanceof Error.
        assert_eq!(
            eval_with_assert(
                "var ok=false; try { assert.strictEqual('a','b'); } \
                 catch (e) { ok = (e instanceof assert.AssertionError) && (e instanceof Error); } ok"
            ),
            "true"
        );
    }

    #[test]
    fn callable_assert_checks_truthiness() {
        assert_eq!(eval_with_assert("assert(1); assert(true); assert('x'); 'ok'"), "ok");
        assert_eq!(
            eval_with_assert("var t=false; try { assert(0); } catch (e) { t = true; } t"),
            "true"
        );
    }

    #[test]
    fn deep_strict_equal_compares_structurally() {
        assert_eq!(
            eval_with_assert("assert.deepStrictEqual({a:[1,2]}, {a:[1,2]}); 'ok'"),
            "ok"
        );
        // Type-strict: 1 !== '1'.
        assert_eq!(
            eval_with_assert(
                "var t=false; try { assert.deepStrictEqual({a:1}, {a:'1'}); } catch (e) { t=true; } t"
            ),
            "true"
        );
        // NaN deep-equals NaN; +0 is NOT deep-strict-equal to -0.
        assert_eq!(eval_with_assert("assert.deepStrictEqual(NaN, NaN); 'ok'"), "ok");
        assert_eq!(
            eval_with_assert("var t=false; try { assert.deepStrictEqual(0, -0); } catch (e) { t=true; } t"),
            "true"
        );
        // Map and Set deep equality.
        assert_eq!(
            eval_with_assert(
                "assert.deepStrictEqual(new Map([['a',1]]), new Map([['a',1]])); \
                 assert.deepStrictEqual(new Set([1,2]), new Set([2,1])); 'ok'"
            ),
            "ok"
        );
    }

    #[test]
    fn throws_matches_constructor_and_regexp() {
        // Matches by constructor.
        assert_eq!(
            eval_with_assert("assert.throws(function () { throw new TypeError('boom'); }, TypeError); 'ok'"),
            "ok"
        );
        // Matches by RegExp against the message.
        assert_eq!(
            eval_with_assert("assert.throws(function () { throw new Error('boom'); }, /boom/); 'ok'"),
            "ok"
        );
        // A function that does not throw makes `throws` itself throw.
        assert_eq!(
            eval_with_assert("var t=false; try { assert.throws(function () {}); } catch (e) { t = e.code === 'ERR_ASSERTION'; } t"),
            "true"
        );
    }

    #[test]
    fn does_not_throw_and_fail_and_iferror() {
        assert_eq!(eval_with_assert("assert.doesNotThrow(function () { return 1; }); 'ok'"), "ok");
        assert_eq!(
            eval_with_assert("var t=false; try { assert.fail('nope'); } catch (e) { t = e.message === 'nope'; } t"),
            "true"
        );
        assert_eq!(eval_with_assert("assert.ifError(null); assert.ifError(undefined); 'ok'"), "ok");
        assert_eq!(
            eval_with_assert("var t=false; try { assert.ifError(new Error('x')); } catch (e) { t=true; } t"),
            "true"
        );
    }

    #[test]
    fn strict_namespace_aliases_to_strict_comparisons() {
        // assert.strict.equal is strict (Object.is) — so 1 vs '1' fails.
        assert_eq!(
            eval_with_assert("var t=false; try { assert.strict.equal(1, '1'); } catch (e) { t=true; } t"),
            "true"
        );
        // ...and a matching strict equal passes.
        assert_eq!(eval_with_assert("assert.strict.equal(2, 2); 'ok'"), "ok");
    }
}
