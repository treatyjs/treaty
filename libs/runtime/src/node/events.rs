//! `node:events` — EventEmitter (on/once/emit/removeListener) and the events helpers.
//!
//! Lazy: built only on first `require`/`import` of `node:events` (unless surfaced as a global by
//! `globals.rs`). Body filled by the build subagent; the shared core supplies this uniform
//! `install` seam and the `NodeModule` specifier binding.

use nova_vm::ecmascript::{Agent, Object, OrdinaryObject};

use crate::node::core::{InstallError, NodeCtx};
use crate::node::{GcScope, NodeModule};

/// Zero-sized marker for the `node:events` builtin.
pub(crate) struct EventsModule;

impl NodeModule for EventsModule {
    const SPECIFIER: &'static str = "events";

    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: GcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError> {
        install(agent, ctx, gc)
    }
}

/// Uniform per-module entry. Returns the `node:events` exports object.
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    let obj = OrdinaryObject::create_empty_object(agent, gc.into_nogc());
    Ok(obj.into())
}
