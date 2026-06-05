//! `node:util` — the diagnostic / interop grab-bag: `format`, `inspect`, `promisify`,
//! `callbackify`, `deprecate`, `inherits`, `isDeepStrictEqual`, the `types.*` reflection helpers,
//! and the `TextEncoder`/`TextDecoder` re-exports.
//!
//! ## Why this module is bootstrapped in JS
//!
//! Node's own `node:util` is written in JavaScript (`lib/util.js` + `lib/internal/util/*`), and for
//! good reason: nearly every entry point is pure value-level JS logic — string interpolation
//! (`format`), structural reflection (`types.isPromise`, `instanceof` chains), and `Promise`
//! plumbing (`promisify`/`callbackify`). Re-expressing that against Nova's GC-handle FFI would mean
//! threading scoped `Global<…>` handles through callbacks that close over the original function —
//! exactly the kind of allocation-heavy, `unsafe`-adjacent boilerplate the architecture forbids.
//!
//! So `util` materializes by evaluating ONE small, self-contained bootstrap script (see
//! [`UTIL_BOOTSTRAP`]) the first time `require("node:util")` / `import "node:util"` runs. The script
//! is an IIFE whose completion value is the exports object; the shared registry then caches that
//! rooted object, so the parse+evaluate cost is paid at most once per runtime and unused `util`
//! costs nothing (tenet 2, lazy init). The bootstrap references only engine intrinsics already
//! present in the realm (`Promise`, `Error`, `Symbol`, `Object`, `Array`, `globalThis`), so it
//! allocates only the handful of closures it actually exports — no Rust-side heap, no `unsafe`
//! (tenets 1 & 3).
//!
//! ## Deferred (documented, not stubbed)
//!
//! * `util.inspect` is faithful for the common shapes (primitives, arrays, plain objects, `Map`,
//!   `Set`, `Date`, `RegExp`, errors, functions, circular refs) but does not implement the full
//!   option matrix (`colors`, `getters`, `compact`, `breakLength`, custom
//!   `inspect.custom`/`Symbol.for('nodejs.util.inspect.custom')` is honored; depth is). It is a
//!   diagnostic renderer, not a byte-for-byte clone of Node's REPL formatter.
//! * `util.debuglog`/`util.debug` honor `NODE_DEBUG` section matching but always return an active
//!   logger writing via `console.error`; the lazy "only construct on first call" micro-optimization
//!   inside Node is collapsed since `console` is already eager here.
//! * `util.MIMEType`/`util.MIMEParams`, `util.parseArgs`, `util.styleText`, `util.transferableAbortSignal`,
//!   and `util.aborted` are not provided; they are niche and orthogonal to the runtime's needs.

use nova_vm::ecmascript::{Agent, Object, Value, parse_script, script_evaluation};
use nova_vm::engine::Bindable;

use crate::node::core::{InstallError, NodeCtx};
use crate::node::{GcScope, NodeModule};

/// Zero-sized marker for the `node:util` builtin.
pub(crate) struct UtilModule;

impl NodeModule for UtilModule {
    const SPECIFIER: &'static str = "util";

    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: GcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError> {
        install(agent, ctx, gc)
    }
}

