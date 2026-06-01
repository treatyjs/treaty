//! The detector: read the workspace manifests, discover every direct
//! dependency, query the registries for the newest released version, and turn
//! the result into an ordered list of [`UpdatePlan`]s.
//!
//! Three layers, each pure and independently testable:
//!
//! 1. **Manifest parsing** ([`parse_cargo_manifest`], [`parse_package_json`],
//!    [`discover_dependencies`]) — read-only over the repo's `Cargo.toml`s and
//!    `package.json`s, producing a deduplicated [`Dependency`] list. No network.
//! 2. **Registry version query** ([`RegistryClient`]) — a trait that maps a
//!    [`Dependency`] to its newest published [`semver::Version`]. The real
//!    implementations parse the crates.io *sparse index* line format and the
//!    npm registry's package JSON; both are split so the network fetch sits
//!    behind a separate [`Fetcher`] seam, leaving the parsing deterministic and
//!    unit-testable. Tests inject a [`StaticRegistry`].
//! 3. **Plan building** ([`build_update_plan`]) — joins the discovered
//!    dependencies with the latest versions, keeps only the genuinely outdated
//!    ones, classifies each by semver delta, and returns them in a stable order.
//!
//! Everything is deterministic and idempotent: identical manifests + identical
//! registry answers always yield byte-identical plans, regardless of the order
//! the manifests were read in.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::model::{DepKind, UpdatePlan};

/// One direct dependency discovered in a manifest.
///
/// `current` is the *minimum* version the requirement pins to (the semver core
/// extracted from a Cargo requirement like `"0.133.0"` or an npm range like
/// `"^21.2.15"`). It is what we compare the registry's latest against to decide
/// whether an update exists and how large the jump is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    /// Dependency name as written in the manifest (e.g. `oxc_ast`, `@angular/core`).
    pub name: String,
    /// Ecosystem the dependency belongs to.
    pub kind: DepKind,
    /// The semver core pinned by the manifest requirement.
    pub current: semver::Version,
    /// The raw requirement string as written (e.g. `^21.2.15`, `~5.9.0`, `0.133.0`).
    ///
    /// Retained verbatim so a later bump step can preserve the operator prefix
    /// (`^`/`~`) when it rewrites the manifest, and so reports can show exactly
    /// what the manifest said.
    pub requirement: String,
    /// Manifest path the dependency was read from, relative to the repo root
    /// (e.g. `Cargo.toml`, `libs/treaty-ivy/facade/Cargo.toml`, `package.json`).
    pub manifest: String,
}

/// Errors the detector can produce.
#[derive(Debug)]
pub enum DetectError {
    /// A manifest file could not be read.
    Io { path: String, source: std::io::Error },
    /// A `Cargo.toml` could not be parsed as TOML.
    Toml { path: String, source: toml::de::Error },
    /// A `package.json` could not be parsed as JSON.
    Json { path: String, source: serde_json::Error },
}

impl std::fmt::Display for DetectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DetectError::Io { path, source } => write!(f, "reading {path}: {source}"),
            DetectError::Toml { path, source } => write!(f, "parsing TOML {path}: {source}"),
            DetectError::Json { path, source } => write!(f, "parsing JSON {path}: {source}"),
        }
    }
}

impl std::error::Error for DetectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DetectError::Io { source, .. } => Some(source),
            DetectError::Toml { source, .. } => Some(source),
            DetectError::Json { source, .. } => Some(source),
        }
    }
}

// ---------------------------------------------------------------------------
// Layer 1: manifest parsing
// ---------------------------------------------------------------------------

/// Parse a Cargo dependency requirement string into its semver core.
///
/// Cargo's default (caret) requirements are written bare (`"1.2.3"`,
/// `"0.133.0"`); explicit operators (`^`, `~`, `>=`, `=`, `<`, `>`) and
/// wildcards (`*`, `1.*`) are also accepted. We extract the leading
/// `major[.minor[.patch]]` and zero-fill missing components so a requirement
/// like `"1"` or `"0.133"` still yields a comparable [`semver::Version`].
///
/// Returns `None` for requirements with no concrete numeric core (a bare `*`),
/// which are intentionally skipped: there is no current version to update from.
fn parse_cargo_req(req: &str) -> Option<semver::Version> {
    parse_loose_version(req)
}

