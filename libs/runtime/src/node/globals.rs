//! Eager + lazy global installation, and the one shared helper every module reuses to define a
//! function on an object.
//!
//! This is the single place that wires globals into a realm. It is called once from
//! [`crate::JsRuntime::with_node_compat`] via Nova's `initialize_global_object` realm hook. Globals
//! split two ways (tenet 2 — lazy init):
//!
//! * **Eager** — installed immediately because Node/WinterCG code assumes they exist without an
//!   `import`. The ones that need **no host services** and so can be materialized with only
//!   `&mut Agent` + a [`GcScope`] are installed here directly: the `globalThis`/`global`/`self`
//!   self-references. The eager globals that wrap a per-module body (`process`, `Buffer`, `console`,
//!   the timer functions, `TextEncoder`/`TextDecoder`, `structuredClone`) are surfaced through the
//!   same [`define_value`]/[`define_fn`] seam by their owning modules; see the module note below for
//!   why those are not force-built from inside this realm-init hook.
//! * **Lazy** — installed as a *self-replacing accessor property* whose getter materializes the real
//!   value on first read and then redefines itself as a plain data property, so the second and later
//!   reads cost a normal property lookup and the value is built **at most once**. Until first touch a
//!   lazy global costs only a property descriptor — no object, no function, no Nova handle (tenet 2).
//!   The WinterCG `self` global is wired this way as the worked example and conformance anchor; the
//!   same [`define_lazy`] seam wires the rarely-touched `URL`/`URLSearchParams`/`fetch`/`Headers`/
//!   `Request`/`Response` globals to their module builders.
//!
//! ## Why module-backed eager globals are wired by their modules, not force-built here
//!
//! A module's exports object is produced by its `install(agent, &NodeCtx, gc)`, and a [`NodeCtx`]
//! borrows the [`HostState`] out of `agent.get_host_data()`. Building one therefore needs an
//! immutable borrow of the agent (for the ctx) *and* a mutable borrow (to allocate the object) at the
//! same time. That is sound only where the caller already holds the `HostState` **separately** from
//! the agent — which the module loader/registry does (the `HostState` lives in a box on
//! [`crate::JsRuntime`], distinct from the agent), but the `initialize_global_object` hook does not:
//! it is handed only `&mut Agent`. Synthesising a `&HostState` from the agent here would require
//! `unsafe` outside the one documented Nova FFI boundary, which the architecture forbids. So this hook
//! installs only the host-service-free self-references eagerly and exposes the [`define_*`]/
//! [`define_lazy`] seams; the host-service-backed globals are attached by the registry path that owns
//! a `HostState` borrow. This keeps the unused-module-cost-zero invariant intact and the hook
//! allocation-free beyond the three self-reference descriptors.

use nova_vm::ecmascript::{
    Agent, ArgumentsList, Behaviour, BuiltinFunctionArgs, ExceptionType, InternalMethods, JsResult,
    Object, OrdinaryObject, PropertyDescriptor, PropertyKey, RegularFn, String as JsString, Value,
    create_builtin_function, parse_script, script_evaluation, unwrap_try,
};
use nova_vm::engine::{Bindable, GcScope, NoGcScope};

use crate::node::core::{HostState, InstallError, NodeCtx};

/// Define a Rust-backed function as a data property `name` (arity `len`) on `obj`.
///
/// Lifted from the verified Nova CLI `create_obj_func` (`nova_cli/src/lib/globals.rs`). Every leaf
/// module reuses this to build its exports object, so the function-definition pattern lives in one
/// place. Uses [`PropertyKey::from_static_str`] to intern the (always `&'static`) key without a heap
/// string (tenet 3 — minimize allocation).
pub(crate) fn define_fn(
    agent: &mut Agent,
    obj: OrdinaryObject,
    name: &'static str,
    f: RegularFn,
    len: u32,
    gc: NoGcScope,
) {
    let function = create_builtin_function(
        agent,
        Behaviour::Regular(f),
        BuiltinFunctionArgs::new(len, name),
        gc,
    );
    let key = PropertyKey::from_static_str(agent, name, gc);
    unwrap_try(obj.try_define_own_property(
        agent,
        key,
        PropertyDescriptor::new_data_descriptor(function),
        None,
        gc,
    ));
}

/// Define `value` as a data property `name` on `obj`. The shared helper for installing a module's
/// exports object (or a sub-namespace) onto a parent object / the global.
pub(crate) fn define_value(
    agent: &mut Agent,
    obj: OrdinaryObject,
    name: &'static str,
    value: Object,
    gc: NoGcScope,
) {
    let key = PropertyKey::from_static_str(agent, name, gc);
    unwrap_try(obj.try_define_own_property(
        agent,
        key,
        PropertyDescriptor::new_data_descriptor(value),
        None,
        gc,
    ));
}

