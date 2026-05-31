//! The bump -> verify -> codemod -> PR loop.
//!
//! This module scripts what happens to **one** [`UpdatePlan`]: bump the
//! manifest to `latest`, run the repo's own gates (cargo `--workspace` build +
//! test, the oracle/compliance harness, and `tsgo` + `oxlint` for TS), and — on
//! breakage — run the [`CodemodRule`]s whose `dep`/range cover the bump, then
//! re-verify. A green result yields a PR description (changelog + the codemods
//! that ran); a still-red result yields a precise, human-facing failing report
//! (never an LLM).
//!
//! Everything that touches the world (editing manifests, applying codemods,
//! spawning processes) goes through the [`Repo`] trait, which composes a
//! [`CommandRunner`] for the verify gates with manifest/codemod side effects.
//! The loop logic itself is a pure function of the plan, the rule set, and what
//! the `Repo` reports — so it is unit-testable with a mock `Repo` that never
//! mutates the real tree, and a **dry-run** mode plans the actions without
//! executing any of them.

use crate::codemod::{apply_rewrite, matcher_hits};
use crate::model::{CodemodRule, UpdatePlan, VerifyResult, VerifyStep};
use crate::process::{CommandOutcome, CommandRunner};
use crate::RuleSet;

/// How many trailing characters of a failing gate's stderr to keep in reports.
///
/// Enough to surface the first compiler/test error without dumping the whole
/// build log into a PR body or report.
const STDERR_EXCERPT_CHARS: usize = 2_000;

/// One verification gate: a label plus the program/args used to run it.
///
/// The set of gates is fixed and ordered (cheapest/most-fundamental first) so
/// the loop fails fast and reports the *first* thing that broke. All gates run
/// in the repo root via the [`Repo`]'s [`CommandRunner`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gate {
    /// Short label recorded in [`VerifyStep::name`] (e.g. `cargo build`).
    pub name: String,
    /// Executable to invoke (e.g. `cargo`, `bun`, `npx`).
    pub program: String,
    /// Arguments passed to `program`.
    pub args: Vec<String>,
}

