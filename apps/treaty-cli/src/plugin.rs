//! The Treaty CLI plugin system.
//!
//! Two extension points let the CLI grow without editing its core:
//!
//!   * [`CliPlugin`] — registers one or more subcommands. The `treaty` binary
//!     wires its built-in commands as plugins and third parties can add more by
//!     pushing into a [`PluginRegistry`] before dispatch.
//!   * [`crate::bundler::BundlerBackend`] — abstracts dev/build over a concrete
//!     bundling tool, resolved from the project's `treaty.config`.
//!
//! The registry is intentionally tiny and dependency-free: a plugin contributes
//! [`SubcommandSpec`]s (name + help) and a dispatch closure, and the registry
//! resolves an invocation to the owning plugin. This keeps subcommand ownership
//! explicit and testable without pulling `clap`'s builder API into every plugin.

use std::collections::BTreeMap;

/// A subcommand a [`CliPlugin`] contributes: the verb the user types plus a
/// one-line help string surfaced by `treaty --help`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubcommandSpec {
    /// The subcommand verb (e.g. `"generate"`, `"affected"`). Must be unique
    /// across all registered plugins.
    pub name: String,
    /// A short, single-line description.
    pub about: String,
}

impl SubcommandSpec {
    /// Convenience constructor.
    pub fn new(name: impl Into<String>, about: impl Into<String>) -> Self {
        SubcommandSpec {
            name: name.into(),
            about: about.into(),
        }
    }
}

/// The outcome of a plugin handling an invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginOutcome {
    /// The command ran to completion successfully.
    Ok,
    /// The command ran but reported a failure (non-zero exit).
    Failed,
}

/// A CLI plugin: it declares the subcommands it owns and handles dispatch for
/// them. Built-in commands (`generate`, `build`, `dev`, `affected`, `compile`)
/// are themselves plugins, so the core and third-party extensions share one
/// uniform contract.
pub trait CliPlugin {
    /// A stable identifier for diagnostics (not user-visible).
    fn name(&self) -> &str;

    /// The subcommands this plugin contributes. Called once at registration to
    /// build the dispatch table; the names must be unique workspace-wide.
    fn subcommands(&self) -> Vec<SubcommandSpec>;

    /// Handle an invocation of one of this plugin's subcommands.
    ///
    /// `command` is the resolved subcommand name and `args` are the raw
    /// remaining arguments. Returns the outcome; errors are reported by the
    /// plugin (it owns its own diagnostics) and collapsed to
    /// [`PluginOutcome::Failed`].
    fn handle(&self, command: &str, args: &[String]) -> PluginOutcome;
}

/// A registry mapping subcommand names to the [`CliPlugin`] that owns them.
///
/// Registration is order-sensitive only for duplicate detection: registering a
/// plugin whose subcommand name collides with an already-registered one is a
/// hard error, so the command surface is always unambiguous.
#[derive(Default)]
pub struct PluginRegistry {
    plugins: Vec<Box<dyn CliPlugin>>,
    /// subcommand name -> index into `plugins`.
    routes: BTreeMap<String, usize>,
}

impl std::fmt::Debug for PluginRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginRegistry")
            .field("plugins", &self.plugins.iter().map(|p| p.name()).collect::<Vec<_>>())
            .field("commands", &self.routes.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Errors raised while building a [`PluginRegistry`].
#[derive(Debug, PartialEq, Eq)]
pub enum RegistryError {
    /// A plugin tried to register an empty subcommand name.
    EmptyName(String),
    /// Two plugins claimed the same subcommand name.
    Duplicate { command: String, existing: String, incoming: String },
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::EmptyName(plugin) => {
                write!(f, "plugin {plugin:?} declared an empty subcommand name")
            }
            RegistryError::Duplicate { command, existing, incoming } => write!(
                f,
                "subcommand {command:?} is already owned by plugin {existing:?}, \
                 cannot be re-registered by {incoming:?}"
            ),
        }
    }
}

impl std::error::Error for RegistryError {}