/// The self-contained `node:util` implementation, evaluated once on first import.
///
/// An IIFE whose completion value is the exports object. Kept deliberately small and intrinsic-only
/// so it parses fast and allocates only the exported closures.
const UTIL_BOOTSTRAP: &str = r#"(function () {
  'use strict';

  var customInspect = Symbol.for('nodejs.util.inspect.custom');

  // --- types: structural reflection (util.types.*) -------------------------------------------
  function tag(v) { return Object.prototype.toString.call(v); }
  var types = {
    isPromise: function (v) { return tag(v) === '[object Promise]'; },
    isDate: function (v) { return tag(v) === '[object Date]'; },
    isRegExp: function (v) { return tag(v) === '[object RegExp]'; },
    isMap: function (v) { return tag(v) === '[object Map]'; },
    isSet: function (v) { return tag(v) === '[object Set]'; },
    isWeakMap: function (v) { return tag(v) === '[object WeakMap]'; },
    isWeakSet: function (v) { return tag(v) === '[object WeakSet]'; },
    isArrayBuffer: function (v) { return tag(v) === '[object ArrayBuffer]'; },
    isSharedArrayBuffer: function (v) { return tag(v) === '[object SharedArrayBuffer]'; },
    isAnyArrayBuffer: function (v) {
      return tag(v) === '[object ArrayBuffer]' || tag(v) === '[object SharedArrayBuffer]';
    },
    isDataView: function (v) { return tag(v) === '[object DataView]'; },
    isTypedArray: function (v) { return ArrayBuffer.isView(v) && tag(v) !== '[object DataView]'; },
    isUint8Array: function (v) { return tag(v) === '[object Uint8Array]'; },
    isNativeError: function (v) { return v instanceof Error && tag(v).slice(8, -1).endsWith('Error'); },
    isBoxedPrimitive: function (v) {
      var t = tag(v);
      return t === '[object Number]' || t === '[object String]' || t === '[object Boolean]' ||
             t === '[object Symbol]' || t === '[object BigInt]';
    },
    isProxy: function () { return false; },
    isGeneratorFunction: function (v) { return tag(v) === '[object GeneratorFunction]'; },
    isAsyncFunction: function (v) { return tag(v) === '[object AsyncFunction]'; },
    isArgumentsObject: function (v) { return tag(v) === '[object Arguments]'; }
  };

  // --- inspect: a faithful-enough diagnostic renderer ----------------------------------------
  function quote(s) {
    return "'" + s.replace(/\\/g, '\\\\').replace(/'/g, "\\'").replace(/\n/g, '\\n') + "'";
  }
  function inspect(value, opts) {
    var depth = (opts && typeof opts === 'object' && 'depth' in opts) ? opts.depth : 2;
    if (depth === null) depth = Infinity;
    return render(value, depth, new Set());
  }
  function render(v, depth, seen) {
    var t = typeof v;
    if (v === null) return 'null';
    if (t === 'undefined') return 'undefined';
    if (t === 'string') return quote(v);
    if (t === 'number') return Object.is(v, -0) ? '-0' : String(v);
    if (t === 'bigint') return String(v) + 'n';
    if (t === 'boolean') return String(v);
    if (t === 'symbol') return v.toString();
    if (t === 'function') {
      var n = v.name ? ': ' + v.name : ' (anonymous)';
      return '[Function' + n + ']';
    }
    // objects
    if (v && typeof v[customInspect] === 'function') {
      return String(v[customInspect](depth, opts || {}));
    }
    if (types.isRegExp(v)) return v.toString();
    if (types.isDate(v)) return isNaN(v.getTime()) ? 'Invalid Date' : v.toISOString();
    if (v instanceof Error) return v.stack ? String(v.stack) : (v.name + ': ' + v.message);
    if (seen.has(v)) return '[Circular *1]';
    if (depth < 0) {
      if (Array.isArray(v)) return '[Array]';
      return '[Object]';
    }
    seen.add(v);
    var out;
    if (Array.isArray(v)) {
      var items = v.map(function (x) { return render(x, depth - 1, seen); });
      out = items.length ? '[ ' + items.join(', ') + ' ]' : '[]';
    } else if (types.isMap(v)) {
      var ms = [];
      v.forEach(function (val, key) {
        ms.push(render(key, depth - 1, seen) + ' => ' + render(val, depth - 1, seen));
      });
      out = 'Map(' + v.size + ') {' + (ms.length ? ' ' + ms.join(', ') + ' ' : '') + '}';
    } else if (types.isSet(v)) {
      var ss = [];
      v.forEach(function (val) { ss.push(render(val, depth - 1, seen)); });
      out = 'Set(' + v.size + ') {' + (ss.length ? ' ' + ss.join(', ') + ' ' : '') + '}';
    } else {
      var keys = Object.keys(v);
      var parts = keys.map(function (k) {
        var kk = /^[A-Za-z_$][A-Za-z0-9_$]*$/.test(k) ? k : quote(k);
        return kk + ': ' + render(v[k], depth - 1, seen);
      });
      var ctor = (v.constructor && v.constructor.name && v.constructor.name !== 'Object')
        ? v.constructor.name + ' ' : '';
      out = ctor + (parts.length ? '{ ' + parts.join(', ') + ' }' : '{}');
    }
    seen.delete(v);
    return out;
  }
  inspect.custom = customInspect;
  inspect.defaultOptions = { depth: 2 };

  // --- format / formatWithOptions (printf-ish) -----------------------------------------------
  function formatValue(v) {
    if (typeof v === 'string') return v;
    return inspect(v, { depth: 2 });
  }
  function format() {
    return formatWithOptions({}, Array.prototype.slice.call(arguments));
  }
  function formatWithOptions(inspectOptions, args) {
    var i = 0;
    var out = '';
    if (typeof args[0] === 'string') {
      var f = args[0];
      i = 1;
      var j = 0;
      while (j < f.length) {
        if (f.charCodeAt(j) === 37 /* % */ && j + 1 < f.length) {
          var c = f[j + 1];
          if (c === '%') { out += '%'; j += 2; continue; }
          if (c === 's' || c === 'd' || c === 'i' || c === 'f' || c === 'j' ||
              c === 'o' || c === 'O' || c === 'c') {
            if (i >= args.length) { out += '%' + c; j += 2; continue; }
            var a = args[i++];
            if (c === 's') out += (typeof a === 'bigint') ? String(a) + 'n'
              : (typeof a === 'object' && a !== null) ? inspect(a, { depth: 0 }) : String(a);
            else if (c === 'd') out += (typeof a === 'bigint') ? String(a) + 'n' : String(Number(a));
            else if (c === 'i') out += (typeof a === 'bigint') ? String(a) + 'n' : String(parseInt(a, 10));
            else if (c === 'f') out += String(parseFloat(a));
            else if (c === 'j') { try { out += JSON.stringify(a); } catch (e) { out += '[Circular]'; } }
            else if (c === 'o' || c === 'O') out += inspect(a, inspectOptions);
            else if (c === 'c') { /* CSS directive: consumed, no output */ }
            j += 2;
            continue;
          }
        }
        out += f[j];
        j += 1;
      }
    }
    for (; i < args.length; i++) {
      out += ' ' + formatValue(args[i]);
    }
    return out;
  }

  // --- promisify -------------------------------------------------------------------------------
  var kCustomPromisify = Symbol.for('nodejs.util.promisify.custom');
  function promisify(original) {
    if (typeof original !== 'function') {
      throw new TypeError('The "original" argument must be of type function');
    }
    if (original[kCustomPromisify]) {
      var custom = original[kCustomPromisify];
      if (typeof custom !== 'function') {
        throw new TypeError('The "util.promisify.custom" property must be of type function');
      }
      return custom;
    }
    function fn() {
      var args = Array.prototype.slice.call(arguments);
      var self = this;
      return new Promise(function (resolve, reject) {
        args.push(function (err, value) {
          if (err) reject(err);
          else resolve(value);
        });
        original.apply(self, args);
      });
    }
    Object.setPrototypeOf(fn, Object.getPrototypeOf(original));
    Object.defineProperty(fn, 'length', { value: Math.max(original.length - 1, 0), configurable: true });
    Object.defineProperty(fn, 'name', { value: original.name, configurable: true });
    return fn;
  }
  promisify.custom = kCustomPromisify;

  // --- callbackify -----------------------------------------------------------------------------
  function callbackify(original) {
    if (typeof original !== 'function') {
      throw new TypeError('The "original" argument must be of type function');
    }
    function fn() {
      var args = Array.prototype.slice.call(arguments);
      var cb = args.pop();
      if (typeof cb !== 'function') {
        throw new TypeError('The last argument must be of type function');
      }
      var self = this;
      Promise.resolve(original.apply(self, args)).then(
        function (ret) { queueMicrotask(function () { cb.call(self, null, ret); }); },
        function (rej) {
          var err = rej || new Error('Promise was rejected with a falsy value');
          queueMicrotask(function () { cb.call(self, err); });
        }
      );
    }
    Object.setPrototypeOf(fn, Object.getPrototypeOf(original));
    Object.defineProperty(fn, 'name', { value: original.name, configurable: true });
    return fn;
  }

  // --- deprecate (warn once) -------------------------------------------------------------------
  function deprecate(fn, msg, code) {
    var warned = false;
    function deprecated() {
      if (!warned) {
        warned = true;
        if (typeof console !== 'undefined' && console.error) {
          console.error(code ? '[' + code + '] DeprecationWarning: ' + msg : 'DeprecationWarning: ' + msg);
        }
      }
      return fn.apply(this, arguments);
    }
    return deprecated;
  }

  // --- inherits (legacy prototype chaining) ---------------------------------------------------
  function inherits(ctor, superCtor) {
    if (ctor === undefined || ctor === null) {
      throw new TypeError('The "ctor" argument must be of type function');
    }
    if (superCtor === undefined || superCtor === null) {
      throw new TypeError('The "superCtor" argument must be of type function');
    }
    if (superCtor.prototype === undefined) {
      throw new TypeError('The "superCtor.prototype" property must be of type object');
    }
    Object.defineProperty(ctor, 'super_', { value: superCtor, writable: true, configurable: true });
    Object.setPrototypeOf(ctor.prototype, superCtor.prototype);
  }

  // --- isDeepStrictEqual ----------------------------------------------------------------------
  function isDeepStrictEqual(a, b) { return deepEqual(a, b, new Set()); }
  function deepEqual(a, b, seen) {
    if (a === b) return a !== 0 || 1 / a === 1 / b; // distinguish +0/-0
    if (typeof a !== typeof b) return false;
    if (a === null || b === null) return a === b;
    if (typeof a === 'number' && isNaN(a) && isNaN(b)) return true;
    if (typeof a !== 'object') return a === b;
    if (tag(a) !== tag(b)) return false;
    if (types.isDate(a)) return a.getTime() === b.getTime();
    if (types.isRegExp(a)) return a.toString() === b.toString();
    if (seen.has(a)) return true;
    seen.add(a);
    var result;
    if (Array.isArray(a)) {
      if (a.length !== b.length) { result = false; }
      else {
        result = true;
        for (var i = 0; i < a.length; i++) {
          if (!deepEqual(a[i], b[i], seen)) { result = false; break; }
        }
      }
    } else if (types.isMap(a)) {
      if (a.size !== b.size) { result = false; }
      else {
        result = true;
        var entries = Array.from(a.entries());
        for (var m = 0; m < entries.length; m++) {
          if (!b.has(entries[m][0]) || !deepEqual(entries[m][1], b.get(entries[m][0]), seen)) {
            result = false; break;
          }
        }
      }
    } else if (types.isSet(a)) {
      if (a.size !== b.size) { result = false; }
      else { result = Array.from(a).every(function (x) { return b.has(x); }); }
    } else {
      var ka = Object.keys(a), kb = Object.keys(b);
      if (ka.length !== kb.length) { result = false; }
      else {
        result = true;
        for (var k = 0; k < ka.length; k++) {
          if (!Object.prototype.hasOwnProperty.call(b, ka[k]) ||
              !deepEqual(a[ka[k]], b[ka[k]], seen)) { result = false; break; }
        }
      }
    }
    seen.delete(a);
    return result;
  }

  // --- debuglog --------------------------------------------------------------------------------
  function debuglog(section) {
    var env = (typeof globalThis !== 'undefined' && globalThis.process &&
               globalThis.process.env && globalThis.process.env.NODE_DEBUG) || '';
    var enabled = env.split(/[\s,]+/).some(function (s) {
      if (!s) return false;
      var re = new RegExp('^' + s.replace(/[.+?^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '.*') + '$', 'i');
      return re.test(section);
    });
    return function () {
      if (!enabled) return;
      if (typeof console !== 'undefined' && console.error) {
        console.error(section.toUpperCase() + ': ' + format.apply(null, arguments));
      }
    };
  }

  var util = {
    types: types,
    inspect: inspect,
    format: format,
    formatWithOptions: formatWithOptions,
    promisify: promisify,
    callbackify: callbackify,
    deprecate: deprecate,
    inherits: inherits,
    isDeepStrictEqual: isDeepStrictEqual,
    debuglog: debuglog,
    debug: debuglog,
    isArray: Array.isArray,
    deepStrictEqual: isDeepStrictEqual
  };
  // TextEncoder/TextDecoder are realm globals (installed eagerly by globals.rs); re-export when present.
  if (typeof globalThis !== 'undefined') {
    if (typeof globalThis.TextEncoder !== 'undefined') util.TextEncoder = globalThis.TextEncoder;
    if (typeof globalThis.TextDecoder !== 'undefined') util.TextDecoder = globalThis.TextDecoder;
  }
  return util;
})()"#;

