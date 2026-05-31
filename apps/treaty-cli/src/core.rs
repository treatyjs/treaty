//! Shared core types for the Treaty CLI.
//!
//! These are the small, serializable value types that flow between the CLI's
//! subcommands and the compiler. They are intentionally free of `clap` and of
//! the heavyweight compiler structs so that other crates (e.g. `treaty_packagr`)
//! could reuse the same vocabulary if desired.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::Federation;

/// The result of compiling a single authoring source file.
///
/// This is the CLI-facing projection of the compiler's richer return types
/// ([`rust_authoring::CompiledAuthoring`] / `render3`'s `CompiledComponent`):
/// it carries only what a build pipeline or a JSON report consumer needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompileOutput {
    /// The source file that was compiled (as passed on the command line).
    pub input: PathBuf,
    /// The emitted client module source. Empty when `errors` is non-empty.
    pub code: String,
    /// Generated backend/server module source, when the source declared a
    /// `server { … }` block. `None` for the common client-only case.
    pub server_module: Option<String>,
    /// Diagnostics produced while compiling. A non-empty list means the
    /// compile did not produce usable `code`.
    pub errors: Vec<String>,
}

impl CompileOutput {
    /// Whether the compile succeeded (produced code and no diagnostics).
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Which authoring front-end produced a [`CompileOutput`].
///
/// The CLI picks the front-end from the input file's extension; this enum
/// records that decision so reports and logs are unambiguous.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Frontend {
    /// `.treaty` / `.tsx` / `.ts` driven through `rust_authoring::compile_file`.
    Authoring,
    /// A bare `@Component` TypeScript class driven through
    /// `render3::source_compile::compile_component_source`.
    Component,
}

/// The compilation target environment, mirroring Angular's notion of a
/// browser vs. server (SSR) build. Carried into the bundler config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Target {
    Browser,
    Server,
}

impl Default for Target {
    fn default() -> Self {
        Target::Browser
    }
}

/// A resolved build context: the inputs, the output directory, and the target.
///
/// This is the single value threaded through `treaty build` and handed to a
/// [`crate::bundler::BundlerBackend`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildContext {
    /// Entry source files to compile and bundle.
    pub entries: Vec<PathBuf>,
    /// Directory the bundler should write artifacts into.
    pub out_dir: PathBuf,
    /// Target environment.
    pub target: Target,
    /// Module Federation configuration. Enabled by default; the external
    /// bundler config injects the auto-federation wiring when enabled. Defaults
    /// to the zero-config host toggle.
    #[serde(default)]
    pub federation: Federation,
    /// Dev-server host (used by [`crate::bundler::BundlerBackend::dev`]).
    #[serde(default)]
    pub host: Option<String>,
    /// Dev-server port.
    #[serde(default)]
    pub port: Option<u16>,
}

impl BuildContext {
    /// A minimal build context for `target`/`out_dir` with default federation.
    pub fn new(entries: Vec<PathBuf>, out_dir: PathBuf, target: Target) -> Self {
        BuildContext {
            entries,
            out_dir,
            target,
            federation: Federation::default(),
            host: None,
            port: None,
        }
    }
}