impl PluginRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        PluginRegistry::default()
    }

    /// Register a plugin, claiming each of its subcommand names.
    ///
    /// # Errors
    /// Returns [`RegistryError`] if any contributed name is empty or already
    /// owned by a previously-registered plugin. On error the registry is left
    /// unchanged (the plugin is not partially added).
    pub fn register(&mut self, plugin: Box<dyn CliPlugin>) -> Result<&mut Self, RegistryError> {
        let specs = plugin.subcommands();
        // Validate fully before mutating so registration is all-or-nothing.
        for spec in &specs {
            if spec.name.is_empty() {
                return Err(RegistryError::EmptyName(plugin.name().to_string()));
            }
            if let Some(&existing_idx) = self.routes.get(&spec.name) {
                return Err(RegistryError::Duplicate {
                    command: spec.name.clone(),
                    existing: self.plugins[existing_idx].name().to_string(),
                    incoming: plugin.name().to_string(),
                });
            }
        }
        let idx = self.plugins.len();
        for spec in &specs {
            self.routes.insert(spec.name.clone(), idx);
        }
        self.plugins.push(plugin);
        Ok(self)
    }

    /// Whether a subcommand name is registered.
    pub fn has(&self, command: &str) -> bool {
        self.routes.contains_key(command)
    }

    /// The plugin owning `command`, if any.
    pub fn plugin_for(&self, command: &str) -> Option<&dyn CliPlugin> {
        self.routes
            .get(command)
            .map(|&idx| self.plugins[idx].as_ref())
    }

    /// Every registered subcommand, sorted by name (deterministic for help text
    /// and tests).
    pub fn subcommands(&self) -> Vec<SubcommandSpec> {
        let mut out: Vec<SubcommandSpec> = self
            .plugins
            .iter()
            .flat_map(|p| p.subcommands())
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Registered subcommand names, sorted ascending.
    pub fn command_names(&self) -> Vec<String> {
        self.routes.keys().cloned().collect()
    }

    /// Dispatch `command` to its owning plugin.
    ///
    /// Returns `None` when no plugin owns the command (the caller decides how to
    /// report an unknown command); otherwise the plugin's [`PluginOutcome`].
    pub fn dispatch(&self, command: &str, args: &[String]) -> Option<PluginOutcome> {
        self.plugin_for(command).map(|p| p.handle(command, args))
    }
}

/// A built-in command plugin: it owns exactly the subcommands listed and is the
/// vocabulary the registry routes over. The actual dispatch is performed by
/// `main` against `clap`'s parsed values; [`CliPlugin::handle`] here is the
/// programmatic entry point (used by tests and any embedder that drives the CLI
/// without `clap`), reporting which subcommand ran.
pub struct BuiltinPlugin {
    id: &'static str,
    specs: Vec<SubcommandSpec>,
}

impl BuiltinPlugin {
    fn new(id: &'static str, specs: Vec<SubcommandSpec>) -> Self {
        BuiltinPlugin { id, specs }
    }
}

impl CliPlugin for BuiltinPlugin {
    fn name(&self) -> &str {
        self.id
    }
    fn subcommands(&self) -> Vec<SubcommandSpec> {
        self.specs.clone()
    }
    fn handle(&self, command: &str, _args: &[String]) -> PluginOutcome {
        // Built-ins are dispatched by `main`; routing here just confirms
        // ownership of the verb.
        if self.specs.iter().any(|s| s.name == command) {
            PluginOutcome::Ok
        } else {
            PluginOutcome::Failed
        }
    }
}

