//! `angular.json` workspace parsing + architect-target resolution.
//!
//! This is the bridge that makes `treaty` 1:1 with `ng` for **workspace-driven**
//! projects. The Angular CLI describes a workspace in `angular.json`: a set of
//! named *projects*, each with an `architect` map of *targets* (`build`, `serve`,
//! `test`, `lint`, …). A target names a *builder* (e.g.
//! `@angular/build:application`) and carries `options` plus per-configuration
//! `configurations` overrides (`production`, `development`, …).
//!
//! `ng build my-app -c production` means: take project `my-app`'s `build` target,
//! merge its base `options` with the `production` configuration's overrides, and
//! run the named builder with the result. This module reproduces exactly that
//! resolution in Rust ([`resolve_architect_target`]) and exposes the option keys
//! Treaty's native pipelines consume (`outputPath`, `browser`/`main`, `index`,
//! `tsConfig`, `styles`, `assets`).
//!
//! Treaty does NOT re-implement the builders' bundling internals — it maps the
//! resolved target onto its OWN native pipelines (the Rust module-graph build, the
//! tokio/axum serve, the compiler). For targets Treaty cannot run natively
//! (`test`, `lint`, custom third-party builders) the CLI is honest and shells out
//! to the real builder via Node, the same way it does for schematics/migrations.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

/// A parsed `angular.json` workspace: its projects keyed by name, plus the file's
/// own location (so option paths can be resolved relative to the workspace root).
#[derive(Debug, Clone)]
pub struct Workspace {
    /// Absolute path to the `angular.json` file that produced this workspace.
    pub path: PathBuf,
    /// Absolute directory the `angular.json` lives in — the workspace root that
    /// every project/option relative path is anchored to.
    pub root: PathBuf,
    /// `newProjectRoot` (defaults to `projects`); where `ng generate application`
    /// and friends place new projects.
    pub new_project_root: Option<String>,
    /// The default project name (legacy `defaultProject`, still honored as a
    /// convenience fallback when no project is named on the command line).
    pub default_project: Option<String>,
    /// Projects keyed by name.
    pub projects: BTreeMap<String, Project>,
}

/// A single workspace project.
#[derive(Debug, Clone, Deserialize)]
pub struct Project {
    /// `application` | `library` (absent for some tool projects).
    #[serde(rename = "projectType", default)]
    pub project_type: Option<String>,
    /// Project root, relative to the workspace root.
    #[serde(default)]
    pub root: String,
    /// Source root (`src`), relative to the workspace root.
    #[serde(rename = "sourceRoot", default)]
    pub source_root: Option<String>,
    /// Selector/file prefix (`app`).
    #[serde(default)]
    pub prefix: Option<String>,
    /// Per-project schematic defaults (`@schematics/angular:component`: { style }).
    /// Kept as raw JSON — they are passed through to the real schematics runner.
    #[serde(default)]
    pub schematics: Option<Value>,
    /// The architect targets (`build`, `serve`, `test`, `lint`, …).
    #[serde(default)]
    pub architect: BTreeMap<String, ArchitectTarget>,
}

/// One architect target: a builder plus base options and named configurations.
#[derive(Debug, Clone, Deserialize)]
pub struct ArchitectTarget {
    /// The builder id, e.g. `@angular/build:application`.
    pub builder: String,
    /// Base options (merged under any selected configuration).
    #[serde(default)]
    pub options: Value,
    /// Named configuration overrides (`production`, `development`, …).
    #[serde(default)]
    pub configurations: BTreeMap<String, Value>,
    /// The configuration applied when none is named on the command line.
    #[serde(rename = "defaultConfiguration", default)]
    pub default_configuration: Option<String>,
}

/// The on-disk shape we deserialize `angular.json` into before post-processing.
#[derive(Debug, Deserialize)]
struct RawWorkspace {
    #[serde(rename = "newProjectRoot", default)]
    new_project_root: Option<String>,
    #[serde(rename = "defaultProject", default)]
    default_project: Option<String>,
    #[serde(default)]
    projects: BTreeMap<String, Project>,
}

/// Errors parsing/resolving a workspace.
#[derive(Debug)]
pub enum AngularError {
    Io(std::io::Error),
    Parse(serde_json::Error),
    NoProject(String),
    NoTarget { project: String, target: String },
    NoConfiguration { project: String, target: String, configuration: String },
    AmbiguousDefaultProject(Vec<String>),
}

