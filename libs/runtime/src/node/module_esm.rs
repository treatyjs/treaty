//! ESM module loading: Nova's `HostLoadImportedModule` graph-loading hook.
//!
//! This file owns the host side of ESM `import`. [`load_imported_module`] is the function
//! [`crate::node::core::HostState`]'s `HostHooks::load_imported_module` forwards to; it drives the
//! full resolve -> read -> transpile -> [`parse_module`] pipeline that mirrors the verified Nova CLI
//! reference (`nova_cli/src/lib/{host_hooks,module_map}.rs`), but routes resolution through the
//! crate's `oxc_resolver`-backed [`crate::node::resolver::ModuleResolver`] and loads `.ts` through
//! the crate's existing oxc transpile ([`crate::transpile_ts`]).
//!
//! Memory / allocation discipline (tenets 2 + 3):
//! * The per-realm ESM module cache is created **lazily** — the realm's `[[HostDefined]]` slot is
//!   only populated the first time an `import` is actually loaded, so a script that never imports
//!   pays nothing. Each resolved file is parsed at most once and the rooted [`AbstractModule`] is
//!   reused on every subsequent request (deduping diamond imports and breaking cycles).
//! * Source text is read with one `std::fs::read_to_string` (tenet 4 — direct IO, minimal syscalls).
//!   The specifier and path are borrowed where possible; the only owned allocations are the ones
//!   Nova's own API forces on us (the heap `String`, the `Rc<PathBuf>` host-defined key).
//! * The cache lives behind the realm's host-defined slot rather than [`HostState`], because Nova's
//!   module records are typed [`AbstractModule`] (not the `Object` the shared `module_cache` holds)
//!   and `module_esm` owns the ESM-specific record type.
//!
//! What is implemented here: real loading of user `.js`/`.mjs`/`.cjs`/`.ts`/`.mts`/`.cts` ESM files
//! (relative, absolute, and bare package specifiers, all via `oxc_resolver`), with caching and a
//! clean thrown `Error` (never a panic) on resolve/IO/parse failure.
//!
//! What is deferred (documented, never red): turning a `node:` builtin into a synthetic ESM module
//! namespace for `import fs from "node:fs"`. Builtins are fully reachable today via `require(...)`
//! and via the eager/lazy globals; synthesizing a *live ESM binding* namespace for them is a large,
//! separable piece, so an ESM `import` of a builtin currently throws a precise, actionable error
//! rather than silently misbehaving. JSON modules (`import data from "./x.json"`) are likewise
//! deferred to a synthetic-module follow-up and throw a clear message.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use nova_vm::ecmascript::{
    AbstractModule, Agent, ExceptionType, GraphLoadingStateRecord, HostDefined, JsResult,
    ModuleRequest, Object, Realm, Referrer, String as JsString, finish_loading_imported_module,
    parse_module,
};
use nova_vm::engine::{Bindable, Global, NoGcScope};

use crate::node::core::{InstallError, NodeCtx};
use crate::node::resolver::{ModuleKind, Resolved};
use crate::node::GcScope;

/// The per-realm ESM module cache: resolved absolute path -> rooted module record.
///
/// Stored in the realm's `[[HostDefined]]` slot (lazily; see [`module_map`]) so it is shared by
/// every `import` evaluated in that realm without being owned by [`crate::node::core::HostState`]
/// (whose own `module_cache` is `Object`-typed, for CJS `exports`, not `AbstractModule`).
///
/// `RefCell` because Nova's `load_imported_module` hook runs with `&Agent` while we mutate the map;
/// the runtime is single-threaded so there is never contention.
#[derive(Default)]
pub(crate) struct EsmModuleMap {
    map: RefCell<HashMap<PathBuf, Global<AbstractModule<'static>>>>,
}

impl EsmModuleMap {
    /// Look up a previously-parsed module by its resolved absolute path.
    fn get<'gc>(
        &self,
        agent: &Agent,
        path: &Path,
        gc: NoGcScope<'gc, '_>,
    ) -> Option<AbstractModule<'gc>> {
        self.map.borrow().get(path).map(|g| g.get(agent, gc))
    }

    /// Root and cache a freshly-parsed module under its resolved absolute path.
    fn insert(&self, agent: &Agent, path: PathBuf, module: AbstractModule) {
        let global = Global::new(agent, module.unbind());
        self.map.borrow_mut().insert(path, global);
    }
}

/// Borrow the realm's [`EsmModuleMap`], creating and installing it on first use.
///
/// Lazy by design (tenet 2): the realm's host-defined slot stays `None` until the first `import`
/// actually loads a module, so a Node runtime that never imports never allocates the cache. After
/// the first call the same `Rc` is returned on every subsequent load, giving one shared cache per
/// realm for the runtime's lifetime.
fn module_map(agent: &mut Agent, realm: Realm, gc: NoGcScope) -> Rc<EsmModuleMap> {
    if let Some(existing) = realm.host_defined(agent) {
        if let Ok(map) = existing.downcast::<EsmModuleMap>() {
            return map;
        }
        // The slot is occupied by something other than our map. This only happens if a future
        // realm-init path repurposes the slot; fall back to a detached map so loading still works
        // (it just will not be shared). This branch is defensive and not reached in normal use.
        return Rc::new(EsmModuleMap::default());
    }
    let map = Rc::new(EsmModuleMap::default());
    realm.initialize_host_defined(agent, map.clone() as HostDefined);
    let _ = gc;
    map
}

