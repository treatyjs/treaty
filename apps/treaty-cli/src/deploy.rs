//! The pluggable deploy/rollback layer for Treaty's federated modules.
//!
//! Rust port of `@treaty/federation-deploy`'s `plugin.ts`. Treaty is a
//! **compiler**, not a host: it emits each federated module's artifact plus a
//! versioned federation manifest. *How* an artifact actually lands somewhere
//! servable — copied to a CDN bucket, pushed to object storage, uploaded via a
//! platform API — is deployment-target-specific and therefore pluggable.
//!
//! A [`DeployPlugin`] encapsulates one deployment method: `deploy` publishes a
//! module's built artifact and reports the [`ModuleDeployment`] (version + url)
//! that became live; `rollback` repoints a module at an already-published prior
//! version. The [`DeployPluginRegistry`] resolves a plugin by name so CI can
//! select a method per environment without the orchestration code knowing the
//! details. A reference [`NoopDeployPlugin`] (records intent; used for dry-runs
//! and tests) ships here.

use std::collections::BTreeMap;

/// What kind of federated module a deployment describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleKind {
    Host,
    Route,
    Lib,
}

impl ModuleKind {
    /// The lowercase token form.
    pub fn as_str(self) -> &'static str {
        match self {
            ModuleKind::Host => "host",
            ModuleKind::Route => "route",
            ModuleKind::Lib => "lib",
        }
    }
}

/// One federated module's live deployment state: the version served and the url
/// it is served from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleDeployment {
    pub version: String,
    pub url: String,
}

/// Identity of a federated module being deployed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployModule {
    /// Stable identity of the module across versions (the manifest key).
    pub module_id: String,
    /// The version being published.
    pub version: String,
    /// What kind of module this is.
    pub kind: ModuleKind,
}

/// The built output for one module that a [`DeployPlugin`] publishes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeployArtifact {
    /// Directory holding the module's built files (the remote entry + chunks).
    pub dir: Option<String>,
    /// The remote entry filename within `dir` (e.g. `remoteEntry.js`).
    pub entry: Option<String>,
    /// Raw file contents to publish, keyed by relative path. An alternative to
    /// `dir` for in-memory / generated artifacts.
    pub files: BTreeMap<String, String>,
}

/// Ambient context handed to every [`DeployPlugin`] call.
#[derive(Debug, Clone, Default)]
pub struct DeployContext {
    /// Informational app name.
    pub app: Option<String>,
    /// Target environment label (e.g. `staging` / `prod`).
    pub env: Option<String>,
    /// Plugin-specific parameters (bucket name, base url, ...).
    pub params: BTreeMap<String, String>,
}

/// Errors a [`DeployPlugin`] can surface.
#[derive(Debug)]
pub enum DeployError {
    /// The requested rollback target was never published.
    NotPublished { module_id: String, version: String },
    /// An underlying I/O failure.
    Io(std::io::Error),
    /// A backend-specific failure.
    Backend(String),
}

impl std::fmt::Display for DeployError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeployError::NotPublished { module_id, version } => write!(
                f,
                "cannot rollback {module_id:?} to {version:?}: no published artifact"
            ),
            DeployError::Io(e) => write!(f, "deploy io error: {e}"),
            DeployError::Backend(m) => write!(f, "deploy backend error: {m}"),
        }
    }
}

impl std::error::Error for DeployError {}

impl From<std::io::Error> for DeployError {
    fn from(e: std::io::Error) -> Self {
        DeployError::Io(e)
    }
}

/// A pluggable deployment method for federated modules. One plugin = one way to
/// publish/revert an artifact. Implementations publish a new version
/// ([`DeployPlugin::deploy`]) and repoint to an already-published prior version
/// ([`DeployPlugin::rollback`]); both report the resulting [`ModuleDeployment`].
pub trait DeployPlugin {
    /// Unique name this plugin registers under.
    fn name(&self) -> &str;

    /// Publish `module`'s `artifact` and return the deployment now live. Must
    /// not mutate its inputs.
    fn deploy(
        &self,
        module: &DeployModule,
        artifact: &DeployArtifact,
        ctx: &DeployContext,
    ) -> Result<ModuleDeployment, DeployError>;

    /// Repoint `module` to an already-published `to_version` and return that
    /// deployment. A rollback never rebuilds.
    fn rollback(
        &self,
        module: &DeployModule,
        to_version: &str,
        ctx: &DeployContext,
    ) -> Result<ModuleDeployment, DeployError>;
}

/// A registry of [`DeployPlugin`]s keyed by name, so CI can pick a deployment
/// method by string without statically importing it. Names are unique.
#[derive(Default)]
pub struct DeployPluginRegistry {
    plugins: BTreeMap<String, Box<dyn DeployPlugin>>,
}