impl Gate {
    fn new(name: &str, program: &str, args: &[&str]) -> Self {
        Gate {
            name: name.to_string(),
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// `args` as `&str` slices, for handing to [`CommandRunner::run`].
    fn arg_refs(&self) -> Vec<&str> {
        self.args.iter().map(String::as_str).collect()
    }

    /// The repo's default verification gates, in fail-fast order.
    ///
    /// 1. `cargo build --workspace` — compile the whole Rust workspace.
    /// 2. `cargo test --workspace`  — the unit/integration suite.
    /// 3. `oracle`                  — the differential oracle harness.
    /// 4. `compliance`              — the Angular compliance suite.
    /// 5. `tsgo`                    — TypeScript type-check for the JS packages.
    /// 6. `oxlint`                  — lint the JS packages.
    ///
    /// These mirror the gates named in `migration/DEP-UPDATER-PLAN.md`. The
    /// loop treats them opaquely (it only cares about exit codes), so the exact
    /// command for the oracle/compliance/ts steps can be tuned without touching
    /// the loop.
    pub fn default_gates() -> Vec<Gate> {
        vec![
            Gate::new("cargo build", "cargo", &["build", "--workspace"]),
            Gate::new("cargo test", "cargo", &["test", "--workspace"]),
            Gate::new("oracle", "cargo", &["run", "-p", "oracle", "--quiet"]),
            Gate::new("compliance", "cargo", &["test", "-p", "compliance", "--quiet"]),
            Gate::new("tsgo", "bun", &["run", "tsgo"]),
            Gate::new("oxlint", "bun", &["run", "oxlint"]),
        ]
    }
}

/// Side effects the orchestration needs, behind one trait so the loop is
/// testable without mutating the repo or spawning processes.
///
/// Production wires this to [`FsRepo`] (real manifest edits + real
/// [`RealCommandRunner`](crate::process::RealCommandRunner) gates). Tests inject
/// a mock that records calls and returns canned outcomes.
pub trait Repo {
    /// Edit the manifest to pin `plan.name` at `plan.latest`.
    ///
    /// Idempotent: applying the same bump twice leaves the manifest pinned at
    /// `latest` either way. Returns the human-readable change made (for the
    /// PR/report), or an error if the manifest could not be updated.
    fn apply_bump(&mut self, plan: &UpdatePlan) -> Result<String, String>;

    /// Apply one codemod rule to the working tree.
    ///
    /// Returns `Ok(true)` if the rule changed anything, `Ok(false)` if it was a
    /// no-op (already migrated — codemods are idempotent), or `Err` if the
    /// rewrite itself failed. The loop records every rule it *ran*; whether a
    /// rule made an edit is informational for the report.
    fn apply_codemod(&mut self, rule: &CodemodRule) -> Result<bool, String>;

    /// The runner used to execute the verification gates.
    fn runner(&self) -> &dyn CommandRunner;

    /// Absolute repo root the gates run in.
    fn root(&self) -> &str;
}

// ---------------------------------------------------------------------------
// Production Repo: real manifest edits + on-disk codemods + real gates.
// ---------------------------------------------------------------------------

/// The filesystem operations [`FsRepo`] needs, behind a trait so the production
/// repo is unit-testable with an in-memory fake (no real I/O).
///
/// Deliberately tiny: read a file, write a file, and enumerate the `*.rs`
/// sources a codemod should scan. Keeping fs access behind this seam — exactly
/// as the registry sits behind [`crate::detect::Fetcher`] — is what lets the
/// whole apply/codemod path run in a test against an in-memory fake instead of
/// [`RealFileSystem`].
pub trait FileSystem {
    /// Read a UTF-8 file relative to the repo root. `Err` on missing/unreadable.
    fn read(&self, rel_path: &str) -> Result<String, String>;

    /// Write `contents` to a file relative to the repo root, creating it if
    /// needed. `Err` if the write fails.
    fn write(&self, rel_path: &str, contents: &str) -> Result<(), String>;

    /// Every Rust source file (repo-relative path) a codemod should consider.
    ///
    /// The orchestration applies each matching codemod to every file this
    /// returns; `Err` only on an enumeration failure.
    fn rust_sources(&self) -> Result<Vec<String>, String>;
}

/// Pin a dependency to `new_version` in a Cargo manifest's source, preserving
/// the original requirement's operator prefix (`^`, `~`, `=`, none).
///
/// Pure: takes the manifest text and returns the edited text plus a summary,
/// or `Err` if the dependency line could not be located. Handles both the
/// `name = "req"` and `name = { version = "req", .. }` shapes. Idempotent —
/// re-pinning to a version already present is a no-op that still reports success.
pub fn pin_cargo_dependency(
    manifest_src: &str,
    dep: &str,
    new_version: &str,
) -> Result<(String, String), String> {
    // Match `dep = "x"` or `dep = { ... version = "x" ... }`. We rewrite only
    // the version string, keeping any leading operator the author used so the
    // bump respects their pinning style.
    let escaped = regex::escape(dep);
    // First try the inline-table `version = "..."` belonging to this dep.
    let table_re = regex::Regex::new(&format!(
        r#"(?m)^(\s*{escaped}\s*=\s*\{{[^}}]*?version\s*=\s*")([\^~=<> ]*)([^"]+)(")"#
    ))
    .map_err(|e| e.to_string())?;
    if let Some(caps) = table_re.captures(manifest_src) {
        let op = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        let old = caps.get(3).map(|m| m.as_str()).unwrap_or("");
        let replaced = table_re.replace(manifest_src, |c: &regex::Captures| {
            format!("{}{}{}{}", &c[1], op, new_version, &c[4])
        });
        let summary = format!("{dep}: \"{op}{old}\" -> \"{op}{new_version}\" (Cargo.toml table)");
        return Ok((replaced.into_owned(), summary));
    }

    // Then the bare string form `dep = "..."`.
    let str_re = regex::Regex::new(&format!(
        r#"(?m)^(\s*{escaped}\s*=\s*")([\^~=<> ]*)([^"]+)(")"#
    ))
    .map_err(|e| e.to_string())?;
    if let Some(caps) = str_re.captures(manifest_src) {
        let op = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        let old = caps.get(3).map(|m| m.as_str()).unwrap_or("");
        let replaced = str_re.replace(manifest_src, |c: &regex::Captures| {
            format!("{}{}{}{}", &c[1], op, new_version, &c[4])
        });
        let summary = format!("{dep}: \"{op}{old}\" -> \"{op}{new_version}\" (Cargo.toml)");
        return Ok((replaced.into_owned(), summary));
    }

    Err(format!("dependency `{dep}` not found in manifest"))
}

/// Pin a dependency to `new_version` in a `package.json` source, preserving the
/// original range operator (`^`, `~`, none).
///
/// Pure: returns the edited JSON text plus a summary, or `Err` if the
/// dependency key is absent. Edits the value string in place (rather than
/// re-serializing) so unrelated formatting in the file is left untouched.
pub fn pin_npm_dependency(
    manifest_src: &str,
    dep: &str,
    new_version: &str,
) -> Result<(String, String), String> {
    let escaped = regex::escape(dep);
    // `"name": "^1.2.3"` — capture the operator so we keep the author's range style.
    let re = regex::Regex::new(&format!(
        r#"("{escaped}"\s*:\s*")([\^~]?)([^"]+)(")"#
    ))
    .map_err(|e| e.to_string())?;
    if let Some(caps) = re.captures(manifest_src) {
        let op = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        let old = caps.get(3).map(|m| m.as_str()).unwrap_or("");
        let replaced = re.replace(manifest_src, |c: &regex::Captures| {
            format!("{}{}{}{}", &c[1], op, new_version, &c[4])
        });
        let summary = format!("{dep}: \"{op}{old}\" -> \"{op}{new_version}\" (package.json)");
        return Ok((replaced.into_owned(), summary));
    }
    Err(format!("dependency `{dep}` not found in package.json"))
}

/// The production [`Repo`]: edits real manifests, applies codemods to real
/// source files, and runs the gates as real OS processes.
///
/// Filesystem access goes through an injected [`FileSystem`] and command
/// execution through an injected [`CommandRunner`], so the production wiring is
/// itself testable against fakes. The default constructor
/// ([`FsRepo::at`]) wires the real implementations.
pub struct FsRepo<C: CommandRunner, F: FileSystem> {
    root: String,
    runner: C,
    fs: F,
}

impl<C: CommandRunner, F: FileSystem> FsRepo<C, F> {
    /// Build a repo over `root` with explicit runner and filesystem.
    pub fn new(root: impl Into<String>, runner: C, fs: F) -> Self {
        FsRepo {
            root: root.into(),
            runner,
            fs,
        }
    }
}

impl<C: CommandRunner, F: FileSystem> Repo for FsRepo<C, F> {
    fn apply_bump(&mut self, plan: &UpdatePlan) -> Result<String, String> {
        let src = self.fs.read(&plan.manifest)?;
        let new_version = plan.latest.to_string();
        let (edited, summary) = match plan.kind {
            crate::model::DepKind::Crate => {
                pin_cargo_dependency(&src, &plan.name, &new_version)?
            }
            crate::model::DepKind::Npm => pin_npm_dependency(&src, &plan.name, &new_version)?,
        };
        // Skip the write when nothing changed so a re-run stays a true no-op.
        if edited != src {
            self.fs.write(&plan.manifest, &edited)?;
        }
        Ok(summary)
    }

    fn apply_codemod(&mut self, rule: &CodemodRule) -> Result<bool, String> {
        let mut changed_any = false;
        for path in self.fs.rust_sources()? {
            let src = self.fs.read(&path)?;
            if !matcher_hits(&rule.matcher, &src).map_err(|e| e.to_string())? {
                continue;
            }
            let next = apply_rewrite(&rule.rewrite, &src).map_err(|e| e.to_string())?;
            if next != src {
                self.fs.write(&path, &next)?;
                changed_any = true;
            }
        }
        Ok(changed_any)
    }

    fn runner(&self) -> &dyn CommandRunner {
        &self.runner
    }

    fn root(&self) -> &str {
        &self.root
    }
}

/// A real, recursive-walk [`FileSystem`] rooted at a repo directory.
///
/// Reads/writes resolve relative to `root`; [`rust_sources`](FileSystem::rust_sources)
/// walks the tree for `*.rs` files, skipping the usual non-source directories
/// (`target`, `.git`, `node_modules`) so codemods never touch build artifacts.
pub struct RealFileSystem {
    root: std::path::PathBuf,
}

impl RealFileSystem {
    /// Root the filesystem at `root`.
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        RealFileSystem { root: root.into() }
    }

    fn walk(dir: &std::path::Path, root: &std::path::Path, out: &mut Vec<String>) -> Result<(), String> {
        let entries = std::fs::read_dir(dir).map_err(|e| format!("read_dir {dir:?}: {e}"))?;
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|e| e.to_string())?;
            if file_type.is_dir() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if matches!(name.as_ref(), "target" | ".git" | "node_modules") {
                    continue;
                }
                Self::walk(&path, root, out)?;
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                if let Ok(rel) = path.strip_prefix(root) {
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
        Ok(())
    }
}

impl FileSystem for RealFileSystem {
    fn read(&self, rel_path: &str) -> Result<String, String> {
        std::fs::read_to_string(self.root.join(rel_path))
            .map_err(|e| format!("read {rel_path}: {e}"))
    }