/// Parse an npm version specifier into the semver core it pins.
///
/// Handles the common range operators (`^`, `~`, `>=`, `=`, `v`) plus bare
/// versions. Non-semver specifiers npm allows — dist-tags (`latest`, `next`),
/// URLs, `workspace:`/`file:`/`link:` protocols, and bespoke channel strings
/// like `volar-2.4` — have no numeric core and yield `None`, so they are
/// skipped by the detector (nothing meaningful to compare against a registry
/// version).
fn parse_npm_req(req: &str) -> Option<semver::Version> {
    let trimmed = req.trim();
    // Reject protocol/URL/tag specifiers up front: they carry no version core
    // we can responsibly bump.
    if trimmed.is_empty()
        || trimmed.contains(':')
        || trimmed.contains('/')
        || trimmed.starts_with("npm:")
    {
        return None;
    }
    parse_loose_version(trimmed)
}

/// Shared loose-version extractor for both ecosystems.
///
/// Strips a single leading range operator / `v` prefix, then reads the leading
/// run of `digit`/`.` characters as `major[.minor[.patch]]`, zero-filling
/// missing components and discarding any pre-release/build/wildcard tail. This
/// is deliberately permissive: the goal is a comparable lower-bound version,
/// not full range semantics.
fn parse_loose_version(req: &str) -> Option<semver::Version> {
    let s = req.trim();
    // Drop a leading comparator/operator.
    let s = s
        .trim_start_matches(['^', '~', '=', '>', '<', ' '])
        .trim_start();
    let s = s.strip_prefix('v').unwrap_or(s);

    // Read the leading numeric dotted core.
    let core: String = s
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let mut parts = core
        .split('.')
        .filter(|p| !p.is_empty())
        .map(|p| p.parse::<u64>().ok());

    let major = parts.next()??;
    let minor = parts.next().flatten().unwrap_or(0);
    let patch = parts.next().flatten().unwrap_or(0);
    Some(semver::Version::new(major, minor, patch))
}

/// Extract `[dependencies]`, `[dev-dependencies]`, and `[build-dependencies]`
/// from a parsed `Cargo.toml`, returning concrete registry dependencies.
///
/// Skips:
/// - `path`/`git` dependencies (no registry version to track),
/// - `workspace = true` inheritance stubs (the version lives in the root
///   `[workspace.dependencies]`, captured when that file is parsed),
/// - requirements with no numeric core.
///
/// `manifest` is the repo-relative path recorded on each [`Dependency`].
pub fn parse_cargo_manifest(toml_src: &str, manifest: &str) -> Result<Vec<Dependency>, DetectError> {
    let value: toml::Value = toml::from_str(toml_src).map_err(|source| DetectError::Toml {
        path: manifest.to_string(),
        source,
    })?;

    let mut out = Vec::new();

    // Ordinary dependency tables on the manifest.
    for table_key in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(table) = value.get(table_key).and_then(toml::Value::as_table) {
            collect_cargo_table(table, manifest, &mut out);
        }
    }

    // Root workspace dependency table: `[workspace.dependencies]`.
    if let Some(ws) = value.get("workspace").and_then(toml::Value::as_table) {
        if let Some(table) = ws.get("dependencies").and_then(toml::Value::as_table) {
            collect_cargo_table(table, manifest, &mut out);
        }
    }

    Ok(out)
}

/// Pull concrete dependencies out of a single Cargo dependency table.
fn collect_cargo_table(table: &toml::Table, manifest: &str, out: &mut Vec<Dependency>) {
    for (name, spec) in table {
        let req = match spec {
            // `dep = "1.2.3"`
            toml::Value::String(s) => s.clone(),
            // `dep = { version = "1.2.3", ... }`
            toml::Value::Table(t) => {
                // Skip path/git/workspace-inherited deps: no registry version.
                if t.contains_key("path")
                    || t.contains_key("git")
                    || t.get("workspace")
                        .and_then(toml::Value::as_bool)
                        .unwrap_or(false)
                {
                    continue;
                }
                match t.get("version").and_then(toml::Value::as_str) {
                    Some(v) => v.to_string(),
                    None => continue,
                }
            }
            _ => continue,
        };

        if let Some(current) = parse_cargo_req(&req) {
            out.push(Dependency {
                name: name.clone(),
                kind: DepKind::Crate,
                current,
                requirement: req,
                manifest: manifest.to_string(),
            });
        }
    }
}

/// Parse a `package.json`, returning its `dependencies` and `devDependencies`
/// as concrete npm registry dependencies.
///
/// Specifiers with no numeric core (dist-tags, protocols, URLs) are skipped.
/// `manifest` is the repo-relative path recorded on each [`Dependency`].
pub fn parse_package_json(json_src: &str, manifest: &str) -> Result<Vec<Dependency>, DetectError> {
    let value: serde_json::Value =
        serde_json::from_str(json_src).map_err(|source| DetectError::Json {
            path: manifest.to_string(),
            source,
        })?;

    let mut out = Vec::new();
    for table_key in ["dependencies", "devDependencies"] {
        if let Some(map) = value.get(table_key).and_then(serde_json::Value::as_object) {
            for (name, spec) in map {
                let Some(req) = spec.as_str() else { continue };
                if let Some(current) = parse_npm_req(req) {
                    out.push(Dependency {
                        name: name.clone(),
                        kind: DepKind::Npm,
                        current,
                        requirement: req.to_string(),
                        manifest: manifest.to_string(),
                    });
                }
            }
        }
    }
    Ok(out)
}

