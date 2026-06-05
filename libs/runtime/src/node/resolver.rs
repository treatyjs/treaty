//! The module-resolution seam, built on `oxc_resolver` (the Rust node-resolution crate).
//!
//! Resolution decision: `oxc_resolver 11.20.0` builds cleanly alongside the workspace's oxc `0.133`
//! parser/transformer crates (it is a standalone crate with no dependency on the oxc AST crates),
//! so it is used directly rather than hand-rolling node-module resolution. It gives us, for free:
//! the `node_modules` walk, `package.json` `main`/`exports`/`imports` with conditions, extension
//! probing, and `tsconfig` `paths`. We keep exactly one [`oxc_resolver::Resolver`] per
//! [`crate::JsRuntime`] (resolvers carry an internal cache; reusing one is both faster and lower
//! memory — tenet 3).
//!
//! The [`ModuleResolver`] is the only resolution entry the rest of the Node layer sees; swapping the
//! backend later means changing only this file.

use std::path::{Path, PathBuf};

use oxc_resolver::{ModuleType, ResolveOptions, Resolver};

/// Node + TypeScript file extensions probed during resolution, in priority order.
///
/// `.ts`/`.mts`/`.cts` precede nothing JS-specific by accident: Node itself does not resolve these,
/// but Treaty loads `.ts` through the existing oxc transpile, so they must resolve like source.
const EXTENSIONS: &[&str] = &[
    ".js", ".mjs", ".cjs", ".ts", ".mts", ".cts", ".json", ".node",
];

/// Export/import conditions matched in priority order: `node`, then ESM `import`, then CJS
/// `require`, then `default`.
const CONDITIONS: &[&str] = &["node", "import", "require", "default"];

/// A resolved specifier.
///
/// `Builtin` short-circuits before any filesystem touch (the bare/`node:` specifier names a Treaty
/// builtin). `File` carries the absolute path plus the module kind `oxc_resolver` inferred from
/// `package.json` `type` / the extension, which the loader uses to choose the CJS or ESM path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Resolved {
    /// A Treaty `node:` builtin; the `&'static str` is its canonical bare specifier.
    Builtin(&'static str),
    /// A file on disk plus its inferred module type (when known).
    File(PathBuf, ModuleKind),
}

/// The module system a resolved file should be loaded as.
///
/// Mirrors the subset of [`oxc_resolver::ModuleType`] the loaders care about; `Unknown` defers the
/// decision to the loader's extension/heuristic fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModuleKind {
    /// ECMAScript module (`.mjs`, or `package.json` `"type": "module"`).
    Esm,
    /// CommonJS module (`.cjs`, or the Node default).
    CommonJs,
    /// JSON module.
    Json,
    /// Could not be determined from resolution metadata; loader falls back to extension.
    Unknown,
}

impl From<Option<ModuleType>> for ModuleKind {
    fn from(value: Option<ModuleType>) -> Self {
        match value {
            Some(ModuleType::Module) => ModuleKind::Esm,
            Some(ModuleType::CommonJs) => ModuleKind::CommonJs,
            Some(ModuleType::Json) => ModuleKind::Json,
            // `Wasm`/`Addon` are not executable JS here; treat as unknown so the loader rejects them
            // explicitly rather than mislabeling them.
            Some(_) | None => ModuleKind::Unknown,
        }
    }
}

/// An error raised while resolving a specifier.
///
/// Carries the `oxc_resolver` diagnostic text; the loader maps it to a thrown `Error` / an
/// [`crate::node::core::InstallError::Resolve`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolveError(pub String);

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cannot resolve module: {}", self.0)
    }
}

impl std::error::Error for ResolveError {}

/// The one resolver per runtime. Wraps [`oxc_resolver::Resolver`] configured for Node + TS.
pub(crate) struct ModuleResolver {
    inner: Resolver,
}

impl std::fmt::Debug for ModuleResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The inner oxc resolver is not `Debug`; summarize instead so `HostState` can derive Debug.
        f.write_str("ModuleResolver { oxc_resolver }")
    }
}

