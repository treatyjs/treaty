//! CommonJS module loading: the `require(...)` bridge.
//!
//! `require(specifier)` resolves a specifier through the runtime resolver
//! ([`crate::node::module_resolver::resolve_and_load`]), then returns a *cached* exports object,
//! building it exactly once on a cache miss. This is the synchronous Node `require` contract:
//! repeated `require` of the same module yields the identical object (module identity / singleton
//! semantics), so the cache is correctness, not just speed.
//!
//! Two cache tiers, mirroring [`crate::node::core::HostState`]:
//!
//! * **`node:` builtins** — keyed by their canonical `&'static str` specifier in
//!   [`NodeCtx::builtin_cache`]. On first `require("node:fs")` the builtin's lazy `install` runs and
//!   its exports object is rooted in a [`Global`]; every later `require` of that specifier returns
//!   the same rooted object. An untouched builtin never materializes (tenet 2: zero startup cost).
//! * **user files** — keyed by absolute path in [`HostState::module_cache`]. On first
//!   `require("./mod")` the resolver reads + lowers the file to JavaScript, the body is evaluated
//!   inside a CommonJS wrapper (`module`/`exports`/`require`/`__dirname`/`__filename` in scope), and
//!   `module.exports` is rooted under the resolved absolute path; every later `require` of the same
//!   file returns the identical object.
//!
//! Allocation discipline (tenet 3): the builtin key is a `&'static str` re-derived from the static
//! `BUILTINS` table (no `String`); a cache hit clones only the cheap `Global` handle, never the
//! exports object; the specifier argument is read as a borrowed [`Cow`] and only owned when Nova has
//! to transcode it.
//!
//! `require` is surfaced as a Node global. Because the scaffold owns `globals.rs`, this file exposes
//! [`install_require`] (the uniform "install me onto the realm global" seam) and the `RegularFn`
//! [`require`] so the global wiring can call it; the resolve→cache→materialize heart lives in
//! [`require_specifier`], which is engine-driven and unit-tested against a live agent below.
//!
//! ### User-file evaluation
//!
//! A *user* `.js`/`.ts`/`.json` file is loaded by running its (already-lowered-to-JS) body inside a
//! CommonJS wrapper that supplies the five module locals (`module`, `exports`, `require`,
//! `__filename`, `__dirname`). The non-engine half — resolution, `std::fs` reading, TS transpile,
//! JSON wrapping, absolute-path cache-key derivation — is done by [`crate::node::module_resolver`];
//! the engine half is done here with the same public Nova operations the crate already uses to run a
//! script ([`parse_script`] + [`script_evaluation`]), so no `pub(crate)`/visibility hack and no
//! `unsafe` beyond the one shared FFI boundary in [`crate::node::core`]. The wrapper script's
//! completion value *is* the module's `module.exports`, read back directly. Cycle safety follows
//! Node: the freshly-created `exports` object is rooted into the module cache **before** the body
//! evaluates, so a cyclic `require` of the same file mid-evaluation observes the partially-populated
//! exports rather than recursing forever.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use nova_vm::ecmascript::{
    Agent, ArgumentsList, Behaviour, BuiltinFunctionArgs, ExceptionType, InternalMethods, JsResult,
    Object, OrdinaryObject, PropertyDescriptor, PropertyKey, String as JsString, Value,
    create_builtin_function, parse_script, script_evaluation, unwrap_try,
};
use nova_vm::engine::{Bindable, Global, NoGcScope};

use crate::node::core::{InstallError, NodeCtx};
use crate::node::globals::define_value;
use crate::node::module_resolver::{resolve_and_load, LoadAction};
use crate::node::GcScope;

/// The arity reported to JS for `require` (one argument: the specifier).
const REQUIRE_ARITY: u32 = 1;