/// Read the root `Cargo.toml`'s `[workspace] members` list.
///
/// Returns the member directory paths exactly as written (repo-relative).
/// Returns an empty vec for a manifest with no workspace members.
pub fn workspace_members(root_cargo_src: &str, manifest: &str) -> Result<Vec<String>, DetectError> {
    let value: toml::Value = toml::from_str(root_cargo_src).map_err(|source| DetectError::Toml {
        path: manifest.to_string(),
        source,
    })?;
    let members = value
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(toml::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(toml::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Ok(members)
}

/// Discover every direct dependency reachable from a repo root.
///
/// Reads the root `Cargo.toml` (its `[workspace]` members and any
/// `[workspace.dependencies]`), each member crate's `Cargo.toml`, and the root
/// `package.json` if present. The results are deduplicated by
/// `(kind, name, manifest)` and returned in a stable, sorted order so the
/// downstream plan is deterministic regardless of filesystem iteration order.
///
/// This is the one function that touches the filesystem; all reads are
/// read-only. Missing optional files (e.g. no root `package.json`) are skipped
/// silently — only genuine read/parse failures surface as [`DetectError`].
pub fn discover_dependencies(repo_root: &Path) -> Result<Vec<Dependency>, DetectError> {
    let mut deps = Vec::new();

    // Root Cargo.toml + workspace members.
    let root_cargo = repo_root.join("Cargo.toml");
    if let Some(src) = read_optional(&root_cargo)? {
        deps.extend(parse_cargo_manifest(&src, "Cargo.toml")?);
        for member in workspace_members(&src, "Cargo.toml")? {
            let member_path = repo_root.join(&member).join("Cargo.toml");
            if let Some(member_src) = read_optional(&member_path)? {
                let rel = format!("{member}/Cargo.toml");
                deps.extend(parse_cargo_manifest(&member_src, &rel)?);
            }
        }
    }

    // Root package.json.
    let root_pkg = repo_root.join("package.json");
    if let Some(src) = read_optional(&root_pkg)? {
        deps.extend(parse_package_json(&src, "package.json")?);
    }

    deps.sort_by(|a, b| {
        a.kind
            .as_str()
            .cmp(b.kind.as_str())
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.manifest.cmp(&b.manifest))
    });
    deps.dedup_by(|a, b| a.kind == b.kind && a.name == b.name && a.manifest == b.manifest);

    Ok(deps)
}

/// Read a file, returning `Ok(None)` if it does not exist and surfacing any
/// other I/O error as [`DetectError::Io`].
fn read_optional(path: &Path) -> Result<Option<String>, DetectError> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(DetectError::Io {
            path: path.display().to_string(),
            source,
        }),
    }
}

// ---------------------------------------------------------------------------
// Layer 2: registry version query (mockable behind a trait)
// ---------------------------------------------------------------------------

/// Looks up the newest published version of a dependency.
///
/// The detector depends only on this trait, so plan building can be tested
/// against a [`StaticRegistry`] with no network access. Production wiring uses
/// [`SparseIndexRegistry`] / [`NpmRegistry`], which keep their *parsing* pure
/// and obtain bytes through a separate [`Fetcher`] seam.
pub trait RegistryClient {
    /// The newest version published for `dep`, or `None` if it is unknown /
    /// unpublished / could not be resolved. Errors that should abort detection
    /// are surfaced as `Err`; a simple "not found" is `Ok(None)`.
    fn latest_version(&self, dep: &Dependency) -> Result<Option<semver::Version>, RegistryError>;
}

/// Error from a registry lookup.
#[derive(Debug)]
pub enum RegistryError {
    /// The underlying fetch failed (network, process, etc.).
    Fetch(String),
    /// The registry payload could not be parsed.
    Parse(String),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::Fetch(m) => write!(f, "registry fetch failed: {m}"),
            RegistryError::Parse(m) => write!(f, "registry parse failed: {m}"),
        }
    }
}

impl std::error::Error for RegistryError {}

/// Fetches raw registry payloads by URL.
///
/// This is the *only* network seam. Splitting it from the registry clients
/// keeps the index/JSON parsing deterministic and unit-testable while leaving
/// the actual transport (HTTP via curl, a cached mirror, etc.) swappable.
pub trait Fetcher {
    /// Fetch the body at `url` as text. A 404 should be reported as
    /// `Ok(None)` so the caller can treat it as "unknown package".
    fn get(&self, url: &str) -> Result<Option<String>, RegistryError>;
}

