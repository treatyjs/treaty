//! Convention-based project configuration for the standalone Treaty CLI.
//!
//! A Treaty project has **no `angular.json`**. It is described either by a tiny
//! optional `treaty.config.json` file or — when that is absent — purely by
//! conventions (`index.html` + `src/main.ts`, output to `dist/`). This is the
//! Rust port of the TypeScript `@treaty/cli` config resolver; it keeps the same
//! defaults and precedence (built-in defaults -> config file -> CLI overrides).
//!
//! Treaty is a compiler, not a host: this module only resolves *where* things
//! are and *whether* the app federates. The `build`/`dev` commands hand the
//! resolved config to a [`crate::bundler::BundlerBackend`], which owns the
//! bundler integration and the automatic Module Federation wiring.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::angular::{self, AngularError, ResolvedTarget, Workspace};

/// Which bundler the CLI drives. Mirrors the external tools the
/// [`crate::bundler::ExternalBundler`] can delegate to, plus the in-process
/// Rust-native fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Bundler {
    Rspack,
    Rsbuild,
    Vite,
    /// The documented Rust-native fallback backend (no external toolchain).
    Native,
}

impl Bundler {
    /// The `--bundler` / config token for this bundler.
    pub fn as_str(self) -> &'static str {
        match self {
            Bundler::Rspack => "rspack",
            Bundler::Rsbuild => "rsbuild",
            Bundler::Vite => "vite",
            Bundler::Native => "native",
        }
    }

    /// Parse a bundler token, case-insensitively.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "rspack" => Some(Bundler::Rspack),
            "rsbuild" => Some(Bundler::Rsbuild),
            "vite" => Some(Bundler::Vite),
            "native" => Some(Bundler::Native),
            _ => None,
        }
    }
}

/// Default bundler when neither config nor `--bundler` selects one. Vite is the
/// zero-config dev default, matching the TypeScript CLI.
pub const DEFAULT_BUNDLER: Bundler = Bundler::Vite;
/// Default app entry module (a standalone bootstrap).
pub const DEFAULT_ENTRY: &str = "src/main.ts";
/// Default build output directory.
pub const DEFAULT_OUT_DIR: &str = "dist";
/// Default dev-server host.
pub const DEFAULT_HOST: &str = "localhost";
/// Default dev-server port.
pub const DEFAULT_PORT: u16 = 4200;

/// The config-file basenames the CLI looks for, in resolution order. The Rust
/// CLI reads JSON only (the `.ts`/`.mjs` factory forms are a JS-runtime concern
/// the TypeScript CLI owned); a JSON config keeps the Rust driver dependency-free.
pub const CONFIG_FILENAMES: &[&str] = &["treaty.config.json"];

/// Automatic Module Federation options. Every Treaty app is a federation host
/// by default; this only names the app and declares the remotes it consumes and
/// the modules it exposes. Set [`TreatyConfig::module_federation`] to `false` to
/// opt out entirely.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MfOptions {
    /// The federated app/remote name. Defaults to the project directory name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Remote module ids this app consumes (`name -> url`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remotes: Vec<String>,
    /// Module paths this app exposes as remotes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exposes: Vec<String>,
    /// Extra shared dependencies (beyond the Angular runtime singletons).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared: Vec<String>,
}

/// Module Federation setting: either explicit [`MfOptions`], or a bare bool to
/// turn the zero-config host on (`true`, the default) or off (`false`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Federation {
    /// `true` = zero-config host on; `false` = opt out.
    Toggle(bool),
    /// Explicit federation options (implies on).
    Options(MfOptions),
}

impl Default for Federation {
    fn default() -> Self {
        Federation::Toggle(true)
    }
}

impl Federation {
    /// Whether federation is enabled at all.
    pub fn is_enabled(&self) -> bool {
        match self {
            Federation::Toggle(on) => *on,
            Federation::Options(_) => true,
        }
    }

    /// The resolved options when enabled (defaults when only a `true` toggle).
    pub fn options(&self) -> Option<MfOptions> {
        match self {
            Federation::Toggle(true) => Some(MfOptions::default()),
            Federation::Toggle(false) => None,
            Federation::Options(o) => Some(o.clone()),
        }
    }
}

/// The shape of a `treaty.config.json` file (every field optional). Most apps
/// need none of it — defaults + conventions cover a standalone, federation-ready
/// app.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreatyConfig {
    /// Which bundler `dev`/`build` should use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundler: Option<Bundler>,
    /// Project root, relative to the config file (or cwd).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// The app's entry module.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    /// The dir built output is emitted to.
    #[serde(default, rename = "outDir", skip_serializing_if = "Option::is_none")]
    pub out_dir: Option<String>,
    /// The public base path for built assets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// Dev-server host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Dev-server port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Automatic Module Federation options (or a bare on/off toggle).
    #[serde(default, rename = "moduleFederation", skip_serializing_if = "Option::is_none")]
    pub module_federation: Option<Federation>,
}