    fn write(&self, rel_path: &str, contents: &str) -> Result<(), String> {
        let full = self.root.join(rel_path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {parent:?}: {e}"))?;
        }
        std::fs::write(&full, contents).map_err(|e| format!("write {rel_path}: {e}"))
    }

    fn rust_sources(&self) -> Result<Vec<String>, String> {
        let mut out = Vec::new();
        if self.root.is_dir() {
            Self::walk(&self.root, &self.root, &mut out)?;
        }
        out.sort();
        Ok(out)
    }
}

impl<C: CommandRunner> FsRepo<C, RealFileSystem> {
    /// Wire a production repo at `root` with the given runner and a real,
    /// tree-walking filesystem.
    pub fn at(root: impl Into<String>, runner: C) -> Self {
        let root = root.into();
        let fs = RealFileSystem::new(std::path::PathBuf::from(&root));
        FsRepo {
            root,
            runner,
            fs,
        }
    }
}

/// Mode the loop runs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Actually bump, verify, codemod, and produce a real PR/report.
    Execute,
    /// Plan the actions only — no manifest edits, no processes, no codemods.
    DryRun,
}

/// Configuration for one orchestration run.
#[derive(Debug, Clone)]
pub struct OrchestrationConfig {
    /// Execute vs dry-run.
    pub mode: Mode,
    /// The verification gates to run, in order. Defaults to [`Gate::default_gates`].
    pub gates: Vec<Gate>,
}

impl Default for OrchestrationConfig {
    fn default() -> Self {
        OrchestrationConfig {
            mode: Mode::Execute,
            gates: Gate::default_gates(),
        }
    }
}

impl OrchestrationConfig {
    /// An execute-mode config with the default gates.
    pub fn execute() -> Self {
        OrchestrationConfig::default()
    }

    /// A dry-run config with the default gates.
    pub fn dry_run() -> Self {
        OrchestrationConfig {
            mode: Mode::DryRun,
            ..OrchestrationConfig::default()
        }
    }
}

/// A single action the loop intends to (or did) take.
///
/// In dry-run mode the loop returns the full ordered plan of these without
/// executing anything; in execute mode the same sequence is what actually ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Pin the dependency at its latest version in the named manifest.
    Bump {
        /// Dependency name.
        dep: String,
        /// Version being pinned.
        to: String,
        /// Manifest edited.
        manifest: String,
    },
    /// Run the verification gates (build/test/oracle/...).
    Verify {
        /// Whether this verify pass is before or after codemods.
        phase: VerifyPhase,
    },
    /// Apply a codemod rule.
    Codemod {
        /// Rule id applied.
        rule_id: String,
    },
}

/// Which verification pass an [`Action::Verify`] / [`VerifyResult`] belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyPhase {
    /// Right after the bump, before any codemods.
    PostBump,
    /// After codemods were applied.
    PostCodemod,
}

/// The terminal outcome of orchestrating one plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The loop would run these actions; nothing was executed (dry-run mode).
    Planned {
        /// The plan that would be processed.
        plan: UpdatePlan,
        /// Ordered actions the execute path would take.
        actions: Vec<Action>,
    },
    /// The bump verified green (immediately, or after codemods) — ready for a PR.
    PrReady(PrDescription),
    /// Still red after the matching codemods ran — needs a human.
    Failed(FailingReport),
}

impl Outcome {
    /// True for a green/PR-ready outcome.
    pub fn is_pr_ready(&self) -> bool {
        matches!(self, Outcome::PrReady(_))
    }

    /// True for a failing outcome.
    pub fn is_failed(&self) -> bool {
        matches!(self, Outcome::Failed(_))
    }

    /// The PR description, if this outcome is PR-ready.
    pub fn pr(&self) -> Option<&PrDescription> {
        match self {
            Outcome::PrReady(pr) => Some(pr),
            _ => None,
        }
    }

    /// The failing report, if this outcome failed.
    pub fn report(&self) -> Option<&FailingReport> {
        match self {
            Outcome::Failed(r) => Some(r),
            _ => None,
        }
    }
}

/// A PR description for a successfully-applied bump.
///
/// Carries everything a reviewer (or a PR-opening step) needs: which dependency
/// moved, the codemods that were required to keep it green, and the verify
/// record that proves it. `body()` renders the human-facing Markdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrDescription {
    /// The plan this PR realizes.
    pub plan: UpdatePlan,
    /// Human-readable summary of the manifest change.
    pub bump_summary: String,
    /// Ids of codemods that were applied to fix breakage (empty if it built clean).
    pub codemods_applied: Vec<String>,
    /// The final, passing verify result.
    pub verify: VerifyResult,
}

