//! CI affected-change detection over Treaty's federated module graph.
//!
//! This is the Rust port of `@treaty/federation-deploy`'s `affected.ts`.
//! Federation is Treaty's unit of **deployment granularity**: the compiler emits
//! the host, every lazy feature route, and every library as an independently
//! versioned, deployable, rollback-able module. CI should only compile, test,
//! and deploy the modules a change actually touches — not the whole app.
//!
//! The core is [`compute_affected_modules`]: given a set of changed file paths
//! and a [`ModuleDependencyGraph`] (`moduleId -> { files, depends_on }`), it
//! returns the set of modules that changed. A module is affected when **one of
//! its own files changed**, OR when a module it `depends_on` is (transitively)
//! affected — the shared-lib fan-out: editing a shared library marks every
//! route/lib that depends on it, directly or through other libs.
//!
//! The computation is pure and deterministic: same inputs, same sorted output.
//! It makes no assumptions about path format (it compares the strings it is
//! given), so callers can pass repo-relative or absolute paths as long as they
//! are consistent with the graph's `files`.

use std::collections::{BTreeMap, HashSet};

/// One node in the federated module dependency graph: the source `files` that
/// make up the module, and the `moduleId`s it `depends_on`. Edges are directed
/// consumer -> dependency; affectedness propagates the opposite way (a changed
/// dependency marks its consumers).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModuleNode {
    /// Source files belonging to this module. Compared verbatim against changed
    /// paths (subject to the configured matcher).
    pub files: Vec<String>,
    /// Module ids this module directly depends on (consumes the code of).
    pub depends_on: Vec<String>,
}

impl ModuleNode {
    /// Build a node from file + dependency lists.
    pub fn new(
        files: impl IntoIterator<Item = impl Into<String>>,
        depends_on: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        ModuleNode {
            files: files.into_iter().map(Into::into).collect(),
            depends_on: depends_on.into_iter().map(Into::into).collect(),
        }
    }
}

/// The federated module dependency graph: every `moduleId` (host, route
/// remotes, libs) mapped to its [`ModuleNode`]. A `depends_on` entry that is not
/// itself a key is a dangling edge; see [`OnMissingDependency`].
///
/// Backed by a [`BTreeMap`] so iteration (and thus the result) is deterministic
/// regardless of insertion order.
pub type ModuleDependencyGraph = BTreeMap<String, ModuleNode>;

/// How to match a changed file path against a module's `files`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileMatch {
    /// Exact string equality (the default, matching the TS port).
    Exact,
    /// A module file that is a path prefix of the changed path matches it. Lets
    /// a directory entry own every file beneath it.
    Prefix,
}

impl Default for FileMatch {
    fn default() -> Self {
        FileMatch::Exact
    }
}

impl FileMatch {
    fn matches(self, changed_path: &str, module_file: &str) -> bool {
        match self {
            FileMatch::Exact => changed_path == module_file,
            FileMatch::Prefix => changed_path == module_file || changed_path.starts_with(module_file),
        }
    }
}

/// What to do when a `depends_on` edge names a module the graph has no node for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnMissingDependency {
    /// Skip the dangling edge (the default).
    Ignore,
    /// Surface a malformed graph in CI early.
    Error,
}

impl Default for OnMissingDependency {
    fn default() -> Self {
        OnMissingDependency::Ignore
    }
}

/// Options for [`compute_affected_modules`].
#[derive(Debug, Clone, Default)]
pub struct ComputeAffectedOptions {
    pub match_file: FileMatch,
    pub on_missing_dependency: OnMissingDependency,
}

/// Errors surfaced by [`compute_affected_modules`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AffectedError {
    /// A `depends_on` edge named a module the graph has no node for, under
    /// [`OnMissingDependency::Error`].
    MissingDependency { module: String, dependency: String },
    /// The graph is not a DAG (Treaty's module graph must be acyclic).
    Cycle { module: String },
}

impl std::fmt::Display for AffectedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AffectedError::MissingDependency { module, dependency } => write!(
                f,
                "module {module:?} dependsOn unknown module {dependency:?}"
            ),
            AffectedError::Cycle { module } => {
                write!(f, "dependency cycle detected at module {module:?}")
            }
        }
    }
}

impl std::error::Error for AffectedError {}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    InProgress,
    Affected,
    Unaffected,
}

/// Compute the set of federated modules a change actually affects.
///
/// A module is **affected** if either:
///   1. one of its own `files` matches a changed path (a direct edit), or
///   2. any module it `depends_on` is affected — applied transitively, so a
///      change to a shared lib fans out to every route/lib that (directly or
///      through intermediate libs) depends on it.
///
/// Pure and deterministic: the returned vector is sorted and contains each
/// affected `moduleId` exactly once. Modules absent from the result need no
/// recompile/retest/redeploy for this change.
///
/// # Errors
/// Returns [`AffectedError::MissingDependency`] under
/// [`OnMissingDependency::Error`] for a dangling edge, or
/// [`AffectedError::Cycle`] if the graph is not a DAG.
pub fn compute_affected_modules<'a>(
    changed_files: impl IntoIterator<Item = &'a str>,
    graph: &ModuleDependencyGraph,
    options: &ComputeAffectedOptions,
) -> Result<Vec<String>, AffectedError> {
    let changed: HashSet<&str> = changed_files.into_iter().collect();

    // Validate dependsOn edges up front so 'error' mode surfaces a bad graph
    // even for modules that turn out not to be affected.
    if options.on_missing_dependency == OnMissingDependency::Error {
        for (id, node) in graph {
            for dep in &node.depends_on {
                if !graph.contains_key(dep) {
                    return Err(AffectedError::MissingDependency {
                        module: id.clone(),
                        dependency: dep.clone(),
                    });
                }
            }
        }
    }

    let mut state: BTreeMap<&str, State> = BTreeMap::new();
    let mut result = Vec::new();

    // BTreeMap iteration is sorted, so the result is already sorted.
    for id in graph.keys() {
        if is_affected(id, graph, &changed, options, &mut state)? {
            result.push(id.clone());
        }
    }
    Ok(result)
}