/// Resolve `specifier` and return its exports object, building+caching builtins exactly once.
///
/// This is the synchronous CommonJS `require` core. It is engine-driven (it constructs the builtin's
/// exports object via the module's lazy `install`) but holds no `unsafe`: rooting goes through the
/// safe [`Global`] API and the `&'static str` builtin key keeps the cache allocation-free.
///
/// `ctx` borrows the runtime's [`crate::node::core::HostState`], which is owned by the JsRuntime's
/// box and lives independently of the `&mut Agent`; the two reference disjoint memory (the host-side
/// resolver + `RefCell` caches vs. the Nova heap), so they coexist without aliasing and without a
/// held-borrow conflict. The caller ([`require`]) is responsible for handing in such a decoupled
/// `ctx` (see its FFI-boundary note).
///
/// * On a [`LoadAction::Builtin`] cache hit, the rooted exports object is resolved to the caller's
///   scope and returned with no allocation.
/// * On a builtin cache miss, the module's `install` runs once, the result is rooted into the
///   builtin cache, and the same object is returned. Subsequent calls hit the cache.
/// * A [`LoadAction::File`] is loaded as a CommonJS module: on a [`HostState::module_cache`] miss the
///   lowered source is evaluated inside a `(module, exports, require, __filename, __dirname)` wrapper
///   ([`eval_cjs_file`]); the resulting `module.exports` is rooted under the resolved absolute path
///   and returned. A cache hit returns the identical rooted object (CJS singleton semantics).
///
/// [`HostState::module_cache`]: crate::node::core::HostState::module_cache
pub(crate) fn require_specifier<'gc>(
    agent: &mut Agent,
    ctx: &NodeCtx,
    referrer: Option<&std::path::Path>,
    specifier: &str,
    mut gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    // `ctx` borrows the `HostState`, which lives in the JsRuntime's box at a stable address —
    // *independent* of the `agent` borrow (see `require`'s call site and `core::extend_lifetime`).
    // That decoupling is what lets `ctx` (read access to resolver + caches) coexist with `&mut Agent`
    // (engine heap) without aliasing: they reference disjoint memory, and the caches are `RefCell`-
    // guarded for interior mutability. No phasing, no held-borrow conflict, no `unsafe` here.
    let action = resolve_and_load(ctx, referrer, specifier)?;

    match action {
        LoadAction::Builtin { specifier: canonical, install } => {
            // Cache hit: resolve the rooted handle to a live object in the current scope. `Global` is
            // not `Clone`, so we read it through the `RefCell` borrow directly — sound because the
            // borrow is over the cache (`&HostState`), disjoint from the `&mut Agent` `get` needs.
            {
                let cache = ctx.builtin_cache().borrow();
                if let Some(handle) = cache.get(canonical) {
                    return Ok(handle.get(agent, gc.nogc()));
                }
            }

            // Miss: build the exports object exactly once via the module's lazy `install`, then root
            // it for the cache and hand back a live handle bound to the caller's scope.
            let exports = install(agent, ctx, gc.reborrow())?.unbind();
            let rooted: Global<Object<'static>> = Global::new(agent, exports);
            let live = rooted.get(agent, gc.nogc());
            ctx.builtin_cache().borrow_mut().insert(canonical, rooted);
            Ok(live)
        }
        LoadAction::File { path, source, .. } => {
            // Cache hit: the file was already loaded; return its rooted `module.exports`. Read the
            // handle through the `RefCell` borrow (disjoint from `&mut Agent`); `Global` is not
            // `Clone`, so resolve it to a live object in the current scope.
            {
                let cache = ctx.module_cache().borrow();
                if let Some(handle) = cache.get(&path) {
                    return Ok(handle.get(agent, gc.nogc()));
                }
            }

            // Miss: evaluate the file as a CommonJS module. `eval_cjs_file` pre-roots the fresh
            // `exports` object into the module cache (keyed by `path`) *before* running the body, so
            // a cyclic require resolves to the partial exports instead of recursing; on completion it
            // updates the cache to the final `module.exports` and returns it.
            eval_cjs_file(agent, ctx, &path, &source, gc)
        }
    }
}

// ---------------------------------------------------------------------------------------------
// User-file CommonJS evaluation.
// ---------------------------------------------------------------------------------------------

/// Monotonic id source for the per-load temporary global that hands the pre-built `module` object to
/// the wrapper script. A `u64` counter never realistically wraps within a process; even if it did,
/// the slot is created and `delete`d within a single synchronous `require`, so reuse is harmless.
static CJS_LOAD_ID: AtomicU64 = AtomicU64::new(0);

