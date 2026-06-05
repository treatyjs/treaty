//! `treaty` — the Treaty Rust CLI.
//!
//! The Rust-native successor to the TypeScript `@treaty/cli`. It calls the
//! committed `render3` Ivy compiler and the `rust_authoring` front-ends
//! directly (no NAPI), and orchestrates bundling through a pluggable
//! [`bundler::BundlerBackend`]. Commands are themselves [`plugin::CliPlugin`]s
//! registered in a [`plugin::PluginRegistry`], so the surface is extensible.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};

use treaty_cli::affected::{
    compute_affected_modules, ComputeAffectedOptions, ModuleDependencyGraph, ModuleNode,
    OnMissingDependency,
};
use treaty_cli::bundler::{backend_for_bundler, BundleInput};
use treaty_cli::config::{
    resolve_config, resolve_project, AngularResolvedConfig, Bundler, ConfigOverrides, ProjectConfig,
};
use treaty_cli::core::{BuildContext, Target};
use treaty_cli::deploy::{
    DeployArtifact, DeployContext, DeployModule, DeployPluginRegistry, ModuleKind, NoopDeployPlugin,
};
use treaty_cli::generate::{run_generate, GenerateKind, GenerateOptions};
use treaty_cli::plugin::build_default_registry;
use treaty_cli::compile;
use treaty_cli::native_build;
use treaty_cli::node_cmd::{
    self, find_node, resolution_roots, MigrationRun, SchematicRun,
};
use treaty_cli::serve;

