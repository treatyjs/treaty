//! Pluggable bundler backends.
//!
//! A full Rust-native bundler is a large future effort. Today the CLI ships:
//!
//!   * [`ExternalBundler`] — the production path. It emits a config for an
//!     existing JS bundler (rspack / rsbuild / vite) and invokes it, after the
//!     Treaty front-ends have already lowered each entry to Ivy output.
//!   * [`NativeBundler`] — a documented Rust-native fallback that links the
//!     compiled modules into a single emit. It is deliberately minimal (no tree
//!     shaking, no code splitting) and exists so the trait has an in-process
//!     implementation; a real Rust-native graph bundler is tracked as future
//!     work (see migration/ROADMAP-PHASE2).
//!
//! Both implement [`BundlerBackend`] so callers are agnostic to the choice.

use std::path::PathBuf;

use crate::config::{Bundler, Federation};
use crate::core::BuildContext;

/// A single compiled entry handed to a bundler: the lowered module source and
/// the logical name it should be emitted under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleInput {
    /// Logical module name (typically the entry's file stem).
    pub name: String,
    /// The compiled (Ivy-lowered) module source.
    pub code: String,
}

/// The artifacts a bundler produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleOutput {
    /// Files written to disk, as absolute or out-dir-relative paths.
    pub written: Vec<PathBuf>,
    /// Human-readable notes (e.g. the external tool that was invoked).
    pub notes: Vec<String>,
}

/// Errors a bundler backend can surface.
#[derive(Debug)]
pub enum BundleError {
    /// An underlying I/O failure.
    Io(std::io::Error),
    /// The selected external tool was not found / failed.
    Tool(String),
}

impl std::fmt::Display for BundleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BundleError::Io(e) => write!(f, "io error: {e}"),
            BundleError::Tool(m) => write!(f, "bundler tool error: {m}"),
        }
    }
}

impl std::error::Error for BundleError {}

impl From<std::io::Error> for BundleError {
    fn from(e: std::io::Error) -> Self {
        BundleError::Io(e)
    }
}

/// The result of preparing a dev session: the dev server invocation that was
/// (or would be) started, plus the files written to drive it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevOutput {
    /// The url the dev server is served from (informational).
    pub url: String,
    /// Files written to prepare the session (config, entry stubs).
    pub written: Vec<PathBuf>,
    /// Human-readable notes (e.g. the tool that was launched).
    pub notes: Vec<String>,
}

/// The backend contract: take already-compiled entries plus a build context and
/// produce bundled artifacts in `ctx.out_dir`, or start a dev session.
pub trait BundlerBackend {
    /// A stable identifier for logging / `--bundler` selection.
    fn name(&self) -> &str;

    /// Bundle the given compiled inputs into a production build.
    fn bundle(
        &self,
        inputs: &[BundleInput],
        ctx: &BuildContext,
    ) -> Result<BundleOutput, BundleError>;

    /// Start (or, in dry-run, prepare) a development session over `inputs`.
    ///
    /// The default implementation prepares the same artifacts as [`Self::bundle`]
    /// and reports a dev url derived from `ctx.host`/`ctx.port`; tool-backed
    /// backends override this to spawn a watching dev server.
    fn dev(&self, inputs: &[BundleInput], ctx: &BuildContext) -> Result<DevOutput, BundleError> {
        let out = self.bundle(inputs, ctx)?;
        let host = ctx.host.as_deref().unwrap_or("localhost");
        let port = ctx.port.unwrap_or(4200);
        let mut notes = out.notes;
        notes.push(format!("dev session prepared for {}", self.name()));
        Ok(DevOutput {
            url: format!("http://{host}:{port}/"),
            written: out.written,
            notes,
        })
    }
}