/// Build the registry seeded with Treaty's built-in command plugins. The binary
/// uses this to validate (and could use it to dispatch) its command surface;
/// third parties can register additional [`CliPlugin`]s on top.
pub fn build_default_registry() -> PluginRegistry {
    let mut reg = PluginRegistry::new();
    // Unwraps are safe: the built-in names are unique by construction. If a
    // future edit introduces a collision the unit test below catches it.
    reg.register(Box::new(BuiltinPlugin::new(
        "generate",
        vec![SubcommandSpec::new(
            "generate",
            "Scaffold a selectorless, signal, standalone source",
        )],
    )))
    .expect("built-in generate plugin");
    reg.register(Box::new(BuiltinPlugin::new(
        "bundle",
        vec![
            SubcommandSpec::new("build", "Produce a federation-ready production build"),
            SubcommandSpec::new("dev", "Start a development session"),
        ],
    )))
    .expect("built-in bundle plugin");
    reg.register(Box::new(BuiltinPlugin::new(
        "affected",
        vec![SubcommandSpec::new(
            "affected",
            "Compute the federated modules a change affects",
        )],
    )))
    .expect("built-in affected plugin");
    reg.register(Box::new(BuiltinPlugin::new(
        "compile",
        vec![SubcommandSpec::new("compile", "Compile a single source file")],
    )))
    .expect("built-in compile plugin");
    reg
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn default_registry_has_unique_builtin_commands() {
        let reg = build_default_registry();
        for cmd in ["generate", "build", "dev", "affected", "compile"] {
            assert!(reg.has(cmd), "missing {cmd}");
        }
        // generate is owned by the "generate" plugin, build/dev by "bundle".
        assert_eq!(reg.plugin_for("generate").unwrap().name(), "generate");
        assert_eq!(reg.plugin_for("build").unwrap().name(), "bundle");
        assert_eq!(reg.plugin_for("dev").unwrap().name(), "bundle");
        assert_eq!(reg.dispatch("build", &[]), Some(PluginOutcome::Ok));
    }

    struct StubPlugin {
        id: &'static str,
        specs: Vec<SubcommandSpec>,
        calls: Rc<Cell<u32>>,
    }

    impl CliPlugin for StubPlugin {
        fn name(&self) -> &str {
            self.id
        }
        fn subcommands(&self) -> Vec<SubcommandSpec> {
            self.specs.clone()
        }
        fn handle(&self, _command: &str, _args: &[String]) -> PluginOutcome {
            self.calls.set(self.calls.get() + 1);
            PluginOutcome::Ok
        }
    }

    #[test]
    fn registers_and_routes_to_owner() {
        let mut reg = PluginRegistry::new();
        let calls = Rc::new(Cell::new(0));
        reg.register(Box::new(StubPlugin {
            id: "gen",
            specs: vec![SubcommandSpec::new("generate", "scaffold")],
            calls: calls.clone(),
        }))
        .unwrap();

        assert!(reg.has("generate"));
        assert_eq!(reg.plugin_for("generate").unwrap().name(), "gen");
        assert_eq!(reg.dispatch("generate", &[]), Some(PluginOutcome::Ok));
        assert_eq!(calls.get(), 1);
        assert_eq!(reg.dispatch("unknown", &[]), None);
    }

    #[test]
    fn duplicate_subcommand_is_rejected() {
        let mut reg = PluginRegistry::new();
        reg.register(Box::new(StubPlugin {
            id: "a",
            specs: vec![SubcommandSpec::new("build", "build a")],
            calls: Rc::new(Cell::new(0)),
        }))
        .unwrap();

        let err = reg
            .register(Box::new(StubPlugin {
                id: "b",
                specs: vec![SubcommandSpec::new("build", "build b")],
                calls: Rc::new(Cell::new(0)),
            }))
            .unwrap_err();

        match err {
            RegistryError::Duplicate { command, existing, incoming } => {
                assert_eq!(command, "build");
                assert_eq!(existing, "a");
                assert_eq!(incoming, "b");
            }
            other => panic!("expected duplicate error, got {other:?}"),
        }
        // The failed registration must not have leaked routes.
        assert_eq!(reg.plugin_for("build").unwrap().name(), "a");
    }

    #[test]
    fn empty_name_is_rejected() {
        let mut reg = PluginRegistry::new();
        let err = reg
            .register(Box::new(StubPlugin {
                id: "bad",
                specs: vec![SubcommandSpec::new("", "no name")],
                calls: Rc::new(Cell::new(0)),
            }))
            .unwrap_err();
        assert_eq!(err, RegistryError::EmptyName("bad".to_string()));
    }

    #[test]
    fn subcommands_are_sorted() {
        let mut reg = PluginRegistry::new();
        reg.register(Box::new(StubPlugin {
            id: "p",
            specs: vec![
                SubcommandSpec::new("zeta", "z"),
                SubcommandSpec::new("alpha", "a"),
            ],
            calls: Rc::new(Cell::new(0)),
        }))
        .unwrap();
        let names: Vec<_> = reg.subcommands().into_iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["alpha", "zeta"]);
    }
}