/// Evaluate an already-lowered-to-JS file `source` as a CommonJS module rooted at `path`.
///
/// The engine half of user-file `require`, built on the same public Nova operations the crate uses
/// to run any script ([`parse_script`] + [`script_evaluation`]) — no visibility hack, no `unsafe`.
///
/// Mechanics, and why each step is shaped this way:
///
/// 1. Build the module locals in Rust: a `module` object whose `exports` is a fresh empty object.
///    The empty `exports` is rooted into [`crate::node::core::HostState::module_cache`] under `path`
///    **before** evaluation — Node's cycle-safety contract: a `require` of this same file reached
///    while the body is still running returns the partially-populated exports rather than recursing.
/// 2. Hand `module` to the wrapper by parking it on the realm global under a process-unique,
///    non-enumerable key (`__treaty_cjs_load_<id>`). Passing it through a global is what lets the
///    body be a plain [`script_evaluation`] (which runs in global scope) without needing the
///    `pub(crate)` `call_function`. The wrapper `delete`s the key before returning, so the global is
///    left exactly as it was.
/// 3. The wrapper is an IIFE that closes the five CommonJS locals over the body and whose trailing
///    expression is `module.exports`, so [`script_evaluation`]'s completion value *is* the module's
///    exports — read back directly with no `get_global_object`.
/// 4. On success, update the cache entry to the final `module.exports` (the body may reassign
///    `module.exports = ...`) and return it. A thrown error during evaluation removes the speculative
///    cache entry so a later `require` can retry, and surfaces as [`InstallError::Nova`].
fn eval_cjs_file<'gc>(
    agent: &mut Agent,
    ctx: &NodeCtx,
    path: &Path,
    source: &str,
    mut gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    let id = CJS_LOAD_ID.fetch_add(1, Ordering::Relaxed);
    let slot = format!("__treaty_cjs_load_{id}");

    // --- 1. Build `module = { exports: {} }` and pre-root the empty `exports` (cycle safety). ---
    let exports = OrdinaryObject::create_empty_object(agent, gc.nogc());
    let module = OrdinaryObject::create_empty_object(agent, gc.nogc());
    define_value(agent, module, "exports", exports.into(), gc.nogc());

    let exports_obj: Object = exports.into();
    let pre_rooted: Global<Object<'static>> = Global::new(agent, exports_obj.unbind());
    // A second root for the same initial exports, kept on the Rust stack across evaluation. Reused as
    // the fallback when the body replaces `module.exports` with a non-object primitive, and as a
    // GC-safe handle to the object (the bare `exports`/`module` locals are bound to a pre-evaluation
    // `nogc` scope and must not be read after `script_evaluation`, which may move the heap).
    let initial_exports: Global<Object<'static>> =
        Global::new(agent, Object::from(exports).unbind());
    ctx.module_cache()
        .borrow_mut()
        .insert(path.to_path_buf(), pre_rooted);

    // --- 2. Park `module` on the realm global under the unique slot key. ---
    let global = agent.current_realm(gc.nogc()).global_object(agent);
    define_global_slot(agent, global, &slot, module.into(), gc.nogc());

    // --- 3. Build + evaluate the wrapper; its completion value is `module.exports`. ---
    let dir = path.parent().unwrap_or_else(|| Path::new(""));
    let wrapper = build_cjs_wrapper(&slot, source, path, dir);

    let result = run_script(agent, wrapper, gc.reborrow());

    match result {
        Ok(value) => {
            // The completion value is `module.exports`. It must be an object for the require contract
            // (CJS exports is always an object); a non-object would mean the body replaced
            // `module.exports` with a primitive, which `require` returns as-is — but our cache stores
            // `Object`, so fall back to the (already-rooted) initial exports for the rare primitive
            // case rather than failing the whole require.
            let exports_obj = match Object::try_from(value.unbind().bind(gc.nogc())) {
                Ok(obj) => obj.unbind(),
                // Body replaced `module.exports` with a primitive: `require` would return that
                // primitive, but the cache stores an `Object`, so fall back to the (rooted) initial
                // exports — read from its GC-safe handle, never the stale pre-eval local.
                Err(_) => initial_exports.get(agent, gc.nogc()).unbind(),
            };
            // Update the cache to the final exports (the body may have reassigned `module.exports`).
            let rooted: Global<Object<'static>> = Global::new(agent, exports_obj);
            let live = rooted.get(agent, gc.nogc());
            ctx.module_cache()
                .borrow_mut()
                .insert(path.to_path_buf(), rooted);
            Ok(live)
        }
        Err(message) => {
            // Evaluation failed: drop the speculative cache entry so a later require can retry, and
            // surface the thrown message.
            ctx.module_cache().borrow_mut().remove(path);
            Err(InstallError::Nova(message))
        }
    }
}