/// Render the automatic Module Federation block injected into an external
/// bundler config. Every Treaty app is a federation host by default; this emits
/// the host plugin shape the `@treaty/{rspack,vite}` wrappers expect (name,
/// remotes, exposes, shared). Returns an empty string when federation is off.
fn render_federation(federation: &Federation) -> String {
    let Some(mf) = federation.options() else {
        return "  // module federation: disabled\n".to_string();
    };
    let name = mf.name.clone().unwrap_or_else(|| "host".to_string());
    let remotes = mf
        .remotes
        .iter()
        .map(|r| format!("{r:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    let exposes = mf
        .exposes
        .iter()
        .map(|e| format!("{e:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    let shared = {
        let mut s = vec!["\"@angular/core\"".to_string(), "\"@angular/common\"".to_string()];
        s.extend(mf.shared.iter().map(|d| format!("{d:?}")));
        s.join(", ")
    };
    format!(
        "  // Auto Module Federation (on by default; toggle in treaty.config.json).\n  \
         moduleFederation: {{\n    \
           name: {name:?},\n    \
           remotes: [{remotes}],\n    \
           exposes: [{exposes}],\n    \
           shared: [{shared}],\n  \
         }},\n"
    )
}

/// Which external JS bundler the [`ExternalBundler`] should delegate to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalTool {
    Rspack,
    Rsbuild,
    Vite,
}

impl ExternalTool {
    /// The config file name this tool reads.
    fn config_file(self) -> &'static str {
        match self {
            ExternalTool::Rspack => "rspack.config.mjs",
            ExternalTool::Rsbuild => "rsbuild.config.mjs",
            ExternalTool::Vite => "vite.config.mjs",
        }
    }

    /// The package-runner subcommand used to invoke a production build.
    fn runner_args(self) -> &'static [&'static str] {
        match self {
            ExternalTool::Rspack => &["rspack", "build"],
            ExternalTool::Rsbuild => &["rsbuild", "build"],
            ExternalTool::Vite => &["vite", "build"],
        }
    }

    /// The package-runner subcommand used to start a dev server.
    fn dev_args(self) -> &'static [&'static str] {
        match self {
            ExternalTool::Rspack => &["rspack", "serve"],
            ExternalTool::Rsbuild => &["rsbuild", "dev"],
            ExternalTool::Vite => &[],
        }
    }

    /// Map a resolved [`Bundler`] to the external tool, if it is an external one.
    pub fn from_bundler(b: Bundler) -> Option<Self> {
        match b {
            Bundler::Rspack => Some(ExternalTool::Rspack),
            Bundler::Rsbuild => Some(ExternalTool::Rsbuild),
            Bundler::Vite => Some(ExternalTool::Vite),
            Bundler::Native => None,
        }
    }
}

/// The production bundler backend: write each compiled entry to `out_dir`,
/// emit a config for the chosen external tool, then invoke it.
pub struct ExternalBundler {
    pub tool: ExternalTool,
    /// The package runner used to launch the tool (e.g. `npx`, `pnpm dlx`).
    /// Split into program + leading args so Windows `npx.cmd` works too.
    pub runner: Vec<String>,
    /// When true, write inputs + config but do not actually spawn the tool.
    /// Used by tests and `--dry-run`.
    pub dry_run: bool,
}

impl Default for ExternalBundler {
    fn default() -> Self {
        ExternalBundler {
            tool: ExternalTool::Rspack,
            runner: vec!["npx".to_string()],
            dry_run: true,
        }
    }
}

impl ExternalBundler {
    /// Render a minimal config for `self.tool` over the given entry names.
    fn render_config(&self, inputs: &[BundleInput], ctx: &BuildContext) -> String {
        let entries: Vec<String> = inputs
            .iter()
            .map(|i| format!("    {:?}: \"./{}.js\"", i.name, i.name))
            .collect();
        let target = match ctx.target {
            crate::core::Target::Browser => "web",
            crate::core::Target::Server => "node",
        };
        let federation = render_federation(&ctx.federation);
        format!(
            "// Generated by the Treaty CLI ({tool}). Do not edit by hand.\n\
             export default {{\n  \
             target: {target:?},\n  \
             output: {{ path: {out:?} }},\n  \
             entry: {{\n{entries}\n  }},\n\
             {federation}}};\n",
            tool = self.tool.config_file(),
            target = target,
            out = ctx.out_dir.to_string_lossy(),
            entries = entries.join(",\n"),
            federation = federation,
        )
    }

    fn write_inputs(
        &self,
        inputs: &[BundleInput],
        ctx: &BuildContext,
    ) -> Result<Vec<PathBuf>, BundleError> {
        std::fs::create_dir_all(&ctx.out_dir)?;
        let mut written = Vec::new();
        for input in inputs {
            let path = ctx.out_dir.join(format!("{}.js", input.name));
            std::fs::write(&path, &input.code)?;
            written.push(path);
        }
        let config_path = ctx.out_dir.join(self.tool.config_file());
        std::fs::write(&config_path, self.render_config(inputs, ctx))?;
        written.push(config_path);
        Ok(written)
    }
}

