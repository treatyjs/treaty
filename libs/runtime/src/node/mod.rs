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
pub(crate) mod assert;
pub(crate) mod buffer;
pub(crate) mod console;
pub(crate) mod crypto;
pub(crate) mod events;
pub(crate) mod fetch;
pub(crate) mod fs;
pub(crate) mod fs_promises;
pub(crate) mod microtask;
pub(crate) mod os;
pub(crate) mod path;
pub(crate) mod process;
pub(crate) mod querystring;
pub(crate) mod string_decoder;
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
    ("crypto", crypto::install),
    ("querystring", querystring::install),
    ("assert", assert::install),
    ("string_decoder", string_decoder::install),
];

/// Strip the optional `node:` scheme from a specifier, yielding the bare name to match against the
/// table.
///
/// Borrowing slice work only (tenet 3): returns a sub-slice of the input, never an allocation. An
/// empty result (the degenerate `"node:"` with nothing after the scheme) is surfaced as `None` so
/// callers treat it as "not a builtin" rather than matching a hypothetical empty table entry.
#[inline]
fn bare_specifier(specifier: &str) -> Option<&str> {
    let bare = specifier.strip_prefix("node:").unwrap_or(specifier);
    if bare.is_empty() {
        None
    } else {
        Some(bare)
    }
}

/// Resolve a specifier to its canonical table entry, tolerating the optional `node:` prefix.
///
/// Returns the `&'static str` canonical bare specifier *and* the [`install`] fn pointer in one scan.
/// Callers that need the static name (e.g. the resolver, which records `Resolved::Builtin(&'static
/// str)`) get it without a second table walk. Pure scan over a handful of `&'static str`s — no
/// allocation, no filesystem touch.
#[inline]
pub(crate) fn lookup_canonical(specifier: &str) -> Option<(&'static str, InstallFn)> {
    let bare = bare_specifier(specifier)?;
    BUILTINS
        .iter()
        .find(|(s, _)| *s == bare)
        .map(|(s, f)| (*s, *f))
}

/// Look up a builtin by specifier, tolerating the optional `node:` prefix.
///
/// Returns the module's [`install`] fn pointer on a hit. This is a pure table scan over a handful of
/// `&'static str`s — no allocation, no filesystem touch — so the module resolver can short-circuit
/// builtins before reaching for `oxc_resolver`.
#[inline]
pub(crate) fn lookup(specifier: &str) -> Option<InstallFn> {
    lookup_canonical(specifier).map(|(_, f)| f)
}

/// Whether `specifier` names a Treaty `node:` builtin.
///
/// The faithful analogue of Node's `module.isBuiltin(id)` / `process.binding`-era checks: both the
/// bare (`"fs"`) and scheme-qualified (`"node:fs"`) forms report `true`. Used by callers that must
/// decide builtin-vs-userland without materializing the module.
#[inline]
pub(crate) fn is_builtin(specifier: &str) -> bool {
    lookup_canonical(specifier).is_some()
}

/// Install the Node-compatibility layer's globals into a freshly created realm's global object.
///
/// This is the single entry the runtime wires into Nova's `initialize_global_object` realm hook. It
/// delegates to [`globals::install_globals`], which installs the always-present globals eagerly and
/// the rarely-touched ones as self-replacing lazy accessors.
pub(crate) fn install(agent: &mut Agent, global: Object, gc: GcScope) {
    globals::install_globals(agent, global, gc);
}

