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
    }
}