/// A fully-resolved config: every field present, all paths absolute. The single
/// shape the dev/build commands consume so they never re-apply defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedConfig {
    pub bundler: Bundler,
    pub root: PathBuf,
    pub entry: PathBuf,
    pub out_dir: PathBuf,
    pub base: String,
    pub host: String,
    pub port: u16,
    /// Module Federation: enabled by default, with resolved options.
    pub module_federation: Federation,
    /// Absolute path of the config file that was loaded, or `None` for pure
    /// conventions.
    pub config_file: Option<PathBuf>,
}

/// Options that override file/convention config (typically parsed from argv).
#[derive(Debug, Clone, Default)]
pub struct ConfigOverrides {
    pub bundler: Option<Bundler>,
    pub root: Option<String>,
    pub out_dir: Option<String>,
    pub base: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    /// When set, disable Module Federation regardless of config (the
    /// `--no-federation` flag).
    pub disable_federation: bool,
    /// An explicit config-file path; skips auto-discovery when set.
    pub config_file: Option<String>,
}

/// Resolve a possibly-relative path against a base directory to an absolute one.
fn to_absolute(base: &Path, target: &str) -> PathBuf {
    let p = Path::new(target);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}

/// Locate the project's config file under `root`, or `None` if none exists.
/// Pure filesystem probing — it does not load the file.
pub fn find_config_file(root: &Path) -> Option<PathBuf> {
    for name in CONFIG_FILENAMES {
        let candidate = root.join(name);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// Errors loading/parsing a config file.
#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Parse(serde_json::Error),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io(e) => write!(f, "cannot read config: {e}"),
            ConfigError::Parse(e) => write!(f, "invalid treaty.config.json: {e}"),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Load and parse a `treaty.config.json` into a raw [`TreatyConfig`].
pub fn load_config_file(file: &Path) -> Result<TreatyConfig, ConfigError> {
    let text = std::fs::read_to_string(file).map_err(ConfigError::Io)?;
    parse_config(&text)
}

/// Parse config text (split out so it is unit-testable without disk I/O).
pub fn parse_config(text: &str) -> Result<TreatyConfig, ConfigError> {
    serde_json::from_str(text).map_err(ConfigError::Parse)
}

/// Resolve the effective [`ResolvedConfig`]. Precedence, lowest to highest:
/// built-in defaults -> loaded `treaty.config.json` -> CLI overrides. `cwd`
/// seeds the root when nothing else specifies it. Paths in the result are
/// absolute.
pub fn resolve_config(cwd: &Path, overrides: &ConfigOverrides) -> Result<ResolvedConfig, ConfigError> {
    let initial_root = to_absolute(cwd, overrides.root.as_deref().unwrap_or("."));

    let config_file = match &overrides.config_file {
        Some(c) => Some(to_absolute(cwd, c)),
        None => find_config_file(&initial_root),
    };
    let file_config = match &config_file {
        Some(f) => load_config_file(f)?,
        None => TreatyConfig::default(),
    };

    // The config file may itself relocate the root; re-anchor against it.
    let root = match &file_config.root {
        Some(r) => to_absolute(&initial_root, r),
        None => initial_root.clone(),
    };

    let bundler = overrides
        .bundler
        .or(file_config.bundler)
        .unwrap_or(DEFAULT_BUNDLER);

    let module_federation = if overrides.disable_federation {
        Federation::Toggle(false)
    } else {
        file_config.module_federation.clone().unwrap_or_default()
    };

    Ok(ResolvedConfig {
        bundler,
        entry: to_absolute(&root, file_config.entry.as_deref().unwrap_or(DEFAULT_ENTRY)),
        out_dir: to_absolute(
            &root,
            overrides
                .out_dir
                .as_deref()
                .or(file_config.out_dir.as_deref())
                .unwrap_or(DEFAULT_OUT_DIR),
        ),
        base: overrides
            .base
            .clone()
            .or(file_config.base)
            .unwrap_or_else(|| "/".to_string()),
        host: overrides
            .host
            .clone()
            .or(file_config.host)
            .unwrap_or_else(|| DEFAULT_HOST.to_string()),
        port: overrides.port.or(file_config.port).unwrap_or(DEFAULT_PORT),
        module_federation,
        config_file,
        root,
    })
}

/// A config resolved FROM an `angular.json` architect target. Unlike
/// [`ResolvedConfig`] (the convention/`treaty.config.json` path), this carries the
/// full resolved architect target so the build/serve commands can honor
/// `outputPath`/`browser`/`index`/`tsConfig`/`styles`/`assets`.
#[derive(Debug, Clone)]
pub struct AngularResolvedConfig {
    /// The parsed workspace.
    pub workspace: Workspace,
    /// The resolved architect target (builder + merged options + projected paths).
    pub target: ResolvedTarget,
    /// The entry module (`browser`/`main`), absolute. Falls back to
    /// `<sourceRoot>/main.ts` when the target names none.
    pub entry: PathBuf,
    /// The output directory (`outputPath`), absolute. Falls back to `dist/<project>`.
    pub out_dir: PathBuf,
    /// The project root (workspace root + project `root`), absolute.
    pub project_root: PathBuf,
}

/// The source a build/serve config was resolved from: an `angular.json` architect
/// target, or the convention/`treaty.config.json` path.
#[derive(Debug, Clone)]
pub enum ProjectConfig {
    /// Resolved from `angular.json`.
    Angular(Box<AngularResolvedConfig>),
    /// Resolved from `treaty.config.json` / conventions.
    Treaty(ResolvedConfig),
}

/// Errors resolving a project's config.
#[derive(Debug)]
pub enum ResolveError {
    /// An `angular.json` error (parse / project / target resolution).
    Angular(AngularError),
    /// A `treaty.config.json` error.
    Config(ConfigError),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::Angular(e) => write!(f, "{e}"),
            ResolveError::Config(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ResolveError {}

/// Resolve a build/serve config, preferring `angular.json` when one is present
/// (walking up from `root`), and falling back to `treaty.config.json` /
/// conventions otherwise.
///
/// `project`/`configuration`/`target` select the architect target on the
/// angular.json path; they are ignored on the treaty.config path (which is
/// single-app by convention). `overrides` supply CLI flag overrides for the
/// fallback path.
pub fn resolve_project(
    root: &Path,
    project: Option<&str>,
    target: &str,
    configuration: Option<&str>,
    overrides: &ConfigOverrides,
) -> Result<ProjectConfig, ResolveError> {
    if let Some(ng_path) = angular::find_angular_json(root) {
        let ws = angular::parse_workspace(&ng_path).map_err(ResolveError::Angular)?;
        let project_name = ws
            .resolve_project_name(project)
            .map_err(ResolveError::Angular)?;
        let resolved = angular::resolve_architect_target(&ws, &project_name, target, configuration)
            .map_err(ResolveError::Angular)?;

        let proj = ws.project(&project_name).map_err(ResolveError::Angular)?;
        let project_root = if proj.root.is_empty() {
            ws.root.clone()
        } else {
            ws.root.join(&proj.root)
        };
        let source_root = proj
            .source_root
            .clone()
            .unwrap_or_else(|| "src".to_string());

        let entry = resolved
            .main
            .clone()
            .unwrap_or_else(|| ws.root.join(&source_root).join("main.ts"));
        let out_dir = resolved
            .output_path
            .clone()
            .unwrap_or_else(|| ws.root.join("dist").join(&project_name));

        return Ok(ProjectConfig::Angular(Box::new(AngularResolvedConfig {
            workspace: ws,
            target: resolved,
            entry,
            out_dir,
            project_root,
        })));
    }

    let cfg = resolve_config(root, overrides).map_err(ResolveError::Config)?;
    Ok(ProjectConfig::Treaty(cfg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_apply_without_a_config_file() {
        let cwd = std::env::temp_dir().join(format!("treaty-cfg-{}-none", std::process::id()));
        let _ = std::fs::remove_dir_all(&cwd);
        std::fs::create_dir_all(&cwd).unwrap();
        let cfg = resolve_config(&cwd, &ConfigOverrides::default()).unwrap();
        assert_eq!(cfg.bundler, DEFAULT_BUNDLER);
        assert_eq!(cfg.port, DEFAULT_PORT);
        assert!(cfg.config_file.is_none());
        assert!(cfg.module_federation.is_enabled());
        assert!(cfg.entry.ends_with("src/main.ts"));
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn config_file_and_overrides_compose() {
        let json = r#"{ "bundler": "rspack", "port": 5000, "moduleFederation": { "name": "shop" } }"#;
        let parsed = parse_config(json).unwrap();
        assert_eq!(parsed.bundler, Some(Bundler::Rspack));
        assert_eq!(parsed.port, Some(5000));
        assert_eq!(
            parsed.module_federation.as_ref().unwrap().options().unwrap().name.as_deref(),
            Some("shop")
        );
    }

    #[test]
    fn override_disables_federation() {
        let cwd = std::env::temp_dir().join(format!("treaty-cfg-{}-fed", std::process::id()));
        let _ = std::fs::remove_dir_all(&cwd);
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(
            cwd.join("treaty.config.json"),
            r#"{ "moduleFederation": { "name": "x" } }"#,
        )
        .unwrap();
        let ov = ConfigOverrides { disable_federation: true, ..Default::default() };
        let cfg = resolve_config(&cwd, &ov).unwrap();
        assert!(!cfg.module_federation.is_enabled());
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn federation_toggle_false_parses() {
        let parsed = parse_config(r#"{ "moduleFederation": false }"#).unwrap();
        assert!(!parsed.module_federation.unwrap().is_enabled());
    }

    #[test]
    fn bundler_roundtrips() {
        for b in [Bundler::Rspack, Bundler::Rsbuild, Bundler::Vite, Bundler::Native] {
            assert_eq!(Bundler::parse(b.as_str()), Some(b));
        }
        assert_eq!(Bundler::parse("RSPACK"), Some(Bundler::Rspack));
        assert!(Bundler::parse("webpack").is_none());
    }
}
