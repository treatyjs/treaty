//! Eager + lazy global installation, and the one shared helper every module reuses to define a
//! function on an object.
//!
//! This is the single place that wires globals into a realm. It is called once from
//! [`crate::JsRuntime::with_node_compat`] via Nova's `initialize_global_object` realm hook. Globals
//! split two ways (tenet 2):
//!
//! * **Eager** — installed immediately because Node code assumes they exist without `import`:
//!   `process`, `Buffer`, `console`, the timer functions, `queueMicrotask`, `structuredClone`,
//!   `TextEncoder`/`TextDecoder`, and the `globalThis`/`global` self-reference. (The per-module
//!   bodies are filled by the build subagents; this core installs the `global`/`globalThis` alias
//!   and exposes [`define_fn`] so those bodies are mechanical.)
//! * **Lazy** — installed as self-replacing accessor properties whose getter materializes the real
//!   value on first read, then redefines itself as a plain data property: `URL`,
//!   `URLSearchParams`, `fetch`, `Headers`/`Request`/`Response`. Until first touch they cost only a
//!   property descriptor. (Wired by the build subagents using the accessor seam documented here.)

use nova_vm::ecmascript::{
    Agent, Behaviour, BuiltinFunctionArgs, InternalMethods, Object, OrdinaryObject,
    PropertyDescriptor, PropertyKey, RegularFn, create_builtin_function, unwrap_try,
};
use nova_vm::engine::{GcScope, NoGcScope};

/// Define a Rust-backed function as a data property `name` (arity `len`) on `obj`.
///
/// Lifted from the verified Nova CLI `create_obj_func` (`nova_cli/src/lib/globals.rs`). Every leaf
/// module reuses this to build its exports object, so the function-definition pattern lives in one
/// place. Uses [`PropertyKey::from_static_str`] to intern the (always `&'static`) key without a heap
/// string (tenet 3).
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

/// Install the Node-compat globals into `global`.
///
/// Called once per realm from the `initialize_global_object` hook. The shared core installs the
/// `globalThis`/`global` self-reference (the one global every module and lazy accessor needs as an
/// anchor); the eager builtins and lazy accessors are layered on by the per-module build subagents
/// through [`define_fn`]/[`define_value`] and the accessor seam.
///
/// Kept total and panic-free: an empty realm with only the self-reference is a valid Node-ish
/// global, so partial builds still run.
pub(crate) fn install_globals(agent: &mut Agent, global: Object, gc: GcScope) {
    let gc = gc.into_nogc();

    // `global` is Node's alias for `globalThis`. Point it at the realm's global object so code that
    // reads `global.X` sees the same object as `globalThis.X`.
    let key = PropertyKey::from_static_str(agent, "global", gc);
    unwrap_try(global.try_define_own_property(
        agent,
        key,
        PropertyDescriptor::new_data_descriptor(global),
        None,
        gc,
    ));
}

#[cfg(test)]
mod tests {
    // The behavioral coverage for globals lives in the crate-level `JsRuntime::with_node_compat`
    // tests (a live agent is required to exercise `install_globals`/`define_fn`). This module
    // intentionally holds no unit tests of its own to avoid duplicating that setup.
}