/// Install a **self-replacing lazy accessor** named `name` on the realm's `global` object.
///
/// `getter` is a plain builtin function (no captured state — Nova builtins are bare fn pointers) that
/// materializes the global's value the first time the property is read. By convention every such
/// getter ends by [`redefine_as_data`]-ing `name` on its `this` (the global) to the value it returns,
/// which atomically replaces this accessor with a normal writable/configurable data property. The
/// net effect:
///
/// * **Before first read** the global costs exactly one property descriptor — no object, no function
///   body invoked, no Nova heap handle (tenet 2 — unused modules cost zero startup memory/time).
/// * **First read** runs `getter` once, which builds the value and redefines the slot.
/// * **Later reads** hit the plain data property; the getter is never called again, so the value is
///   built at most once.
///
/// The accessor is installed non-enumerable (matching how host globals like `URL` present) and
/// configurable (so the getter's self-redefinition is permitted, and so user code may still override
/// it).
pub(crate) fn define_lazy(
    agent: &mut Agent,
    global: Object,
    name: &'static str,
    getter: RegularFn,
    gc: NoGcScope,
) {
    let function = create_builtin_function(
        agent,
        Behaviour::Regular(getter),
        // A getter takes no arguments; name it `get <name>` per the accessor naming convention.
        BuiltinFunctionArgs::new(0, name),
        gc,
    );
    let key = PropertyKey::from_static_str(agent, name, gc);
    let descriptor = PropertyDescriptor {
        get: Some(Some(function.into())),
        set: None,
        enumerable: Some(false),
        configurable: Some(true),
        ..Default::default()
    };
    unwrap_try(global.try_define_own_property(agent, key, descriptor, None, gc));
}

/// Replace property `name` on `target` with the plain data property `value`.
///
/// This is the second half of the self-replacing-accessor pattern: a lazy getter calls it to swap
/// the accessor slot for the now-materialized value, so subsequent reads bypass the getter entirely.
/// The new property is writable/enumerable/configurable — a normal data property — matching what an
/// eagerly-installed global would have looked like.
pub(crate) fn redefine_as_data(
    agent: &mut Agent,
    target: Object,
    name: &'static str,
    value: Value,
    gc: NoGcScope,
) {
    let key = PropertyKey::from_static_str(agent, name, gc);
    unwrap_try(target.try_define_own_property(
        agent,
        key,
        PropertyDescriptor::new_data_descriptor(value),
        None,
        gc,
    ));
}

/// Coerce the `this` a builtin getter received into the global [`Object`] to redefine itself on.
///
/// Lazy getters installed by [`define_lazy`] live on the global object, so their `this` is that
/// object. This narrows the [`Value`] to an [`Object`], falling back to the realm's current global
/// object if (defensively) `this` is not an object — so the self-redefinition always targets a real
/// object and the getter can never panic.
fn getter_target<'gc>(agent: &mut Agent, this: Value, gc: NoGcScope<'gc, '_>) -> Object<'gc> {
    match Object::try_from(this.bind(gc)) {
        Ok(object) => object,
        Err(_) => agent.current_realm(gc).global_object(agent).bind(gc),
    }
}

/// The WinterCG `self` global: a self-reference to the global object, identical to `globalThis`.
///
/// Installed **lazily** as the worked example of the self-replacing-accessor seam: scripts rarely read
/// bare `self` (most reach for `globalThis`), so it costs only a descriptor until first touched. On
/// first read this getter resolves the global, redefines `self` as a plain data property pointing at
/// it, and returns it; thereafter `self` is an ordinary property and this getter is never re-entered.
fn lazy_self_getter<'gc>(
    agent: &mut Agent,
    this: Value,
    _args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let gc = gc.into_nogc();
    let global = getter_target(agent, this, gc);
    // `self === globalThis === global`. Materialize by pointing `self` at the global and collapse the
    // accessor into a data property so this getter runs exactly once.
    redefine_as_data(agent, global, "self", global.into(), gc);
    Ok(global.into())
}

/// Install the Node-compat globals into `global`.
///
/// Called once per realm from the `initialize_global_object` hook. Installs the host-service-free
/// globals: the `global`/`globalThis`/`self` self-references (the anchors every module and lazy
/// accessor needs), with `self` demonstrating the lazy seam. The host-service-backed eager globals
/// and the module-backed lazy globals are attached through the [`define_value`]/[`define_fn`]/
/// [`define_lazy`] seams by the registry path that owns a `HostState` borrow (see the module note).
///
/// Kept total and panic-free: a realm carrying only the self-references is a valid Node-ish global, so
/// partial builds still run.
pub(crate) fn install_globals(agent: &mut Agent, global: Object, gc: GcScope) {
    let gc = gc.into_nogc();

    // `global` is Node's alias for `globalThis`. Point it at the realm's global object so code that
    // reads `global.X` sees the same object as `globalThis.X`. Eager: every module and lazy getter
    // anchors on it, and it needs no host services to install.
    let key = PropertyKey::from_static_str(agent, "global", gc);
    unwrap_try(global.try_define_own_property(
        agent,
        key,
        PropertyDescriptor::new_data_descriptor(global),
        None,
        gc,
    ));

    // `self` is the WinterCG global self-reference. Installed lazily via the self-replacing-accessor
    // seam as the worked example: zero cost until first read, built at most once.
    define_lazy(agent, global, "self", lazy_self_getter, gc);

    // `require` is the CommonJS module-loading bridge. Node/CJS code assumes it exists without an
    // import, so it is installed eagerly. Crucially it needs **no host services at install time**:
    // the function object is a single builtin allocation, and it recovers the `HostState` (resolver
    // + caches) from the agent only when *called* (see `module_cjs::require`). That is what lets it
    // be wired here, inside the `initialize_global_object` hook that holds only `&mut Agent`, without
    // the `HostState` borrow the module-backed eager globals need. Installing it still materializes
    // **no** module: every `node:` builtin stays lazy until the first `require("node:...")` runs
    // (tenet 2).
    crate::node::module_cjs::install_require(agent, global, gc);
}

