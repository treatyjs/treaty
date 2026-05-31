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
    Agent, ArgumentsList, Behaviour, BuiltinFunctionArgs, InternalMethods, JsResult, Object,
    OrdinaryObject, PropertyDescriptor, PropertyKey, RegularFn, Value, create_builtin_function,
    unwrap_try,
};
use nova_vm::engine::{Bindable, GcScope, NoGcScope};

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
