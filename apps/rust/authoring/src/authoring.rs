//! Authoring-plugin layer: a uniform front-end registry over the compiler's authoring formats.
//!
//! Every authoring format Treaty supports is just an [`AuthoringPlugin`] — there is no privileged
//! built-in. A plugin declares the file extensions it owns and knows how to compile a source of
//! that format into a [`CompiledAuthoring`] (client code + optional server module + diagnostics).
//!
//! Two plugins ship by default:
//!   * [`TreatySfcPlugin`] — the `.treaty` single-file-component format, delegating to
//!     [`crate::sfc::compile_treaty_authoring`].
//!   * [`JsxAuthoringPlugin`] — the `.tsx` / `.tjsx` JSX format, delegating to [`crate::jsx`].
//!
//! The [`AuthoringRegistry`] resolves a plugin by name or by file extension, with the first
//! registered plugin as the default.

use crate::CompiledAuthoring;

/// A compiler front-end for one authoring format.
///
/// Implementations turn a source string of their format into a [`CompiledAuthoring`]. The registry
/// dispatches to a plugin by name or by the file extensions it claims via [`AuthoringPlugin::extensions`].
pub trait AuthoringPlugin {
    /// Stable identifier for this plugin (e.g. `"treaty"`), used for registry lookup by name.
    fn name(&self) -> &str;

    /// The file extensions this plugin owns, without the leading dot (e.g. `["tsx", "tjsx"]`).
    fn extensions(&self) -> &[&str];

    /// Compile a source of this format into client code + optional server module + diagnostics.
    fn compile(&self, source: &str, file_name: &str) -> CompiledAuthoring;
}

/// The `.treaty` single-file-component plugin. Delegates to [`crate::sfc::compile_treaty_authoring`],
/// so Treaty is registered exactly like any other authoring format — no privileged path.
pub struct TreatySfcPlugin;

impl AuthoringPlugin for TreatySfcPlugin {
    fn name(&self) -> &str {
        "treaty"
    }

    fn extensions(&self) -> &[&str] {
        &["treaty"]
    }

    fn compile(&self, source: &str, file_name: &str) -> CompiledAuthoring {
        crate::sfc::compile_treaty_authoring(source, file_name)
    }
}

/// The JSX authoring plugin (`.tsx` / `.tjsx`). Delegates to [`crate::jsx::compile`].
pub struct JsxAuthoringPlugin;

impl AuthoringPlugin for JsxAuthoringPlugin {
    fn name(&self) -> &str {
        "jsx"
    }

    fn extensions(&self) -> &[&str] {
        &["tsx", "tjsx"]
    }

    fn compile(&self, source: &str, file_name: &str) -> CompiledAuthoring {
        crate::jsx::compile(source, file_name)
    }
}

/// A registry of available authoring plugins with a default selection.
///
/// The first plugin registered is the default; [`AuthoringRegistry::with_defaults`] seeds it with
/// the [`TreatySfcPlugin`] followed by the [`JsxAuthoringPlugin`].
pub struct AuthoringRegistry {
    plugins: Vec<Box<dyn AuthoringPlugin>>,
}

impl AuthoringRegistry {
    /// An empty registry (no plugins, no default).
    pub fn new() -> Self {
        Self { plugins: Vec::new() }
    }

    /// A registry preloaded with the default plugins: the `.treaty` SFC plugin (the default) and the
    /// JSX plugin.
    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        registry.register(Box::new(TreatySfcPlugin));
        registry.register(Box::new(JsxAuthoringPlugin));
        registry
    }

    /// Register an authoring plugin. The first one registered is the default.
    pub fn register(&mut self, plugin: Box<dyn AuthoringPlugin>) {
        self.plugins.push(plugin);
    }

    /// Look up a plugin by its [`AuthoringPlugin::name`].
    pub fn get(&self, name: &str) -> Option<&dyn AuthoringPlugin> {
        self.plugins
            .iter()
            .find(|p| p.name() == name)
            .map(|p| p.as_ref())
    }

    /// Look up the plugin that owns a file extension (matched case-insensitively, leading dot
    /// optional — both `"tsx"` and `".tsx"` resolve).
    pub fn for_extension(&self, ext: &str) -> Option<&dyn AuthoringPlugin> {
        let needle = ext.trim_start_matches('.').to_ascii_lowercase();
        self.plugins
            .iter()
            .find(|p| p.extensions().iter().any(|e| e.eq_ignore_ascii_case(&needle)))
            .map(|p| p.as_ref())
    }

    /// The default plugin (the first one registered), if any.
    pub fn default_plugin(&self) -> Option<&dyn AuthoringPlugin> {
        self.plugins.first().map(|p| p.as_ref())
    }
}

impl Default for AuthoringRegistry {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFINE: &str = "\u{0275}\u{0275}defineComponent";

    #[test]
    fn for_extension_treaty_resolves_treaty_plugin() {
        let registry = AuthoringRegistry::with_defaults();
        let plugin = registry.for_extension("treaty").expect("treaty plugin");
        assert_eq!(plugin.name(), "treaty");
        // The leading-dot form resolves the same plugin.
        assert_eq!(
            registry.for_extension(".treaty").map(|p| p.name()),
            Some("treaty")
        );
    }

    #[test]
    fn for_extension_tsx_and_tjsx_resolve_jsx_plugin() {
        let registry = AuthoringRegistry::with_defaults();
        assert_eq!(registry.for_extension("tsx").map(|p| p.name()), Some("jsx"));
        assert_eq!(registry.for_extension("tjsx").map(|p| p.name()), Some("jsx"));
    }

    #[test]
    fn default_plugin_is_treaty() {
        let registry = AuthoringRegistry::with_defaults();
        assert_eq!(registry.default_plugin().map(|p| p.name()), Some("treaty"));
        assert!(registry.get("treaty").is_some());
        assert!(registry.get("jsx").is_some());
        assert!(registry.get("nope").is_none());
    }

    #[test]
    fn treaty_plugin_compiles_existing_sample_unchanged() {
        // The same source `sfc` compiles must produce the same output through the registry path,
        // proving the plugin is a thin delegation (no behavior change).
        let source = "const name = 'World';\n<div>{{ name }}</div>";
        let plugin = AuthoringRegistry::with_defaults()
            .for_extension("treaty")
            .expect("treaty plugin")
            .compile(source, "greeting.treaty");
        let direct = crate::sfc::compile_treaty_authoring(source, "greeting.treaty");
        assert_eq!(plugin, direct, "registry path diverged from direct sfc compile");
        assert!(plugin.code.contains(DEFINE), "no defineComponent; got: {}", plugin.code);
    }

    #[test]
    fn jsx_plugin_compiles_trivial_component() {
        let source = "export default function App() {\n  return <div>hi</div>;\n}\n";
        let out = AuthoringRegistry::with_defaults()
            .for_extension("tsx")
            .expect("jsx plugin")
            .compile(source, "app.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(out.code.contains(DEFINE), "no defineComponent; got: {}", out.code);
    }
}