/// Install the host-service-backed Node globals that the realm-init hook cannot build.
///
/// This is the second half of global wiring and the fix for the long-standing gap where the
/// always-present Node globals (`process`, the timer functions, `queueMicrotask`, and the WHATWG
/// `URL`/`URLSearchParams`/`TextEncoder`/`TextDecoder`/`fetch` family) never actually materialized:
/// [`install_globals`] runs inside Nova's `initialize_global_object` realm hook, which is handed only
/// `&mut Agent` and therefore cannot build a module's exports (those need a [`NodeCtx`] borrowed out of
/// the [`HostState`], see the module note). This function runs *after* realm creation, from
/// [`crate::JsRuntime::with_node_compat`], where the agent and the boxed [`HostState`] are borrowed
/// **separately** — exactly the decoupled borrow the module path requires.
///
/// Wiring, mirroring Node's eager/lazy split (tenet 2):
///
/// * **Eager** — `process` (built via [`crate::node::process::install`] and bound on the global as a
///   data property) and the timer family + `queueMicrotask` (built via [`crate::node::timers::install`]
///   and [`crate::node::microtask::install`], whose bootstraps publish `setTimeout`/`clearTimeout`/
///   `setInterval`/`clearInterval`/`setImmediate`/`clearImmediate`/`queueMicrotask` onto `globalThis`
///   themselves). Real Node code reads these bare on practically every turn, so paying their (tiny)
///   build cost once at startup is the right trade.
/// * **Lazy** — the WHATWG class globals `URL`, `URLSearchParams`, `TextEncoder`, `TextDecoder`,
///   `Headers`, `Request`, `Response`, and `fetch`. Each is a self-replacing accessor ([`define_lazy`])
///   whose getter JS-bootstraps the class over the native primitives the corresponding leaf module
///   exports (`node:url`, `node:text_encoding`, `node:fetch`), then redefines itself as a data
///   property so the value is built at most once and only if the program touches it.
///
/// Returns the first [`InstallError`] from building `process`/timers/microtask (the eager set, which
/// must succeed for a usable runtime); the lazy accessors cannot fail here since installing an accessor
/// descriptor never runs the getter.
pub(crate) fn install_module_globals(
    agent: &mut Agent,
    state: &HostState,
    mut gc: GcScope,
) -> Result<(), InstallError> {
    let ctx = NodeCtx::new(state);

    // --- Eager: process (bound as a global data property). ---
    let process = crate::node::process::install(agent, &ctx, gc.reborrow())?.unbind();
    {
        let nogc = gc.nogc();
        let global = agent.current_realm(nogc).global_object(agent);
        define_value_on(agent, global, "process", process.bind(nogc), nogc);
    }

    // --- Eager: timers + queueMicrotask (the bootstraps self-publish onto globalThis). ---
    // Each returns its exports object (ignored here); the side effect — defining the timer functions
    // and `queueMicrotask` as globals — is what we want. Idempotent if later `require`d (they reuse
    // the registry/global they parked).
    crate::node::timers::install(agent, &ctx, gc.reborrow())?;
    crate::node::microtask::install(agent, &ctx, gc.reborrow())?;

    // --- Lazy: the WHATWG class globals, each a self-replacing accessor. ---
    let nogc = gc.into_nogc();
    let global = agent.current_realm(nogc).global_object(agent);
    define_lazy(agent, global, "URL", lazy_url_getter, nogc);
    define_lazy(agent, global, "URLSearchParams", lazy_url_search_params_getter, nogc);
    define_lazy(agent, global, "TextEncoder", lazy_text_encoder_getter, nogc);
    define_lazy(agent, global, "TextDecoder", lazy_text_decoder_getter, nogc);
    define_lazy(agent, global, "Headers", lazy_headers_getter, nogc);
    define_lazy(agent, global, "Request", lazy_request_getter, nogc);
    define_lazy(agent, global, "Response", lazy_response_getter, nogc);
    define_lazy(agent, global, "fetch", lazy_fetch_getter, nogc);

    Ok(())
}

/// `define_value` for an arbitrary [`Object`] target (the shared helper takes an [`OrdinaryObject`]).
///
/// The realm global is an [`Object`], so installing `process` onto it needs this generic-target form;
/// it is otherwise identical to [`define_value`] (a writable/enumerable/configurable data property).
fn define_value_on(
    agent: &mut Agent,
    target: Object,
    name: &'static str,
    value: Object,
    gc: NoGcScope,
) {
    let key = PropertyKey::from_static_str(agent, name, gc);
    unwrap_try(target.try_define_own_property(
        agent,
        key,
        PropertyDescriptor::new_data_descriptor(value),
        None,
        gc,
    ));
}

// =================================================================================================
// Lazy WHATWG-class global getters.
//
// Each getter materializes its class(es) by evaluating a small JS bootstrap over the native
// primitives the corresponding leaf module exports (reached via the global `require`, which is
// installed eagerly in `install_globals`). On success it redefines the global name(s) it owns as
// plain data properties (collapsing the accessor so the build runs at most once) and returns the
// requested constructor. The bootstrap source is a single `&'static str` per family (tenet 3).
//
// `URL`/`URLSearchParams` share one bootstrap (they are mutually referential — `url.searchParams`
// is a `URLSearchParams`), so either getter builds both and redefines both; the second-touched
// global then already finds itself a data property and never re-enters its getter.
// =================================================================================================

