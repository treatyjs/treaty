//! `structuredClone` — the HTML/WHATWG structured-clone algorithm, exposed as an eager global.
//!
//! `structuredClone(value, { transfer })` performs a deep, structure-preserving copy of a JS value:
//! it duplicates nested objects/arrays, **preserves shared references and cycles** (the same source
//! object encountered twice yields the same cloned object twice), clones the platform "transferable"
//! and serializable types (`Date`, `RegExp`, `Map`, `Set`, `ArrayBuffer`, typed arrays, `DataView`,
//! `Boolean`/`Number`/`String` wrapper objects, `Error` subclasses), and **throws `DataCloneError`**
//! for things that cannot be cloned (functions, symbols, and — in our realm — DOM-only types). This
//! is the same algorithm Node exposes as a global since v17 and that WinterCG mandates for runtimes.
//!
//! ## Why this module is bootstrapped in JS
//!
//! The structured-clone algorithm is, at its core, a recursive walk over arbitrary JS values with a
//! memory map (source object -> cloned object) to preserve identity and break cycles. Expressing that
//! against Nova's GC-handle FFI would mean threading scoped `Global<…>` handles through a recursion
//! that allocates a fresh handle per visited node and a `HashMap` of handles for the memory — exactly
//! the allocation-heavy, `unsafe`-adjacent boilerplate the architecture forbids (tenets 1 & 3). The
//! algorithm is *pure value-level JS logic* and references only engine intrinsics already present in
//! every realm (`Object`, `Array`, `Map`, `Set`, `Date`, `RegExp`, `ArrayBuffer`, the typed arrays,
//! `Error`, `WeakMap`, `Reflect`). So this module materializes by evaluating ONE small, self-contained
//! bootstrap script ([`STRUCTURED_CLONE_BOOTSTRAP`]) the first time it is built. The script is an IIFE
//! whose completion value is the `structuredClone` function itself; the realm wiring (`globals.rs`)
//! installs that function as the eager `structuredClone` global, and the registry caches the returned
//! object, so the parse+evaluate cost is paid at most once per runtime (tenet 2, lazy init — even an
//! "eager" global only materializes when the realm wires it, and never twice).
//!
//! The `WeakMap` memory means a cloned graph is collected with the clone, holds no Rust-side state,
//! and adds no allocation beyond the closures and the per-call memory the algorithm inherently needs.
//!
//! ## Faithfulness and documented divergences
//!
//! Implemented faithfully:
//! * **Primitives** (`undefined`, `null`, booleans, numbers, bigints, strings) pass through by value.
//! * **Identity + cycles**: a `WeakMap` memo maps each already-cloned source object to its clone, so
//!   `a.self = a; structuredClone(a).self === clone` and shared sub-objects stay shared.
//! * **Plain objects / arrays**: own enumerable string-keyed properties are cloned (array holes and
//!   `length` preserved); the prototype is reset to `Object.prototype`/`Array.prototype` per spec
//!   (structured clone does not preserve custom prototypes).
//! * **`Date`** (cloned by time value), **`RegExp`** (source + flags + `lastIndex`), **`Map`**/**`Set`**
//!   (keys and values deep-cloned), **`ArrayBuffer`** (byte copy), **typed arrays + `DataView`**
//!   (cloned over a cloned buffer, offset/length preserved), **wrapper objects**
//!   (`new Number/String/Boolean`), and **`Error`** subclasses (`name`/`message`/`stack`/`cause`).
//! * **Uncloneable inputs throw `DataCloneError`**: functions, symbols (as values or own symbol keys
//!   are skipped per spec), and any other exotic/host object the algorithm does not recognize.
//!
//! Deferred (documented, not stubbed — these are DOM/host concepts absent from a server realm):
//! * The `transfer` option's *detaching* semantics. We accept the option and, for any listed
//!   `ArrayBuffer`, clone it (a copy), but do not detach the original — Nova exposes no embedder API to
//!   detach an `ArrayBuffer`, and a server runtime has no `MessagePort`/`OffscreenCanvas` to transfer
//!   ownership to. The cloned graph is correct; only the source-side detachment is skipped.
//! * Host-only serializables (`Blob`, `File`, `ImageData`, `DOMException`, `MessagePort`, …) are not
//!   present in this realm, so they fall through to the `DataCloneError` path exactly as they would for
//!   any unrecognized object.