/// A registry that answers from an in-memory table — the test double.
///
/// Keyed by `(kind, name)`. Lookups for unknown dependencies return
/// `Ok(None)`, mirroring an unpublished package. Construction is order
/// independent, so tests read deterministically.
#[derive(Debug, Default, Clone)]
pub struct StaticRegistry {
    versions: HashMap<(DepKind, String), semver::Version>,
}

impl StaticRegistry {
    /// An empty registry (every lookup is `None`).
    pub fn new() -> Self {
        StaticRegistry::default()
    }

    /// Register the latest version for a `(kind, name)` pair.
    pub fn with(mut self, kind: DepKind, name: &str, latest: &str) -> Self {
        self.versions.insert(
            (kind, name.to_string()),
            semver::Version::parse(latest).expect("test version must be valid semver"),
        );
        self
    }
}

impl RegistryClient for StaticRegistry {
    fn latest_version(&self, dep: &Dependency) -> Result<Option<semver::Version>, RegistryError> {
        Ok(self
            .versions
            .get(&(dep.kind, dep.name.clone()))
            .cloned())
    }
}

/// crates.io *sparse index* registry client.
///
/// Resolves the sparse-index path for a crate, fetches the newline-delimited
/// JSON document via the injected [`Fetcher`], and picks the newest
/// non-yanked, non-prerelease version. The path derivation and line parsing
/// are pure and unit-tested; only the fetch hits the network.
pub struct SparseIndexRegistry<F: Fetcher> {
    /// Sparse index base, e.g. `https://index.crates.io`.
    base: String,
    fetcher: F,
}

impl<F: Fetcher> SparseIndexRegistry<F> {
    /// Build a client against the canonical crates.io sparse index.
    pub fn crates_io(fetcher: F) -> Self {
        SparseIndexRegistry {
            base: "https://index.crates.io".to_string(),
            fetcher,
        }
    }

    /// Build a client against an arbitrary sparse-index `base` URL.
    pub fn with_base(base: impl Into<String>, fetcher: F) -> Self {
        SparseIndexRegistry {
            base: base.into(),
            fetcher,
        }
    }

    /// The full sparse-index URL for `crate_name`.
    pub fn index_url(&self, crate_name: &str) -> String {
        format!("{}/{}", self.base.trim_end_matches('/'), index_path(crate_name))
    }
}

/// Derive the crates.io sparse-index relative path for a crate name.
///
/// The index lays files out by name length:
/// - 1 char  → `1/{name}`
/// - 2 chars → `2/{name}`
/// - 3 chars → `3/{first}/{name}`
/// - 4+      → `{first2}/{next2}/{name}`
///
/// Names are lowercased per the index's case-insensitive convention.
pub fn index_path(crate_name: &str) -> String {
    let name = crate_name.to_ascii_lowercase();
    let bytes = name.as_bytes();
    match bytes.len() {
        0 => name,
        1 => format!("1/{name}"),
        2 => format!("2/{name}"),
        3 => format!("3/{}/{name}", &name[0..1]),
        _ => format!("{}/{}/{name}", &name[0..2], &name[2..4]),
    }
}

/// Pick the newest stable, non-yanked version from sparse-index NDJSON.
///
/// Each non-empty line is a JSON object with at least `vers` and (optionally)
/// `yanked`. Yanked versions and pre-releases are ignored. Returns `None` when
/// no usable version remains.
pub fn parse_sparse_index(body: &str) -> Result<Option<semver::Version>, RegistryError> {
    let mut best: Option<semver::Version> = None;
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let entry: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| RegistryError::Parse(format!("index line: {e}")))?;
        if entry.get("yanked").and_then(serde_json::Value::as_bool) == Some(true) {
            continue;
        }
        let Some(vers) = entry.get("vers").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Ok(version) = semver::Version::parse(vers) else {
            continue;
        };
        if !version.pre.is_empty() {
            continue;
        }
        if best.as_ref().is_none_or(|b| &version > b) {
            best = Some(version);
        }
    }
    Ok(best)
}

impl<F: Fetcher> RegistryClient for SparseIndexRegistry<F> {
    fn latest_version(&self, dep: &Dependency) -> Result<Option<semver::Version>, RegistryError> {
        let url = self.index_url(&dep.name);
        match self.fetcher.get(&url)? {
            Some(body) => parse_sparse_index(&body),
            None => Ok(None),
        }
    }
}

/// npm registry client.
///
/// Fetches the package document (`{base}/{name}`) via the injected [`Fetcher`]
/// and reads `dist-tags.latest`. Both the URL derivation (which percent-aware
/// encodes the `/` in scoped names) and the JSON parsing are pure.
pub struct NpmRegistry<F: Fetcher> {
    base: String,
    fetcher: F,
}

