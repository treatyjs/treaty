//! Core data model for the deterministic dependency updater.
//!
//! Everything here is plain data: serializable, comparable, and free of any
//! network or process side effects. The detector populates [`UpdatePlan`]s, the
//! codemod engine consumes [`CodemodRule`]s, and the orchestration records
//! [`VerifyResult`]s. No AI, no GPU — every decision is a pure function of these
//! structures.

use serde::{Deserialize, Serialize};

/// Which package ecosystem a dependency belongs to.
///
/// Determines both how the current version is read (Cargo.toml vs package.json)
/// and how the latest version is queried (crates.io index vs npm registry).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DepKind {
    /// A Rust crate resolved against the crates.io sparse index.
    Crate,
    /// An npm package resolved against the npm registry.
    Npm,
}

impl DepKind {
    /// Stable lowercase token used in plan ids and on the CLI.
    pub fn as_str(self) -> &'static str {
        match self {
            DepKind::Crate => "crate",
            DepKind::Npm => "npm",
        }
    }
}

impl std::fmt::Display for DepKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Classification of an update by its semver delta.
///
/// Computed deterministically from `current` -> `latest` via
/// [`SemverClass::classify`]. Drives risk gating: `Major` updates are the only
/// ones expected to require codemods; `Patch`/`Minor` should build clean.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SemverClass {
    /// Leading non-zero version component increased (e.g. 1.2.3 -> 2.0.0).
    Major,
    /// Minor increased within the same major (e.g. 1.2.3 -> 1.3.0).
    Minor,
    /// Only the patch increased (e.g. 1.2.3 -> 1.2.4).
    Patch,
    /// `latest` is not strictly greater than `current` (no real update).
    None,
}

impl SemverClass {
    /// Stable lowercase token used in plan ids and reports.
    pub fn as_str(self) -> &'static str {
        match self {
            SemverClass::Major => "major",
            SemverClass::Minor => "minor",
            SemverClass::Patch => "patch",
            SemverClass::None => "none",
        }
    }

    /// Classify the jump from `current` to `latest`.
    ///
    /// `0.x` releases are treated under standard semver arithmetic (a bump of
    /// the minor component on a `0.x` line reports [`SemverClass::Minor`]); the
    /// codemod gating layer is what knows that `0.x` minors can still break,
    /// not this raw classifier. Returns [`SemverClass::None`] when `latest` is
    /// not strictly newer than `current`.
    pub fn classify(current: &semver::Version, latest: &semver::Version) -> SemverClass {
        if latest <= current {
            return SemverClass::None;
        }
        if latest.major != current.major {
            SemverClass::Major
        } else if latest.minor != current.minor {
            SemverClass::Minor
        } else {
            SemverClass::Patch
        }
    }
}

impl std::fmt::Display for SemverClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A single proposed dependency bump.
///
/// One plan == one update applied in isolation on its own branch. The
/// orchestration bumps the manifest to `latest`, rebuilds, and on breakage
/// runs the codemods whose `dep`/range match this plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdatePlan {
    /// Dependency name as it appears in the manifest (e.g. `oxc_ast`, `typescript`).
    pub name: String,
    /// Ecosystem the dependency belongs to.
    pub kind: DepKind,
    /// The version currently pinned in the manifest.
    pub current: semver::Version,
    /// The newest version discovered in the registry.
    pub latest: semver::Version,
    /// Semver delta from `current` to `latest`.
    pub class: SemverClass,
    /// Manifest the dependency was read from (e.g. `Cargo.toml`, `libs/treaty-ivy/facade/Cargo.toml`).
    ///
    /// Recorded so the orchestration knows which file to edit for the bump and
    /// so reports point a human at the exact source of the pin.
    pub manifest: String,
}

impl UpdatePlan {
    /// Build a plan, classifying the semver delta automatically.
    pub fn new(
        name: impl Into<String>,
        kind: DepKind,
        current: semver::Version,
        latest: semver::Version,
        manifest: impl Into<String>,
    ) -> Self {
        let class = SemverClass::classify(&current, &latest);
        UpdatePlan {
            name: name.into(),
            kind,
            current,
            latest,
            class,
            manifest: manifest.into(),
        }
    }

    /// Whether this plan represents a real, applicable update.
    pub fn is_actionable(&self) -> bool {
        self.class != SemverClass::None
    }