/// Install the host-service-backed Node globals after realm creation.
///
/// The realm-init hook ([`install`]) holds only `&mut Agent` and so can wire only the host-service-free
/// self-references. The always-present module-backed globals — `process`, the timer functions,
/// `queueMicrotask`, and the lazy WHATWG `URL`/`URLSearchParams`/`TextEncoder`/`TextDecoder`/`fetch`
/// family — need a [`crate::node::core::HostState`] borrow to build their modules, which only exists
/// *after* realm creation (where the agent and the boxed `HostState` are borrowed separately). This is
/// the seam [`crate::JsRuntime::with_node_compat`] calls at that point; see
/// [`globals::install_module_globals`] for the eager/lazy split.
pub(crate) fn install_module_globals(
    agent: &mut Agent,
    state: &core::HostState,
    gc: GcScope,
) -> Result<(), InstallError> {
    globals::install_module_globals(agent, state, gc)
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
        assert!(lookup("../up").is_none());
        assert!(lookup("/abs/path").is_none());
    }

    #[test]
    fn lookup_rejects_empty_and_bare_scheme() {
        // The degenerate scheme-only / empty forms must never match a builtin.
        assert!(lookup("").is_none());
        assert!(lookup("node:").is_none());
        assert!(!is_builtin(""));
        assert!(!is_builtin("node:"));
    }

    #[test]
    fn lookup_does_not_double_strip_the_scheme() {
        // Only one `node:` prefix is stripped; a doubled scheme is not a builtin.
        assert!(lookup("node:node:path").is_none());
        // A bare `node` (the package name without a sub-path) is not one of our builtins.
        assert!(lookup("node").is_none());
    }

    #[test]
    fn is_builtin_reports_both_forms() {
        assert!(is_builtin("os"));
        assert!(is_builtin("node:os"));
        assert!(!is_builtin("lodash"));
    }

    #[test]
    fn lookup_canonical_returns_static_bare_name() {
        let (name, _) = lookup_canonical("node:fs/promises").expect("builtin present");
        // The returned name is the canonical bare specifier, scheme stripped, borrowed from the
        // static table (no allocation, usable as `&'static str`).
        assert_eq!(name, "fs/promises");
        let _static: &'static str = name;
    }

    #[test]
    fn lookup_canonical_and_lookup_agree() {
        for (spec, _) in BUILTINS {
            let via_canonical = lookup_canonical(spec).map(|(_, f)| f);
            let via_lookup = lookup(spec);
            assert_eq!(
                via_canonical.is_some(),
                via_lookup.is_some(),
                "lookup helpers disagree for {spec}"
            );
        }
    }

    #[test]
    fn every_builtin_specifier_resolves() {
        // Each registered specifier must round-trip through both the bare and `node:`-prefixed form.
        for (spec, _) in BUILTINS {
            assert!(lookup(spec).is_some(), "bare lookup failed for {spec}");
            let prefixed = format!("node:{spec}");
            assert!(lookup(&prefixed).is_some(), "node: lookup failed for {prefixed}");
            let (canonical, _) = lookup_canonical(&prefixed).expect("prefixed resolves");
            assert_eq!(canonical, *spec, "canonical name drifted for {spec}");
        }
    }

    #[test]
    fn builtin_table_has_no_duplicate_specifiers() {
        for (i, (a, _)) in BUILTINS.iter().enumerate() {
            for (b, _) in &BUILTINS[i + 1..] {
                assert_ne!(a, b, "duplicate specifier in BUILTINS: {a}");
            }
        }
    }

    #[test]
    fn builtin_specifiers_match_their_module_trait_constants() {
        // Lockstep guard: each importable leaf module's declared `NodeModule::SPECIFIER` must equal
        // the name it is registered under in BUILTINS. Catches drift between a module renaming its
        // canonical specifier and the table.
        use crate::node::NodeModule;
        fn registered(spec: &str) -> bool {
            BUILTINS.iter().any(|(s, _)| *s == spec)
        }
        assert!(registered(path::PathModule::SPECIFIER));
        assert!(registered(process::ProcessModule::SPECIFIER));
        assert!(registered(fs::FsModule::SPECIFIER));
        assert!(registered(fs_promises::FsPromisesModule::SPECIFIER));
        assert!(registered(buffer::BufferModule::SPECIFIER));
        assert!(registered(os::OsModule::SPECIFIER));
        assert!(registered(util::UtilModule::SPECIFIER));
        assert!(registered(events::EventsModule::SPECIFIER));
        assert!(registered(console::ConsoleModule::SPECIFIER));
        assert!(registered(timers::TimersModule::SPECIFIER));
        assert!(registered(url::UrlModule::SPECIFIER));
        assert!(registered(crypto::CryptoModule::SPECIFIER));
        assert!(registered(querystring::QuerystringModule::SPECIFIER));
        assert!(registered(assert::AssertModule::SPECIFIER));
        assert!(registered(string_decoder::StringDecoderModule::SPECIFIER));
    }

    #[test]
    fn globals_only_modules_are_not_in_the_import_table() {
        // `fetch`, `microtask`, `structured_clone`, and `text_encoding` are wired as globals, not as
        // importable `node:` builtins, so they must NOT appear in BUILTINS.
        for spec in ["fetch", "microtask", "structured_clone", "text_encoding"] {
            assert!(lookup(spec).is_none(), "{spec} should not be importable");
        }
    }
}