impl BundlerBackend for ExternalBundler {
    fn name(&self) -> &str {
        "external"
    }

    fn bundle(
        &self,
        inputs: &[BundleInput],
        ctx: &BuildContext,
    ) -> Result<BundleOutput, BundleError> {
        let written = self.write_inputs(inputs, ctx)?;
        let mut notes = vec![format!(
            "emitted {} module(s) + {} for {:?}",
            inputs.len(),
            self.tool.config_file(),
            self.tool
        )];

        if self.dry_run {
            notes.push("dry-run: external tool not invoked".to_string());
            return Ok(BundleOutput { written, notes });
        }

        let (program, lead) = self
            .runner
            .split_first()
            .ok_or_else(|| BundleError::Tool("empty runner command".into()))?;
        let mut cmd = std::process::Command::new(program);
        cmd.args(lead);
        cmd.args(self.tool.runner_args());
        cmd.arg("--config").arg(self.tool.config_file());
        cmd.current_dir(&ctx.out_dir);

        let status = cmd
            .status()
            .map_err(|e| BundleError::Tool(format!("failed to spawn {program}: {e}")))?;
        if !status.success() {
            return Err(BundleError::Tool(format!(
                "{program} exited with {status}"
            )));
        }
        notes.push(format!("invoked {program} ({:?})", self.tool));
        Ok(BundleOutput { written, notes })
    }

    fn dev(&self, inputs: &[BundleInput], ctx: &BuildContext) -> Result<DevOutput, BundleError> {
        let written = self.write_inputs(inputs, ctx)?;
        let host = ctx.host.as_deref().unwrap_or("localhost");
        let port = ctx.port.unwrap_or(4200);
        let url = format!("http://{host}:{port}/");
        let mut notes = vec![format!(
            "emitted {} module(s) + {} for {:?} dev",
            inputs.len(),
            self.tool.config_file(),
            self.tool
        )];

        if self.dry_run {
            notes.push("dry-run: dev server not started".to_string());
            return Ok(DevOutput { url, written, notes });
        }

        let (program, lead) = self
            .runner
            .split_first()
            .ok_or_else(|| BundleError::Tool("empty runner command".into()))?;
        let mut cmd = std::process::Command::new(program);
        cmd.args(lead);
        let dev_args = self.tool.dev_args();
        if dev_args.is_empty() {
            // Vite's dev server is the bare `vite` invocation.
            cmd.arg("vite");
        } else {
            cmd.args(dev_args);
        }
        cmd.arg("--config").arg(self.tool.config_file());
        cmd.current_dir(&ctx.out_dir);
        let status = cmd
            .status()
            .map_err(|e| BundleError::Tool(format!("failed to spawn {program}: {e}")))?;
        if !status.success() {
            return Err(BundleError::Tool(format!("{program} dev exited with {status}")));
        }
        notes.push(format!("started {program} dev server ({:?})", self.tool));
        Ok(DevOutput { url, written, notes })
    }
}

/// The Rust-native fallback backend.
///
/// This is a documented stub: it concatenates the compiled entries into a
/// single `bundle.js` with module banners. It performs NO dependency-graph
/// resolution, tree shaking, or code splitting — a real Rust-native bundler
/// (likely built on `oxc_resolver` + a linker) is future work. It exists so the
/// [`BundlerBackend`] trait has an in-process implementation usable without any
/// external toolchain.
pub struct NativeBundler;

impl BundlerBackend for NativeBundler {
    fn name(&self) -> &str {
        "native"
    }

    fn bundle(
        &self,
        inputs: &[BundleInput],
        ctx: &BuildContext,
    ) -> Result<BundleOutput, BundleError> {
        std::fs::create_dir_all(&ctx.out_dir)?;
        let mut bundle = String::new();
        bundle.push_str("// Treaty native bundler (fallback). Naive concatenation.\n");
        for input in inputs {
            bundle.push_str(&format!("\n// --- module: {} ---\n", input.name));
            bundle.push_str(&input.code);
            bundle.push('\n');
        }
        let path = ctx.out_dir.join("bundle.js");
        std::fs::write(&path, bundle)?;
        Ok(BundleOutput {
            written: vec![path],
            notes: vec!["native fallback: naive concatenation, no tree-shaking".to_string()],
        })
    }
}

