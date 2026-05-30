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

/// The JSX authoring plugin (`.tsx` / `.tjsx`). Delegates to [`crate::jsx::compile`], which handles
/// both `@Component` JSX and BARE JSX (`export default function App() { return <div/> }`).
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

/// The base-Angular `.ts` plugin. Delegates to [`crate::angular_source::compile_angular_source`],
/// which compiles `@Component` classes to `ɵɵdefineComponent` (server-block aware) and passes
/// through every other `.ts` shape (`@Directive` / `@Pipe` / `@Injectable` / `@NgModule`, or a plain
/// non-Angular module) unchanged.
pub struct AngularSourcePlugin;

impl AuthoringPlugin for AngularSourcePlugin {
    fn name(&self) -> &str {
        "angular"
    }

    fn extensions(&self) -> &[&str] {
        &["ts"]
    }

    fn compile(&self, source: &str, file_name: &str) -> CompiledAuthoring {
        crate::angular_source::compile_angular_source(source, file_name)
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
        registry.register(Box::new(AngularSourcePlugin));
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

/// The lowercase file extension of `file_name` (without the leading dot), if any. `"app.component.ts"`
/// → `"ts"`; `"Counter.tsx"` → `"tsx"`; a name with no `.` → `None`.
fn extension_of(file_name: &str) -> Option<String> {
    let base = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name);
    base.rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .filter(|ext| !ext.is_empty())
}

/// The single unified per-file authoring entry the NAPI addon calls.
///
/// Resolves the [`AuthoringPlugin`] that owns `file_name`'s extension via
/// [`AuthoringRegistry::with_defaults`] and compiles `source` through it:
///   * `.treaty` → [`TreatySfcPlugin`]
///   * `.tsx` / `.tjsx` → [`JsxAuthoringPlugin`] (handles BARE JSX as well as `@Component` JSX)
///   * `.ts` → [`AngularSourcePlugin`] (the base-Angular path)
///
/// A file whose extension no plugin claims is treated as opaque source and passed through unchanged
/// (no compile, no diagnostics) so the bundler always receives a usable module — the same faithful
/// pass-through the base-Angular path applies to non-Angular `.ts`.
pub fn compile_file(source: &str, file_name: &str) -> CompiledAuthoring {
    let registry = AuthoringRegistry::with_defaults();
    match extension_of(file_name).and_then(|ext| {
        registry
            .for_extension(&ext)
            .map(|plugin| plugin.compile(source, file_name))
    }) {
        Some(compiled) => compiled,
        None => CompiledAuthoring {
            code: source.to_string(),
            server_module: None,
            errors: Vec::new(),
        },
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

    #[test]
    fn for_extension_ts_resolves_angular_plugin() {
        let registry = AuthoringRegistry::with_defaults();
        assert_eq!(registry.for_extension("ts").map(|p| p.name()), Some("angular"));
    }

    #[test]
    fn compile_file_bare_jsx_tsx_compiles_to_define_component() {
        // The headline gap this closes: BARE JSX (no `@Component`) compiles to a `defineComponent`
        // through the unified per-file entry by routing `.tsx` to the JSX plugin.
        let source = "export default function App() {\n  const x = 1;\n  return <div>{x}</div>;\n}\n";
        let out = compile_file(source, "App.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(out.code.contains(DEFINE), "no defineComponent; got: {}", out.code);
        assert!(out.code.contains("App_Template"), "no template fn; got: {}", out.code);
    }

    #[test]
    fn compile_file_treaty_routes_to_treaty_plugin() {
        // A `.treaty` file routes to the SFC plugin and compiles to a `defineComponent`, matching the
        // direct `compile_treaty_authoring` output (proving the route is a thin delegation).
        let source = "const name = 'World';\n<div>{{ name }}</div>";
        let out = compile_file(source, "greeting.treaty");
        let direct = crate::sfc::compile_treaty_authoring(source, "greeting.treaty");
        assert_eq!(out, direct, "compile_file diverged from direct treaty compile");
        assert!(out.code.contains(DEFINE), "no defineComponent; got: {}", out.code);
    }

    #[test]
    fn compile_file_plain_ts_passes_through_unchanged() {
        // A plain non-Angular `.ts` passes through verbatim, no errors, no server module.
        let source = "export const greet = (n: string) => `hi ${n}`;\n";
        let out = compile_file(source, "util.ts");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(out.server_module.is_none(), "unexpected server module");
        assert_eq!(out.code, source, "plain .ts was not passed through unchanged");
    }

    #[test]
    fn compile_file_unknown_extension_passes_through_unchanged() {
        let source = "{ \"a\": 1 }\n";
        let out = compile_file(source, "data.json");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert_eq!(out.code, source, "unknown extension was not passed through unchanged");
    }
}