use nova_vm::ecmascript::{Agent, Object, Value, parse_script, script_evaluation};
use nova_vm::engine::Bindable;

use crate::node::core::{InstallError, NodeCtx};
use crate::node::{GcScope, NodeModule};

/// Zero-sized marker for the `node:structured_clone` builtin.
///
/// `structuredClone` is surfaced as an eager *global* by `globals.rs`, not as an importable `node:`
/// specifier (it has no module form in Node either), so this marker exists for symmetry with the other
/// leaf modules and to pin the canonical name; it is intentionally absent from `BUILTINS`.
pub(crate) struct StructuredCloneModule;

impl NodeModule for StructuredCloneModule {
    const SPECIFIER: &'static str = "structured_clone";

    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: GcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError> {
        install(agent, ctx, gc)
    }
}

/// The self-contained `structuredClone` implementation, evaluated once on first build.
///
/// An IIFE whose completion value is the `structuredClone` **function** (also reachable as the
/// `.structuredClone` property of itself, so the registry can treat the result uniformly as an
/// object). Kept deliberately small and intrinsic-only so it parses fast and allocates only the
/// closures it exports. The memory map is a per-call `WeakMap`, so a cloned graph holds no global
/// state and is GC'd with its clone.
const STRUCTURED_CLONE_BOOTSTRAP: &str = r#"(function () {
  'use strict';

  var toString = Object.prototype.toString;
  function tag(v) { return toString.call(v); }

  // A DataCloneError, matching the platform: a DOMException-shaped error with name 'DataCloneError'.
  // No DOMException exists in this realm, so we synthesize an Error carrying the canonical name.
  function dataCloneError(message) {
    var err = new Error(message);
    err.name = 'DataCloneError';
    return err;
  }

  function isUncloneablePrimitive(t) {
    // typeof: 'function' and 'symbol' are never structured-cloneable.
    return t === 'function' || t === 'symbol';
  }

  function cloneArrayBuffer(buf) {
    // Copy the bytes into a fresh ArrayBuffer. `slice(0)` is a spec-defined byte copy.
    return buf.slice(0);
  }

  function clone(value, memory) {
    var t = typeof value;

    // Primitives pass through by value (undefined, boolean, number, string, bigint, null).
    if (value === null || (t !== 'object' && t !== 'function')) {
      if (isUncloneablePrimitive(t)) {
        throw dataCloneError(
          (t === 'function' ? 'A function' : 'A symbol') + ' could not be cloned.'
        );
      }
      return value;
    }
    if (t === 'function') {
      throw dataCloneError('A function could not be cloned.');
    }

    // Identity + cycle preservation: if we have already cloned this exact object, reuse the clone.
    var existing = memory.get(value);
    if (existing !== undefined) return existing;

    var t2 = tag(value);

    // --- Date -------------------------------------------------------------------------------
    if (t2 === '[object Date]') {
      var d = new Date(value.getTime());
      memory.set(value, d);
      return d;
    }

    // --- RegExp -----------------------------------------------------------------------------
    if (t2 === '[object RegExp]') {
      var re = new RegExp(value.source, value.flags);
      re.lastIndex = value.lastIndex;
      memory.set(value, re);
      return re;
    }

    // --- ArrayBuffer ------------------------------------------------------------------------
    if (t2 === '[object ArrayBuffer]') {
      var ab = cloneArrayBuffer(value);
      memory.set(value, ab);
      return ab;
    }

    // --- SharedArrayBuffer (shared by reference, not copied) --------------------------------
    if (t2 === '[object SharedArrayBuffer]') {
      memory.set(value, value);
      return value;
    }

    // --- DataView ---------------------------------------------------------------------------
    if (t2 === '[object DataView]') {
      var dvBuf = clone(value.buffer, memory);
      var dv = new DataView(dvBuf, value.byteOffset, value.byteLength);
      memory.set(value, dv);
      return dv;
    }

    // --- Typed arrays (Uint8Array, Float64Array, BigInt64Array, ...) ------------------------
    if (ArrayBuffer.isView(value)) {
      var Ctor = value.constructor;
      var taBuf = clone(value.buffer, memory);
      var ta = new Ctor(taBuf, value.byteOffset, value.length);
      memory.set(value, ta);
      return ta;
    }

    // --- Map --------------------------------------------------------------------------------
    if (t2 === '[object Map]') {
      var m = new Map();
      memory.set(value, m);
      value.forEach(function (v, k) {
        m.set(clone(k, memory), clone(v, memory));
      });
      return m;
    }

    // --- Set --------------------------------------------------------------------------------
    if (t2 === '[object Set]') {
      var s = new Set();
      memory.set(value, s);
      value.forEach(function (v) {
        s.add(clone(v, memory));
      });
      return s;
    }

    // --- Error subclasses -------------------------------------------------------------------
    if (value instanceof Error) {
      // Reconstruct with the closest standard constructor by name; default to Error.
      var ctorByName = {
        Error: Error, TypeError: TypeError, RangeError: RangeError,
        ReferenceError: ReferenceError, SyntaxError: SyntaxError,
        EvalError: EvalError, URIError: URIError
      };
      var EC = ctorByName[value.name] || Error;
      var e = new EC(value.message);
      memory.set(value, e);
      if (value.name !== e.name) { try { e.name = value.name; } catch (_) {} }
      if ('stack' in value) { try { e.stack = value.stack; } catch (_) {} }
      if (value.cause !== undefined) {
        try { e.cause = clone(value.cause, memory); } catch (_) {}
      }
      return e;
    }

    // --- Boxed primitive wrapper objects (new Number/String/Boolean) ------------------------
    if (t2 === '[object Number]') {
      var nObj = new Number(value.valueOf());
      memory.set(value, nObj);
      return nObj;
    }
    if (t2 === '[object String]') {
      var strObj = new String(value.valueOf());
      memory.set(value, strObj);
      return strObj;
    }
    if (t2 === '[object Boolean]') {
      var bObj = new Boolean(value.valueOf());
      memory.set(value, bObj);
      return bObj;
    }
    if (t2 === '[object BigInt]') {
      // A boxed BigInt object; unbox and re-box via Object() of the primitive.
      var biObj = Object(value.valueOf());
      memory.set(value, biObj);
      return biObj;
    }

    // --- Arrays -----------------------------------------------------------------------------
    if (Array.isArray(value)) {
      var arr = new Array(value.length);
      memory.set(value, arr);
      // Own enumerable string keys: copies indices (preserving holes) plus any extra named props.
      var aKeys = Object.keys(value);
      for (var ai = 0; ai < aKeys.length; ai++) {
        var ak = aKeys[ai];
        arr[ak] = clone(value[ak], memory);
      }
      return arr;
    }

    // --- Plain objects (and null-prototype objects) -----------------------------------------
    // Structured clone does not preserve custom prototypes: the result is a plain object.
    // Anything with an exotic class tag we did not handle above is not cloneable.
    if (t2 === '[object Object]') {
      var out = {};
      memory.set(value, out);
      var oKeys = Object.keys(value); // own enumerable string keys; symbol keys are skipped per spec
      for (var oi = 0; oi < oKeys.length; oi++) {
        var ok = oKeys[oi];
        out[ok] = clone(value[ok], memory);
      }
      return out;
    }

    // Anything else (Promise, WeakMap/WeakSet, host/exotic objects, Proxy of an exotic) cannot be
    // structurally cloned.
    throw dataCloneError(tag(value).slice(8, -1) + ' object could not be cloned.');
  }

  function structuredClone(value, options) {
    // The `transfer` option is accepted for API compatibility. Detaching is not performed (see the
    // module note): listed ArrayBuffers are cloned by copy like any other, which is observable only as
    // the source not being detached.
    void options;
    var memory = new WeakMap();
    return clone(value, memory);
  }

  // Return the function. It is itself an object, so the registry can treat the export uniformly; the
  // realm wiring installs it as the eager `structuredClone` global.
  return structuredClone;
})()"#;

