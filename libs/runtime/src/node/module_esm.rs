//! ESM module loading: Nova's `HostLoadImportedModule` graph-loading hook plus the uniform
//! `install` entry for `import`-based access to builtins.
//!
//! This file owns the host side of ESM `import`: [`load_imported_module`] is the function
//! [`crate::node::core::HostState`]'s `HostHooks::load_imported_module` forwards to. The full
//! resolve -> read -> transpile -> `parse_module` pipeline (mirroring the Nova CLI's `module_map`)
//! is filled by the loader build subagent; the shared core supplies the seam and a clean failure
//! path so an unhandled `import` throws rather than panics.

use nova_vm::ecmascript::{
    Agent, ExceptionType, GraphLoadingStateRecord, HostDefined, ModuleRequest, Object, Referrer,
    finish_loading_imported_module,
};
use nova_vm::engine::NoGcScope;

use crate::node::core::{HostState, InstallError, NodeCtx};
use crate::node::GcScope;

/// Host-side ESM graph loading. Forwarded from `HostState::load_imported_module`.
///
/// Resolves `module_request` via the runtime's resolver, reads + transpiles the source, and calls
/// [`finish_loading_imported_module`] with the parsed module (or a throw completion). Until the full
/// loader lands, an `import` resolves to a clean thrown `Error` describing the unsupported specifier
/// — never a panic — so the rest of the runtime stays robust.
pub(crate) fn load_imported_module<'gc>(
    _state: &HostState,
    agent: &mut Agent,
    referrer: Referrer<'gc>,
    module_request: ModuleRequest<'gc>,
    _host_defined: Option<HostDefined>,
    payload: &mut GraphLoadingStateRecord<'gc>,
    gc: NoGcScope<'gc, '_>,
) {
    let specifier = module_request.specifier(agent).to_string_lossy(agent).into_owned();
    let result = Err(agent.throw_exception(
        ExceptionType::Error,
        format!("ESM import of '{specifier}' is not yet supported by the Treaty runtime"),
        gc,
    ));
    finish_loading_imported_module(agent, referrer, module_request, payload, result, gc);
}

/// Uniform per-module entry. ESM has no standalone exports object of its own (it surfaces other
/// modules), so this returns an empty object as a placeholder until the loader build subagent fills
/// the `import`-bridge body.
pub(crate) fn install<'gc>(
    _agent: &mut Agent,
    _ctx: &NodeCtx,
    _gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    Err(InstallError::Nova(
        "module_esm::install is a loader seam, not a directly-installable builtin".to_owned(),
    ))
}