/// Evaluate `bootstrap` (an IIFE) in the current realm and return its completion value.
///
/// Used by the lazy class getters: the bootstrap closes over `require` (a global) to pull native
/// primitives, defines the class(es), publishes any sibling globals it owns, and ends with the
/// constructor the getter must return. A parse/eval failure surfaces as a thrown JS exception so the
/// triggering `typeof URL` / `new URL(...)` site sees a real error rather than a panic.
fn eval_bootstrap<'gc>(
    agent: &mut Agent,
    bootstrap: &'static str,
    mut gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let source = JsString::from_static_str(agent, bootstrap, gc.nogc());
    let realm = agent.current_realm(gc.nogc());
    let script = match parse_script(agent, source, realm, true, None, gc.nogc()) {
        Ok(script) => script,
        Err(diags) => {
            let msg = diags
                .iter()
                .map(|d| d.to_string())
                .collect::<Vec<_>>()
                .join("; ");
            return Err(agent.throw_exception(
                ExceptionType::Error,
                if msg.is_empty() {
                    "failed to parse global bootstrap".to_owned()
                } else {
                    msg
                },
                gc.into_nogc(),
            ));
        }
    };
    script_evaluation(agent, script.unbind(), gc.reborrow())
        .unbind()
        .map(|v| v.bind(gc.into_nogc()))
}

/// The shared `URL` + `URLSearchParams` bootstrap.
///
/// Builds both classes over `require("node:url").parse`/`.format` (the functional core the `url`
/// module already implements) and publishes both as globals. The completion value is `URL`.
const URL_BOOTSTRAP: &str = r##"
(function () {
  var nodeUrl = globalThis.__treaty_native_module;

  function decode(s) { try { return decodeURIComponent(s.replace(/\+/g, " ")); } catch (e) { return s; } }
  function encode(s) { return encodeURIComponent(s); }

  function parseQuery(init, sp) {
    sp._list = [];
    if (init == null || init === "") return;
    if (typeof init === "string") {
      var q = init.charAt(0) === "?" ? init.slice(1) : init;
      if (q === "") return;
      var pairs = q.split("&");
      for (var i = 0; i < pairs.length; i++) {
        if (pairs[i] === "") continue;
        var eq = pairs[i].indexOf("=");
        var k = eq < 0 ? pairs[i] : pairs[i].slice(0, eq);
        var v = eq < 0 ? "" : pairs[i].slice(eq + 1);
        sp._list.push([decode(k), decode(v)]);
      }
    } else if (typeof init.forEach === "function" && typeof init !== "string") {
      // Map / another URLSearchParams / array of pairs.
      if (Array.isArray(init)) {
        for (var j = 0; j < init.length; j++) { sp._list.push([String(init[j][0]), String(init[j][1])]); }
      } else {
        init.forEach(function (val, key) { sp._list.push([String(key), String(val)]); });
      }
    } else {
      var keys = Object.keys(init);
      for (var k2 = 0; k2 < keys.length; k2++) { sp._list.push([keys[k2], String(init[keys[k2]])]); }
    }
  }

  function URLSearchParams(init) {
    if (!(this instanceof URLSearchParams)) { return new URLSearchParams(init); }
    this._list = [];
    this._url = null; // back-reference so mutations re-serialize the owning URL
    parseQuery(init, this);
  }
  function sync(sp) { if (sp._url) { sp._url._search = sp.toString(); } }
  URLSearchParams.prototype.append = function (k, v) { this._list.push([String(k), String(v)]); sync(this); };
  URLSearchParams.prototype.delete = function (k) { k = String(k); this._list = this._list.filter(function (p) { return p[0] !== k; }); sync(this); };
  URLSearchParams.prototype.get = function (k) { k = String(k); for (var i = 0; i < this._list.length; i++) { if (this._list[i][0] === k) return this._list[i][1]; } return null; };
  URLSearchParams.prototype.getAll = function (k) { k = String(k); var o = []; for (var i = 0; i < this._list.length; i++) { if (this._list[i][0] === k) o.push(this._list[i][1]); } return o; };
  URLSearchParams.prototype.has = function (k) { k = String(k); for (var i = 0; i < this._list.length; i++) { if (this._list[i][0] === k) return true; } return false; };
  URLSearchParams.prototype.set = function (k, v) { k = String(k); v = String(v); var done = false; var o = []; for (var i = 0; i < this._list.length; i++) { if (this._list[i][0] === k) { if (!done) { o.push([k, v]); done = true; } } else { o.push(this._list[i]); } } if (!done) o.push([k, v]); this._list = o; sync(this); };
  URLSearchParams.prototype.forEach = function (cb, thisArg) { for (var i = 0; i < this._list.length; i++) { cb.call(thisArg, this._list[i][1], this._list[i][0], this); } };
  URLSearchParams.prototype.keys = function () { return this._list.map(function (p) { return p[0]; })[Symbol.iterator](); };
  URLSearchParams.prototype.values = function () { return this._list.map(function (p) { return p[1]; })[Symbol.iterator](); };
  URLSearchParams.prototype.entries = function () { return this._list.map(function (p) { return [p[0], p[1]]; })[Symbol.iterator](); };
  URLSearchParams.prototype[Symbol.iterator] = function () { return this.entries(); };
  URLSearchParams.prototype.sort = function () { this._list.sort(function (a, b) { return a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0; }); sync(this); };
  URLSearchParams.prototype.toString = function () { return this._list.map(function (p) { return encode(p[0]) + "=" + encode(p[1]); }).join("&"); };
  Object.defineProperty(URLSearchParams.prototype, "size", { get: function () { return this._list.length; }, configurable: true });

  function URL(input, base) {
    if (!(this instanceof URL)) { return new URL(input, base); }
    input = String(input);
    var resolved = input;
    // Minimal base resolution: if input has no scheme and a base is given, join against the base.
    var parsed = nodeUrl.parse(input);
    if ((!parsed.protocol) && base != null) {
      var b = nodeUrl.parse(String(base));
      if (!b.protocol) { throw new TypeError("Invalid base URL: " + base); }
      var basePath = b.pathname || "/";
      if (input.charAt(0) === "/") {
        resolved = b.protocol + "//" + (b.host || "") + input;
      } else if (input.charAt(0) === "?" || input.charAt(0) === "#") {
        resolved = b.protocol + "//" + (b.host || "") + basePath + input;
      } else {
        var dir = basePath.slice(0, basePath.lastIndexOf("/") + 1);
        resolved = b.protocol + "//" + (b.host || "") + dir + input;
      }
      parsed = nodeUrl.parse(resolved);
    }
    if (!parsed.protocol) { throw new TypeError("Invalid URL: " + input); }
    this._protocol = parsed.protocol || "";
    this._hostname = parsed.hostname || "";
    this._port = parsed.port || "";
    this._pathname = parsed.pathname || (this._hostname ? "/" : "");
    this._search = parsed.search || "";
    this._hash = parsed.hash || "";
    var u = parsed.href || resolved;
    var at = (u.indexOf("@") >= 0 && u.indexOf("//") >= 0) ? u : u;
    this._username = "";
    this._password = "";
    var sp = new URLSearchParams(this._search);
    sp._url = this;
    this._searchParams = sp;
  }
  function host(u) { return u._port ? u._hostname + ":" + u._port : u._hostname; }
  Object.defineProperty(URL.prototype, "protocol", { get: function () { return this._protocol; }, set: function (v) { v = String(v); this._protocol = v.charAt(v.length - 1) === ":" ? v : v + ":"; }, configurable: true });
  Object.defineProperty(URL.prototype, "hostname", { get: function () { return this._hostname; }, set: function (v) { this._hostname = String(v); }, configurable: true });
  Object.defineProperty(URL.prototype, "port", { get: function () { return this._port; }, set: function (v) { this._port = String(v); }, configurable: true });
  Object.defineProperty(URL.prototype, "host", { get: function () { return host(this); }, set: function (v) { v = String(v); var c = v.indexOf(":"); if (c >= 0) { this._hostname = v.slice(0, c); this._port = v.slice(c + 1); } else { this._hostname = v; this._port = ""; } }, configurable: true });
  Object.defineProperty(URL.prototype, "pathname", { get: function () { return this._pathname; }, set: function (v) { v = String(v); this._pathname = v.charAt(0) === "/" || v === "" ? v : "/" + v; }, configurable: true });
  Object.defineProperty(URL.prototype, "search", { get: function () { return this._search; }, set: function (v) { v = String(v); this._search = v === "" ? "" : (v.charAt(0) === "?" ? v : "?" + v); parseQuery(this._search, this._searchParams); }, configurable: true });
  Object.defineProperty(URL.prototype, "hash", { get: function () { return this._hash; }, set: function (v) { v = String(v); this._hash = v === "" ? "" : (v.charAt(0) === "#" ? v : "#" + v); }, configurable: true });
  Object.defineProperty(URL.prototype, "searchParams", { get: function () { return this._searchParams; }, configurable: true });
  Object.defineProperty(URL.prototype, "origin", { get: function () { return this._hostname ? this._protocol + "//" + host(this) : "null"; }, configurable: true });
  Object.defineProperty(URL.prototype, "href", { get: function () {
    var s = this._searchParams && this._searchParams._list.length ? "?" + this._searchParams.toString() : this._search;
    var out = this._protocol;
    if (this._hostname) { out += "//" + host(this); }
    out += this._pathname + (s || "") + this._hash;
    return out;
  }, set: function (v) { URL.call(this, String(v)); }, configurable: true });
  URL.prototype.toString = function () { return this.href; };
  URL.prototype.toJSON = function () { return this.href; };
  return { URL: URL, URLSearchParams: URLSearchParams };
})()
"##;