/// Resolve a backend for a configured [`Bundler`].
///
/// `dry_run` controls whether the external tool is actually spawned (false in
/// real builds, true in tests / `--dry-run`). The Rust-native fallback ignores
/// it (it always runs in-process).
pub fn backend_for_bundler(bundler: Bundler, dry_run: bool) -> Box<dyn BundlerBackend> {
    match ExternalTool::from_bundler(bundler) {
        Some(tool) => Box::new(ExternalBundler {
            tool,
            runner: vec!["npx".to_string()],
            dry_run,
        }),
        None => Box::new(NativeBundler),
    }
}

/// Resolve a backend by name (the value of `--bundler`). Accepts the external
/// tool names, the alias `"external"` (-> rspack), and `"native"`. Always
/// dry-run; `main` uses [`backend_for_bundler`] directly to control spawning.
pub fn backend_for(name: &str) -> Option<Box<dyn BundlerBackend>> {
    match name {
        "external" => Some(backend_for_bundler(Bundler::Rspack, true)),
        "native" => Some(Box::new(NativeBundler)),
        other => Bundler::parse(other).map(|b| backend_for_bundler(b, true)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Target;

    fn ctx(out: &str) -> BuildContext {
        BuildContext::new(vec![], PathBuf::from(out), Target::Browser)
    }

    #[test]
    fn backend_lookup() {
        assert_eq!(backend_for("native").unwrap().name(), "native");
        assert_eq!(backend_for("rspack").unwrap().name(), "external");
        assert_eq!(backend_for("vite").unwrap().name(), "external");
        assert!(backend_for("nope").is_none());
        assert_eq!(
            backend_for_bundler(Bundler::Native, true).name(),
            "native"
        );
    }

    #[test]
    fn external_config_injects_federation_by_default() {
        let b = ExternalBundler::default();
        let inputs = vec![BundleInput { name: "main".into(), code: "1;".into() }];
        let mut c = ctx("ignored");
        c.federation = Federation::Options(crate::config::MfOptions {
            name: Some("shop".into()),
            remotes: vec!["checkout@http://x/remoteEntry.js".into()],
            ..Default::default()
        });
        let cfg = b.render_config(&inputs, &c);
        assert!(cfg.contains("moduleFederation"));
        assert!(cfg.contains("\"shop\""));
        assert!(cfg.contains("@angular/core"));
    }

    #[test]
    fn external_config_marks_federation_disabled_when_off() {
        let b = ExternalBundler::default();
        let inputs = vec![BundleInput { name: "main".into(), code: "1;".into() }];
        let mut c = ctx("ignored");
        c.federation = Federation::Toggle(false);
        let cfg = b.render_config(&inputs, &c);
        assert!(cfg.contains("module federation: disabled"));
    }

    #[test]
    fn dev_dry_run_prepares_without_spawning() {
        let dir = std::env::temp_dir().join(format!("treaty-cli-dev-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let b = ExternalBundler::default();
        let inputs = vec![BundleInput { name: "main".into(), code: "1;".into() }];
        let mut c = ctx(dir.to_str().unwrap());
        c.port = Some(4321);
        let out = b.dev(&inputs, &c).unwrap();
        assert_eq!(out.url, "http://localhost:4321/");
        assert!(out.notes.iter().any(|n| n.contains("dev server not started")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn external_dry_run_writes_config_and_modules() {
        let dir = std::env::temp_dir().join(format!("treaty-cli-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let b = ExternalBundler::default();
        let inputs = vec![BundleInput {
            name: "main".into(),
            code: "export const x = 1;".into(),
        }];
        let out = b.bundle(&inputs, &ctx(dir.to_str().unwrap())).unwrap();
        assert!(out.written.iter().any(|p| p.ends_with("main.js")));
        assert!(out.written.iter().any(|p| p.ends_with("rspack.config.mjs")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn native_concatenates() {
        let dir = std::env::temp_dir().join(format!("treaty-cli-native-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let inputs = vec![BundleInput {
            name: "a".into(),
            code: "1;".into(),
        }];
        let out = NativeBundler.bundle(&inputs, &ctx(dir.to_str().unwrap())).unwrap();
        assert_eq!(out.written.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

}