impl PrDescription {
    /// A one-line PR title, e.g. `chore(deps): bump oxc_ast 0.29.0 -> 0.133.0 (minor)`.
    pub fn title(&self) -> String {
        format!(
            "chore(deps): bump {} {} -> {} ({})",
            self.plan.name, self.plan.current, self.plan.latest, self.plan.class
        )
    }

    /// The full PR body in Markdown: changelog + codemods that ran + verify gates.
    pub fn body(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("# {}\n\n", self.title()));
        out.push_str("## Changelog\n");
        out.push_str(&format!(
            "- **{}** ({}): `{}` -> `{}` [{}]\n",
            self.plan.name,
            self.plan.kind,
            self.plan.current,
            self.plan.latest,
            self.plan.class,
        ));
        out.push_str(&format!("- manifest: `{}`\n", self.plan.manifest));
        out.push_str(&format!("- {}\n\n", self.bump_summary));

        out.push_str("## Codemods applied\n");
        if self.codemods_applied.is_empty() {
            out.push_str("- none — bump built clean\n\n");
        } else {
            for id in &self.codemods_applied {
                out.push_str(&format!("- `{}`\n", id));
            }
            out.push('\n');
        }

        out.push_str("## Verification\n");
        for step in &self.verify.steps {
            let mark = if step.success { "PASS" } else { "FAIL" };
            out.push_str(&format!("- [{}] {}\n", mark, step.name));
        }
        out.push_str("\nAll gates green. Generated deterministically (no AI).\n");
        out
    }
}

/// A precise, human-facing report for a bump that stayed red after codemods.
///
/// Records what was attempted (the bump, the codemods that ran) and the exact
/// gate that failed plus its stderr tail, so a human can act without rerunning
/// anything. The loop never escalates to an LLM — this report is the handoff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailingReport {
    /// The plan that failed.
    pub plan: UpdatePlan,
    /// Summary of the manifest change that was applied.
    pub bump_summary: String,
    /// Codemods that were run in the attempt to fix breakage.
    pub codemods_applied: Vec<String>,
    /// The verify result whose failure we are reporting (the final pass).
    pub verify: VerifyResult,
}

impl FailingReport {
    /// The name of the gate that broke, if recorded.
    pub fn failing_gate(&self) -> Option<&str> {
        self.verify.first_failure().map(|s| s.name.as_str())
    }

    /// The full report in Markdown.
    pub fn body(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "# FAILED: bump {} {} -> {}\n\n",
            self.plan.name, self.plan.current, self.plan.latest
        ));
        out.push_str(&format!("- manifest: `{}`\n", self.plan.manifest));
        out.push_str(&format!("- {}\n\n", self.bump_summary));

        out.push_str("## Codemods attempted\n");
        if self.codemods_applied.is_empty() {
            out.push_str("- none matched this bump\n\n");
        } else {
            for id in &self.codemods_applied {
                out.push_str(&format!("- `{}`\n", id));
            }
            out.push('\n');
        }

        out.push_str("## Failure\n");
        match self.verify.first_failure() {
            Some(step) => {
                out.push_str(&format!(
                    "Gate `{}` failed (exit {}).\n\n",
                    step.name, step.exit_code
                ));
                if !step.stderr_excerpt.is_empty() {
                    out.push_str("```\n");
                    out.push_str(&step.stderr_excerpt);
                    out.push_str("\n```\n");
                }
            }
            None => out.push_str("No failing gate recorded (unexpected).\n"),
        }
        out.push_str("\nNeeds a human: no codemod resolves this bump. (No AI involved.)\n");
        out
    }
}

/// Drives one [`UpdatePlan`] through the bump -> verify -> codemod -> PR loop.
pub struct Orchestrator<'a> {
    config: OrchestrationConfig,
    rules: &'a RuleSet,
}

impl<'a> Orchestrator<'a> {
    /// Build an orchestrator with the given config and codemod rule set.
    pub fn new(config: OrchestrationConfig, rules: &'a RuleSet) -> Self {
        Orchestrator { config, rules }
    }

