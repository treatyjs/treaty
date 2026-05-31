//! CommonJS module loading: the `require(...)` bridge.
//!
//! `require` resolves a specifier via the runtime resolver, returns a cached builtin/user module if
//! present, else builds it once (running the builtin's `install` or evaluating the user file in a
//! CJS wrapper) and caches the rooted exports. The full body is filled by the loader build
//! subagent; this file holds the uniform `install` seam so the registry and parallel agents agree
//! on the signature.

use nova_vm::ecmascript::{Agent, Object, OrdinaryObject};

use crate::node::core::{InstallError, NodeCtx};
use crate::node::GcScope;

/// Uniform per-module entry. The CJS bridge is surfaced as a service rather than an importable
/// object; this returns an empty exports object as a placeholder until the loader build subagent
/// fills the `require` body.
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    let obj = OrdinaryObject::create_empty_object(agent, gc.into_nogc());
    Ok(obj.into())
}