#[derive(Parser)]
#[command(name = "treaty", version, about = "Treaty's Rust-native Angular compiler CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scaffold via the REAL `@angular-devkit` schematics (1:1 with `ng generate`).
    ///
    /// `treaty generate component foo` spawns Node running the genuine
    /// `@schematics/angular` collection resolved from the project's node_modules,
    /// honoring `angular.json` schematic defaults. Any extra `--opt value` flags
    /// are passed straight through to the schematic.
    #[command(alias = "g")]
    Generate {
        /// The schematic (`component`, `service`, `pipe`, `directive`, `guard`, …),
        /// optionally `collection:schematic`.
        schematic: String,
        /// The artifact name (most schematics take one).
        name: Option<String>,
        /// Collection to resolve the schematic from. Defaults to `@schematics/angular`.
        #[arg(long, default_value = "@schematics/angular")]
        collection: String,
        /// Project root (where `angular.json` lives). Defaults to cwd.
        #[arg(long)]
        root: Option<PathBuf>,
        /// Plan the files but do not write them.
        #[arg(long)]
        dry_run: bool,
        /// Overwrite existing files instead of skipping them.
        #[arg(long)]
        force: bool,
        /// Use the legacy Rust-native scaffolder instead of real schematics
        /// (only `app`/`lib`/`component`; no Node required).
        #[arg(long)]
        native: bool,
        /// Extra arguments passed 1:1 to the schematic (after `--`).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        passthrough: Vec<String>,
    },
    /// Scaffold a new workspace via the real `ng-new` schematic (1:1 with `ng new`).
    New {
        /// The new workspace/app name.
        name: String,
        /// Directory to create the workspace in. Defaults to cwd.
        #[arg(long)]
        root: Option<PathBuf>,
        /// Plan only.
        #[arg(long)]
        dry_run: bool,
        /// Extra arguments passed 1:1 to `ng-new` (e.g. `--style=scss`).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        passthrough: Vec<String>,
    },
    /// Run `@angular-devkit` update migrations (1:1 with `ng update`).
    Update {
        /// The package whose migrations to run (e.g. `@angular/core`). When
        /// omitted, `--migrations` must name a collection path directly.
        package: Option<String>,
        /// The migration name to run (from the package's `migrations.json`).
        #[arg(long)]
        migration: Option<String>,
        /// An explicit migrations-collection path (overrides package lookup).
        #[arg(long)]
        migrations: Option<String>,
        /// Project root (where `angular.json` lives). Defaults to cwd.
        #[arg(long)]
        root: Option<PathBuf>,
        /// Plan only.
        #[arg(long)]
        dry_run: bool,
        /// Extra arguments passed 1:1 to the migration.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        passthrough: Vec<String>,
    },
    /// Resolve a project's `build` architect target and build it through Treaty.
    ///
    /// With an `angular.json` present, `treaty build [project] -c <config>` resolves
    /// the project's `build` target (builder + merged options), honoring
    /// `outputPath`/`browser`/`index`/`tsConfig`/`styles`/`assets`, and runs it
    /// through the native Rust module-graph build. Falls back to
    /// `treaty.config.json`/conventions (and the `<entries>` form) when there is no
    /// `angular.json`.
    Build {
        /// Project name from `angular.json` (defaults to the sole/only project).
        project: Option<String>,
        /// The architect configuration (`production`, `development`, …).
        #[arg(short = 'c', long = "configuration")]
        configuration: Option<String>,
        /// Project root (where `angular.json`/`treaty.config.json` lives). Defaults to cwd.
        #[arg(long)]
        root: Option<PathBuf>,
        /// Output directory override.
        #[arg(long)]
        out_dir: Option<PathBuf>,
        /// Bundler override: rspack | rsbuild | vite | native.
        #[arg(long)]
        bundler: Option<String>,
        /// Compilation target.
        #[arg(long, value_enum, default_value_t = TargetArg::Browser)]
        target: TargetArg,
        /// Disable automatic Module Federation (on by default).
        #[arg(long)]
        no_federation: bool,
        /// Actually invoke the external bundler (default: emit config only).
        #[arg(long)]
        run: bool,
        /// Entry source files to compile (the no-`angular.json` form). When an
        /// `angular.json` resolves the target, these are ignored.
        entries: Vec<PathBuf>,
    },
    /// Resolve the configured bundler and start a dev session.
    Dev {
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long)]
        bundler: Option<String>,
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        no_federation: bool,
        /// Actually start the external dev server (default: emit config only).
        #[arg(long)]
        run: bool,
        #[arg(required = true)]
        entries: Vec<PathBuf>,
    },
    /// Compute the federated modules a set of changed files affects.
    Affected {
        /// Path to a JSON module-dependency graph
        /// (`{ "<moduleId>": { "files": [...], "dependsOn": [...] } }`).
        #[arg(long)]
        graph: PathBuf,
        /// Changed file paths (compared against the graph's `files`).
        #[arg(required = true)]
        changed: Vec<String>,
        /// Treat a dangling `dependsOn` edge as an error.
        #[arg(long)]
        strict: bool,
        /// Also print the deploy plan (version + url per affected module) using a
        /// registered [`deploy::DeployPlugin`]. Defaults to the `noop` dry-run plugin.
        #[arg(long)]
        deploy_plan: bool,
        /// The deploy plugin name to plan with. Defaults to `noop`.
        #[arg(long, default_value = "noop")]
        deploy_with: String,
        /// The version to stamp in the deploy plan.
        #[arg(long, default_value = "0.0.0")]
        version: String,
    },
    /// Compile a single authoring/component source file and print the result.
    Compile {
        input: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Start a FULLY NATIVE Rust dev server (tokio + axum) for an app dir.
    ///
    /// Compiles `.ts`/`.treaty`/`.tsx` to Ivy ESM in-process (oxc), links
    /// `@angular/*` partials to AOT, serves the index that boots the app, and
    /// live-reloads on file change — no Node, no external bundler, no NAPI.
    Serve {
        /// Project name from `angular.json` (defaults to the sole/only project).
        /// When an `angular.json` resolves the project, its `build`/`serve` target
        /// supplies the entry/index; otherwise `dir`/`--entry` apply.
        project: Option<String>,
        /// The architect configuration (`development`, `production`, …).
        #[arg(short = 'c', long = "configuration")]
        configuration: Option<String>,
        /// App dir (where `index.html` + `src/` live). Defaults to cwd. Ignored
        /// when `angular.json` resolves the project root.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Entry module relative to the app dir. Defaults to `src/main.ts`.
        #[arg(long)]
        entry: Option<PathBuf>,
        /// Host to bind. Defaults to config / `localhost`.
        #[arg(long)]
        host: Option<String>,
        /// Port to bind. Defaults to config / `4200`.
        #[arg(long)]
        port: Option<u16>,
        /// Disable dev source maps (no inline `sourceMappingURL`). Maps are ON by
        /// default so DevTools shows the original `.ts`/`.treaty`.
        #[arg(long = "no-source-map")]
        no_source_map: bool,
        /// Disable true module HMR; fall back to a full page reload on any change.
        /// HMR is ON by default.
        #[arg(long = "no-hmr")]
        no_hmr: bool,
    },
    /// Run a project's `test` architect target via the real builder (through Node).
    ///
    /// Treaty does not re-implement the Karma/Vitest builders; it resolves the
    /// `test` target from `angular.json` and runs the genuine builder so results
    /// are 1:1. Honest: this shells out to Node when a Node test builder is present.
    Test {
        /// Project name (defaults to the sole project).
        project: Option<String>,
        #[arg(short = 'c', long = "configuration")]
        configuration: Option<String>,
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        passthrough: Vec<String>,
    },
    /// Run a project's `lint` architect target via the real builder (through Node).
    Lint {
        /// Project name (defaults to the sole project).
        project: Option<String>,
        #[arg(short = 'c', long = "configuration")]
        configuration: Option<String>,
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        passthrough: Vec<String>,
    },
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum TargetArg {
    Browser,
    Server,
}

impl From<TargetArg> for Target {
    fn from(t: TargetArg) -> Self {
        match t {
            TargetArg::Browser => Target::Browser,
            TargetArg::Server => Target::Server,
        }
    }
}

fn main() -> ExitCode {
    // Build the plugin registry so the command surface is the registered one.
    // (Exercised in tests; here it asserts the built-in commands are unique.)
    let registry = build_default_registry();
    debug_assert!(registry.has("generate") && registry.has("affected"));
    let _ = &registry;

    let cli = Cli::parse();
    match cli.command {
        Command::Generate {
            schematic,
            name,
            collection,
            root,
            dry_run,
            force,
            native,
            passthrough,
        } => run_generate_cmd(
            schematic, name, collection, root, dry_run, force, native, passthrough,
        ),
        Command::New { name, root, dry_run, passthrough } => {
            run_new(name, root, dry_run, passthrough)
        }
        Command::Update { package, migration, migrations, root, dry_run, passthrough } => {
            run_update(package, migration, migrations, root, dry_run, passthrough)
        }
        Command::Build {
            project,
            configuration,
            root,
            out_dir,
            bundler,
            target,
            no_federation,
            run,
            entries,
        } => run_build(
            project,
            configuration,
            root,
            out_dir,
            bundler,
            target.into(),
            no_federation,
            run,
            entries,
        ),
        Command::Dev { root, bundler, host, port, no_federation, run, entries } => {
            run_dev(root, bundler, host, port, no_federation, run, entries)
        }
        Command::Affected { graph, changed, strict, deploy_plan, deploy_with, version } => {
            run_affected(&graph, &changed, strict, deploy_plan, &deploy_with, &version)
        }
        Command::Compile { input, json } => run_compile(&input, json),
        Command::Serve {
            project,
            configuration,
            dir,
            entry,
            host,
            port,
            no_source_map,
            no_hmr,
        } => run_serve(
            project,
            configuration,
            dir,
            entry,
            host,
            port,
            !no_source_map,
            !no_hmr,
        ),
        Command::Test { project, configuration, root, passthrough } => {
            run_architect_via_node("test", project, configuration, root, passthrough)
        }
        Command::Lint { project, configuration, root, passthrough } => {
            run_architect_via_node("lint", project, configuration, root, passthrough)
        }
    }
}

/// `treaty generate <schematic> [name]` — run the real `@angular-devkit`
/// schematic. `--native` falls back to the Rust-native scaffolder (app/lib/
/// component only), which needs no Node.
#[allow(clippy::too_many_arguments)]
fn run_generate_cmd(
    schematic: String,
    name: Option<String>,
    collection: String,
    root: Option<PathBuf>,
    dry_run: bool,
    force: bool,
    native: bool,
    passthrough: Vec<String>,
) -> ExitCode {
    if native {
        return run_native_generate(&schematic, name, dry_run, force);
    }

    let project_root = resolve_root(root);

    // Support the `collection:schematic` shorthand in the schematic slot.
    let (collection, schematic) = match schematic.split_once(':') {
        Some((c, s)) => (c.to_string(), s.to_string()),
        None => (collection, schematic),
    };

    // PROJECT INJECTION (1:1 with `ng generate`): most `@schematics/angular`
    // schematics (`component`, `service`, `pipe`, …) require a `project` resolved
    // from `angular.json`. The real CLI derives it from the workspace + cwd; we do
    // the same — when an angular.json is present and the user did not pass
    // `--project`, inject the workspace's default/sole project so the schematic
    // validates exactly as it does under `ng`.
    let mut passthrough = passthrough;
    if !passthrough.iter().any(|a| a == "--project" || a.starts_with("--project=")) {
        if let Some(ng_path) = treaty_cli::angular::find_angular_json(&project_root) {
            if let Ok(ws) = treaty_cli::angular::parse_workspace(&ng_path) {
                if let Ok(default_project) = ws.resolve_project_name(None) {
                    passthrough.push("--project".to_string());
                    passthrough.push(default_project);
                }
            }
        }
    }

    let run = SchematicRun {
        project_root,
        collection,
        schematic,
        name,
        dry_run,
        force,
        passthrough,
    };
    match node_cmd::run_schematic(&run) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("treaty: generate failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The legacy Rust-native scaffolder (`--native`): app/lib/component only.
fn run_native_generate(
    schematic: &str,
    name: Option<String>,
    dry_run: bool,
    force: bool,
) -> ExitCode {
    let kind = match schematic {
        "app" | "application" => GenerateKind::App,
        "lib" | "library" => GenerateKind::Lib,
        "component" => GenerateKind::Component,
        other => {
            eprintln!(
                "treaty: --native scaffolder supports app|lib|component, not {other:?} \
                 (drop --native to use the real schematics)"
            );
            return ExitCode::FAILURE;
        }
    };
    let Some(name) = name else {
        eprintln!("treaty: --native generate requires a name");
        return ExitCode::FAILURE;
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let opts = GenerateOptions { kind, name, cwd, dry_run, force };
    match run_generate(&opts) {
        Ok(result) => {
            if dry_run {
                for f in &result.files {
                    println!("would write {}", f.path.display());
                }
            } else {
                for p in &result.written {
                    println!("created {}", p.display());
                }
                for p in &result.skipped {
                    eprintln!("skipped (exists) {}", p.display());
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("treaty: generate failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `treaty new <name>` — scaffold a workspace via the real `ng-new` schematic.
fn run_new(
    name: String,
    root: Option<PathBuf>,
    dry_run: bool,
    passthrough: Vec<String>,
) -> ExitCode {
    let project_root = resolve_root(root);
    // ng-new reads the name from `--name`; we also pass it positionally for
    // schematics that accept either.
    let run = SchematicRun {
        project_root,
        collection: "@schematics/angular".to_string(),
        schematic: "ng-new".to_string(),
        name: Some(name),
        dry_run,
        force: false,
        passthrough,
    };
    match node_cmd::run_schematic(&run) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("treaty: new failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `treaty update [package] [--migration <name>]` — run a real migration.
fn run_update(
    package: Option<String>,
    migration: Option<String>,
    migrations: Option<String>,
    root: Option<PathBuf>,
    dry_run: bool,
    passthrough: Vec<String>,
) -> ExitCode {
    let project_root = resolve_root(root);

    // Resolve the migration collection path: an explicit `--migrations` wins;
    // otherwise derive it from the package's `ng-update.migrations` field.
    let collection = match migrations {
        Some(c) => c,
        None => match &package {
            Some(pkg) => match resolve_package_migrations(&project_root, pkg) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("treaty: update: {e}");
                    return ExitCode::FAILURE;
                }
            },
            None => {
                eprintln!(
                    "treaty: update needs a package (e.g. `@angular/core`) or an explicit --migrations <path>"
                );
                return ExitCode::FAILURE;
            }
        },
    };

    let run = MigrationRun {
        project_root,
        collection,
        schematic: migration,
        dry_run,
        passthrough,
    };
    match node_cmd::run_migration(&run) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("treaty: update failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Resolve a package's migration collection path from its
/// `ng-update.migrations` field, searching the project + ancestor node_modules.
fn resolve_package_migrations(project_root: &Path, package: &str) -> Result<String, String> {
    for root in resolution_roots(project_root) {
        let pkg_json = root
            .join("node_modules")
            .join(package.replace('/', std::path::MAIN_SEPARATOR_STR))
            .join("package.json");
        if let Ok(text) = std::fs::read_to_string(&pkg_json) {
            let parsed: serde_json::Value = serde_json::from_str(&text)
                .map_err(|e| format!("invalid {}: {e}", pkg_json.display()))?;
            if let Some(rel) = parsed
                .get("ng-update")
                .and_then(|u| u.get("migrations"))
                .and_then(|m| m.as_str())
            {
                let abs = pkg_json.parent().unwrap().join(rel);
                return Ok(abs.to_string_lossy().into_owned());
            }
            return Err(format!(
                "package {package:?} has no `ng-update.migrations` field (nothing to migrate)"
            ));
        }
    }
    Err(format!(
        "package {package:?} not found in node_modules (searched the project + ancestors)"
    ))
}

/// Resolve the project/workspace root from an optional `--root`, defaulting to cwd.
fn resolve_root(root: Option<PathBuf>) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match root {
        Some(r) if r.is_absolute() => r,
        Some(r) => cwd.join(r),
        None => cwd,
    }
}

/// Compile all entries; collect [`BundleInput`]s or report errors.
fn compile_entries(entries: &[PathBuf]) -> Result<Vec<BundleInput>, ()> {
    let mut inputs = Vec::new();
    let mut had_error = false;
    for entry in entries {
        let out = match compile::compile_path(entry) {
            Ok(out) => out,
            Err(e) => {
                eprintln!("treaty: cannot read {}: {e}", entry.display());
                had_error = true;
                continue;
            }
        };
        for err in &out.errors {
            eprintln!("treaty: {}: {err}", entry.display());
        }
        if !out.is_ok() {
            had_error = true;
            continue;
        }
        let name = entry
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "entry".to_string());
        inputs.push(BundleInput { name, code: out.code });
    }
    if had_error { Err(()) } else { Ok(inputs) }
}

#[allow(clippy::too_many_arguments)]
fn run_build(
    project: Option<String>,
    configuration: Option<String>,
    root: Option<PathBuf>,
    out_dir: Option<PathBuf>,
    bundler: Option<String>,
    target: Target,
    no_federation: bool,
    run: bool,
    entries: Vec<PathBuf>,
) -> ExitCode {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let resolve_at = match &root {
        Some(r) if r.is_absolute() => r.clone(),
        Some(r) => cwd.join(r),
        None => cwd.clone(),
    };

    let overrides = ConfigOverrides {
        root: root.map(|p| p.to_string_lossy().into_owned()),
        out_dir: out_dir.clone().map(|p| p.to_string_lossy().into_owned()),
        bundler: bundler.as_deref().and_then(Bundler::parse),
        disable_federation: no_federation,
        ..Default::default()
    };

    let resolved = match resolve_project(
        &resolve_at,
        project.as_deref(),
        "build",
        configuration.as_deref(),
        &overrides,
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("treaty: {e}");
            return ExitCode::FAILURE;
        }
    };

    match resolved {
        // ANGULAR.JSON PATH: resolve the project's `build` architect target and
        // run it through Treaty's native module-graph build, honoring
        // outputPath/browser/index/styles/assets.
        ProjectConfig::Angular(ang) => {
            run_angular_build(&ang, out_dir)
        }
        // CONVENTION PATH: the documented `<entries>` form over the configured
        // bundler (native module-graph build, or the external bundler config).
        ProjectConfig::Treaty(cfg) => {
            if entries.is_empty() {
                eprintln!(
                    "treaty: build needs entry source files (no angular.json found, \
                     and none given) — e.g. `treaty build src/main.ts`"
                );
                return ExitCode::FAILURE;
            }
            if cfg.bundler == Bundler::Native {
                return run_native_build(&cfg.root, &entries, &cfg.out_dir);
            }

            let inputs = match compile_entries(&entries) {
                Ok(i) => i,
                Err(()) => {
                    eprintln!("treaty: build aborted due to compile errors");
                    return ExitCode::FAILURE;
                }
            };

            let mut ctx = BuildContext::new(entries, cfg.out_dir.clone(), target);
            ctx.federation = cfg.module_federation.clone();

            let backend = backend_for_bundler(cfg.bundler, !run);
            eprintln!("treaty: building with {} ({})", backend.name(), cfg.bundler.as_str());
            match backend.bundle(&inputs, &ctx) {
                Ok(out) => {
                    for note in &out.notes {
                        eprintln!("treaty [{}]: {note}", backend.name());
                    }
                    for path in &out.written {
                        println!("{}", path.display());
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("treaty: bundle failed: {e}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

/// Build a project resolved from `angular.json` through Treaty's native
/// module-graph build (a bootable, JIT-free ESM dist). The architect target
/// supplies the entry (`browser`/`main`) and the output dir (`outputPath`); a
/// `--out-dir` override still wins.
fn run_angular_build(ang: &AngularResolvedConfig, out_dir_override: Option<PathBuf>) -> ExitCode {
    eprintln!(
        "treaty: building project {:?} target build{} via {} (native Rust module-graph)",
        ang.target.project,
        ang.target
            .configuration
            .as_deref()
            .map(|c| format!(":{c}"))
            .unwrap_or_default(),
        ang.target.builder,
    );
    if !ang.entry.exists() {
        eprintln!("treaty: resolved entry does not exist: {}", ang.entry.display());
        return ExitCode::FAILURE;
    }
    let out_dir = out_dir_override.unwrap_or_else(|| ang.out_dir.clone());
    let opts = native_build::NativeBuildOptions {
        root: ang.project_root.clone(),
        entry: ang.entry.clone(),
        out_dir,
    };
    match native_build::build(&opts) {
        Ok(out) => {
            for note in &out.notes {
                eprintln!("treaty [native]: {note}");
            }
            for path in &out.written {
                println!("{}", path.display());
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("treaty: native build failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The Rust-native production build: crawl the entry's import graph, compile +
/// link every module in-process, and emit a bootable ESM dist.
fn run_native_build(root: &Path, entries: &[PathBuf], out_dir: &Path) -> ExitCode {
    // The native build is whole-app: it crawls from a single entry. Use the first
    // entry (the conventional `src/main.ts`); additional entries are uncommon for
    // a standalone bootstrap and are ignored with a note.
    let Some(entry) = entries.first() else {
        eprintln!("treaty: native build needs an entry");
        return ExitCode::FAILURE;
    };
    if entries.len() > 1 {
        eprintln!("treaty: native build uses the first entry ({}); others ignored", entry.display());
    }
    // The entry may be given absolute, relative-to-cwd (as typed on the command
    // line), or relative-to-root. Prefer the form that exists on disk.
    let entry_abs = if entry.is_absolute() {
        entry.clone()
    } else if entry.exists() {
        // Relative to the current working directory (the literal arg).
        std::fs::canonicalize(entry).unwrap_or_else(|_| entry.clone())
    } else {
        root.join(entry)
    };
    let opts = native_build::NativeBuildOptions {
        root: root.to_path_buf(),
        entry: entry_abs,
        out_dir: out_dir.to_path_buf(),
    };
    eprintln!("treaty: building with native (Rust module-graph bundler)");
    match native_build::build(&opts) {
        Ok(out) => {
            for note in &out.notes {
                eprintln!("treaty [native]: {note}");
            }
            for path in &out.written {
                println!("{}", path.display());
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("treaty: native build failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Start the fully-native Rust dev server, resolving the project from
/// `angular.json` when present (its `serve`/`build` target supplies the entry +
/// app root) and falling back to `treaty.config.json`/conventions otherwise.
#[allow(clippy::too_many_arguments)]
fn run_serve(
    project: Option<String>,
    configuration: Option<String>,
    dir: Option<PathBuf>,
    entry: Option<PathBuf>,
    host: Option<String>,
    port: Option<u16>,
    source_maps: bool,
    hmr: bool,
) -> ExitCode {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = match &dir {
        Some(d) if d.is_absolute() => d.clone(),
        Some(d) => cwd.join(d),
        None => cwd.clone(),
    };

    // Resolve the project (angular.json-aware). The `serve` target's options are a
    // `buildTarget` reference (e.g. `app:build:development`); for the entry/root we
    // resolve the `build` target directly, applying the requested configuration.
    let resolved = match resolve_project(
        &root,
        project.as_deref(),
        "build",
        configuration.as_deref(),
        &ConfigOverrides { host: host.clone(), port, ..Default::default() },
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("treaty: {e}");
            return ExitCode::FAILURE;
        }
    };

    let (serve_root, entry_path) = match &resolved {
        ProjectConfig::Angular(ang) => {
            // The dev server roots at the app's project dir (where index.html lives,
            // via the build target's `index`). Entry from `browser`/`main`.
            let app_root = ang
                .target
                .index
                .as_ref()
                .and_then(|i| i.parent().map(Path::to_path_buf))
                .unwrap_or_else(|| ang.project_root.clone());
            let entry_path = match &entry {
                Some(e) if e.is_absolute() => e.clone(),
                Some(e) => app_root.join(e),
                None => ang.entry.clone(),
            };
            (ang.project_root.clone(), entry_path)
        }
        ProjectConfig::Treaty(cfg) => {
            let entry_path = match &entry {
                Some(e) if e.is_absolute() => e.clone(),
                Some(e) => root.join(e),
                None => cfg.entry.clone(),
            };
            (cfg.root.clone(), entry_path)
        }
    };

    if !entry_path.exists() {
        eprintln!("treaty: serve entry not found: {}", entry_path.display());
        return ExitCode::FAILURE;
    }

    let resolved_host = host.unwrap_or_else(|| match &resolved {
        ProjectConfig::Treaty(cfg) => cfg.host.clone(),
        ProjectConfig::Angular(_) => "localhost".to_string(),
    });
    let resolved_port = port.unwrap_or_else(|| match &resolved {
        ProjectConfig::Treaty(cfg) => cfg.port,
        ProjectConfig::Angular(_) => 4200,
    });

    let opts = serve::ServeOptions {
        root: serve_root,
        entry: entry_path,
        host: resolved_host,
        port: resolved_port,
        source_maps,
        hmr,
    };
    match serve::serve_blocking(opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("treaty: serve failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `treaty test` / `treaty lint` — resolve the architect target from
/// `angular.json` and run its real builder via Node (Treaty does not re-implement
/// the Karma/ESLint builders). Honest: when there is no `angular.json` or no such
/// target, the command reports that plainly rather than pretending.
fn run_architect_via_node(
    target: &str,
    project: Option<String>,
    configuration: Option<String>,
    root: Option<PathBuf>,
    passthrough: Vec<String>,
) -> ExitCode {
    let project_root = resolve_root(root);
    let resolved = match resolve_project(
        &project_root,
        project.as_deref(),
        target,
        configuration.as_deref(),
        &ConfigOverrides::default(),
    ) {
        Ok(ProjectConfig::Angular(ang)) => ang,
        Ok(ProjectConfig::Treaty(_)) => {
            eprintln!(
                "treaty: `{target}` requires an angular.json architect target \
                 (no angular.json found at {})",
                project_root.display()
            );
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("treaty: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Treaty runs the genuine builder via `ng`'s architect through Node. Surface
    // the resolved target so the user sees exactly what runs.
    eprintln!(
        "treaty: running {} target {}:{} via {} (real builder through Node)",
        resolved.target.project, resolved.target.project, target, resolved.target.builder
    );
    match run_architect(&project_root, &resolved.target.project, target, &configuration, &passthrough) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("treaty: {target} failed: {msg}");
            ExitCode::FAILURE
        }
    }
}

/// Invoke the Angular architect (`ng run <project>:<target>`) through Node, using
/// the local `@angular/cli` when resolvable. This keeps `test`/`lint` 1:1 with the
/// real builders without Treaty re-implementing them.
fn run_architect(
    project_root: &Path,
    project: &str,
    target: &str,
    configuration: &Option<String>,
    passthrough: &[String],
) -> Result<(), String> {
    let node = find_node().ok_or_else(|| {
        "node not found on PATH (test/lint run the real Angular builder via Node)".to_string()
    })?;
    // Find a node_modules with `@angular/cli` to drive architect.
    let cli_root = resolution_roots(project_root)
        .into_iter()
        .find(|r| r.join("node_modules/@angular/cli/bin/ng.js").is_file())
        .ok_or_else(|| {
            "could not find @angular/cli in node_modules (needed to run the real test/lint builder)"
                .to_string()
        })?;
    let ng_js = cli_root.join("node_modules/@angular/cli/bin/ng.js");
    let mut target_ref = format!("{project}:{target}");
    if let Some(c) = configuration {
        if !c.is_empty() {
            target_ref = format!("{target_ref}:{c}");
        }
    }
    let mut cmd = std::process::Command::new(&node);
    cmd.current_dir(project_root)
        .arg(&ng_js)
        .arg("run")
        .arg(&target_ref);
    for a in passthrough {
        cmd.arg(a);
    }
    let status = cmd.status().map_err(|e| format!("spawn ng: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("ng run {target_ref} exited with {:?}", status.code()))
    }
}

#[allow(clippy::too_many_arguments)]
fn run_dev(
    root: Option<PathBuf>,
    bundler: Option<String>,
    host: Option<String>,
    port: Option<u16>,
    no_federation: bool,
    run: bool,
    entries: Vec<PathBuf>,
) -> ExitCode {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let cfg = match resolve_config(
        &cwd,
        &ConfigOverrides {
            root: root.map(|p| p.to_string_lossy().into_owned()),
            bundler: bundler.as_deref().and_then(Bundler::parse),
            host: host.clone(),
            port,
            disable_federation: no_federation,
            ..Default::default()
        },
    ) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("treaty: {e}");
            return ExitCode::FAILURE;
        }
    };

    let inputs = match compile_entries(&entries) {
        Ok(i) => i,
        Err(()) => {
            eprintln!("treaty: dev aborted due to compile errors");
            return ExitCode::FAILURE;
        }
    };

    let mut ctx = BuildContext::new(entries, cfg.out_dir.clone(), Target::Browser);
    ctx.federation = cfg.module_federation.clone();
    ctx.host = Some(cfg.host.clone());
    ctx.port = Some(cfg.port);

    let backend = backend_for_bundler(cfg.bundler, !run);
    match backend.dev(&inputs, &ctx) {
        Ok(out) => {
            for note in &out.notes {
                eprintln!("treaty [{}]: {note}", backend.name());
            }
            println!("{}", out.url);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("treaty: dev failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_affected(
    graph_path: &PathBuf,
    changed: &[String],
    strict: bool,
    deploy_plan: bool,
    deploy_with: &str,
    version: &str,
) -> ExitCode {
    let graph = match load_graph(graph_path) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("treaty: cannot load graph {}: {e}", graph_path.display());
            return ExitCode::FAILURE;
        }
    };
    let opts = ComputeAffectedOptions {
        on_missing_dependency: if strict {
            OnMissingDependency::Error
        } else {
            OnMissingDependency::Ignore
        },
        ..Default::default()
    };
    let changed_refs: Vec<&str> = changed.iter().map(String::as_str).collect();
    let modules = match compute_affected_modules(changed_refs, &graph, &opts) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("treaty: affected: {e}");
            return ExitCode::FAILURE;
        }
    };

    if !deploy_plan {
        for m in &modules {
            println!("{m}");
        }
        return ExitCode::SUCCESS;
    }

    // CI deploy-only-changed: plan a deploy for each affected module through the
    // registered deploy plugin (default: the noop dry-run plugin).
    let mut registry = DeployPluginRegistry::new();
    registry
        .register(Box::new(NoopDeployPlugin::new()), false)
        .expect("seed noop deploy plugin");
    let plugin = match registry.get(deploy_with) {
        Some(p) => p,
        None => {
            eprintln!(
                "treaty: unknown deploy plugin {deploy_with:?} (registered: {})",
                registry.list().join(", ")
            );
            return ExitCode::FAILURE;
        }
    };
    let ctx = DeployContext::default();
    for module_id in &modules {
        let module = DeployModule {
            module_id: module_id.clone(),
            version: version.to_string(),
            kind: ModuleKind::Route,
        };
        match plugin.deploy(&module, &DeployArtifact::default(), &ctx) {
            Ok(d) => println!("{module_id} -> {} ({})", d.version, d.url),
            Err(e) => {
                eprintln!("treaty: deploy plan failed for {module_id}: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}

/// The on-disk JSON shape of an affected graph node.
#[derive(serde::Deserialize)]
struct GraphNodeJson {
    #[serde(default)]
    files: Vec<String>,
    #[serde(default, rename = "dependsOn")]
    depends_on: Vec<String>,
}

fn load_graph(path: &PathBuf) -> std::io::Result<ModuleDependencyGraph> {
    let text = std::fs::read_to_string(path)?;
    let raw: std::collections::BTreeMap<String, GraphNodeJson> = serde_json::from_str(&text)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(raw
        .into_iter()
        .map(|(id, n)| (id, ModuleNode { files: n.files, depends_on: n.depends_on }))
        .collect())
}

fn run_compile(input: &PathBuf, json: bool) -> ExitCode {
    let out = match compile::compile_path(input) {
        Ok(out) => out,
        Err(e) => {
            eprintln!("treaty: cannot read {}: {e}", input.display());
            return ExitCode::FAILURE;
        }
    };

    if json {
        match serde_json::to_string_pretty(&out) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("treaty: failed to serialize output: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        for err in &out.errors {
            eprintln!("treaty: {err}");
        }
        if out.is_ok() {
            print!("{}", out.code);
        }
    }

    if out.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_registry_owns_the_builtin_commands() {
        let reg = build_default_registry();
        for cmd in ["generate", "build", "dev", "serve", "affected", "compile"] {
            assert!(reg.has(cmd), "registry missing {cmd}");
        }
    }
}