    /// The codemod rules that match this plan's bump, in deterministic order.
    ///
    /// The returned references borrow the rule set (`'a`), but only for as long
    /// as the call's `plan` borrow lives — so callers get the rules without the
    /// plan having to outlive the orchestrator.
    fn matching_rules<'p>(&self, plan: &'p UpdatePlan) -> Vec<&'a CodemodRule>
    where
        'a: 'p,
    {
        self.rules
            .rules
            .iter()
            .filter(|r| r.applies_to(&plan.name, &plan.latest))
            .collect()
    }

    /// Plan the actions the execute path would take, without running anything.
    ///
    /// Deterministic: bump, post-bump verify, then (optimistically) every
    /// matching codemod plus a post-codemod verify. The real run short-circuits
    /// if the post-bump verify is already green, but the *plan* lists the full
    /// remediation path so a dry-run reviewer sees the worst case.
    pub fn plan_actions(&self, plan: &UpdatePlan) -> Vec<Action> {
        let mut actions = vec![
            Action::Bump {
                dep: plan.name.clone(),
                to: plan.latest.to_string(),
                manifest: plan.manifest.clone(),
            },
            Action::Verify {
                phase: VerifyPhase::PostBump,
            },
        ];
        let matching = self.matching_rules(plan);
        if !matching.is_empty() {
            for rule in matching {
                actions.push(Action::Codemod {
                    rule_id: rule.id.clone(),
                });
            }
            actions.push(Action::Verify {
                phase: VerifyPhase::PostCodemod,
            });
        }
        actions
    }

    /// Plan the loop for one plan without a [`Repo`] — a convenience for the
    /// CLI's `dry-run`, which never touches the working tree.
    ///
    /// Always returns [`Outcome::Planned`] (the dry-run outcome), regardless of
    /// the configured [`Mode`]; the action list is [`plan_actions`].
    ///
    /// [`plan_actions`]: Self::plan_actions
    pub fn run_plan_dry(&self, plan: &UpdatePlan) -> Outcome {
        Outcome::Planned {
            plan: plan.clone(),
            actions: self.plan_actions(plan),
        }
    }

    /// Run (or, in dry-run mode, plan) the loop for one plan against `repo`.
    ///
    /// Execute mode:
    /// 1. apply the bump;
    /// 2. verify — green ⇒ [`Outcome::PrReady`] with no codemods;
    /// 3. on red, apply every matching codemod and re-verify — green ⇒
    ///    PR-ready recording the codemods, still red ⇒ [`Outcome::Failed`].
    ///
    /// Dry-run mode returns [`Outcome::Planned`] with [`plan_actions`] and does
    /// not touch `repo`.
    ///
    /// [`plan_actions`]: Self::plan_actions
    pub fn run_plan<R: Repo>(&self, plan: &UpdatePlan, repo: &mut R) -> Outcome {
        if self.config.mode == Mode::DryRun {
            return Outcome::Planned {
                plan: plan.clone(),
                actions: self.plan_actions(plan),
            };
        }

        // 1. Bump.
        let bump_summary = match repo.apply_bump(plan) {
            Ok(s) => s,
            Err(e) => {
                // A failed bump is reported the same way as a failed gate so
                // the caller has one failure surface to handle.
                let mut verify = VerifyResult::new(plan.id());
                verify.record(VerifyStep::from_exit("apply bump", 1, e));
                return Outcome::Failed(FailingReport {
                    plan: plan.clone(),
                    bump_summary: format!("failed to bump {} to {}", plan.name, plan.latest),
                    codemods_applied: Vec::new(),
                    verify,
                });
            }
        };

        // 2. Verify post-bump.
        let post_bump = self.verify(plan, repo, &[]);
        if post_bump.passed {
            return Outcome::PrReady(PrDescription {
                plan: plan.clone(),
                bump_summary,
                codemods_applied: Vec::new(),
                verify: post_bump,
            });
        }

        // 3. Red: run the matching codemods, then re-verify.
        let matching = self.matching_rules(plan);
        let mut applied: Vec<String> = Vec::new();
        for rule in &matching {
            // We record every codemod we *ran*. A rewrite error is itself a
            // failure surface, reported like a gate failure.
            match repo.apply_codemod(rule) {
                Ok(_changed) => applied.push(rule.id.clone()),
                Err(e) => {
                    let mut verify = VerifyResult::new(plan.id());
                    verify.codemods_applied = applied.clone();
                    verify.record(VerifyStep::from_exit(
                        format!("codemod {}", rule.id),
                        1,
                        e,
                    ));
                    return Outcome::Failed(FailingReport {
                        plan: plan.clone(),
                        bump_summary,
                        codemods_applied: applied,
                        verify,
                    });
                }
            }
        }

        // If no codemod matched, there is nothing to retry — fail with the
        // original red verify so the report points at the real gate failure.
        if applied.is_empty() {
            return Outcome::Failed(FailingReport {
                plan: plan.clone(),
                bump_summary,
                codemods_applied: applied,
                verify: post_bump,
            });
        }

        let post_codemod = self.verify(plan, repo, &applied);
        if post_codemod.passed {
            Outcome::PrReady(PrDescription {
                plan: plan.clone(),
                bump_summary,
                codemods_applied: applied,
                verify: post_codemod,
            })
        } else {
            Outcome::Failed(FailingReport {
                plan: plan.clone(),
                bump_summary,
                codemods_applied: applied,
                verify: post_codemod,
            })
        }
    }

    /// Run the gates in order, stopping at the first failure (fail-fast).
    ///
    /// `codemods_applied` is threaded onto the [`VerifyResult`] so a PR/report
    /// built from it knows which codemods preceded this verification.
    fn verify<R: Repo>(
        &self,
        plan: &UpdatePlan,
        repo: &R,
        codemods_applied: &[String],
    ) -> VerifyResult {
        let mut result = VerifyResult::new(plan.id());
        result.codemods_applied = codemods_applied.to_vec();
        let runner = repo.runner();
        let root = repo.root();
        for gate in &self.config.gates {
            let outcome = match runner.run(&gate.program, &gate.arg_refs(), root) {
                Ok(o) => o,
                Err(e) => CommandOutcome {
                    exit_code: 127,
                    stdout: String::new(),
                    stderr: format!("failed to spawn `{}`: {}", gate.program, e),
                },
            };
            let excerpt = outcome.stderr_tail(STDERR_EXCERPT_CHARS);
            result.record(VerifyStep::from_exit(&gate.name, outcome.exit_code, excerpt));
            // Fail-fast: the first broken gate is the precise thing to report;
            // running later gates would only add noise.
            if !outcome.success() {
                break;
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DepKind, Matcher, Rewrite};
    use crate::process::CommandOutcome;
    use semver::Version;
    use std::cell::RefCell;
    use std::collections::HashMap;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    fn oxc_plan() -> UpdatePlan {
        UpdatePlan::new(
            "oxc_ast",
            DepKind::Crate,
            v("0.29.0"),
            v("0.133.0"),
            "Cargo.toml",
        )
    }

    fn visitmut_rule() -> CodemodRule {
        CodemodRule {
            id: "oxc-visitmut-import-move".into(),
            dep: "oxc_ast".into(),
            from: v("0.30.0"),
            to: None,
            matcher: Matcher::Literal {
                contains: "use oxc_ast::VisitMut".into(),
            },
            rewrite: Rewrite::Replace {
                find: "use oxc_ast::VisitMut".into(),
                replace: "use oxc_ast_visit::VisitMut".into(),
            },
            description: "VisitMut moved to oxc_ast_visit".into(),
        }
    }

    /// A scripted runner: returns the queued outcome per gate name, recording
    /// every command it was asked to run. The same gate may be queried twice
    /// (post-bump and post-codemod), so outcomes are keyed by gate name with a
    /// FIFO queue per name.
    struct ScriptedRunner {
        // gate name -> queued outcomes (consumed front to back).
        scripts: RefCell<HashMap<String, Vec<CommandOutcome>>>,
        default: CommandOutcome,
        calls: RefCell<Vec<String>>,
    }

    impl ScriptedRunner {
        fn always_pass() -> Self {
            ScriptedRunner {
                scripts: RefCell::new(HashMap::new()),
                default: CommandOutcome {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                },
                calls: RefCell::new(Vec::new()),
            }
        }

        fn queue(&self, gate: &str, outcome: CommandOutcome) {
            self.scripts
                .borrow_mut()
                .entry(gate.to_string())
                .or_default()
                .push(outcome);
        }
    }

    impl CommandRunner for ScriptedRunner {
        fn run(&self, program: &str, args: &[&str], cwd: &str) -> std::io::Result<CommandOutcome> {
            self.calls
                .borrow_mut()
                .push(format!("{program} {} @ {cwd}", args.join(" ")));
            // Map the command back to a gate name by matching default_gates.
            let joined = format!("{program} {}", args.join(" "));
            let gate_name = Gate::default_gates()
                .into_iter()
                .find(|g| format!("{} {}", g.program, g.args.join(" ")) == joined)
                .map(|g| g.name)
                .unwrap_or_else(|| joined.clone());
            let mut scripts = self.scripts.borrow_mut();
            if let Some(queue) = scripts.get_mut(&gate_name) {
                if !queue.is_empty() {
                    return Ok(queue.remove(0));
                }
            }
            Ok(self.default.clone())
        }
    }

    /// A mock repo recording bumps and codemods, verifying via a scripted runner.
    struct MockRepo {
        runner: ScriptedRunner,
        root: String,
        bumps: RefCell<Vec<String>>,
        codemods: RefCell<Vec<String>>,
        bump_should_fail: bool,
        codemod_errors: RefCell<HashMap<String, String>>,
    }

    impl MockRepo {
        fn new(runner: ScriptedRunner) -> Self {
            MockRepo {
                runner,
                root: "/repo".into(),
                bumps: RefCell::new(Vec::new()),
                codemods: RefCell::new(Vec::new()),
                bump_should_fail: false,
                codemod_errors: RefCell::new(HashMap::new()),
            }
        }
    }

    impl Repo for MockRepo {
        fn apply_bump(&mut self, plan: &UpdatePlan) -> Result<String, String> {
            if self.bump_should_fail {
                return Err(format!("could not edit {}", plan.manifest));
            }
            self.bumps.borrow_mut().push(plan.id());
            Ok(format!("pinned {} = {}", plan.name, plan.latest))
        }

        fn apply_codemod(&mut self, rule: &CodemodRule) -> Result<bool, String> {
            if let Some(err) = self.codemod_errors.borrow().get(&rule.id) {
                return Err(err.clone());
            }
            self.codemods.borrow_mut().push(rule.id.clone());
            Ok(true)
        }

        fn runner(&self) -> &dyn CommandRunner {
            &self.runner
        }

        fn root(&self) -> &str {
            &self.root
        }
    }

    // ---- green path: keeps the bump, no codemods ----

    #[test]
    fn green_path_is_pr_ready_with_no_codemods() {
        let rules = RuleSet {
            rules: vec![visitmut_rule()],
        };
        let orch = Orchestrator::new(OrchestrationConfig::execute(), &rules);
        let mut repo = MockRepo::new(ScriptedRunner::always_pass());

        let outcome = orch.run_plan(&oxc_plan(), &mut repo);

        assert!(outcome.is_pr_ready(), "expected PR-ready, got {outcome:?}");
        let pr = outcome.pr().unwrap();
        assert!(pr.codemods_applied.is_empty());
        assert!(pr.verify.passed);
        // Bump applied; no codemods run because verify was green.
        assert_eq!(repo.bumps.borrow().len(), 1);
        assert!(repo.codemods.borrow().is_empty());
        // PR body mentions the bump and that no codemods were needed.
        let body = pr.body();
        assert!(body.contains("0.29.0"));
        assert!(body.contains("0.133.0"));
        assert!(body.contains("none — bump built clean"));
    }

    // ---- red-then-codemod-green: records the codemod ----

    #[test]
    fn red_then_codemod_green_records_the_codemod() {
        let rules = RuleSet {
            rules: vec![visitmut_rule()],
        };
        let orch = Orchestrator::new(OrchestrationConfig::execute(), &rules);

        let runner = ScriptedRunner::always_pass();
        // First build fails (post-bump); after the codemod, build passes again.
        runner.queue(
            "cargo build",
            CommandOutcome {
                exit_code: 1,
                stdout: String::new(),
                stderr: "error[E0432]: unresolved import `oxc_ast::VisitMut`".into(),
            },
        );
        runner.queue(
            "cargo build",
            CommandOutcome {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            },
        );
        let mut repo = MockRepo::new(runner);

        let outcome = orch.run_plan(&oxc_plan(), &mut repo);

        assert!(outcome.is_pr_ready(), "expected PR-ready, got {outcome:?}");
        let pr = outcome.pr().unwrap();
        assert_eq!(pr.codemods_applied, vec!["oxc-visitmut-import-move"]);
        assert!(pr.verify.passed);
        // The codemod actually ran on the repo.
        assert_eq!(
            repo.codemods.borrow().as_slice(),
            &["oxc-visitmut-import-move".to_string()]
        );
        // PR body lists the codemod that fixed it.
        assert!(pr.body().contains("oxc-visitmut-import-move"));
    }

    // ---- red-after-codemods: failing report ----

    #[test]
    fn red_after_codemods_is_a_failing_report() {
        let rules = RuleSet {
            rules: vec![visitmut_rule()],
        };
        let orch = Orchestrator::new(OrchestrationConfig::execute(), &rules);

        let runner = ScriptedRunner::always_pass();
        // Build fails both before and after the codemod.
        runner.queue(
            "cargo build",
            CommandOutcome {
                exit_code: 1,
                stdout: String::new(),
                stderr: "error: still broken before codemod".into(),
            },
        );
        runner.queue(
            "cargo build",
            CommandOutcome {
                exit_code: 1,
                stdout: String::new(),
                stderr: "error: still broken after codemod".into(),
            },
        );
        let mut repo = MockRepo::new(runner);

        let outcome = orch.run_plan(&oxc_plan(), &mut repo);

        assert!(outcome.is_failed(), "expected failure, got {outcome:?}");
        let report = outcome.report().unwrap();
        // The codemod was attempted and recorded.
        assert_eq!(report.codemods_applied, vec!["oxc-visitmut-import-move"]);
        assert_eq!(report.failing_gate(), Some("cargo build"));
        // The report surfaces the *final* (post-codemod) failure.
        let body = report.body();
        assert!(body.contains("still broken after codemod"));
        assert!(body.contains("oxc-visitmut-import-move"));
        assert!(body.contains("Needs a human"));
    }

    // ---- red with no matching codemod: failing report, no retry ----

    #[test]
    fn red_with_no_matching_codemod_fails_without_retry() {
        // Rule set is empty, so nothing matches the bump.
        let rules = RuleSet::new();
        let orch = Orchestrator::new(OrchestrationConfig::execute(), &rules);

        let runner = ScriptedRunner::always_pass();
        runner.queue(
            "cargo test",
            CommandOutcome {
                exit_code: 101,
                stdout: String::new(),
                stderr: "test failure".into(),
            },
        );
        let mut repo = MockRepo::new(runner);

        let outcome = orch.run_plan(&oxc_plan(), &mut repo);

        assert!(outcome.is_failed());
        let report = outcome.report().unwrap();
        assert!(report.codemods_applied.is_empty());
        assert_eq!(report.failing_gate(), Some("cargo test"));
        // No codemod ran (none matched).
        assert!(repo.codemods.borrow().is_empty());
    }

    // ---- bump failure ----

    #[test]
    fn bump_failure_is_a_failing_report() {
        let rules = RuleSet::new();
        let orch = Orchestrator::new(OrchestrationConfig::execute(), &rules);
        let mut repo = MockRepo::new(ScriptedRunner::always_pass());
        repo.bump_should_fail = true;

        let outcome = orch.run_plan(&oxc_plan(), &mut repo);

        assert!(outcome.is_failed());
        let report = outcome.report().unwrap();
        assert_eq!(report.failing_gate(), Some("apply bump"));
        // No gates ran because the bump never landed.
        assert!(repo.runner.calls.borrow().is_empty());
    }

    // ---- codemod application error ----

    #[test]
    fn codemod_apply_error_is_a_failing_report() {
        let rules = RuleSet {
            rules: vec![visitmut_rule()],
        };
        let orch = Orchestrator::new(OrchestrationConfig::execute(), &rules);

        let runner = ScriptedRunner::always_pass();
        runner.queue(
            "cargo build",
            CommandOutcome {
                exit_code: 1,
                stdout: String::new(),
                stderr: "broken".into(),
            },
        );
        let mut repo = MockRepo::new(runner);
        repo.codemod_errors
            .borrow_mut()
            .insert("oxc-visitmut-import-move".into(), "rewrite blew up".into());

        let outcome = orch.run_plan(&oxc_plan(), &mut repo);

        assert!(outcome.is_failed());
        let report = outcome.report().unwrap();
        assert_eq!(
            report.failing_gate(),
            Some("codemod oxc-visitmut-import-move")
        );
        assert!(report.body().contains("rewrite blew up"));
    }

    // ---- dry-run: plans without executing ----

    #[test]
    fn dry_run_plans_actions_without_touching_repo() {
        let rules = RuleSet {
            rules: vec![visitmut_rule()],
        };
        let orch = Orchestrator::new(OrchestrationConfig::dry_run(), &rules);
        let mut repo = MockRepo::new(ScriptedRunner::always_pass());

        let outcome = orch.run_plan(&oxc_plan(), &mut repo);

        match outcome {
            Outcome::Planned { plan, actions } => {
                assert_eq!(plan, oxc_plan());
                // bump, post-bump verify, codemod, post-codemod verify.
                assert_eq!(
                    actions,
                    vec![
                        Action::Bump {
                            dep: "oxc_ast".into(),
                            to: "0.133.0".into(),
                            manifest: "Cargo.toml".into(),
                        },
                        Action::Verify {
                            phase: VerifyPhase::PostBump
                        },
                        Action::Codemod {
                            rule_id: "oxc-visitmut-import-move".into(),
                        },
                        Action::Verify {
                            phase: VerifyPhase::PostCodemod
                        },
                    ]
                );
            }
            other => panic!("expected Planned, got {other:?}"),
        }
        // Nothing executed: no bumps, no codemods, no commands.
        assert!(repo.bumps.borrow().is_empty());
        assert!(repo.codemods.borrow().is_empty());
        assert!(repo.runner.calls.borrow().is_empty());
    }

    #[test]
    fn dry_run_without_matching_rules_plans_only_bump_and_verify() {
        let rules = RuleSet::new();
        let orch = Orchestrator::new(OrchestrationConfig::dry_run(), &rules);
        let mut repo = MockRepo::new(ScriptedRunner::always_pass());

        let outcome = orch.run_plan(&oxc_plan(), &mut repo);

        match outcome {
            Outcome::Planned { actions, .. } => {
                assert_eq!(
                    actions,
                    vec![
                        Action::Bump {
                            dep: "oxc_ast".into(),
                            to: "0.133.0".into(),
                            manifest: "Cargo.toml".into(),
                        },
                        Action::Verify {
                            phase: VerifyPhase::PostBump
                        },
                    ]
                );
            }
            other => panic!("expected Planned, got {other:?}"),
        }
    }

    // ---- fail-fast: later gates do not run after the first failure ----

    #[test]
    fn verify_is_fail_fast() {
        let rules = RuleSet::new();
        let orch = Orchestrator::new(OrchestrationConfig::execute(), &rules);

        let runner = ScriptedRunner::always_pass();
        // cargo build fails -> cargo test and the rest must not run.
        runner.queue(
            "cargo build",
            CommandOutcome {
                exit_code: 1,
                stdout: String::new(),
                stderr: "build broke".into(),
            },
        );
        let mut repo = MockRepo::new(runner);

        let outcome = orch.run_plan(&oxc_plan(), &mut repo);

        assert!(outcome.is_failed());
        // Exactly one gate (cargo build) ran; nothing after it.
        let calls = repo.runner.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].starts_with("cargo build --workspace"));
    }

    // ---- production FsRepo over an in-memory filesystem ----

    /// An in-memory filesystem fake: a path -> contents map plus a fixed list
    /// of "rust sources". Records writes so tests can assert on disk effects
    /// without touching the real filesystem.
    struct MemFs {
        files: RefCell<HashMap<String, String>>,
        rust: Vec<String>,
    }

    impl MemFs {
        fn new(files: &[(&str, &str)], rust: &[&str]) -> Self {
            MemFs {
                files: RefCell::new(
                    files
                        .iter()
                        .map(|(p, c)| (p.to_string(), c.to_string()))
                        .collect(),
                ),
                rust: rust.iter().map(|s| s.to_string()).collect(),
            }
        }

        fn get(&self, path: &str) -> String {
            self.files.borrow().get(path).cloned().unwrap_or_default()
        }
    }

    impl FileSystem for MemFs {
        fn read(&self, rel_path: &str) -> Result<String, String> {
            self.files
                .borrow()
                .get(rel_path)
                .cloned()
                .ok_or_else(|| format!("no such file {rel_path}"))
        }

        fn write(&self, rel_path: &str, contents: &str) -> Result<(), String> {
            self.files
                .borrow_mut()
                .insert(rel_path.to_string(), contents.to_string());
            Ok(())
        }

        fn rust_sources(&self) -> Result<Vec<String>, String> {
            Ok(self.rust.clone())
        }
    }

    #[test]
    fn pin_cargo_string_form_preserves_no_operator() {
        let src = "[dependencies]\noxc_ast = \"0.29.0\"\nserde = \"1\"\n";
        let (out, summary) = pin_cargo_dependency(src, "oxc_ast", "0.133.0").unwrap();
        assert!(out.contains("oxc_ast = \"0.133.0\""));
        // serde untouched.
        assert!(out.contains("serde = \"1\""));
        assert!(summary.contains("0.29.0"));
        assert!(summary.contains("0.133.0"));
    }

    #[test]
    fn pin_cargo_table_form_preserves_operator_and_features() {
        let src = "[dependencies]\nnapi = { version = \"^2.10.2\", features = [\"napi4\"] }\n";
        let (out, _) = pin_cargo_dependency(src, "napi", "2.16.0").unwrap();
        assert!(out.contains("version = \"^2.16.0\""), "got: {out}");
        // features preserved.
        assert!(out.contains("features = [\"napi4\"]"));
    }

    #[test]
    fn pin_cargo_missing_dep_errors() {
        let src = "[dependencies]\nserde = \"1\"\n";
        assert!(pin_cargo_dependency(src, "absent", "1.0.0").is_err());
    }

    #[test]
    fn pin_npm_preserves_caret() {
        let src = "{\n  \"dependencies\": {\n    \"@angular/core\": \"^21.2.15\"\n  }\n}";
        let (out, _) = pin_npm_dependency(src, "@angular/core", "22.0.0").unwrap();
        assert!(out.contains("\"@angular/core\": \"^22.0.0\""), "got: {out}");
    }

    #[test]
    fn fs_repo_bump_edits_the_manifest_in_place() {
        let fs = MemFs::new(
            &[("Cargo.toml", "[dependencies]\noxc_ast = \"0.29.0\"\n")],
            &[],
        );
        let mut repo = FsRepo::new("/repo", ScriptedRunner::always_pass(), fs);
        let summary = repo.apply_bump(&oxc_plan()).unwrap();
        assert!(summary.contains("0.133.0"));
        assert_eq!(
            repo.fs.get("Cargo.toml"),
            "[dependencies]\noxc_ast = \"0.133.0\"\n"
        );
    }

    #[test]
    fn fs_repo_codemod_rewrites_only_matching_sources() {
        let fs = MemFs::new(
            &[
                ("src/a.rs", "use oxc_ast::VisitMut;\n"),
                ("src/b.rs", "fn untouched() {}\n"),
            ],
            &["src/a.rs", "src/b.rs"],
        );
        let mut repo = FsRepo::new("/repo", ScriptedRunner::always_pass(), fs);
        let changed = repo.apply_codemod(&visitmut_rule()).unwrap();
        assert!(changed);
        assert_eq!(repo.fs.get("src/a.rs"), "use oxc_ast_visit::VisitMut;\n");
        // The non-matching file is left byte-for-byte alone.
        assert_eq!(repo.fs.get("src/b.rs"), "fn untouched() {}\n");
        // Idempotent: a second run changes nothing.
        assert!(!repo.apply_codemod(&visitmut_rule()).unwrap());
    }

    #[test]
    fn fs_repo_drives_a_full_green_run() {
        // End-to-end through the orchestrator with the production repo over a
        // fake fs: bump the manifest, build red, codemod fixes it, build green.
        let rules = RuleSet {
            rules: vec![visitmut_rule()],
        };
        let orch = Orchestrator::new(OrchestrationConfig::execute(), &rules);

        let runner = ScriptedRunner::always_pass();
        runner.queue(
            "cargo build",
            CommandOutcome {
                exit_code: 1,
                stdout: String::new(),
                stderr: "unresolved import oxc_ast::VisitMut".into(),
            },
        );
        let fs = MemFs::new(
            &[
                ("Cargo.toml", "[dependencies]\noxc_ast = \"0.29.0\"\n"),
                ("src/lib.rs", "use oxc_ast::VisitMut;\n"),
            ],
            &["src/lib.rs"],
        );
        let mut repo = FsRepo::new("/repo", runner, fs);

        let outcome = orch.run_plan(&oxc_plan(), &mut repo);

        assert!(outcome.is_pr_ready(), "got {outcome:?}");
        // The manifest was bumped and the source codemodded on the real repo.
        assert_eq!(
            repo.fs.get("Cargo.toml"),
            "[dependencies]\noxc_ast = \"0.133.0\"\n"
        );
        assert_eq!(repo.fs.get("src/lib.rs"), "use oxc_ast_visit::VisitMut;\n");
    }
}