fn is_affected<'g>(
    id: &'g str,
    graph: &'g ModuleDependencyGraph,
    changed: &HashSet<&str>,
    options: &ComputeAffectedOptions,
    state: &mut BTreeMap<&'g str, State>,
) -> Result<bool, AffectedError> {
    match state.get(id) {
        Some(State::Affected) => return Ok(true),
        Some(State::Unaffected) => return Ok(false),
        Some(State::InProgress) => {
            return Err(AffectedError::Cycle { module: id.to_string() });
        }
        None => {}
    }
    state.insert(id, State::InProgress);

    let node = &graph[id];

    // 1. A direct edit to one of this module's own files.
    let mut affected = node
        .files
        .iter()
        .any(|file| changed.iter().any(|c| options.match_file.matches(c, file)));

    // 2. Fan-out: affected if any dependency is (transitively) affected.
    if !affected {
        for dep in &node.depends_on {
            // Resolve `dep` to the graph's owned key so the recursive borrow has
            // the graph's lifetime (not the node's).
            let dep_key = match graph.get_key_value(dep.as_str()) {
                Some((k, _)) => k.as_str(),
                None => continue, // dangling; 'error' mode already returned above
            };
            if is_affected(dep_key, graph, changed, options, state)? {
                affected = true;
                break;
            }
        }
    }

    state.insert(
        id,
        if affected { State::Affected } else { State::Unaffected },
    );
    Ok(affected)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph() -> ModuleDependencyGraph {
        // host -> {dashboard, settings}; dashboard -> ui-kit; settings -> ui-kit;
        // ui-kit is a shared lib. orphan depends on nothing shared.
        let mut g = ModuleDependencyGraph::new();
        g.insert("host".into(), ModuleNode::new(["src/main.ts"], ["dashboard", "settings"]));
        g.insert(
            "dashboard".into(),
            ModuleNode::new(["src/features/dashboard.ts"], ["ui-kit"]),
        );
        g.insert(
            "settings".into(),
            ModuleNode::new(["src/features/settings.ts"], ["ui-kit"]),
        );
        g.insert("ui-kit".into(), ModuleNode::new(["libs/ui-kit/index.ts"], Vec::<String>::new()));
        g.insert("orphan".into(), ModuleNode::new(["libs/orphan/x.ts"], Vec::<String>::new()));
        g
    }

    #[test]
    fn shared_lib_change_fans_out_to_all_consumers() {
        let g = graph();
        let affected = compute_affected_modules(
            ["libs/ui-kit/index.ts"],
            &g,
            &ComputeAffectedOptions::default(),
        )
        .unwrap();
        // ui-kit changed -> dashboard, settings (depend on it), host (depends on
        // those) are all affected. orphan is untouched.
        assert_eq!(affected, vec!["dashboard", "host", "settings", "ui-kit"]);
        assert!(!affected.contains(&"orphan".to_string()));
    }

    #[test]
    fn leaf_change_affects_only_the_module_and_its_consumers() {
        let g = graph();
        let affected = compute_affected_modules(
            ["src/features/dashboard.ts"],
            &g,
            &ComputeAffectedOptions::default(),
        )
        .unwrap();
        // dashboard changed -> host (consumes it). settings/ui-kit/orphan unaffected.
        assert_eq!(affected, vec!["dashboard", "host"]);
    }

    #[test]
    fn no_change_yields_nothing() {
        let g = graph();
        let affected =
            compute_affected_modules(["unrelated/file.ts"], &g, &ComputeAffectedOptions::default())
                .unwrap();
        assert!(affected.is_empty());
    }

    #[test]
    fn prefix_matcher_owns_files_beneath_a_dir() {
        let mut g = ModuleDependencyGraph::new();
        g.insert("lib".into(), ModuleNode::new(["libs/ui/"], Vec::<String>::new()));
        let opts = ComputeAffectedOptions { match_file: FileMatch::Prefix, ..Default::default() };
        let affected = compute_affected_modules(["libs/ui/button.ts"], &g, &opts).unwrap();
        assert_eq!(affected, vec!["lib"]);
    }

    #[test]
    fn missing_dependency_errors_in_error_mode() {
        let mut g = ModuleDependencyGraph::new();
        g.insert("a".into(), ModuleNode::new(["a.ts"], ["ghost"]));
        let opts = ComputeAffectedOptions {
            on_missing_dependency: OnMissingDependency::Error,
            ..Default::default()
        };
        let err = compute_affected_modules(["a.ts"], &g, &opts).unwrap_err();
        assert_eq!(
            err,
            AffectedError::MissingDependency { module: "a".into(), dependency: "ghost".into() }
        );
        // Ignore mode tolerates the dangling edge.
        let ok = compute_affected_modules(["a.ts"], &g, &ComputeAffectedOptions::default()).unwrap();
        assert_eq!(ok, vec!["a"]);
    }

    #[test]
    fn cycle_is_detected() {
        let mut g = ModuleDependencyGraph::new();
        g.insert("a".into(), ModuleNode::new(Vec::<String>::new(), ["b"]));
        g.insert("b".into(), ModuleNode::new(Vec::<String>::new(), ["a"]));
        let err = compute_affected_modules(["x"], &g, &ComputeAffectedOptions::default()).unwrap_err();
        assert!(matches!(err, AffectedError::Cycle { .. }));
    }
}