impl<F: Fetcher> NpmRegistry<F> {
    /// Build a client against the canonical npm registry.
    pub fn npmjs(fetcher: F) -> Self {
        NpmRegistry {
            base: "https://registry.npmjs.org".to_string(),
            fetcher,
        }
    }

    /// Build a client against an arbitrary registry `base` URL.
    pub fn with_base(base: impl Into<String>, fetcher: F) -> Self {
        NpmRegistry {
            base: base.into(),
            fetcher,
        }
    }

    /// The package-document URL for `pkg`.
    pub fn package_url(&self, pkg: &str) -> String {
        format!("{}/{}", self.base.trim_end_matches('/'), npm_path(pkg))
    }
}

/// Derive the npm registry path for a package name.
///
/// Scoped packages (`@scope/name`) keep their `/` per the registry's
/// convention (`@scope%2fname` is also accepted, but the bare form is
/// canonical and what the registry redirects to); unscoped names pass through
/// unchanged.
pub fn npm_path(pkg: &str) -> String {
    pkg.to_string()
}

/// Read `dist-tags.latest` out of an npm package document.
pub fn parse_npm_document(body: &str) -> Result<Option<semver::Version>, RegistryError> {
    let doc: serde_json::Value =
        serde_json::from_str(body).map_err(|e| RegistryError::Parse(format!("npm doc: {e}")))?;
    let Some(latest) = doc
        .get("dist-tags")
        .and_then(|t| t.get("latest"))
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(None);
    };
    match semver::Version::parse(latest) {
        Ok(v) => Ok(Some(v)),
        Err(e) => Err(RegistryError::Parse(format!("npm latest '{latest}': {e}"))),
    }
}

impl<F: Fetcher> RegistryClient for NpmRegistry<F> {
    fn latest_version(&self, dep: &Dependency) -> Result<Option<semver::Version>, RegistryError> {
        let url = self.package_url(&dep.name);
        match self.fetcher.get(&url)? {
            Some(body) => parse_npm_document(&body),
            None => Ok(None),
        }
    }
}

// ---------------------------------------------------------------------------
// Layer 3: plan building
// ---------------------------------------------------------------------------

/// Join discovered dependencies with their latest registry versions into an
/// ordered list of actionable [`UpdatePlan`]s.
///
/// For each dependency the registry knows about, classifies the
/// `current → latest` jump; only genuinely outdated dependencies (where
/// `latest > current`, i.e. a non-[`crate::model::SemverClass::None`] class)
/// are kept. The
/// result is sorted deterministically by `(kind, name, manifest)` so identical
/// inputs always yield an identical plan (idempotency).
///
/// Registry lookups returning `Ok(None)` (unknown/unpublished) are silently
/// skipped; lookup errors abort with [`RegistryError`] so a transient registry
/// failure never masquerades as "nothing to update".
pub fn build_update_plan<R: RegistryClient>(
    deps: &[Dependency],
    registry: &R,
) -> Result<Vec<UpdatePlan>, RegistryError> {
    let mut plans = Vec::new();
    for dep in deps {
        let Some(latest) = registry.latest_version(dep)? else {
            continue;
        };
        let plan = UpdatePlan::new(
            dep.name.clone(),
            dep.kind,
            dep.current.clone(),
            latest,
            dep.manifest.clone(),
        );
        if plan.is_actionable() {
            plans.push(plan);
        }
    }

    plans.sort_by(|a, b| {
        a.kind
            .as_str()
            .cmp(b.kind.as_str())
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.manifest.cmp(&b.manifest))
    });

    Ok(plans)
}