/// Uniform per-module entry. Materializes the `structuredClone` function by evaluating
/// [`STRUCTURED_CLONE_BOOTSTRAP`] once against the current realm and returns it as an [`Object`].
///
/// Lazy by construction (tenet 2): this runs at most once — the realm wiring (`globals.rs`) calls it a
/// single time to install the eager `structuredClone` global, and the registry caches the result, so a
/// runtime that never touches `structuredClone` pays nothing. The bootstrap references only realm
/// intrinsics, so beyond the `&'static` source string the only allocations are the closures the script
/// itself creates (tenets 1 & 3 — no `unsafe`, no Rust-side heap, no per-clone Nova handles).
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    mut gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    // The source is `&'static`, so this is the only allocation the module makes beyond the closures the
    // script itself creates.
    let source =
        nova_vm::ecmascript::String::from_static_str(agent, STRUCTURED_CLONE_BOOTSTRAP, gc.nogc());
    let realm = agent.current_realm(gc.nogc());

    // Strict-mode script; the bootstrap opens with `'use strict'` regardless. No host-defined data.
    let script =
        parse_script(agent, source, realm, true, None, gc.nogc()).map_err(|diagnostics| {
            let message = diagnostics
                .iter()
                .map(|d| d.to_string())
                .collect::<Vec<_>>()
                .join("; ");
            InstallError::Nova(if message.is_empty() {
                "failed to parse structuredClone bootstrap".to_owned()
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
        InstallError::Nova("structuredClone bootstrap did not evaluate to an object".to_owned())
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

    /// Run `script` against a realm that has `structuredClone` installed as a global, returning the
    /// completion value rendered as a Rust `String`. Exercises the real [`install`] path end-to-end on
    /// a live Nova agent without depending on the (sibling-owned) module loader or globals wiring.
    fn eval_with_clone(script: &str) -> String {
        let mut agent = GcAgent::new(Default::default(), &DefaultHostHooks);
        let realm = agent.create_default_realm();
        agent.run_in_realm(&realm, |agent, mut gc| {
            // Build structuredClone and pin it as the `structuredClone` global so the test script can
            // call it exactly as Node code would.
            let state = HostState::new(std::env::current_dir().unwrap(), EnvMap::new());
            let ctx = NodeCtx::new(&state);
            let func = install(agent, &ctx, gc.reborrow())
                .expect("structuredClone installs")
                .unbind()
                .scope(agent, gc.nogc());
            let global = agent.current_realm(gc.nogc()).global_object(agent);
            let key = PropertyKey::from_static_str(agent, "structuredClone", gc.nogc());
            unwrap_try(global.try_define_own_property(
                agent,
                key,
                PropertyDescriptor::new_data_descriptor(func.get(agent)),
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
    fn install_returns_a_callable_structured_clone() {
        // The export is a function; calling it on a primitive returns that primitive.
        assert_eq!(eval_with_clone("typeof structuredClone"), "function");
        assert_eq!(eval_with_clone("structuredClone(42)"), "42");
        assert_eq!(eval_with_clone("structuredClone('hi')"), "hi");
        assert_eq!(eval_with_clone("String(structuredClone(null))"), "null");
        assert_eq!(eval_with_clone("String(structuredClone(undefined))"), "undefined");
        assert_eq!(eval_with_clone("structuredClone(true)"), "true");
        assert_eq!(eval_with_clone("String(structuredClone(10n))"), "10");
    }

    #[test]
    fn deep_clones_nested_objects_without_aliasing() {
        // The clone is a distinct object graph: mutating the clone must not affect the source.
        let script = "var a = { x: 1, inner: { y: 2 } };\
             var b = structuredClone(a);\
             b.inner.y = 99;\
             [a !== b, a.inner !== b.inner, a.inner.y === 2, b.inner.y === 99].join(',')";
        assert_eq!(eval_with_clone(script), "true,true,true,true");
    }

    #[test]
    fn deep_clones_arrays_and_preserves_holes() {
        let script = "var a = [1, , 3];\
             var b = structuredClone(a);\
             [Array.isArray(b), b.length === 3, b[0] === 1, b[2] === 3, (1 in b)].join(',')";
        // index 1 is a hole and must remain absent.
        assert_eq!(eval_with_clone(script), "true,true,true,true,false");
    }

    #[test]
    fn preserves_shared_references_and_cycles() {
        // A shared sub-object stays shared; a self-cycle is reproduced (no infinite recursion).
        let shared = "var inner = { v: 1 };\
             var a = { p: inner, q: inner };\
             var b = structuredClone(a);\
             (b.p === b.q).toString()";
        assert_eq!(eval_with_clone(shared), "true");

        let cyclic = "var a = {};\
             a.self = a;\
             var b = structuredClone(a);\
             [b.self === b, b !== a].join(',')";
        assert_eq!(eval_with_clone(cyclic), "true,true");
    }

    #[test]
    fn clones_date_by_value() {
        let script = "var d = new Date(1234567890000);\
             var c = structuredClone(d);\
             [c instanceof Date, c !== d, c.getTime() === d.getTime()].join(',')";
        assert_eq!(eval_with_clone(script), "true,true,true");
    }

    #[test]
    fn clones_regexp_with_flags_and_last_index() {
        let script = "var r = /ab+c/gi; r.lastIndex = 2;\
             var c = structuredClone(r);\
             [c instanceof RegExp, c !== r, c.source === 'ab+c', c.flags === 'gi', c.lastIndex === 2]\
               .join(',')";
        assert_eq!(eval_with_clone(script), "true,true,true,true,true");
    }

    #[test]
    fn clones_map_and_set_deeply() {
        let mapScript = "var m = new Map([['k', { n: 1 }]]);\
             var c = structuredClone(m);\
             c.get('k').n = 2;\
             [c instanceof Map, c !== m, c.size === 1, m.get('k').n === 1, c.get('k').n === 2]\
               .join(',')";
        assert_eq!(eval_with_clone(mapScript), "true,true,true,true,true");

        let setScript = "var s = new Set([1, 2, 3]);\
             var c = structuredClone(s);\
             [c instanceof Set, c !== s, c.size === 3, c.has(2)].join(',')";
        assert_eq!(eval_with_clone(setScript), "true,true,true,true");
    }

    #[test]
    fn clones_array_buffer_and_typed_arrays() {
        // ArrayBuffer is byte-copied; the underlying bytes match but the buffer is distinct.
        let abScript = "var ab = new Uint8Array([1,2,3,4]).buffer;\
             var c = structuredClone(ab);\
             var src = new Uint8Array(ab), dst = new Uint8Array(c);\
             [c instanceof ArrayBuffer, c !== ab, c.byteLength === 4, dst[0] === 1, dst[3] === 4]\
               .join(',')";
        assert_eq!(eval_with_clone(abScript), "true,true,true,true,true");

        // Typed array is cloned over a cloned buffer; mutation does not write back to the source.
        let taScript = "var ta = new Uint16Array([10, 20, 30]);\
             var c = structuredClone(ta);\
             c[0] = 999;\
             [c instanceof Uint16Array, c.length === 3, ta[0] === 10, c[0] === 999, c[2] === 30]\
               .join(',')";
        assert_eq!(eval_with_clone(taScript), "true,true,true,true,true");
    }

    #[test]
    fn clones_error_preserving_name_and_message() {
        let script = "var e = new TypeError('boom'); e.cause = { why: 1 };\
             var c = structuredClone(e);\
             [c instanceof Error, c.name === 'TypeError', c.message === 'boom', \
              c !== e, c.cause && c.cause.why === 1].join(',')";
        assert_eq!(eval_with_clone(script), "true,true,true,true,true");
    }

    #[test]
    fn throws_data_clone_error_for_functions_and_symbols() {
        // A function is not cloneable: the call throws a DataCloneError.
        let fnScript = "var ok = false; try { structuredClone(function () {}); } \
             catch (e) { ok = e.name === 'DataCloneError'; } ok.toString()";
        assert_eq!(eval_with_clone(fnScript), "true");

        // A symbol value is not cloneable.
        let symScript = "var ok = false; try { structuredClone(Symbol('s')); } \
             catch (e) { ok = e.name === 'DataCloneError'; } ok.toString()";
        assert_eq!(eval_with_clone(symScript), "true");

        // A function nested inside an object also triggers the error.
        let nestedScript = "var ok = false; try { structuredClone({ f: function () {} }); } \
             catch (e) { ok = e.name === 'DataCloneError'; } ok.toString()";
        assert_eq!(eval_with_clone(nestedScript), "true");
    }

    #[test]
    fn skips_symbol_keys_and_resets_prototype() {
        // Own symbol-keyed properties are not part of the structured clone; the result is a plain
        // object whose prototype is Object.prototype regardless of the source's prototype.
        let script = "function Custom() { this.a = 1; } Custom.prototype.tag = 'x';\
             var src = new Custom(); src[Symbol('hidden')] = 2;\
             var c = structuredClone(src);\
             [c.a === 1, Object.getPrototypeOf(c) === Object.prototype, \
              Object.getOwnPropertySymbols(c).length === 0].join(',')";
        assert_eq!(eval_with_clone(script), "true,true,true");
    }
}
