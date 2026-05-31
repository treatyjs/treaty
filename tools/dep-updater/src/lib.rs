//! `dep-updater` — a deterministic, AI-free self-updater for the treaty repo's
//! own dependencies.
//!
//! This is a self-hosted renovate/dependabot tailored to this repo: it keeps
//! the repo's own Rust crates (especially the `oxc_*` family) and npm packages
//! current, and fixes the resulting breaking changes via a registry of
//! deterministic, idempotent codemods seeded from
//! `migration/OXC-MIGRATION-CRIB.md`.
//!
//! Pipeline (see `migration/DEP-UPDATER-PLAN.md`):
//! 1. **Detect** — parse the workspace manifests, query registries, emit
//!    [`UpdatePlan`]s.
//! 2. **Apply** — bump one dependency at a time on a branch.
//! 3. **Verify** — run the repo's own gates (build/test/oracle/oxlint),
//!    producing a [`VerifyResult`].
//! 4. **Codemod on breakage** — run the [`CodemodRule`]s whose `dep`/range
//!    cover the bump, then re-verify. Still red ⇒ emit a precise human report.
//!
//! There is no AI, GPU, or model in this loop: every step is a pure function of
//! the manifests, registry responses, and the rule set, so runs are
//! reproducible and idempotent.
//!
//! This crate is a **standalone** Cargo workspace (its `Cargo.toml` declares an
//! empty `[workspace]`); it is deliberately *not* a member of the parent treaty
//! workspace so that building or testing it never compiles `libs/render3` or
//! `apps/**`.

pub mod codemod;
pub mod detect;
pub mod model;
pub mod orchestrate;
pub mod process;

pub use codemod::{
    apply_rewrite, matcher_hits, oxc_29_to_133_rules, CodemodEngine, CodemodError, CodemodOutput,
};
pub use model::{
    CodemodRule, DepKind, Matcher, Rewrite, SemverClass, UpdatePlan, VerifyResult, VerifyStep,
};
pub use orchestrate::{
    Action, FailingReport, Gate, Mode, Orchestrator, OrchestrationConfig, Outcome, PrDescription,
    Repo, VerifyPhase,
};
pub use process::{CommandOutcome, CommandRunner, RealCommandRunner};

/// A named collection of codemod rules — the seeded OXC-CRIB rule set lives
/// here once populated. Kept as its own type so it can be (de)serialized to a
/// `rules.json` and unit-tested against old->new fixtures.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RuleSet {
    /// All registered rules.
    pub rules: Vec<CodemodRule>,
}

impl RuleSet {
    /// An empty rule set.
    pub fn new() -> Self {
        RuleSet::default()
    }

    /// Parse a rule set from JSON.
    pub fn from_json(json: &str) -> serde_json::Result<Self> {
        serde_json::from_str(json)
    }

    /// Serialize the rule set to pretty JSON.
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    /// Every rule that applies to a bump of `dep` landing on `version`.
    ///
    /// This is exactly the set the orchestration runs on breakage. Pure and
    /// order-preserving so codemod application is deterministic.
    pub fn rules_for<'a>(
        &'a self,
        dep: &'a str,
        version: &'a semver::Version,
    ) -> impl Iterator<Item = &'a CodemodRule> + 'a {
        self.rules.iter().filter(move |r| r.applies_to(dep, version))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use semver::Version;

    fn rule(id: &str, dep: &str, from: &str) -> CodemodRule {
        CodemodRule {
            id: id.into(),
            dep: dep.into(),
            from: Version::parse(from).unwrap(),
            to: None,
            matcher: Matcher::Literal { contains: "x".into() },
            rewrite: Rewrite::Replace { find: "a".into(), replace: "b".into() },
            description: String::new(),
        }
    }

    #[test]
    fn ruleset_filters_by_dep_and_version() {
        let set = RuleSet {
            rules: vec![
                rule("a", "oxc_ast", "0.30.0"),
                rule("b", "oxc_str", "0.133.0"),
                rule("c", "oxc_ast", "0.50.0"),
            ],
        };
        let v = Version::parse("0.133.0").unwrap();
        let ids: Vec<&str> = set.rules_for("oxc_ast", &v).map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "c"]);
    }

    #[test]
    fn ruleset_json_roundtrips() {
        let set = RuleSet { rules: vec![rule("a", "oxc_ast", "0.30.0")] };
        let json = set.to_json().unwrap();
        let back = RuleSet::from_json(&json).unwrap();
        assert_eq!(set, back);
    }
}
