//! The runtime-facing resolution helper that classifies a specifier and threads it to the right
//! loader (builtin vs file, CJS vs ESM).
//!
//! [`crate::node::resolver`] owns the `oxc_resolver` wrapper and the `Resolved` enum; this file is
//! the thin policy layer the loaders call to turn `(referrer, specifier)` into a load action. The
//! full policy (referrer-relative base directory, `.ts` transpile dispatch, cache key derivation)
//! is filled by the loader build subagent; the uniform `install` seam keeps the signature aligned.

use nova_vm::ecmascript::{Agent, Object, OrdinaryObject};

use crate::node::core::{InstallError, NodeCtx};
use crate::node::GcScope;

/// Uniform per-module entry. The resolver policy is an internal service, not an importable object;
/// this returns an empty object placeholder until the loader build subagent fills the body.
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    let obj = OrdinaryObject::create_empty_object(agent, gc.into_nogc());
    Ok(obj.into())
}