    /// Deterministic, stable identifier for this plan.
    ///
    /// Used to name branches, key cached verify results, and correlate codemod
    /// runs. Pure function of the plan fields, so identical inputs always yield
    /// the same id (idempotency).
    pub fn id(&self) -> String {
        format!(
            "{}-{}-{}-to-{}",
            self.kind.as_str(),
            self.name,
            self.current,
            self.latest
        )
    }
}

/// How a [`CodemodRule`] decides whether a source line/region applies.
///
/// Deliberately small and declarative so rules stay pure and unit-testable
/// against old->new fixtures. Higher-fidelity AST matching (ast-grep / oxc) can
/// be layered later behind [`Matcher::AstGrep`] without changing this model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "matcher", rename_all = "snake_case")]
pub enum Matcher {
    /// Literal substring match. Simplest and fully deterministic.
    Literal { contains: String },
    /// Regex match. The pattern is validated when the rule is loaded.
    Regex { pattern: String },
    /// An ast-grep rule pattern (oxc-adjacent), applied structurally.
    AstGrep { pattern: String },
}

/// How a [`CodemodRule`] transforms a matched region.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "rewrite", rename_all = "snake_case")]
pub enum Rewrite {
    /// Replace every occurrence of `find` with `replace` (idempotent: once the
    /// replacement is present, re-running is a no-op as `find` no longer matches).
    Replace { find: String, replace: String },
    /// Apply a regex substitution. `$1`-style capture references are allowed.
    RegexReplace { pattern: String, replacement: String },
}

/// A deterministic source rewrite keyed to a specific dependency version jump.
///
/// Seeded from `migration/OXC-MIGRATION-CRIB.md`: each documented 0.29->0.133
/// API change becomes one rule. Rules are pure and idempotent; the engine runs
/// only those whose `dep` and `[from, to)` range cover the bump being applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodemodRule {
    /// Stable unique id (e.g. `oxc-visitmut-import-move`). Used in reports and
    /// to record which codemods resolved a given bump.
    pub id: String,
    /// Dependency this rule fixes (matches [`UpdatePlan::name`]).
    pub dep: String,
    /// Lowest dependency version this rule applies from (inclusive).
    pub from: semver::Version,
    /// Version at/above which the rule no longer applies (exclusive). `None`
    /// means "applies to every version >= `from`".
    pub to: Option<semver::Version>,
    /// How a candidate region is recognized.
    pub matcher: Matcher,
    /// How a recognized region is rewritten.
    pub rewrite: Rewrite,
    /// Human-readable note (the crib-sheet line this rule encodes).
    #[serde(default)]
    pub description: String,
}

impl CodemodRule {
    /// Whether this rule applies to a bump landing on `version` for `dep`.
    ///
    /// A rule applies when the dependency name matches and `version` falls in
    /// `[from, to)`. This is what gates "run only the relevant codemods".
    pub fn applies_to(&self, dep: &str, version: &semver::Version) -> bool {
        if self.dep != dep {
            return false;
        }
        if version < &self.from {
            return false;
        }
        match &self.to {
            Some(to) => version < to,
            None => true,
        }
    }
}

/// Outcome of verifying the repo after a bump (and any codemods).
///
/// Captures the green/red signal plus enough context for either a PR body or a
/// precise human-facing failure report. No interpretation is done here — the
/// orchestration decides what to do with `passed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyResult {
    /// The plan this result corresponds to (via [`UpdatePlan::id`]).
    pub plan_id: String,
    /// True iff every verification step succeeded.
    pub passed: bool,
    /// Ordered record of each step that ran (build, test, oracle, ...).
    pub steps: Vec<VerifyStep>,
    /// Ids of codemod rules that were applied before this verification.
    #[serde(default)]
    pub codemods_applied: Vec<String>,
}

impl VerifyResult {
    /// A passing result with no steps yet recorded.
    pub fn new(plan_id: impl Into<String>) -> Self {
        VerifyResult {
            plan_id: plan_id.into(),
            passed: true,
            steps: Vec::new(),
            codemods_applied: Vec::new(),
        }
    }

    /// Record a step and fold its success into the aggregate `passed` flag.
    pub fn record(&mut self, step: VerifyStep) {
        self.passed &= step.success;
        self.steps.push(step);
    }

    /// The first failing step, if any — the precise thing a human must look at.
    pub fn first_failure(&self) -> Option<&VerifyStep> {
        self.steps.iter().find(|s| !s.success)
    }
}