fn lazy_url_getter<'gc>(
    agent: &mut Agent,
    this: Value,
    _args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    materialize_family(
        agent,
        this,
        URL_BOOTSTRAP,
        &["URL", "URLSearchParams"],
        "URL",
        crate::node::url::install,
        gc,
    )
}

fn lazy_url_search_params_getter<'gc>(
    agent: &mut Agent,
    this: Value,
    _args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    materialize_family(
        agent,
        this,
        URL_BOOTSTRAP,
        &["URL", "URLSearchParams"],
        "URLSearchParams",
        crate::node::url::install,
        gc,
    )
}

/// `TextEncoder` / `TextDecoder` bootstrap over `require("node:text_encoding")`.
///
/// The native module exchanges bytes as a plain integer `Array` (the pinned Nova rev exposes no
/// embedder `Uint8Array` construction — see `text_encoding.rs`); the class wrappers present the
/// WHATWG surface (`.encode` returns a `Uint8Array`, `.decode` accepts one) by converting between an
/// integer array and a `Uint8Array` in JS. Publishes both classes as globals; completion value is the
/// requested constructor name, read back by the caller.
const TEXT_ENCODING_BOOTSTRAP: &str = r#"
(function () {
  var te = globalThis.__treaty_native_module;

  function TextEncoder() {}
  Object.defineProperty(TextEncoder.prototype, "encoding", { get: function () { return "utf-8"; }, configurable: true });
  TextEncoder.prototype.encode = function (input) {
    var arr = te.encode(input === undefined ? "" : String(input));
    return Uint8Array.from(arr);
  };
  TextEncoder.prototype.encodeInto = function (source, dest) {
    var bytes = te.encode(source === undefined ? "" : String(source));
    var n = Math.min(bytes.length, dest.length);
    for (var i = 0; i < n; i++) dest[i] = bytes[i];
    // Approximate read count: with no surrogate splitting we report the chars consumed for the bytes
    // written; for the common all-ASCII / fully-fitting case this matches WHATWG.
    return { read: source ? String(source).length : 0, written: n };
  };

  function TextDecoder(label, options) {
    this._encoding = "utf-8";
    options = options || {};
    this._fatal = !!options.fatal;
    this._ignoreBOM = !!options.ignoreBOM;
  }
  Object.defineProperty(TextDecoder.prototype, "encoding", { get: function () { return this._encoding; }, configurable: true });
  Object.defineProperty(TextDecoder.prototype, "fatal", { get: function () { return this._fatal; }, configurable: true });
  Object.defineProperty(TextDecoder.prototype, "ignoreBOM", { get: function () { return this._ignoreBOM; }, configurable: true });
  TextDecoder.prototype.decode = function (input) {
    var bytes;
    if (input == null) { bytes = []; }
    else if (Array.isArray(input)) { bytes = input; }
    else if (input.buffer !== undefined || input.byteLength !== undefined) {
      var view = input.BYTES_PER_ELEMENT === 1 ? input : new Uint8Array(input.buffer || input);
      bytes = Array.prototype.slice.call(view);
    } else { bytes = Array.prototype.slice.call(input); }
    return te.decode(bytes, { fatal: this._fatal, ignoreBOM: this._ignoreBOM });
  };

  return { TextEncoder: TextEncoder, TextDecoder: TextDecoder };
})()
"#;