/// The directory a relative/bare specifier is resolved against, taken from the referrer.
///
/// A module referrer carries its own absolute path in its `[[HostDefined]]` slot (we set it when we
/// parse — see [`load_imported_module`]), so a nested `import` resolves relative to the importing
/// file. The top-level referrer is the realm (no path), so we fall back to the runtime's cwd. The
/// returned `PathBuf` is a directory (the parent of a file referrer).
fn base_dir(agent: &Agent, referrer: Referrer, fallback_cwd: &Path) -> PathBuf {
    if let Some(hd) = referrer.host_defined(agent) {
        if let Ok(path) = hd.downcast::<PathBuf>() {
            if let Some(parent) = path.parent() {
                return parent.to_path_buf();
            }
        }
    }
    fallback_cwd.to_path_buf()
}

/// Host-side ESM graph loading. Forwarded from `HostState::load_imported_module`.
///
/// Resolves `module_request` via the runtime's resolver, returns the cached module on a hit, else
/// reads + transpiles the source and [`parse_module`]s it, caches the rooted record, and calls
/// [`finish_loading_imported_module`]. Every failure path produces a thrown `Error` handed to
/// `finish_loading_imported_module` — never a panic — so the engine's loading state machine always
/// makes progress.
pub(crate) fn load_imported_module<'gc>(
    state: &crate::node::core::HostState,
    agent: &mut Agent,
    referrer: Referrer<'gc>,
    module_request: ModuleRequest<'gc>,
    _host_defined: Option<HostDefined>,
    payload: &mut GraphLoadingStateRecord<'gc>,
    gc: NoGcScope<'gc, '_>,
) {
    let result = resolve_and_parse(state, agent, referrer, module_request, gc);
    finish_loading_imported_module(agent, referrer, module_request, payload, result, gc);
}

/// The fallible core of [`load_imported_module`], split out so every early return is a thrown
/// `Error` collected into one `JsResult` the hook hands to `finish_loading_imported_module`.
fn resolve_and_parse<'gc>(
    state: &crate::node::core::HostState,
    agent: &mut Agent,
    referrer: Referrer<'gc>,
    module_request: ModuleRequest<'gc>,
    gc: NoGcScope<'gc, '_>,
) -> JsResult<'gc, AbstractModule<'gc>> {
    // The specifier as written in the `import`, e.g. "./util.js", "lodash", "node:fs".
    let specifier = module_request
        .specifier(agent)
        .to_string_lossy(agent)
        .into_owned();

    // Resolve relative to the importing file's directory (or cwd for the entry referrer).
    let from_dir = base_dir(agent, referrer, state.cwd());
    let resolved = state.resolver().resolve(&from_dir, &specifier).map_err(|err| {
        agent.throw_exception(
            ExceptionType::Error,
            format!("cannot find module '{specifier}' imported from {}: {err}", from_dir.display()),
            gc,
        )
    })?;

    let (path, kind) = match resolved {
        Resolved::File(path, kind) => (path, kind),
        Resolved::Builtin(name) => {
            // Builtins are reachable via `require` and the globals today; a synthetic ESM namespace
            // for `import x from "node:<name>"` is a documented follow-up.
            return Err(agent.throw_exception(
                ExceptionType::Error,
                format!(
                    "ESM `import` of the builtin 'node:{name}' is not yet supported; use \
                     `require('node:{name}')` or the corresponding global"
                ),
                gc,
            ));
        }
    };

    // JSON modules need a synthetic default-export module record; deferred with a clear message.
    if matches!(kind, ModuleKind::Json) || has_extension(&path, "json") {
        return Err(agent.throw_exception(
            ExceptionType::Error,
            format!(
                "JSON module import is not yet supported by the Treaty runtime: {}",
                path.display()
            ),
            gc,
        ));
    }

    // Cache hit: reuse the already-parsed, rooted module (dedupes diamonds, breaks cycles).
    let map = module_map(agent, referrer.realm(agent, gc), gc);
    if let Some(cached) = map.get(agent, &path, gc) {
        return Ok(cached);
    }

    // Cache miss: read source directly (tenet 4), transpile `.ts*` to JS via the crate's oxc step.
    let source = std::fs::read_to_string(&path).map_err(|err| {
        agent.throw_exception(
            ExceptionType::Error,
            format!("cannot read module {}: {err}", path.display()),
            gc,
        )
    })?;
    let js = match prepare_source(&path, source) {
        Ok(js) => js,
        Err(message) => {
            return Err(agent.throw_exception(ExceptionType::SyntaxError, message, gc));
        }
    };

    // Parse as an ECMAScript module, threading the resolved path as the module's host-defined data
    // so its own nested imports resolve relative to it (see `base_dir`).
    let realm = referrer.realm(agent, gc);
    let source_text = JsString::from_string(agent, js, gc);
    let host_defined: HostDefined = Rc::new(path.clone());
    let module = parse_module(agent, source_text, realm, Some(host_defined), gc).map_err(|errs| {
        let message = errs
            .first()
            .map(|d| d.to_string())
            .unwrap_or_else(|| "failed to parse module".to_owned());
        agent.throw_exception(
            ExceptionType::SyntaxError,
            format!("{} ({})", message, path.display()),
            gc,
        )
    })?;

    let abstract_module: AbstractModule = module.into();
    map.insert(agent, path, abstract_module);
    Ok(abstract_module)
}

