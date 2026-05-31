//! `node:https` — the TLS-secured counterpart of `node:http` (`createServer`, `request`, `get`,
//! `Agent`).
//!
//! Shared-core **scaffold**: pins the canonical specifier via [`NodeModule`] and exposes the uniform
//! [`install`] seam, currently returning an empty exports object.
//!
//! ## TLS is a documented follow-up
//!
//! A faithful `node:https` needs a TLS implementation. The pure-Rust options (`rustls` + a webpki
//! roots crate) pull in a certificate-roots dependency that does **not** build offline here, and the
//! one constraint this layer must hold is an offline-clean build. So the Build agents implement the
//! `node:https` *surface* over the same `node:http` request/response model — reusing `node:http`'s
//! `std::net` plumbing for shape parity — and the actual TLS handshake is deferred until an
//! offline-buildable TLS path is available. Until the body lands, `require("node:https")` resolves
//! to a real (empty) object rather than throwing, preserving the registry contract and the
//! lazy-cost-zero invariant (tenet 2).

use nova_vm::ecmascript::{Agent, Object, OrdinaryObject};

use crate::node::core::{InstallError, NodeCtx};
use crate::node::{GcScope, NodeModule};

/// Zero-sized marker for the `node:https` builtin.
pub(crate) struct HttpsModule;

impl NodeModule for HttpsModule {
    const SPECIFIER: &'static str = "https";

    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: GcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError> {
        install(agent, ctx, gc)
    }
}

/// Uniform per-module entry. Returns the `node:https` exports object (scaffold: empty), built once
/// lazily on first import (tenet 2).
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    let gc = gc.into_nogc();
    Ok(OrdinaryObject::create_empty_object(agent, gc).into())
}