fn lazy_text_encoder_getter<'gc>(
    agent: &mut Agent,
    this: Value,
    _args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    materialize_family(
        agent,
        this,
        TEXT_ENCODING_BOOTSTRAP,
        &["TextEncoder", "TextDecoder"],
        "TextEncoder",
        crate::node::text_encoding::install,
        gc,
    )
}

fn lazy_text_decoder_getter<'gc>(
    agent: &mut Agent,
    this: Value,
    _args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    materialize_family(
        agent,
        this,
        TEXT_ENCODING_BOOTSTRAP,
        &["TextEncoder", "TextDecoder"],
        "TextDecoder",
        crate::node::text_encoding::install,
        gc,
    )
}

/// `Headers`/`Request`/`Response`/`fetch` bootstrap over `require("node:fetch")`.
///
/// The fetch module exposes the WHATWG header/method/status primitives over a `[name, value][]` pair
/// array (no network, no internal slots — see `fetch.rs`). The class shells here carry their header
/// list as a plain JS field and delegate validation/combine/sort to those primitives. `fetch()` has no
/// transport in this offline runtime, so it returns a rejected promise with a clear, catchable message
/// (Node layers `fetch` over a transport too); the request/response *shape* is fully usable. Publishes
/// all four globals; completion value unused (the caller reads the requested name back).
const FETCH_BOOTSTRAP: &str = r#"
(function () {
  var F = globalThis.__treaty_native_module;

  function pairsFrom(init) {
    var list = [];
    if (init == null) return list;
    if (Array.isArray(init)) {
      for (var i = 0; i < init.length; i++) { list.push([String(init[i][0]), String(init[i][1])]); }
    } else if (typeof init.forEach === "function") {
      init.forEach(function (v, k) { list.push([String(k), String(v)]); });
    } else if (typeof init === "object") {
      var keys = Object.keys(init);
      for (var j = 0; j < keys.length; j++) { list.push([keys[j], String(init[keys[j]])]); }
    }
    return list;
  }

  function Headers(init) {
    if (!(this instanceof Headers)) return new Headers(init);
    this._list = [];
    var pairs = init instanceof Headers ? init._list.slice() : pairsFrom(init);
    for (var i = 0; i < pairs.length; i++) { this._list = F.headersAppend(this._list, pairs[i][0], pairs[i][1]); }
  }
  Headers.prototype.append = function (n, v) { this._list = F.headersAppend(this._list, String(n), String(v)); };
  Headers.prototype.set = function (n, v) { this._list = F.headersSet(this._list, String(n), String(v)); };
  Headers.prototype.get = function (n) { return F.headersGet(this._list, String(n)); };
  Headers.prototype.has = function (n) { return F.headersHas(this._list, String(n)); };
  Headers.prototype["delete"] = function (n) { this._list = F.headersDelete(this._list, String(n)); };
  Headers.prototype.getSetCookie = function () { return F.headersGetSetCookie(this._list); };
  Headers.prototype.forEach = function (cb, thisArg) { var s = F.headersSortedCombined(this._list); for (var i = 0; i < s.length; i++) { cb.call(thisArg, s[i][1], s[i][0], this); } };
  Headers.prototype.entries = function () { return F.headersSortedCombined(this._list)[Symbol.iterator](); };
  Headers.prototype.keys = function () { return F.headersSortedCombined(this._list).map(function (p) { return p[0]; })[Symbol.iterator](); };
  Headers.prototype.values = function () { return F.headersSortedCombined(this._list).map(function (p) { return p[1]; })[Symbol.iterator](); };
  Headers.prototype[Symbol.iterator] = function () { return this.entries(); };

  function bodyMixin(proto) {
    proto.text = function () { return Promise.resolve(this._bodyText == null ? "" : String(this._bodyText)); };
    proto.json = function () { var t = this._bodyText; return Promise.resolve().then(function () { return JSON.parse(t == null ? "null" : t); }); };
    proto.arrayBuffer = function () { var t = this._bodyText == null ? "" : String(this._bodyText); return Promise.resolve(new TextEncoder().encode(t).buffer); };
    Object.defineProperty(proto, "bodyUsed", { get: function () { return !!this._bodyUsed; }, configurable: true });
  }

  function Request(input, init) {
    if (!(this instanceof Request)) return new Request(input, init);
    init = init || {};
    this.url = typeof input === "object" && input.url ? input.url : String(input);
    var m = init.method ? F.normalizeMethod(String(init.method)) : (input.method || "GET");
    if (F.isForbiddenMethod(m)) { throw new TypeError("Method " + m + " is forbidden"); }
    this.method = m;
    this.headers = new Headers(init.headers || (input.headers));
    this._bodyText = init.body != null ? String(init.body) : (input._bodyText != null ? input._bodyText : null);
    this.redirect = init.redirect || "follow";
  }
  bodyMixin(Request.prototype);
  Request.prototype.clone = function () { return new Request(this.url, { method: this.method, headers: this.headers, body: this._bodyText }); };

  function Response(body, init) {
    if (!(this instanceof Response)) return new Response(body, init);
    init = init || {};
    this._bodyText = body != null ? String(body) : null;
    this.status = init.status != null ? (init.status | 0) : 200;
    this.statusText = init.statusText != null ? String(init.statusText) : "";
    this.headers = new Headers(init.headers);
    this.ok = F.isOkStatus(this.status);
    this.redirected = false;
    this.type = "default";
    this.url = "";
  }
  bodyMixin(Response.prototype);
  Response.prototype.clone = function () { var r = new Response(this._bodyText, { status: this.status, statusText: this.statusText, headers: this.headers }); return r; };
  Response.json = function (data, init) { init = init || {}; var h = new Headers(init.headers); if (!h.has("content-type")) h.set("content-type", "application/json"); return new Response(JSON.stringify(data), { status: init.status, statusText: init.statusText, headers: h }); };
  Response.error = function () { var r = new Response(null, { status: 0 }); r.type = "error"; return r; };
  Response.redirect = function (url, status) { var r = new Response(null, { status: status || 302 }); r.headers.set("location", String(url)); return r; };

  function fetch(input, init) {
    // No HTTP transport in this offline runtime: surface a catchable rejection rather than pretend.
    return Promise.reject(new TypeError(
      "fetch() is not supported in this runtime: no network transport is available"
    ));
  }

  return { Headers: Headers, Request: Request, Response: Response, fetch: fetch };
})()
"#;

