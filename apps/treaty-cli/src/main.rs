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
use treaty_cli::config::{resolve_config, Bundler, ConfigOverrides};
use treaty_cli::core::{BuildContext, Target};
use treaty_cli::deploy::{
    DeployArtifact, DeployContext, DeployModule, DeployPluginRegistry, ModuleKind, NoopDeployPlugin,
};
use treaty_cli::generate::{run_generate, GenerateKind, GenerateOptions};
use treaty_cli::plugin::build_default_registry;
use treaty_cli::compile;
use treaty_cli::native_build;
use treaty_cli::serve;

#[derive(Parser)]
#[command(name = "treaty", version, about = "Treaty's Rust-native Angular compiler CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scaffold a selectorless, signal, standalone source (app|lib|component).
    Generate {
        /// What to scaffold.
        #[arg(value_enum)]
        kind: GenerateKindArg,
        /// The artifact name.
        name: String,
        /// Plan the files but do not write them.
        #[arg(long)]
        dry_run: bool,
        /// Overwrite existing files instead of skipping them.
        #[arg(long)]
        force: bool,
    },
    /// Resolve the configured bundler and produce a federation-ready build.
    Build {
        /// Project root (where `treaty.config.json` lives). Defaults to cwd.
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
        /// Entry source files to compile.
        #[arg(required = true)]
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
        /// App dir (where `index.html` + `src/` live). Defaults to cwd.
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
    },
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum GenerateKindArg {
    App,
    Lib,
    Component,
}

impl From<GenerateKindArg> for GenerateKind {
    fn from(k: GenerateKindArg) -> Self {
        match k {
            GenerateKindArg::App => GenerateKind::App,
            GenerateKindArg::Lib => GenerateKind::Lib,
            GenerateKindArg::Component => GenerateKind::Component,
        }
    }
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
        Command::Generate { kind, name, dry_run, force } => {
            run_generate_cmd(kind.into(), name, dry_run, force)
        }
        Command::Build { root, out_dir, bundler, target, no_federation, run, entries } => {
            run_build(root, out_dir, bundler, target.into(), no_federation, run, entries)
        }
        Command::Dev { root, bundler, host, port, no_federation, run, entries } => {
            run_dev(root, bundler, host, port, no_federation, run, entries)
        }
        Command::Affected { graph, changed, strict, deploy_plan, deploy_with, version } => {
            run_affected(&graph, &changed, strict, deploy_plan, &deploy_with, &version)
        }
        Command::Compile { input, json } => run_compile(&input, json),
        Command::Serve { dir, entry, host, port } => run_serve(dir, entry, host, port),
    }
}

fn run_generate_cmd(kind: GenerateKind, name: String, dry_run: bool, force: bool) -> ExitCode {
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
    root: Option<PathBuf>,
    out_dir: Option<PathBuf>,
    bundler: Option<String>,
    target: Target,
    no_federation: bool,
    run: bool,
    entries: Vec<PathBuf>,
) -> ExitCode {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let cfg = match resolve_config(
        &cwd,
        &ConfigOverrides {
            root: root.map(|p| p.to_string_lossy().into_owned()),
            out_dir: out_dir.map(|p| p.to_string_lossy().into_owned()),
            bundler: bundler.as_deref().and_then(Bundler::parse),
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

    // The Rust-native bundler does a full module-graph crawl + per-module Ivy
    // compile + `@angular/*` partial linking + ESM emit, producing a BOOTABLE
    // dist with no JIT. (The external rspack/rsbuild/vite path below stays the
    // single-entry "compile then hand to the JS bundler" flow.)
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

/// Start the fully-native Rust dev server for an app dir.
fn run_serve(
    dir: Option<PathBuf>,
    entry: Option<PathBuf>,
    host: Option<String>,
    port: Option<u16>,
) -> ExitCode {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = match &dir {
        Some(d) if d.is_absolute() => d.clone(),
        Some(d) => cwd.join(d),
        None => cwd.clone(),
    };
    // Resolve host/port/entry through the config resolver (so a treaty.config.json
    // in the app dir is honored), with CLI flags taking precedence.
    let cfg = match resolve_config(
        &root,
        &ConfigOverrides {
            host: host.clone(),
            port,
            ..Default::default()
        },
    ) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("treaty: {e}");
            return ExitCode::FAILURE;
        }
    };
    let entry_path = match entry {
        Some(e) if e.is_absolute() => e,
        Some(e) => root.join(e),
        None => cfg.entry.clone(),
    };
    if !entry_path.exists() {
        eprintln!("treaty: serve entry not found: {}", entry_path.display());
        return ExitCode::FAILURE;
    }

    let opts = serve::ServeOptions {
        root: cfg.root.clone(),
        entry: entry_path,
        host: cfg.host.clone(),
        port: cfg.port,
    };
    match serve::serve_blocking(opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("treaty: serve failed: {e}");
            ExitCode::FAILURE
        }
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
