//! Treaty Node-compatibility layer.
//!
//! This module turns [`crate::JsRuntime`] from a synchronous expression evaluator into a small,
//! lazy, low-memory Node runtime on top of the Nova engine. See `ARCHITECTURE.md` (next to this
//! file) for the authoritative design; the short version:
//!
//! * Each `node:` builtin lives in its own single-responsibility file and exposes the identical
//!   [`install`] entry point. A builtin's JS object and Rust-backed functions are materialized only
//!   on the *first* `require`/`import` of its specifier (lazy; tenet 2), so unused modules cost one
//!   `&'static str` table entry and a function pointer — zero heap, zero Nova handles.
//! * The shared core is small: [`core`] (the `HostState`, the lazy registry, the single `unsafe`
//!   Nova FFI lifetime extension), [`event_loop`] (the microtask + timer pump that lets
//!   `async`/`await` and `setTimeout` make progress), [`globals`] (eager + lazy global wiring), and
//!   [`resolver`] (the `oxc_resolver`-backed module resolution seam).
//! * The Node layer is purely additive: it is installed behind [`crate::JsRuntime::with_node_compat`]
//!   and does not change the behavior of [`crate::JsRuntime::new`], `eval`, `run_macro`, or
//!   `run_server_fn`.
//!
//! This module is the shared-core scaffold: the registry, event loop, resolver, host state, and one
//! uniform `install` seam per builtin. The leaf module bodies are filled by parallel build agents,
//! so several core items (the registry table, `NodeCtx` accessors, the `define_*` helpers, the
//! resolver classification enums) are referenced only by those forthcoming bodies and by tests. The
//! crate-wide `dead_code` allowance below keeps the scaffold building warning-free until they land;
//! it is scoped to `node` and does not affect the public crate surface.
#![allow(dead_code)]

pub(crate) mod core;
pub(crate) mod event_loop;
pub(crate) mod globals;
pub(crate) mod resolver;

// Module-loading machinery (shared core seams; bodies filled by the loader subagents).
pub(crate) mod module_cjs;
pub(crate) mod module_esm;
pub(crate) mod module_resolver;

// Leaf builtin modules — one file each, all exposing the uniform `install` entry.
pub(crate) mod buffer;
pub(crate) mod console;
pub(crate) mod events;
pub(crate) mod fetch;
pub(crate) mod fs;
pub(crate) mod fs_promises;
pub(crate) mod microtask;
pub(crate) mod os;
pub(crate) mod path;
pub(crate) mod process;
pub(crate) mod structured_clone;
pub(crate) mod text_encoding;
pub(crate) mod timers;
pub(crate) mod url;
pub(crate) mod util;

pub(crate) use crate::node::core::{InstallError, NodeCtx};
pub(crate) use nova_vm::{
    ecmascript::{Agent, Object},
    engine::GcScope,
};

/// One Node builtin module.
///
/// Implemented as a zero-sized unit struct per file (e.g. `pub struct PathModule;`) so the registry
/// table can be an array of function pointers rather than boxed trait objects — an unused module
/// therefore costs no allocation. The trait and the free-function [`install`] entry are kept in
/// lockstep: the registry stores the plain `install` fn pointer, while the trait documents the
/// contract and pins the canonical specifier.
pub(crate) trait NodeModule {
    /// The canonical bare specifier, e.g. `"path"`. The registry also matches the `node:`-prefixed
    /// form by stripping the prefix before lookup.
    const SPECIFIER: &'static str;

    /// Build this module's exports object. Called at most ONCE per runtime, on the first
    /// `require`/`import` of [`Self::SPECIFIER`]. Must allocate only what the module needs.
    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: GcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError>;
}

/// THE uniform per-module entry point.
///
/// Every file under `node/` exposes a free function with exactly this signature. It returns the
/// module's exports object, freshly built on the Nova heap. This is the type the registry stores as
/// a function pointer.
pub(crate) type InstallFn =
    for<'gc> fn(&mut Agent, &NodeCtx, GcScope<'gc, '_>) -> Result<Object<'gc>, InstallError>;

/// The static dispatch table: `(specifier, install fn)` pairs, stored in read-only memory (no heap).
///
/// Surfacing a module here makes `require("<spec>")` / `import "<spec>"` (and the `node:`-prefixed
/// forms) resolve to its lazy [`install`]. Modules that are *only* reachable as globals
/// (`text_encoding`, `fetch`, `structured_clone`) are wired by [`globals`] and intentionally do not
/// appear here.
pub(crate) const BUILTINS: &[(&str, InstallFn)] = &[
    ("path", path::install),
    ("process", process::install),
    ("fs", fs::install),
    ("fs/promises", fs_promises::install),
    ("buffer", buffer::install),
    ("os", os::install),
    ("util", util::install),
    ("events", events::install),
    ("console", console::install),
    ("timers", timers::install),
    ("url", url::install),
];

/// Look up a builtin by specifier, tolerating the optional `node:` prefix.
///
/// Returns the module's [`install`] fn pointer on a hit. This is a pure table scan over a handful of
/// `&'static str`s — no allocation, no filesystem touch — so the module resolver can short-circuit
/// builtins before reaching for `oxc_resolver`.
pub(crate) fn lookup(specifier: &str) -> Option<InstallFn> {
    let bare = specifier.strip_prefix("node:").unwrap_or(specifier);
    BUILTINS
        .iter()
        .find(|(s, _)| *s == bare)
        .map(|(_, f)| *f)
}

/// Install the Node-compatibility layer's globals into a freshly created realm's global object.
///
/// This is the single entry the runtime wires into Nova's `initialize_global_object` realm hook. It
/// delegates to [`globals::install_globals`], which installs the always-present globals eagerly and
/// the rarely-touched ones as self-replacing lazy accessors.
pub(crate) fn install(agent: &mut Agent, global: Object, gc: GcScope) {
    globals::install_globals(agent, global, gc);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_matches_bare_and_node_prefixed_specifiers() {
        assert!(lookup("path").is_some());
        assert!(lookup("node:path").is_some());
        assert!(lookup("fs/promises").is_some());
        assert!(lookup("node:fs/promises").is_some());
    }

    #[test]
    fn lookup_rejects_unknown_specifiers() {
        assert!(lookup("definitely-not-a-builtin").is_none());
        assert!(lookup("./relative").is_none());
    }

    #[test]
    fn builtin_table_has_no_duplicate_specifiers() {
        for (i, (a, _)) in BUILTINS.iter().enumerate() {
            for (b, _) in &BUILTINS[i + 1..] {
                assert_ne!(a, b, "duplicate specifier in BUILTINS: {a}");
            }
        }
    }
}