/// Define `value` on `global` as a non-enumerable, configurable data property named `name`.
///
/// Used to hand the per-load `module` object to the wrapper script. Non-enumerable so it never shows
/// up in `Object.keys(globalThis)` during the (brief) window before the wrapper deletes it;
/// configurable so the wrapper's `delete globalThis[name]` succeeds.
fn define_global_slot(
    agent: &mut Agent,
    global: Object,
    name: &str,
    value: Object,
    gc: NoGcScope,
) {
    let key = PropertyKey::from_str(agent, name, gc);
    let descriptor = PropertyDescriptor {
        value: Some(value.into()),
        writable: Some(true),
        enumerable: Some(false),
        configurable: Some(true),
        ..Default::default()
    };
    unwrap_try(global.try_define_own_property(agent, key, descriptor, None, gc));
}

/// Parse + evaluate `source` as a strict-mode script in the current realm, returning its completion
/// value, or the thrown value's string form on an abrupt completion.
///
/// Mirrors the parse→evaluate path in [`crate::JsRuntime::eval_with_input`] so user-file CJS runs
/// through exactly the same engine entry points the rest of the crate uses.
fn run_script<'gc>(
    agent: &mut Agent,
    source: String,
    mut gc: GcScope<'gc, '_>,
) -> Result<Value<'gc>, String> {
    let source_text = JsString::from_string(agent, source, gc.nogc());
    let realm = agent.current_realm(gc.nogc());
    let script = match parse_script(agent, source_text, realm, true, None, gc.nogc()) {
        Ok(script) => script,
        Err(diagnostics) => {
            let message = diagnostics
                .iter()
                .map(|d| d.to_string())
                .collect::<Vec<_>>()
                .join("; ");
            return Err(if message.is_empty() {
                "failed to parse module".to_owned()
            } else {
                message
            });
        }
    };

    match script_evaluation(agent, script.unbind(), gc.reborrow()) {
        Ok(value) => Ok(value.unbind().bind(gc.into_nogc())),
        Err(error) => {
            let message = error
                .value()
                .unbind()
                .string_repr(agent, gc)
                .to_string_lossy(agent)
                .into_owned();
            Err(message)
        }
    }
}

/// Build the CommonJS wrapper script for `source`.
///
/// The wrapper reads the pre-built `module` object from the global `slot`, closes the five module
/// locals over the body via an inner IIFE, deletes the slot, and ends with `module.exports` so the
/// script's completion value is the module's exports. `__filename`/`__dirname` are embedded as
/// JS-string literals.
fn build_cjs_wrapper(slot: &str, source: &str, file: &Path, dir: &Path) -> String {
    let filename_lit = encode_js_string_literal(&file.to_string_lossy());
    let dirname_lit = encode_js_string_literal(&dir.to_string_lossy());
    // The inner IIFE provides the body's `module`/`exports`/`require`/`__filename`/`__dirname`
    // bindings; the outer IIFE owns the slot read + cleanup and yields `module.exports`.
    format!(
        "(function () {{\n\
           var module = globalThis.{slot};\n\
           delete globalThis.{slot};\n\
           var exports = module.exports;\n\
           (function (module, exports, require, __filename, __dirname) {{\n{source}\n}})\
             (module, module.exports, require, {filename_lit}, {dirname_lit});\n\
           return module.exports;\n\
         }})()"
    )
}

/// Encode `s` as a double-quoted JavaScript string literal.
///
/// Embeds `__filename`/`__dirname` (and any path bytes) safely into the wrapper source. Mirrors the
/// escaping in [`crate::node::module_resolver`]: the JS-significant characters plus the C0 controls
/// and the two line/paragraph separators that would otherwise terminate a string literal.
fn encode_js_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------------------------------------
// JS-facing `require` global.
// ---------------------------------------------------------------------------------------------