impl DeployPluginRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        DeployPluginRegistry::default()
    }

    /// Register a plugin under its [`DeployPlugin::name`].
    ///
    /// # Errors
    /// Returns the conflicting name if one is already registered (and `override_`
    /// is false), or if the plugin has an empty name.
    pub fn register(
        &mut self,
        plugin: Box<dyn DeployPlugin>,
        override_: bool,
    ) -> Result<&mut Self, String> {
        let name = plugin.name().to_string();
        if name.is_empty() {
            return Err("deploy plugin needs a non-empty name".to_string());
        }
        if !override_ && self.plugins.contains_key(&name) {
            return Err(format!("a deploy plugin named {name:?} is already registered"));
        }
        self.plugins.insert(name, plugin);
        Ok(self)
    }

    /// Whether a plugin is registered under `name`.
    pub fn has(&self, name: &str) -> bool {
        self.plugins.contains_key(name)
    }

    /// Resolve a plugin by name, or `None`.
    pub fn get(&self, name: &str) -> Option<&dyn DeployPlugin> {
        self.plugins.get(name).map(|p| p.as_ref())
    }

    /// The registered plugin names, sorted ascending.
    pub fn list(&self) -> Vec<String> {
        self.plugins.keys().cloned().collect()
    }
}

/// Reference [`DeployPlugin`] that performs no I/O: it records the deployment it
/// *would* make and returns it. Useful for dry-runs, plan previews, and tests.
/// The served url is derived from `ctx.params["baseUrl"]` (or a `noop://` base).
pub struct NoopDeployPlugin {
    name: String,
}

impl NoopDeployPlugin {
    /// Construct with the default name `"noop"`.
    pub fn new() -> Self {
        NoopDeployPlugin { name: "noop".to_string() }
    }

    /// Construct with a custom registry name.
    pub fn named(name: impl Into<String>) -> Self {
        NoopDeployPlugin { name: name.into() }
    }

    fn deployment(&self, module_id: &str, version: &str, ctx: &DeployContext) -> ModuleDeployment {
        let base = ctx
            .params
            .get("baseUrl")
            .map(String::as_str)
            .unwrap_or("noop://deploy");
        let base = base.strip_suffix('/').unwrap_or(base);
        ModuleDeployment {
            version: version.to_string(),
            url: format!("{base}/{module_id}/{version}/remoteEntry.js"),
        }
    }
}

impl Default for NoopDeployPlugin {
    fn default() -> Self {
        NoopDeployPlugin::new()
    }
}

impl DeployPlugin for NoopDeployPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    fn deploy(
        &self,
        module: &DeployModule,
        _artifact: &DeployArtifact,
        ctx: &DeployContext,
    ) -> Result<ModuleDeployment, DeployError> {
        Ok(self.deployment(&module.module_id, &module.version, ctx))
    }

    fn rollback(
        &self,
        module: &DeployModule,
        to_version: &str,
        ctx: &DeployContext,
    ) -> Result<ModuleDeployment, DeployError> {
        Ok(self.deployment(&module.module_id, to_version, ctx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module() -> DeployModule {
        DeployModule {
            module_id: "dashboard".into(),
            version: "1.2.3".into(),
            kind: ModuleKind::Route,
        }
    }

    #[test]
    fn noop_deploy_derives_url() {
        let p = NoopDeployPlugin::new();
        let mut ctx = DeployContext::default();
        ctx.params.insert("baseUrl".into(), "https://cdn.example/".into());
        let d = p.deploy(&module(), &DeployArtifact::default(), &ctx).unwrap();
        assert_eq!(d.version, "1.2.3");
        assert_eq!(d.url, "https://cdn.example/dashboard/1.2.3/remoteEntry.js");
    }

    #[test]
    fn registry_resolves_and_rejects_duplicates() {
        let mut reg = DeployPluginRegistry::new();
        reg.register(Box::new(NoopDeployPlugin::new()), false).unwrap();
        assert!(reg.has("noop"));
        assert_eq!(reg.list(), vec!["noop"]);
        // Duplicate without override is rejected.
        assert!(reg.register(Box::new(NoopDeployPlugin::new()), false).is_err());
        // Override succeeds.
        assert!(reg.register(Box::new(NoopDeployPlugin::named("noop")), true).is_ok());
        // Resolve + rollback round-trips.
        let d = reg
            .get("noop")
            .unwrap()
            .rollback(&module(), "1.0.0", &DeployContext::default())
            .unwrap();
        assert_eq!(d.version, "1.0.0");
    }
}