fn lazy_headers_getter<'gc>(
    agent: &mut Agent,
    this: Value,
    _args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    materialize_family(
        agent,
        this,
        FETCH_BOOTSTRAP,
        FETCH_FAMILY,
        "Headers",
        crate::node::fetch::install,
        gc,
    )
}

fn lazy_request_getter<'gc>(
    agent: &mut Agent,
    this: Value,
    _args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    materialize_family(
        agent,
        this,
        FETCH_BOOTSTRAP,
        FETCH_FAMILY,
        "Request",
        crate::node::fetch::install,
        gc,
    )
}

fn lazy_response_getter<'gc>(
    agent: &mut Agent,
    this: Value,
    _args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    materialize_family(
        agent,
        this,
        FETCH_BOOTSTRAP,
        FETCH_FAMILY,
        "Response",
        crate::node::fetch::install,
        gc,
    )
}

fn lazy_fetch_getter<'gc>(
    agent: &mut Agent,
    this: Value,
    _args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    materialize_family(
        agent,
        this,
        FETCH_BOOTSTRAP,
        FETCH_FAMILY,
        "fetch",
        crate::node::fetch::install,
        gc,
    )
}

/// The four global names the fetch bootstrap materializes together.
const FETCH_FAMILY: &[&str] = &["Headers", "Request", "Response", "fetch"];

/// The hidden, non-enumerable global slot through which a lazy getter hands the leaf module's native
/// primitives to its JS bootstrap. Parked just before the bootstrap evaluates and deleted immediately
/// after, so it is never observable between turns (and `delete` succeeds because it is configurable).
const NATIVE_SLOT: &str = "__treaty_native_module";

/// Remove a (configurable) data property `name` from `target`. The teardown half of [`NATIVE_SLOT`].
fn delete_global_slot(agent: &mut Agent, target: Object, name: &str, gc: NoGcScope) {
    let key = PropertyKey::from_str(agent, name, gc);
    unwrap_try(target.try_delete(agent, key, gc));
}

