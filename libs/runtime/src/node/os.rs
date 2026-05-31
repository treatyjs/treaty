//! `node:os` — platform, arch, cpus, hostname, homedir, tmpdir, EOL, release.
//!
//! Lazy: built only on first `require`/`import` of `node:os` (unless surfaced as a global by
//! `globals.rs`). Body filled by the build subagent; the shared core supplies this uniform
//! `install` seam and the `NodeModule` specifier binding.

use nova_vm::ecmascript::{Agent, Object, OrdinaryObject};

use crate::node::core::{InstallError, NodeCtx};
use crate::node::{GcScope, NodeModule};

/// Zero-sized marker for the `node:os` builtin.
pub(crate) struct OsModule;

impl NodeModule for OsModule {
    const SPECIFIER: &'static str = "os";

    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: GcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError> {
        install(agent, ctx, gc)
    }
}

/// Uniform per-module entry. Returns the `node:os` exports object.
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    let obj = OrdinaryObject::create_empty_object(agent, gc.into_nogc());
    Ok(obj.into())
}