/// The Rust-backed `require(specifier)` exposed to JavaScript.
///
/// Reads argument 0 as a string (throwing `TypeError` for a non-string, matching Node), recovers the
/// runtime's [`NodeCtx`], and delegates to [`require_specifier`]. A resolution/build failure is
/// surfaced as a thrown JS `Error` carrying the underlying message, so user code can `try/catch` it.
///
/// `referrer` is `None`: `require` called from a top-level script / `eval` resolves relative to the
/// runtime CWD, which is correct for the macro and server-fn entry contexts. A nested `require` from
/// within a loaded user file still resolves against the runtime CWD (or an absolute/builtin
/// specifier); threading each module's own path through as the referrer for relative re-resolution
/// is a follow-up that does not affect CWD-relative, absolute, or `node:` requires.
pub(crate) fn require<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    mut gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    // Argument validation first (Node throws `TypeError` for a non-string specifier). Own the
    // specifier (a small `String`) so it does not borrow Nova heap state across the `&mut Agent`
    // resolution + build below.
    let specifier: String = match JsString::try_from(args.get(0)) {
        Ok(s) => s.to_string_lossy(agent).into_owned(),
        Err(_) => {
            return Err(agent.throw_exception_with_static_message(
                ExceptionType::TypeError,
                "The \"id\" argument must be of type string",
                gc.into_nogc(),
            ));
        }
    };

    // Recover a `HostState` borrow that is *decoupled* from the `&mut Agent` we need for engine work.
    // `core::host_state` returns `&HostState` tied to `&Agent`; that borrow alone cannot coexist with
    // the `&mut Agent` that `require_specifier` -> `install` requires. The fix is the one already used
    // (and justified) at the Nova FFI boundary in `core`: the `HostState` is owned by the JsRuntime's
    // `Box<HostState>` at a stable address and outlives this entire `run_in_realm` call, so extending
    // the borrow's lifetime to decouple it from the agent is sound. The `&HostState` (resolver + the
    // `RefCell` caches) and the `&mut Agent` (the Nova heap) reference disjoint memory.
    //
    // SAFETY: see `core::extend_lifetime`. The produced reference is used only for the duration of
    // this call (strictly shorter than the owning box's lifetime), and `HostState`'s own state is
    // mutated only through its `RefCell` interior mutability — never aliased mutably with `&mut Agent`.
    let state = match crate::node::core::host_state(agent) {
        Some(state) => unsafe { crate::node::core::extend_lifetime(state) },
        None => {
            return Err(agent.throw_exception_with_static_message(
                ExceptionType::Error,
                "require() is unavailable: the Node compatibility layer is not installed",
                gc.into_nogc(),
            ));
        }
    };
    let ctx = NodeCtx::new(state);

    // Drive the core under a reborrow so `gc` is still available to throw on the error path. On
    // success, rebind the returned object to the caller's `'gc` scope before converting to a `Value`.
    match require_specifier(agent, &ctx, None, &specifier, gc.reborrow()) {
        Ok(exports) => Ok(exports.unbind().bind(gc.into_nogc()).into()),
        Err(err) => {
            let message = err.to_string();
            Err(agent.throw_exception(ExceptionType::Error, message, gc.into_nogc()))
        }
    }
}

/// Install `require` as a data property on the realm global object.
///
/// The uniform "wire me into the global" seam the scaffold's `globals` layer calls. `require` is an
/// eager global in Node (CJS code assumes it exists without importing it), but the function object
/// itself is tiny — a single builtin-function allocation — and, crucially, materializes **no**
/// module: every `node:` builtin stays lazy until the first `require("node:...")` actually runs
/// (tenet 2).
pub(crate) fn install_require(agent: &mut Agent, global: Object, gc: NoGcScope) {
    let function = create_builtin_function(
        agent,
        Behaviour::Regular(require),
        BuiltinFunctionArgs::new(REQUIRE_ARITY, "require"),
        gc,
    );
    let key = PropertyKey::from_static_str(agent, "require", gc);
    // Define `require` directly on the realm global as a plain data property (the same path the
    // shared `globals::install_globals` uses for `global`), so reads are a direct slot lookup.
    unwrap_try(global.try_define_own_property(
        agent,
        key,
        PropertyDescriptor::new_data_descriptor(function),
        None,
        gc,
    ));
}