impl std::fmt::Display for AngularError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AngularError::Io(e) => write!(f, "cannot read angular.json: {e}"),
            AngularError::Parse(e) => write!(f, "invalid angular.json: {e}"),
            AngularError::NoProject(p) => write!(f, "no such project {p:?} in angular.json"),
            AngularError::NoTarget { project, target } => {
                write!(f, "project {project:?} has no architect target {target:?}")
            }
            AngularError::NoConfiguration { project, target, configuration } => write!(
                f,
                "target {project:?}:{target:?} has no configuration {configuration:?}"
            ),
            AngularError::AmbiguousDefaultProject(names) => write!(
                f,
                "no project named and no unambiguous default (candidates: {})",
                names.join(", ")
            ),
        }
    }
}

impl std::error::Error for AngularError {}

/// Walk up from `start` looking for an `angular.json`, returning the first found.
pub fn find_angular_json(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let candidate = d.join("angular.json");
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = d.parent();
    }
    None
}

/// Parse a workspace from an `angular.json` file path.
pub fn parse_workspace(path: &Path) -> Result<Workspace, AngularError> {
    let text = std::fs::read_to_string(path).map_err(AngularError::Io)?;
    parse_workspace_str(&text, path)
}

/// Parse a workspace from in-memory text (split out for unit testing).
pub fn parse_workspace_str(text: &str, path: &Path) -> Result<Workspace, AngularError> {
    let raw: RawWorkspace = serde_json::from_str(text).map_err(AngularError::Parse)?;
    let root = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    Ok(Workspace {
        path: path.to_path_buf(),
        root,
        new_project_root: raw.new_project_root,
        default_project: raw.default_project,
        projects: raw.projects,
    })
}

impl Workspace {
    /// Resolve the project to operate on. Precedence:
    ///   1. an explicitly named project,
    ///   2. the legacy `defaultProject`,
    ///   3. the sole project when the workspace has exactly one,
    /// else an [`AngularError::AmbiguousDefaultProject`] / [`AngularError::NoProject`].
    pub fn resolve_project_name(&self, named: Option<&str>) -> Result<String, AngularError> {
        if let Some(name) = named {
            return if self.projects.contains_key(name) {
                Ok(name.to_string())
            } else {
                Err(AngularError::NoProject(name.to_string()))
            };
        }
        if let Some(default) = &self.default_project {
            if self.projects.contains_key(default) {
                return Ok(default.clone());
            }
        }
        let names: Vec<String> = self.projects.keys().cloned().collect();
        match names.len() {
            1 => Ok(names.into_iter().next().unwrap()),
            _ => Err(AngularError::AmbiguousDefaultProject(names)),
        }
    }

    /// Look up a project by name.
    pub fn project(&self, name: &str) -> Result<&Project, AngularError> {
        self.projects
            .get(name)
            .ok_or_else(|| AngularError::NoProject(name.to_string()))
    }
}

/// A fully-resolved architect target: its builder and the merged option object
/// (base options overlaid with the selected configuration), plus a typed view of
/// the option keys Treaty's pipelines consume. All paths are absolute (anchored
/// to the workspace root or, when present, the project root).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTarget {
    /// The project this target belongs to.
    pub project: String,
    /// The target name (`build`, `serve`, …).
    pub target: String,
    /// The configuration applied (after default-configuration fallback), if any.
    pub configuration: Option<String>,
    /// The builder id, e.g. `@angular/build:application`.
    pub builder: String,
    /// The merged option object (raw JSON), exactly what a builder would receive.
    pub options: Value,
    /// `outputPath` resolved to an absolute dir, when present.
    pub output_path: Option<PathBuf>,
    /// The entry module — `browser` (new application builder) or `main` (legacy),
    /// resolved absolute.
    pub main: Option<PathBuf>,
    /// `index` HTML resolved absolute.
    pub index: Option<PathBuf>,
    /// `tsConfig` resolved absolute.
    pub ts_config: Option<PathBuf>,
    /// `styles` entries resolved absolute (string entries only; object entries are
    /// passed through in `options`).
    pub styles: Vec<PathBuf>,
    /// `assets` entries resolved absolute (string entries only).
    pub assets: Vec<PathBuf>,
}

