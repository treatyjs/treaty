//! Module resolution for the native dev server + build, on `oxc_resolver`.
//!
//! Treaty is a compiler, not a host — but a dev server DOES have to resolve
//! `import` specifiers to on-disk files so it can serve them. This wraps
//! `oxc_resolver` (the same resolver `@treaty/*` bundler plugins use) with the
//! conditions/fields a browser ESM build needs:
//!
//!   * `extensions`: `.ts`/`.tsx`/`.mjs`/`.js`/`.json` so a bare `./foo` import
//!     finds `foo.ts` (the dev server compiles it on the fly).
//!   * `condition_names`/`main_fields`: prefer the ESM `module` / fesm entry so
//!     `@angular/core` resolves to its `fesm2022/*.mjs` (the *partial* bundle the
//!     server links to AOT before serving).
//!
//! The resolver itself does no I/O beyond stat/read of `package.json`; reading
//! and compiling/linking the resolved file is the server's job.

use std::path::{Path, PathBuf};

use oxc_resolver::{ResolveOptions, Resolver};

/// A resolver configured for browser ESM resolution of a Treaty app.
pub struct ModuleResolver {
    inner: Resolver,
}

impl ModuleResolver {
    /// Build a resolver rooted at the app. `extensions` covers TypeScript source
    /// (compiled on the fly) and published ESM; conditions/fields prefer the ESM
    /// fesm entry so `@angular/*` resolves to its partial `fesm2022/*.mjs`.
    pub fn new() -> Self {
        let options = ResolveOptions {
            extensions: vec![
                ".ts".into(),
                ".tsx".into(),
                ".mjs".into(),
                ".js".into(),
                ".json".into(),
            ],
            // Browser ESM: prefer `module`/`import`/`browser` over `require`.
            condition_names: vec![
                "module".into(),
                "import".into(),
                "browser".into(),
                "default".into(),
            ],
            main_fields: vec!["module".into(), "browser".into(), "main".into()],
            // Let a directory import fall back to `index.*`.
            main_files: vec!["index".into()],
            ..Default::default()
        };
        ModuleResolver {
            inner: Resolver::new(options),
        }
    }

    /// Resolve `specifier` as imported from a module in `importer_dir`.
    ///
    /// Returns the absolute path of the resolved file, or `None` if it cannot be
    /// resolved (e.g. a Node builtin, or a genuinely missing module).
    pub fn resolve(&self, importer_dir: &Path, specifier: &str) -> Option<PathBuf> {
        self.inner
            .resolve(importer_dir, specifier)
            .ok()
            .map(|r| r.full_path())
    }
}

impl Default for ModuleResolver {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether a specifier is a *bare* package specifier (not relative/absolute):
/// `@angular/core`, `rxjs`, `rxjs/operators` — vs `./x`, `../x`, `/x`.
pub fn is_bare_specifier(spec: &str) -> bool {
    !(spec.starts_with("./") || spec.starts_with("../") || spec.starts_with('/'))
}

/// Whether a resolved file path is under a `node_modules` directory — the cheap
/// path-level guard for "this is a published library, maybe a *partial* one the
/// linker must de-partial". The real check (does the file contain a
/// `ɵɵngDeclare*` call) is applied by the linker caller against file contents.
pub fn is_under_node_modules(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == "node_modules")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_bare_vs_relative_specifiers() {
        assert!(is_bare_specifier("@angular/core"));
        assert!(is_bare_specifier("rxjs"));
        assert!(is_bare_specifier("rxjs/operators"));
        assert!(!is_bare_specifier("./foo"));
        assert!(!is_bare_specifier("../foo"));
        assert!(!is_bare_specifier("/abs/foo"));
    }

    #[test]
    fn node_modules_guard() {
        assert!(is_under_node_modules(Path::new(
            "/app/node_modules/@angular/core/fesm2022/core.mjs"
        )));
        assert!(!is_under_node_modules(Path::new("/app/src/app/app.ts")));
    }

    #[test]
    fn resolves_a_relative_ts_sibling() {
        // Build a tiny fixture: a dir with a.ts importing ./b, and b.ts present.
        let dir = std::env::temp_dir().join(format!("treaty-resolve-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("b.ts"), "export const b = 1;").unwrap();
        let r = ModuleResolver::new();
        let got = r.resolve(&dir, "./b");
        assert!(got.is_some(), "expected ./b to resolve to b.ts");
        assert!(got.unwrap().ends_with("b.ts"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
