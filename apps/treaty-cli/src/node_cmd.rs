//! The Node bridge: run the REAL `@angular-devkit` toolchain for the parts of the
//! Angular CLI that are intrinsically Node programs — schematics (`generate`,
//! `new`) and `ng update` migrations.
//!
//! Treaty's compile/build/serve are pure Rust. But schematics and migrations ARE
//! the `@angular-devkit/schematics` + `@schematics/angular` packages: they are
//! TypeScript/JS programs with their own template engine, virtual filesystem, and
//! task runners. Re-implementing them would diverge from Angular the moment a
//! schematic changes upstream. So Treaty does the honest, 1:1 thing: it spawns
//! Node running the genuine `NodeWorkflow`, resolved from the project's own
//! `node_modules` (with a fallback resolution root for workspaces whose devkit
//! lives at the repo root), and passes the user's arguments straight through.
//!
//! This module:
//!   * locates a `node` executable,
//!   * materializes the embedded JS driver scripts into a temp dir,
//!   * spawns Node with the resolved arguments, streaming stdout/stderr through,
//!   * reports a clean error when Node or the devkit is absent (so a Treaty user
//!     without a Node toolchain gets a precise message, not a stack trace).

use std::path::{Path, PathBuf};
use std::process::Command;

/// The embedded schematics driver (runs `@angular-devkit/schematics`'
/// `NodeWorkflow` against a named collection + schematic). See
/// `js/run-schematic.mjs`.
const RUN_SCHEMATIC_JS: &str = include_str!("js/run-schematic.mjs");
/// The embedded migration driver (runs a migration collection via the same
/// `NodeWorkflow`). See `js/run-migration.mjs`.
const RUN_MIGRATION_JS: &str = include_str!("js/run-migration.mjs");

/// How a Node-bridge invocation finished.
#[derive(Debug)]
pub enum NodeRunError {
    /// No `node` executable could be located.
    NodeNotFound,
    /// The devkit packages could not be resolved from the project (or fallback)
    /// `node_modules`. Carries the resolution roots that were tried.
    DevkitNotFound { tried: Vec<PathBuf> },
    /// Spawning Node failed at the OS level.
    Spawn(std::io::Error),
    /// Node ran but exited non-zero. Carries the exit code (when available).
    NonZero(Option<i32>),
}

impl std::fmt::Display for NodeRunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NodeRunError::NodeNotFound => write!(
                f,
                "node executable not found on PATH — schematics and migrations \
                 run the real @angular-devkit, which requires Node"
            ),
            NodeRunError::DevkitNotFound { tried } => write!(
                f,
                "could not resolve @angular-devkit/schematics from any of: {}",
                tried.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
            ),
            NodeRunError::Spawn(e) => write!(f, "failed to spawn node: {e}"),
            NodeRunError::NonZero(Some(c)) => write!(f, "node exited with status {c}"),
            NodeRunError::NonZero(None) => write!(f, "node terminated by signal"),
        }
    }
}

impl std::error::Error for NodeRunError {}

/// Locate a `node` executable: honor `$TREATY_NODE`, then `$NODE`, else `node` on
/// PATH (verified by a `--version` probe so we fail early with a clean message).
pub fn find_node() -> Option<PathBuf> {
    for var in ["TREATY_NODE", "NODE"] {
        if let Ok(p) = std::env::var(var) {
            if !p.is_empty() {
                return Some(PathBuf::from(p));
            }
        }
    }
    // Probe `node --version`; if it runs, `node` is on PATH.
    let ok = Command::new("node")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    ok.then(|| PathBuf::from("node"))
}

/// Determine the resolution roots from which the devkit is resolved. The project
/// root is tried first (a normal Angular workspace has the devkit in its own
/// `node_modules`); the repo root is a fallback so a monorepo whose devkit is
/// hoisted still works. Both are returned so a precise "tried" list can be shown.
pub fn resolution_roots(project_root: &Path) -> Vec<PathBuf> {
    let mut roots = vec![project_root.to_path_buf()];
    // Walk up adding ancestor dirs that contain a node_modules/@angular-devkit.
    let mut dir = project_root.parent();
    while let Some(d) = dir {
        if d.join("node_modules/@angular-devkit/schematics").is_dir() {
            roots.push(d.to_path_buf());
        }
        dir = d.parent();
    }
    roots
}

/// The resolution root that actually contains a resolvable
/// `@angular-devkit/schematics`, if any.
fn first_resolvable_root(roots: &[PathBuf]) -> Option<PathBuf> {
    roots
        .iter()
        .find(|r| r.join("node_modules/@angular-devkit/schematics").is_dir())
        .cloned()
}

/// Write the embedded driver scripts into a per-process temp dir and return that
/// dir. Idempotent within a process run.
fn driver_dir() -> std::io::Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("treaty-node-driver-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("run-schematic.mjs"), RUN_SCHEMATIC_JS)?;
    std::fs::write(dir.join("run-migration.mjs"), RUN_MIGRATION_JS)?;
    Ok(dir)
}