/// Turn raw file source into the JavaScript handed to [`parse_module`].
///
/// `.ts`/`.mts`/`.cts` go through the crate's existing oxc transpile (TypeScript syntax stripped);
/// `.js`/`.mjs`/`.cjs` (and unknown extensions, treated as JS) pass through untouched — zero copy
/// beyond what the caller already owns. A transpile failure is returned as a message for the caller
/// to throw as a `SyntaxError`.
fn prepare_source(path: &Path, source: String) -> Result<String, String> {
    if is_typescript(path) {
        crate::transpile_ts(&source).map_err(|e| format!("{} ({})", e.0, path.display()))
    } else {
        Ok(source)
    }
}

/// True when `path` has a TypeScript source extension Treaty transpiles before parsing.
fn is_typescript(path: &Path) -> bool {
    has_extension(path, "ts") || has_extension(path, "mts") || has_extension(path, "cts")
}

/// Case-insensitive extension check that avoids allocating the extension string.
fn has_extension(path: &Path, ext: &str) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

/// Uniform per-module entry. ESM has no standalone exports object of its own (it surfaces other
/// modules through the loader, not as an importable builtin), so attempting to install it as a
/// builtin is a programming error rather than a runtime condition.
pub(crate) fn install<'gc>(
    _agent: &mut Agent,
    _ctx: &NodeCtx,
    _gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    Err(InstallError::Nova(
        "module_esm is the ESM loader seam, not a directly-installable builtin".to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn typescript_extensions_are_detected() {
        assert!(is_typescript(Path::new("/a/b.ts")));
        assert!(is_typescript(Path::new("/a/b.mts")));
        assert!(is_typescript(Path::new("/a/b.cts")));
        assert!(is_typescript(Path::new("/a/b.TS"))); // case-insensitive
        assert!(!is_typescript(Path::new("/a/b.js")));
        assert!(!is_typescript(Path::new("/a/b.mjs")));
        assert!(!is_typescript(Path::new("/a/b")));
    }

    #[test]
    fn has_extension_is_case_insensitive_and_alloc_free_semantics() {
        assert!(has_extension(Path::new("data.json"), "json"));
        assert!(has_extension(Path::new("data.JSON"), "json"));
        assert!(!has_extension(Path::new("data.js"), "json"));
        assert!(!has_extension(Path::new("noext"), "json"));
    }

    #[test]
    fn prepare_source_passes_js_through_unchanged() {
        let src = "export const x = 1 + 2;\n".to_owned();
        let out = prepare_source(Path::new("/m/a.js"), src.clone()).unwrap();
        assert_eq!(out, src, "plain JS must not be rewritten");
    }

    #[test]
    fn prepare_source_strips_typescript_types() {
        let out = prepare_source(
            Path::new("/m/a.ts"),
            "export const x: number = 1; type T = string;".to_owned(),
        )
        .unwrap();
        assert!(!out.contains(": number"), "type annotation should be stripped: {out}");
        assert!(!out.to_lowercase().contains("type t"), "type alias should be stripped: {out}");
        assert!(out.contains("export const x"), "value binding should survive: {out}");
    }

    #[test]
    fn prepare_source_reports_typescript_syntax_error() {
        let err = prepare_source(Path::new("/m/bad.ts"), "const = ;".to_owned())
            .expect_err("invalid TS should not transpile");
        assert!(!err.is_empty());
        assert!(err.contains("bad.ts"), "error should name the file: {err}");
    }

    #[test]
    fn empty_esm_module_map_has_no_entries() {
        let map = EsmModuleMap::default();
        assert!(
            map.map.borrow().is_empty(),
            "a fresh cache must be empty so unused realms cost nothing"
        );
    }

    #[test]
    fn install_is_a_loader_seam_not_a_builtin() {
        // The ESM loader is not a directly-installable module object; the registry never lists it.
        assert!(!crate::node::BUILTINS.iter().any(|(s, _)| *s == "module_esm"));
    }
}