/// Uniform per-module entry. The CJS bridge is a service, not an importable object, so this returns
/// an empty exports object — the registry seam stays uniform with the leaf builtins, but `module_cjs`
/// is intentionally absent from `BUILTINS` (you do not `require("module_cjs")`).
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    let obj = OrdinaryObject::create_empty_object(agent, gc.into_nogc());
    Ok(obj.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::core::{extend_lifetime, EnvMap, HostState};
    use nova_vm::ecmascript::{AgentOptions, GcAgent};

    /// Stand up a live Nova agent with the Node `HostState` installed, run `body` inside its realm,
    /// and tear it down. Mirrors `JsRuntime::with_node_compat` but kept local so this module's core
    /// (`require_specifier`) is exercised against a real engine without depending on the public
    /// `JsRuntime` surface. The single `unsafe` is the shared, documented Nova FFI lifetime
    /// extension from `core` — identical to the production path and dropped in the same order
    /// (agent before host state).
    fn with_agent<R>(
        body: impl for<'a, 'b> FnOnce(&mut Agent, &HostState, GcScope<'a, 'b>) -> R,
    ) -> R {
        let cwd = std::env::current_dir().unwrap();
        let host_state = Box::new(HostState::new(cwd, EnvMap::new()));
        // SAFETY: the agent below is dropped before `host_state` (declared after it here), so it
        // never observes a freed `HostState`. This is the same FFI-boundary `unsafe` documented on
        // `core::extend_lifetime`; no leaf logic depends on it.
        let hooks: &'static HostState = unsafe { extend_lifetime(&*host_state) };
        let mut agent = GcAgent::new(
            AgentOptions {
                disable_gc: false,
                print_internals: false,
                no_block: false,
            },
            hooks,
        );
        let init: Option<fn(&mut Agent, Object, GcScope)> = Some(crate::node::install);
        let create_obj: Option<for<'a> fn(&mut Agent, GcScope<'a, '_>) -> Object<'a>> = None;
        let create_this: Option<for<'a> fn(&mut Agent, GcScope<'a, '_>) -> Object<'a>> = None;
        let realm = agent.create_realm(create_obj, create_this, init);
        let out = agent.run_in_realm(&realm, |agent, gc| {
            let state = crate::node::core::host_state(agent).unwrap();
            // Re-derive the borrow off the agent for the closure (same reborrow shape as `with_ctx`).
            let state: &HostState = unsafe { extend_lifetime(state) };
            body(agent, state, gc)
        });
        drop(agent);
        drop(host_state);
        out
    }

    #[test]
    fn require_builtin_returns_an_object() {
        with_agent(|agent, state, mut gc| {
            let ctx = NodeCtx::new(state);
            let exports =
                require_specifier(agent, &ctx, None, "node:path", gc.reborrow()).unwrap();
            // It is an object (a module exports namespace), not undefined/null.
            let _: Object = exports;
        });
    }

    #[test]
    fn require_builtin_is_cached_and_identity_stable() {
        with_agent(|agent, state, mut gc| {
            let ctx = NodeCtx::new(state);
            assert!(
                ctx.builtin_cache().borrow().is_empty(),
                "cache starts empty (nothing required yet)"
            );

            // Unbind each result immediately so it no longer borrows the per-call `gc.reborrow()`,
            // letting us compare object identity across two `require` calls.
            let first = require_specifier(agent, &ctx, None, "path", gc.reborrow())
                .unwrap()
                .unbind();
            assert_eq!(
                ctx.builtin_cache().borrow().len(),
                1,
                "first require materializes + caches exactly one builtin"
            );

            let second = require_specifier(agent, &ctx, None, "node:path", gc.reborrow())
                .unwrap()
                .unbind();
            assert_eq!(
                ctx.builtin_cache().borrow().len(),
                1,
                "second require (node:-prefixed, same module) is a cache hit, not a new entry"
            );

            // Module identity: repeated require yields the same underlying object (CJS singleton).
            assert_eq!(
                first, second,
                "require returns the identical cached exports object"
            );
        });
    }

    #[test]
    fn require_distinct_builtins_make_distinct_cache_entries() {
        with_agent(|agent, state, mut gc| {
            let ctx = NodeCtx::new(state);
            let _ = require_specifier(agent, &ctx, None, "path", gc.reborrow()).unwrap();
            let _ = require_specifier(agent, &ctx, None, "os", gc.reborrow()).unwrap();
            assert_eq!(
                ctx.builtin_cache().borrow().len(),
                2,
                "two different builtins occupy two cache slots"
            );
        });
    }

    #[test]
    fn require_unknown_specifier_is_a_resolve_error() {
        with_agent(|agent, state, mut gc| {
            let ctx = NodeCtx::new(state);
            let err = require_specifier(
                agent,
                &ctx,
                None,
                "this-package-does-not-exist-xyz",
                gc.reborrow(),
            )
            .unwrap_err();
            assert!(
                matches!(err, InstallError::Resolve(_)),
                "a bare specifier with no matching package/builtin must be a resolve error: {err:?}"
            );
        });
    }

    #[test]
    fn require_user_file_loads_and_is_cached_with_stable_identity() {
        // A relative user file is resolved, read, evaluated as a CommonJS module, and its
        // `module.exports` object is returned + cached: a second require yields the identical object
        // (CJS singleton), and exactly one module-cache slot is occupied.
        let dir = std::env::temp_dir().join(format!("treaty-cjs-load-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("mod.js");
        // Export an object so the result is a cacheable `Object` and we can assert identity.
        std::fs::write(&file, "module.exports = { value: 42 };\n").unwrap();

        let cwd = dir.clone();
        let host_state = Box::new(HostState::new(cwd, EnvMap::new()));
        // SAFETY: agent dropped before host_state (declared after); same documented FFI boundary.
        let hooks: &'static HostState = unsafe { extend_lifetime(&*host_state) };
        let mut agent = GcAgent::new(AgentOptions::default(), hooks);
        let init: Option<fn(&mut Agent, Object, GcScope)> = Some(crate::node::install);
        let create_obj: Option<for<'a> fn(&mut Agent, GcScope<'a, '_>) -> Object<'a>> = None;
        let create_this: Option<for<'a> fn(&mut Agent, GcScope<'a, '_>) -> Object<'a>> = None;
        let realm = agent.create_realm(create_obj, create_this, init);
        agent.run_in_realm(&realm, |agent, mut gc| {
            let state = crate::node::core::host_state(agent).unwrap();
            let state: &HostState = unsafe { extend_lifetime(state) };
            let ctx = NodeCtx::new(state);

            assert!(
                ctx.module_cache().borrow().is_empty(),
                "module cache starts empty"
            );

            let first = require_specifier(agent, &ctx, None, "./mod.js", gc.reborrow())
                .expect("user file should load as a CommonJS module")
                .unbind();
            assert_eq!(
                ctx.module_cache().borrow().len(),
                1,
                "first require evaluates + caches exactly one user module"
            );

            let second = require_specifier(agent, &ctx, None, "./mod.js", gc.reborrow())
                .expect("second require should hit the cache")
                .unbind();
            assert_eq!(
                ctx.module_cache().borrow().len(),
                1,
                "second require of the same file is a cache hit, not a new entry"
            );

            // Module identity: repeated require yields the same underlying exports object.
            assert_eq!(
                first, second,
                "require returns the identical cached exports object"
            );
        });
        drop(agent);
        drop(host_state);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn require_user_file_works_end_to_end_through_eval() {
        // The full public path: `require()` of an absolute-path user `.js` file from an `eval`ed
        // script reads, evaluates as CommonJS, and exposes its `module.exports` (here exercising the
        // `module`/`exports`/`__filename` locals the wrapper supplies).
        use crate::JsRuntime;
        use serde_json::json;

        let dir = std::env::temp_dir().join(format!("treaty-cjs-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("dep.js");
        std::fs::write(
            &file,
            "const sum = (a, b) => a + b;\n\
             module.exports = { sum, name: __filename.length > 0 };\n",
        )
        .unwrap();

        // `serde_json` of the path yields a valid JS string literal (escapes Windows backslashes).
        let lit = serde_json::to_string(&file.to_string_lossy().into_owned()).unwrap();
        let src = format!(
            "const dep = require({lit});\
             [dep.sum(2, 3), dep.name]"
        );

        let mut rt = JsRuntime::with_node_compat();
        assert_eq!(rt.eval(&src).unwrap(), json!([5, true]));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
