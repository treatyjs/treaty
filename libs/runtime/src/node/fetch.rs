//! fetch / Headers / Request / Response (WHATWG fetch). Surfaced as lazy globals; this module backs them.
//!
//! Lazy: built only on first `require`/`import` of `node:fetch` (unless surfaced as a global by
//! `globals.rs`). Body filled by the build subagent; the shared core supplies this uniform
//! `install` seam and the `NodeModule` specifier binding.

use nova_vm::ecmascript::{Agent, Object, OrdinaryObject};

use crate::node::core::{InstallError, NodeCtx};
use crate::node::{GcScope, NodeModule};

/// Zero-sized marker for the `node:fetch` builtin.
pub(crate) struct FetchModule;

impl NodeModule for FetchModule {
    const SPECIFIER: &'static str = "fetch";

    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: GcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError> {
        install(agent, ctx, gc)
    }
}

/// Uniform per-module entry. Returns the `node:fetch` exports object.
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    let obj = OrdinaryObject::create_empty_object(agent, gc.into_nogc());
    Ok(obj.into())
}