/// Materialize a family of WHATWG globals from one bootstrap and return the requested member.
///
/// The bootstrap (an IIFE) returns an object mapping each `member` name to its freshly-built class /
/// function. This:
///   1. evaluates it once (it never touches `globalThis` for the names it owns, so re-reading the
///      lazy accessor — which would re-enter this getter — cannot happen),
///   2. reads each `members[i]` off the returned object and `redefine_as_data`s it onto the realm
///      global, collapsing **every** sibling accessor in the family at once (so touching one member
///      builds the whole family exactly once and the others are already plain data properties), and
///   3. returns the value bound to `want`.
///
/// Redefining the accessor as a data property is the lazy-init contract; doing it from Rust (rather
/// than the bootstrap assigning `globalThis.X = …`) is required because the accessor has no setter and
/// the bootstrap runs in strict mode, where such an assignment would throw.
fn materialize_family<'gc>(
    agent: &mut Agent,
    this: Value,
    bootstrap: &'static str,
    members: &[&'static str],
    want: &'static str,
    native: crate::node::InstallFn,
    mut gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    // Build the leaf module's native primitives in Rust and park them on a hidden global slot the
    // bootstrap reads. The native modules backing these globals (`text_encoding`, `fetch`) are
    // intentionally NOT importable `node:` specifiers (they appear only as globals — see the
    // `globals_only_modules_are_not_in_the_import_table` registry test), so the bootstrap cannot reach
    // them via `require`; the host hands them in directly through the slot instead. Recovering the
    // `HostState` to build the module mirrors the documented Nova FFI boundary used by `require`.
    let state = match crate::node::core::host_state(agent) {
        // SAFETY: see `core::extend_lifetime`. The `HostState` lives in the JsRuntime's box at a stable
        // address that outlives this call; the borrow is used only here and never aliased mutably with
        // `&mut Agent` (its caches are `RefCell`-guarded). Identical to `module_cjs::require`.
        Some(state) => unsafe { crate::node::core::extend_lifetime(state) },
        None => {
            return Err(agent.throw_exception_with_static_message(
                ExceptionType::Error,
                "the Node compatibility layer is not installed",
                gc.into_nogc(),
            ));
        }
    };
    let ctx = NodeCtx::new(state);
    let native_obj = match native(agent, &ctx, gc.reborrow()) {
        Ok(obj) => obj.unbind(),
        Err(e) => {
            let msg = e.to_string();
            return Err(agent.throw_exception(ExceptionType::Error, msg, gc.into_nogc()));
        }
    };
    {
        let nogc = gc.nogc();
        let global = agent.current_realm(nogc).global_object(agent);
        define_value_on(agent, global, NATIVE_SLOT, native_obj.bind(nogc), nogc);
    }

    let result = eval_bootstrap(agent, bootstrap, gc.reborrow()).unbind()?;
    let nogc = gc.into_nogc();
    // Clear the slot so it does not linger as an observable global.
    let global = getter_target(agent, this, nogc);
    delete_global_slot(agent, global, NATIVE_SLOT, nogc);
    let Ok(result_obj) = Object::try_from(result.bind(nogc)) else {
        return Err(agent.throw_exception_with_static_message(
            ExceptionType::Error,
            "global bootstrap did not return an object",
            nogc,
        ));
    };

    let mut wanted = Value::Undefined;
    for &name in members {
        let key = PropertyKey::from_static_str(agent, name, nogc);
        let value = match result_obj.try_get(agent, key, result_obj.into(), None, nogc) {
            std::ops::ControlFlow::Continue(nova_vm::ecmascript::TryGetResult::Value(v)) => v,
            _ => Value::Undefined,
        };
        redefine_as_data(agent, global, name, value, nogc);
        if name == want {
            wanted = value;
        }
    }
    Ok(wanted)
}

#[cfg(test)]
mod tests {
    use crate::JsRuntime;
    use serde_json::json;

    // `install_globals`, `define_fn`, `define_value`, `define_lazy`, and the self-replacing-accessor
    // contract require a live realm to exercise, so they are proven through `JsRuntime` end-to-end
    // rather than against a fabricated agent.

    #[test]
    fn global_aliases_global_this_eagerly() {
        // Eager self-reference: Node's `global` is the realm's `globalThis`.
        let mut rt = JsRuntime::with_node_compat();
        assert_eq!(rt.eval("global === globalThis").unwrap(), json!(true));
        // And it is a live alias: a property set through one is visible through the other.
        assert_eq!(
            rt.eval("global.__alias_probe = 7; globalThis.__alias_probe")
                .unwrap(),
            json!(7)
        );
    }

    #[test]
    fn lazy_self_global_resolves_to_global_this() {
        // The lazy self-replacing accessor materializes `self` to the global object on first read.
        let mut rt = JsRuntime::with_node_compat();
        assert_eq!(rt.eval("self === globalThis").unwrap(), json!(true));
        assert_eq!(rt.eval("self === global").unwrap(), json!(true));
    }

    #[test]
    fn lazy_self_is_stable_and_data_backed_after_first_read() {
        // After the first read the accessor must have collapsed into a plain data property: it keeps
        // returning the same object, and (being a normal writable data property) it round-trips a
        // reassignment instead of re-invoking a getter.
        let mut rt = JsRuntime::with_node_compat();
        assert_eq!(
            rt.eval("const a = self; const b = self; a === b && b === globalThis")
                .unwrap(),
            json!(true)
        );
        // Now that `self` is a data property, assigning to it sticks (a getter-only accessor would
        // silently drop the write in strict mode it would throw — either way it would not read back).
        assert_eq!(
            rt.eval("self = 123; self").unwrap(),
            json!(123),
            "self should be a writable data property after lazy materialization"
        );
    }

    #[test]
    fn untouched_lazy_global_does_not_break_evaluation() {
        // A program that never reads `self` must evaluate exactly as the plain runtime would — the
        // lazy descriptor is inert until touched (unused-module-cost-zero).
        let mut rt = JsRuntime::with_node_compat();
        assert_eq!(rt.eval("1 + 2").unwrap(), json!(3));
        assert_eq!(
            rt.eval("({ a: [1, 2], b: 'x' })").unwrap(),
            json!({ "a": [1, 2], "b": "x" })
        );
    }

    #[test]
    fn lazy_self_is_not_enumerable() {
        // Host globals like `self`/`URL` are non-enumerable; the lazy accessor must present that way
        // so `for..in` / `Object.keys(globalThis)` do not surface it before it is touched.
        let mut rt = JsRuntime::with_node_compat();
        assert_eq!(
            rt.eval("Object.getOwnPropertyNames(globalThis).includes('self')")
                .unwrap(),
            json!(true),
            "self should exist as an own (non-enumerable) property"
        );
        assert_eq!(
            rt.eval("Object.keys(globalThis).includes('self')").unwrap(),
            json!(false),
            "self should not be enumerable"
        );
    }
}