/// Options for a schematic run.
#[derive(Debug, Clone)]
pub struct SchematicRun {
    /// The directory the schematic operates on (the workspace/project root).
    pub project_root: PathBuf,
    /// The collection (`@schematics/angular`, or a custom collection name/path).
    pub collection: String,
    /// The schematic name (`component`, `service`, `ng-new`, …).
    pub schematic: String,
    /// Positional `name` arg (most schematics take one). `None` for schematics
    /// like `ng-new` that read it from `--name`/extra args instead.
    pub name: Option<String>,
    /// Whether to plan only (no writes).
    pub dry_run: bool,
    /// Whether to overwrite existing files.
    pub force: bool,
    /// Extra `--key value` / `--flag` arguments passed straight through to the
    /// schematic (Treaty does not interpret them).
    pub passthrough: Vec<String>,
}

/// Run a schematic through the real `@angular-devkit` workflow.
pub fn run_schematic(run: &SchematicRun) -> Result<(), NodeRunError> {
    let node = find_node().ok_or(NodeRunError::NodeNotFound)?;
    let roots = resolution_roots(&run.project_root);
    let resolve_root = first_resolvable_root(&roots)
        .ok_or_else(|| NodeRunError::DevkitNotFound { tried: roots.clone() })?;
    let dir = driver_dir().map_err(NodeRunError::Spawn)?;
    let script = dir.join("run-schematic.mjs");

    let mut cmd = Command::new(&node);
    cmd.arg(&script)
        .arg(&run.project_root)
        .arg(&resolve_root)
        .arg(&run.collection)
        .arg(&run.schematic)
        .arg(if run.dry_run { "dry" } else { "write" })
        .arg(if run.force { "force" } else { "no-force" })
        .arg(run.name.as_deref().unwrap_or(""));
    // Everything after a `--` separator is passthrough for the schematic.
    cmd.arg("--");
    for a in &run.passthrough {
        cmd.arg(a);
    }
    run_inheriting(cmd)
}

/// Options for a migration run (`treaty update`).
#[derive(Debug, Clone)]
pub struct MigrationRun {
    /// The workspace root the migration operates on.
    pub project_root: PathBuf,
    /// An absolute path to a migration collection (`<pkg>/.../migrations.json`),
    /// OR a collection name resolvable from the project.
    pub collection: String,
    /// The migration schematic name; `None` runs the whole collection's
    /// recommended migrations is NOT supported here — a name is required (the CLI
    /// resolves "all migrations for a package" upstream).
    pub schematic: Option<String>,
    /// Plan only.
    pub dry_run: bool,
    /// Extra passthrough args.
    pub passthrough: Vec<String>,
}

/// Run a migration schematic through the real `@angular-devkit` workflow.
pub fn run_migration(run: &MigrationRun) -> Result<(), NodeRunError> {
    let node = find_node().ok_or(NodeRunError::NodeNotFound)?;
    let roots = resolution_roots(&run.project_root);
    let resolve_root = first_resolvable_root(&roots)
        .ok_or_else(|| NodeRunError::DevkitNotFound { tried: roots.clone() })?;
    let dir = driver_dir().map_err(NodeRunError::Spawn)?;
    let script = dir.join("run-migration.mjs");

    let mut cmd = Command::new(&node);
    cmd.arg(&script)
        .arg(&run.project_root)
        .arg(&resolve_root)
        .arg(&run.collection)
        .arg(run.schematic.as_deref().unwrap_or(""))
        .arg(if run.dry_run { "dry" } else { "write" });
    cmd.arg("--");
    for a in &run.passthrough {
        cmd.arg(a);
    }
    run_inheriting(cmd)
}

/// Spawn a command inheriting this process's stdio, mapping the exit status to a
/// [`NodeRunError`].
fn run_inheriting(mut cmd: Command) -> Result<(), NodeRunError> {
    cmd.stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());
    let status = cmd.status().map_err(NodeRunError::Spawn)?;
    if status.success() {
        Ok(())
    } else {
        Err(NodeRunError::NonZero(status.code()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_scripts_materialize() {
        let dir = driver_dir().unwrap();
        assert!(dir.join("run-schematic.mjs").is_file());
        assert!(dir.join("run-migration.mjs").is_file());
        // The schematic driver embeds the NodeWorkflow call.
        let js = std::fs::read_to_string(dir.join("run-schematic.mjs")).unwrap();
        assert!(js.contains("NodeWorkflow"), "driver lost its workflow call");
    }

    #[test]
    fn resolution_roots_starts_with_project_root() {
        let roots = resolution_roots(Path::new("/some/project"));
        assert_eq!(roots[0], PathBuf::from("/some/project"));
    }

    #[test]
    fn devkit_not_found_lists_tried_roots() {
        // A path that definitely has no node_modules/@angular-devkit.
        let bogus = std::env::temp_dir().join("treaty-no-devkit-here-xyz");
        let _ = std::fs::create_dir_all(&bogus);
        let run = SchematicRun {
            project_root: bogus.clone(),
            collection: "@schematics/angular".into(),
            schematic: "component".into(),
            name: Some("x".into()),
            dry_run: true,
            force: false,
            passthrough: vec![],
        };
        // Only assert the resolution-failure shape when node IS available (so the
        // NodeNotFound branch doesn't mask it). When node is absent we still get a
        // clean typed error, which is also acceptable.
        match run_schematic(&run) {
            Err(NodeRunError::DevkitNotFound { tried }) => {
                assert!(tried.iter().any(|p| p == &bogus));
            }
            Err(NodeRunError::NodeNotFound) => { /* acceptable on a Node-less box */ }
            other => panic!("expected DevkitNotFound or NodeNotFound, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&bogus);
    }
}