/// Convenience: the repo root is the directory two levels above this crate
/// (`tools/dep-updater` → repo root). Useful for the CLI's `detect` command.
///
/// Returns the canonicalized path so downstream manifest reads are stable
/// regardless of the process's working directory.
pub fn default_repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points at tools/dep-updater at compile time; walk up.
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    crate_dir
        .parent() // tools/
        .and_then(Path::parent) // repo root
        .map(Path::to_path_buf)
        .unwrap_or_else(|| crate_dir.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SemverClass;

    fn v(s: &str) -> semver::Version {
        semver::Version::parse(s).unwrap()
    }

    // ----- loose version parsing -----

    #[test]
    fn cargo_req_parses_bare_and_operator_forms() {
        assert_eq!(parse_cargo_req("0.133.0"), Some(v("0.133.0")));
        assert_eq!(parse_cargo_req("^1.2.3"), Some(v("1.2.3")));
        assert_eq!(parse_cargo_req("~1.2"), Some(v("1.2.0")));
        assert_eq!(parse_cargo_req("1"), Some(v("1.0.0")));
        assert_eq!(parse_cargo_req(">=2.10.2"), Some(v("2.10.2")));
        assert_eq!(parse_cargo_req("*"), None);
    }

    #[test]
    fn npm_req_parses_ranges_and_rejects_non_semver() {
        assert_eq!(parse_npm_req("^21.2.15"), Some(v("21.2.15")));
        assert_eq!(parse_npm_req("~5.9.0"), Some(v("5.9.0")));
        assert_eq!(parse_npm_req("1.4.1"), Some(v("1.4.1")));
        // dist-tags and bespoke channels: skipped.
        assert_eq!(parse_npm_req("latest"), None);
        assert_eq!(parse_npm_req("volar-2.4"), None);
        // protocols / urls: skipped.
        assert_eq!(parse_npm_req("workspace:*"), None);
        assert_eq!(parse_npm_req("file:../x"), None);
        assert_eq!(parse_npm_req("github:user/repo"), None);
        // prerelease tail is dropped to the core.
        assert_eq!(parse_npm_req("^7.0.0-dev.20260527.2"), Some(v("7.0.0")));
    }

    // ----- Cargo.toml parsing -----

    const SAMPLE_CARGO: &str = r#"
[package]
name = "sample"
version = "0.1.0"
edition = "2021"

[dependencies]
oxc_ast = "0.133.0"
serde = { version = "1", features = ["derive"] }
napi = { version = "2.10.2", default-features = false, features = ["napi4"] }
treaty_ivy = { path = "../../libs/treaty-ivy/facade" }
inherited = { workspace = true }
some_git = { git = "https://example.com/x.git" }

[build-dependencies]
napi-build = "2.0.1"

[dev-dependencies]
proptest = "1.4.0"
"#;

    #[test]
    fn parse_cargo_extracts_only_registry_deps() {
        let mut deps = parse_cargo_manifest(SAMPLE_CARGO, "Cargo.toml").unwrap();
        deps.sort_by(|a, b| a.name.cmp(&b.name));
        let names: Vec<&str> = deps.iter().map(|d| d.name.as_str()).collect();
        // path (treaty_ivy), workspace (inherited), and git (some_git) deps are skipped.
        assert_eq!(
            names,
            vec!["napi", "napi-build", "oxc_ast", "proptest", "serde"]
        );
        assert!(deps.iter().all(|d| d.kind == DepKind::Crate));
        assert!(deps.iter().all(|d| d.manifest == "Cargo.toml"));

        let oxc = deps.iter().find(|d| d.name == "oxc_ast").unwrap();
        assert_eq!(oxc.current, v("0.133.0"));
        assert_eq!(oxc.requirement, "0.133.0");

        let serde = deps.iter().find(|d| d.name == "serde").unwrap();
        assert_eq!(serde.current, v("1.0.0"));
        assert_eq!(serde.requirement, "1");

        let napi = deps.iter().find(|d| d.name == "napi").unwrap();
        assert_eq!(napi.current, v("2.10.2"));
    }

    #[test]
    fn parse_workspace_dependencies_table() {
        let src = r#"
[workspace]
members = ["a", "b"]

[workspace.dependencies]
oxc_ast = "0.133.0"
local = { path = "x" }
"#;
        let deps = parse_cargo_manifest(src, "Cargo.toml").unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "oxc_ast");
        assert_eq!(deps[0].current, v("0.133.0"));

        let members = workspace_members(src, "Cargo.toml").unwrap();
        assert_eq!(members, vec!["a", "b"]);
    }

    #[test]
    fn malformed_cargo_is_an_error() {
        let err = parse_cargo_manifest("this = = not toml", "Cargo.toml").unwrap_err();
        assert!(matches!(err, DetectError::Toml { .. }));
    }

    // ----- package.json parsing -----

    const SAMPLE_PKG: &str = r#"
{
  "name": "@treaty/source",
  "private": true,
  "dependencies": {
    "@angular/core": "^21.2.15",
    "rxjs": "~7.8.0",
    "volar-service-css": "volar-2.4",
    "@swc/core": "1.4.1"
  },
  "devDependencies": {
    "typescript": "~5.9.0",
    "oxlint": "^1.67.0",
    "esbuild": "latest",
    "bun-types": "latest"
  }
}
"#;

    #[test]
    fn parse_package_json_extracts_semver_deps_only() {
        let mut deps = parse_package_json(SAMPLE_PKG, "package.json").unwrap();
        deps.sort_by(|a, b| a.name.cmp(&b.name));
        let names: Vec<&str> = deps.iter().map(|d| d.name.as_str()).collect();
        // "latest" (esbuild, bun-types) and "volar-2.4" are skipped.
        assert_eq!(
            names,
            vec!["@angular/core", "@swc/core", "oxlint", "rxjs", "typescript"]
        );
        assert!(deps.iter().all(|d| d.kind == DepKind::Npm));

        let ng = deps.iter().find(|d| d.name == "@angular/core").unwrap();
        assert_eq!(ng.current, v("21.2.15"));
        assert_eq!(ng.requirement, "^21.2.15");

        let ts = deps.iter().find(|d| d.name == "typescript").unwrap();
        assert_eq!(ts.current, v("5.9.0"));
    }

    #[test]
    fn malformed_json_is_an_error() {
        let err = parse_package_json("{ not json", "package.json").unwrap_err();
        assert!(matches!(err, DetectError::Json { .. }));
    }

    // ----- sparse index / npm document parsing -----

    #[test]
    fn sparse_index_picks_newest_stable_unyanked() {
        let body = concat!(
            r#"{"name":"oxc_ast","vers":"0.130.0","yanked":false}"#,
            "\n",
            r#"{"name":"oxc_ast","vers":"0.133.0","yanked":false}"#,
            "\n",
            r#"{"name":"oxc_ast","vers":"0.134.0","yanked":true}"#,
            "\n",
            r#"{"name":"oxc_ast","vers":"0.135.0-alpha.1","yanked":false}"#,
            "\n",
        );
        assert_eq!(parse_sparse_index(body).unwrap(), Some(v("0.133.0")));
    }

    #[test]
    fn sparse_index_empty_is_none() {
        assert_eq!(parse_sparse_index("\n  \n").unwrap(), None);
    }

    #[test]
    fn index_path_layout_matches_crates_io() {
        assert_eq!(index_path("a"), "1/a");
        assert_eq!(index_path("ab"), "2/ab");
        assert_eq!(index_path("abc"), "3/a/abc");
        assert_eq!(index_path("serde"), "se/rd/serde");
        // case-insensitive, lowercased.
        assert_eq!(index_path("Oxc_Ast"), "ox/c_/oxc_ast");
    }

    #[test]
    fn npm_document_reads_dist_tags_latest() {
        let body = r#"{"name":"typescript","dist-tags":{"latest":"5.9.2","next":"6.0.0-dev"}}"#;
        assert_eq!(parse_npm_document(body).unwrap(), Some(v("5.9.2")));
    }

    #[test]
    fn npm_document_missing_latest_is_none() {
        let body = r#"{"name":"x","dist-tags":{}}"#;
        assert_eq!(parse_npm_document(body).unwrap(), None);
    }

    // ----- registry clients over a fake fetcher -----

    struct MapFetcher {
        responses: HashMap<String, Option<String>>,
    }

    impl Fetcher for MapFetcher {
        fn get(&self, url: &str) -> Result<Option<String>, RegistryError> {
            match self.responses.get(url) {
                Some(body) => Ok(body.clone()),
                None => Err(RegistryError::Fetch(format!("unexpected url {url}"))),
            }
        }
    }

    #[test]
    fn sparse_registry_resolves_via_fetcher() {
        let mut responses = HashMap::new();
        responses.insert(
            "https://index.crates.io/ox/c_/oxc_ast".to_string(),
            Some(r#"{"vers":"0.140.0","yanked":false}"#.to_string()),
        );
        let reg = SparseIndexRegistry::crates_io(MapFetcher { responses });
        let dep = Dependency {
            name: "oxc_ast".into(),
            kind: DepKind::Crate,
            current: v("0.133.0"),
            requirement: "0.133.0".into(),
            manifest: "Cargo.toml".into(),
        };
        assert_eq!(reg.latest_version(&dep).unwrap(), Some(v("0.140.0")));
    }

    #[test]
    fn npm_registry_resolves_via_fetcher() {
        let mut responses = HashMap::new();
        responses.insert(
            "https://registry.npmjs.org/typescript".to_string(),
            Some(r#"{"dist-tags":{"latest":"5.9.2"}}"#.to_string()),
        );
        let reg = NpmRegistry::npmjs(MapFetcher { responses });
        let dep = Dependency {
            name: "typescript".into(),
            kind: DepKind::Npm,
            current: v("5.9.0"),
            requirement: "~5.9.0".into(),
            manifest: "package.json".into(),
        };
        assert_eq!(reg.latest_version(&dep).unwrap(), Some(v("5.9.2")));
    }

    // ----- plan building with a mocked registry -----

    fn dep(name: &str, kind: DepKind, current: &str, manifest: &str) -> Dependency {
        Dependency {
            name: name.into(),
            kind,
            current: v(current),
            requirement: current.into(),
            manifest: manifest.into(),
        }
    }

    #[test]
    fn build_plan_keeps_only_outdated_and_classifies() {
        let deps = vec![
            dep("oxc_ast", DepKind::Crate, "0.133.0", "libs/treaty-ivy/facade/Cargo.toml"),
            dep("serde", DepKind::Crate, "1.0.200", "Cargo.toml"),
            dep("up_to_date", DepKind::Crate, "2.0.0", "Cargo.toml"),
            dep("typescript", DepKind::Npm, "5.9.0", "package.json"),
            dep("unpublished", DepKind::Crate, "0.1.0", "Cargo.toml"),
        ];
        let registry = StaticRegistry::new()
            .with(DepKind::Crate, "oxc_ast", "0.140.0") // minor bump (0.x)
            .with(DepKind::Crate, "serde", "1.0.210") // patch
            .with(DepKind::Crate, "up_to_date", "2.0.0") // no change -> dropped
            .with(DepKind::Npm, "typescript", "6.0.0"); // major
        // "unpublished" intentionally absent -> dropped.

        let plans = build_update_plan(&deps, &registry).unwrap();

        // Deterministic order: crate deps (by name) then npm deps.
        let summary: Vec<(&str, SemverClass)> =
            plans.iter().map(|p| (p.name.as_str(), p.class)).collect();
        assert_eq!(
            summary,
            vec![
                ("oxc_ast", SemverClass::Minor),
                ("serde", SemverClass::Patch),
                ("typescript", SemverClass::Major),
            ]
        );

        let oxc = &plans[0];
        assert_eq!(oxc.current, v("0.133.0"));
        assert_eq!(oxc.latest, v("0.140.0"));
        assert_eq!(oxc.manifest, "libs/treaty-ivy/facade/Cargo.toml");
        assert_eq!(oxc.id(), "crate-oxc_ast-0.133.0-to-0.140.0");
    }

    #[test]
    fn build_plan_is_idempotent_regardless_of_input_order() {
        let registry = StaticRegistry::new()
            .with(DepKind::Crate, "a", "2.0.0")
            .with(DepKind::Crate, "b", "2.0.0")
            .with(DepKind::Npm, "z", "2.0.0");

        let forward = vec![
            dep("a", DepKind::Crate, "1.0.0", "Cargo.toml"),
            dep("b", DepKind::Crate, "1.0.0", "Cargo.toml"),
            dep("z", DepKind::Npm, "1.0.0", "package.json"),
        ];
        let reversed: Vec<Dependency> = forward.iter().rev().cloned().collect();

        let p1 = build_update_plan(&forward, &registry).unwrap();
        let p2 = build_update_plan(&reversed, &registry).unwrap();
        assert_eq!(p1, p2);
        let ids: Vec<String> = p1.iter().map(UpdatePlan::id).collect();
        assert_eq!(
            ids,
            vec![
                "crate-a-1.0.0-to-2.0.0",
                "crate-b-1.0.0-to-2.0.0",
                "npm-z-1.0.0-to-2.0.0",
            ]
        );
    }

    #[test]
    fn build_plan_propagates_registry_errors() {
        struct Failing;
        impl RegistryClient for Failing {
            fn latest_version(
                &self,
                _dep: &Dependency,
            ) -> Result<Option<semver::Version>, RegistryError> {
                Err(RegistryError::Fetch("boom".into()))
            }
        }
        let deps = vec![dep("x", DepKind::Crate, "1.0.0", "Cargo.toml")];
        assert!(build_update_plan(&deps, &Failing).is_err());
    }

    #[test]
    fn discover_dependencies_reads_a_temp_repo() {
        // Build a tiny fake repo on disk and discover end-to-end.
        let base = std::env::temp_dir().join(format!("dep-updater-detect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("crates/core")).unwrap();

        std::fs::write(
            base.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/core\"]\n\n[workspace.dependencies]\nshared = \"1.2.3\"\n",
        )
        .unwrap();
        std::fs::write(
            base.join("crates/core/Cargo.toml"),
            "[package]\nname = \"core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\noxc_ast = \"0.133.0\"\nlocal = { path = \"../x\" }\n",
        )
        .unwrap();
        std::fs::write(
            base.join("package.json"),
            "{\"dependencies\":{\"typescript\":\"~5.9.0\"},\"devDependencies\":{\"esbuild\":\"latest\"}}",
        )
        .unwrap();

        let deps = discover_dependencies(&base).unwrap();
        let names: Vec<(&str, &str)> = deps
            .iter()
            .map(|d| (d.name.as_str(), d.manifest.as_str()))
            .collect();
        assert_eq!(
            names,
            vec![
                ("oxc_ast", "crates/core/Cargo.toml"),
                ("shared", "Cargo.toml"),
                ("typescript", "package.json"),
            ]
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}