/// Merge `overlay`'s keys onto `base` (shallow object merge — Angular's
/// configuration merge is a top-level key override, arrays/objects replaced
/// wholesale). Non-object values: `overlay` wins.
fn merge_options(base: &Value, overlay: &Value) -> Value {
    match (base, overlay) {
        (Value::Object(b), Value::Object(o)) => {
            let mut out = b.clone();
            for (k, v) in o {
                out.insert(k.clone(), v.clone());
            }
            Value::Object(out)
        }
        // A non-object overlay replaces the base entirely.
        (_, o) => o.clone(),
    }
}

/// Resolve a relative option path against the workspace root.
fn abs_under(root: &Path, rel: &str) -> PathBuf {
    let p = Path::new(rel);
    if p.is_absolute() { p.to_path_buf() } else { root.join(p) }
}

/// Extract a string field from an option object.
fn opt_str<'a>(opts: &'a Value, key: &str) -> Option<&'a str> {
    opts.get(key).and_then(Value::as_str)
}

/// Collect the plain-string entries of an option array (e.g. `styles`, `assets`),
/// resolved absolute. Object-form entries (`{ input, bundleName }`) are left in
/// the raw `options` for a builder that understands them.
fn opt_str_array(opts: &Value, key: &str, root: &Path) -> Vec<PathBuf> {
    opts.get(key)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(|s| abs_under(root, s))
                .collect()
        })
        .unwrap_or_default()
}