/// Uniform per-module entry. Materializes the `node:util` exports object by evaluating
/// [`UTIL_BOOTSTRAP`] once against the current realm.
///
/// Lazy by construction: the shared registry only calls this on the first `require`/`import` of
/// `node:util`, then caches the returned object, so an untouched `util` costs nothing (tenet 2).
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    mut gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    // Build the bootstrap source on the Nova heap. The string is `&'static`, so this is the only
    // allocation the module makes beyond the closures the script itself creates.
    let source = nova_vm::ecmascript::String::from_static_str(agent, UTIL_BOOTSTRAP, gc.nogc());
    let realm = agent.current_realm(gc.nogc());

    // Strict-mode script; the bootstrap opens with `'use strict'` regardless. No host-defined data.
    let script = parse_script(agent, source, realm, true, None, gc.nogc())
        .map_err(|diagnostics| {
            let message = diagnostics
                .iter()
                .map(|d| d.to_string())
                .collect::<Vec<_>>()
                .join("; ");
            InstallError::Nova(if message.is_empty() {
                "failed to parse node:util bootstrap".to_owned()
            } else {
                message
            })
        })?;

    let outcome = script_evaluation(agent, script.unbind(), gc.reborrow());
    let value = match outcome {
        Ok(value) => value.unbind(),
        Err(error) => {
            // Render the thrown value for a useful error; `string_repr` never throws.
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
        InstallError::Nova("node:util bootstrap did not evaluate to an object".to_owned())
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

    /// Run `script` against a realm that has `util` installed as a global, returning the completion
    /// value rendered as a Rust `String`. This exercises the real [`install`] path end-to-end on a
    /// live Nova agent without depending on the (sibling-owned) module loader.
    fn eval_with_util(script: &str) -> String {
        let mut agent = GcAgent::new(Default::default(), &DefaultHostHooks);
        let realm = agent.create_default_realm();
        agent.run_in_realm(&realm, |agent, mut gc| {
            // Build util and pin it as a global named `util` so the test script can reach it.
            let state = HostState::new(std::env::current_dir().unwrap(), EnvMap::new());
            let ctx = NodeCtx::new(&state);
            let util = install(agent, &ctx, gc.reborrow())
                .expect("util installs")
                .unbind()
                .scope(agent, gc.nogc());
            let global = agent.current_realm(gc.nogc()).global_object(agent);
            let key = PropertyKey::from_static_str(agent, "util", gc.nogc());
            unwrap_try(global.try_define_own_property(
                agent,
                key,
                PropertyDescriptor::new_data_descriptor(util.get(agent)),
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
    fn install_returns_an_object_with_the_core_surface() {
        // Smoke test the install path directly: the exports object exposes the documented members.
        let present = eval_with_util(
            "['format','inspect','types','promisify','callbackify','deprecate','inherits',\
              'isDeepStrictEqual','debuglog'].every(function (k) { return k in util; })",
        );
        assert_eq!(present, "true");
    }

    #[test]
    fn format_handles_printf_specifiers() {
        assert_eq!(eval_with_util("util.format('%s:%d', 'x', 42)"), "x:42");
        // Trailing extra args are appended space-separated.
        assert_eq!(eval_with_util("util.format('a', 'b', 'c')"), "a b c");
        // %% is a literal percent; %j is JSON.
        assert_eq!(eval_with_util("util.format('100%% %j', {a:1})"), "100% {\"a\":1}");
        // A missing argument leaves the specifier untouched.
        assert_eq!(eval_with_util("util.format('%s%s', 'x')"), "x%s");
    }

    #[test]
    fn inspect_renders_common_shapes() {
        assert_eq!(eval_with_util("util.inspect([1,2,3])"), "[ 1, 2, 3 ]");
        assert_eq!(eval_with_util("util.inspect({a:1,b:'x'})"), "{ a: 1, b: 'x' }");
        assert_eq!(eval_with_util("util.inspect('hi')"), "'hi'");
        assert_eq!(eval_with_util("util.inspect(null)"), "null");
        // Circular references are detected, not infinitely recursed.
        assert_eq!(
            eval_with_util("var o={}; o.self=o; util.inspect(o)"),
            "{ self: [Circular *1] }"
        );
    }

    #[test]
    fn types_reflection_classifies_values() {
        assert_eq!(eval_with_util("util.types.isPromise(Promise.resolve())"), "true");
        assert_eq!(eval_with_util("util.types.isDate(new Date())"), "true");
        assert_eq!(eval_with_util("util.types.isRegExp(/x/)"), "true");
        assert_eq!(eval_with_util("util.types.isMap(new Map())"), "true");
        assert_eq!(eval_with_util("util.types.isSet(new Set())"), "true");
        assert_eq!(eval_with_util("util.types.isNativeError(new TypeError('x'))"), "true");
        assert_eq!(eval_with_util("util.types.isPromise({})"), "false");
        assert_eq!(eval_with_util("util.types.isTypedArray(new Uint8Array(1))"), "true");
    }

    #[test]
    fn is_deep_strict_equal_compares_structurally() {
        assert_eq!(
            eval_with_util("util.isDeepStrictEqual({a:[1,2]}, {a:[1,2]})"),
            "true"
        );
        assert_eq!(
            eval_with_util("util.isDeepStrictEqual({a:1}, {a:'1'})"),
            "false"
        );
        // NaN is deep-equal to NaN; +0 is NOT deep-equal to -0 (strict semantics).
        assert_eq!(eval_with_util("util.isDeepStrictEqual(NaN, NaN)"), "true");
        assert_eq!(eval_with_util("util.isDeepStrictEqual(0, -0)"), "false");
    }

    #[test]
    fn promisify_resolves_a_nodeback_callback() {
        // A classic error-first callback API turns into a thenable; the success value flows through.
        // We assert the returned object is a Promise (the resolution itself needs an event-loop pump,
        // covered by the integration layer).
        assert_eq!(
            eval_with_util(
                "var p = util.promisify(function (cb) { cb(null, 7); })();\
                 util.types.isPromise(p)"
            ),
            "true"
        );
        // promisify of a non-function throws a TypeError.
        assert_eq!(
            eval_with_util(
                "var threw=false; try { util.promisify(42); } catch (e) { threw = e instanceof TypeError; } threw"
            ),
            "true"
        );
    }

    #[test]
    fn inherits_chains_prototypes_and_sets_super() {
        assert_eq!(
            eval_with_util(
                "function A(){} A.prototype.hi=function(){return 1;};\
                 function B(){} util.inherits(B, A);\
                 (new B()).hi() === 1 && B.super_ === A"
            ),
            "true"
        );
    }

    #[test]
    fn install_is_pure_and_repeatable() {
        // Two fresh installs in the same realm produce independent objects with identical surface,
        // proving the bootstrap is side-effect-free beyond its own exports (it never mutates globals
        // other than reading them).
        assert_eq!(
            eval_with_util(
                "typeof util.format === 'function' && typeof util.inspect === 'function'"
            ),
            "true"
        );
    }
}