impl ModuleResolver {
    /// Build a resolver rooted at `cwd`.
    ///
    /// `cwd` is recorded for callers that resolve relative to the project root; per-call resolution
    /// uses the importing file's directory. `tsconfig` discovery is left at its default (`paths`
    /// honored when a referenced `tsconfig.json` is supplied) so the common case stays allocation-
    /// and IO-light.
    pub(crate) fn new(_cwd: &Path) -> Self {
        let options = ResolveOptions {
            condition_names: CONDITIONS.iter().map(|s| (*s).to_owned()).collect(),
            extensions: EXTENSIONS.iter().map(|s| (*s).to_owned()).collect(),
            // Read `package.json` `"type"` so we can classify ESM vs CJS without a second probe.
            module_type: true,
            ..ResolveOptions::default()
        };
        Self {
            inner: Resolver::new(options),
        }
    }

    /// Classify and resolve `specifier` as imported from `from_dir`.
    ///
    /// Builtins short-circuit with no filesystem access. Everything else delegates to the inner
    /// [`oxc_resolver::Resolver`], whose [`oxc_resolver::Resolution`] yields the absolute path and
    /// (when `package.json`/extension makes it knowable) the module kind.
    pub(crate) fn resolve(&self, from_dir: &Path, specifier: &str) -> Result<Resolved, ResolveError> {
        if let Some(_install) = crate::node::lookup(specifier) {
            // Re-derive the canonical bare specifier as a `&'static str` from the table so the
            // returned `Resolved::Builtin` borrows static memory (zero allocation).
            let bare = specifier.strip_prefix("node:").unwrap_or(specifier);
            if let Some((s, _)) = crate::node::BUILTINS.iter().find(|(s, _)| *s == bare) {
                return Ok(Resolved::Builtin(s));
            }
        }

        match self.inner.resolve(from_dir, specifier) {
            Ok(resolution) => {
                let kind = ModuleKind::from(resolution.module_type());
                Ok(Resolved::File(resolution.into_path_buf(), kind))
            }
            Err(err) => Err(ResolveError(err.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A self-cleaning temp directory for the filesystem-backed resolution tests.
    ///
    /// Uses only `std::fs` (tenet 4) and a process-unique name so parallel test threads never
    /// collide; `Drop` removes the tree so a test leaves nothing behind. We avoid a `tempfile`
    /// dependency on purpose — adding crates is out of this module's edit scope.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let mut base = std::env::temp_dir();
            base.push(format!("treaty_resolver_{}_{}_{}", tag, std::process::id(), n));
            std::fs::create_dir_all(&base).expect("create temp dir");
            Self(base)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn write(&self, rel: &str, contents: &str) {
            let p = self.0.join(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).expect("create parent dir");
            }
            std::fs::write(p, contents).expect("write temp file");
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            // Best-effort cleanup; ignore errors so a failing test still reports its real cause.
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn builtin_specifiers_short_circuit_without_fs() {
        let resolver = ModuleResolver::new(Path::new("."));
        // A nonexistent directory proves no filesystem access happens for builtins.
        let nowhere = Path::new("/nonexistent/treaty/dir");
        assert_eq!(
            resolver.resolve(nowhere, "path").unwrap(),
            Resolved::Builtin("path")
        );
        assert_eq!(
            resolver.resolve(nowhere, "node:fs/promises").unwrap(),
            Resolved::Builtin("fs/promises")
        );
    }

    #[test]
    fn unknown_relative_specifier_errors() {
        let resolver = ModuleResolver::new(Path::new("."));
        let nowhere = Path::new("/nonexistent/treaty/dir");
        assert!(resolver.resolve(nowhere, "./does-not-exist").is_err());
    }

    #[test]
    fn module_kind_maps_oxc_types() {
        assert_eq!(ModuleKind::from(Some(ModuleType::Module)), ModuleKind::Esm);
        assert_eq!(
            ModuleKind::from(Some(ModuleType::CommonJs)),
            ModuleKind::CommonJs
        );
        assert_eq!(ModuleKind::from(Some(ModuleType::Json)), ModuleKind::Json);
        assert_eq!(ModuleKind::from(None), ModuleKind::Unknown);
        // `Wasm`/`Addon` are not executable JS here, so they fall through to `Unknown` and the
        // loader rejects them explicitly rather than mislabeling them as ESM/CJS.
        assert_eq!(ModuleKind::from(Some(ModuleType::Wasm)), ModuleKind::Unknown);
        assert_eq!(
            ModuleKind::from(Some(ModuleType::Addon)),
            ModuleKind::Unknown
        );
    }

    #[test]
    fn resolves_relative_file_to_absolute_path() {
        let dir = TempDir::new("rel");
        dir.write("dep.js", "module.exports = 1;\n");

        let resolver = ModuleResolver::new(dir.path());
        let resolved = resolver
            .resolve(dir.path(), "./dep")
            .expect("relative .js should resolve via extension probing");

        match resolved {
            Resolved::File(path, _) => {
                assert!(path.is_absolute(), "resolved path must be absolute");
                assert_eq!(path, dir.path().join("dep.js"));
            }
            other => panic!("expected Resolved::File, got {other:?}"),
        }
    }

    #[test]
    fn classifies_esm_from_package_json_type_module() {
        let dir = TempDir::new("esm");
        // `"type": "module"` makes the bare `.js` file resolve as ESM per the Node ESM algorithm,
        // which `oxc_resolver` reports through `module_type` (enabled in `ModuleResolver::new`).
        dir.write("package.json", r#"{ "type": "module" }"#);
        dir.write("mod.js", "export default 1;\n");

        let resolver = ModuleResolver::new(dir.path());
        let resolved = resolver.resolve(dir.path(), "./mod").expect("resolve esm file");

        assert_eq!(
            resolved,
            Resolved::File(dir.path().join("mod.js"), ModuleKind::Esm)
        );
    }

    #[test]
    fn classifies_commonjs_from_package_json_type_commonjs() {
        let dir = TempDir::new("cjs");
        dir.write("package.json", r#"{ "type": "commonjs" }"#);
        dir.write("mod.js", "module.exports = 1;\n");

        let resolver = ModuleResolver::new(dir.path());
        let resolved = resolver.resolve(dir.path(), "./mod").expect("resolve cjs file");

        assert_eq!(
            resolved,
            Resolved::File(dir.path().join("mod.js"), ModuleKind::CommonJs)
        );
    }

    #[test]
    fn resolves_bare_package_via_node_modules_main() {
        let dir = TempDir::new("pkg");
        // A minimal installed dependency: `node_modules/dep` with a `main` entry. Proves the
        // `node_modules` walk + `package.json` `main` field path through `oxc_resolver`.
        dir.write(
            "node_modules/dep/package.json",
            r#"{ "name": "dep", "version": "1.0.0", "main": "lib/index.js" }"#,
        );
        dir.write("node_modules/dep/lib/index.js", "module.exports = 42;\n");

        let resolver = ModuleResolver::new(dir.path());
        let resolved = resolver
            .resolve(dir.path(), "dep")
            .expect("bare specifier should resolve through node_modules main");

        match resolved {
            Resolved::File(path, _) => {
                assert_eq!(path, dir.path().join("node_modules/dep/lib/index.js"));
            }
            other => panic!("expected Resolved::File, got {other:?}"),
        }
    }

    #[test]
    fn builtin_short_circuits_even_when_a_node_modules_shadow_exists() {
        let dir = TempDir::new("shadow");
        // Even if a userland package named `path` is installed, the `node:` builtin wins: resolution
        // must short-circuit to `Builtin` before any `node_modules` walk (matching Node semantics
        // for core specifiers and keeping the filesystem untouched for builtins — tenet 4).
        dir.write(
            "node_modules/path/package.json",
            r#"{ "name": "path", "main": "index.js" }"#,
        );
        dir.write("node_modules/path/index.js", "module.exports = {};\n");

        let resolver = ModuleResolver::new(dir.path());
        assert_eq!(
            resolver.resolve(dir.path(), "path").unwrap(),
            Resolved::Builtin("path")
        );
        assert_eq!(
            resolver.resolve(dir.path(), "node:path").unwrap(),
            Resolved::Builtin("path")
        );
    }
}