/// Resolve a project's architect target, merging base options with the selected
/// configuration (falling back to the target's `defaultConfiguration` when none
/// is named), and projecting the option keys Treaty's pipelines consume.
///
/// `config` of `None` uses the target's `defaultConfiguration` if it declares one;
/// pass `Some("")` is treated the same as `None`.
pub fn resolve_architect_target(
    ws: &Workspace,
    project: &str,
    target: &str,
    config: Option<&str>,
) -> Result<ResolvedTarget, AngularError> {
    let proj = ws.project(project)?;
    let arch = proj.architect.get(target).ok_or_else(|| AngularError::NoTarget {
        project: project.to_string(),
        target: target.to_string(),
    })?;

    // Determine the effective configuration: an explicit non-empty name, else the
    // target's declared default.
    let requested = config.filter(|c| !c.is_empty());
    let effective_config = requested
        .map(str::to_string)
        .or_else(|| arch.default_configuration.clone());

    // Merge base options with the selected configuration's overrides.
    let mut options = arch.options.clone();
    if let Some(cfg) = &effective_config {
        match arch.configurations.get(cfg) {
            Some(overrides) => options = merge_options(&options, overrides),
            None => {
                // An EXPLICITLY requested but missing configuration is an error
                // (matches `ng`). A missing *default* configuration is tolerated.
                if requested.is_some() {
                    return Err(AngularError::NoConfiguration {
                        project: project.to_string(),
                        target: target.to_string(),
                        configuration: cfg.clone(),
                    });
                }
            }
        }
    }

    // Anchor option paths. `outputPath` may itself be an object
    // (`{ base, browser }`) in the new builder; we only project the string form.
    let root = &ws.root;
    let output_path = opt_str(&options, "outputPath")
        .map(|s| abs_under(root, s))
        .or_else(|| {
            options
                .get("outputPath")
                .and_then(|v| v.get("base"))
                .and_then(Value::as_str)
                .map(|s| abs_under(root, s))
        });
    let main = opt_str(&options, "browser")
        .or_else(|| opt_str(&options, "main"))
        .map(|s| abs_under(root, s));
    let index = opt_str(&options, "index").map(|s| abs_under(root, s));
    let ts_config = opt_str(&options, "tsConfig").map(|s| abs_under(root, s));
    let styles = opt_str_array(&options, "styles", root);
    let assets = opt_str_array(&options, "assets", root);

    Ok(ResolvedTarget {
        project: project.to_string(),
        target: target.to_string(),
        configuration: effective_config,
        builder: arch.builder.clone(),
        options,
        output_path,
        main,
        index,
        ts_config,
        styles,
        assets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NG_BENCH: &str = r#"{
      "version": 1,
      "newProjectRoot": "projects",
      "projects": {
        "ng-bench-app": {
          "projectType": "application",
          "schematics": { "@schematics/angular:component": { "style": "css" } },
          "root": "",
          "sourceRoot": "src",
          "prefix": "app",
          "architect": {
            "build": {
              "builder": "@angular/build:application",
              "options": {
                "outputPath": "dist/ng-bench-app",
                "index": "src/index.html",
                "browser": "src/main.ts",
                "tsConfig": "tsconfig.app.json",
                "styles": ["src/styles.css"]
              },
              "configurations": {
                "production": { "outputHashing": "all" },
                "development": { "optimization": false, "sourceMap": true }
              },
              "defaultConfiguration": "production"
            },
            "serve": {
              "builder": "@angular/build:dev-server",
              "configurations": {
                "production": { "buildTarget": "ng-bench-app:build:production" },
                "development": { "buildTarget": "ng-bench-app:build:development" }
              },
              "defaultConfiguration": "development"
            }
          }
        }
      }
    }"#;

    fn ws() -> Workspace {
        parse_workspace_str(NG_BENCH, Path::new("/work/angular.json")).unwrap()
    }

    #[test]
    fn parses_projects_and_targets() {
        let ws = ws();
        assert_eq!(ws.projects.len(), 1);
        let p = ws.project("ng-bench-app").unwrap();
        assert_eq!(p.project_type.as_deref(), Some("application"));
        assert_eq!(p.source_root.as_deref(), Some("src"));
        assert_eq!(p.prefix.as_deref(), Some("app"));
        assert!(p.architect.contains_key("build"));
        assert!(p.architect.contains_key("serve"));
    }

    #[test]
    fn sole_project_is_the_default() {
        let ws = ws();
        assert_eq!(ws.resolve_project_name(None).unwrap(), "ng-bench-app");
        assert_eq!(ws.resolve_project_name(Some("ng-bench-app")).unwrap(), "ng-bench-app");
        assert!(matches!(
            ws.resolve_project_name(Some("nope")),
            Err(AngularError::NoProject(_))
        ));
    }

    #[test]
    fn resolves_build_target_with_default_configuration() {
        let ws = ws();
        // No config named -> the target's defaultConfiguration ("production") applies.
        let t = resolve_architect_target(&ws, "ng-bench-app", "build", None).unwrap();
        assert_eq!(t.builder, "@angular/build:application");
        assert_eq!(t.configuration.as_deref(), Some("production"));
        // Base option survives.
        assert!(t.main.unwrap().ends_with("src/main.ts"));
        assert!(t.index.unwrap().ends_with("src/index.html"));
        assert!(t.output_path.unwrap().ends_with("dist/ng-bench-app"));
        assert!(t.ts_config.unwrap().ends_with("tsconfig.app.json"));
        assert_eq!(t.styles.len(), 1);
        assert!(t.styles[0].ends_with("src/styles.css"));
        // The production override merged in.
        assert_eq!(t.options.get("outputHashing").and_then(|v| v.as_str()), Some("all"));
    }

    #[test]
    fn explicit_development_configuration_overrides_base() {
        let ws = ws();
        let t = resolve_architect_target(&ws, "ng-bench-app", "build", Some("development")).unwrap();
        assert_eq!(t.configuration.as_deref(), Some("development"));
        assert_eq!(t.options.get("optimization").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(t.options.get("sourceMap").and_then(|v| v.as_bool()), Some(true));
        // Base options still present after the merge.
        assert!(t.main.unwrap().ends_with("src/main.ts"));
    }

    #[test]
    fn missing_explicit_configuration_errors() {
        let ws = ws();
        let err = resolve_architect_target(&ws, "ng-bench-app", "build", Some("staging")).unwrap_err();
        assert!(matches!(err, AngularError::NoConfiguration { .. }));
    }

    #[test]
    fn missing_target_errors() {
        let ws = ws();
        let err = resolve_architect_target(&ws, "ng-bench-app", "lint", None).unwrap_err();
        assert!(matches!(err, AngularError::NoTarget { .. }));
    }

    #[test]
    fn serve_target_resolves_its_build_target_reference() {
        let ws = ws();
        let t = resolve_architect_target(&ws, "ng-bench-app", "serve", None).unwrap();
        assert_eq!(t.builder, "@angular/build:dev-server");
        assert_eq!(t.configuration.as_deref(), Some("development"));
        assert_eq!(
            t.options.get("buildTarget").and_then(|v| v.as_str()),
            Some("ng-bench-app:build:development")
        );
    }

    #[test]
    fn find_angular_json_walks_up() {
        let dir = std::env::temp_dir().join(format!("treaty-ngjson-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let nested = dir.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(dir.join("angular.json"), NG_BENCH).unwrap();
        let found = find_angular_json(&nested).expect("walks up to angular.json");
        assert_eq!(found, dir.join("angular.json"));
        assert!(find_angular_json(&dir.parent().unwrap().join("definitely-not-here")).is_none()
            || find_angular_json(Path::new("/")).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