/// One verification command and its result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyStep {
    /// Short label (e.g. `cargo build`, `cargo test`, `oracle`, `oxlint`).
    pub name: String,
    /// Process exit status: zero is success.
    pub exit_code: i32,
    /// Whether the step is considered to have passed.
    pub success: bool,
    /// Captured stderr tail, truncated for reports (no stdout spam).
    #[serde(default)]
    pub stderr_excerpt: String,
}

impl VerifyStep {
    /// Build a step from a finished command outcome.
    pub fn from_exit(name: impl Into<String>, exit_code: i32, stderr_excerpt: impl Into<String>) -> Self {
        VerifyStep {
            name: name.into(),
            exit_code,
            success: exit_code == 0,
            stderr_excerpt: stderr_excerpt.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use semver::Version;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn classify_covers_all_deltas() {
        assert_eq!(SemverClass::classify(&v("1.2.3"), &v("2.0.0")), SemverClass::Major);
        assert_eq!(SemverClass::classify(&v("1.2.3"), &v("1.3.0")), SemverClass::Minor);
        assert_eq!(SemverClass::classify(&v("1.2.3"), &v("1.2.4")), SemverClass::Patch);
        assert_eq!(SemverClass::classify(&v("1.2.3"), &v("1.2.3")), SemverClass::None);
        assert_eq!(SemverClass::classify(&v("2.0.0"), &v("1.9.9")), SemverClass::None);
    }

    #[test]
    fn zerover_minor_is_minor_class() {
        // The raw classifier uses plain semver arithmetic; codemod gating is
        // what knows 0.x minors can break.
        assert_eq!(SemverClass::classify(&v("0.29.0"), &v("0.133.0")), SemverClass::Minor);
    }

    #[test]
    fn plan_id_is_deterministic_and_actionable() {
        let p = UpdatePlan::new("oxc_ast", DepKind::Crate, v("0.29.0"), v("0.133.0"), "Cargo.toml");
        assert_eq!(p.id(), "crate-oxc_ast-0.29.0-to-0.133.0");
        assert_eq!(p.id(), p.clone().id());
        assert!(p.is_actionable());
        assert_eq!(p.class, SemverClass::Minor);
    }

    #[test]
    fn non_update_plan_is_not_actionable() {
        let p = UpdatePlan::new("serde", DepKind::Crate, v("1.0.0"), v("1.0.0"), "Cargo.toml");
        assert!(!p.is_actionable());
    }

    #[test]
    fn codemod_range_gating() {
        let rule = CodemodRule {
            id: "oxc-visitmut".into(),
            dep: "oxc_ast".into(),
            from: v("0.30.0"),
            to: Some(v("0.133.0")),
            matcher: Matcher::Literal { contains: "use oxc_ast::VisitMut".into() },
            rewrite: Rewrite::Replace { find: "a".into(), replace: "b".into() },
            description: String::new(),
        };
        assert!(!rule.applies_to("oxc_ast", &v("0.29.0"))); // below from
        assert!(rule.applies_to("oxc_ast", &v("0.30.0"))); // inclusive lower
        assert!(rule.applies_to("oxc_ast", &v("0.132.9")));
        assert!(!rule.applies_to("oxc_ast", &v("0.133.0"))); // exclusive upper
        assert!(!rule.applies_to("serde", &v("0.50.0"))); // wrong dep
    }

    #[test]
    fn open_ended_range_applies_forever() {
        let rule = CodemodRule {
            id: "x".into(),
            dep: "oxc_str".into(),
            from: v("0.133.0"),
            to: None,
            matcher: Matcher::Literal { contains: "".into() },
            rewrite: Rewrite::Replace { find: "a".into(), replace: "b".into() },
            description: String::new(),
        };
        assert!(rule.applies_to("oxc_str", &v("0.133.0")));
        assert!(rule.applies_to("oxc_str", &v("1.0.0")));
    }

    #[test]
    fn verify_result_aggregates_and_reports_first_failure() {
        let mut r = VerifyResult::new("crate-oxc_ast-0.29.0-to-0.133.0");
        r.record(VerifyStep::from_exit("cargo build", 0, ""));
        assert!(r.passed);
        r.record(VerifyStep::from_exit("cargo test", 101, "panicked"));
        assert!(!r.passed);
        assert_eq!(r.first_failure().unwrap().name, "cargo test");
    }

    #[test]
    fn model_roundtrips_through_json() {
        let p = UpdatePlan::new("typescript", DepKind::Npm, v("5.4.0"), v("5.5.0"), "package.json");
        let json = serde_json::to_string(&p).unwrap();
        let back: UpdatePlan = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }
}
