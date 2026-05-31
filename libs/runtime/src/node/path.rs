//! `node:path` — path manipulation (`join`, `resolve`, `dirname`, `basename`, `extname`, `sep`,
//! `parse`/`format`, POSIX + win32 variants).
//!
//! Lazy: built only on first `require("node:path")`/`import`. Pure string work — no allocation
//! beyond the result strings, no Nova handles cached. Body filled by the build subagent.

use nova_vm::ecmascript::{Agent, Object, OrdinaryObject};

use crate::node::core::{InstallError, NodeCtx};
use crate::node::{GcScope, NodeModule};

/// Zero-sized marker for the `node:path` builtin.
pub(crate) struct PathModule;

impl NodeModule for PathModule {
    const SPECIFIER: &'static str = "path";

    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: GcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError> {
        install(agent, ctx, gc)
    }
}

/// Uniform per-module entry. Returns the `node:path` exports object.
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    let obj = OrdinaryObject::create_empty_object(agent, gc.into_nogc());
    Ok(obj.into())
}
